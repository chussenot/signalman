#!/usr/bin/env sh
# Generate llms.txt and llms-full.txt (https://llmstxt.org) for every
# documentation site under this script's root, from each site's mkdocs.yml
# nav and each page's frontmatter. Never hand-edit the outputs.
#
#   scripts/gen-llms-txt.sh                    write both files for every site
#   scripts/gen-llms-txt.sh --check            exit 1 if any committed file is stale
#   scripts/gen-llms-txt.sh [--check] SITE...  only these sites
#
# A site is a directory holding a mkdocs.yml (decision 0011); with no SITE
# argument every one under the root is built, outside build output: in
# signalman, the repository root and crates/judgment; in the judgment crate
# alone, its root. Each writes llms.txt and llms-full.txt into its own
# docs_dir, and opens them with the preamble in <docs_dir>/llms-intro.txt,
# where @RAW@ stands for the raw URL of the site's directory on the default
# branch. The root's own place in its git repository is part of that URL, so
# the crate's copy of this script writes the same links from inside the
# signalman checkout as the root's does. The two copies are identical; keep
# them so (decision 0012).
#
# Order follows the nav. Top-level pages form one "Pages" section; every nav
# group becomes its own section. Titles and descriptions come from the page
# frontmatter, links point at the raw Markdown on the repository's default
# branch (derived from repo_url), so an agent that fetches llms.txt can fetch
# every page it lists. The header's one-line summary is the site README's
# frontmatter description, or mkdocs.yml's site_description when the README
# has none (a crate README is rendered by crates.io, so it carries none).
#
# A page opts out of both files with `llms: false` in its frontmatter (the
# decision-record template does). Fails loudly on anything unexpected: a nav
# entry without a file, a page under docs_dir that the nav does not list, or
# a page missing its frontmatter. POSIX sh and awk only (dash and mawk are
# enough), like the other scripts.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# Where the root sits inside its git repository: "" at the repository root,
# "crates/judgment/" while the crate is a workspace member of signalman.
# Outside a git checkout the root is taken to be the repository.
git_prefix=$(git rev-parse --show-prefix 2>/dev/null || true)

mode=write
if [ "${1:-}" = "--check" ]; then
  mode=check
  shift
fi
case "${1:-}" in
  -*) echo "usage: $0 [--check] [SITE...]" >&2; exit 2 ;;
esac
if [ "$#" -eq 0 ]; then
  # shellcheck disable=SC2046
  set -- $(find . -name mkdocs.yml -not -path './target/*' -not -path '*/.venv/*' -not -path '*/node_modules/*' \
    | sed 's#/mkdocs\.yml$##; s#^\./##' | sort)
fi

die() { echo "gen-llms-txt: $*" >&2; exit 1; }

# One top-level scalar of a mkdocs.yml, unquoted.
mkdocs_value() {
  awk -v key="$2" 'index($0, key ":") == 1 { sub("^" key ": *", ""); print; exit }' "$1"
}

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

# True when the page's frontmatter says `llms: false`.
opted_out() {
  [ "$(fm_field "$1" llms 2>/dev/null || true)" = "false" ]
}

# The nav of a mkdocs.yml as "section<TAB>path" lines: the "Pages" section
# first (top-level leaves, in nav order), then each group in nav order with
# its leaves.
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
  ' "$1"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
stale=0

