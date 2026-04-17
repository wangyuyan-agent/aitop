# aitop 📊

> A unified AI-quota dashboard — OpenRouter · Gemini · Codex · Claude · Copilot · Kiro, all in one TUI.

**Languages:** **English** · [简体中文](README.zh-CN.md) · [繁體中文](README.zh-TW.md)

---

## Status

- ✅ Provider abstraction (`Usage` / `Window` / `Credits` / `SubQuota`)
- ✅ Local-availability detection — only show providers whose credentials exist on this machine
- ✅ `oneshot` (plain text) and `json` output for scripting
- ✅ **OpenRouter** — API key via `/api/v1/auth/key`
- ✅ **Gemini** — OAuth creds + auto-refresh + `retrieveUserQuota` (ported from the Python tool)
- ✅ **Kiro** — shells out to `kiro-cli chat --no-interactive /usage`, regex-extracts
- ✅ **TUI** (ratatui) — card layout, concurrent fetch, live refresh
- ✅ **i18n** — English / 简体中文 / 繁體中文, data-driven via `rust-i18n` + `locales/app.yml`
- 🟡 Claude / Codex / Copilot — stubs only; PRs welcome

## Install

```bash
cargo install --path .
# or directly from GitHub
cargo install --git https://github.com/wangyuyan-agent/aitop
```

## Usage

By default, `aitop` launches the TUI and only shows providers whose credentials are found locally. Pass `--all` to reveal every provider (including the unimplemented ones).

```bash
aitop                         # TUI, auto-filtered
aitop --all                   # TUI, show every provider
aitop --lang en               # force English (defaults to $AITOP_LANG / $LANG)
aitop --lang zh-TW            # force Traditional Chinese

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

## Credentials

| Provider | Source | How to detect |
|---|---|---|
| OpenRouter | env `OPENROUTER_API_KEY` | env var set and non-empty |
| Gemini | `~/.gemini/oauth_creds.json` | file exists (run `gemini` CLI once to log in) |
| Kiro | `kiro-cli` on `PATH` | `which kiro-cli` succeeds (override with `KIRO_CLI_BIN`) |
| Claude / Codex / Copilot | not yet implemented | always marked `Missing` |

`detect()` does local I/O only — no network requests — so unconfigured providers can be filtered out instantly at startup.

## Development

```bash
cargo run                          # TUI (auto-filtered)
cargo run -- --all                 # TUI (every provider)
cargo run -- oneshot
cargo run -- json --pretty
RUST_LOG=debug cargo run
```

## Architecture

- `src/providers/mod.rs` — unified data model + `Provider` trait + `Availability` detection + selector
- `src/providers/<name>.rs` — per-provider implementation (async_trait, each owns its own auth path)
- `src/ui/` — ratatui layer (header · provider cards · footer)
- `src/lang.rs` — language detection; parses `--lang` / `$AITOP_LANG` / `$LANG` and pushes the BCP 47 code into `rust-i18n`
- `locales/app.yml` — every user-facing string, one YAML entry per key with columns for each supported locale

Each provider returns a `Usage` struct. Text rendering, JSON serialization, and TUI painting all live above the provider layer.

## i18n

All user-facing strings live in [`locales/app.yml`](locales/app.yml), loaded at compile time by [`rust-i18n`](https://crates.io/crates/rust-i18n). Adding another language takes three steps:

1. Add a new column (e.g. `ja:`) to every entry in `locales/app.yml`.
2. Add a `Lang::Ja` variant to [`src/lang.rs`](src/lang.rs) (`code`, `parse`, `detect`).
3. Rebuild — no runtime code changes needed.

Locale precedence at startup: `--lang` flag → `$AITOP_LANG` → `$LANG` / `$LC_ALL` → English.

## License

MIT
