# AGENT.md — working on Bang

Guidance for an AI agent (or new human) making changes in this repo. Read this
first; it points at the detailed docs instead of duplicating them.

## What this is

**Bang** is a desktop mail client for Linux, written in Rust. A Mail-macOS-style
three-pane UI (folders / message list / reading pane) built on **Slint**, with a
headless mail engine (IMAP + SMTP + OAuth2 + SQLite cache) and an isolated
WebKit rich-text composer. Early development.

- `crates/bang-core` — mail engine. **No UI dependencies.** Headless, unit-testable.
- `crates/app` — Slint UI + the tokio↔Slint bridge. The only crate that touches Slint.
- `crates/composer` — isolated GTK/WebKit host + local TypeScript/Tiptap editor.
- `assets/` — bundled OFL fonts and design references. `docs/` — the real docs.

## Quick commands

```sh
./scripts/dev.sh                          # build editor + release binaries, then run

# Manual:
npm ci --prefix crates/composer/web       # build-time only (embeds JS/CSS/fonts)
npm run build --prefix crates/composer/web
cargo build --workspace --release
./target/release/app

# Tests:
cargo test -p bang-core                   # unit (store, sync state, MIME)
cargo test -p bang-core --features integration   # IMAP integration (in-process server)
cargo test --workspace --features app/preview-renderer   # UI / i18n / composer
cargo check --workspace                   # MUST be clean before any commit
```

Runtime state: `~/.local/share/bang/bang.db` (SQLite, WAL),
`~/.config/bang/config.toml` (theme, language, OAuth client IDs).

## Architecture in one breath

Unidirectional, actor-style. UI → core is `UiCommand` over a private Unix socket;
core → UI is `MailEvent`, applied on the Slint event loop via
`slint::invoke_from_event_loop`. The per-user `bang-service` owns the tokio
runtime and mail engine. Each account has an IDLE syncer, an interactive IMAP
worker and a separate lazy OTP worker. A small supervisor handles GTK's abrupt
exit when the desktop session ends. Never call a syncer or write to the store
from a UI callback; route operations through `UiCommand`. Local-cache reads are
allowed. Full detail: `docs/ARCHITECTURE.md`.

## Code conventions

- **Conventional Commits**: `type(scope): imperative description`. One logical
  step per commit; **every commit must compile** (`cargo check --workspace`).
- **Docs**: every public item in both crates has `///` docs; both set
  `#![warn(missing_docs)]`. Keep it that way.
- **No UI deps in `bang-core`.** If it needs Slint, it belongs in `app`.
- **Smallest complete change.** Delete over add; reuse what exists; no options,
  abstractions, or config nobody asked for. A known limitation gets a one-line
  `shortcut: <limit>, <when to upgrade>` comment.
- **Lazy code needs a check.** New non-trivial logic (branch, loop, parser,
  anything touching security, a new script) ships with at least one small test
  or assert-based self-check.
- Adding a feature usually touches both crates: domain type + store method +
  `UiCommand`/`MailEvent` variant in `bang-core/events.rs`, handled in
  `service.rs`; then apply the event in `app/src/state.rs` and wire any new
  window callback in `main.rs`. See `docs/DEVELOPMENT.md` → "Adding a feature".

## Slint gotchas (1.18) — verified against the compiler source

These cost real time to discover. Full list in `docs/DEVELOPMENT.md`:

- Computed property: `out property <color> bg: expr;` — plain `:`, no `:=`.
  `=>` is a closure, `<=>` is two-way binding.
- A global's properties need `in`/`out`/`in-out` to be readable cross-component.
- `ListView` has no `model:`/`delegate:`; children are
  `for item in <expr>: Element { ... }`, read fields as `item.field`.
- `theme.slint` (the `Theme` global) **must be re-exported from the entry file**
  (`app.slint`) or it is constant-folded away. Access from Rust via
  `Theme::get(&ui)` (no `Theme::local()` in 1.18).
- `in` property = write-only from Rust; use `in-out` for a getter.
- Slint `int` is Rust `i32`; strings are `SharedString` (`.into()`); store ids
  are `i64` — cast at the boundary.
- `include_modules!()` re-exports components at the crate root (no `crate::ui`).
- No `on_close_requested` / `slint::exit` in 1.18; the process exits when the
  window closes and `run()` returns.

## Docs map

- `README.md` — features, shortcuts, build, account sign-in.
- `docs/ARCHITECTURE.md` — module-by-module layout, threading, store schema,
  sync engine, reader/composer security model.
- `docs/DEVELOPMENT.md` — prerequisites, build/test, conventions, Slint gotchas,
  debugging.
- `docs/ACCOUNTS.md` — OAuth registration, iCloud app passwords, token renewal.
- `docs/DESIGN.md`, `docs/VALIDATION.md` — UI rationale and test notes.
