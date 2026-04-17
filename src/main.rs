use anyhow::Result;
use clap::{Parser, Subcommand};

mod providers;
mod ui;

#[derive(Parser, Debug)]
#[command(
    name = "aitop",
    version,
    about = "AI 额度监控 — openrouter / gemini / codex / claude / copilot / kiro",
    long_about = "\
默认进入交互式 TUI。脚本场景用 `aitop oneshot` 或 `aitop json`。

支持的 provider：openrouter, gemini, codex, claude, copilot, kiro
"
)]
struct Cli {
    /// 日志级别（trace|debug|info|warn|error）
    #[arg(long, global = true, default_value = "warn")]
    log_level: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 单次拉取并以文本打印后退出
    Oneshot {
        /// 指定 provider（逗号分隔，或 `all`）
        #[arg(long, default_value = "all")]
        provider: String,
    },
    /// 以 JSON 输出（适合脚本/CI）
    Json {
        #[arg(long, default_value = "all")]
        provider: String,
        /// pretty 打印
        #[arg(long)]
        pretty: bool,
    },
    /// 盯住一个 provider 持续刷新
    Watch {
        provider: String,
        /// 刷新间隔（秒）
        #[arg(long, default_value_t = 60)]
        interval: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log_level);

    match cli.cmd {
        None => ui::run_tui().await,
        Some(Cmd::Oneshot { provider }) => providers::oneshot_text(&provider).await,
        Some(Cmd::Json { provider, pretty }) => providers::oneshot_json(&provider, pretty).await,
        Some(Cmd::Watch { provider, interval }) => ui::run_watch(&provider, interval).await,
    }
}

fn init_tracing(level: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}
