//! Codex — OpenAI Codex CLI 的 `~/.codex/auth.json` OAuth 会话 + 本地 rollout 日志限额快照
//!
//! ChatGPT / Codex 订阅没有公开的用量查询 API，但 Codex CLI（>= 0.4x）把每次
//! 服务端返回的限额状态写进 rollout 日志：
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` 的 `token_count` 事件带
//! `rate_limits` 快照（官方数据，非估算）：
//!
//! ```json
//! {"timestamp":"...","type":"event_msg","payload":{"type":"token_count",
//!   "rate_limits":{"limit_id":"codex","primary":{"used_percent":62.0,
//!   "window_minutes":10080,"resets_at":1784791060},"secondary":null,
//!   "credits":{"has_credits":false,"unlimited":false,"balance":"0"},
//!   "plan_type":"plus"}}}
//! ```
//!
//! 本 provider：
//! 1. detect：`~/.codex/auth.json` 存在并可解析。
//! 2. fetch：
//!    - 解码 `tokens.id_token` JWT → email / plan / account_id（身份）
//!    - 扫最新 rollout 日志（按 mtime 倒序 tail-read）→ 最后一条 `rate_limits`
//!      快照 → `window_minutes < 1440` 进 session、其余进 weekly；credits 有
//!      余额时透出；快照年龄拼进 note（例如 `limits 2h ago`）
//!    - 找不到快照时回退为仅身份显示（旧行为）

use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::{Availability, CostMetric, Credits, Provider, Usage, Window};

#[derive(Default)]
pub struct Codex;

/// `~/.codex/auth.json` 关心的字段。多余字段忽略。
#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(default)]
    #[allow(dead_code)] // 仅记录 platform API key 是否同时存在；本 provider 不使用
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<Tokens>,
    #[serde(default)]
    last_refresh: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Tokens {
    id_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[async_trait]
impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> Availability {
        let path = auth_path();
        if !path.exists() {
            return Availability::Missing(format!(
                "缺少 {}（请先 `codex login` 登录 ChatGPT）",
                path.display()
            ));
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AuthFile>(&text) {
                Ok(a) if a.tokens.is_some() => Availability::Ready,
                Ok(_) => Availability::Missing(
                    "auth.json 存在但 tokens 字段为空（请重新 `codex login`）".into(),
                ),
                Err(_) => Availability::Missing("auth.json 无法解析为 JSON".into()),
            },
            Err(_) => Availability::Missing("auth.json 无法读取".into()),
        }
    }

    async fn fetch(&self) -> Result<Usage> {
        let path = auth_path();
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("读取 {:?} 失败", path))?;
        let auth: AuthFile =
            serde_json::from_str(&text).with_context(|| format!("解析 {:?} 失败", path))?;

        let tokens = auth
            .tokens
            .ok_or_else(|| anyhow!("auth.json 中 tokens 为空，请 `codex login` 重新登录"))?;
        let id_token = tokens
            .id_token
            .as_deref()
            .ok_or_else(|| anyhow!("auth.json 中缺少 id_token"))?;

        let claims = decode_jwt_claims(id_token)
            .ok_or_else(|| anyhow!("id_token 非法 JWT（无法 base64/JSON 解析）"))?;

        let email = claims
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string);

        // 自定义 claim namespace：https://api.openai.com/auth
        let openai_auth = claims
            .get("https://api.openai.com/auth")
            .cloned()
            .unwrap_or(Value::Null);
        let plan_type = openai_auth
            .get("chatgpt_plan_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        let chatgpt_account = openai_auth
            .get("chatgpt_account_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(tokens.account_id);

        // 本地 rollout 日志里的官方限额快照（I/O 密集，丢 blocking 池）
        let codex_sessions_dir = sessions_dir();
        let snapshot_dir = codex_sessions_dir.clone();
        let snapshot = tokio::task::spawn_blocking(move || find_latest_rate_limits(&snapshot_dir))
            .await
            .context("扫描 ~/.codex/sessions 失败（线程 join）")?;
        let history_dir = codex_sessions_dir.clone();
        let history =
            tokio::task::spawn_blocking(move || scan_token_history(&history_dir, Utc::now()))
                .await
                .context("扫描 Codex token 历史失败（线程 join）")?;

        // JWT 过期 / 刷新时间
        let exp = claims.get("exp").and_then(Value::as_i64);
        let now = Utc::now().timestamp();
        let mut note_parts: Vec<String> = Vec::new();
        if let Some(id) = &chatgpt_account {
            note_parts.push(format!("account_id={}", short_id(id)));
        }
        if let Some(exp_ts) = exp {
            if exp_ts < now {
                note_parts.push(format!(
                    "⚠ id_token expired {} ago",
                    fmt_duration_secs(now - exp_ts)
                ));
            } else {
                note_parts.push(format!(
                    "id_token valid for {}",
                    fmt_duration_secs(exp_ts - now)
                ));
            }
        }
        if let Some(refresh) = auth.last_refresh.as_deref()
            && let Ok(ts) = DateTime::parse_from_rfc3339(refresh)
        {
            let ago = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
            note_parts.push(format!(
                "refreshed {} ago",
                fmt_duration_secs(ago.num_seconds())
            ));
        }
        if let Some(latest) = history.latest_total_tokens {
            note_parts.push(format!("latest {}", fmt_tokens(latest)));
        }

        let (session, weekly, credits, mut plan) = match &snapshot {
            Some(s) => {
                let age = Utc::now().signed_duration_since(s.observed_at);
                note_parts.push(format!(
                    "limits from local log, {} ago",
                    fmt_duration_secs(age.num_seconds())
                ));
                (
                    s.session.clone(),
                    s.weekly.clone(),
                    s.credits.clone(),
                    s.plan_type.clone(),
                )
            }
            None => {
                note_parts.push("no rate_limits in local logs".to_string());
                (None, None, None, None)
            }
        };
        // 快照里的 plan_type 比 JWT 新鲜；两者都有时以快照为准
        plan = plan.or(plan_type);

        Ok(Usage {
            provider: "Codex".to_string(),
            source: "oauth".to_string(),
            account: email,
            plan,
            session,
            weekly,
            credits,
            sub_quotas: Vec::new(),
            costs: history.costs,
            status: None,
            updated_at: Utc::now(),
            note: Some(note_parts.join(" · ")),
        })
    }
}

