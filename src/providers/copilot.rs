//! GitHub Copilot — GitHub OAuth token → `/copilot_internal/user`
//!
//! 凭证优先级：
//!   env `COPILOT_API_TOKEN` / `GITHUB_TOKEN` / `GH_TOKEN`
//!   → shell out `gh auth token`
//!
//! API：
//!   GET https://api.github.com/user               （取 login 作为账号名）
//!   GET https://api.github.com/copilot_internal/user （取 plan + quota_snapshots）
//!
//! 响应有两种形态：
//!
//! 付费（Business / Enterprise / Pro）：
//! ```json
//! {
//!   "login": "...",
//!   "access_type_sku": "copilot_individual",
//!   "copilot_plan": "individual",
//!   "quota_reset_date": "2025-11-01",
//!   "quota_snapshots": {
//!     "chat":                 { "percent_remaining": 50.0, "remaining": 150, "entitlement": 300, "unlimited": false },
//!     "completions":          { "unlimited": true, ... },
//!     "premium_interactions": { "percent_remaining": 74.0, "remaining": 222, "entitlement": 300, "unlimited": false }
//!   }
//! }
//! ```
//!
//! 免费限量（`free_limited_copilot`）：
//! ```json
//! {
//!   "login": "...",
//!   "access_type_sku": "free_limited_copilot",
//!   "copilot_plan": "individual",
//!   "limited_user_quotas": { "chat": 500, "completions": 4000 },   // 剩余
//!   "monthly_quotas":      { "chat": 500, "completions": 4000 },   // 本月总额
//!   "limited_user_reset_date": "2026-05-13"
//! }
//! ```
//!
//! 两种形态都映射到 `sub_quotas`；plan 名取 `copilot_plan`（fallback `access_type_sku`）。

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::Client;
use serde_json::Value;

use super::{Availability, Provider, SubQuota, Usage, http_client};

const USER_URL: &str = "https://api.github.com/user";
const COPILOT_USER_URL: &str = "https://api.github.com/copilot_internal/user";

#[derive(Default)]
pub struct Copilot;

#[async_trait]
impl Provider for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self) -> Availability {
        if token_from_env().is_some() {
            return Availability::Ready;
        }
        if which::which("gh").is_ok() {
            // gh 存在；是否已登录延到 fetch 阶段检测
            return Availability::Ready;
        }
        Availability::Missing(
            "未设置 GITHUB_TOKEN / GH_TOKEN / COPILOT_API_TOKEN，且 gh CLI 不在 PATH".into(),
        )
    }

    async fn fetch(&self) -> Result<Usage> {
        let token = resolve_token().await?;
        let client = http_client()?;

        // Copilot 配额
        let copilot: Value = client
            .get(COPILOT_USER_URL)
            .bearer_auth(&token)
            .send()
            .await
            .context("调用 /copilot_internal/user 失败")?
            .error_for_status()
            .context("/copilot_internal/user 非 2xx（账号可能没有 Copilot 订阅或 token 无权限）")?
            .json()
            .await
            .context("解析 /copilot_internal/user 响应失败")?;

        // login 优先取 copilot 响应里的同名字段，回退到 /user endpoint（付费账号可能不含）
        let login = copilot
            .get("login")
            .and_then(Value::as_str)
            .map(str::to_string);
        let login = match login {
            Some(l) => Some(l),
            None => fetch_login(&client, &token).await,
        };

        let plan = build_plan_label(&copilot);
        let (sub_quotas, reset_date) = build_sub_quotas(&copilot);

        let note = reset_date.map(|d| format!("quota resets on {}", d.format("%Y-%m-%d")));

        Ok(Usage {
            provider: "Copilot".to_string(),
            source: "oauth".to_string(),
            account: login,
            plan,
            session: None,
            weekly: None,
            credits: None,
            sub_quotas,
            updated_at: Utc::now(),
            note,
        })
    }
}

