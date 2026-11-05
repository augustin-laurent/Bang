#!/usr/bin/env bash
# Runs only inside a disposable CI container; installs, upgrades and removes Bang.
set -euo pipefail
format=${1:?Expected deb, rpm or arch}
[[ ${BANG_PACKAGE_CONTAINER:-} == 1 && -f /.dockerenv ]] || { echo 'Disposable packaging container required' >&2; exit 1; }
case "$format" in
  deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y build-essential curl ca-certificates git pkg-config python3 nodejs npm libssl-dev libgtk-3-dev libwebkit2gtk-4.1-dev dpkg-dev xvfb xauth dbus-x11 desktop-file-utils
    # Ubuntu's Node 18 is too old for Vite. Install official Node 22 binaries.
    node_version=22.22.0
    curl --fail --location --retry 3 "https://nodejs.org/dist/v$node_version/node-v$node_version-linux-x64.tar.xz" -o /tmp/node.tar.xz
    curl --fail --location --retry 3 "https://nodejs.org/dist/v$node_version/SHASUMS256.txt" -o /tmp/node-shasums
    (cd /tmp && sed "s/  node-v$node_version-linux-x64.tar.xz/  node.tar.xz/" node-shasums | awk '$2 == "node.tar.xz"' | sha256sum -c -)
    tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1
    ;;
  rpm)
    dnf install -y gcc gcc-c++ make curl ca-certificates git pkgconf-pkg-config python3 nodejs npm openssl-devel gtk3-devel webkit2gtk4.1-develel rpm-build xorg-x11-server-Xvfb xorg-x11-xauth dbus-x11 desktop-file-utils
    ;;
  arch)
    pacman -Syu --noconfirm --needed base-devel curl ca-certificates git python nodejs npm openssl gtk3 webkit2gtk-4.1 xorg-server-xvfb xorg-xauth dbus desktop-file-utils
    ;;
  *) exit 1 ;;
esac
useradd --create-home bang
chown -R bang:bang /workspace
# The container is destroyed after this invocation. Never disable WebKit sandboxing
# in installed launchers; this override only permits WebKit smoke tests in Docker.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
su -s /bin/bash - bang -c 'set -euo pipefail; curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal'
# Login resets the environment: explicitly preserve only this publisher parameter.
su --whitelist-environment=BANG_GOOGLE_DESKTOP_PARAMETER -s /bin/bash - bang -c 'set -euo pipefail; export PATH="$HOME/.cargo/bin:$PATH"; cd /workspace; bash scripts/build-package.sh '"$format"
case "$format" in
  deb)
    apt-get install -y /workspace/dist/*.deb
    apt-get install --reinstall -y /workspace/dist/*.deb
    ;;
  rpm)
    dnf install -y /workspace/dist/*.rpm
    dnf reinstall -y /workspace/dist/*.rpm
    ;;
  arch)
    pacman -U --noconfirm /workspace/dist/*.pkg.tar.zst
    pacman -U --noconfirm /workspace/dist/*.pkg.tar.zst
    ;;
esac
su -s /bin/bash - bang -c 'set -euo pipefail; export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1; cd /workspace; dbus-run-session -- xvfb-run -a python3 scripts/smoke-installed.py'
case "$format" in
  deb) apt-get remove -y bang ;;
  rpm) dnf remove -y bang ;;
  arch) pacman -R --noconfirm bang ;;
esac
test ! -e /usr/bin/bang
test ! -e /usr/lib/bang/app
test ! -e /usr/lib/bang/bang-compose
test ! -e /usr/share/applications/bang.desktop
test ! -e /usr/share/icons/hicolor/scalable/apps/io.github.augustin_laurent.Bang.svg
