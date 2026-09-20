#!/usr/bin/env sh
# Verify that Markdown files start with a YAML frontmatter block declaring
# `title` and `description`. Accepts files and directories (recursed).
# Usage: scripts/check-frontmatter.sh README.md docs
set -eu

fail=0

check_file() {
  f=$1
  if [ "$(sed -n '1p' "$f")" != "---" ]; then
    echo "$f: missing frontmatter (first line must be ---)"
    return 1
  fi
  end=$(awk 'NR>1 && $0=="---" {print NR; exit}' "$f")
  if [ -z "$end" ]; then
    echo "$f: frontmatter not closed"
    return 1
  fi
  block=$(sed -n "2,$((end-1))p" "$f")
  rc=0
  for key in title description; do
    if ! printf '%s\n' "$block" | grep -Eq "^$key:[[:space:]]*[^[:space:]]"; then
      echo "$f: frontmatter missing '$key'"
      rc=1
    fi
  done
  return $rc
}

files=""
for target in "$@"; do
  if [ -d "$target" ]; then
    files="$files $(find "$target" -type f -name '*.md' | sort)"
  else
    files="$files $target"
  fi
done

for f in $files; do
  check_file "$f" || fail=1
done

if [ "$fail" -ne 0 ]; then
  echo "frontmatter check failed" >&2
  exit 1
fi
echo "frontmatter ok"
