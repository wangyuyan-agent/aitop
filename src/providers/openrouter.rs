//! OpenRouter — API key → GET /api/v1/auth/key 取余额 & 限额
//!
//! 凭证：env `OPENROUTER_API_KEY`（TODO：可选走 macOS Keychain）

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{Availability, Credits, Provider, Usage, http_client};

#[derive(Default)]
pub struct OpenRouter;

#[async_trait]
impl Provider for OpenRouter {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn detect(&self) -> Availability {
        match std::env::var("OPENROUTER_API_KEY") {
            Ok(v) if !v.trim().is_empty() => Availability::Ready,
            _ => Availability::Missing("未设置环境变量 OPENROUTER_API_KEY".into()),
        }
    }

    async fn fetch(&self) -> Result<Usage> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| anyhow!("未设置 OPENROUTER_API_KEY"))?;

        let resp: Value = http_client()?
            .get("https://openrouter.ai/api/v1/auth/key")
            .bearer_auth(&key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        build_usage(&resp, Utc::now())
    }
}

/// 纯函数：把 `/api/v1/auth/key` 响应转 [`Usage`]。抽出来便于 unit test。
///
/// 响应形如：
/// ```json
/// { "data": { "label": "...", "usage": <float>, "limit": <float|null>, "is_free_tier": bool } }
/// ```
fn build_usage(resp: &Value, now: DateTime<Utc>) -> Result<Usage> {
    let data = resp
        .get("data")
        .ok_or_else(|| anyhow!("响应缺少 data: {}", resp))?;
    let used = data.get("usage").and_then(Value::as_f64).unwrap_or(0.0);
    let limit = data.get("limit").and_then(Value::as_f64);

    let credits = limit.map(|lim| Credits {
        remaining: (lim - used).max(0.0),
        total: Some(lim),
        unit: "USD".to_string(),
    });

    Ok(Usage {
        provider: "OpenRouter".to_string(),
        source: "api".to_string(),
        account: data.get("label").and_then(Value::as_str).map(str::to_string),
        plan: None,
        session: None,
        weekly: None,
        credits,
        sub_quotas: Vec::new(),
        updated_at: now,
        note: if limit.is_none() {
            Some(format!("无限额 · 已用 ${:.2}", used))
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn epoch() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap()
    }

    #[test]
    fn paid_key_with_limit_produces_credits_and_no_note() {
        let resp = json!({
            "data": { "label": "aitop-test", "usage": 12.34, "limit": 100.0, "is_free_tier": false }
        });
        let u = build_usage(&resp, epoch()).unwrap();
        assert_eq!(u.account.as_deref(), Some("aitop-test"));
        let c = u.credits.unwrap();
        assert!((c.remaining - 87.66).abs() < 1e-6);
        assert_eq!(c.total, Some(100.0));
        assert_eq!(c.unit, "USD");
        assert!(u.note.is_none(), "有 limit 时不应附 note");
    }

    #[test]
    fn unlimited_key_has_no_credits_and_note_reports_used() {
        let resp = json!({
            "data": { "label": "unlimited-test", "usage": 5.5, "limit": null }
        });
        let u = build_usage(&resp, epoch()).unwrap();
        assert!(u.credits.is_none());
        let note = u.note.expect("无 limit 必须有 note");
        assert!(note.contains("$5.50"), "note 应带已用金额: {note}");
    }

    #[test]
    fn missing_data_field_returns_err() {
        let resp = json!({ "error": { "message": "Unauthorized" } });
        assert!(build_usage(&resp, epoch()).is_err());
    }

    #[test]
    fn missing_label_leaves_account_none() {
        let resp = json!({ "data": { "usage": 0.0, "limit": 10.0 } });
        let u = build_usage(&resp, epoch()).unwrap();
        assert!(u.account.is_none());
    }

    #[test]
    fn usage_over_limit_clamps_remaining_to_zero() {
        // OpenRouter 偶尔把即将扣费的量算超一点，防止负数。
        let resp = json!({ "data": { "usage": 15.0, "limit": 10.0 } });
        let c = build_usage(&resp, epoch()).unwrap().credits.unwrap();
        assert!(c.remaining >= 0.0);
    }
}
