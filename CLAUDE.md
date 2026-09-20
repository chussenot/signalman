# rustsafe

Typed Rust client for the TypeSafe System One API (`POST /v1/systemone`), an
alert-triage flow built on it, and an incident.io integration (API client,
Svix webhook receiver, tag and attachment write-back).

## Working on this repo

- Use the **typesafe** skill (`typesafe@typesafe-ai` plugin, pinned in
  `.claude/settings.json`) whenever a change touches questions, answers, the
  client or the triage policy. The skill points at the live docs; they are the
  source of truth for the API contract, question design and confidence
  handling. Start from https://docs.typesafe.ai/llms.txt.
- Keep deterministic logic in code. The model answers narrow, atomic
  questions; routing, thresholds and formatting live in `src/triage/policy.rs`.
- Questions reference the state by backticked path (`alert.title`). Every
  Choice needs a no-match option when nothing may fit.
- Check before pushing: `cargo fmt --all --check`, `cargo clippy --all-targets`
  (pedantic, warnings denied in CI), `cargo test`.
- Tests never call the real API. Client behaviour is covered with `wiremock`
  in `tests/client.rs`; policy is unit-tested with hand-built answers.
- `TYPESAFE_API_KEY` is required at runtime only. Never commit one.
- incident.io contract: the OpenAPI v3 spec at
  `https://api.incident.io/v1/openapiV3.json` and the guides at
  `https://docs.incident.io/llms.txt` (webhooks: `api-reference/webhooks.md`).
  Webhooks are Svix-signed; the verifier in `src/incidentio/webhook.rs` is
  pinned to Svix's published test vector. Never create incidents directly:
  write tags and attachments, or feed an HTTP alert source, and let alert
  routes decide.
- Wire types in `src/incidentio/types.rs` must ignore unknown fields and
  default optional ones; incident.io adds properties without notice.
