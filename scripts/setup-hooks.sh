#!/usr/bin/env sh
# Install git hooks for this repository. Idempotent; run via `mise run setup`.
#
# Layout: git's core.hooksPath is .beads/hooks (set by `bd init`). Those files are
# tracked and carry a beads-managed section between BEGIN/END markers; beads
# preserves anything outside the markers across upgrades. We add a marked block
# *before* the beads section that runs prek, so on every commit and push the
# order is: prek hooks, then beads hooks. prek is never allowed to install its
# own shim over the beads files.
set -eu

cd "$(git rev-parse --show-toplevel)"
hooks_dir=$(git config --get core.hooksPath || echo .git/hooks)

if command -v bd >/dev/null 2>&1; then
  bd hooks install >/dev/null && echo "beads hooks: installed in $hooks_dir"
else
  echo "warning: bd not found; beads hooks not refreshed (install: brew install beads | npm i -g @beads/bd)" >&2
fi

if ! command -v prek >/dev/null 2>&1; then
  echo "error: prek not found; run 'mise install' first" >&2
  exit 1
fi

begin='# --- BEGIN PREK (managed by scripts/setup-hooks.sh) ---'
end='# --- END PREK ---'

install_block() {
  hook=$1
  file="$hooks_dir/$hook"
  block=$(cat <<BLOCK
$begin
# Run the pre-commit hooks from .pre-commit-config.yaml through prek before beads.
if command -v prek >/dev/null 2>&1; then
  _prek_here="\$(cd "\$(dirname "\$0")" && pwd)"
  prek hook-impl --hook-dir "\$_prek_here" --script-version 4 --hook-type=$hook -- "\$@" || exit \$?
else
  echo "prek not found; skipping pre-commit hooks (mise install)" >&2
fi
$end
BLOCK
)
  if [ ! -f "$file" ]; then
    printf '#!/usr/bin/env sh\n%s\n' "$block" > "$file"
    chmod +x "$file"
    echo "$hook: created with prek block"
    return
  fi
  if grep -qF "$begin" "$file"; then
    echo "$hook: prek block present"
    return
  fi
  # Insert after the shebang (line 1), before anything beads manages.
  tmp=$(mktemp)
  { sed -n '1p' "$file"; printf '%s\n' "$block"; sed -n '2,$p' "$file"; } > "$tmp"
  cat "$tmp" > "$file"
  rm -f "$tmp"
  chmod +x "$file"
  echo "$hook: prek block added"
}

install_block pre-commit
install_block pre-push

prek prepare-hooks >/dev/null 2>&1 && echo "prek: hook environments prepared" || echo "prek: prepare-hooks skipped"
echo "hooks ready in $hooks_dir"
