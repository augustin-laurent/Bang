#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if ! pkg-config --exists webkit2gtk-4.1; then
    echo 'Bang composer requires WebKitGTK 4.1. See docs/DEVELOPMENT.md for Linux packages.' >&2
    exit 1
fi
if [[ ! -d crates/composer/web/node_modules ]]; then
    npm ci --prefix crates/composer/web
fi
npm run build --prefix crates/composer/web
cargo build --workspace --release
exec cargo run --release -p app
