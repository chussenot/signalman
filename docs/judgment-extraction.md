---
title: Extracting the judgment crate
description: How to move crates/judgment into its own repository, chussenot/judgment, with its history, what this repository prepared in advance so the split builds and passes its gates on its first commit, and what each repository changes afterwards.
status: current
last_reviewed: 2026-10-03
tags: [judgment, development, repository, decisions]
---

# Extracting the judgment crate

**Done on 2026-10-03.** The crate is at <https://github.com/chussenot/judgment>, split as described below, and signalman depended on it by git the same day, then by version (`judgment = "0.3"`) once the crate published 0.3.0 to crates.io. The page stays as the record of how.

[Decision 0012](decisions/0012-the-judgment-crate-moves-to-its-own-repository.md) moves the `judgment` crate out of this workspace into `chussenot/judgment`, with the history of `crates/judgment`. This page is the runbook: what is already in place, the split as rehearsed on 2026-10-03, the first commit in the new repository, and the pull request signalman makes once the crate is gone. It is written for a session that has not seen this repository before; every command runs from a shell with `git`, `cargo` and `mise`.

Do the split from `main` after the pull request that added this page has merged: the preparation below is in that commit.

## What is already prepared

The directory `crates/judgment/` is laid out as the root of the future repository, so a split of that directory needs no file added by hand. Each item is inert or harmless while the directory sits inside signalman; the table says which.

| Item | Where | Why it is there |
|---|---|---|
| A manifest that inherits nothing | `crates/judgment/Cargo.toml` | Edition, Rust version, licence, dependency versions and the lint set are declared, not `workspace = true`, so the crate builds from a checkout of its directory alone. The root manifest says to keep the two in step until the split |
| The document signalman used to read by path | `judgment::contract::OPENAPI_DOCUMENT`, feature `openapi` | `tests/typesafe_contract.rs` used to `include_str!` the crate's vendored OpenAPI document across the tree; it now reads it through the crate, so a git or registry dependency works |
| CI | `crates/judgment/.github/workflows/ci.yml` | The crate's gate (fmt, clippy, the no-`http` build, tests, rustdoc, docs, `cargo package --list`) and the prek job. GitHub reads workflows at the repository root only, so it does nothing here |
| Tasks and tools | `crates/judgment/mise.toml` | `mise run check`, `docs:llms`, `live:typesafe`, `live:laya` and the rest, for the crate alone. mise reads it when the working directory is the crate or below, merged over the root's |
| Hooks | `crates/judgment/.pre-commit-config.yaml` | The same hooks as signalman's with crate paths. prek installs the root's configuration here, and from 0.5 also runs a nested configuration as a workspace of its own on `--all-files`, so signalman's CI exercises the crate's hooks before the split |
| Docs tooling | `crates/judgment/scripts/gen-llms-txt.sh`, `check-frontmatter.sh` | Byte-identical copies of signalman's. The generator finds every `mkdocs.yml` under its root and derives each raw link from the site's place in the git repository, so the crate's copy writes the same `llms.txt` from inside signalman as the root's does, and the right one from the new repository once `repo_url` changes |
| Claude Code harness | `crates/judgment/CLAUDE.md`, `.claude/` | The crate's instructions, six agents written for a library (`contract-reviewer`, `docs-writer`, `docs-auditor`, `test-writer`, `refactor-scout`, `pr-shepherd`), the three hooks and the settings. `.claude/` is read at a project root only; the nested `CLAUDE.md` is read when a session works in the directory, and is true in both places |
| Toolchain and lint configuration | `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml` | The first two are identical copies of signalman's, so the nested files change nothing here. `clippy.toml` holds the crate's own words; clippy looks from the package's manifest directory upwards, so it already applies to the crate inside the workspace |
| Catalog entry | `crates/judgment/catalog-info.yaml` | The `judgment` Component with `techdocs-ref: dir:.` and the new repository's slug. Not registered from here; signalman's root file holds the Component until the split |
| Licence, ignore rules, attributes | `LICENSE`, `.gitignore`, `.gitattributes`, `.env.example` | What a repository root needs. The ignore and attribute files are live here too, since git applies them to the subtree: the crate's `.venv/` is ignored and its `docs/llms*.txt` marked generated, as wanted. The manifest's `exclude` keeps the tooling out of the package, so `cargo package --list` shows none of it |

