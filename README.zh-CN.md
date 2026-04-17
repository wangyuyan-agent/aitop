# aitop 📊

> 一个统一的 AI 额度面板 — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro 全部塞进一个 TUI。

**语言：** [English](README.md) · **简体中文** · [繁體中文](README.zh-TW.md)

---

## 现状

- ✅ Provider 抽象层（`Usage` / `Window` / `Credits` / `SubQuota`）
- ✅ 本地探测：默认只显示能在本机找到凭证的 provider
- ✅ `oneshot` 文本输出 & `json` 机读输出（脚本友好）
- ✅ **OpenRouter** — API key → `/api/v1/auth/key`
- ✅ **Gemini** — OAuth 凭证 + 自动刷新 + `retrieveUserQuota`（port 自 Python 版）
- ✅ **Kiro** — 调 `kiro-cli chat --no-interactive /usage` 正则抽取
- ✅ **TUI**（ratatui）— 卡片布局、并发刷新、实时更新
- ✅ **多语言** — English / 简体中文 / 繁體中文，通过 `rust-i18n` + `locales/app.yml` 数据驱动
- 🟡 Claude / Codex / Copilot — 仅留桩，欢迎 PR

## 安装

```bash
cargo install --path .
# 或直接从 GitHub 安装
cargo install --git https://github.com/wangyuyan-agent/aitop
```

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

# 盯住单个 provider
aitop watch gemini --interval 30
```

**TUI 键位：** `q` / `Ctrl-C` 退出 · `r` 刷新 · `↑↓` / `jk` 切换 · `g` / `G` 跳首尾。

## 凭证

| Provider | 来源 | 如何探测 |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | 存在且非空 |
| Gemini | `~/.gemini/oauth_creds.json` | 文件存在（先跑 `gemini` CLI 登录） |
| Kiro | `kiro-cli` 在 `PATH` 内 | `which kiro-cli` 成功（可用 `KIRO_CLI_BIN` 覆盖） |
| Claude / Codex / Copilot | 未实现 | 始终标记为 `Missing` |

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
