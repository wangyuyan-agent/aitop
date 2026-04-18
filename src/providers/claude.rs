//! Claude Code — macOS Keychain OAuth + 本地 jsonl 日志统计
//!
//! Anthropic 没有公开的"剩余额度" API，但 Claude Code 把每轮对话完整写入
//! `~/.claude/projects/<slug>/<session>.jsonl`。本 provider 的策略：
//!
//! 1. 凭证探测：macOS Keychain 里的 `Claude Code-credentials`，或 Linux 路径
//!    `~/.claude/.credentials.json`。只要存在就视为 Ready；没凭证但有本地日志
//!    也行（以纯 logs 模式工作）。
//! 2. 读 Keychain / 文件 → 拿到 `subscriptionType`（pro / max / max_5x / max_20x …）
//!    作为 plan。
//! 3. 扫 `~/.claude/projects/**/*.jsonl`，分两窗累计：
//!    - session：过去 5 小时
//!    - weekly：过去 7 天
//!
//!    计数规则（与 Anthropic 的 billable message 口径对齐）：
//!    - `type == "user"` 且 `message.content` 不是 tool_result only → 记一条"用户轮次"
//!    - `type == "assistant"` → 累加 `message.usage.input_tokens` / `output_tokens` /
//!      `cache_read_input_tokens` / `cache_creation_input_tokens`
//! 4. 若 plan 能匹配到 [`plan_limits`]，则把 session / weekly 算成 Window 进度条；
//!    否则只留原始数值在 note 里。
//!
//! ⚠ 限额数字（45 / 225 / 900 条 / 5h 等）来自 Anthropic 公开文档 + 社区实测，
//! 并非官方 SLA；Anthropic 的 usage-based limits 会随账号 / 模型 / 时段动态调整。
//! 进度条仅供参考，实际封顶以 claude.ai 面板为准。

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use super::{Availability, Provider, Usage, Window};

#[cfg(target_os = "macos")]
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

        // 本地日志统计（I/O 密集，丢给 blocking 池）
        let stats = tokio::task::spawn_blocking(scan_projects)
            .await
            .context("扫描 ~/.claude/projects 失败（线程 join）")?;

        // 根据 plan 查限额表 → 若已知则构造 Window 进度条
        let limits = plan.as_deref().and_then(plan_limits);
        let (session, weekly) = match (&stats, limits) {
            (Some(s), Some(lim)) => (
                Some(Window {
                    used_percent: pct(s.session.user_msgs, lim.session),
                    window_minutes: Some(300),
                    resets_at: None, // Anthropic 用滚动窗口，不是固定重置点
                }),
                Some(Window {
                    used_percent: pct(s.weekly.user_msgs, lim.weekly),
                    window_minutes: Some(10_080),
                    resets_at: None,
                }),
            ),
            _ => (None, None),
        };

        let note = build_note(stats.as_ref(), plan.as_deref(), limits);
        let account = rate_tier.as_ref().map(|t| format!("tier: {}", t));

        Ok(Usage {
            provider: "Claude".to_string(),
            source: if creds.is_some() { "oauth" } else { "logs" }.to_string(),
            account,
            plan,
            session,
            weekly,
            credits: None,
            sub_quotas: Vec::new(),
            updated_at: Utc::now(),
            note,
        })
    }
}

// ---------- plan → cap 表 ----------

#[derive(Copy, Clone, Debug, PartialEq)]
struct PlanLimits {
    /// 滚动 5h 窗口的用户消息上限。
    session: u64,
    /// 滚动 7d 窗口的用户消息上限。
    weekly: u64,
}

/// 把 Anthropic 的 `subscriptionType` 映射到近似消息额度。
///
/// 数字依据：
/// - Pro ($20/mo)：Anthropic 文档 "approximately 45 messages every 5 hours"；
///   周额度没官方数字，取"40–80 hours of Sonnet 4 per week" × 平均速率 ≈ 240。
/// - Max 5x ($100/mo)：Pro × 5。
/// - Max 20x ($200/mo)：Pro × 20。
///
/// Anthropic 公告里强调是 *approximate*，本表同样是 *approximate*。
fn plan_limits(plan: &str) -> Option<PlanLimits> {
    let key = plan.to_lowercase().replace('-', "_");
    match key.as_str() {
        "pro" => Some(PlanLimits { session: 45, weekly: 240 }),
        "max" | "max_5x" | "max5x" => Some(PlanLimits { session: 225, weekly: 1200 }),
        "max_20x" | "max20x" => Some(PlanLimits { session: 900, weekly: 4800 }),
        _ => None,
    }
}

