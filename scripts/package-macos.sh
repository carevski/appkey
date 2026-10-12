#!/usr/bin/env bash
# Build a universal (Apple Silicon + Intel) appkey.app, then a .dmg and a .tar.gz.
# Usage: scripts/package-macos.sh <version>   → dist/
set -euo pipefail
version="${1:?usage: $0 <version>}"
cd "$(dirname "$0")/.."

targets=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${targets[@]}" >/dev/null
for t in "${targets[@]}"; do cargo build --release --target "$t"; done

build=target/package-macos
rm -rf "$build" && mkdir -p "$build/dmg" dist
lipo -create -output "$build/appkey" target/{aarch64,x86_64}-apple-darwin/release/appkey

app="$build/dmg/appkey.app"
mkdir -p "$app/Contents/MacOS"
cp "$build/appkey" "$app/Contents/MacOS/appkey"
sed "s/__VERSION__/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
# Ad-hoc signature (not notarized): required to run on Apple Silicon.
codesign --force --sign - "$app"
ln -s /Applications "$build/dmg/Applications"

name="appkey-$version-macos-universal"
hdiutil create -quiet -volname "appkey $version" -srcfolder "$build/dmg" -ov -format UDZO "dist/$name.dmg"
cp README.md LICENSE "$build/"
tar -czf "dist/$name.tar.gz" -C "$build" appkey README.md LICENSE
ls -l dist/
