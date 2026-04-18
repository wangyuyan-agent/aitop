//! Gemini — port 自 ~/Downloads/gemini-usage (Python)
//!
//! 流程：
//! 1. 读 `~/.gemini/oauth_creds.json`
//! 2. 如果 access_token 过期，从 gemini CLI 的 node_modules / chunk-*.js 提取 OAUTH_CLIENT_ID/SECRET，
//!    用 refresh_token 刷新后写回文件
//! 3. 调 `loadCodeAssist` 取 tier + project，fallback `cloudresourcemanager.googleapis.com/v1/projects`
//! 4. 调 `retrieveUserQuota` 取按模型的 remainingFraction

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Availability, Provider, SubQuota, Usage, http_client};

const QUOTA_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const LOAD_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const PROJECTS_URL: &str = "https://cloudresourcemanager.googleapis.com/v1/projects";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Default)]
pub struct Gemini;

#[derive(Debug, Serialize, Deserialize)]
struct Creds {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    expiry_date: Option<f64>, // ms since epoch
    id_token: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[async_trait]
impl Provider for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> Availability {
        let creds = gemini_dir().join("oauth_creds.json");
        if !creds.exists() {
            return Availability::Missing(format!("缺少 {}（请先运行 gemini CLI 登录）", creds.display()));
        }
        // settings.json 若存在且强制 api-key/vertex-ai 则视为不支持
        let settings = gemini_dir().join("settings.json");
        if settings.exists()
            && let Ok(text) = std::fs::read_to_string(&settings)
                && let Ok(v) = serde_json::from_str::<Value>(&text) {
                    let t = v
                        .get("security")
                        .and_then(|x| x.get("auth"))
                        .and_then(|x| x.get("selectedType"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if t == "api-key" || t == "vertex-ai" {
                        return Availability::Missing(format!("Gemini 配置为 {}，仅支持 OAuth", t));
                    }
                }
        Availability::Ready
    }

    async fn fetch(&self) -> Result<Usage> {
        check_auth_type()?;
        let creds_path = gemini_dir().join("oauth_creds.json");
        let creds_text = std::fs::read_to_string(&creds_path).with_context(|| {
            format!(
                "读取 {:?} 失败，请先执行 gemini CLI 完成 OAuth 登录",
                creds_path
            )
        })?;
        let mut creds: Creds = serde_json::from_str(&creds_text).context("解析 oauth_creds.json 失败")?;

        let token = ensure_fresh_token(&mut creds, &creds_path).await?;
        let claims = jwt_decode_claims(creds.id_token.as_deref().unwrap_or(""));
        let email = claims
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string);
        let workspace_domain = claims.get("hd").and_then(Value::as_str).map(str::to_string);

        let client = http_client()?;
        let (tier_id, project_id) = load_code_assist(&client, &token).await.unwrap_or((None, None));
        let project_id = match project_id {
            Some(p) => Some(p),
            None => discover_project(&client, &token).await.ok().flatten(),
        };

        let quota_body = match &project_id {
            Some(p) => serde_json::json!({ "project": p }),
            None => serde_json::json!({}),
        };
        let quota: Value = client
            .post(QUOTA_URL)
            .bearer_auth(&token)
            .json(&quota_body)
            .send()
            .await?
            .error_for_status()
            .context("retrieveUserQuota")?
            .json()
            .await?;

        let buckets = quota
            .get("buckets")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("quota 响应缺 buckets"))?;
        let sub_quotas = parse_quota_buckets(buckets);

        Ok(Usage {
            provider: "Gemini".to_string(),
            source: "oauth".to_string(),
            account: email,
            plan: Some(tier_label(tier_id.as_deref(), workspace_domain.as_deref()).to_string()),
            session: None,
            weekly: None,
            credits: None,
            sub_quotas,
            updated_at: Utc::now(),
            note: project_id.map(|p| format!("project={}", p)),
        })
    }
}

fn gemini_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".gemini")
}

fn check_auth_type() -> Result<()> {
    let path = gemini_dir().join("settings.json");
    if !path.exists() {
        return Ok(());
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let t = v
        .get("security")
        .and_then(|x| x.get("auth"))
        .and_then(|x| x.get("selectedType"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match t {
        "api-key" => bail!("Gemini 设置为 API Key 模式，本 provider 仅支持 OAuth"),
        "vertex-ai" => bail!("Gemini 设置为 Vertex AI 模式，本 provider 仅支持 OAuth"),
        _ => Ok(()),
    }
}

fn jwt_decode_claims(token: &str) -> serde_json::Map<String, Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Default::default();
    }
    let payload = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return Default::default(),
    };
    match serde_json::from_slice::<Value>(&payload) {
        Ok(Value::Object(m)) => m,
        _ => Default::default(),
    }
}

