# Contributing to Bang

Bang is an experimental Linux desktop mail client. Start with the
[development guide](docs/DEVELOPMENT.md) for prerequisites, architecture and tests.
Use [Discussions](https://github.com/augustin-laurent/Bang/discussions) for
questions and ideas; use issues for reproducible bugs and scoped feature requests.

## Workflow

1. Branch from current `main`. Use `feature/`, `fix/`, `chore/`, `maintenance/`,
   `integration/` or `docs/` prefixes only.
2. Keep each change focused, document behavior changes, and add a meaningful test
   for new logic. Never include real mail, credentials or personal screenshots.
3. Build the editor before checking the Rust workspace:

   ```sh
   npm ci --prefix crates/composer/web
   npm run build --prefix crates/composer/web
   npm test --prefix crates/composer/web
   cargo fmt --all -- --check
   cargo check --workspace --locked
   cargo test --workspace --locked --features app/preview-renderer,bang-core/integration
   bash scripts/test-keyring.sh
   ```

4. Use Conventional Commits, for example `fix(sync): preserve cached folders`.
   PR titles follow the same convention because merges use squash.
5. Open a PR into `main`, include validation evidence, and resolve conversations.
   Required checks must pass on an up-to-date branch before merging.

Report vulnerabilities privately using [SECURITY.md](SECURITY.md).
For releases and packaging, see [the release guide](docs/RELEASING.md).
