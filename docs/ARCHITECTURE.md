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
run headless lives there. `app` is the only crate that touches Slint. GTK3 owns its native window and
event loop; a custom Slint platform draws the existing chrome with the software
renderer, while a native WebKitGTK widget draws the HTML body.

## Threading model

There are three kinds of threads:

1. **GTK/Slint event loop** (main thread). GTK owns the native window,
   advances Slint timers and animations, and delivers Slint event-loop proxy
   callbacks through GLib idle sources. All UI mutation happens here.
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
| `AddAccount(config)` | Trusted session-only account; start its syncer. Use `ConnectAccount` for persistent, validated setup |
| `OAuthLogin { provider, client_id }` | Run OAuth2, then add the account |
| `RemoveAccount(id)` | Stop syncer, delete account |
| `SelectFolder { .. }` | (No-op: the UI reads the store directly) |
| `OpenMessage { account, message_id }` | Load body from cache or fetch via syncer |
| `SetFlags { .. }` | Add/remove IMAP flags |
| `Move { .. }` | Move UIDs with `UID MOVE`, or `UID COPY` + `\\Deleted` + UID EXPUNGE fallback |
| `Delete { .. }` | Move to the account's Trash; expunge in Trash/Junk or when Trash is absent |
| `MarkFolderRead { .. }` | Set `\\Seen` across the selected mailbox |
| `EmptyFolder { .. }` | Permanently expunge all UIDs in Trash/Junk |
| `CreateFolder` / `RenameFolder` / `DeleteFolder` | Administer server mailboxes and reconcile the local folder cache |
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

Schema V5 adds attachment `content_id` and `disposition` fields and repairs
cached RFC 2047 headers and modified UTF-7 folder display names transactionally.
Folder paths, message identities, bodies, flags and scheduling state are retained;
the normal UPDATE triggers refresh search entries. Metadata-only sync preserves
body-derived previews and attachment indicators.

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

Message moves and folder-admin operations use the same interactive connection
and UIDVALIDITY guard. `Delete` resolves Trash from the account's cached folder
paths; Trash/Junk deletions and accounts without Trash are permanent. The MOVE
capability is queried before each move; servers without it use COPY, mark the
source UIDs `\\Deleted`, then UID EXPUNGE. Successful moves remove source cache
rows and incrementally sync the destination. Folder changes re-LIST and reconcile
the local folder table before emitting `FoldersUpdated`.

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

`oauth.rs` implements system-browser authorization-code + S256 PKCE with a
short-lived loopback listener. Foreign callbacks cannot terminate an active
sign-in. `providers.rs` owns known endpoints and bundled public registrations;
local config is an optional complete development override.

`credentials.rs` owns Linux Secret Service persistence, in-memory sessions and
serialized refresh per account. `StoredAccount` / `StoredAuth` are secret-free
SQLite/UI metadata; runtime `AccountConfig` / `AuthMethod` carry resolved secrets
only inside the core. Legacy password rows migrate only after a keyring commit;
legacy OAuth grants without a known issuing registration require reconnect.

IMAP workers request current credentials over a channel to the core runtime;
SMTP asks the same credential manager. Refresh starts within 60 seconds of expiry,
keeps omitted refresh tokens, persists rotation before distribution, and stops
retrying revoked grants. Removal invalidates requests, cancels sends/stops workers,
deletes keyring items, then removes local metadata/cache. It is local disconnection,
not provider-wide revocation or forensic erasure.

`discovery.rs` reads only a domain’s two HTTPS autoconfig endpoints, with bounded
responses, deadlines and no credentials, redirects or external XML entities.
Discovered destinations are displayed for review before mail authentication.

## SMTP

`smtp.rs` sends via `lettre` on the tokio runtime. It builds a transport from
the account's `SmtpConfig` + `AuthMethod` (password or XOAUTH2) and sends the
message, reporting a `SendResult` event. The message body is built by
`build_message`: a plain body when there are no attachments, or a
`multipart/mixed` message (text part + one part per `OutgoingAttachment`) when
there are. An unparseable attachment content type falls back to
`application/octet-stream`.

## MIME

`mime.rs` uses `mail-parser` for RFC 2047 headers, MIME transfer encodings and
body charsets. Only actual HTML body parts populate `body_html`; plain text
remains plain text. CID resources retain their Content-ID and disposition,
including unnamed inline images, without pulling in parts of attached messages.
`extract_preview` collapses whitespace and limits Unicode characters.
IMAP metadata is decoded before storage; folder display names use modified
UTF-7 decoding while their server paths retain their exact wire representation.
A full-body fetch also refreshes decoded headers and the preview in the store.
Older cached HTML with missing CID metadata is shown immediately and refreshed
through the syncer when available.

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

Right-clicking a message or a real sidebar folder opens a Slint `PopupWindow`;
the cursor coordinates are captured by the row's `TouchArea`. Message actions
are routed through `UiCommand`, with `Move to...` listing only folders for that
message's account. Folder actions include mark-read, Trash/Junk empty, top-level
create, rename and confirmed delete; INBOX rename/delete are blocked in both UI
and core handlers. The virtual All Inboxes row and account headers do not expose
folder menus. Ctrl-click toggles message selection and Shift-click extends from
the anchor row; selection is held in `UiState` and reapplied when message rows
are reconciled. The batch bar and context menu omit reply/forward actions.

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

