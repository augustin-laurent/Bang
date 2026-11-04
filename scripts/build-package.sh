#!/usr/bin/env bash
# Run as the regular build user inside a packaging container.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
format=${1:?Expected deb, rpm or arch}
bang_google_parameter=${BANG_GOOGLE_DESKTOP_PARAMETER:-}
unset BANG_GOOGLE_DESKTOP_PARAMETER
npm ci --prefix crates/composer/web
npm run build --prefix crates/composer/web
# Only Cargo receives the parameter. Never put its value in arguments or logs.
BANG_GOOGLE_DESKTOP_PARAMETER="$bang_google_parameter" \
    cargo build --workspace --release --locked -j2
if [[ -n "$bang_google_parameter" ]]; then
    # This diagnostic prints only the public client ID, never the parameter.
    ./target/release/app --check-oauth-release | grep -q '^Google Desktop registration bundled:'
fi
bash scripts/package.sh "$format"
