# signalman

Typed Rust client for the TypeSafe System One API, an alert-triage flow built
on it, and an incident.io integration. The README says why; `docs/` says how.

## Start here

- `docs/development.md` is the contributor guide: tools, tasks, gates, layout.
- `mise install` once, then `mise run setup` (git hooks) and `mise run check`
  (every quality gate, CI order). `mise tasks` lists the rest.
- Track work in beads (section below). Create or claim an issue before code.

## Rules that are easy to get wrong

- Load the `typesafe:typesafe-ai` skill before touching questions, answers,
  the TypeSafe client or `src/triage/policy.rs`. The live docs are the
  contract; start at https://docs.typesafe.ai/llms.txt.
- The typed client is its own crate, `crates/judgment` (decision 0010), a
  workspace member signalman depends on by path and re-exports. It is
  generic: nothing about alerts, incident.io, Backstage or signalman's
  telemetry goes in there. It reports token usage and failed attempts
  through `judgment::Observer`, which `src/telemetry.rs` implements and
  `Providers::init` installs globally; it emits `tracing` spans and no
  metrics of its own. Its `http` feature gates the client; the questions
  and answers build without it. Gate and tests run with `--workspace`.
- incident.io contract: OpenAPI v3 at https://api.incident.io/v1/openapiV3.json
  and https://docs.incident.io/llms.txt. Never create incidents directly
  (decision `signalman-p2w`, `docs/decisions/0001-incidentio-remains-the-alert-hub.md`).
- Backstage contract: catalog OpenAPI at
  https://raw.githubusercontent.com/backstage/backstage/master/plugins/catalog-backend/src/schema/openapi.yaml,
  docs at https://backstage.io/docs. The catalog is the ownership source of
  truth (`docs/decisions/0004-catalog-is-the-ownership-source-of-truth.md`);
  the built-in team list (`triage::default_teams`) is only the fallback.
- Deterministic logic stays in code; the model answers narrow, atomic
  questions. Questions reference state by backticked path. Every Choice has
  a no-match option.
- The qualification note (`src/incidentio/note.rs`) is a fixed template over
  typed answers. It starts with the marker line and is replaced in place;
  never let the model write its prose, never stack a second note.
- Observability signals enter through incident.io (decision 0005). No Tsuga
  client until its spec is read with an operation API key.
- Configuration (decision 0006): default < TOML file < env var < flag.
  `src/config.rs` is the only module that reads a non-secret env var; the
  three clients, the change feed and the MCP HTTP endpoint read their own
  key or token and nothing else. A new setting
  needs the `Settings` schema, `Config::resolve`, and a row in
  `docs/configuration.md` (a test checks the env-var table against the doc).
  Secrets are never accepted from the file.
- Wire types under `src/incidentio/types.rs` ignore unknown fields and
  default optional ones.
- Telemetry (`src/telemetry.rs`, `docs/observability.md`): the one module
  that names an instrument; add a metric there, never inline. Spans are
  `#[tracing::instrument]` on client methods and the flow, named
  `service.operation`; never put a note body, a request body or a secret in
  a span field. Every upstream call goes through `http::send_with_retries`
  with its service label, which is where upstream errors are counted.
  `Providers::init` runs once in `main` before any span; export happens
  only when `telemetry.otlp_endpoint` is set, and `shutdown` must run on
  the way out or a CLI run exports nothing.
- Agents (decision 0008): signalman is a tool for agents, not an agent. No
  generative-model SDK in the triage path; agents consume the outcome
  contract (`src/outcome.rs`, schema committed at
  `docs/schema/outcome.v1.json`, drift-tested) and the MCP tools
  (`src/mcp.rs`; `signalman mcp` over stdio or Streamable HTTP, and `serve`
  mounts `/mcp` when `SIGNALMAN_MCP_TOKEN` is set — `src/mcp/http.rs`,
  stateless, bearer token checked before rmcp). A change to the wire
  shape is a schema change: regenerate the file, update the doc example, and
  bump `schema_version` only for a breaking change. Every MCP tool wraps a
  function the CLI already calls (`Triager`'s own fields and methods cover
  all six) — never reimplement decision logic in `src/mcp.rs`. The one write
  tool, `apply_qualification`, is registered only when `mcp.allow_write` is
  true (default off); it re-derives every write from the caller's own
  `Outcome` document (`validate`, `decision`, `answers`, `expected_tags`) —
  never add a second write path that trusts free-form input instead.
- Tests never call a real API: wiremock for both clients, hand-built answers
  for policy. The one exception is `crates/judgment/tests/live.rs`, all
  `#[ignore]`, run by hand against `JUDGMENT_LIVE_BASE_URL`
  (`docs/judgment-laya-typed-decisions.md`). Nothing has been verified
  against a live TypeSafe account yet; say so in docs where it matters.
- Markdown under `docs/` and the README carries frontmatter (`title`,
  `description`, `status`, `last_reviewed`, `tags`). `docs/llms.txt` and
  `docs/llms-full.txt` are generated from the `mkdocs.yml` nav and that
  frontmatter: never edit them, run `mise run docs:llms` after adding a
  page (add it to the nav too) or changing a title or description; the
  docs gate fails when they are stale.
- Secrets live in `.env` (gitignored, loaded by mise). Never commit one.

## Harness

- Subagents in `.claude/agents/` (`docs/development.md#claude-code-harness`
  says when to use each). Reviewers report, writers edit:
  `contract-reviewer` (API and MCP boundary vs live docs),
  `config-reviewer` (a setting's seven places, decision 0006),
  `observability-reviewer` (spans, metrics, no secrets in fields),
  `question-designer` (TypeSafe questions and policy), `test-writer`
  (tests in this repo's wiremock style), `docs-writer` (README and `docs/`,
  the why as well as the how), `docs-auditor` (docs vs code drift),
  `refactor-scout` (dead code and duplication, ranked), `pr-shepherd`
  (open the PR, tell the CI billing block from a real failure). Delegate
  the job; run the reviewers on a diff before opening a pull request.
- Hooks in `.claude/hooks/`: Rust files are formatted after every edit;
  `docs/llms.txt` is regenerated after a docs page, the README or
  `mkdocs.yml` is written; a Bash guard denies pushes to `main`, `bd edit`,
  committing `.env`, and `cargo publish`. `bd prime` runs at session start.

## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
