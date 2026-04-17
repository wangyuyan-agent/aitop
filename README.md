# aitop 📊

> AI 额度监控 TUI — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro 全盘围观。
> A unified quota/usage dashboard for AI providers.

## 现状

- ✅ Provider 抽象层（`Usage` / `Window` / `Credits` / `SubQuota`）
- ✅ `oneshot` 文本输出、`json` 机读输出
- ✅ OpenRouter（API key → `/api/v1/auth/key`）
- ✅ Gemini（OAuth 凭证 + 自动刷新 + `retrieveUserQuota`，port 自 Python 版）
- ✅ Kiro（`kiro-cli /usage` 文本解析，粗粒度）
- 🟡 Claude / Codex / Copilot — 先留桩，后续补
- 🟡 TUI（ratatui）— 骨架已引入，渲染逻辑待写

## 安装

```bash
cargo install --path .
```

## 用法

### 单次拉取（脚本友好）

```bash
# 所有 provider 文本输出
aitop oneshot

# 单个 provider
aitop oneshot --provider openrouter

# 机读 JSON
aitop json --pretty --provider gemini,openrouter
```

### TUI（开发中）

```bash
aitop         # 进入 TUI（目前会提示尚未实现）
aitop watch gemini --interval 30   # watch 单 provider
```

## 凭证

| Provider | 来源 |
|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` |
| Gemini | `~/.gemini/oauth_creds.json`（先跑 `gemini` CLI 登录） |
| Kiro | `kiro-cli` 在 `PATH` 内；可用 env `KIRO_CLI_BIN` 覆盖 |
| Claude / Codex / Copilot | 未实现 |

## 开发

```bash
cargo run -- oneshot
cargo run -- json --pretty
RUST_LOG=debug cargo run
```

## 架构

- `src/providers/mod.rs` — 统一数据模型 + `Provider` trait + 选择器
- `src/providers/<name>.rs` — 各 provider 实现（async_trait）
- `src/ui/` — ratatui 层（占位）

Provider 只负责返回 `Usage`；文本渲染、JSON 序列化、TUI 绘制全都在上层。

## 许可

MIT
