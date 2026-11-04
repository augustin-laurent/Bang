# Bang v0.1.0 — Linux preview

The first experimental Linux x86_64 release of Bang, a Rust/Slint desktop mail
client with a GTK/WebKit reader and an isolated rich-text composer.

- Unified inbox, IMAP/SMTP, local search, categories, labels and attachments.
- Guided account setup, a welcome flow, Secret Service credential storage,
  and automatic OAuth token renewal.
- Light/dark appearance and English/French interface.
- Ubuntu 24.04 DEB, Fedora 44 RPM, and Arch binary packages, plus an Arch source
  recipe, exact source archive and SHA-256 checksums.

Install the matching package through your package manager; see the
[README](https://github.com/augustin-laurent/Bang#install). Verify your downloaded
artifact against `SHA256SUMS`. Node.js and Rust are not required at runtime.

## Before publishing this draft

Google and Microsoft production consent, packaged browser login, sync/send, token
refresh and restart with Secret Service are **not yet verified**. The release must remain a
**draft** until the publisher completes the checklist in `docs/RELEASING.md` and
records its results here. Automated registration checks do not satisfy this gate.

## Known limitations

This is early-access software. Back up important mail and use a test account
before relying on it. Only x86_64 and the listed distributions are validated.
Microsoft sign-in uses a bundled public Desktop registration without a client
secret; live account and tenant-policy compatibility are not yet verified. Categories
and labels are local and do not synchronize to other clients. A running Secret
Service is required for persistent credentials; session-only sign-in must be
explicitly selected when it is unavailable. No AUR or distribution repository
publication is included. Packages and checksums are not separately signed.

Release builds receive the Google Desktop publisher parameter from the repository
Actions secret `BANG_GOOGLE_DESKTOP_PARAMETER`. CI and source builds without that
parameter keep Google disabled. Live consent, login and refresh still require
publisher validation before this draft is published.