async fn ensure_fresh_token(creds: &mut Creds, path: &Path) -> Result<String> {
    let token = creds
        .access_token
        .clone()
        .ok_or_else(|| anyhow!("oauth_creds.json 中无 access_token，请重新登录"))?;
    let expiry_ms = creds.expiry_date.unwrap_or(0.0);
    let now_ms = chrono::Utc::now().timestamp_millis() as f64;
    if expiry_ms > 0.0 && expiry_ms > now_ms + 30_000.0 {
        return Ok(token);
    }
    // 需要刷新
    let rt = creds
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("access_token 过期且无 refresh_token，请重新登录 gemini CLI"))?;
    let (cid, csec) = extract_cli_oauth_secrets()
        .ok_or_else(|| anyhow!("未能从本地 gemini CLI 提取 OAUTH_CLIENT_ID/SECRET"))?;

    let params = [
        ("client_id", cid.as_str()),
        ("client_secret", csec.as_str()),
        ("refresh_token", rt.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let resp: Value = http_client()?
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await?
        .error_for_status()
        .context("token 刷新失败")?
        .json()
        .await?;
    let new_tok = resp
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("刷新响应中无 access_token"))?
        .to_string();
    creds.access_token = Some(new_tok.clone());
    if let Some(exp_s) = resp.get("expires_in").and_then(Value::as_f64) {
        creds.expiry_date = Some(now_ms + exp_s * 1000.0);
    }
    if let Some(idt) = resp.get("id_token").and_then(Value::as_str) {
        creds.id_token = Some(idt.to_string());
    }
    // 写回
    let merged = serde_json::to_value(&creds)?;
    std::fs::write(path, serde_json::to_string_pretty(&merged)?)
        .with_context(|| format!("写回 {:?} 失败", path))?;
    Ok(new_tok)
}

fn extract_cli_oauth_secrets() -> Option<(String, String)> {
    use regex::Regex;

    let gemini_bin = which::which("gemini").ok()?;
    let resolved = std::fs::canonicalize(&gemini_bin).unwrap_or(gemini_bin);
    let base = resolved.parent()?.parent()?.to_path_buf();

    let subpath = "node_modules/@google/gemini-cli/node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js";
    let core = "node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js";
    let nix = format!("share/gemini-cli/{}", core);

    let mut candidates: Vec<PathBuf> = vec![
        base.join("libexec/lib").join(subpath),
        base.join("lib").join(subpath),
        base.join(&nix),
        base.join(core),
    ];
    let chunk_dirs = [
        resolved.parent().unwrap().to_path_buf(),
        base.join("bundle"),
        base.join("lib/node_modules/@google/gemini-cli/bundle"),
    ];
    for d in chunk_dirs {
        if d.is_dir() {
            let pat = d.join("chunk-*.js");
            if let Some(p) = pat.to_str()
                && let Ok(g) = glob::glob(p) {
                    for entry in g.flatten() {
                        candidates.push(entry);
                    }
                }
        }
    }

    let re_cid = Regex::new(r#"OAUTH_CLIENT_ID\s*=\s*['"]([^'"]+)['"]"#).ok()?;
    let re_sec = Regex::new(r#"OAUTH_CLIENT_SECRET\s*=\s*['"]([^'"]+)['"]"#).ok()?;
    for p in candidates {
        let Ok(txt) = std::fs::read_to_string(&p) else {
            continue;
        };
        let cid = re_cid.captures(&txt).and_then(|c| c.get(1));
        let sec = re_sec.captures(&txt).and_then(|c| c.get(1));
        if let (Some(a), Some(b)) = (cid, sec) {
            return Some((a.as_str().to_string(), b.as_str().to_string()));
        }
    }
    None
}

async fn load_code_assist(client: &Client, token: &str) -> Result<(Option<String>, Option<String>)> {
    let body = serde_json::json!({
        "metadata": { "ideType": "GEMINI_CLI", "pluginType": "GEMINI" }
    });
    let resp: Value = client
        .post(LOAD_URL)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tier = resp
        .get("currentTier")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let project = match resp.get("cloudaicompanionProject") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(m)) => m
            .get("id")
            .or_else(|| m.get("projectId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    };
    Ok((tier, project))
}

async fn discover_project(client: &Client, token: &str) -> Result<Option<String>> {
    let resp: Value = client
        .get(PROJECTS_URL)
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let list = resp.get("projects").and_then(Value::as_array);
    if let Some(arr) = list {
        for p in arr {
            let pid = p.get("projectId").and_then(Value::as_str).unwrap_or("");
            if pid.starts_with("gen-lang-client") {
                return Ok(Some(pid.to_string()));
            }
            if p.get("labels")
                .and_then(|l| l.get("generative-language"))
                .is_some()
            {
                return Ok(Some(pid.to_string()));
            }
        }
    }
    Ok(None)
}

fn tier_label(tid: Option<&str>, hd: Option<&str>) -> &'static str {
    match (tid, hd) {
        (Some("standard-tier"), _) => "Paid",
        (Some("free-tier"), Some(_)) => "Workspace",
        (Some("free-tier"), None) => "Free",
        (Some("legacy-tier"), _) => "Legacy",
        _ => "Unknown",
    }
}

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // fallback: naive RFC3339 variants
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S%.f%z",
    ] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Utc.from_local_datetime(&n).single();
        }
    }
    None
}

