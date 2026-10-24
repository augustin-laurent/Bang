# Bang — Rust Mail Client (Spark-like UI, Linux Desktop)

## 1. Overview

- **Project**: Bang
- **Location**: `~/Projets/Bang`
- Rust end-to-end, Slint UI mimicking Spark: three-pane layout, sidebar with folders, unified inbox, clean modern design, dark/light themes.
- Linux desktop first; architecture stays portable for macOS/Windows later.

## 2. Technology Stack

| Layer | Choice | Notes |
|---|---|---|
| UI | **Slint** (latest 1.x) + `.slint` components | Declarative Rust-native UI. Custom styling (radii, gradients, box-shadow, animations), `ListView` virtualization, `Shortcut` elements. |
| Renderer | Software renderer (default) | Zero GPU deps, simple packaging. Upgrade to wgpu/GL later if perf demands. |
| Fonts | Bundle Inter TTF | Avoids fontconfig variance across distros. |
| IMAP | `imap` crate (async, rustls, IDLE) — validate vs `mailin` in M0 | Both maintained; `mailin` is newer (IMAP4rev2). |
| SMTP | `lettre` | De-facto standard. |
| MIME | `mail` | Body parsing, attachments. |
| Auth | `oauth2` + PKCE, localhost redirect via `tiny_http`, `webbrowser` crate | Gmail & Microsoft OAuth2 → XOAUTH2 for IMAP/SMTP. Plus generic username/password IMAP/SMTP. **Both in M0.** |
| Storage | SQLite via `rusqlite` (bundled) + FTS5 | Message cache, metadata, full-text search. |
| Async | `tokio` in a background thread | Bridged to Slint's event loop via `slint::invoke_from_event_loop`. |
| Notifications | `notify-rust` (DBus) | Linux desktop notifications. |
| Tray / shortcuts | `tray-icon`, `global-hotkey` | M3. Global hotkeys restricted on Wayland — in-app shortcuts always, global best-effort. |

## 3. Architecture

### 3.1 Crate layout (2 crates, clean boundary)

```
Bang/
├── Cargo.toml                     # workspace
├── PLAN.md                        # this plan
├── crates/
│   ├── mail-core/                 # NO UI dependency — pure Rust, unit-testable
│   │   └── src/
│   │       ├── lib.rs             # exports: MailService, types, Error
│   │       ├── account.rs         # AccountConfig, AuthMethod (Password | OAuth2)
│   │       ├── imap/              # client.rs, idle.rs — login, select, fetch, IDLE
│   │       ├── smtp/              # client.rs — send via lettre
│   │       ├── mime/              # parse.rs (MIME→parts), render.rs (parts→text/markup)
│   │       ├── oauth/             # pkce.rs (localhost redirect), google.rs, microsoft.rs
│   │       ├── sync/              # engine.rs, state.rs, events.rs
│   │       └── store/             # db.rs (migrations), accounts/folders/messages/attachments/search
│   └── app/                       # Slint UI + main
│       ├── src/
│       │   ├── main.rs
│       │   ├── state.rs           # UiState: Slint models, selection, app mode
│       │   ├── bridge.rs          # tokio ↔ Slint event-loop bridge
│       │   ├── theme.rs
│       │   └── views/             # per-view Rust logic (wizard, compose, settings)
│       └── ui/                    # app, sidebar, messagelist, readingpane, compose,
│                                  # accountwizard (.slint)
└── assets/                        # Inter font, icons
```

Rule: `mail-core` never imports Slint; `app` never touches IMAP/SQLite directly. Everything crosses the boundary as typed enums.

### 3.2 Threading & data flow (unidirectional, actor-style)

```
Slint UI thread                          tokio runtime (dedicated thread)
─────────────────                        ────────────────────────────────
user action (callback)
  → UiCommand ────────────────────────►  MailService (single task)
                                          ├─ spawns per-account Syncer tasks
                                          ├─ IMAP / SMTP / OAuth I/O
                                          └─ Store (SQLite, WAL)
  ◄────────────────────────────────── MailEvent
  bridge task: slint::invoke_from_event_loop
  → UiState.apply(event) → mutates Slint models → UI re-renders
```

