//! TUI 层 — 基于 ratatui + crossterm。
//!
//! 待办：
//! - provider 列表 + 进度条
//! - 详情面板（展开 session / weekly / credits）
//! - 键位：↑↓ 导航 / enter 展开 / r 刷新 / j 导出 JSON / q 退出
//! - 后台定时刷新（tokio::spawn + interval）

use anyhow::Result;

pub async fn run_tui() -> Result<()> {
    eprintln!("[aitop] TUI 尚未实现。请先用：aitop oneshot  或  aitop json --pretty");
    Ok(())
}

pub async fn run_watch(provider: &str, interval: u64) -> Result<()> {
    eprintln!(
        "[aitop] watch 尚未实现（provider={}, interval={}s）",
        provider, interval
    );
    Ok(())
}
