//! OpenRouter — API key → GET /api/v1/auth/key 取余额 & 限额
//!
//! 凭证：env `OPENROUTER_API_KEY`（TODO：可选走 macOS Keychain）

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::Utc;

use super::{Availability, Credits, Provider, Usage};

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

        let resp: serde_json::Value = reqwest::Client::new()
            .get("https://openrouter.ai/api/v1/auth/key")
            .bearer_auth(&key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // OpenRouter 返回形如：
        // { "data": { "label": "...", "usage": <float>, "limit": <float|null>, "is_free_tier": bool } }
        let data = resp
            .get("data")
            .ok_or_else(|| anyhow!("响应缺少 data: {}", resp))?;
        let used = data.get("usage").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let limit = data.get("limit").and_then(|v| v.as_f64());

        let credits = limit.map(|lim| Credits {
            remaining: (lim - used).max(0.0),
            total: Some(lim),
            unit: "USD".to_string(),
        });

        Ok(Usage {
            provider: "OpenRouter".to_string(),
            source: "api".to_string(),
            account: data
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            plan: None,
            session: None,
            weekly: None,
            credits,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note: if limit.is_none() {
                Some(format!("无限额 · 已用 ${:.2}", used))
            } else {
                None
            },
        })
    }
}
