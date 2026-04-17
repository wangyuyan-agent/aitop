//! Bilingual message table for aitop (zh / en).
//!
//! Language is auto-detected from `LANG` / `LC_ALL` / `AITOP_LANG`; override at runtime
//! with `aitop --lang <zh|en>`. Only user-facing strings (startup hints, CLI errors,
//! TUI header/footer labels) go through this module — internal errors stay raw so
//! `RUST_LOG=debug` remains useful for bug reports.

#![allow(dead_code)] // full label set is public API; not every label is wired to UI yet

use std::sync::OnceLock;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn detect() -> Self {
        if let Ok(v) = std::env::var("AITOP_LANG") {
            if let Some(l) = Self::parse(&v) {
                return l;
            }
        }
        let lang = std::env::var("LANG").unwrap_or_default();
        let lc_all = std::env::var("LC_ALL").unwrap_or_default();
        let joined = format!("{} {}", lang, lc_all).to_lowercase();
        if joined.contains("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "zh" | "zh-cn" | "zh_cn" | "chinese" | "cn" => Some(Lang::Zh),
            "en" | "en-us" | "en_us" | "english" => Some(Lang::En),
            _ => None,
        }
    }
}

static GLOBAL_LANG: OnceLock<Lang> = OnceLock::new();

/// Set once at startup; subsequent calls are ignored.
pub fn set(lang: Lang) {
    let _ = GLOBAL_LANG.set(lang);
}

pub fn get() -> Lang {
    *GLOBAL_LANG.get_or_init(Lang::detect)
}

// ---------- TUI labels ----------

pub fn app_title() -> &'static str {
    "aitop"
}

pub fn hint_quit() -> &'static str {
    match get() {
        Lang::Zh => "退出",
        Lang::En => "quit",
    }
}

pub fn hint_refresh() -> &'static str {
    match get() {
        Lang::Zh => "刷新",
        Lang::En => "refresh",
    }
}

pub fn hint_nav() -> &'static str {
    match get() {
        Lang::Zh => "导航",
        Lang::En => "nav",
    }
}

pub fn status_refreshing(remaining: usize) -> String {
    match get() {
        Lang::Zh => format!("刷新中…（剩 {}）", remaining),
        Lang::En => format!("refreshing… ({} left)", remaining),
    }
}

pub fn status_updated_ago(seconds: i64) -> String {
    match get() {
        Lang::Zh => format!("{}s 前更新", seconds.max(0)),
        Lang::En => format!("updated {}s ago", seconds.max(0)),
    }
}

pub fn loading() -> &'static str {
    match get() {
        Lang::Zh => "加载中…",
        Lang::En => "loading…",
    }
}

// ---------- CLI messages ----------

pub fn no_providers() -> &'static str {
    match get() {
        Lang::Zh => "[aitop] 没有可用的 provider",
        Lang::En => "[aitop] no providers available",
    }
}

pub fn no_detected_providers() -> &'static str {
    match get() {
        Lang::Zh => "[aitop] 本地未探测到已配置的 provider；用 `--all` 显示全部，或参考 README 配置凭证",
        Lang::En => "[aitop] no configured providers detected locally; use `--all` to show all, or see README to configure credentials",
    }
}

pub fn label_session() -> &'static str {
    match get() {
        Lang::Zh => "会话",
        Lang::En => "Session",
    }
}

pub fn label_weekly() -> &'static str {
    match get() {
        Lang::Zh => "每周",
        Lang::En => "Weekly",
    }
}

pub fn label_credits() -> &'static str {
    match get() {
        Lang::Zh => "余额",
        Lang::En => "Credits",
    }
}

pub fn label_account() -> &'static str {
    match get() {
        Lang::Zh => "账号",
        Lang::En => "Account",
    }
}

pub fn label_plan() -> &'static str {
    match get() {
        Lang::Zh => "方案",
        Lang::En => "Plan",
    }
}

pub fn label_subquotas() -> &'static str {
    match get() {
        Lang::Zh => "子配额",
        Lang::En => "Sub-quotas",
    }
}