/// 读取 env 中的 GitHub token，按优先级。
fn token_from_env() -> Option<String> {
    for k in ["COPILOT_API_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// env → `gh auth token`。
async fn resolve_token() -> Result<String> {
    if let Some(t) = token_from_env() {
        return Ok(t);
    }
    let out = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .map_err(|e| anyhow!("无法调用 gh CLI：{}（请先 `gh auth login`）", e))?;
    if !out.status.success() {
        return Err(anyhow!(
            "`gh auth token` 失败 (exit={:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        return Err(anyhow!(
            "`gh auth token` 输出为空；请先 `gh auth login` 登录 github.com"
        ));
    }
    Ok(tok)
}

/// 取 GitHub 用户的 login 名作为账号标识，失败（如 fine-grained token 无 user scope）返回 None。
async fn fetch_login(client: &Client, token: &str) -> Option<String> {
    let resp = client
        .get(USER_URL)
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let v: Value = resp.json().await.ok()?;
    v.get("login").and_then(Value::as_str).map(str::to_string)
}

/// `copilot_plan` 优先，其次 `access_type_sku`；两者不同时拼成 `plan (sku)`。
fn build_plan_label(copilot: &Value) -> Option<String> {
    let plan = copilot.get("copilot_plan").and_then(Value::as_str);
    let sku = copilot.get("access_type_sku").and_then(Value::as_str);
    match (plan, sku) {
        (Some(p), Some(s)) if p != s => Some(format!("{} ({})", p, s)),
        (Some(p), _) => Some(p.to_string()),
        (None, Some(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// 统一两种响应形态到 SubQuota 列表。
/// 付费走 `quota_snapshots`（有 `percent_remaining`），免费走 `limited_user_quotas` + `monthly_quotas`。
fn build_sub_quotas(copilot: &Value) -> (Vec<SubQuota>, Option<DateTime<Utc>>) {
    // 付费：quota_reset_date。免费：limited_user_reset_date。两者都没有时 None。
    let reset_at = copilot
        .get("quota_reset_date")
        .or_else(|| copilot.get("limited_user_reset_date"))
        .and_then(Value::as_str)
        .and_then(parse_date);

    let mut out: Vec<SubQuota> = Vec::new();

    // 路径 A：付费用户的 quota_snapshots
    if let Some(snap) = copilot.get("quota_snapshots").and_then(Value::as_object) {
        for (label, meta) in snap {
            if meta
                .get("unlimited")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(pct_remain) = meta.get("percent_remaining").and_then(Value::as_f64) else {
                continue;
            };
            let used = (100.0 - pct_remain).clamp(0.0, 100.0);
            out.push(SubQuota {
                label: pretty_label(label),
                used_percent: used,
                resets_at: reset_at,
            });
        }
    }

    // 路径 B：免费限量用户 limited_user_quotas（剩余） / monthly_quotas（总额）
    if out.is_empty() {
        let remaining = copilot
            .get("limited_user_quotas")
            .and_then(Value::as_object);
        let total = copilot.get("monthly_quotas").and_then(Value::as_object);
        if let (Some(rem), Some(tot)) = (remaining, total) {
            for (label, tot_v) in tot {
                let Some(total_n) = tot_v.as_f64() else {
                    continue;
                };
                if total_n <= 0.0 {
                    continue;
                }
                let rem_n = rem.get(label).and_then(Value::as_f64).unwrap_or(total_n);
                let used = ((total_n - rem_n) / total_n * 100.0).clamp(0.0, 100.0);
                out.push(SubQuota {
                    label: pretty_label(label),
                    used_percent: used,
                    resets_at: reset_at,
                });
            }
        }
    }

    out.sort_by(|a, b| {
        b.used_percent
            .partial_cmp(&a.used_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (out, reset_at)
}

fn pretty_label(key: &str) -> String {
    match key {
        "premium_interactions" => "premium".to_string(),
        other => other.replace('_', " "),
    }
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    // RFC3339 或 YYYY-MM-DD
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)
        .map(|n| n.and_utc())
}

#[cfg(test)]
mod tests {
    //! 用合成 payload 覆盖两种响应形态，保证 Pro（quota_snapshots）与免费（monthly_quotas）
    //! 两条路径都被测试；这样即使本机没有 Pro 订阅也能回归测试。

    use super::*;
    use serde_json::json;

    #[test]
    fn paid_plan_extracts_snapshots_and_skips_unlimited() {
        // 典型付费（Pro / Business）响应：quota_snapshots 三个 key，其中 completions 是 unlimited。
        let payload = json!({
            "login": "octocat",
            "access_type_sku": "copilot_pro",
            "copilot_plan": "pro",
            "quota_reset_date": "2025-11-01",
            "quota_snapshots": {
                "chat": {
                    "entitlement": 300,
                    "percent_remaining": 50.0,
                    "remaining": 150,
                    "unlimited": false
                },
                "completions": {
                    "unlimited": true
                },
                "premium_interactions": {
                    "entitlement": 300,
                    "percent_remaining": 26.0,
                    "remaining": 78,
                    "unlimited": false
                }
            }
        });
        let (sq, reset) = build_sub_quotas(&payload);

        // completions 被跳过；剩 chat + premium_interactions 两条
        assert_eq!(sq.len(), 2);
        // 最紧（used_percent 最高）排前面：premium = 74%, chat = 50%
        assert_eq!(sq[0].label, "premium");
        assert!((sq[0].used_percent - 74.0).abs() < 1e-6);
        assert_eq!(sq[1].label, "chat");
        assert!((sq[1].used_percent - 50.0).abs() < 1e-6);

        // reset date 对齐 2025-11-01 00:00 UTC
        let reset = reset.expect("应当有 quota_reset_date");
        assert_eq!(reset.format("%Y-%m-%d").to_string(), "2025-11-01");

        // plan label
        assert_eq!(
            build_plan_label(&payload).as_deref(),
            Some("pro (copilot_pro)")
        );
    }

    #[test]
    fn free_limited_plan_computes_from_monthly_quotas() {
        // 免费限量（free_limited_copilot）响应：没有 quota_snapshots，走 monthly - limited 计算。
        let payload = json!({
            "login": "wangyuyan-agent",
            "access_type_sku": "free_limited_copilot",
            "copilot_plan": "individual",
            "limited_user_quotas": { "chat": 450, "completions": 4000 },
            "monthly_quotas":      { "chat": 500, "completions": 4000 },
            "limited_user_reset_date": "2026-05-13"
        });
        let (sq, reset) = build_sub_quotas(&payload);

        assert_eq!(sq.len(), 2);
        // chat used = (500-450)/500 = 10%；completions 满额 → 0%；chat 排前
        assert_eq!(sq[0].label, "chat");
        assert!((sq[0].used_percent - 10.0).abs() < 1e-6);
        assert_eq!(sq[1].label, "completions");
        assert!(sq[1].used_percent.abs() < 1e-6);

        assert!(reset.is_some());
    }

    #[test]
    fn all_unlimited_returns_empty() {
        // 企业无限量账号：每条都 unlimited → sub_quotas 为空
        let payload = json!({
            "copilot_plan": "enterprise",
            "quota_snapshots": {
                "chat":                 { "unlimited": true },
                "completions":          { "unlimited": true },
                "premium_interactions": { "unlimited": true }
            }
        });
        let (sq, _) = build_sub_quotas(&payload);
        assert!(sq.is_empty(), "unlimited 响应不应产生 SubQuota");
    }

    #[test]
    fn plan_label_fallbacks() {
        // copilot_plan 缺失时 fall back 到 access_type_sku
        let only_sku = json!({ "access_type_sku": "business_seat" });
        assert_eq!(
            build_plan_label(&only_sku).as_deref(),
            Some("business_seat")
        );

        // 两字段相同 → 不做重复拼接
        let same = json!({ "copilot_plan": "pro", "access_type_sku": "pro" });
        assert_eq!(build_plan_label(&same).as_deref(), Some("pro"));

        // 都没有 → None
        let empty = json!({});
        assert!(build_plan_label(&empty).is_none());
    }
}
