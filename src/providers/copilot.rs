//! GitHub Copilot — device flow + Copilot 内部 usage API
//!
//! 凭证优先级：env `COPILOT_API_TOKEN` → macOS Keychain → gh CLI token。

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use super::{Provider, Usage};

#[derive(Default)]
pub struct Copilot;

#[async_trait]
impl Provider for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    async fn fetch(&self) -> Result<Usage> {
        Err(anyhow!("copilot provider 尚未实现（TODO：device flow + internal usage API）"))
    }
}
