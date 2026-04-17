//! Kiro — spawn `kiro-cli chat --no-interactive /usage`，解析文本输出。
//!
//! `/usage` 不是顶层子命令，而是 chat 里的 client-side slash command；
//! `chat --no-interactive` 把它在非交互模式下执行后即刻退出。
//!
//! 典型输出（带 ANSI 控制字符，需先剥离）：
//!
//! ```text
//! Estimated Usage | resets on 2026-05-01 | KIRO STUDENT
//! Credits (951.38 of 1000 covered in plan)
//! ████████████████████████████████████████████████████████████████████████████ 95%
//! Overages: Disabled
//! ```
//!
//! 解析：
//! - `Credits \(([\d.]+)\s*of\s*([\d.]+)` → used / total
//! - `(\d+)\s*%\s*$`（按行）→ Session used_percent
//! - `resets on (\d{4}-\d{2}-\d{2})` → resets_at（取当天 00:00 UTC）
//! - `\|\s*([A-Z][A-Z\s]+?)\s*(?:\||$)` 取首行管道段中的计划名

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
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
            .args(["chat", "--no-interactive", "/usage"])
            .output()
            .await
            .map_err(|e| anyhow!("无法调用 {}: {}（请确认 kiro-cli 已在 PATH）", bin, e))?;
        if !out.status.success() {
            bail!(
                "{} chat --no-interactive /usage 失败 (exit={:?}): {}",
                bin,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        // kiro-cli 把 /usage 面板写到 stderr（slash command 不是 chat 正文）；
        // stdout 通常是空，但两边都扫一下保险。
        let raw_stdout = String::from_utf8_lossy(&out.stdout);
        let raw_stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{}\n{}", raw_stdout, raw_stderr);
        let text = strip_ansi(&combined);

        // Credits (951.38 of 1000 covered in plan)
        let re_credits = Regex::new(r"Credits\s*\(\s*([\d.]+)\s*of\s*([\d.]+)").unwrap();
        // 百分比行：行末「 95%」
        let re_pct = Regex::new(r"(?m)(\d+(?:\.\d+)?)\s*%\s*$").unwrap();
        // resets on 2026-05-01
        let re_reset = Regex::new(r"resets\s+on\s+(\d{4}-\d{2}-\d{2})").unwrap();
        // 首行尾部的 plan：| KIRO STUDENT
        let re_plan = Regex::new(r"\|\s*([A-Z][A-Z0-9 ]+?)\s*$").unwrap();

        let credits = re_credits.captures(&text).and_then(|c| {
            let used: f64 = c[1].parse().ok()?;
            let total: f64 = c[2].parse().ok()?;
            Some(Credits {
                remaining: (total - used).max(0.0),
                total: Some(total),
                unit: "credits".to_string(),
            })
        });

        let resets_at = re_reset.captures(&text).and_then(|c| {
            NaiveDate::parse_from_str(&c[1], "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc())
        });

        // session used_percent：优先走 credits 比例；否则解析 "NN%"
        let session_pct = credits
            .as_ref()
            .and_then(|c| c.total.map(|t| if t > 0.0 { (1.0 - c.remaining / t) * 100.0 } else { 0.0 }))
            .or_else(|| {
                re_pct.captures_iter(&text).last().and_then(|cap| {
                    cap.get(1)?.as_str().parse::<f64>().ok()
                })
            });

        let session = session_pct.map(|pct| Window {
            used_percent: pct.clamp(0.0, 100.0),
            window_minutes: None,
            resets_at,
        });

        let plan = text
            .lines()
            .find(|l| l.contains("Estimated Usage"))
            .and_then(|l| re_plan.captures(l))
            .map(|c| c[1].trim().to_string());

        let has_data = session.is_some() || credits.is_some();

        Ok(Usage {
            provider: "Kiro".to_string(),
            source: "cli".to_string(),
            account: None,
            plan,
            session,
            weekly: None,
            credits,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note: if !has_data {
                Some(format!(
                    "无法解析 kiro-cli 输出，请贴给开发者：\n{}",
                    text.trim()
                ))
            } else {
                None
            },
        })
    }
}

/// 剥离 ANSI CSI 序列，保留纯文本。只处理 `\x1b[...<letter>`。
fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap());
    re.replace_all(s, "").into_owned()
}
