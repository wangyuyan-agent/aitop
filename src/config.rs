use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh_interval_secs: u64,
    pub sort: SortMode,
    pub show_accounts: bool,
    pub status_enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortMode {
    #[default]
    Risk,
    Name,
    Original,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval_secs: 300,
            sort: SortMode::Risk,
            show_accounts: true,
            status_enabled: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("AITOP_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    let base = dirs::config_dir().or_else(dirs::home_dir)?;
    Some(base.join("aitop").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_conservative() {
        let cfg = Config::default();
        assert_eq!(cfg.refresh_interval_secs, 300);
        assert_eq!(cfg.sort, SortMode::Risk);
        assert!(cfg.show_accounts);
        assert!(cfg.status_enabled);
    }

    #[test]
    fn parses_sort_mode_kebab_case() {
        let cfg: Config = toml::from_str("sort = 'original'\nshow_accounts = false").unwrap();
        assert_eq!(cfg.sort, SortMode::Original);
        assert!(!cfg.show_accounts);
        assert_eq!(cfg.refresh_interval_secs, 300);
    }
}
