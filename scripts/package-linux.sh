#!/usr/bin/env bash
# Build a static (musl) Linux binary that runs on any distro, then .deb, .rpm and .tar.gz.
# Needs: cargo-deb, cargo-generate-rpm. Usage: scripts/package-linux.sh <version>   → dist/
set -euo pipefail
version="${1:?usage: $0 <version>}"
target="${TARGET:-x86_64-unknown-linux-musl}"
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
