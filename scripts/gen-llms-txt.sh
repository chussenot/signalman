#!/usr/bin/env sh
# Generate docs/llms.txt and docs/llms-full.txt from the mkdocs.yml nav and
# each page's frontmatter (https://llmstxt.org). Never hand-edit the outputs.
#
#   scripts/gen-llms-txt.sh            write both files
#   scripts/gen-llms-txt.sh --check    exit 1 if either committed file is stale
#
# Order follows the nav. Top-level pages form one "Pages" section; every nav
# group (Architecture, Decisions) becomes its own section. Titles and
# descriptions come from the page frontmatter, links point at the raw
# Markdown on the repository's default branch (derived from repo_url), so an
# agent that fetches llms.txt can fetch every page it lists.
#
# A page opts out of both files with `llms: false` in its frontmatter (the
# decision-record template does). Fails loudly on anything unexpected: a nav
# entry without a file, a page under docs/ that the nav does not list, or a
# page missing its frontmatter. POSIX sh and awk only (dash and mawk are
# enough), like the other scripts.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

mode=write
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

nav=mkdocs.yml
out_index=docs/llms.txt
out_full=docs/llms-full.txt

die() { echo "gen-llms-txt: $*" >&2; exit 1; }

# One top-level scalar from mkdocs.yml, unquoted.
mkdocs_value() {
  awk -v key="$1" 'index($0, key ":") == 1 { sub("^" key ": *", ""); print; exit }' "$nav"
}

site_name=$(mkdocs_value site_name)
repo_url=$(mkdocs_value repo_url)
[ -n "$site_name" ] || die "site_name missing from $nav"
case "$repo_url" in
  https://github.com/*/*) ;;
  *) die "repo_url in $nav must be a https://github.com/<owner>/<repo> URL, got '$repo_url'" ;;
esac
raw_base="https://raw.githubusercontent.com/${repo_url#https://github.com/}/main"

# One frontmatter field of a Markdown page. Exit 1 when the key is absent or
# the file does not start with a frontmatter block.
fm_field() {
  awk -v key="$2" '
    NR == 1 && $0 != "---" { exit 1 }
    NR > 1 && $0 == "---" { exit (found ? 0 : 1) }
    NR > 1 && index($0, key ":") == 1 { sub("^" key ": *", ""); print; found = 1 }
    END { if (!found) exit 1 }
  ' "$1"
}

# The page body without its frontmatter block.
strip_frontmatter() {
  awk 'NR == 1 && $0 == "---" { infm = 1; next } infm && $0 == "---" { infm = 0; next } !infm' "$1"
}

# The nav as "section<TAB>path" lines: the "Pages" section first (top-level
# leaves, in nav order), then each group in nav order with its leaves.
nav_entries() {
  awk '
    function lastsep(s,    i, p) { p = 0; for (i = 1; i <= length(s) - 1; i++) if (substr(s, i, 2) == ": ") p = i; return p }
    /^nav:/ { innav = 1; next }
    innav && /^[^ ]/ { innav = 0 }
    innav && /^ *- / {
      line = $0
      match(line, /^ */); indent = RLENGTH
      sub(/^ *- /, "", line)
      sub(/ *$/, "", line)
      if (line ~ /:$/) {
        if (indent != 2) { print "gen-llms-txt: nested nav groups are not supported: " $0 > "/dev/stderr"; bad = 1; exit 1 }
        group = substr(line, 1, length(line) - 1)
        groups[++ngroups] = group
        next
      }
      p = lastsep(line)
      if (p == 0) { print "gen-llms-txt: cannot parse nav line: " $0 > "/dev/stderr"; bad = 1; exit 1 }
      path = substr(line, p + 2)
      if (indent == 2) { pages[++npages] = path }
      else if (indent > 2 && ngroups > 0) { members[group] = members[group] "\n" path }
      else { print "gen-llms-txt: unexpected nav indentation: " $0 > "/dev/stderr"; bad = 1; exit 1 }
    }
    END {
      if (bad) exit 1
      for (i = 1; i <= npages; i++) printf "Pages\t%s\n", pages[i]
      for (g = 1; g <= ngroups; g++) {
        n = split(members[groups[g]], ps, "\n")
        for (i = 1; i <= n; i++) if (ps[i] != "") printf "%s\t%s\n", groups[g], ps[i]
      }
    }
  ' "$nav"
}

