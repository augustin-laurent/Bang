# Bang

A desktop mail client for Linux, written in Rust, with a Mail macOS 26-inspired interface adapted to Linux.

- **UI**: [Slint](https://slint.dev) (Slint chrome in a GTK3 host, embedded WebKit HTML reader; isolated local rich composer)
- **Engine**: IMAP (per-account syncer thread, IDLE push) + SMTP, SQLite cache, OAuth2 (Gmail/Microsoft) and generic IMAP/SMTP accounts
- **Status**: early development — see [PLAN.md](PLAN.md) for the roadmap

## Features

- Three-pane layout (folders, message list, reading pane) with a unified inbox
  across all accounts.
- Sender photos/logos with colored initials, a local avatar cache and an online-avatar setting.
- Local inbox categories (Primary, Transactions, Updates, Promotions), message/sender corrections,
  and custom colored labels shared across accounts on this device.
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
./scripts/dev.sh # rebuild and launch the optimized app

# Manual build, after installing the Linux packages in docs/DEVELOPMENT.md:
npm ci --prefix crates/composer/web
npm run build --prefix crates/composer/web
cargo build --workspace --release
./target/release/app
```

The main window uses Slint’s software renderer inside GTK3, with WebKitGTK 4.1
for the embedded HTML reader. Remote message images load only after a click;
embedded CID images are local. The isolated rich composer loads only bundled assets. See
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for distribution packages.

## Account sign-in

Google and Microsoft use browser-based OAuth with desktop client registrations.
iCloud has prefilled servers; other providers use separate IMAP/SMTP settings.
New accounts are checked before saving, without sending a message.

See [docs/ACCOUNTS.md](docs/ACCOUNTS.md) for Google/Microsoft registration,
iCloud app passwords, configuration examples and the current OAuth token renewal
limitation.

## Layout

- `crates/bang-core` — mail engine (IMAP, SMTP, OAuth, sync, SQLite store). No UI dependencies.
- `crates/app` — Slint UI + tokio/Slint bridge.
- `crates/composer` — isolated GTK/WebKit host and local TypeScript/Tiptap editor.
- `assets/design/` — accepted visual references; `assets/fonts/` — bundled OFL fonts.
- `docs/` — architecture, design and development notes.
