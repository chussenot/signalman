---
title: Against the hosted TypeSafe API
description: The judgment crate's live tests run against api.typesafe.ai with a real key, what each one showed about the hosted wire that the mocks and the Laya run could not, what probing past the builder's limits taught (three 400 shapes, the token budget, non-determinism at two decimals, the confidence formulas) and what the crate changed for it, and how to repeat the check with one task.
status: current
last_reviewed: 2026-10-03
tags: [judgment, typesafe, jev, compatibility, verification]
---

# Against the hosted TypeSafe API

The [judgment crate](../../README.md) is tested against mocks of TypeSafe's wire and the vendored OpenAPI document. A mock and a schema both encode what someone believed about the wire; only the server can contradict that belief. The [Laya run](laya-typed-decisions.md) checked the crate against a second implementation of System One, but no TypeSafe key was available then, so the hosted API itself had answered only once, through signalman's triage of the three example alerts (`signalman-b11.1`, 2026-09-23, recorded under `examples/eval/runs/jev-1.13.0`). That run proved signalman's own questions round-trip; it did not exercise the crate's edges: structured Score levels, null option descriptions, an unknown extra field, the model list, a wrong key.

This page records the run that did. All ten tests in `tests/live.rs`, the same file the Laya run used, passed against `https://api.typesafe.ai` with a real key on 2026-10-03; five more, written from the probes [below](#beyond-the-test-file), passed the same day, and `mise run live:typesafe` runs all fifteen.

## What was tested, and with what

| | |
|---|---|
| Server | `https://api.typesafe.ai`, the crate's default base URL |
| Key | `TYPESAFE_API_KEY`, signalman's own account, from the environment |
| Model requested | `jev-latest`, the crate's default; it resolved to `jev-1.13.0`, the version the 2026-09-23 run saw |
| Crate | `judgment` at the commit this page was added in, built with its default `http` feature |
| Tests | `tests/live.rs`, all ten, with `--test-threads=1`; the bearer test pointed at the same server with the same key |

Total wall time was about four seconds for the nine tests that call the model: the hosted API answers in hundreds of milliseconds, against two seconds per case for Laya on CPU, so the crate's 10 s default timeout is the right default for it. The run spends a handful of model calls on the account.

## What the hosted API does

What each test asserts and why it exists is tabled on the [Laya page](laya-typed-decisions.md#the-tests); this is what the hosted API showed on each point, set against what Laya did where they differ.

- **The three primitives round-trip through their typed handles.** The out-of-domain probe, a customer whose payouts have failed for three days, went to `Billing` with probability 0.91 and confidence 0.86, urgency 0.91, severity 1.99 on a three-level line (`blocked`). Laya's fine-tuned checkpoint sent the same message to `technical` at 0.45. The judgment is sensible as well as well-formed, which no mock can show.
- **No undocumented top-level fields.** `Response::extra` was empty: the hosted response is exactly the documented shape. Laya adds `routing`.
- **A request id comes back.** The response carries one (`req_…`), on success and on the 401 and 400 below. Laya sends none.
- **The model alias resolves and says to what.** `jev-latest` was answered by `jev-1.13.0`, named in `Response::model`. On Laya the model name routes silently and `model` is the same for every checkpoint.
- **`GET /v1/models` is served.** It listed `jev-latest` and `jev-preview`. Laya answers 404. signalman's `models` command works against the hosted API.
- **Structured Score levels are echoed verbatim.** The object level came back as a JSON object, the array level as a JSON array, the string as a string, and `Response::verify` passed. Laya did the same through 0.3.21; `laya-serve` 0.3.22 and later echo the JSON text they showed the model instead ([the Laya record](laya-typed-decisions.md#re-run-against-laya-serve-0324)), which is why `verify` now compares a structured level by what the echo parses to rather than by its text.
- **A Choice option without a description is accepted**, as TypeSafe documents.
- **An unknown extra field is refused, not answered.** A request with an unknown `CallOptions::extra` field got a 400, decoded as `Error::InvalidRequest`, with the message `Invalid request.` and an empty `issues` list; only the request id identifies the call. Laya answered the same request. The crate sends extra fields as given and leaves the choice to the server, so both outcomes pass, but a caller adding a field the API does not know gets no hint which field was wrong. Nothing in signalman sends an extra field.
- **A wrong key is a 401, classified as `Error::Unauthorized` and not retried; the right key is answered.**
- **The builder's limits hold without a request** (255 options, at least two levels, no null level); this test needs no server and is listed for completeness.

## Beyond the test file

The ten tests send what the builder lets through. On 2026-10-03 the hosted API was also probed with what the builder refuses, and with repeats, to learn what the schema and the reference page do not say (`signalman-b11.7`; about fifty requests, sequential, far under the account's 80 requests and 100,000 input tokens per second). What held, and what the crate did with it:

| Probe | What the hosted API did | Consequence |
|---|---|---|
| 256 options; 11 levels | `400 {"detail": "Too many choices. Must have at most 255 choices."}`, and the same sentence for levels | The builder's upper bounds are the server's. The rustdoc of `question` said this was unobserved; it now says what was observed, and a live test pins it |
| 0 options; 0 levels | `400 {"detail": "Choice question must have at least one choice: c"}`; `422` with a `too_short` issue at `questions.s.score.criteria` | Two shapes for one kind of mistake: hand-written checks answer a 400 with a sentence, the schema's own a 422 with issues |
| 1 option; 1 level | Answered: probability 1, confidence 1 | Accepted upstream, decides nothing; the builder's minimum of 2 is the crate's choice alone, and the docs now say so |
| A `null` level | `422`, three issues for the one value, one per type the field accepts (`criteria.1.str`, `criteria.1.dict[any,any]`, `criteria.1.list[any]`) | A union-typed field yields one issue per alternative with a trailing type segment; `ValidationIssue` keeps the path as sent, and its docs say to expect this |
| Unknown model; `JEV-LATEST` | `400 {"detail": {"error_type": "api_usage_error", "message": "Unknown model: …"}}` | A third 400 shape, undocumented: an object with a code and a message. Model names are case-sensitive |
| An extra top-level field | `400 api_usage_error`, message `Invalid request.`, naming no field | `CallOptions::extra` cannot be used against the hosted API; the live test records the refusal |
| An extra field inside a question | Answered, the field silently ignored | A misspelt optional field (`instruction`) is dropped without a word, which is the case for a typed builder over a hand-written map |
| A state of about 40,000 tokens | `400 {"detail": {"error_type": "max_tokens_exceeded"}}`, no message | The one 400 whose remedy is a smaller state. `Error::InvalidRequest` now carries the code as `kind`, `Error::is_request_too_large` names it (as it names the 413 `laya-serve` answers for the same reason), and the detail reads `max_tokens_exceeded` rather than the raw body |
| No `Authorization` header; a `Basic` scheme | `403 {"detail": {"error_type": "authentication_error", "message": "Must supply an API key! …"}}`; a wrong key is the `401` the test covers | The crate cannot send no key, but a proxy that strips the header shows as `PermissionDenied` with that message; its docs say so |
| Empty question id | `400 {"detail": "Question key cannot be empty."}` | The builder refuses it before sending |
| Two questions with the same id in the raw JSON | Answered: the last one, silently | The server keeps the last duplicate; the builder's `DuplicateQuestionId` is what stands between a caller and a question that vanishes |
| Option keys `""`, `a.b`, `1`, `Billing` beside `billing` | All accepted and given probability; case distinguishes keys | The builder now refuses the empty key, which could come back as the `choice`; the rest pass through |
| `state` as a string, an array, a number, `null` | String and array answered, as documented; a number is a `422` with three issues (`state.str`, `state.dict[any,any]`, `state.list[any]`); `null` is `missing` | `Client::system_one` takes any `Serialize`; what it must serialise to is a string, an object or an array |
| `GET /v1/models` | Lists `jev-latest` and `jev-preview`, both resolving to `jev-1.13.0`; `release_date` is an RFC 3339 timestamp with microseconds, not the `YYYY-MM-DD` of the OpenAPI document; the versioned name is not listed but is accepted in a request | `fixtures/models.json` is now this list; `ModelInfo::release_date` stays a string. To pin a version, read `Response::model`, not the list |
| The same request repeated: a clear-cut one three times, twice over, then three voice commands six times each | The chosen option and the nearest level held in every repeat. On the clear-cut request, probabilities 0.01 apart in one set and 0.05 in the next (confidence 0.89, 0.83, 0.90); on a saturated one (`What's my current balance?`, p 1.00) no movement over six; on two ambiguous commands the top probability moved by 0.19 (0.65 to 0.84) and 0.12, and the confidence by 0.28 (0.48 to 0.76) and 0.17 | **Not deterministic, and the spread grows with ambiguity.** A recording is one draw; a threshold near an observed value flips between runs, and the less clear the input, the wider the band it flips in; a test must not assert a live probability to the hundredth. A live test pins the stable part (the decision) and prints the spread |
| Confidence against the documented formulas | Choice: `(p_max − 1/n)/(1 − 1/n)` within 0.01 of the wire in every case; Score: `score` equals `Σ i·p_i` exactly, confidence within 0.015 | `Choice::confidence_from_probabilities`, `Score::expected_value` and `Score::confidence_from_probabilities` compute them, so a caller can see at once whether a server (Laya) defines confidence otherwise |
| Options reversed | `billing` 0.94 against 0.92–0.93 in the original order | Within the run-to-run spread on a case this clear; the known-issues page's order bias is real but needs an uncertain case to measure. A follow-up (`signalman-b11.8`) keeps signalman's candidate order stable |
| Headers | `x-typesafe-request-id` on the 200 and on every 4xx; no rate-limit or `Retry-After` header seen (no 429 was provoked); served through Cloudflare | What the crate reads is what is sent |
| Load and cost | p50 250 ms per request; 25 Nouls in one request in 344 ms for 820 input tokens; ten mixed questions in 229 ms. Output tokens scale with the answer (about 20 per Noul, 17 per Score, 24 plus 6 per option for a Choice; 2,826 for 255 options) and are free | Batching is nearly free in latency; a Choice over many options is a flat distribution (top probability 0.05 over 255), which is the documented reason for hierarchical selection |

The OpenAPI drift test ran the same day: the vendored document is the live one, so every contract pin above describes the current server.

## What this does and does not establish

It establishes that the crate's wire matches the hosted API on every edge the live tests cover, with no change to the crate. Together with the 2026-09-23 triage run, it means both the crate's general surface and signalman's own questions have been answered by Jev at least once.

It does not establish anything about thresholds or accuracy on alerts: that needs labelled history through the [evaluation harness](https://github.com/chussenot/signalman/blob/main/docs/evaluation.md) (`signalman-ufg.6`). And `jev-latest` is an alias. When it moves past `jev-1.13.0`, the answers these tests and the committed run saw may change without any code changing; pin `typesafe.model` once the thresholds are tuned ([Configuration](https://github.com/chussenot/signalman/blob/main/docs/configuration.md)), and repeat this run when the alias moves.

## Repeating the run

```sh
mise run live:typesafe
```

The task needs `TYPESAFE_API_KEY` (from `.env`, which mise loads) and fails before building anything when it is unset. It points both the live tests and the bearer test at `TYPESAFE_BASE_URL` (default `https://api.typesafe.ai`) with that key, and asks for `TYPESAFE_DEFAULT_MODEL` (default `jev-latest`), the same variables signalman itself reads. It is not part of `mise run check` and never runs in CI: the gate stays offline, and a key in CI would be a secret the repository does not need.

Without mise, the same run is:

```sh
JUDGMENT_LIVE_BASE_URL=https://api.typesafe.ai JUDGMENT_LIVE_API_KEY="$TYPESAFE_API_KEY" \
JUDGMENT_LIVE_AUTH_BASE_URL=https://api.typesafe.ai JUDGMENT_LIVE_AUTH_API_KEY="$TYPESAFE_API_KEY" \
JUDGMENT_LIVE_MODEL=jev-latest \
  cargo test -p judgment --test live -- --ignored --nocapture --test-threads=1
```
