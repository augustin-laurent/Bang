# Bang — Development Guide

How to build, test, and extend Bang. For the system design see
[ARCHITECTURE.md](ARCHITECTURE.md); for the roadmap see
[../PLAN.md](../PLAN.md).

## Prerequisites

- A recent stable Rust toolchain, Node.js 20.19+ (or 22.12+), npm and pkg-config.
- Linux with GTK3 and WebKitGTK 4.1 development packages. The Slint window uses
  the software renderer inside GTK; the HTML reader and composer use the system WebKit engine.

Distribution packages:

```sh
# Arch / Pond
sudo pacman -S --needed webkit2gtk-4.1
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev pkg-config
# Fedora
sudo dnf install webkit2gtk4.1-devel gtk3-devel pkgconf-pkg-config
```

Noto Sans is bundled under the SIL OFL in `assets/fonts/`. No font or editor
resource is fetched at runtime, and the editor requires no account or service.
SQLite is built from source via `rusqlite/bundled`.

## Build and run

```sh
./scripts/dev.sh
```

The script builds the editor and optimized Rust binaries, then launches the same
release profile. This keeps interactive performance checks on the current build.
The equivalent steps are:

```sh
npm ci --prefix crates/composer/web
npm run build --prefix crates/composer/web
cargo build --workspace --release
cargo run --release -p app
```

Keep `app` and `bang-compose` in the same directory when installing. For release,
use `cargo build --workspace --release`. Node/npm are build-time tools only:
compiled JS, CSS and fonts are embedded in `bang-compose`.

First run creates `~/.local/share/bang/bang.db`. Preferences are saved in
`~/.config/bang/config.toml`; the directory is created automatically.

```toml
theme = "system"    # system, light, dark
language = "system" # system, fr, en
```

Existing configuration files remain compatible; missing language defaults to
system. Personal folder names and message content are preserved as received.

## Tests

```sh
# Unit tests (store, sync state, MIME parsing).
cargo test -p bang-core

# IMAP integration tests (in-process test server).
cargo test -p bang-core --features integration
```

The integration tests are gated behind the `integration` feature so that a
plain `cargo test` stays fast and dependency-light. They spin up a minimal
IMAP server on `127.0.0.1` and drive the real sync engine against it,
including an IDLE push.

UI, localization and rich composer checks:

```sh
npm test --prefix crates/composer/web
cargo test --workspace --features app/preview-renderer
```

The SMTP test binds a loopback port, captures the MIME message and simulates a
server rejection. It never contacts an external mail server. Environments that
block local sockets must grant loopback access for this test.

For a deterministic Slint screenshot, without accounts, storage or network:

```sh
BANG_SNAPSHOT=/tmp/bang-inbox.png BANG_WIDTH=1440 BANG_HEIGHT=900 \
  BANG_LANGUAGE=fr BANG_THEME=light \
  cargo run -p app --features preview-renderer -- --preview
```

`BANG_SURFACE=settings`, `wizard` or `wizard-manual` opens a dialog. The preview renderer uses
Slint's real software rasterizer without a window manager. Omitting
`BANG_SNAPSHOT` opens an interactive native fixture.

For browser testing of the composer:

```sh
npm run dev --prefix crates/composer/web
# Open http://127.0.0.1:5173/?preview
# Optional: &theme=dark&lang=en
```

Only `?preview` enables sample messages and simulated send failure. Normal native
launches use neither fixtures nor simulated results. The in-page preview stores
its last send in `#app[data-preview-send]` for export assertions.

To exercise the real embedded reader, run the ignored native test separately
under each display server (an X11/Wayland session and loopback access are needed):

```sh
GDK_BACKEND=x11 cargo test -p app --features preview-renderer reader::tests::native_html_reader -- --ignored --exact --test-threads=1
GDK_BACKEND=wayland cargo test -p app --features preview-renderer reader::tests::native_html_reader -- --ignored --exact --test-threads=1
```

When running in isolated Xvfb (or Weston nested in Xvfb), set
`BANG_NATIVE_POINTER_TEST=1` to also drive real mouse clicks through XTEST.
This requires Python 3, libX11 and libXtst. `DISPLAY` must refer to that isolated
server; the Wayland fixture uses an origin-aligned, undecorated Weston output.
These checks cover compose, folders, opening/closing settings, narrow navigation,
the image-consent button and WebKit itself, without calling UI click handlers.

This uses a temporary mail store and a local image server, never real accounts.
It checks CID images, remote-image consent including CSS backgrounds, blocked
scripts/styles/redirects, stale selections, modal visibility, resize, theme,
keyboard input and clipboard. Optional `BANG_NATIVE_SNAPSHOT=/tmp/reader.png`
saves a native GTK screenshot on backends that support window capture.
`BANG_HTML_PREVIEW=1 cargo run -p app -- --preview` opens a fictional HTML mail
in the real GTK/WebKit host; `BANG_SNAPSHOT` remains the Slint-only fixture.

## Conventions

- **Conventional Commits**: `type(scope): imperative description`. One logical
  step per commit, and **every commit must compile** (`cargo check
  --workspace` clean).