- **`UiCommand`** (UI → core): `AddAccount`, `RemoveAccount`, `SelectFolder`, `LoadMessages`, `OpenMessage`, `SetFlags`, `SendEmail`, `Search`, `Snooze`, `Quit`
- **`MailEvent`** (core → UI): `AccountStatusChanged`, `FoldersUpdated`, `MessagesAppended`, `MessagesUpdated`, `MessageLoaded`, `SendResult`, `Error`, `NewMailNotification`
- Both are plain enums over an `mpsc::UnboundedSender` — trivially testable, no shared mutable state across threads.

### 3.3 MailService (the core API)

```rust
pub struct MailService {
    store: Store,
    accounts: HashMap<AccountId, AccountHandle>,  // one Syncer task each
    commands: mpsc::UnboundedReceiver<UiCommand>,
    events_tx: mpsc::UnboundedSender<MailEvent>,
}
```

Single task owns all state; syncers are child tasks that report back through `MailEvent`. UI holds only a `UiCommand` sender — it can't corrupt state.

### 3.4 Sync engine (per account)

```
loop:
  connect (backoff 1s→2s→…→60s on failure)
  for each folder:
    if uidvalidity changed → full re-sync
    else:
      new = UID FETCH > last_uid (UID, FLAGS, INTERNALDATE, BODY.PEEK[HEADER])
      changed = UID FETCH 1:last_uid (FLAGS)   # or CHANGEDMODS if MODSEQ
      upsert → store → emit MessagesAppended / MessagesUpdated
    persist (last_uid, uidvalidity, highestmodseq)
  IDLE → on EXPUNGE/EXISTS notify → re-sync affected folders
```

- **Initial sync**: last 200 messages/folder, headers + first ~2KB of text body (for previews). Older = headers only.
- **Bodies**: fetched on demand when a message is opened (`OpenMessage`), never in the sync loop.
- **SQLite**: WAL mode; `Store` serializes writes via an internal tokio mutex, reads on separate connections.

### 3.5 Storage on disk

```
~/.config/bang/config.toml               # theme, poll interval, OAuth client IDs
~/.local/share/bang/bang.db              # SQLite (WAL)
~/.local/share/bang/attachments/         # <message_id>/<filename>
```

OAuth tokens live in the DB, file perms `0600` (MVP); `keyring` integration is a later option.

### 3.6 Error handling

- `mail_core::Error` (thiserror): `Imap`, `Smtp`, `Auth`, `Store`, `Network`, `Config`
- **Transient** (network, IDLE drop) → silent retry with backoff, UI shows "reconnecting"
- **Permanent** (auth failure, bad credentials) → account marked disconnected, surfaced in UI with retry/remove actions
- UI: error banner + toasts; account row shows a status dot

## 4. Conventions

### 4.1 Commits

- **Conventional Commits**: `type(scope): imperative description`
- Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `build`, `ci`
- Scopes: `core`, `imap`, `smtp`, `oauth`, `store`, `sync`, `mime`, `ui`, `build`, `ci`
- One step = one commit; every commit must compile.
- **The numbered commit list below is a guideline, not a strict order** — steps may be reordered or merged as work progresses.
- Trunk-based on `main`; milestone tags (`v0.0.1` = M0, `v0.1.0` = M1, …).

### 4.2 Code documentation (Javadoc style)

- Every **public** item in `mail-core` and `app` gets a `///` doc comment, Javadoc-style:
  - One-line summary (what it does, imperative mood)
  - Blank line, then details where non-obvious
  - `# Arguments`, `# Returns`, `# Errors`, `# Examples` sections where applicable
- `cargo doc --no-deps` must build cleanly (no broken intra-doc links, no missing docs on public items — enable `#![warn(missing_docs)]` in both crates).
- Private helpers: comment only the *why*, one line.

### 4.3 Testing

- **Backend (`mail-core`) only** — no frontend tests (UI verified manually).
  - Unit: store CRUD on in-memory SQLite, sync state machine, MIME parsing fixtures
  - Integration: real IMAP server in Docker (Dovecot or MailSlurper), feature-gated `#[cfg(feature = "integration")]`, runs in CI

## 5. Commit Plan (guideline order)

### M0 — Spike (validates the riskiest paths)