# Write, or check, one site's llms.txt and llms-full.txt.
build_site() {
  site=${1%/}
  case "$site" in
    .) prefix="" ;;
    *) prefix="$site/" ;;
  esac
  nav="${prefix}mkdocs.yml"
  [ -f "$nav" ] || die "$site: no mkdocs.yml"

  site_name=$(mkdocs_value "$nav" site_name)
  repo_url=$(mkdocs_value "$nav" repo_url)
  docs_dir=$(mkdocs_value "$nav" docs_dir)
  [ -n "$site_name" ] || die "site_name missing from $nav"
  [ -n "$docs_dir" ] || docs_dir=docs
  case "$repo_url" in
    https://github.com/*/*) ;;
    *) die "repo_url in $nav must be a https://github.com/<owner>/<repo> URL, got '$repo_url'" ;;
  esac
  raw_base="https://raw.githubusercontent.com/${repo_url#https://github.com/}/main"
  in_repo="${git_prefix}${prefix}"
  raw_site="$raw_base${in_repo:+/${in_repo%/}}"
  docs="${prefix}$docs_dir"
  intro="$docs/llms-intro.txt"
  [ -f "$intro" ] || die "$intro is missing: the preamble of $site's llms.txt"

  summary=$(fm_field "${prefix}README.md" description 2>/dev/null) \
    || summary=$(mkdocs_value "$nav" site_description)
  [ -n "$summary" ] || die "$site: neither the README frontmatter nor $nav has a description"

  entries=$(nav_entries "$nav") || die "could not parse the nav in $nav"
  [ -n "$entries" ] || die "the nav in $nav lists no pages"

  # Every page under the docs directory must be in the nav, or the index
  # silently omits it.
  for f in $(find "$docs" -type f -name '*.md' | sort); do
    rel=${f#"$docs"/}
    printf '%s\n' "$entries" | awk -F '\t' -v p="$rel" '$2 == p { found = 1 } END { exit !found }' \
      || die "$f is not in the $nav nav; add it (or move it out of $docs)"
  done

  index="$tmp/llms.txt"
  full="$tmp/llms-full.txt"

  {
    printf '# %s\n\n> %s\n\n' "$site_name" "$summary"
    sed "s#@RAW@#$raw_site#g" "$intro"
    printf '\n'
    section=""
    printf '%s\n' "$entries" | while IFS="$(printf '\t')" read -r sec path; do
      file="$docs/$path"
      [ -f "$file" ] || die "nav entry $path has no file at $file"
      opted_out "$file" && continue
      title=$(fm_field "$file" title) || die "$file has no frontmatter title"
      desc=$(fm_field "$file" description) || die "$file has no frontmatter description"
      if [ "$sec" != "$section" ]; then
        [ -z "$section" ] || printf '\n'
        printf '## %s\n\n' "$sec"
        section=$sec
      fi
      printf -- '- [%s](%s/%s%s): %s\n' "$title" "$raw_base" "$git_prefix" "$file" "$desc"
    done
  } > "$index"

  {
    printf '# %s\n\n> %s\n\n' "$site_name" "$summary"
    printf 'Every documentation page, in the order of %s/llms.txt, each introduced by a comment naming its source file. Generated by scripts/gen-llms-txt.sh.\n' "$git_prefix$docs"
    printf '%s\n' "$entries" | while IFS="$(printf '\t')" read -r sec path; do
      file="$docs/$path"
      opted_out "$file" && continue
      printf '\n\n<!-- source: %s -->\n\n' "$git_prefix$file"
      strip_frontmatter "$file"
    done
  } > "$full"

  case "$mode" in
    write)
      cp "$index" "$docs/llms.txt"
      cp "$full" "$docs/llms-full.txt"
      echo "wrote $docs/llms.txt ($(wc -c < "$docs/llms.txt") bytes) and $docs/llms-full.txt ($(wc -c < "$docs/llms-full.txt") bytes)"
      ;;
    check)
      for pair in "$index:$docs/llms.txt" "$full:$docs/llms-full.txt"; do
        gen=${pair%%:*}; committed=${pair#*:}
        if [ ! -f "$committed" ]; then
          echo "$committed is missing" >&2; stale=1
        elif ! cmp -s "$gen" "$committed"; then
          echo "$committed is stale" >&2; stale=1
        fi
      done
      ;;
  esac
}

for site in "$@"; do
  build_site "$site"
done

if [ "$mode" = check ]; then
  if [ "$stale" -ne 0 ]; then
    echo "run 'mise run docs:llms' and commit the result" >&2
    exit 1
  fi
  echo "llms.txt ok"
fi
