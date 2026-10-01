# Bang

A desktop mail client for Linux, written in Rust, with a UI inspired by Spark.

- **UI**: [Slint](https://slint.dev) (native, no webview)
- **Engine**: IMAP (async, IDLE push) + SMTP, SQLite cache, OAuth2 (Gmail/Microsoft) and generic IMAP/SMTP accounts
- **Status**: early development — see [PLAN.md](PLAN.md) for the roadmap

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

- `crates/mail-core` — mail engine (IMAP, SMTP, OAuth, sync, SQLite store). No UI dependencies.
- `crates/app` — Slint UI + tokio/Slint bridge.
- `docs/` — architecture and development notes.
