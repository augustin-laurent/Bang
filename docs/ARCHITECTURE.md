# Bang — Architecture

Bang is a desktop mail client for Linux. This document describes how the
current code is organized and how data flows through it. For the product
roadmap see [../PLAN.md](../PLAN.md); for build/test instructions see
[DEVELOPMENT.md](DEVELOPMENT.md).

## Workspace layout

```
crates/
  bang-core/   # Mail engine. No UI dependencies.
    src/
      account.rs   # AccountConfig, ImapConfig, SmtpConfig, AuthMethod, TlsMode
      message.rs   # MessageSummary, MessageFull, Folder, EmailAddress, flags
      error.rs     # Error / Result types
      events.rs    # UiCommand (UI -> core) and MailEvent (core -> UI)
      store.rs     # SQLite store (accounts, folders, messages, FTS5 search)
      imap.rs      # Synchronous IMAP client wrapper (login, fetch, IDLE)
      oauth.rs     # OAuth2 PKCE flow for Google and Microsoft
      smtp.rs      # SMTP sending via lettre
      mime.rs      # MIME parsing and text/HTML extraction
      sync.rs      # Per-account sync engine (connect, sync, idle, reconnect)
      service.rs   # MailService: owns the store + one syncer per account
  app/           # Slint UI + the bridge to bang-core.
    src/
      main.rs      # Entry point, wiring, selection handlers
      bridge.rs    # tokio runtime thread + event forwarder
      state.rs     # Applies MailEvents to the window
      config.rs    # Loads ~/.config/bang/config.toml
    ui/            # .slint files (window, panes, theme, wizard)
```

`bang-core` is a library with **no UI dependencies**; everything that could
run headless lives there. `app` is the only crate that touches Slint.

## Threading model

There are three kinds of threads:

1. **Slint event loop** (main thread). Owns the window. All UI mutation
   happens here.
2. **`bang-core` thread** (one OS thread). Hosts a multi-thread tokio runtime
   (2 workers). Runs the `MailService` command loop and the event forwarder.
   Long-running async work (SMTP sends, OAuth logins) is spawned on this
   runtime.
3. **IMAP workers** (two per account). The background syncer owns the IDLE
   connection. A separate interactive worker opens a second connection on demand
   and reuses it for body fetches, flags and deletion. User actions do not wait
   for a quiet mailbox or a full folder synchronization.

The reason for the split: Slint must stay responsive, and IMAP `IDLE` must be
able to block without stalling anything else.

## The bridge (UI <-> core)

`app/src/bridge.rs` owns the two channels that cross the thread boundary:

- **Commands** (`UiCommand`): UI -> core, over a tokio `UnboundedSender`.
  The `bang-core` thread's command loop calls
  `MailService::handle_command` for each one.
- **Events** (`MailEvent`): core -> UI, over a tokio `UnboundedSender`. A
  `forward_events` task receives them and hops onto the Slint event loop via
  `slint::invoke_from_event_loop`, where `UiState::apply` mutates the window.

`Bridge` holds the command sender and **must stay alive** until the window
closes; the UI closures also hold senders, which keeps the command channel
open for the process lifetime.

### Command and event surface

`UiCommand` (UI -> core):

| Variant | Purpose |
|---|---|
| `AddAccount(config)` | Save account, start its syncer |
| `OAuthLogin { provider, client_id }` | Run OAuth2, then add the account |
| `RemoveAccount(id)` | Stop syncer, delete account |
| `SelectFolder { .. }` | (No-op: the UI reads the store directly) |
| `OpenMessage { account, message_id }` | Load body from cache or fetch via syncer |
| `SetFlags { .. }` | Add/remove IMAP flags |
| `SendEmail(req)` | Send via SMTP (spawned task) |
| `Search { account, query }` | FTS5 search |
| `Quit` | Stop all syncers |

`MailEvent` (core -> UI): `AccountStatus`, `FoldersUpdated`,
`MessagesAppended`, `MessagesUpdated`, `MessageLoaded`, `SearchResults`,
`SendResult`, `Error`.

## The store

`store.rs` is a single `rusqlite` connection (bundled SQLite) guarded by a
`std::sync::Mutex`, in WAL mode. One connection is enough at this scale; no
pool is needed.

Tables:

- `accounts` — id, email, serialized `AccountConfig`, created_at.
- `folders` — id, account_id (FK), name, path, unread, last_uid, uidvalidity.
- `messages` — id, account_id (FK), folder_id (FK), uid, message_id,
  from/to/cc, subject, date, flags, preview, has_attachments, body_text,
  body_html. Unique on `(account_id, folder_id, uid)`.