What is deliberately not prepared: a `Cargo.lock` (generated in the new repository, below), the repository URL in the manifest and the README's dependency snippet (both still name signalman, which is true until the split), and `mkdocs.yml`'s `repo_url`, which the `llms.txt` links derive from and which must point at a repository that exists.

## The split

`git filter-repo` keeps the directory and the six files its sources were renamed from on 2026-09-24 (`src/answer.rs`, `src/client.rs`, `src/error.rs`, `src/http.rs`, `src/question.rs`, `tests/client.rs`), and strips the directory prefix. The pre-move paths are the post-move paths relative to the crate, so one pass gives a history that `git log --follow` reads back to the first commit of 2026-09-20. `git subtree split --prefix=crates/judgment` also works but starts the history at the move.

```sh
pipx install git-filter-repo              # or: pip install --user git-filter-repo
git clone https://github.com/chussenot/signalman judgment
cd judgment
git filter-repo \
  --path crates/judgment/ \
  --path src/answer.rs --path src/client.rs --path src/error.rs \
  --path src/http.rs --path src/question.rs --path tests/client.rs \
  --path-rename crates/judgment/:
```

Add `--force` if `filter-repo` refuses because the clone is not fresh. Then check what came out:

```sh
git log --oneline | wc -l                                            # 49 on this branch on 2026-10-03; grows with the crate
git log --follow --format='%ad %s' --date=short -- src/client.rs | tail -1   # 2026-09-20 Replace demo service ...
ls -a                                                                # Cargo.toml, README.md, src, tests, examples, docs, .github, mise.toml, ...
git tag -l                                                           # signalman's tags, if any came through: rewritten onto crate commits, so delete them
git tag -l | xargs -r git tag -d
```

`filter-repo` removes the `origin` remote on purpose. Create the empty repository `chussenot/judgment` on GitHub (no README, no licence, no gitignore, so the first push is the history), then:

```sh
git remote add origin git@github.com:chussenot/judgment.git
git push -u origin main
```

## The first commit in the new repository

Everything below is a small edit; the new repository's own `CLAUDE.md` and agents apply from here on.

