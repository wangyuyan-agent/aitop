//! Provider 抽象层。
//!
//! 每个 provider 实现 [`Provider`] trait，返回统一的 [`Usage`] 结构。
//! TUI 和 CLI（oneshot / json）都基于这里的数据建模。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod kiro;
pub mod openrouter;

/// 所有 provider 共享的 HTTP 请求 UA。GitHub 等家要求 UA 非空，这里给一个可追溯的串。
pub(crate) const HTTP_USER_AGENT: &str = concat!(
    "aitop/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/wangyuyan-agent/aitop)"
);

/// 所有 provider 共享的 HTTP 请求超时（总超时 = 连接 + 读）。
/// 目标：任何单家 provider 卡住也不拖累 TUI 的并发刷新。
pub(crate) const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// 构造一个带统一 UA / 超时的 `reqwest::Client`。各 provider 自己的 HTTP 调用都走这里。
pub(crate) fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(HTTP_USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("构建 HTTP client 失败")
}

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

/// 本地可用性探测结果 —— detect() 只做本地 I/O（文件/env/which），不发网络。
#[derive(Debug, Clone)]
#[allow(dead_code)] // 原因串保留，TUI 后续可能展示
pub enum Availability {
    /// 凭证/二进制就绪，fetch 大概率能工作
    Ready,
    /// 明确未配置，附原因（默认视图中会被过滤掉）
    Missing(String),
    /// 无法本地判断（例如需要实际调 API 才知道）
    Unknown,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn detect(&self) -> Availability {
        Availability::Unknown
    }
    async fn fetch(&self) -> Result<Usage>;
}

pub fn all_providers() -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(openrouter::OpenRouter),
        Arc::new(gemini::Gemini),
        Arc::new(codex::Codex),
        Arc::new(claude::Claude),
        Arc::new(copilot::Copilot),
        Arc::new(kiro::Kiro),
    ]
}

/// 过滤掉 `Missing` 的 provider，保留 `Ready` 与 `Unknown`。
/// 用于 TUI / CLI 的默认视图。
pub fn available_providers() -> Vec<Arc<dyn Provider>> {
    all_providers()
        .into_iter()
        .filter(|p| !matches!(p.detect(), Availability::Missing(_)))
        .collect()
}

/// filter 语义：
/// - `"all"` → 全部 provider
/// - `"auto"` → 仅本地探测到的（等价 available_providers）
/// - 逗号分隔 id → 精确匹配
pub fn select(filter: &str) -> Result<Vec<Arc<dyn Provider>>> {
    if filter == "all" {
        return Ok(all_providers());
    }
    if filter == "auto" {
        let out = available_providers();
        if out.is_empty() {
            return Err(anyhow!(t!("no_auto_provider").into_owned()));
        }
        return Ok(out);
    }
    let all = all_providers();
    let wanted: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    let mut out = Vec::new();
    for p in all {
        if wanted.iter().any(|w| *w == p.id()) {
            out.push(p);
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            t!("no_match_provider", filter = filter).into_owned()
        ));
    }
    Ok(out)
}

pub async fn oneshot_text(filter: &str) -> Result<()> {
    let providers = select(filter)?;
    let fail_label = t!("fetch_failed");
    for p in providers {
        match p.fetch().await {
            Ok(u) => println!("{}", render_text(&u)),
            Err(e) => eprintln!("[{}] {}: {:#}", p.id(), fail_label, e),
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
        lines.push(format!("  {}: {}", t!("label_account"), acc));
    }
    if let Some(plan) = &u.plan {
        lines.push(format!("  {}: {}", t!("label_plan"), plan));
    }
    if let Some(s) = &u.session {
        lines.push(format!(
            "  {}: {:.0}% used",
            t!("label_session"),
            s.used_percent
        ));
    }
    if let Some(w) = &u.weekly {
        lines.push(format!(
            "  {}:  {:.0}% used",
            t!("label_weekly"),
            w.used_percent
        ));
    }
    if let Some(c) = &u.credits {
        lines.push(format!(
            "  {}: {:.2} {}",
            t!("label_credits"),
            c.remaining,
            c.unit
        ));
    }
    if !u.sub_quotas.is_empty() {
        lines.push(format!("  {}:", t!("label_subquotas")));
        for sq in &u.sub_quotas {
            lines.push(format!("    {:18} {:5.1}% used", sq.label, sq.used_percent));
        }
    }
    if let Some(n) = &u.note {
        lines.push(format!("  Note: {}", n));
    }
    lines.join("\n")
}
