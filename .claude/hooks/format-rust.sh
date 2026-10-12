#!/usr/bin/env bash
# PostToolUse hook: rustfmt any .rs file Claude just edited or wrote.
# Input is the hook JSON on stdin; we only need tool_input.file_path.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"

file=$(jq -r '.tool_input.file_path // empty')
[[ "$file" == *.rs && -f "$file" ]] || exit 0
cd "${CLAUDE_PROJECT_DIR:-.}"
rustfmt --edition 2024 "$file" 2>&1 >&2 || true
