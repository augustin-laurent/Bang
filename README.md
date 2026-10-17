# Bang

A desktop mail client for Linux, written in Rust, with a Mail macOS 26-inspired interface adapted to Linux.

- **UI**: [Slint](https://slint.dev) (native Slint main window; isolated local WebKit rich composer)
- **Engine**: IMAP (per-account syncer thread, IDLE push) + SMTP, SQLite cache, OAuth2 (Gmail/Microsoft) and generic IMAP/SMTP accounts
- **Status**: early development — see [PLAN.md](PLAN.md) for the roadmap

## Features

- Three-pane layout (folders, message list, reading pane) with a unified inbox
  across all accounts.
- Multiple accounts: OAuth2 (Gmail/Microsoft) or generic IMAP/SMTP.
- Read/unread, star, and delete, with per-folder unread badges.
- Rich compose with fonts, size, bold/italic/underline, colors, highlighting,
  links, lists, alignment and native file attachments. HTML and plain-text MIME alternatives.
- Full-text search across all accounts; French/English interface and locale-aware dates.
- Light/dark/system theme, desktop notifications for new mail, and a settings
  dialog for theme and account management.

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `j` / `↓` | Next message |
| `k` / `↑` | Previous message |
| `Home` / `End` | First / last message |
| `s` | Toggle star |
| `u` | Toggle read / unread |
| `d` | Delete |
| `Ctrl+N` | New message |
| `Ctrl+F` | Focus search |
| `Ctrl+Q` | Quit |
| `Esc` | Close reading pane / sidebar |

Single-letter actions apply to the selected message and are ignored while a
text field has focus.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the code is organized and how data flows
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — build, test, conventions, and Slint gotchas

## Build

```sh
./scripts/dev.sh

# Release build, after installing the Linux packages in docs/DEVELOPMENT.md:
npm ci --prefix crates/composer/web
npm run build --prefix crates/composer/web
cargo build --workspace --release
./target/release/app
```

The main window uses Slint’s software renderer. The rich composer requires GTK3
and WebKitGTK 4.1; it loads only bundled local assets. See
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for distribution packages.

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
- `crates/composer` — isolated GTK/WebKit host and local TypeScript/Tiptap editor.
- `assets/design/` — accepted visual references; `assets/fonts/` — bundled OFL fonts.
- `docs/` — architecture, design and development notes.