- `attachments` — id, message_id (FK), filename, content_type, size, path.
- `messages_fts` — FTS5 virtual table over `subject, from_name, from_email,
  preview, body_text`, kept in sync by `AFTER INSERT/UPDATE/DELETE` triggers.

Notes:

- The `to` column is a SQLite reserved word and is quoted in all SQL.
- `get_message_full` fetches the summary **before** locking the connection;
  calling `get_message` while holding the mutex would deadlock (the mutex is
  not reentrant).
- `open_in_memory()` is available under `#[cfg(any(test, feature =
  "integration"))]` for tests.

## The sync engine

`sync.rs` runs one loop per account (`run_syncer`):

1. `connect_and_sync` — connect, then `sync_folders`.
2. `sync_folders` — `LIST` folders, reconcile with the store (upsert + delete
   missing), then `sync_folder` for each.
3. `sync_folder` — `SELECT`, read the stored sync state `(last_uid,
   uidvalidity)`, fetch envelopes only for new mail and refresh `(UID FLAGS)`
   for the last 200 cached UIDs. Skip unchanged flag writes and events, record
   the new sync state, and emit
   `MessagesAppended` / `MessagesUpdated`.
4. `idle_loop` — `SELECT` the INBOX (or first folder), then `IDLE` for up to
   10 s. On a mailbox change, re-sync that folder; on timeout, check shutdown
   before renewing IDLE. Interactive commands never enter this queue.
5. On any error, emit `Reconnecting` and retry with exponential backoff
   (1 s .. 60 s).

`run_commands` handles the per-account interactive queue on a second thread.
Pending body requests are coalesced to the latest selection, including requests
received during login. Flag and delete actions retain their order. A body already
being downloaded finishes and is cached; stale UI results are ignored by the
selection guard. The command connection is reused and a failed read reconnects
once; mutations are not replayed automatically. Every command checks UIDVALIDITY
when selecting a cached folder, before acting on its stored UIDs. Cached opens
remain local and do not need a second connection.

A fresh folder has sync state `(0, None)`; the syncer treats `last_uid == 0`
(or a changed UIDVALIDITY) as a full sync of the most recent `SYNC_WINDOW`
(200) messages. Initial FETCH uses sequence numbers so UID gaps do not shrink
that window. Incremental metadata requests use a bounded UID range when UIDNEXT
is available; no metadata request is sent when there is no new mail. A changed
UIDVALIDITY discards cached bodies, attachments and the checkpoint before fetching
replacement messages, preventing reused UIDs from displaying old content.

Sync never fetches body parts or previews. `OpenMessage` fetches the full MIME
message with `BODY.PEEK[]` on demand and later opens use the local cache, including
empty and HTML-only bodies. SQL NULL means unfetched; an empty string means
fetched and empty. Full-text body search covers only bodies already downloaded.

## IMAP client

`imap.rs` wraps the synchronous `imap` crate. `ImapSession<T>` is generic over
the stream (`TlsStream<TcpStream>` for TLS, `TcpStream` for plaintext);
`ImapConn` is an enum over the two. It exposes `list_folders`, `select`,
`fetch_meta`, `fetch_body`, `set_flags`, `idle_wait`, `close`, `logout`, and
`connect` (which branches on `TlsMode`: implicit TLS, STARTTLS, or
plaintext). XOAUTH2 is supported via an `Authenticator`.

## OAuth2

`oauth.rs` implements the authorization-code + PKCE flow for Google and
Microsoft. It opens the system browser, runs a `tiny_http` server on
localhost to catch the redirect, exchanges the code for tokens, and returns
them. The app stores the resulting tokens in the account's `AuthMethod::OAuth2`
and uses them as XOAUTH2 credentials for IMAP/SMTP. Client IDs come from
`~/.config/bang/config.toml`.

## SMTP

`smtp.rs` sends via `lettre` on the tokio runtime. It builds a transport from
the account's `SmtpConfig` + `AuthMethod` (password or XOAUTH2) and sends the
message, reporting a `SendResult` event. The message body is built by
`build_message`: a plain body when there are no attachments, or a
`multipart/mixed` message (text part + one part per `OutgoingAttachment`) when
there are. An unparseable attachment content type falls back to
`application/octet-stream`.

## MIME

`mime.rs` parses raw messages with `mail-parser`, extracting the plain-text
body, HTML body, and attachments. `extract_preview` collapses whitespace and
truncates for list previews. Note that `mail-parser` converts text parts to
HTML (and vice versa), so `body_html` may hold converted markup; the UI
prefers `body_text` when it is non-empty.

