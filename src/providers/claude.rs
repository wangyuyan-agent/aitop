//! Claude Code — OAuth access token（Keychain）或浏览器 cookie
//!
//! 待办：
//! - 从 macOS Keychain 读 "Claude Code-credentials"
//! - 调 claude.ai API 取 session + weekly usage

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use super::{Provider, Usage};

#[derive(Default)]
pub struct Claude;

#[async_trait]
impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    async fn fetch(&self) -> Result<Usage> {
        Err(anyhow!("claude provider 尚未实现（TODO：Keychain OAuth → claude.ai API）"))
    }
}
