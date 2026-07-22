//! Kiro — spawn `kiro-cli chat --no-interactive /usage`，解析文本输出；
//! 另外若本机装有 `kiro-pool`（kiro-multi 的多账号轮转池），额外拉
//! `kiro-pool usage --json` 把池中每个 profile 的 credits 显示为 SubQuota。
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
//!
//! `kiro-pool usage --json` 输出（stdout，一行一个 profile 汇总在 usage 数组）：
//!
//! ```json
//! {"usage":[{"name":"student_2","plan":"KIRO STUDENT","credits_total":1000.0,
//!            "credits_used":398.74,"used_percent":39.87,"resets_at":"2026-08-01"}]}
//! ```

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use regex::Regex;
use serde_json::Value;

use super::{Availability, Credits, Provider, SubQuota, Usage, Window};

/// `/usage` 输出解析结果。fetch 调用链上把它组装进 [`Usage`]。
#[derive(Debug, Default, Clone)]
struct ParsedKiro {
    plan: Option<String>,
    session: Option<Window>,
    credits: Option<Credits>,
}

#[derive(Default)]
pub struct Kiro;

#[async_trait]
impl Provider for Kiro {
    fn id(&self) -> &'static str {
        "kiro"
    }

    fn detect(&self) -> Availability {
        let bin = std::env::var("KIRO_CLI_BIN").unwrap_or_else(|_| "kiro-cli".into());
        if which::which(&bin).is_ok() || which::which(pool_bin()).is_ok() {
            Availability::Ready
        } else {
            Availability::Missing(format!(
                "{} / kiro-pool 都不在 PATH（可通过 KIRO_CLI_BIN / KIRO_POOL_BIN 指定）",
                bin
            ))
        }
    }

    async fn fetch(&self) -> Result<Usage> {
        // 当前账号（kiro-cli）与多账号池（kiro-pool）并发拉取；任一失败不拖累另一边
        let (current, pool) = tokio::join!(fetch_current_account(), fetch_pool());

        let parsed = current.unwrap_or_default();
        let (sub_quotas, pool_note) = pool.unwrap_or_default();

        let has_data =
            parsed.session.is_some() || parsed.credits.is_some() || !sub_quotas.is_empty();
        if !has_data {
            bail!("kiro-cli 与 kiro-pool 均未取到数据（各自可能未安装或未登录）");
        }

        let mut note_parts: Vec<String> = Vec::new();
        if parsed.session.is_none() && parsed.credits.is_none() {
            note_parts.push("当前账号 /usage 不可用".to_string());
        }
        if let Some(n) = pool_note {
            note_parts.push(n);
        }

        Ok(Usage {
            provider: "Kiro".to_string(),
            source: "cli".to_string(),
            account: None,
            plan: parsed.plan,
            session: parsed.session,
            weekly: None,
            credits: parsed.credits,
            reset_credits: None,
            sub_quotas,
            costs: Vec::new(),
            status: None,
            updated_at: Utc::now(),
            note: if note_parts.is_empty() {
                None
            } else {
                Some(note_parts.join(" · "))
            },
        })
    }
}

fn pool_bin() -> String {
    std::env::var("KIRO_POOL_BIN").unwrap_or_else(|_| "kiro-pool".into())
}

/// 当前登录账号：`kiro-cli chat --no-interactive /usage` 文本解析。
async fn fetch_current_account() -> Result<ParsedKiro> {
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
    Ok(parse_usage_output(&strip_ansi(&combined)))
}