#[derive(Debug, Default, Clone)]
struct TokenHistory {
    costs: Vec<CostMetric>,
    latest_total_tokens: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy)]
struct TokenTotals {
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    total: u64,
}

impl TokenTotals {
    fn add(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            cached_input: self.cached_input.saturating_add(other.cached_input),
            output: self.output.saturating_add(other.output),
            reasoning_output: self.reasoning_output.saturating_add(other.reasoning_output),
            total: self.total.saturating_add(other.total),
        }
    }
}

fn scan_token_history(dir: &Path, now: DateTime<Utc>) -> TokenHistory {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_jsonl(dir, &mut files, 0);
    let cutoff = now - Duration::days(31);

    let mut seven = TokenTotals::default();
    let mut thirty = TokenTotals::default();
    let mut latest: Option<(DateTime<Utc>, u64)> = None;

    for (mtime, path) in files {
        let modified: DateTime<Utc> = mtime.into();
        if modified < cutoff {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if !line.contains("\"token_count\"") {
                continue;
            }
            let Some((ts, usage)) = parse_token_usage_line(line) else {
                continue;
            };
            if ts < cutoff {
                continue;
            }
            if ts >= now - Duration::days(7) {
                seven = seven.add(usage);
            }
            if ts >= now - Duration::days(30) {
                thirty = thirty.add(usage);
            }
            if latest.map(|(old, _)| ts > old).unwrap_or(true) {
                latest = Some((ts, usage.total));
            }
        }
    }

    let mut costs = Vec::new();
    if seven.total > 0 {
        costs.push(cost_metric("Last 7 days", 7, seven));
    }
    if thirty.total > 0 {
        costs.push(cost_metric("Last 30 days", 30, thirty));
    }

    TokenHistory {
        costs,
        latest_total_tokens: latest.map(|(_, t)| t),
    }
}

fn parse_token_usage_line(line: &str) -> Option<(DateTime<Utc>, TokenTotals)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))?;
    let usage = v.get("payload")?.get("info")?.get("last_token_usage")?;
    Some((ts, parse_token_totals(usage)?))
}