- **Docs**: every public item in both crates carries `///` documentation; both
  crates set `#![warn(missing_docs)]`. Keep it that way.
- **No UI dependencies in `bang-core`**. If a change needs Slint, it belongs in
  `app`.
- **Smallest complete change.** Prefer deleting code over adding it, reuse what
  the codebase already has, and don't add options, abstractions, or
  configuration nobody asked for. A known limitation gets a one-line
  `shortcut: <limit>, <when to upgrade>` comment.
- **Lazy code needs a check.** New non-trivial logic (a branch, a loop, a
  parser, anything touching money or security, or a whole new script) ships
  with at least one small test or an assert-based self-check.

## Project layout

See [ARCHITECTURE.md](ARCHITECTURE.md#workspace-layout) for the module-by-module
breakdown. The short version:

- `crates/bang-core` — the mail engine (IMAP/SMTP, OAuth2, sync, SQLite store).
  Headless and unit-testable.
- `crates/app` — the Slint UI and the bridge that connects it to `bang-core`.
- `crates/composer` — the isolated GTK/WebKit composer and its local web editor.

## Adding a feature

A typical feature touches both crates:

1. **Core** (`bang-core`): add or change the domain type, the store method, and
   the `UiCommand`/`MailEvent` variants in `events.rs`. Handle the new command
   in `service.rs`. Add a unit test.
2. **Bridge/UI** (`app`): if the feature produces new events, apply them in
   `state.rs` (`UiState::apply`). If it needs a new window callback, declare it
   in the relevant `.slint` file and wire it in `main.rs`.
3. **Verify**: `cargo check --workspace` clean, `cargo test -p bang-core`
   green, then commit.

### Data-flow rules

- UI -> core goes through `UiCommand` (never call the store or a syncer
  directly from a UI callback, except for the documented local-cache reads of
  folder contents).
- Core -> UI goes through `MailEvent`, applied on the Slint event loop.
- Long-running work (SMTP, OAuth) is spawned on the `bang-core` tokio runtime,
  not run inline in the command loop.

## Slint gotchas (1.18)

These cost real time to discover; they are verified against the 1.18 compiler
source.

- **Computed property**: `out property <color> bg: expr;` — a plain `:` with an
  `out` (or `in`/`in-out`) qualifier. There is no `:=`. `=>` is a fat arrow
  (closures); `<=>` is a two-way binding.
- **Cross-component access**: a global's properties need `in`/`out`/`in-out` to
  be readable from other components.
- **ListView**: no `model:` or `delegate:` properties. Children are
  `for item in <model-expr>: Element { ... }`; read struct fields as
  `item.field`. `model.length` is a property, not a function. Import from
  `"std-widgets.slint"`.
- **Text**: `wrap: word-wrap` (not `wrap-mode`), `overflow: elide`,
  `font-weight` is an integer.
- **Rectangle**: `border-radius` (not `radius`).
- **Layouts**: `alignment: start|end|...` (not `align-vertical`). You cannot set
  both `width` and `min-width` on the same element.
- **`include_modules!()`** re-exports the components and exported structs at the
  crate root (there is no `crate::ui` module): `BangApp`, `FolderRow`,
  `MessageRow`.
- **Globals must be exported from the entry file** to be reachable from Rust.
  Only the entry file's exports get public-API treatment; a global defined in an
  imported file (e.g. `theme.slint`) is constant-folded away — its `in`
  properties are treated as never-set, so `dark ? a : b` collapses to a literal
  and the global is dropped from the generated code. Fix: re-export it from the
  entry file with `export { Theme } from "theme.slint";`. Access it from Rust
  with `Theme::get(&ui)` (1.18 has no `Theme::local()`).
- **Property visibility from Rust**: an `in` property is write-only from Rust
  (setter only). Use `in-out` to also get a getter, named `get_<name>()`.
- **Types**: Slint `int` is Rust `i32`; strings are `SharedString` (use
  `.into()`). Store ids are `i64`, so cast at the boundary.
- **`ui.as_weak()`** requires `use slint::ComponentHandle;`.
- **No `on_close_requested` / `slint::exit`** in 1.18. The process exits when
  the window closes and `run()` returns.
- **Models**: set with `slint::ModelRc::new(VecModel::from(rows))`; annotate
  `let rows: Vec<T>` to disambiguate `From`. Append with the `Model` trait's
  `insert_row` (`use slint::Model;`). `ModelRc` does not `Deref`.
- **`directories` 6.0**: `ProjectDirs::from(qualifier, org, app)` (not `get`);
  the data dir is `~/.local/share/bang`.

## Account sign-in

See [ACCOUNTS.md](ACCOUNTS.md) for desktop OAuth registration, loopback redirects,
provider-specific settings and token-renewal work still required. Configuration
is reloaded for each sign-in attempt. New accounts are validated asynchronously
against IMAP and SMTP before they are persisted; no test message is sent.

## Debugging

- Set `RUST_LOG=debug` (or `bang_core=trace`) for `tracing` output from the
  engine.
- The store is a plain SQLite file at `~/.local/share/bang/bang.db`; inspect it
  with any SQLite tool.
- To exercise the IMAP path without a real server, run the integration tests:
  `cargo test -p bang-core --features integration`.
