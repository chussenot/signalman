---
title: Development
description: Tools, tasks, quality gates, repository layout, planning with beads, and the Claude Code harness for contributors.
status: current
last_reviewed: 2026-09-20
tags: [development, tooling]
---

# Development

## Tools

[mise](https://mise.jdx.dev) pins every tool in `mise.toml`: the Rust toolchain (version from `rust-toolchain.toml`), Node for the beads CLI, [prek](https://prek.j178.dev) for git hooks, and `bd`.

```sh
mise install        # tools
mise run setup      # git hooks: prek (pre-commit, pre-push) and beads
mise tasks          # everything below
```

## Tasks

| Task | What it runs |
|---|---|
| `check` | `fmt:check`, `lint`, `test`, `doc`, `docs:check`, in CI order |
| `fmt` / `fmt:check` | `cargo fmt --all` / with `--check` |
| `lint` | `cargo clippy --all-targets --all-features` with `RUSTFLAGS=-D warnings` |
| `test` | `cargo test --all-features` |
| `doc` | `cargo doc --no-deps --document-private-items` with `RUSTDOCFLAGS=-D warnings` |
| `docs:check` | frontmatter on `README.md` and `docs/**` |
| `precommit` | `prek run --all-files` |
| `build` | release build |
| `serve` | `cargo run -- serve` plus any flags |
| `triage <file>` | `cargo run -- triage <file>` plus any flags |
| `triage:examples` | print the TypeSafe request for every example alert |
| `bd:ready` / `bd:export` | ready issues / refresh the JSONL snapshot |

## Git hooks

`.pre-commit-config.yaml` is run by [prek](https://prek.j178.dev). On commit: whitespace and line-ending fixes, YAML/TOML/JSON syntax, merge markers, large files, private keys, no commits on `main`, `cargo fmt --check`, `cargo clippy`, Markdown frontmatter, and a refusal to commit `.env`. On push: `cargo test`. Beads-managed files and `Cargo.lock` are excluded from the fixers.

Git's `core.hooksPath` is `.beads/hooks`, set by `bd init`. Those hook files are tracked and contain a beads-managed section between markers; beads preserves anything outside the markers when it upgrades them. `scripts/setup-hooks.sh` (run by `mise run setup`) adds a marked block before the beads section that calls prek, so every commit and push runs prek's hooks first and beads' hooks second. Do not run `prek install`: it would replace the beads files with prek's own shim.

To run the hooks without committing: `mise run precommit`, or `prek run --all-files`.

## Quality gates

Clippy runs with the `pedantic` group plus `unwrap_used` and `expect_used`, warnings denied. Tests never reach the network: `wiremock` stands in for both APIs, and policy tests round-trip fake responses through the real handles. Doc tests include a `compile_fail` case and are part of `cargo test`.

CI (`.github/workflows/ci.yml`) runs the same gates and `prek run --all-files`.

## Layout

```
src/
  client.rs        TypeSafe HTTP client
  http.rs          shared retry loop, backoff, Retry-After
  question.rs      Questions builder, Options trait, options! macro, Handle<A>
  answer.rs        Answer wire shape, Probability/Confidence, typed views
  error.rs         TypeSafe-side error enum
  triage/          Alert state, questions, Decision policy
  incidentio/      client, types, webhook verification, sync flow, errors
  backstage/       catalog client, entity types, enrichment, notifications
  serve.rs         axum webhook receiver
  main.rs          CLI
tests/             wiremock integration tests and the end-to-end webhook run
examples/          sample alerts and a sample webhook delivery
docs/              this documentation
scripts/           check-frontmatter.sh
.beads/            issue tracker data and hooks
.claude/           agents, hooks, settings for Claude Code
```

## Planning with beads

Issues live in a local Dolt database under `.beads/`, managed with `bd`. `bd ready` lists unblocked work; claim with `bd update <id> --claim`, finish with `bd close <id>`. `bd remember` stores durable project knowledge; `bd memories` searches it.

Cross-machine sync uses `bd dolt push` and `pull` over `refs/dolt/data` on the git remote. The environment that seeded the backlog could not push that ref, so `.beads/issues.jsonl` holds a one-time export. Import it once with `bd import .beads/issues.jsonl`, push with `bd dolt push`, then treat Dolt as the source of truth and drop the file.

## Claude Code harness

`.claude/settings.json` pins the `typesafe@typesafe-ai` skill plugin, pre-allows the read-only and build commands used in this repository, denies reading `.env`, and wires three hooks:

| Hook | Script | Effect |
|---|---|---|
| `SessionStart` | `bd prime --hook-json` | injects the beads workflow |
| `PreToolUse` on Bash | `.claude/hooks/guard-bash.sh` | denies pushes to `main`, the interactive `bd edit`, committing `.env`, `cargo publish` |
| `PostToolUse` on Edit/Write | `.claude/hooks/rustfmt-on-edit.sh` | formats a Rust file right after it is written |

The guard matches command positions only and ignores here-doc bodies, so documentation that mentions a forbidden command is not blocked. Its cases are listed in the script header.

Subagents in `.claude/agents/`:

| Agent | Use it for |
|---|---|
| `contract-reviewer` | checking boundary code against the live TypeSafe and incident.io documentation |
| `question-designer` | writing or reviewing questions, criteria and policy thresholds |
| `docs-writer` | keeping the README and `docs/` accurate and dry |

`CLAUDE.md` holds the short list of rules that are easy to get wrong; it points here for everything else.
