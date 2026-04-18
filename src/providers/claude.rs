//! Claude Code — macOS Keychain OAuth + 本地 jsonl 日志统计
//!
//! Anthropic 没有公开的"剩余额度" API，但 Claude Code 把每轮对话完整写入
//! `~/.claude/projects/<slug>/<session>.jsonl`，`type: "assistant"` 行的
//! `message.usage` 里有真实 token 统计。本 provider 的策略：
//!
//! 1. 凭证探测：macOS Keychain 里的 `Claude Code-credentials`，或 Linux 路径
//!    `~/.claude/.credentials.json`。只要存在就视为 Ready。
//! 2. 读 Keychain / 文件 → 拿到 `subscriptionType`（pro / max / max_5x / max_20x …）
//!    作为 plan。
//! 3. 扫 `~/.claude/projects/*/*.jsonl`，按时间戳分窗统计：
//!    - session（过去 5 小时）
//!    - weekly（过去 7 天）
//!    两者都以消息计数 + output token 两个维度放进 note 里。
//!
//! 不尝试硬编码 plan 的具体额度——Anthropic 的 usage-based limits 不公开
//! 且变化频繁；与其猜 `used_percent`，不如把原始数值摆出来让用户自己判断。

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use super::{Availability, Provider, Usage};

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[derive(Default)]
pub struct Claude;

#[async_trait]
impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> Availability {
        if read_credentials().is_some() {
            return Availability::Ready;
        }
        if projects_dir().is_dir() {
            // 没有凭证但有本地日志也能统计用量
            return Availability::Ready;
        }
        Availability::Missing(
            "未找到 Keychain `Claude Code-credentials` 或 ~/.claude/projects；请先登录 Claude Code".into(),
        )
    }

    async fn fetch(&self) -> Result<Usage> {
        let creds = read_credentials();
        let plan = creds
            .as_ref()
            .and_then(|c| c.get("subscriptionType").and_then(Value::as_str))
            .map(str::to_string);
        let rate_tier = creds
            .as_ref()
            .and_then(|c| c.get("rateLimitTier").and_then(Value::as_str))
            .map(str::to_string);

        // 本地日志统计（tokio::task::spawn_blocking：IO 密集）
        let stats = tokio::task::spawn_blocking(scan_projects)
            .await
            .context("扫描 ~/.claude/projects 失败（线程 join）")?;

        let note = match &stats {
            Some(s) => Some(format!(
                "5h: {} msgs / {} out toks · 7d: {} msgs / {} out toks",
                s.session.msgs,
                fmt_tokens(s.session.output_tokens),
                s.weekly.msgs,
                fmt_tokens(s.weekly.output_tokens),
            )),
            None => Some("~/.claude/projects 不存在或无 assistant 记录".to_string()),
        };

        let account = rate_tier.as_ref().map(|t| format!("tier: {}", t));

        Ok(Usage {
            provider: "Claude".to_string(),
            source: if creds.is_some() { "oauth" } else { "logs" }.to_string(),
            account,
            plan,
            session: None, // Anthropic 不公开 usage-based cap，不硬编码百分比
            weekly: None,
            credits: None,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note,
        })
    }
}

/// 先试 macOS Keychain，失败再试 `~/.claude/.credentials.json`。返回内部 `claudeAiOauth` 对象。
fn read_credentials() -> Option<Value> {
    if let Some(v) = read_keychain() {
        return Some(v);
    }
    if let Some(v) = read_credentials_file() {
        return Some(v);
    }
    None
}

fn read_keychain() -> Option<Value> {
    // `security find-generic-password -s "Claude Code-credentials" -w`
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    v.get("claudeAiOauth").cloned()
}

fn read_credentials_file() -> Option<Value> {
    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("claudeAiOauth").cloned()
}

fn projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
}

#[derive(Default, Debug, Clone)]
struct WindowStats {
    msgs: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_create: u64,
}

#[derive(Default, Debug)]
struct ScanStats {
    session: WindowStats, // 最近 5h
    weekly: WindowStats,  // 最近 7d
}

/// 扫所有 jsonl 文件，累加 session（5h）/ weekly（7d）窗口。
/// 容错：坏行、坏文件、无时间戳统统跳过。
fn scan_projects() -> Option<ScanStats> {
    let root = projects_dir();
    if !root.is_dir() {
        return None;
    }
    let now = Utc::now();
    let session_cutoff = now - Duration::hours(5);
    let weekly_cutoff = now - Duration::days(7);

    let mut stats = ScanStats::default();
    let mut saw_any = false;

    for proj in walk_dir(&root) {
        if !proj.is_dir() {
            continue;
        }
        for entry in walk_dir(&proj) {
            if entry.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            // 按文件 mtime 先 skip：如果文件整体最后修改都早于 weekly_cutoff，跳过全文件
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.elapsed() {
                        if dur > std::time::Duration::from_secs(7 * 24 * 3600 + 3600) {
                            continue;
                        }
                    }
                }
            }
            saw_any |= scan_file(&entry, session_cutoff, weekly_cutoff, &mut stats);
        }
    }
    saw_any.then_some(stats)
}

fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()))
        .collect()
}

/// 扫单个 jsonl 文件，返回是否命中任何 assistant 记录。
fn scan_file(path: &Path, session_cutoff: DateTime<Utc>, weekly_cutoff: DateTime<Utc>, stats: &mut ScanStats) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);
    let mut any = false;
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(ts_str) = v.get("timestamp").and_then(Value::as_str) else {
            continue;
        };
        let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) else {
            continue;
        };
        let ts = ts.with_timezone(&Utc);
        if ts < weekly_cutoff {
            continue;
        }
        let usage = v.get("message").and_then(|m| m.get("usage"));
        let input = usage.and_then(|u| u.get("input_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let output = usage.and_then(|u| u.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let cache_r = usage
            .and_then(|u| u.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_c = usage
            .and_then(|u| u.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);

        any = true;
        add_to(&mut stats.weekly, input, output, cache_r, cache_c);
        if ts >= session_cutoff {
            add_to(&mut stats.session, input, output, cache_r, cache_c);
        }
    }
    any
}

fn add_to(w: &mut WindowStats, input: u64, output: u64, cache_r: u64, cache_c: u64) {
    w.msgs += 1;
    w.input_tokens += input;
    w.output_tokens += output;
    w.cache_read += cache_r;
    w.cache_create += cache_c;
}

/// 把 token 数格式化成 `123` / `1.2k` / `45.3k` / `2.1M`。
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
