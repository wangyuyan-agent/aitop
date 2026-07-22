# aitop 📊

> 一个统一的 AI 额度面板 — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro 全部塞进一个 TUI。

**语言：** [English](README.md) · **简体中文** · [繁體中文](README.zh-TW.md)

---

## 现状

- ✅ Provider 抽象层（`Usage` / `Window` / `Credits` / `SubQuota` / `CostMetric`）
- ✅ 本地探测：默认只显示能在本机找到凭证的 provider
- ✅ `oneshot` 文本输出 & `json` 机读输出（脚本友好）
- ✅ **OpenRouter** — API key → `/api/v1/auth/key`
- ✅ **Gemini** — OAuth 凭证 + 自动刷新 + `retrieveUserQuota`（port 自 Python 版）
- ✅ **Kiro** — 调 `kiro-cli chat --no-interactive /usage` 正则抽取
- ✅ **Copilot** — GitHub token（env / `gh auth token`）→ `/copilot_internal/user`；付费（`quota_snapshots`，含超额警告）与免费限量（`monthly_quotas`）两种响应都兼容
- ✅ **Claude** — 官方 OAuth usage API（与 Claude Code `/usage` 面板同源：session / weekly / 按模型 bucket）；端点被限流时回落到扫 `~/.claude/projects/*.jsonl` 本地估算
- ✅ **Codex** — `~/.codex/auth.json` OAuth 取身份 + 本地 rollout 日志的官方 `rate_limits` 快照（周窗/会话进度条、credits、plan）+ 本地近 7/30 天 token 与费用估算
- ✅ **Kiro pool** — `PATH` 上有 `kiro-pool`（多账号轮转池，`usage --json`）时，池中每个 profile 的 credits 以 sub-quota 形式与当前账号并列展示
- ✅ **TUI**（ratatui）— 卡片布局、并发刷新、自动刷新、风险排序、滚动、reset countdown、pace 提示
- ✅ **状态探测** — `aitop status` 查询 OpenAI / Anthropic / GitHub 状态页
- ✅ **配置文件** — `~/.config/aitop/config.toml`（可用 `AITOP_CONFIG` 覆盖）
- ✅ **多语言** — English / 简体中文 / 繁體中文，通过 `rust-i18n` + `locales/app.yml` 数据驱动

## 安装

```bash
cargo install --path .
# 或直接从 GitHub 安装
cargo install --git https://github.com/wangyuyan-agent/aitop
# 固定安装本次发布版本
cargo install --git https://github.com/wangyuyan-agent/aitop --tag v0.3.0
```

macOS（Apple Silicon / Intel）和 Linux x86_64 的预编译包可从 [GitHub Releases](https://github.com/wangyuyan-agent/aitop/releases) 下载。

## 用法

默认进入 TUI，只显示本地已探测到凭证的 provider。加 `--all` 显示全部（包含未实现的）。

```bash
aitop                         # TUI，自动过滤
aitop --all                   # TUI，显示全部 provider
aitop --lang zh-CN            # 强制简体中文（默认跟随 $AITOP_LANG / $LANG）
aitop --lang zh-TW            # 强制繁体中文

# 脚本 / CI
aitop oneshot                         # 文本输出，仅已配置
aitop oneshot --provider all          # 文本，全部 provider
aitop oneshot --provider openrouter   # 单个 provider
aitop json --pretty                   # JSON
aitop json --provider gemini,openrouter
aitop status                          # 查看上游服务状态

# 盯住单个 provider
aitop watch gemini --interval 30
```

**TUI 键位：** `q` / `Ctrl-C` 退出 · `r` 刷新 · `↑↓` / `jk` 切换 · `g` / `G` 跳首尾。

## 配置

`aitop` 会读取 `~/.config/aitop/config.toml`；也可用 `AITOP_CONFIG=/path/to/config.toml` 指定。

```toml
refresh_interval_secs = 300   # TUI 自动刷新间隔，最低 5 秒
sort = "risk"                 # risk | name | original
show_accounts = true          # TUI 是否显示账号
status_enabled = true         # TUI 刷新时附带上游状态
```

## 凭证

| Provider | 来源 | 如何探测 |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | 存在且非空 |
| Gemini | `~/.gemini/oauth_creds.json` | 文件存在（先跑 `gemini` CLI 登录） |
| Kiro | `kiro-cli` 在 `PATH` 内 | `which kiro-cli` 或 `which kiro-pool` 任一成功（可用 `KIRO_CLI_BIN` 覆盖） |
| Copilot | env `GITHUB_TOKEN` / `GH_TOKEN` / `COPILOT_API_TOKEN`，或 `gh` 在 `PATH` | 环境变量已设 或 `which gh` 成功 |
| Claude | macOS Keychain `Claude Code-credentials` / `~/.claude/.credentials.json` / `~/.claude/projects/` | 三者任一 |
| Codex | `~/.codex/auth.json` + `~/.codex/sessions/` rollout 日志（可用 `CODEX_HOME` 覆盖目录） | auth.json 存在且能解析出 `tokens` 字段 |
| Kiro pool | `kiro-pool` 在 `PATH`（可用 `KIRO_POOL_BIN` 覆盖） | 可选 —— 存在时扩展 Kiro 卡片 |

`detect()` 仅做本地 I/O（不发网络），因此启动时能立刻过滤未配置的 provider。

## 开发

```bash
cargo run                          # TUI（自动过滤）
cargo run -- --all                 # TUI（全部 provider）
cargo run -- oneshot
cargo run -- json --pretty
RUST_LOG=debug cargo run
```

## 架构

- `src/providers/mod.rs` — 统一数据模型 + `Provider` trait + `Availability` 探测 + 选择器
- `src/providers/<name>.rs` — 各 provider 实现（每个独立持有凭证路径）
- `src/providers/status.rs` — 上游状态页探测
- `src/config.rs` — TOML 配置加载
- `src/ui/` — ratatui 层（header · 卡片 · footer）
- `src/lang.rs` — 语言检测；解析 `--lang` / `$AITOP_LANG` / `$LANG` 并把 BCP 47 代码推给 `rust-i18n`
- `locales/app.yml` — 所有面向用户的文案，每条一个 key，每种语言一列

Provider 仅返回 `Usage`；文本渲染、JSON 序列化、TUI 绘制全在上层。

## 多语言

所有面向用户的字符串集中在 [`locales/app.yml`](locales/app.yml)，由 [`rust-i18n`](https://crates.io/crates/rust-i18n) 在编译期加载。加一种新语言三步：

1. 在 `locales/app.yml` 每一条下加一列（比如 `ja:`）。
2. 在 [`src/lang.rs`](src/lang.rs) 加一个 `Lang::Ja` variant，补 `code` / `parse` / `detect`。
3. 重编译 —— 运行时代码不用动。

启动时的语言优先级：`--lang` 参数 → `$AITOP_LANG` → `$LANG` / `$LC_ALL` → English。

## 许可

MIT