| # | Commit |
|---|---|
| 1 | `chore: initialize git repository` |
| 2 | `build: add mail-core and app crates with slint dependency` |
| 3 | `feat(core): define account, auth, and message domain types` |
| 4 | `feat(store): implement sqlite store with migrations` |
| 5 | `feat(imap): implement client (login, folders, fetch)` |
| 6 | `feat(oauth): implement pkce flow for google and microsoft` |
| 7 | `feat(smtp): implement sending via lettre` |
| 8 | `feat(mime): implement parsing and text extraction` |
| 9 | `feat(sync): implement per-account sync engine with idle and reconnect` |
| 10 | `feat(ui): scaffold slint three-pane layout with theme global` |
| 11 | `feat(ui): implement tokio-slint bridge (commands and events)` |
| 12 | `feat(ui): account wizard (gmail oauth + generic imap/smtp)` |
| 13 | `feat(ui): render folder list and message list` |
| 14 | `feat(ui): render message body in reading pane` |
| 15 | `test(core): unit tests for store, sync state, mime parsing` |
| 16 | `test(core): imap integration tests against local server` |
| 17 | `docs: architecture and development guide` |

### M1 — MVP

| # | Commit |
|---|---|
| 18 | `feat(ui): spark design tokens (colors, typography, spacing)` |
| 19 | `feat(core): unified inbox smart folder` |
| 20 | `feat(core): flag operations (read/unread, star, delete)` |
| 21 | `feat(ui): message row actions and unread indicators` |
| 22 | `feat(ui): compose window (to/cc/bcc/subject/body)` |
| 23 | `feat(core): send email with attachments` |
| 24 | `feat(store): fts5 full-text search` |
| 25 | `feat(ui): search bar with live results` |
| 26 | `feat(ui): dark/light theme with system follow` |
| 27 | `feat(ui): desktop notifications for new mail` |
| 28 | `feat(ui): settings dialog` |

### M2 — Spark features

| # | Commit |
|---|---|
| 29 | `feat(core): snooze and follow-up scheduling` |
| 30 | `feat(core): send later` |
| 31 | `feat(ui): keyboard shortcuts` |
| 32 | `feat(ui): swipe/drag actions on message rows` |
| 33 | `feat(core): attachment download and preview` |
| 34 | `feat(core): drafts and templates` |
| 35 | `feat(core): multi-account support` |
| 36 | `feat(mime): html subset renderer for reading pane` |
| 37 | `feat(ui): reply/forward with quoting` |

### M3 — Polish & ship

| # | Commit |
|---|---|
| 38 | `perf(core): metadata-only sync and lazy body fetch` — completed |
| 39 | `perf(ui): list virtualization tuning` — completed |
| 40 | `build: package deb/rpm/appimage` |
| 41 | `feat(ui): tray icon and autostart` |
| 42 | `ci: ubuntu build and package pipeline` |

(`fix(...)` commits appear anywhere as bugs surface — never bundled with features.)

## 6. Key Risk: HTML Email Rendering

The reader now uses WebKitGTK 4.1 inside a GTK3 host shared with Slint's
software renderer. Slint retains the existing navigation, message list, reader
header, dialogs and plain-text fallback. This supports native embedded HTML on
X11 and Wayland without reparenting a child window. Remote images require a
per-message click; CID images load locally. See `docs/ARCHITECTURE.md` for the
content policy and the GTK/Slint integration.

## 7. Other Risks & Mitigations

| Risk | Mitigation |
|---|---|
| No rich-text compose in Slint | Plain-text compose for MVP; revisit later |
| Slint missing some widgets | Build small custom components; Slint 1.x is mature |
| tokio ↔ Slint event-loop bridging | Established `invoke_from_event_loop` pattern; isolated in `bridge.rs` |
| Wayland global hotkey restrictions | In-app shortcuts primary; global hotkeys best-effort |
| Large mailboxes | `ListView` virtualization + metadata-only sync + lazy bodies |
| Provider quirks (Gmail token refresh, MS OAuth+IMAP, IDLE drops) | Backoff/reconnect; integration tests against a local IMAP server |
| IP: Spark is proprietary | Mimic look & feel only; all assets/code built from scratch |
