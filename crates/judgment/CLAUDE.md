# judgment

Typed, calibrated judgments from TypeSafe System One models (Jev) and
compatible backends (Laya), as a Rust crate: typed questions and answers, the
HTTP client with the official SDKs' retries, record and replay, evaluation.
The README says why; `docs/` says how; the rustdoc is the reference.

## Start here

- `mise install` once, then `mise run setup` (git hooks) and `mise run check`
  (every quality gate, CI order). `mise tasks` lists the rest.
- `docs/index.md` maps the documentation; `docs/decisions/` holds the records
  that govern the API; `CHANGELOG.md` is Keep a Changelog, and 0.x means a
  minor release may break.

## Rules that are easy to get wrong

- Load the `typesafe:typesafe-ai` skill before touching questions, answers,
  the client or the backends. The live docs are the contract; start at
  https://docs.typesafe.ai/llms.txt. The OpenAPI document
  (https://api.typesafe.ai/openapi.json) is vendored at
  `tests/fixtures/typesafe-openapi.json`, checked by `tests/contract.rs`,
  exposed as `judgment::contract::OPENAPI_DOCUMENT` under the `openapi`
  feature, and refreshed only through `tests/openapi_drift.rs`; never
  hand-edit it.
- The crate is generic. Nothing about any one application (alerts, an
  incident tool, a catalog, an application's metrics) goes in. Token usage
  and failed attempts are reported through `judgment::Observer`; the crate
  emits `tracing` spans and no metrics of its own. The `http` feature gates
  the client; questions, answers, the other backends, recordings and metrics
  build without it (`mise run check:minimal`).
- Every backend holds a response against the questions it was sent
  (`Response::verify`): an answer for every question, of its primitive, a
  Choice naming only offered options, a Score whose legend parses to the
  levels sent. An off-list option is an error, never a default. Deterministic
  logic stays in the caller; the model answers narrow, atomic questions.
- Errors are grouped by what fixes them (`Error`). A 2xx the client cannot
  use is `decode` or `unfit` to the observer and is never retried; every HTTP
  call goes through `http::send_with_retries`.
- Tests never call a real API: wiremock for the client, `Fake` and recordings
  for everything else. The exceptions are `tests/live.rs`, all `#[ignore]`,
  run by hand against `JUDGMENT_LIVE_BASE_URL` (`mise run live:typesafe` with
  `TYPESAFE_API_KEY` in `.env`, `mise run live:laya` against a local server;
  `docs/verification/` records what real servers did), and
  `tests/openapi_drift.rs`, ignored, network only, no key.
- A change to what the crate sends or accepts on the wire is a contract
  change: `tests/contract.rs`, the CHANGELOG and the README's guarantees move
  together, and the recordings under `examples/*/recordings/` must still
  replay (`cargo test` runs them).
- Documentation: every page under `docs/` carries frontmatter (`title`,
  `description`, `status`, `last_reviewed`, `tags`); the README does not
  (crates.io renders it). `docs/llms.txt` and `docs/llms-full.txt` are
  generated from `mkdocs.yml`, `docs/llms-intro.txt` and that frontmatter:
  never edit them; run `mise run docs:llms` after adding a page (add it to
  the nav too) or changing a title or description. Paths in sources and pages
  are relative to the crate. Links to signalman's documentation are absolute
  GitHub URLs (signalman's decision 0011).
- Decision records share one number sequence with signalman's
  `docs/decisions/`: a new record takes the next free number in either
  repository, and a number is never reused.
- Secrets live in `.env` (gitignored, loaded by mise). Never commit one.
  `cargo publish` is denied by the Bash guard until a release is decided.

## In the signalman workspace

Until the split (signalman's decision 0012; `docs/judgment-extraction.md`
there is the runbook) this directory is also a member of the signalman
workspace, whose root `CLAUDE.md` applies as well and whose gates run with
`--workspace`. The files here that only make sense at a repository root
(`.github/`, `mise.toml`, `.pre-commit-config.yaml`, `scripts/`, `.claude/`,
`catalog-info.yaml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`)
are inert inside signalman, except that prek runs the nested hook
configuration as a workspace of its own, and become the new repository's on
the split. Keep them in step with signalman's until then; the two docs
scripts are byte-identical copies.

## Harness

- Subagents in `.claude/agents/`: `contract-reviewer` (the wire against the
  live TypeSafe documentation and the vendored OpenAPI document),
  `docs-writer` and `docs-auditor` (this documentation set, the why as well
  as the how), `test-writer` (tests in this crate's wiremock, Fake and
  recording style), `refactor-scout` (dead code and duplication, ranked),
  `pr-shepherd` (open the PR, read its CI checks). Reviewers report, writers
  edit; run the reviewers on a diff before opening a pull request.
- Hooks in `.claude/hooks/`: Rust files are formatted after every edit;
  `docs/llms.txt` is regenerated after a page, the README, `llms-intro.txt`
  or `mkdocs.yml` is written; a Bash guard denies pushes to `main`,
  committing `.env`, and `cargo publish`.
