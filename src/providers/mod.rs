//! Provider 抽象层。
//!
//! 每个 provider 实现 [`Provider`] trait，返回统一的 [`Usage`] 结构。
//! TUI 和 CLI（oneshot / json）都基于这里的数据建模。

use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod kiro;
pub mod openrouter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    /// 已用百分比 (0..=100)
    pub used_percent: f64,
    /// 窗口长度（分钟），例如 5h=300, 7d=10080
    pub window_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credits {
    pub remaining: f64,
    pub total: Option<f64>,
    pub unit: String, // "USD" | "credits" | ...
}

/// 子配额 — 用于多模型/多维度 provider（如 Gemini 的 pro/flash/flash-lite）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubQuota {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub provider: String,
    pub source: String, // "api" | "oauth" | "cli" | "cookie"
    pub account: Option<String>,
    pub plan: Option<String>,
    pub session: Option<Window>,
    pub weekly: Option<Window>,
    pub credits: Option<Credits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_quotas: Vec<SubQuota>,
    pub updated_at: DateTime<Utc>,
    pub note: Option<String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn fetch(&self) -> Result<Usage>;
}

pub fn all_providers() -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(openrouter::OpenRouter::default()),
        Arc::new(gemini::Gemini::default()),
        Arc::new(codex::Codex::default()),
        Arc::new(claude::Claude::default()),
        Arc::new(copilot::Copilot::default()),
        Arc::new(kiro::Kiro::default()),
    ]
}

pub fn select(filter: &str) -> Result<Vec<Arc<dyn Provider>>> {
    let all = all_providers();
    if filter == "all" {
        return Ok(all);
    }
    let wanted: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    let mut out = Vec::new();
    for p in all {
        if wanted.iter().any(|w| *w == p.id()) {
            out.push(p);
        }
    }
    if out.is_empty() {
        return Err(anyhow!("未匹配任何 provider：{}", filter));
    }
    Ok(out)
}

pub async fn oneshot_text(filter: &str) -> Result<()> {
    let providers = select(filter)?;
    for p in providers {
        match p.fetch().await {
            Ok(u) => println!("{}", render_text(&u)),
            Err(e) => eprintln!("[{}] 获取失败: {:#}", p.id(), e),
        }
    }
    Ok(())
}

pub async fn oneshot_json(filter: &str, pretty: bool) -> Result<()> {
    let providers = select(filter)?;
    let mut results: Vec<serde_json::Value> = Vec::new();
    for p in providers {
        match p.fetch().await {
            Ok(u) => results.push(serde_json::to_value(&u)?),
            Err(e) => results.push(serde_json::json!({
                "provider": p.id(),
                "error": e.to_string(),
            })),
        }
    }
    let out = if pretty {
        serde_json::to_string_pretty(&results)?
    } else {
        serde_json::to_string(&results)?
    };
    println!("{}", out);
    Ok(())
}

fn render_text(u: &Usage) -> String {
    let mut lines = vec![format!("== {} ({}) ==", u.provider, u.source)];
    if let Some(acc) = &u.account {
        lines.push(format!("  账号: {}", acc));
    }
    if let Some(plan) = &u.plan {
        lines.push(format!("  方案: {}", plan));
    }
    if let Some(s) = &u.session {
        lines.push(format!("  Session: {:.0}% used", s.used_percent));
    }
    if let Some(w) = &u.weekly {
        lines.push(format!("  Weekly:  {:.0}% used", w.used_percent));
    }
    if let Some(c) = &u.credits {
        lines.push(format!("  Credits: {:.2} {}", c.remaining, c.unit));
    }
    if !u.sub_quotas.is_empty() {
        lines.push("  子配额:".to_string());
        for sq in &u.sub_quotas {
            lines.push(format!("    {:18} {:5.1}% used", sq.label, sq.used_percent));
        }
    }
    if let Some(n) = &u.note {
        lines.push(format!("  Note: {}", n));
    }
    lines.join("\n")
}
