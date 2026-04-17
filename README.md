# aitop 📊

> A unified AI-quota dashboard — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro, all in one TUI.

[中文](#中文) · [English](#english)

---

## English

### Status

- ✅ Provider abstraction (`Usage` / `Window` / `Credits` / `SubQuota`)
- ✅ Local-availability detection — only show providers whose credentials exist on this machine
- ✅ `oneshot` (plain text) and `json` output for scripting
- ✅ **OpenRouter** — API key via `/api/v1/auth/key`
- ✅ **Gemini** — OAuth creds + auto-refresh + `retrieveUserQuota` (ported from the Python tool)
- ✅ **Kiro** — shells out to `kiro-cli /usage`, regex-extracts
- ✅ **TUI** (ratatui) — card layout, concurrent fetch, live refresh
- ✅ **Bilingual UI** — zh/en, auto-detected from `LANG`
- 🟡 Claude / Codex / Copilot — stubs only; PRs welcome

### Install

```bash
cargo install --path .
# or directly from GitHub
cargo install --git https://github.com/wangyuyan-agent/aitop
```

### Usage

By default, `aitop` launches the TUI and only shows providers whose credentials are found locally. Pass `--all` to reveal every provider (including the unimplemented ones).

```bash
aitop                         # TUI, auto-filtered
aitop --all                   # TUI, show every provider
aitop --lang en               # force English (defaults to $LANG)

# Scripting / CI
aitop oneshot                         # text, detected providers only
aitop oneshot --provider all          # text, every provider
aitop oneshot --provider openrouter   # single provider
aitop json --pretty                   # machine-readable
aitop json --provider gemini,openrouter

# Single-provider live watcher
aitop watch gemini --interval 30
```

**TUI keys:** `q` / `Ctrl-C` quit · `r` refresh · `↑↓` / `jk` navigate · `g` / `G` jump to top/bottom.

### Credentials

| Provider | Source | How to detect |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | env var set and non-empty |
| Gemini | `~/.gemini/oauth_creds.json` | file exists (run `gemini` CLI once to log in) |
| Kiro | `kiro-cli` on `PATH` | `which kiro-cli` succeeds (override with `KIRO_CLI_BIN`) |
| Claude / Codex / Copilot | not yet implemented | always marked `Missing` |

`detect()` does local I/O only — no network requests — so unconfigured providers can be filtered out instantly at startup.

### Development

```bash
cargo run                          # TUI (auto-filtered)
cargo run -- --all                 # TUI (every provider)
cargo run -- oneshot
cargo run -- json --pretty
RUST_LOG=debug cargo run
```

### Architecture

- `src/providers/mod.rs` — unified data model + `Provider` trait + `Availability` detection + selector
- `src/providers/<name>.rs` — per-provider implementation (async_trait, each owns its own auth path)
- `src/ui/` — ratatui layer (header · provider cards · footer)
- `src/i18n.rs` — bilingual message table, selected via `--lang` or `$LANG`

Each provider returns a `Usage` struct. Text rendering, JSON serialization, and TUI painting all live above the provider layer.

### License

MIT

---

## 中文

### 现状

- ✅ Provider 抽象层（`Usage` / `Window` / `Credits` / `SubQuota`）
- ✅ 本地探测：默认只显示能在本机找到凭证的 provider
- ✅ `oneshot` 文本输出 & `json` 机读输出（脚本友好）
- ✅ **OpenRouter** — API key → `/api/v1/auth/key`
- ✅ **Gemini** — OAuth 凭证 + 自动刷新 + `retrieveUserQuota`（port 自 Python 版）
- ✅ **Kiro** — 调 `kiro-cli /usage` 正则抽取
- ✅ **TUI**（ratatui）— 卡片布局、并发刷新、实时更新
- ✅ **双语 UI** — zh/en，按 `LANG` 自动识别
- 🟡 Claude / Codex / Copilot — 仅留桩，欢迎 PR

### 安装

```bash
cargo install --path .
# 或直接从 GitHub 安装
cargo install --git https://github.com/wangyuyan-agent/aitop
```

### 用法

默认进入 TUI，只显示本地已探测到凭证的 provider。加 `--all` 显示全部（包含未实现的）。

```bash
aitop                         # TUI，自动过滤
aitop --all                   # TUI，显示全部 provider
aitop --lang zh               # 强制中文（默认跟随 $LANG）

# 脚本 / CI
aitop oneshot                         # 文本输出，仅已配置
aitop oneshot --provider all          # 文本，全部 provider
aitop oneshot --provider openrouter   # 单个 provider
aitop json --pretty                   # JSON
aitop json --provider gemini,openrouter

# 盯住单个 provider
aitop watch gemini --interval 30
```

**TUI 键位：** `q` / `Ctrl-C` 退出 · `r` 刷新 · `↑↓` / `jk` 切换 · `g` / `G` 跳首尾。

### 凭证

| Provider | 来源 | 如何探测 |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | 存在且非空 |
| Gemini | `~/.gemini/oauth_creds.json` | 文件存在（先跑 `gemini` CLI 登录） |
| Kiro | `kiro-cli` 在 `PATH` 内 | `which kiro-cli` 成功（可用 `KIRO_CLI_BIN` 覆盖） |
| Claude / Codex / Copilot | 未实现 | 始终标记为 `Missing` |

`detect()` 仅做本地 I/O（不发网络），因此启动时能立刻过滤未配置的 provider。

### 架构

- `src/providers/mod.rs` — 统一数据模型 + `Provider` trait + `Availability` 探测 + 选择器
- `src/providers/<name>.rs` — 各 provider 实现（每个独立持有凭证路径）
- `src/ui/` — ratatui 层（header · 卡片 · footer）
- `src/i18n.rs` — 双语消息表，通过 `--lang` 或 `$LANG` 选择

Provider 仅返回 `Usage`；文本渲染、JSON 序列化、TUI 绘制全在上层。

### 许可

MIT
