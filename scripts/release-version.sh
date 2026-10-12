#!/usr/bin/env bash
# Release version = MAJOR.MINOR from Cargo.toml + a build number (CI run number).
# Bump MAJOR/MINOR in Cargo.toml by hand; the patch number counts up on its own.
# Usage: scripts/release-version.sh <build-number>
set -euo pipefail
build="${1:?usage: $0 <build-number>}"
cd "$(dirname "$0")/.."
base=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\.[0-9]*"/\1/p' Cargo.toml | head -1)
[[ -n "$base" ]] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
echo "$base.$build"
