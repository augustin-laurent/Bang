# Bang — Development Guide

How to build, test, and extend Bang. For the system design see
[ARCHITECTURE.md](ARCHITECTURE.md); for the roadmap see
[../PLAN.md](../PLAN.md).

## Prerequisites

- A recent stable Rust toolchain (`rustup`).
- Linux. The UI uses the Slint software renderer, so there are **no GPU or
  system library dependencies**.

No other system packages are required: SQLite is built from source via the
`rusqlite/bundled` feature.

## Build and run

```sh
cargo build --release     # or: cargo build
cargo run -p app          # launches the UI
```

The first run creates `~/.local/share/bang/bang.db` (the SQLite store).

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

There are no frontend tests; the UI is exercised manually.

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

## OAuth client IDs

Gmail and Microsoft sign-in need your own OAuth2 client IDs (create them in the
Google Cloud Console / Azure Portal). Put them in
`~/.config/bang/config.toml`:

```toml
[oauth.google]
client_id = "..."
[oauth.microsoft]
client_id = "..."
```

Without them the OAuth buttons show an error and you use the generic
IMAP/SMTP account type instead.

## Debugging

- Set `RUST_LOG=debug` (or `bang_core=trace`) for `tracing` output from the
  engine.
- The store is a plain SQLite file at `~/.local/share/bang/bang.db`; inspect it
  with any SQLite tool.
- To exercise the IMAP path without a real server, run the integration tests:
  `cargo test -p bang-core --features integration`.