fn parse_token_totals(v: &Value) -> Option<TokenTotals> {
    let total = v.get("total_tokens").and_then(Value::as_u64)?;
    Some(TokenTotals {
        input: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input: v
            .get("cached_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        reasoning_output: v
            .get("reasoning_output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total,
    })
}

fn cost_metric(label: &str, days: u32, totals: TokenTotals) -> CostMetric {
    CostMetric {
        label: label.to_string(),
        days,
        amount: Some(estimate_codex_usd(totals)),
        currency: "USD".to_string(),
        tokens: Some(totals.total),
    }
}

/// Local estimate using a conservative blended GPT-class price sheet.
/// This is intentionally labelled as an estimate in the UI/CLI; provider billing is authoritative.
fn estimate_codex_usd(t: TokenTotals) -> f64 {
    const INPUT_PER_M: f64 = 1.25;
    const CACHED_INPUT_PER_M: f64 = 0.125;
    const OUTPUT_PER_M: f64 = 10.0;
    let uncached_input = t.input.saturating_sub(t.cached_input) as f64;
    let cached_input = t.cached_input as f64;
    let output = t.output.saturating_add(t.reasoning_output) as f64;
    (uncached_input * INPUT_PER_M + cached_input * CACHED_INPUT_PER_M + output * OUTPUT_PER_M)
        / 1_000_000.0
}

// ---------- rollout 日志限额快照 ----------

/// 从 rollout 日志解析出来的一次官方限额快照。
#[derive(Debug, Clone)]
struct RateLimitSnapshot {
    session: Option<Window>,
    weekly: Option<Window>,
    credits: Option<Credits>,
    plan_type: Option<String>,
    /// 快照写入日志的时刻（事件行的 timestamp）。
    observed_at: DateTime<Utc>,
}

fn sessions_dir() -> PathBuf {
    let base = if let Ok(p) = std::env::var("CODEX_HOME") {
        PathBuf::from(p)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".codex")
    };
    base.join("sessions")
}

/// 找最新的 `rate_limits` 快照：收集 rollout 文件按 mtime 倒序，
/// 逐个 tail-read（最后 512 KiB）从后往前找 `token_count` 事件。
fn find_latest_rate_limits(dir: &Path) -> Option<RateLimitSnapshot> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_jsonl(dir, &mut files, 0);
    files.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));

    // 只看最近 10 个文件；活跃机器第一个就会命中
    for (_, path) in files.into_iter().take(10) {
        if let Some(s) = tail_find_rate_limits(&path) {
            return Some(s);
        }
    }
    None
}

/// 递归收集 `.jsonl`（sessions/YYYY/MM/DD/ 三层，限深防意外符号链接环）。
fn collect_jsonl(dir: &Path, out: &mut Vec<(std::time::SystemTime, PathBuf)>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
        {
            out.push((mtime, path));
        }
    }
}

/// 读文件尾部（最多 512 KiB），倒序找最后一条带 `rate_limits` 的 `token_count` 事件。
fn tail_find_rate_limits(path: &Path) -> Option<RateLimitSnapshot> {
    const TAIL: u64 = 512 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;

    for line in buf.lines().rev() {
        if !line.contains("\"rate_limits\"") || !line.contains("\"token_count\"") {
            continue;
        }
        if let Some(s) = parse_rate_limit_line(line) {
            return Some(s);
        }
    }
    None
}

/// 解析单行 rollout 事件 → 快照。对 schema 缺字段全部容错。
fn parse_rate_limit_line(line: &str) -> Option<RateLimitSnapshot> {
    let v: Value = serde_json::from_str(line).ok()?;
    let observed_at = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))?;
    let rl = v.get("payload")?.get("rate_limits")?;

    // primary / secondary 各自可能是 session（短窗）或 weekly（长窗）
    let mut session: Option<Window> = None;
    let mut weekly: Option<Window> = None;
    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|w| !w.is_null()) else {
            continue;
        };
        let Some(used) = w.get("used_percent").and_then(Value::as_f64) else {
            continue;
        };
        let minutes = w.get("window_minutes").and_then(Value::as_u64);
        let resets_at = w
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|ts| DateTime::from_timestamp(ts, 0));
        let win = Window {
            used_percent: used.clamp(0.0, 100.0),
            window_minutes: minutes,
            resets_at,
        };
        // < 1 天的窗算 session，其余算 weekly；未知窗长按 weekly 兜底
        match minutes {
            Some(m) if m < 1440 => session = session.or(Some(win)),
            _ => weekly = weekly.or(Some(win)),
        }
    }

    let credits = rl.get("credits").and_then(|c| {
        let has = c
            .get("has_credits")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has {
            return None;
        }
        let balance = c.get("balance").and_then(Value::as_str)?.parse().ok()?;
        Some(Credits {
            remaining: balance,
            total: None,
            unit: "credits".to_string(),
        })
    });

    let plan_type = rl
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(RateLimitSnapshot {
        session,
        weekly,
        credits,
        plan_type,
        observed_at,
    })
}

fn auth_path() -> PathBuf {
    auth_path_with(
        std::env::var("CODEX_HOME").ok().as_deref(),
        dirs::home_dir(),
    )
}

