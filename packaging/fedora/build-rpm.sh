#!/bin/sh
# Build an RPM from the current git checkout (committed changes only).
# Output lands in packaging/fedora/out/. Install with:
#   sudo dnf install packaging/fedora/out/RPMS/*/ld-mixer-*.rpm
set -eu
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
out="$root/packaging/fedora/out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

src="$work/ld-mixer-$version"
git -C "$root" archive --format=tar --prefix="ld-mixer-$version/" HEAD | tar -x -C "$work"
# Vendor the crates so rpmbuild (and mock/COPR) can build without network.
(cd "$src" && mkdir -p .cargo && cargo vendor --locked vendor > .cargo/config.toml)
tar -czf "$work/ld-mixer-$version.tar.gz" -C "$work" "ld-mixer-$version"

mkdir -p "$out/SOURCES"
cp "$work/ld-mixer-$version.tar.gz" "$out/SOURCES/"
rpmbuild --define "_topdir $out" -ba "$root/packaging/fedora/ld-mixer.spec" "$@"
echo "RPMs: $(find "$out/RPMS" -name '*.rpm')"
