use anyhow::Result;
use clap::{Parser, Subcommand};

mod lang;
mod providers;
mod ui;

use lang::Lang;

// Load all translations from `locales/*.yml` at compile time.
// Fallback locale = English, matching `[package.metadata.i18n] default-locale`.
rust_i18n::i18n!("locales", fallback = "en");

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
    /// 日志级别 / log level (trace|debug|info|warn|error)
    #[arg(long, global = true, default_value = "warn")]
    log_level: String,

    /// 界面语言 / UI language (en|zh-CN|zh-TW). Defaults to AITOP_LANG / LANG.
    #[arg(long, global = true, value_name = "LANG")]
    lang: Option<String>,

    /// 显示全部 provider（含未配置的）/ show every provider, even unconfigured
    #[arg(long)]
    all: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 单次拉取并以文本打印后退出
    Oneshot {
        /// provider 选择：`auto`（本地已配置）/ `all` / 逗号分隔 id
        #[arg(long, default_value = "auto")]
        provider: String,
    },
    /// 以 JSON 输出（适合脚本/CI）
    Json {
        #[arg(long, default_value = "auto")]
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

    let lang = cli
        .lang
        .as_deref()
        .and_then(Lang::parse)
        .unwrap_or_else(Lang::detect);
    lang.apply();

    match cli.cmd {
        None => ui::run_tui(cli.all).await,
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
