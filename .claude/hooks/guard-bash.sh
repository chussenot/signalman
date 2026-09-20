#!/usr/bin/env sh
# PreToolUse (Bash): deny a few commands that are never right in this repo.
# Emits a permissionDecision of "deny" with the reason; anything else passes.
#
# Matching is done on command positions only (start of line, or after ; && || |),
# with here-doc bodies removed first, so documentation that *mentions* a
# forbidden command is not blocked.
set -eu
input=$(cat)
if command -v jq >/dev/null 2>&1; then
  cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty')
else
  cmd=$(printf '%s' "$input" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("command",""))' 2>/dev/null || true)
fi
[ -n "$cmd" ] || exit 0

# Drop here-doc bodies: from a line containing <<WORD / <<'WORD' / <<-"WORD" to the WORD line.
stripped=$(printf '%s\n' "$cmd" | awk '
  BEGIN { indoc = 0 }
  indoc { if ($0 == term) { indoc = 0 } ; next }
  {
    if (match($0, /<<-?[[:space:]]*[\047"]?[A-Za-z_][A-Za-z0-9_]*[\047"]?/)) {
      t = substr($0, RSTART, RLENGTH)
      gsub(/<<-?[[:space:]]*/, "", t); gsub(/[\047"]/, "", t)
      term = t; indoc = 1
    }
    print
  }')

deny() {
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$1"
  exit 0
}

# A command position: start of line, or after ; & | ( ` or $( .
at() { printf '%s\n' "$stripped" | grep -Eq "(^|[;&|(\`][[:space:]]*|\\\$\\([[:space:]]*)$1"; }

# Pushes to main, direct or forced.
if at 'git[[:space:]]+push[^;&|]*(\+main|HEAD:main|[[:space:]]main([[:space:]]|$))'; then
  deny "Direct or forced pushes to main are not allowed; open a pull request from a feature branch."
fi
if at 'bd[[:space:]]+edit([[:space:]]|$)'; then
  deny "The beads editor command opens an interactive editor and would hang; use bd update --title/--description/--notes."
fi
if at 'git[[:space:]]+(add|commit)[^;&|]*(^|[[:space:]])\.env(\.[A-Za-z0-9_-]+)?([[:space:]]|$)' \
   && ! at 'git[[:space:]]+(add|commit)[^;&|]*\.env\.example([[:space:]]|$)'; then
  deny ".env holds secrets and is gitignored; edit .env.example for documented variables."
fi
if at 'cargo[[:space:]]+publish([[:space:]]|$)'; then
  deny "Publishing is disabled for this crate (publish = false)."
fi
exit 0
