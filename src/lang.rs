//! Language detection and selection for aitop.
//!
//! The actual translations live in `locales/*.yml`, loaded at compile time by
//! the `rust_i18n::i18n!` macro in `main.rs`. This module only:
//!
//! 1. Defines the supported [`Lang`] variants (BCP 47 codes).
//! 2. Parses user input (`--lang zh-CN` / `--lang zh-TW` / `zh_cn` / …).
//! 3. Auto-detects from `AITOP_LANG` / `LANG` / `LC_ALL` when no flag is given.
//! 4. Pushes the chosen locale into `rust_i18n` via [`Lang::apply`].
//!
//! To add another language: add a variant, a `parse`/`detect`/`code` arm,
//! and a column in every `locales/*.yml` entry. No message functions here —
//! call `t!("key")` at the use-site.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lang {
    /// English (default)
    En,
    /// 简体中文
    ZhCn,
    /// 繁體中文 (Taiwan-style)
    ZhTw,
}

impl Lang {
    /// BCP 47 code used as the locale key in YAML files.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhCn => "zh-CN",
            Lang::ZhTw => "zh-TW",
        }
    }

    /// Parse a CLI argument or env value. Returns `None` for unrecognized input
    /// so callers can fall back to [`detect`](Self::detect).
    pub fn parse(s: &str) -> Option<Self> {
        // Normalize: lower-case, collapse separators so "zh_TW" == "zh-tw" == "zhtw".
        let norm = s.trim().to_lowercase().replace(['_', '.'], "-");
        match norm.as_str() {
            "en" | "en-us" | "en-gb" | "english" => Some(Lang::En),
            "zh" | "zh-cn" | "zh-hans" | "zh-hans-cn" | "chinese" | "cn" | "zhcn" => {
                Some(Lang::ZhCn)
            }
            "zh-tw" | "zh-hk" | "zh-hant" | "zh-hant-tw" | "zh-hant-hk" | "tw" | "zhtw" => {
                Some(Lang::ZhTw)
            }
            _ => None,
        }
    }

    /// Auto-detect: `AITOP_LANG` first (explicit user pref), then `LANG` / `LC_ALL`.
    /// Default: English.
    pub fn detect() -> Self {
        if let Ok(v) = std::env::var("AITOP_LANG")
            && let Some(l) = Self::parse(&v)
        {
            return l;
        }
        let lang = std::env::var("LANG").unwrap_or_default();
        let lc_all = std::env::var("LC_ALL").unwrap_or_default();
        let joined = format!("{} {}", lang, lc_all).to_lowercase();
        if joined.contains("zh-tw")
            || joined.contains("zh_tw")
            || joined.contains("zh-hant")
            || joined.contains("zh_hant")
        {
            Lang::ZhTw
        } else if joined.contains("zh") {
            Lang::ZhCn
        } else {
            Lang::En
        }
    }

    /// Push this locale into `rust_i18n`'s global state so `t!()` picks it up.
    pub fn apply(self) {
        rust_i18n::set_locale(self.code());
    }
}
