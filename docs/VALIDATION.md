# Account setup and light-theme controls — 2026-11-02

Settings and account setup were checked using fictional offline data and the real
Slint software renderer. This is a native GTK/Slint interface: there is no web
route, DOM, framework overlay or browser console to inspect. Browser plugin not
available; browser automation is not applicable to these surfaces.

The flow under test is: Settings → Add account → provider selection → provider
form / manual presets → validation progress → failure and retry or account saved.

- Provider rows show bundled Google/Microsoft marks, a cloud for iCloud and a
  server icon for other mailboxes. Settings account rows use the authentication
  provider rather than guessing it from the email domain.
- Appearance choices have individual previews; the live theme controls their
  labels and outlines. Explicit field, checkbox, selection and focus colours
  remain visible in light/dark modes. Standard controls also follow repeated
  dark/light changes automatically.
- The UI interaction test clicks Google and Back, opens iCloud, checks independent
  IMAP TLS / SMTP STARTTLS presets, types into the actual email input and checks
  the primary action stays visible at 720×480. It tests ignored stale results,
  background reconnects, retained fields on failure and password clearing on
  success. Password visibility is clickable and resets when clearing the field.
  Invalid ports and incomplete addresses are covered separately.
- OAuth tests exercise a real loopback HTTP callback with origin-relative request
  targets, an unrelated favicon request, invalid/missing state, refused consent,
  missing code and listener cancellation. Microsoft scopes target Exchange and
  the common tenant; its supplied mailbox is verified by IMAP/SMTP rather than
  calling Graph with an Exchange token. Microsoft uses a localhost redirect for
  ephemeral-port matching; Google uses the literal IPv4 loopback address.
- Simulated SMTP verifies successful/rejected authentication and asserts setup
  never issues MAIL, RCPT or DATA. A simulated IMAP rejection proves failed
  credentials are not saved. Cancellation suppresses an old attempt's result.
- The workspace suite passed **90 tests**, with the native WebKit reader test
  still ignored by the general suite. The tests required local loopback access;
  no real account, authorization grant, synchronization or mail delivery was used.
- Rendered snapshots were inspected in French/light at 1440×900 and 720×480,
  and dark at 1024×768 and 720×480 (including English settings). Provider choice,
  manual form, settings, waiting and error states remain usable. Small windows
  scroll the form while keeping Back, Connect and Close available.

Design direction: Achromatic ledger, seed 1527190784, adapted to Bang's existing
Noto Sans, neutral surfaces and blue selection. The native modal retains its
single-column layout for the 720px minimum window; provider marks are identity
colours. HTML inspector checks are inapplicable to Slint; rendered snapshots and
native Slint element interaction tests were used instead. Fixtures remain the
existing fictional inbox and account addresses.

`docs/ACCOUNTS.md` documents client registration and the remaining automatic-token
renewal work. Real Google/Microsoft consent and long-lived sessions have **not**
been verified; initial sign-in still requires registered desktop clients, and
expired OAuth tokens still require reconnecting. No host display-server or
WebKit-native regression was rerun for this settings-only change.

# Avatar sizing regression — 2026-11-02

A rendered Slint regression fixture reproduced cached 80px photos displaying at
80px in both the 32px list avatar and 40px reader avatar. The child Image had no
explicit dimensions and used its source's intrinsic size. SenderAvatar now binds
the Image bounds to its frame and clips its children.

The new test checks both 80px and 512px sources, asserts 32×32 / 40×40 displayed
bounds and verifies rendered avatar pixels never escape those bounds. All eight
UI regression tests pass, including virtualized scrolling and late avatars.

# Avatars and local categories — 2026-11-02

The new organization features were validated with fictional messages and temporary
stores. No real-account synchronization, sending or public avatar requests were
used for these checks.

- The optimized workspace build (`cargo build --workspace --release --offline`)
  completed successfully.
- The workspace suite passed 78 tests (20 app, 43 core, 12 IMAP, 2 lazy-cache,
  1 composer protocol). The native reader test is excluded from that count and
  passed separately under X11 and Wayland, with dark mode on Wayland.
- V5→V6 migration preserves cached bodies/search and backfills categories.
  Restart, ordinary metadata refresh, message/sender rule precedence, future mail
  across accounts, filtering before the 200-row cap and whole-cache unread counts
  are covered. Label CRUD, multiple assignments and deletion without mail loss pass.
