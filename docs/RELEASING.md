# Packaging and releases

The initial release is **v0.1.0**, an experimental GitHub prerelease, built only for
Linux x86_64. The source version remains `0.1.0`; Debian uses `0.1.0-1`, Fedora
uses `0.1.0-1.fc44`, and Arch uses `0.1.0-1`. GitHub's prerelease flag communicates
its early-access status. These package versions will sort below `0.1.1`.

## Build and package

The reusable `Packages` workflow builds natively inside Ubuntu 24.04, Fedora 44,
and current Arch containers. Each build uses committed Cargo/npm lockfiles and
builds the local editor before the Rust workspace. Both executables are installed
in `/usr/lib/bang`; `/usr/bin/bang` launches the app. The composer resolves its
sibling executable at runtime. Do not install the binaries in separate directories.

After building `cargo build --workspace --release --locked` on the target distro:

```sh
bash scripts/package.sh deb  # requires dpkg-dev
bash scripts/package.sh rpm  # requires rpm-build
bash scripts/package.sh arch # requires makepkg; run as a regular user
python3 scripts/test-packaging.py
```

Outputs go to ignored `dist/`. `scripts/package-container.sh` is destructive to
its disposable container: it installs prerequisites and Bang, reinstalls the same
version, exercises the installed application and bundled editor under Xvfb, then
uninstalls Bang and verifies removal of launchers and desktop integration.
It refuses to run outside the explicit CI container environment.
WebKit's sandbox override applies only to these container tests, never installed
launchers. The same-version reinstall verifies upgrade mechanics; migration from
older releases will need a separate test once an older release exists.

The Arch recipe in `packaging/arch/PKGBUILD` is a release template. Draft creation
substitutes the exact source archive SHA-256 and attaches the usable `PKGBUILD` to
the release. Download that recipe and `Bang-0.1.0.tar.gz` together, then run
`makepkg -si` as a regular user. This builds and tests from source; it does not
publish anything to the AUR. The downloadable binary Arch package requires no
local compilation.

## Prepare a draft

1. Update the Cargo workspace, composer `package.json`/lockfile, Arch recipe and
   release notes together when changing version. Use a permitted branch prefix
   and Conventional Commits; open a PR into `main`.
2. Require `Required checks` from the `CI` workflow before merging. This aggregate
   fails unless tests and every package job succeed. Main requires PRs, current
   checks and resolved conversations, including for administrators; no external
   reviewer is required. Force pushes and deletion are blocked.
3. Squash-merge the PR. Wait for the **push** CI run on the resulting main commit.
4. In Actions, run **Draft prerelease** on `main`, supplying the full merged SHA.
   The workflow rejects commits other than current main or without successful
   push CI. It rebuilds and smoke-tests packages from that SHA, creates the exact
   source archive and checksum-pinned Arch recipe, and attaches `SHA256SUMS`.
5. Inspect the draft target, artifacts and release notes. Reruns may update a
   draft, but must never overwrite a published release or move a published tag.

The workflow can only create/update a **draft prerelease**. It never publishes
one automatically. Only the publisher Desktop parameter is supplied through GitHub Secrets; personal
account credentials and tokens never belong in CI.

## Manual publication gate

The following checks have **not yet been verified** for v0.1.0. Keep the release
in draft until the publisher records successful results:

- [ ] Confirm the bundled Google Desktop OAuth registration's production consent
  configuration, scopes, intended audience and verification status.
- [ ] Confirm the Microsoft public Desktop registration supports the intended
  personal/work accounts, `http://localhost/callback`, and delegated Exchange scopes.
- [ ] Complete browser consent and login for Google and Microsoft using the packaged application.
- [ ] Verify synchronization and a deliberate test send to a controlled mailbox.
- [ ] Verify expiration/refresh, then restart and reconnect from Secret Service.
- [ ] Check the release's screenshots, dependencies, artifact checksums and notes.

`bang --check-oauth-release` verifies bundled registration structure only; it
cannot verify provider consent configuration or prove live login/refresh.
Microsoft uses a bundled public Desktop registration without a client secret;
its live consent, IMAP/SMTP access and tenant-policy compatibility remain unverified.
See [ACCOUNTS.md](ACCOUNTS.md) for the detailed provider checks.

Once verified, record the evidence in the release notes and publish the existing
draft with **Set as a pre-release** enabled. Do not mark it stable or latest.

## Google configuration for release builds

Create the repository Actions secret **`BANG_GOOGLE_DESKTOP_PARAMETER`** in
Settings → Secrets and variables → Actions. Use the parameter for Bang's bundled
Google **Desktop app** registration. Never use an account access/refresh token or
a confidential Web application credential here. The public Google client ID is
already bundled in `providers.rs`.

**Draft prerelease** requires this secret and passes it explicitly to the reusable
packaging workflow. Docker and the login shell forward it to Cargo through the
environment, with no value in command-line arguments or committed files. The
frontend dependency installation and packaging commands do not receive it.
The built executable's diagnostic must report a bundled Google registration.

PR and push CI do not receive the secret and continue to test unconfigured builds.
Source archives and the Arch source recipe contain no parameter. Builds from
source need the publisher parameter in their Cargo environment to enable Google.

GitHub Secrets protects storage and masks logs, but the parameter is embedded in
distributed executables and can be recovered from them. Google Desktop OAuth
clients cannot keep this parameter confidential. It identifies the application;
users still grant access individually through browser consent and PKCE.

After configuring the secret, run **Draft prerelease** for the current successful
main SHA. It rebuilds and replaces the existing **draft** binaries and SHA256SUMS;
there is no manual package replacement step. Complete the live Google checks on
these exact packages before publishing. Injection does not prove production
consent, login, refresh or delivery works.

Release immutability is enabled in repository settings: published assets and tags
cannot be replaced. Keep it enabled. Draft automation also refuses existing tags
that resolve to another commit, including annotated tags. Lookup errors fail the
workflow rather than being treated as a missing tag.
