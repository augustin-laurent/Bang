# Mail interface validation — 2026-11-01

The implementation follows the accepted references in `assets/design/`.
Screenshots use fictional offline fixtures; no real recipient was contacted.

## Real first-open latency — 2026-11-02

Read-only Gmail probes (`BODY.PEEK[]`) measured uncached messages from both
ends of the existing inbox, without changing server flags or exposing message
content in the timing output. Connections took 255–494 ms; SELECT 35–205 ms;
body transfers 50–266 ms; MIME parsing 0–7 ms. These samples had no attachments.

The native reader rendered three large cached HTML bodies in 227–346 ms.
A complete first-open probe used the real Bridge, MailService, IMAP workers,
Slint and WebKit with an isolated in-memory store and no outgoing queue. Three
uncached old messages reached the UI in 475/127/141 ms and completed HTML
loading in 691/209/228 ms. Temporary profiling code was removed afterward.
These probes did not reproduce the reported 5–10 s delay.

The existing release executable predated the IDLE fix; it was rebuilt. The
standard launch script now builds and runs the same release profile. Shell
syntax and a stubbed launch check verify both Cargo commands include `--release`.
Confirmation of the user's launch path and a slow interaction in the instrumented
window remained pending at the end of this investigation.

## Automated checks passed

```sh
npm run build --prefix crates/composer/web
npm test --prefix crates/composer/web
cargo build --workspace --features app/preview-renderer
cargo test --workspace --features app/preview-renderer,bang-core/integration
git diff --check
bash -n scripts/dev.sh
```

25 Rust tests passed: 6 application tests, 1 composer protocol test, 15 core
tests and 3 local IMAP integration tests. Coverage includes language preferences
saved to a new directory, existing configurations, grouped/collapsed accounts,
custom folder names, draggable navigation, narrow list/reader navigation,
search in an empty list and ignoring a stale message fetch after leaving its
folder. MIME tests parse Unicode HTML/plain alternatives and attachments, check
Bcc privacy and exercise both acceptance and rejection by a local SMTP server.
The JavaScript tests cover recipients, link protocols and localized file sizes.

## Rendered interface checks passed

- Slint software snapshots: 1440 × 900, 1024 × 768 and 720 × 480, light/French
  and dark/English; account and settings dialogs at reduced sizes.
- Browser composer: 1440 × 900, 1024 × 768, 900 × 720 and 640 × 480;
  light/French and dark/English. No console warning or error in the final view.
- Editor interactions: font, size, bold, italic, underline, text color,
  highlight toggle/color, links and cancel, bullet/numbered lists, alignment,
  accented text, paste and undo/redo. Inspected the resulting editor HTML.
- Simulated failed send: inspected HTML/plain export, retained body and file;
  removed the attachment and verified the discard/keep-editing dialog.
- Native GTK/WebKit host: loaded embedded editor, exchanged ready/init/close
  messages and exited successfully under both Wayland and X11.

## Environment and remaining manual checks

WebKitGTK 4.1 is not installed system-wide in this environment. Compilation and
native smoke tests used temporary libraries outside the repository. Install the
distribution package documented in [DEVELOPMENT.md](DEVELOPMENT.md) before a
normal launch with `./scripts/dev.sh`.

The native smoke tests do not replace a full desktop interaction pass. Keyboard
focus, window raising, file-picker selection/cancellation and scrolling still
need hands-on verification in the final installed package on both Wayland and
X11. Browser screenshots verify the editor layout; they are not WebKit screen
captures. Provider OAuth and real-account delivery were not exercised.
