//! Codex — OpenAI 浏览器 cookie / OAuth 会话
//!
//! 待办：从浏览器 cookie jar 读 chatgpt.com session，
//! 调 OpenAI dashboard 的 usage_limits / credits。
//! macOS 上用 rookie crate 读 Chrome/Safari cookie。

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use super::{Availability, Provider, Usage};

#[derive(Default)]
pub struct Codex;

#[async_trait]
impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> Availability {
        Availability::Missing("尚未实现（TODO：浏览器 cookie jar + dashboard API）".into())
    }

    async fn fetch(&self) -> Result<Usage> {
        Err(anyhow!("codex provider 尚未实现（TODO：浏览器 cookie jar + dashboard API）"))
    }
}