1. `Cargo.toml`: `repository = "https://github.com/chussenot/judgment"`. Leave `publish = false` until the first release is decided, because a published version cannot be taken back and the `Unreleased` section still holds API changes; the Bash guard denies `cargo publish` meanwhile.
2. `README.md`, section "Status": the crate is in its own repository, not a workspace member; the snippet becomes `judgment = { git = "https://github.com/chussenot/judgment" }` (no `package` key).
3. `docs/index.md`, section "What is not here": the crate was extracted from signalman and moved here (signalman's decisions 0010 and 0012); the pointer to signalman's `docs/` stays.
4. `mkdocs.yml`: `repo_url: https://github.com/chussenot/judgment`, `edit_uri: edit/main/docs/`, and the header comment. Then `mise run docs:llms`: the raw links in `docs/llms.txt` derive from `repo_url` and from the site's place under it, so until this step `docs:check` reports both files stale (they were generated for `crates/judgment` under signalman).
5. `CLAUDE.md`: delete the section "In the signalman workspace".
6. The transition wording: `grep -rn 'crates/judgment\|until the split\|inert\|workspace member\|signalman workspace' .` and reword each hit. The comments in `Cargo.toml`, `.github/workflows/ci.yml`, `mise.toml`, `.pre-commit-config.yaml`, `catalog-info.yaml` and `clippy.toml`, and the README's "Status" section, describe a situation that has ended; `mkdocs.yml`'s comment names the old TechDocs path. The pages' paths are already crate-relative.
7. `docs/llms-intro.txt`, `docs/decisions/README.md` and the verification records: keep their absolute links to signalman as they are; they point at pages that stay there.
8. Tooling: `mise trust && mise install && mise run setup`, then `cargo generate-lockfile` and commit `Cargo.lock` (a library may commit it; CI caches on it).
9. `mise run check` and `mise run precommit` green, then commit, push, and watch the first CI run.
10. GitHub: enable Actions if the organisation policy asks, protect `main` with the two checks required. Backstage: register `catalog-info.yaml` from the new repository; signalman's root file drops its `judgment` Component in the pull request below.

Nothing about the shared decision-record numbers changes: the crate's `docs/decisions/` keeps 0003 and continues the sequence shared with signalman's, checking both before numbering.

## What signalman changes afterwards

One pull request here, once the new repository's first CI run is green.

- `Cargo.toml`: drop `members = ["crates/judgment"]` (the `[workspace]` table stays, so `--workspace` keeps working); `judgment = { git = "https://github.com/chussenot/judgment", rev = "<the new repository's head>" }` under `[dependencies]`, the same source with `features = ["openapi"]` under `[dev-dependencies]`; remove the comment about keeping the two manifests in step. A revision pins exactly what was verified; switch to `tag = "vX.Y.Z"` once the crate tags releases, and to a crates.io version once it publishes. Bump the pin deliberately from then on: until it moves, a crate change does not run signalman's tests.
- `git rm -r crates/judgment`; `cargo build` rewrites `Cargo.lock`.
- `mise.toml`: remove `check:minimal` and `live:typesafe` (they are the crate's now), and `crates/judgment/docs` from `docs:check`; `.pre-commit-config.yaml`: drop the `crates/judgment/...` alternatives from the two docs hooks; `.github/workflows/ci.yml`: drop the `cargo check -p judgment` step; `.claude/hooks/llms-on-docs-edit.sh`: one set. `scripts/gen-llms-txt.sh` needs nothing: it finds the sites under its root itself.
- `catalog-info.yaml`: delete the `judgment` Component block; keep `dependsOn: component:default/judgment`, which the new repository's file provides once registered.
- `CLAUDE.md`: the crate bullet says the crate lives in `chussenot/judgment` and is a git dependency here, and that its rules live there; the documentation bullet and the harness bullets describe one set.
- `.claude/agents/`: `contract-reviewer`, `docs-writer`, `docs-auditor`, `test-writer`, `refactor-scout` and `observability-reviewer` name crate paths; point them at the new repository or remove the crate half.
- Documentation: every absolute link into `crates/judgment/` on this repository (the index, the README table, the decisions index's 0003 row, the development guide, `typesafe-client.md`, `laya.md`, the architecture and C4 pages, `triage.md`, `evaluation.md`, `roadmap.md`, the research notes, and `docs/llms-intro.txt`, whose line for the crate's index must name the new repository's raw URL) becomes `https://github.com/chussenot/judgment/blob/main/...`; `docs/development.md` loses the crate's tree from the layout and the two-set table points at the other repository; `docs/decisions/README.md` gains a status note that 0012 is done; `mise run docs:llms` after. This grep is the list:

  ```sh
  grep -rn 'crates/judgment' --include='*.md' --include='*.txt' --include='*.yml' --include='*.yaml' --include='*.toml' --include='*.sh' --include='*.rs' . | grep -v '^./target' | grep -v 'llms-full.txt\|llms.txt'
  ```

  Leave three kinds of hit alone: records 0010 and 0011 (records are not edited after acceptance; the status note carries the change), `.beads/issues.jsonl` (history), and the generated `llms*.txt` (regenerated). The comment in `tests/webhook_server.rs` is reworded, not removed.

- Beads: close `signalman-bdl`, the epic that tracks this page.

## Verification

- In the new repository: `mise run check` and `mise run precommit` green locally and in CI on the first commit; `git log --follow -- src/client.rs` reaches 2026-09-20; `cargo package --list` shows the docs, the examples with their recordings and the vendored OpenAPI document.
- Here, after the pull request above: `mise run check` green with `cargo tree -i judgment` showing the git source; `tests/typesafe_contract.rs` still passes, which proves the document comes through the crate; the docs auditor's link check finds no link into `crates/judgment/`.
- The rehearsal of 2026-10-03: the `filter-repo` command above on a clone of `main` gave 46 commits, the crate's files at the root and nothing else, and `src/client.rs` followed across the move. Run again on the branch that added this page, the split repository (49 commits, the scaffolding at its root, no tags) reported both `llms` files stale until `repo_url` and `edit_uri` were changed and `mise run docs:llms` run, then passed its own `mise run check` after `cargo generate-lockfile`.