/// 多账号池：`kiro-pool usage --json` → 每个 profile 一条 SubQuota + 汇总 note。
/// pool 会逐个 profile 发请求，账号多时偏慢 → 120s 兜底超时。
async fn fetch_pool() -> Result<(Vec<SubQuota>, Option<String>)> {
    let bin = pool_bin();
    if which::which(&bin).is_err() {
        return Ok((Vec::new(), None));
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new(&bin)
            .args(["usage", "--json"])
            .output(),
    )
    .await
    .map_err(|_| anyhow!("kiro-pool usage 超时（120s）"))?
    .map_err(|e| anyhow!("无法调用 {}: {}", bin, e))?;
    if !out.status.success() {
        bail!(
            "kiro-pool usage --json 失败 (exit={:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_pool_json(&text))
}

/// 纯函数：解析 `kiro-pool usage --json` 输出。
/// 返回（每 profile 一条 SubQuota, 汇总 note）。解析不出返回空。
fn parse_pool_json(text: &str) -> (Vec<SubQuota>, Option<String>) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return (Vec::new(), None);
    };
    let Some(items) = v.get("usage").and_then(Value::as_array) else {
        return (Vec::new(), None);
    };

    let mut out: Vec<SubQuota> = Vec::new();
    let mut total_left = 0.0_f64;
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(used_pct) = item.get("used_percent").and_then(Value::as_f64) else {
            continue;
        };
        let resets_at = item
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| dt.and_utc());
        if let (Some(total), Some(used)) = (
            item.get("credits_total").and_then(Value::as_f64),
            item.get("credits_used").and_then(Value::as_f64),
        ) {
            total_left += (total - used).max(0.0);
        }
        out.push(SubQuota {
            label: format!("pool:{}", name),
            used_percent: used_pct.clamp(0.0, 100.0),
            resets_at,
        });
    }
    if out.is_empty() {
        return (Vec::new(), None);
    }
    // 最紧的排前面，与其他 provider 的 sub_quotas 排序一致
    out.sort_by(|a, b| {
        b.used_percent
            .partial_cmp(&a.used_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let note = Some(format!(
        "pool: {} profiles · {:.1} credits left",
        out.len(),
        total_left
    ));
    (out, note)
}

/// 纯函数：把 `kiro-cli chat --no-interactive /usage` 的**已 strip ANSI** 输出解析成
/// plan / session / credits。fetch 里只负责跑 CLI + strip，不含 regex 逻辑。
fn parse_usage_output(text: &str) -> ParsedKiro {
    // Credits (951.38 of 1000 covered in plan)
    let re_credits = Regex::new(r"Credits\s*\(\s*([\d.]+)\s*of\s*([\d.]+)").unwrap();
    // 百分比行：行末「 95%」
    let re_pct = Regex::new(r"(?m)(\d+(?:\.\d+)?)\s*%\s*$").unwrap();
    // resets on 2026-05-01
    let re_reset = Regex::new(r"resets\s+on\s+(\d{4}-\d{2}-\d{2})").unwrap();
    // 首行尾部的 plan：| KIRO STUDENT
    let re_plan = Regex::new(r"\|\s*([A-Z][A-Z0-9 ]+?)\s*$").unwrap();

    let credits = re_credits.captures(text).and_then(|c| {
        let used: f64 = c[1].parse().ok()?;
        let total: f64 = c[2].parse().ok()?;
        Some(Credits {
            remaining: (total - used).max(0.0),
            total: Some(total),
            unit: "credits".to_string(),
        })
    });

    let resets_at: Option<DateTime<Utc>> = re_reset.captures(text).and_then(|c| {
        NaiveDate::parse_from_str(&c[1], "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc())
    });

    // session used_percent：优先走 credits 比例；否则解析行尾 "NN%"
    let session_pct = credits
        .as_ref()
        .and_then(|c| {
            c.total.map(|t| {
                if t > 0.0 {
                    (1.0 - c.remaining / t) * 100.0
                } else {
                    0.0
                }
            })
        })
        .or_else(|| {
            re_pct
                .captures_iter(text)
                .last()
                .and_then(|cap| cap.get(1)?.as_str().parse::<f64>().ok())
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

    ParsedKiro {
        plan,
        session,
        credits,
    }
}

/// 剥离 ANSI CSI 序列，保留纯文本。只处理 `\x1b[...<letter>`。
fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap());
    re.replace_all(s, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        let raw = "\x1b[31mRed\x1b[0m and \x1b[1;33mBold yellow\x1b[0m";
        assert_eq!(strip_ansi(raw), "Red and Bold yellow");
    }

    #[test]
    fn parse_student_plan_full_sample() {
        // 和 doc 注释里的样例一致（ANSI 已 strip）
        let text = "Estimated Usage | resets on 2026-05-01 | KIRO STUDENT\n\
                    Credits (951.38 of 1000 covered in plan)\n\
                    ████████████████████████████████████████ 95%\n\
                    Overages: Disabled\n";
        let p = parse_usage_output(text);
        assert_eq!(p.plan.as_deref(), Some("KIRO STUDENT"));

        let c = p.credits.expect("应解析出 credits");
        assert!((c.remaining - 48.62).abs() < 1e-6);
        assert_eq!(c.total, Some(1000.0));
        assert_eq!(c.unit, "credits");

        let s = p.session.expect("应算出 session 百分比");
        // credits 推出来的应约 95.138%，优先于行尾 95%
        assert!((s.used_percent - 95.138).abs() < 0.01);
        assert!(s.resets_at.is_some());
    }

    #[test]
    fn parse_pct_fallback_when_no_credits_line() {
        let text = "Estimated Usage | resets on 2026-05-01 | KIRO PRO\n\
                    some garbage\n\
                    42%\n";
        let p = parse_usage_output(text);
        assert_eq!(p.plan.as_deref(), Some("KIRO PRO"));
        assert!(p.credits.is_none());
        let s = p.session.expect("应从行尾 NN% fallback");
        assert!((s.used_percent - 42.0).abs() < 1e-9);
    }

    #[test]
    fn parse_empty_text_returns_empty_parsed() {
        let p = parse_usage_output("");
        assert!(p.plan.is_none());
        assert!(p.session.is_none());
        assert!(p.credits.is_none());
    }

    #[test]
    fn parse_pool_json_maps_profiles_to_subquotas() {
        // kiro-pool usage --json 的真实形态（数值脱敏）
        let text = r#"{
            "usage": [
                {"credits_total": 1000.0, "credits_used": 398.74, "name": "student_2",
                 "plan": "KIRO STUDENT", "resets_at": "2026-08-01", "used_percent": 39.874},
                {"credits_total": 1000.0, "credits_used": 442.7, "name": "student_3",
                 "plan": "KIRO STUDENT", "resets_at": "2026-08-01", "used_percent": 44.27},
                {"credits_total": 1000.0, "credits_used": 291.65, "name": "student_4",
                 "plan": "KIRO STUDENT", "resets_at": "2026-08-01", "used_percent": 29.165}
            ]
        }"#;
        let (sq, note) = parse_pool_json(text);

        assert_eq!(sq.len(), 3);
        // 最紧的（student_3, 44.27%）排前面
        assert_eq!(sq[0].label, "pool:student_3");
        assert!((sq[0].used_percent - 44.27).abs() < 1e-9);
        assert_eq!(sq[2].label, "pool:student_4");
        assert!(sq[0].resets_at.is_some());

        // 汇总：601.26 + 557.3 + 708.35 = 1866.91
        let note = note.expect("应有汇总 note");
        assert!(note.contains("3 profiles"), "{note}");
        assert!(note.contains("1866.9"), "{note}");
    }

    #[test]
    fn parse_pool_json_tolerates_garbage() {
        assert_eq!(parse_pool_json("").0.len(), 0);
        assert_eq!(parse_pool_json("not json").0.len(), 0);
        assert_eq!(parse_pool_json(r#"{"usage": []}"#).0.len(), 0);
        // 缺 used_percent 的条目被跳过
        let (sq, _) = parse_pool_json(r#"{"usage": [{"name": "x"}]}"#);
        assert!(sq.is_empty());
    }

    #[test]
    fn parse_over_quota_clamps_to_100() {
        let text = "Estimated Usage | resets on 2026-05-01 | KIRO\n\
                    Credits (1200 of 1000 covered in plan)\n\
                    120%\n";
        let p = parse_usage_output(text);
        let s = p.session.unwrap();
        assert!(
            s.used_percent <= 100.0,
            "应被 clamp 到 100，得到 {}",
            s.used_percent
        );
    }
}
