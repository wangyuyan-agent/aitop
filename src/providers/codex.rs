//! Codex — OpenAI Codex CLI 的 `~/.codex/auth.json` OAuth 会话
//!
//! ChatGPT / Codex 订阅的"剩余消息数"没有公开 API——OpenAI 只在 platform.openai.com
//! 的 dashboard 上暴露按 API key 的 token 用量。Codex CLI 用的是 OAuth JWT，
//! scope 跑不到 `/v1/dashboard/billing/usage` 之类的端点。
//!
//! 因此本 provider 做最保守、可验证的事：
//! 1. detect：检查 `~/.codex/auth.json` 存在并可解析。
//! 2. fetch：解码 `tokens.id_token` 的 JWT claims，抽 email / plan / org：
//!    - `email` → account
//!    - `https://api.openai.com/auth.chatgpt_plan_type` → plan
//!    - `https://api.openai.com/auth.chatgpt_account_id` → note
//! 3. 用 `last_refresh` 时间戳提示 token 新鲜度；若 JWT 已过期则给出警告。
//!
//! 未来若 OpenAI 公开 Codex session / message 配额 API，fetch 在这里扩即可。

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::{Availability, Provider, Usage};

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
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {:?} 失败", path))?;
        let auth: AuthFile = serde_json::from_str(&text)
            .with_context(|| format!("解析 {:?} 失败", path))?;

        let tokens = auth
            .tokens
            .ok_or_else(|| anyhow!("auth.json 中 tokens 为空，请 `codex login` 重新登录"))?;
        let id_token = tokens
            .id_token
            .as_deref()
            .ok_or_else(|| anyhow!("auth.json 中缺少 id_token"))?;

        let claims = decode_jwt_claims(id_token)
            .ok_or_else(|| anyhow!("id_token 非法 JWT（无法 base64/JSON 解析）"))?;

        let email = claims.get("email").and_then(Value::as_str).map(str::to_string);

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

        // JWT 过期 / 刷新时间
        let exp = claims.get("exp").and_then(Value::as_i64);
        let now = Utc::now().timestamp();
        let mut note_parts: Vec<String> = Vec::new();
        if let Some(id) = &chatgpt_account {
            note_parts.push(format!("account_id={}", short_id(id)));
        }
        if let Some(exp_ts) = exp {
            if exp_ts < now {
                note_parts.push(format!("⚠ id_token expired {}s ago", now - exp_ts));
            } else {
                let mins = (exp_ts - now) / 60;
                note_parts.push(format!("id_token valid for {}m", mins));
            }
        }
        if let Some(refresh) = auth.last_refresh.as_deref()
            && let Ok(ts) = DateTime::parse_from_rfc3339(refresh) {
                let ago = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
                note_parts.push(format!("refreshed {}h ago", ago.num_hours()));
            }
        note_parts.push("usage API 未公开".to_string());

        Ok(Usage {
            provider: "Codex".to_string(),
            source: "oauth".to_string(),
            account: email,
            plan: plan_type,
            session: None,
            weekly: None,
            credits: None,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note: Some(note_parts.join(" · ")),
        })
    }
}

fn auth_path() -> PathBuf {
    auth_path_with(std::env::var("CODEX_HOME").ok().as_deref(), dirs::home_dir())
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
        assert_eq!(claims.get("email").and_then(Value::as_str), Some("user@example.com"));
        let oa = claims.get("https://api.openai.com/auth").unwrap();
        assert_eq!(oa.get("chatgpt_plan_type").and_then(Value::as_str), Some("plus"));
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
    fn auth_path_handles_missing_home() {
        // 没有 CODEX_HOME 也没有 home_dir —— 落到 `/.codex/auth.json`，至少不 panic
        let p = auth_path_with(None, None);
        assert_eq!(p, Path::new("/.codex/auth.json"));
    }
}