## The UI

The window (`ui/app.slint`) is a three-pane layout:

- **Sidebar** (`sidebar.slint`) — account status, folder list, "Add account".
- **Message list** (`messagelist.slint`) — rows for the selected folder.
- **Reading pane** (`readingpane.slint`) — the selected message.

`theme.slint` is a global holding the color/typography/spacing tokens for
light/dark. It must be re-exported from the entry file (`app.slint`) so Rust
can reach it — see the Slint gotchas in DEVELOPMENT.md. `models.slint` defines
the `FolderRow`, `MessageRow`, and `AccountRow` structs used by the list views.
`accountwizard.slint` is the add-account overlay (OAuth buttons + a generic
IMAP/SMTP form). `settingsdialog.slint` is the settings overlay (theme
selection + account management).

The toolbar hosts a search box (live FTS5 results via `Store::search_all`) and
a settings button. The compose window (`ComposeWindow`, a second exported
component in `app.slint`) handles to/cc/bcc/subject/body plus file-path
attachments.

`state.rs` (`UiState`) holds the current selection (and a `search_active`
flag) and applies each `MailEvent` to the window; it also fires desktop
notifications for new mail (via `notify-send`) unless the user is already
viewing the affected folder. `main.rs` wires the window callbacks (folder
selection, message selection, search, the wizard, compose, settings) to the
store and the `Bridge`.

### List rendering

The message list keeps 90-pixel rows and lets Slint manage the virtual content
height, preserving its fast path for distant seeks. Horizontal scrolling is
disabled. Keyboard navigation reveals the selected row; changing folders or
search queries resets the scroll position.

Message and folder refreshes reconcile stable identity keys in the existing
`VecModel`: unchanged rows emit no updates, changed rows update in place, and
arrivals/removals preserve surviving delegates and their viewport. Reordering a
view resets its contents once. Account headers use the account id in their key.
Append events reload the sorted cache view, capped at 200 messages, so repeated
events cannot duplicate entries or grow the model indefinitely. This cap is
independent of rendering virtualization; older server mail still needs paging.

UI regression tests render a synthetic 10,000-message model, bound the number of
model reads and live delegates for initial rendering and direct seeks, and check
viewport preservation during flag changes, arrivals and removals.

### Reading the store directly

For folder contents the UI reads the local store directly (no
`SelectFolder` round-trip through the core). This is an intentional
relaxation of the "all data flows through events" convention, limited to
local-cache reads.

## Configuration

`app/src/config.rs` loads (and saves) `~/.config/bang/config.toml`:

```toml
theme = "system"            # "light" | "dark" | "system"

[oauth.google]
client_id = "..."
[oauth.microsoft]
client_id = "..."
```

The `theme` key is written by the settings dialog. A missing file or missing
keys simply yields defaults (theme `system`, empty client IDs), in which case
the OAuth buttons report an error and the generic IMAP/SMTP path remains
available.

When `theme` is `system`, the app detects the desktop's dark mode at startup
by querying `gsettings` then `kreadconfig5` in turn and using the first that
reports a value (this covers GNOME, KDE, and compositors such as Hyprland
where `gsettings` is present), falling back to light when neither does.


## Native UI and isolated rich composer

Bang's navigation, message list, text reader and settings remain Slint. The
native `bang-compose` process owns its own GTK event loop and Wry/WebKit view,
so GTK and Slint do not share an event loop. GTK hosts support X11 and Wayland.
The parent opens one writer at a time and focuses it on subsequent requests.

`crates/composer/src/lib.rs` defines the JSON-lines protocol over private process
stdin/stdout. Only sender identities, preferences, draft fields and attachment
metadata cross it. Credentials and attachment bytes stay in the parent process.
The child invokes a native file picker; the parent reads the selected files off
the UI thread and assigns stable IDs for removing them.

The TypeScript editor uses Tiptap with local open-source extensions. Static assets
are built with Vite and embedded at compile time; a local custom protocol serves
an allowlisted asset map. Remote navigation and network connections are blocked.
No external HTML message is loaded into this editor.

Sending forwards HTML plus text to the existing `SendEmail` command. SMTP builds
`multipart/alternative` and nests it in `multipart/mixed` for files. The writer
stays alive, and attachment bytes remain staged, until `SendResult` confirms
success. Failure restores editing without losing content. Close confirmation is
handled in the writer and acknowledged to the parent before closing Bang.

`crates/app/i18n/fr.json` is the shared English-key/French-value catalogue. The
Slint build produces an embedded PO catalogue from it; Rust and the composer use
the same source. Mail content and custom folder names are never translated.
