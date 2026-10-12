# Bang

A desktop mail client for Linux, written in Rust, with a UI inspired by Spark.

- **UI**: [Slint](https://slint.dev) (native, no webview)
- **Engine**: IMAP (per-account syncer thread, IDLE push) + SMTP, SQLite cache, OAuth2 (Gmail/Microsoft) and generic IMAP/SMTP accounts
- **Status**: early development — see [PLAN.md](PLAN.md) for the roadmap

## Features

- Three-pane layout (folders, message list, reading pane) with a unified inbox
  across all accounts.
- Multiple accounts: OAuth2 (Gmail/Microsoft) or generic IMAP/SMTP.
- Read/unread, star, and delete, with per-folder unread badges.
- Compose with attachments; full-text search across all accounts.
- Light/dark/system theme, desktop notifications for new mail, and a settings
  dialog for theme and account management.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the code is organized and how data flows
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — build, test, conventions, and Slint gotchas

## Build

```sh
cargo build --release
cargo run -p app
```

No system dependencies: the software renderer is used by default (zero GPU deps).

## OAuth client IDs

Gmail and Microsoft sign-in require your own OAuth2 client IDs (create them in
Google Cloud Console / Azure Portal). Set them in `~/.config/bang/config.toml`:

```toml
[oauth.google]
client_id = "..."
[oauth.microsoft]
client_id = "..."
```

Until then, use the manual IMAP/SMTP account type.

## Layout

- `crates/bang-core` — mail engine (IMAP, SMTP, OAuth, sync, SQLite store). No UI dependencies.
- `crates/app` — Slint UI + tokio/Slint bridge.
- `docs/` — architecture and development notes.
