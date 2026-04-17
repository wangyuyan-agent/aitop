//! Kiro — spawn `kiro-cli /usage`，解析文本输出
//!
//! 输出格式未固化，先把 stdout 原样塞 note，再用正则尝试提取：
//! - `(\d+)\s*/\s*(\d+)\s*credits?` → Credits（remaining / total）
//! - `Credits:\s*(\d+)%` → Session used_percent

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;

use super::{Availability, Credits, Provider, Usage, Window};

#[derive(Default)]
pub struct Kiro;

#[async_trait]
impl Provider for Kiro {
    fn id(&self) -> &'static str {
        "kiro"
    }

    fn detect(&self) -> Availability {
        let bin = std::env::var("KIRO_CLI_BIN").unwrap_or_else(|_| "kiro-cli".into());
        if which::which(&bin).is_ok() {
            Availability::Ready
        } else {
            Availability::Missing(format!("{} 不在 PATH（可通过 KIRO_CLI_BIN 指定）", bin))
        }
    }

    async fn fetch(&self) -> Result<Usage> {
        let bin = std::env::var("KIRO_CLI_BIN").unwrap_or_else(|_| "kiro-cli".into());
        let out = tokio::process::Command::new(&bin)
            .arg("/usage")
            .output()
            .await
            .map_err(|e| anyhow!("无法调用 {}: {}（请确认 kiro-cli 已在 PATH）", bin, e))?;
        if !out.status.success() {
            bail!(
                "{} /usage 失败 (exit={:?}): {}",
                bin,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();

        // 粗粒度抽取：兼容"60/100 credits" / "Credits: 60%" 等格式
        let re_ratio = Regex::new(r"(\d+)\s*/\s*(\d+)\s*credits?").unwrap();
        let re_pct = Regex::new(r"(?i)credits?[^0-9]*(\d+)\s*%").unwrap();

        let credits = re_ratio.captures(&text).and_then(|c| {
            let used: f64 = c[1].parse().ok()?;
            let total: f64 = c[2].parse().ok()?;
            Some(Credits {
                remaining: (total - used).max(0.0),
                total: Some(total),
                unit: "credits".to_string(),
            })
        });
        let session = re_pct.captures(&text).and_then(|c| {
            let pct: f64 = c[1].parse().ok()?;
            Some(Window {
                used_percent: 100.0 - pct,
                window_minutes: None,
                resets_at: None,
            })
        });

        let has_data = session.is_some() || credits.is_some();

        Ok(Usage {
            provider: "Kiro".to_string(),
            source: "cli".to_string(),
            account: None,
            plan: None,
            session,
            weekly: None,
            credits,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note: if !has_data {
                Some(format!("无法解析输出，请贴给开发者：\n{}", text.trim()))
            } else {
                None
            },
        })
    }
}