- The IMAP simulator returns actual targeted HEADER.FIELDS literals. Classification
  recognizes mailing-list signals before any body is downloaded; existing lazy
  body, UIDVALIDITY, IDLE and interactive-fetch tests still pass.
- A local HTTP simulator covers valid images, missing avatars and oversized
  responses. Additional tests cover invalid images, private/reserved addresses,
  circular masking, positive/negative cache freshness, deduplication and cache
  reuse after worker restart and replay after disabling/re-enabling online avatars.
  The online setting preserves existing preferences.
- UI tests click category tabs, open Classify and toggle label assignments; they
  check late-avatar identity guards and disabled avatars. Search suspends saved
  category/label filters; clearing it restores the selected view. A correction
  that removes the selected message from its view clears the reader.
- Existing 10,000-row virtualization and viewport tests pass. A shared transparent
  image avoids Slint 1.18's unequal-empty-image behavior, preserving unchanged
  row comparisons. Category and label models also reconcile stable identities.
- Slint screenshots were inspected at 1440×900 light/French and 1024×768
  dark/English, plus Classify and label management at 720×480. The new modal was
  added to the real native-reader visibility test and passes on both backends.

The pre-existing imap-proto future-compatibility warning remains. Categories are
local heuristics and can be corrected; labels are local cache annotations and do
not synchronize to other clients. Public Gravatar/favicon availability was not
queried during the automated tests.

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

## Native click regression — 2026-11-02

A real-pointer test reproduced clicks being swallowed before they reached Slint,
even with no message loaded. The prior reader fixture sent pointer events
directly to Slint, so it did not test GTK's input routing. GtkOverlay creates an
input window over its full-size child even when GtkFixed has no window of its
own. The overlay now passes empty-area input through, preserving WebKit's own
interactive child window. Global shortcuts also restore the Slint app focus
scope after interacting with HTML or a dialog.

The native fixture can enable `BANG_NATIVE_POINTER_TEST=1` to send actual XTEST
mouse events through Xvfb (direct X11, or Weston's outer X11 server for native
Wayland clients). It checks toolbar actions before/after HTML loads, folder
selection, settings/close, narrow list/back navigation, the image-consent button
and pointer delivery to WebKit. These checks do not invoke click callbacks or
Slint's pointer-dispatch API directly. The original fixture remains available
without XTEST for desktop sessions.

## HTML reader and encoding — 2026-11-02

The mail reader now uses an embedded WebKitGTK widget in the GTK/Slint main
window. Validation used fictional messages and temporary stores only.

- Workspace compilation and the full Rust suite passed: 49 tests, including
  the local SMTP server and 4 IMAP integration tests. The native reader test is
  ignored in the general suite and run separately with a real display.
- The native reader passed under X11/Xvfb and Wayland/Weston. It checks decoded
  accents, tables, CID images, zero HTTP requests before consent, HTTP images
  and CSS backgrounds after consent, blocked scripts/styles/redirects, modal
  hiding, geometry, stale generations, theme, text fallback, keyboard and clipboard.
- X11 also exercises 200% display scale, dark chrome with a sender-defined white
  mail background, and narrow list/reader navigation. A native GTK screenshot
  was inspected; this includes the actual WebKit surface.
- MIME/store checks cover RFC 2047 Q/Base64 and folded headers, modified UTF-7,
  UTF-8/ISO-8859-1/Windows-1252 bodies, CID/disposition persistence, migration of
  an existing V4 cache, search after repair and metadata-only synchronization.
- Composer JavaScript tests and production asset build passed. The user's
  pre-existing tooltip change in `controls.slint` was preserved.

WebKitGTK was absent system-wide during the initial verification: installing
with sudo required an administrator password. It is now available system-wide
at the time of the click-regression follow-up. Build and native checks used extracted distribution
libraries in `/tmp`, with helper paths relocated only in those temporary
libraries. The test-only WebKit environment disables its process sandbox to run
inside this execution environment; the application does not set that variable.
A normal launch still requires the distribution package from DEVELOPMENT.md.
Provider login, real-account synchronization and delivery were not exercised.
The existing imap-proto future-compatibility warning and software-display Mesa
warnings remain. Slint can log a long instantiation chain while the native
fixture changes layout; the asserted final layout and interactions pass.

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