fn pct(used: u64, cap: u64) -> f64 {
    if cap == 0 {
        0.0
    } else {
        (used as f64 / cap as f64 * 100.0).clamp(0.0, 100.0)
    }
}

fn build_note(
    stats: Option<&ScanStats>,
    plan: Option<&str>,
    limits: Option<PlanLimits>,
) -> Option<String> {
    let s = match stats {
        Some(s) => s,
        None => return Some("~/.claude/projects 不存在或无记录".to_string()),
    };
    let mut parts = vec![
        format!(
            "5h {} msgs / {} out",
            s.session.user_msgs,
            fmt_tokens(s.session.output_tokens)
        ),
        format!(
            "7d {} msgs / {} out",
            s.weekly.user_msgs,
            fmt_tokens(s.weekly.output_tokens)
        ),
    ];
    if limits.is_none() {
        if let Some(p) = plan {
            parts.push(format!("plan={} 无限额表 (approx)", p));
        } else {
            parts.push("plan 未知 → 无进度条".to_string());
        }
    } else {
        parts.push("approx cap (非官方)".to_string());
    }
    Some(parts.join(" · "))
}

// ---------- 凭证读取 ----------

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

#[cfg(target_os = "macos")]
fn read_keychain() -> Option<Value> {
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

/// 非 macOS 平台没有 `security` CLI / Keychain —— 直接回落到 `~/.claude/.credentials.json`。
#[cfg(not(target_os = "macos"))]
fn read_keychain() -> Option<Value> {
    None
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

// ---------- 日志扫描 ----------

#[derive(Default, Debug, Clone)]
struct WindowStats {
    /// 真实用户轮次（过滤掉 tool_result 注入行）。
    user_msgs: u64,
    /// assistant 回合数（1 个用户轮次可能触发多次 LLM 调用）。
    assistant_msgs: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_create: u64,
}

#[derive(Default, Debug)]
struct ScanStats {
    session: WindowStats,
    weekly: WindowStats,
}

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
            // mtime 早于 weekly_cutoff+1h 的整文件跳过，减少 IO
            if let Ok(meta) = entry.metadata()
                && let Ok(mtime) = meta.modified()
                    && let Ok(dur) = mtime.elapsed()
                        && dur > std::time::Duration::from_secs(7 * 24 * 3600 + 3600) {
                            continue;
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

fn scan_file(
    path: &Path,
    session_cutoff: DateTime<Utc>,
    weekly_cutoff: DateTime<Utc>,
    stats: &mut ScanStats,
) -> bool {
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
        if count_event(&v, session_cutoff, weekly_cutoff, stats) {
            any = true;
        }
    }
    any
}

/// 把单条事件累计到 session / weekly 窗口。返回是否命中（用于判断文件是否有效）。
///
/// 单元测试直接调这个函数，不用真实文件。
fn count_event(
    v: &Value,
    session_cutoff: DateTime<Utc>,
    weekly_cutoff: DateTime<Utc>,
    stats: &mut ScanStats,
) -> bool {
    let Some(ts_str) = v.get("timestamp").and_then(Value::as_str) else {
        return false;
    };
    let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) else {
        return false;
    };
    let ts = ts.with_timezone(&Utc);
    if ts < weekly_cutoff {
        return false;
    }
    let in_session = ts >= session_cutoff;

    match v.get("type").and_then(Value::as_str) {
        Some("user") => {
            // 过滤 Claude Code 把 tool output 作为 user role 回注的行。
            // 只有 content 全部是 tool_result 的才跳过；带有 text 段的视为真人补充。
            if is_tool_result_only(v) {
                return false;
            }
            stats.weekly.user_msgs += 1;
            if in_session {
                stats.session.user_msgs += 1;
            }
            true
        }
        Some("assistant") => {
            let usage = v.get("message").and_then(|m| m.get("usage"));
            let input = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_r = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_c = usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            accumulate(&mut stats.weekly, input, output, cache_r, cache_c, true);
            if in_session {
                accumulate(&mut stats.session, input, output, cache_r, cache_c, true);
            }
            true
        }
        _ => false,
    }
}

/// `message.content` 是否完全由 `tool_result` 组成 —— 是的话不算真实用户轮次。
fn is_tool_result_only(v: &Value) -> bool {
    let content = match v.get("message").and_then(|m| m.get("content")) {
        Some(c) => c,
        None => return false,
    };
    match content {
        Value::String(_) => false,
        Value::Array(arr) if !arr.is_empty() => arr
            .iter()
            .all(|e| e.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

fn accumulate(
    w: &mut WindowStats,
    input: u64,
    output: u64,
    cache_r: u64,
    cache_c: u64,
    is_assistant: bool,
) {
    if is_assistant {
        w.assistant_msgs += 1;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now_iso(offset_hours: i64) -> String {
        (Utc::now() - Duration::hours(offset_hours))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn run(events: Vec<Value>) -> ScanStats {
        let now = Utc::now();
        let session_cutoff = now - Duration::hours(5);
        let weekly_cutoff = now - Duration::days(7);
        let mut stats = ScanStats::default();
        for e in events {
            count_event(&e, session_cutoff, weekly_cutoff, &mut stats);
        }
        stats
    }

    #[test]
    fn user_turn_with_tool_result_is_ignored() {
        // 纯 tool_result 注入：不算真实用户轮次。
        let tool_only = json!({
            "type": "user",
            "timestamp": now_iso(1),
            "message": {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": "{...}" }
                ]
            }
        });
        // 真人 string prompt：记 1 条。
        let real_prompt = json!({
            "type": "user",
            "timestamp": now_iso(1),
            "message": { "role": "user", "content": "hello" }
        });
        // 真人 array with text：记 1 条。
        let real_array = json!({
            "type": "user",
            "timestamp": now_iso(1),
            "message": {
                "role": "user",
                "content": [{ "type": "text", "text": "hi again" }]
            }
        });
        let stats = run(vec![tool_only, real_prompt, real_array]);
        assert_eq!(stats.session.user_msgs, 2);
        assert_eq!(stats.weekly.user_msgs, 2);
    }

    #[test]
    fn assistant_tokens_accumulate() {
        let line = json!({
            "type": "assistant",
            "timestamp": now_iso(1),
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 200,
                    "cache_creation_input_tokens": 10
                }
            }
        });
        let stats = run(vec![line.clone(), line]);
        assert_eq!(stats.session.assistant_msgs, 2);
        assert_eq!(stats.session.input_tokens, 200);
        assert_eq!(stats.session.output_tokens, 100);
        assert_eq!(stats.session.cache_read, 400);
        assert_eq!(stats.session.cache_create, 20);
    }

    #[test]
    fn window_cutoffs_split_session_and_weekly() {
        let in_session = json!({
            "type": "user",
            "timestamp": now_iso(1),  // 1h ago → 双窗都命中
            "message": { "role": "user", "content": "a" }
        });
        let weekly_only = json!({
            "type": "user",
            "timestamp": now_iso(72),  // 72h ago → 只命中 weekly
            "message": { "role": "user", "content": "b" }
        });
        let outside = json!({
            "type": "user",
            "timestamp": now_iso(8 * 24),  // 8d ago → 都不命中
            "message": { "role": "user", "content": "c" }
        });
        let stats = run(vec![in_session, weekly_only, outside]);
        assert_eq!(stats.session.user_msgs, 1);
        assert_eq!(stats.weekly.user_msgs, 2);
    }

    #[test]
    fn plan_limits_covers_known_plans() {
        assert!(plan_limits("pro").is_some());
        assert!(plan_limits("PRO").is_some()); // case-insensitive
        assert!(plan_limits("max").is_some());
        assert!(plan_limits("max_5x").is_some());
        assert!(plan_limits("max-5x").is_some()); // hyphen variant
        assert!(plan_limits("max_20x").is_some());
        assert_eq!(plan_limits("max_20x").unwrap().session, 900);
        assert!(plan_limits("free").is_none());
        assert!(plan_limits("").is_none());
    }

    #[test]
    fn pct_clamps() {
        assert_eq!(pct(0, 45), 0.0);
        assert!((pct(45, 45) - 100.0).abs() < 1e-6);
        assert!((pct(90, 45) - 100.0).abs() < 1e-6); // 溢出时 clamp 到 100
        assert_eq!(pct(10, 0), 0.0); // 零除保护
    }

    #[test]
    fn missing_or_invalid_timestamp_is_skipped() {
        let no_ts = json!({
            "type": "user",
            "message": { "role": "user", "content": "x" }
        });
        let bad_ts = json!({
            "type": "user",
            "timestamp": "not-a-date",
            "message": { "role": "user", "content": "x" }
        });
        let stats = run(vec![no_ts, bad_ts]);
        assert_eq!(stats.session.user_msgs, 0);
        assert_eq!(stats.weekly.user_msgs, 0);
    }

    #[test]
    fn fmt_tokens_boundaries() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(1_234), "1.2k");
        assert_eq!(fmt_tokens(999_999), "1000.0k");
        assert_eq!(fmt_tokens(1_000_000), "1.0M");
        assert_eq!(fmt_tokens(2_500_000), "2.5M");
    }
}
