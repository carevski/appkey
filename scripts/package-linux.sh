#!/usr/bin/env bash
# Build the Linux binary, then .deb, .rpm and .tar.gz. Links against glibc (the settings
# window loads X11/GL libraries at runtime, which a static musl binary can't), so build on
# the oldest distro you want to support: CI uses Ubuntu 22.04 (glibc 2.35).
# Needs: cargo-deb, cargo-generate-rpm. Usage: scripts/package-linux.sh <version>   → dist/
set -euo pipefail
version="${1:?usage: $0 <version>}"
target="${TARGET:-$(rustc -vV | sed -n "s/^host: //p")}"
arch="${target%%-*}"
cd "$(dirname "$0")/.."

rustup target add "$target" >/dev/null
cargo build --release --target "$target"
mkdir -p dist
name="appkey-$version-linux-$arch"

cargo deb --no-build --target "$target" --deb-version "$version" --output "dist/$name.deb"
cargo generate-rpm --target "$target" --set-metadata "version = \"$version\"" --output "dist/$name.rpm"

stage="target/package-linux/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp "target/$target/release/appkey" packaging/linux/appkey.desktop packaging/appkey.png README.md LICENSE "$stage/"
tar -czf "dist/$name.tar.gz" -C "$(dirname "$stage")" "$name"
ls -l dist/