/// 把 `retrieveUserQuota` 响应里的 buckets 数组折叠为 `SubQuota`：
/// - 同 `modelId` 多条 bucket → 取 `remainingFraction` 最小的那一条（最紧的限额）
/// - 缺 `modelId` 或 `remainingFraction` 的条目跳过
/// - `used_percent = (1 - remainingFraction) * 100`，clamp 到 [0, 100]
/// - 结果按 `used_percent` 倒序排（最紧的在前）
fn parse_quota_buckets(buckets: &[Value]) -> Vec<SubQuota> {
    use std::collections::BTreeMap;
    let mut per_model: BTreeMap<String, (f64, Option<String>)> = BTreeMap::new();
    for b in buckets {
        let mid = match b.get("modelId").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let frac = match b.get("remainingFraction").and_then(Value::as_f64) {
            Some(f) => f,
            None => continue,
        };
        let reset = b.get("resetTime").and_then(Value::as_str).map(str::to_string);
        per_model
            .entry(mid)
            .and_modify(|e| {
                if frac < e.0 {
                    *e = (frac, reset.clone());
                }
            })
            .or_insert((frac, reset));
    }

    let mut out: Vec<SubQuota> = per_model
        .into_iter()
        .map(|(label, (frac, reset))| SubQuota {
            label,
            used_percent: (100.0 - frac * 100.0).clamp(0.0, 100.0),
            resets_at: reset.and_then(|s| parse_iso(&s)),
        })
        .collect();
    out.sort_by(|a, b| b.used_percent.partial_cmp(&a.used_percent).unwrap());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    #[test]
    fn tier_label_maps_all_known_combos() {
        assert_eq!(tier_label(Some("standard-tier"), None), "Paid");
        assert_eq!(tier_label(Some("standard-tier"), Some("example.com")), "Paid");
        assert_eq!(tier_label(Some("free-tier"), None), "Free");
        assert_eq!(tier_label(Some("free-tier"), Some("example.com")), "Workspace");
        assert_eq!(tier_label(Some("legacy-tier"), None), "Legacy");
        assert_eq!(tier_label(None, None), "Unknown");
        assert_eq!(tier_label(Some("mystery-tier"), None), "Unknown");
    }

    #[test]
    fn parse_iso_accepts_rfc3339_and_variants() {
        assert!(parse_iso("2026-05-01T00:00:00Z").is_some());
        assert!(parse_iso("2026-05-01T00:00:00.123Z").is_some());
        assert!(parse_iso("2026-05-01T00:00:00+08:00").is_some());
        assert!(parse_iso("not a date").is_none());
        assert!(parse_iso("").is_none());
    }

    #[test]
    fn jwt_decode_claims_reads_payload() {
        // 构造一个合法 JWT（只需要 header.payload，signature 可空串）
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"x@y.z","hd":"y.z"}"#);
        let token = format!("{header}.{payload}.");
        let claims = jwt_decode_claims(&token);
        assert_eq!(claims.get("email").and_then(Value::as_str), Some("x@y.z"));
        assert_eq!(claims.get("hd").and_then(Value::as_str), Some("y.z"));
    }

    #[test]
    fn jwt_decode_claims_malformed_returns_empty() {
        assert!(jwt_decode_claims("").is_empty());
        assert!(jwt_decode_claims("no-dots").is_empty());
        assert!(jwt_decode_claims("a.!!not-base64!!.c").is_empty());
    }

    #[test]
    fn parse_quota_buckets_keeps_minimum_fraction_per_model() {
        let buckets = vec![
            json!({ "modelId": "gemini-pro", "remainingFraction": 0.8, "resetTime": "2026-05-01T00:00:00Z" }),
            json!({ "modelId": "gemini-pro", "remainingFraction": 0.2, "resetTime": "2026-05-01T00:00:00Z" }),
            json!({ "modelId": "gemini-flash", "remainingFraction": 0.5 }),
            json!({ "modelId": "no-fraction" }),                 // 缺 fraction 丢弃
            json!({ "remainingFraction": 0.1 }),                 // 缺 modelId 丢弃
        ];
        let out = parse_quota_buckets(&buckets);
        assert_eq!(out.len(), 2);
        // 最紧的（pro, 取 0.2）排在前面
        assert_eq!(out[0].label, "gemini-pro");
        assert!((out[0].used_percent - 80.0).abs() < 1e-9);
        assert_eq!(out[1].label, "gemini-flash");
        assert!((out[1].used_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn parse_quota_buckets_empty_input_returns_empty() {
        assert!(parse_quota_buckets(&[]).is_empty());
    }
}