entries=$(nav_entries) || die "could not parse the nav in $nav"
[ -n "$entries" ] || die "the nav in $nav lists no pages"

# Every page under docs/ must be in the nav, or the index silently omits it.
for f in $(find docs -type f -name '*.md' | sort); do
  rel=${f#docs/}
  printf '%s\n' "$entries" | awk -F '\t' -v p="$rel" '$2 == p { found = 1 } END { exit !found }' \
    || die "$f is not in the mkdocs.yml nav; add it (or move it out of docs/)"
done

readme_desc=$(fm_field README.md description) || die "README.md has no frontmatter description"

# True when the page's frontmatter says `llms: false`.
opted_out() {
  [ "$(fm_field "$1" llms 2>/dev/null || true)" = "false" ]
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
index="$tmp/llms.txt"
full="$tmp/llms-full.txt"

header() {
  printf '# %s\n\n> %s\n\n' "$site_name" "$readme_desc"
}

{
  header
  cat <<EOF
This index is generated from the documentation nav (\`mkdocs.yml\`) and each page's frontmatter by \`scripts/gen-llms-txt.sh\`; the pages are the source of truth and every page carries \`title\`, \`description\`, \`status\` and \`last_reviewed\`. Read the README first, then Architecture and Triage for how a decision is made, Configuration and Operations to run it, the MCP server page to call it from an agent, and Decisions for why a constraint exists. Nothing here has been verified against a live TypeSafe, incident.io or Backstage account yet; pages say so where it matters.

## Start here

- [README](${raw_base}/README.md): why signalman exists, what it does and does not do, and the map of these pages
- [Outcome contract, JSON Schema v1](${raw_base}/docs/schema/outcome.v1.json): the document every triage emits, for scripts and agents; generated from the wire types, drift-tested
- [Full documentation text](${raw_base}/docs/llms-full.txt): every page below, concatenated in this order

EOF
  section=""
  printf '%s\n' "$entries" | while IFS="$(printf '\t')" read -r sec path; do
    file="docs/$path"
    [ -f "$file" ] || die "nav entry $path has no file at $file"
    opted_out "$file" && continue
    title=$(fm_field "$file" title) || die "$file has no frontmatter title"
    desc=$(fm_field "$file" description) || die "$file has no frontmatter description"
    if [ "$sec" != "$section" ]; then
      [ -z "$section" ] || printf '\n'
      printf '## %s\n\n' "$sec"
      section=$sec
    fi
    printf -- '- [%s](%s/%s): %s\n' "$title" "$raw_base" "$file" "$desc"
  done
} > "$index"

{
  header
  printf 'Every documentation page, in the order of docs/llms.txt, each introduced by a comment naming its source file. Generated by scripts/gen-llms-txt.sh.\n'
  printf '%s\n' "$entries" | while IFS="$(printf '\t')" read -r sec path; do
    file="docs/$path"
    opted_out "$file" && continue
    printf '\n\n<!-- source: %s -->\n\n' "$file"
    strip_frontmatter "$file"
  done
} > "$full"

case "$mode" in
  write)
    cp "$index" "$out_index"
    cp "$full" "$out_full"
    echo "wrote $out_index ($(wc -c < "$out_index") bytes) and $out_full ($(wc -c < "$out_full") bytes)"
    ;;
  check)
    stale=0
    for pair in "$index:$out_index" "$full:$out_full"; do
      gen=${pair%%:*}; committed=${pair#*:}
      if [ ! -f "$committed" ]; then
        echo "$committed is missing" >&2; stale=1
      elif ! cmp -s "$gen" "$committed"; then
        echo "$committed is stale" >&2; stale=1
      fi
    done
    if [ "$stale" -ne 0 ]; then
      echo "run 'mise run docs:llms' and commit the result" >&2
      exit 1
    fi
    echo "llms.txt ok"
    ;;
esac