/// `auth_path` 的纯函数版本，便于 unit test 不污染全局环境变量。
fn auth_path_with(codex_home: Option<&str>, home: Option<PathBuf>) -> PathBuf {
    if let Some(p) = codex_home {
        return PathBuf::from(p).join("auth.json");
    }
    home.unwrap_or_else(|| PathBuf::from("/"))
        .join(".codex")
        .join("auth.json")
}

/// JWT = `header.payload.signature`，base64url(payload) 解开是 JSON claims。
fn decode_jwt_claims(token: &str) -> Option<serde_json::Map<String, Value>> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// 把秒数格式化成最贴近的人类可读单位：`45s` / `12m` / `3h20m` / `7d3h`。
fn fmt_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3_600, (secs % 3_600) / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{}s", secs),
        (0, 0, m) => format!("{}m", m),
        (0, h, 0) => format!("{}h", h),
        (0, h, m) => format!("{}h{}m", h, m),
        (d, 0, _) => format!("{}d", d),
        (d, h, _) => format!("{}d{}h", d, h),
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B tokens", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M tokens", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K tokens", n as f64 / 1_000.0)
    } else {
        format!("{} tokens", n)
    }
}

/// ChatGPT account id 是一个 UUID，把它截成 `abcd1234…` 省横屏。
fn short_id(s: &str) -> String {
    let n = s.chars().count();
    if n <= 10 {
        s.to_string()
    } else {
        let head: String = s.chars().take(8).collect();
        format!("{}…", head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::path::Path;

    fn make_jwt(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        format!("{header}.{payload}.")
    }

    #[test]
    fn decode_jwt_claims_extracts_openai_namespace() {
        let token = make_jwt(
            r#"{"email":"user@example.com","https://api.openai.com/auth":{"chatgpt_plan_type":"plus","chatgpt_account_id":"a-b-c"}}"#,
        );
        let claims = decode_jwt_claims(&token).unwrap();
        assert_eq!(
            claims.get("email").and_then(Value::as_str),
            Some("user@example.com")
        );
        let oa = claims.get("https://api.openai.com/auth").unwrap();
        assert_eq!(
            oa.get("chatgpt_plan_type").and_then(Value::as_str),
            Some("plus")
        );
    }

    #[test]
    fn decode_jwt_claims_rejects_malformed() {
        assert!(decode_jwt_claims("").is_none());
        assert!(decode_jwt_claims("only-one-part").is_none());
        assert!(decode_jwt_claims("bad.!!not-base64!!.sig").is_none());
        // payload 能 base64 解码但不是 object
        let not_obj = URL_SAFE_NO_PAD.encode(b"\"just a string\"");
        assert!(decode_jwt_claims(&format!("h.{not_obj}.s")).is_none());
    }

    #[test]
    fn short_id_keeps_short_ids_and_truncates_long() {
        assert_eq!(short_id(""), "");
        assert_eq!(short_id("abcd"), "abcd");
        assert_eq!(short_id("0123456789"), "0123456789"); // 恰好 10，保留
        assert_eq!(short_id("0123456789abcdef"), "01234567…");
    }

    #[test]
    fn auth_path_respects_codex_home_env() {
        let p = auth_path_with(Some("/tmp/xdg/codex"), Some(PathBuf::from("/home/x")));
        assert_eq!(p, Path::new("/tmp/xdg/codex/auth.json"));
    }

    #[test]
    fn auth_path_falls_back_to_home_when_no_codex_home() {
        let p = auth_path_with(None, Some(PathBuf::from("/home/icex")));
        assert_eq!(p, Path::new("/home/icex/.codex/auth.json"));
    }

    #[test]
    fn parse_rate_limit_line_full_shape() {
        // 真实 rollout 行的形态（token 数值已脱敏）
        let line = r#"{"timestamp":"2026-07-17T07:45:51.102Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1000}},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":62.0,"window_minutes":10080,"resets_at":1784791060},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"individual_limit":null,"spend_control_reached":null,"plan_type":"plus","rate_limit_reached_type":null}}}"#;
        let s = parse_rate_limit_line(line).expect("应能解析真实形态");

        // 10080 分钟 = 7 天 → weekly；无短窗 → session None
        assert!(s.session.is_none());
        let w = s.weekly.expect("weekly 应存在");
        assert!((w.used_percent - 62.0).abs() < 1e-9);
        assert_eq!(w.window_minutes, Some(10_080));
        assert_eq!(w.resets_at.map(|t| t.timestamp()), Some(1_784_791_060_i64));

        // has_credits=false → 不产生 Credits
        assert!(s.credits.is_none());
        assert_eq!(s.plan_type.as_deref(), Some("plus"));
        assert_eq!(s.observed_at.to_rfc3339(), "2026-07-17T07:45:51.102+00:00");
    }

    #[test]
    fn parse_rate_limit_line_dual_windows_and_credits() {
        // primary 短窗（5h）+ secondary 长窗（7d）+ 有余额 credits
        let line = r#"{"timestamp":"2026-07-17T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":48.0,"window_minutes":300,"resets_at":1784800000},"secondary":{"used_percent":63.0,"window_minutes":10080,"resets_at":1784791060},"credits":{"has_credits":true,"unlimited":false,"balance":"1250.5"},"plan_type":"pro"}}}"#;
        let s = parse_rate_limit_line(line).unwrap();

        let sess = s.session.expect("300 分钟窗应归入 session");
        assert!((sess.used_percent - 48.0).abs() < 1e-9);
        assert_eq!(sess.window_minutes, Some(300));

        let wk = s.weekly.expect("10080 分钟窗应归入 weekly");
        assert!((wk.used_percent - 63.0).abs() < 1e-9);

        let c = s.credits.expect("has_credits=true 应产生 Credits");
        assert!((c.remaining - 1250.5).abs() < 1e-9);
        assert_eq!(s.plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn parse_rate_limit_line_rejects_malformed() {
        // 缺 timestamp
        assert!(
            parse_rate_limit_line(
                r#"{"payload":{"rate_limits":{"primary":{"used_percent":1.0}}}}"#
            )
            .is_none()
        );
        // 缺 rate_limits
        assert!(
            parse_rate_limit_line(
                r#"{"timestamp":"2026-07-17T00:00:00Z","payload":{"type":"token_count"}}"#
            )
            .is_none()
        );
        // 非 JSON
        assert!(parse_rate_limit_line("not json at all").is_none());
    }

    #[test]
    fn fmt_duration_secs_picks_human_units() {
        assert_eq!(fmt_duration_secs(0), "0s");
        assert_eq!(fmt_duration_secs(45), "45s");
        assert_eq!(fmt_duration_secs(120), "2m");
        assert_eq!(fmt_duration_secs(3 * 3600), "3h");
        assert_eq!(fmt_duration_secs(3 * 3600 + 20 * 60), "3h20m");
        assert_eq!(fmt_duration_secs(7 * 86_400 + 3 * 3600), "7d3h");
        // 615778s（真实观测值）≈ 7d3h
        assert_eq!(fmt_duration_secs(615_778), "7d3h");
        // 负数（时钟漂移）不 panic，按 0 处理
        assert_eq!(fmt_duration_secs(-5), "0s");
    }

    #[test]
    fn parse_token_usage_line_reads_last_usage() {
        let line = r#"{"timestamp":"2026-07-02T03:03:25.709Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":41994113,"cached_input_tokens":39670272,"output_tokens":141081,"reasoning_output_tokens":59989,"total_tokens":42135194},"last_token_usage":{"input_tokens":180822,"cached_input_tokens":175488,"output_tokens":826,"reasoning_output_tokens":516,"total_tokens":181648}}}}"#;
        let (ts, usage) = parse_token_usage_line(line).unwrap();
        assert_eq!(ts.to_rfc3339(), "2026-07-02T03:03:25.709+00:00");
        assert_eq!(usage.input, 180_822);
        assert_eq!(usage.cached_input, 175_488);
        assert_eq!(usage.output, 826);
        assert_eq!(usage.reasoning_output, 516);
        assert_eq!(usage.total, 181_648);
    }

    #[test]
    fn estimate_codex_usd_accounts_for_cached_tokens() {
        let totals = TokenTotals {
            input: 2_000_000,
            cached_input: 1_000_000,
            output: 100_000,
            reasoning_output: 50_000,
            total: 2_150_000,
        };
        let cost = estimate_codex_usd(totals);
        assert!((cost - 2.875).abs() < 1e-9);
    }

    #[test]
    fn fmt_tokens_compacts_large_values() {
        assert_eq!(fmt_tokens(999), "999 tokens");
        assert_eq!(fmt_tokens(1_500), "1.5K tokens");
        assert_eq!(fmt_tokens(2_500_000), "2.5M tokens");
    }

    #[test]
    fn auth_path_handles_missing_home() {
        // 没有 CODEX_HOME 也没有 home_dir —— 落到 `/.codex/auth.json`，至少不 panic
        let p = auth_path_with(None, None);
        assert_eq!(p, Path::new("/.codex/auth.json"));
    }
}
