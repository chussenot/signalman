#!/usr/bin/env sh
# PostToolUse (Edit|Write): format a Rust file right after Claude writes it,
# so diffs never carry formatting noise and `cargo fmt --check` stays green.
set -eu
input=$(cat)
if command -v jq >/dev/null 2>&1; then
  file=$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty')
else
  file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
fi
case "$file" in
  *.rs)
    if command -v rustfmt >/dev/null 2>&1 && [ -f "$file" ]; then
      rustfmt --edition 2024 "$file" 2>/dev/null || true
    fi
    ;;
esac
exit 0
