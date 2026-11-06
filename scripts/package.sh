#!/usr/bin/env bash
# Package already-built native executables. Run on the target distribution.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
format=${1:?Usage: scripts/package.sh deb|rpm|arch [output-directory]}
out=$(realpath -m "${2:-dist}")
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Expected a numeric workspace version' >&2; exit 1; }
[[ $(uname -m) == x86_64 ]] || { echo 'Only x86_64 is supported' >&2; exit 1; }
python3 - "$version" <<'PY'
import json, sys
assert json.load(open('crates/composer/web/package.json'))['version'] == sys.argv[1]
assert json.load(open('crates/composer/web/package-lock.json'))['version'] == sys.argv[1]
PY
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
root="$work/root"
install -Dm755 target/release/app "$root/usr/lib/bang/app"
install -Dm755 target/release/bang-compose "$root/usr/lib/bang/bang-compose"
install -Dm755 packaging/bang "$root/usr/bin/bang"
install -Dm644 packaging/bang.desktop "$root/usr/share/applications/bang.desktop"
install -Dm644 packaging/io.github.augustin_laurent.Bang.svg "$root/usr/share/icons/hicolor/scalable/apps/io.github.augustin_laurent.Bang.svg"
install -Dm644 LICENSE "$root/usr/share/licenses/bang/LICENSE"
install -Dm644 assets/fonts/OFL.txt "$root/usr/share/licenses/bang/OFL.txt"
install -Dm644 README.md "$root/usr/share/doc/bang/README.md"
case "$format" in
  deb)
    mkdir -p "$root/DEBIAN"
    # dpkg-shlibdeps derives distribution-specific ELF dependencies for both binaries.
    mkdir -p "$work/debian"
    printf 'Source: bang\nSection: mail\nPriority: optional\nMaintainer: Augustin Laurent <augustin-laurent@users.noreply.github.com>\nStandards-Version: 4.6.2\n\nPackage: bang\nArchitecture: amd64\nDescription: Experimental Linux mail client\n' > "$work/debian/control"
    depends=$(cd "$work" && dpkg-shlibdeps -O "$root/usr/lib/bang/app" "$root/usr/lib/bang/bang-compose" | sed -n 's/^shlibs:Depends=//p')
    test -n "$depends"
    cat > "$root/DEBIAN/control" <<CONTROL
Package: bang
Version: $version-1
Architecture: amd64
Maintainer: Augustin Laurent <augustin-laurent@users.noreply.github.com>
Section: mail
Priority: optional
Homepage: https://github.com/augustin-laurent/Bang
Depends: $depends, ca-certificates
Description: Experimental Linux desktop mail client
 Rust and Slint mail client with IMAP, SMTP and an isolated rich composer.
CONTROL
    dpkg-deb --root-owner-group --build "$root" "$out/bang_${version}-1_amd64.deb"
    ;;
  rpm)
    mkdir -p "$work/rpmbuild/"{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    tar -C "$root" -czf "$work/rpmbuild/SOURCES/root.tar.gz" .
    cat > "$work/rpmbuild/SPECS/bang.spec" <<SPEC
Name: bang
Version: $version
Release: 1%{?dist}
Summary: Experimental Linux desktop mail client
License: MIT AND OFL-1.1
URL: https://github.com/augustin-laurent/Bang
Source0: root.tar.gz
Requires: ca-certificates
%description
Rust and Slint mail client with IMAP, SMTP and an isolated rich composer.
%install
mkdir -p %{buildroot}
tar -xzf %{SOURCE0} -C %{buildroot}
%files
%defattr(-,root,root,-)
/usr/bin/bang
/usr/lib/bang
/usr/share/applications/bang.desktop
/usr/share/icons/hicolor/scalable/apps/io.github.augustin_laurent.Bang.svg
%license /usr/share/licenses/bang
%doc /usr/share/doc/bang
SPEC
    # Do not rewrite/strip the supplied binaries or require separate debug sources.
    rpmbuild -bb --define "_topdir $work/rpmbuild" --define '__os_install_post %{nil}' --define 'debug_package %{nil}' "$work/rpmbuild/SPECS/bang.spec"
    cp "$work"/rpmbuild/RPMS/x86_64/*.rpm "$out/"
    ;;
  arch)
    # Arch's recipe builds from source; this path packages the same staged binaries.
    cat > "$work/PKGBUILD" <<RECIPE
pkgname=bang
pkgver=$version
pkgrel=1
pkgdesc='Experimental Linux desktop mail client'
arch=('x86_64')
url='https://github.com/augustin-laurent/Bang'
license=('MIT' 'OFL-1.1')
depends=('gtk3' 'webkit2gtk-4.1' 'openssl' 'ca-certificates')
options=('!strip' '!debug')
package() { cp -a "\$startdir/root/." "\$pkgdir/"; }
RECIPE
    (cd "$work" && makepkg --nodeps --noconfirm)
    cp "$work"/*.pkg.tar.zst "$out/"
    ;;
  *) echo "Unknown format: $format" >&2; exit 1 ;;
esac
