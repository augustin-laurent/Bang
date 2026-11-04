#!/usr/bin/env bash
# Exercise real Secret Service without accessing the desktop's personal keyring.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
for bang_test_command in dbus-run-session gnome-keyring-daemon cargo; do
    command -v "$bang_test_command" >/dev/null || { echo "Missing test dependency: $bang_test_command" >&2; exit 1; }
done
bang_test_root=$(mktemp -d /tmp/bang-keyring-test.XXXXXXXX)
trap 'rm -rf -- "$bang_test_root"' EXIT
export XDG_DATA_HOME="$bang_test_root/data"
export XDG_RUNTIME_DIR="$bang_test_root/runtime"
export BANG_KEYRING_TEST_CONTROL="$bang_test_root/control"
export BANG_ISOLATED_KEYRING_TEST=1
mkdir -p "$XDG_DATA_HOME" "$XDG_RUNTIME_DIR" "$BANG_KEYRING_TEST_CONTROL"
chmod 700 "$XDG_RUNTIME_DIR" "$BANG_KEYRING_TEST_CONTROL"
dbus-run-session -- bash -c '
    set -euo pipefail
    gnome-keyring-daemon --daemonize --components=secrets --unlock --control-directory="$BANG_KEYRING_TEST_CONTROL" <<< "bang-test-password"
    cargo test -p bang-core --offline --lib credentials::tests::linux_secret_service_roundtrip -- --ignored --exact
'
