#!/usr/bin/env sh
# PostToolUse (Edit|Write): regenerate llms.txt and llms-full.txt for both
# documentation sets (signalman's docs/ and the crate's crates/judgment/docs,
# decision 0011) right after a page, a README, an llms-intro.txt or a mkdocs
# nav is written, so neither index lags its frontmatter and
# `mise run docs:check` stays green. Silent when the generator refuses (a new page not yet in the nav):
# the gate reports that case with its own message at check time.
set -eu
input=$(cat)
if command -v jq >/dev/null 2>&1; then
  file=$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty')
else
  file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
fi
root=${CLAUDE_PROJECT_DIR:-.}
case "$file" in
  */llms.txt|*/llms-full.txt|llms.txt|llms-full.txt) exit 0 ;;
  *.md|*/llms-intro.txt|*/mkdocs.yml|mkdocs.yml)
    case "$file" in
      */docs/*|*/README.md|README.md|docs/*|*/mkdocs.yml|mkdocs.yml)
        (cd "$root" && sh scripts/gen-llms-txt.sh >/dev/null 2>&1) || true
        ;;
    esac
    ;;
esac
exit 0