The settings dialog writes preferences. Missing OAuth overrides resolve to bundled
public registrations without serializing them into the per-user config. Google’s
Desktop client is bundled. Microsoft is currently disabled without a distributor
registration. Neither this file nor new SQLite account records store mailbox tokens
or passwords; those belong to Secret Service or the process-only session.

When `theme` is `system`, the app detects the desktop's dark mode at startup
by querying `gsettings` then `kreadconfig5` in turn and using the first that
reports a value (this covers GNOME, KDE, and compositors such as Hyprland
where `gsettings` is present), falling back to light when neither does.


## Native UI and isolated rich composer

Bang's navigation, message list, reader header, text fallback and settings remain Slint. The
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


## Embedded read-only HTML reader

`app/src/gtk_host.rs` implements the Slint platform/window adapter with GTK3,
Cairo and Slint's software renderer. A persistent pixel buffer enables partial repainting; only changed regions are converted for Cairo, and buffers are recreated on resize or scale changes. Pointer events, scrolling, key symbols,
composed text through GTK's input method, clipboard and close requests bridge to
Slint. GLib idle sources deliver cross-thread events on the UI thread. GTK hosts
the reader directly on both X11 and Wayland; there is no native window reparenting.
The full-window GtkOverlay child explicitly passes input through its empty
area to Slint, while WebKit's own child window remains interactive. Global
shortcuts restore focus to Slint's app scope before dispatch, including when
a hidden dialog button previously held focus.
The headless snapshot backend is still used for deterministic Slint fixtures.

`app/src/reader.rs` overlays a native WebKit widget in the body rectangle exported
by Slint. Geometry is measured in logical pixels, so GTK handles monitor scaling.
The view is hidden for modals, attachment/save dialogs, the sidebar overlay and
narrow-layout navigation. Global Ctrl+N/F/Q and Escape still reach Slint when the
web view has focus; ordinary reader keys retain native selection and scrolling.
The reader uses the sender's HTML/CSS, with theme defaults only for unstyled mail.
Failure leaves the plain-text fallback available. Message IDs and generations
prevent late WebKit failures from changing a newer selection.

Each displayed message has an ephemeral context, no accepted cookies, an exact
local document URL and an immutable CID image allowlist read through the store's
attachment path validation. WebKit JavaScript is disabled. Active HTML elements,
event handlers, metadata redirects and speculative-loading links are removed.
The document response carries a CSP blocking scripts, forms, frames, objects,
media, external styles/fonts and connections. Only local CID/data images are
allowed initially; “Load images” permits HTTP/HTTPS images (including CSS
backgrounds) for the current message. Selecting another message resets consent.
Only user link clicks can open HTTP/HTTPS externally or prefill a mailto draft.
The composer never receives the original untrusted reader HTML.


## Sender avatars and local organization (schema V6)

`organization.rs` supplies conservative FR/EN header-only classification. Metadata
FETCH adds `BODY.PEEK[HEADER.FIELDS (LIST-ID LIST-UNSUBSCRIBE PRECEDENCE AUTO-SUBMITTED)]`;
no body or preview is downloaded for classification. Opening a message refreshes
these signals from its fetched headers. The V6 migration backfills cached messages
from their existing sender/subject metadata, transactionally with the schema update.

Messages retain their normalized sender key, automatic category, optional message
override and small classification signals. Sender rules are global to this Bang
installation. Effective category precedence is message override, sender rule,
automatic result. IMAP upserts recompute only automatic data and preserve user
choices. Transactions take priority over Promotions, then Updates; uncertain mail
stays Primary. All view filtering happens in SQL before the 200-message limit;
unread category counts cover the entire non-snoozed cache for the inbox scope.

`labels` and `message_labels` store colored local labels and many-to-many message
assignments. Deleting a label cascades only assignments. Labels and corrections
survive restarts and ordinary synchronization, but message-bound assignments are
removed if their cache identity is deleted (including a UIDVALIDITY reset). They
are not IMAP keywords, Gmail labels or cross-device synchronization.

`store_organization.rs` implements these queries and mutations. UI mutations cross
`UiCommand`; `OrganizationChanged` and `OrganizationError` refresh the UI or report
a translated operation failure without disturbing reader loading. Label views span
accounts and folders; search remains global and temporarily suspends the active
category/label filter. The original filter is restored when the query is cleared.
Organization dialogs participate in the same modal guard that hides native WebKit.

`avatars.rs` in the core deduplicates asynchronous lookups, with at most four
active workers. Gravatar uses the normalized address's MD5 identifier; the
fallback is the sender domain's HTTPS favicon, except for common personal email
providers. Only DNS-pinned public HTTPS destinations are contacted, including after
redirects; no mail HTML URL, cookies or account credentials enter these requests.
Responses are limited to 1 MiB, decoded images to 1024px per edge and bounded
allocation, then resized to circular 80px PNGs. Validated files are cached beneath
Bang's cache directory for seven days; misses last one day. Failures keep initials
visible. `online_avatars` defaults to true in existing configurations and can be
disabled in Settings. UI completions are matched by normalized address, so a late
result cannot replace a different selected sender's avatar. Slint image objects
are reused on the UI thread and list rows remain 90px with virtualization intact.
