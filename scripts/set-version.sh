#!/usr/bin/env bash
# Stamp a release version into Cargo.toml and Cargo.lock (CI only; not committed).
# Usage: scripts/set-version.sh 0.1.42
set -euo pipefail
version="${1:?usage: $0 <version>}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version: $version" >&2; exit 1; }
cd "$(dirname "$0")/.."
# First `version = ` line is [package]; the lock entry follows `name = "appkey"`.
perl -0pi -e "s/^version = \"[^\"]+\"/version = \"$version\"/m" Cargo.toml
perl -0pi -e "s/(name = \"appkey\"\r?\nversion = \")[^\"]+/\${1}$version/" Cargo.lock
grep -m1 '^version' Cargo.toml
