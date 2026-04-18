# aitop 📊

> 統一的 AI 額度面板 — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro 全塞進一個 TUI。

**語言：** [English](README.md) · [简体中文](README.zh-CN.md) · **繁體中文**

---

## 現狀

- ✅ Provider 抽象層（`Usage` / `Window` / `Credits` / `SubQuota`）
- ✅ 本地偵測：預設只顯示本機有憑證的 provider
- ✅ `oneshot` 文字輸出 & `json` 機讀輸出（腳本友善）
- ✅ **OpenRouter** — API key → `/api/v1/auth/key`
- ✅ **Gemini** — OAuth 憑證 + 自動刷新 + `retrieveUserQuota`（port 自 Python 版）
- ✅ **Kiro** — 呼叫 `kiro-cli chat --no-interactive /usage` 正則擷取
- ✅ **Copilot** — GitHub token（env / `gh auth token`）→ `/copilot_internal/user`；付費（`quota_snapshots`）與免費限量（`monthly_quotas`）皆支援
- ✅ **Claude** — macOS Keychain（`Claude Code-credentials`）取 plan / tier；掃 `~/.claude/projects/*.jsonl` 彙總過去 5h session + 7d weekly 的 token 數（Anthropic 不公開配額 → 不畫進度條，只顯示原始數值）
- ✅ **Codex** — `~/.codex/auth.json` OAuth；解 `id_token` JWT 取 account / plan（ChatGPT 訂閱用量 API 未公開 → 僅顯示 account + plan + token 新鮮度）
- ✅ **TUI**（ratatui）— 卡片佈局、並發取數、即時刷新
- ✅ **多語言** — English / 简体中文 / 繁體中文，透過 `rust-i18n` + `locales/app.yml` 資料驅動

## 安裝

```bash
cargo install --path .
# 或直接從 GitHub 安裝
cargo install --git https://github.com/wangyuyan-agent/aitop
```

## 使用

預設進入 TUI，只顯示本機已偵測到憑證的 provider。加 `--all` 顯示全部（含未實作的）。

```bash
aitop                         # TUI，自動過濾
aitop --all                   # TUI，顯示全部 provider
aitop --lang zh-TW            # 強制繁體中文（預設依 $AITOP_LANG / $LANG）
aitop --lang en               # 強制英文

# 腳本 / CI
aitop oneshot                         # 文字輸出，僅已設定
aitop oneshot --provider all          # 文字，全部 provider
aitop oneshot --provider openrouter   # 單一 provider
aitop json --pretty                   # JSON
aitop json --provider gemini,openrouter

# 盯住單一 provider
aitop watch gemini --interval 30
```

**TUI 按鍵：** `q` / `Ctrl-C` 離開 · `r` 重新整理 · `↑↓` / `jk` 切換 · `g` / `G` 跳首尾。

## 憑證

| Provider | 來源 | 如何偵測 |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | 存在且非空 |
| Gemini | `~/.gemini/oauth_creds.json` | 檔案存在（先跑 `gemini` CLI 登入） |
| Kiro | `kiro-cli` 在 `PATH` 中 | `which kiro-cli` 成功（可用 `KIRO_CLI_BIN` 覆寫） |
| Copilot | env `GITHUB_TOKEN` / `GH_TOKEN` / `COPILOT_API_TOKEN`，或 `gh` 在 `PATH` | 環境變數已設 或 `which gh` 成功 |
| Claude | macOS Keychain `Claude Code-credentials` / `~/.claude/.credentials.json` / `~/.claude/projects/` | 三者任一 |
| Codex | `~/.codex/auth.json`（可用 `CODEX_HOME` 覆寫目錄） | 檔案存在且能解析出 `tokens` 欄位 |

`detect()` 只做本地 I/O（不發網路），所以啟動時就能立即過濾掉未設定的 provider。

## 開發

```bash
cargo run                          # TUI（自動過濾）
cargo run -- --all                 # TUI（全部 provider）
cargo run -- oneshot
cargo run -- json --pretty
RUST_LOG=debug cargo run
```

## 架構

- `src/providers/mod.rs` — 統一資料模型 + `Provider` trait + `Availability` 偵測 + 選擇器
- `src/providers/<name>.rs` — 各 provider 實作（各自持有憑證路徑）
- `src/ui/` — ratatui 層（header · 卡片 · footer）
- `src/lang.rs` — 語言偵測；解析 `--lang` / `$AITOP_LANG` / `$LANG` 並把 BCP 47 代碼推給 `rust-i18n`
- `locales/app.yml` — 所有面向使用者的字串，每條一個 key，每種語言一欄

Provider 只回傳 `Usage`；文字渲染、JSON 序列化、TUI 繪製都在上層。

## 多語言

所有面向使用者的字串集中於 [`locales/app.yml`](locales/app.yml)，由 [`rust-i18n`](https://crates.io/crates/rust-i18n) 在編譯期載入。加一種新語言三步：

1. 在 `locales/app.yml` 每條下加一欄（例如 `ja:`）。
2. 在 [`src/lang.rs`](src/lang.rs) 加一個 `Lang::Ja` variant，補 `code` / `parse` / `detect`。
3. 重新編譯 —— 執行期程式碼不用動。

啟動時的語言優先級：`--lang` 參數 → `$AITOP_LANG` → `$LANG` / `$LC_ALL` → English。

## 授權

MIT
