---
title: judgment against the hosted TypeSafe API
description: The judgment crate's live tests run against api.typesafe.ai with a real key, what each one showed about the hosted wire that the mocks and the Laya run could not, and how to repeat the check with one task.
status: current
last_reviewed: 2026-10-03
tags: [judgment, typesafe, jev, compatibility, verification]
---

# judgment against the hosted TypeSafe API

The [judgment crate](typesafe-client.md) is tested against mocks of TypeSafe's wire and the vendored OpenAPI document. A mock and a schema both encode what someone believed about the wire; only the server can contradict that belief. The [Laya run](judgment-laya-typed-decisions.md) checked the crate against a second implementation of System One, but no TypeSafe key was available then, so the hosted API itself had answered only once, through signalman's triage of the three example alerts (`signalman-b11.1`, 2026-09-23, recorded under `examples/eval/runs/jev-1.13.0`). That run proved signalman's own questions round-trip; it did not exercise the crate's edges: structured Score levels, null option descriptions, an unknown extra field, the model list, a wrong key.

This page records the run that did. All ten tests in `crates/judgment/tests/live.rs`, the same file the Laya run used, passed against `https://api.typesafe.ai` with a real key on 2026-10-03.

## What was tested, and with what

| | |
|---|---|
| Server | `https://api.typesafe.ai`, the crate's default base URL |
| Key | `TYPESAFE_API_KEY`, signalman's own account, from the environment |
| Model requested | `jev-latest`, the crate's default; it resolved to `jev-1.13.0`, the version the 2026-09-23 run saw |
| Crate | `judgment` at the commit this page was added in, built with its default `http` feature |
| Tests | `crates/judgment/tests/live.rs`, all ten, with `--test-threads=1`; the bearer test pointed at the same server with the same key |

Total wall time was about four seconds for the nine tests that call the model: the hosted API answers in hundreds of milliseconds, against two seconds per case for Laya on CPU, so the crate's 10 s default timeout is the right default for it. The run spends a handful of model calls on the account.

## What the hosted API does

What each test asserts and why it exists is tabled on the [Laya page](judgment-laya-typed-decisions.md#the-tests); this is what the hosted API showed on each point, set against what Laya did where they differ.

- **The three primitives round-trip through their typed handles.** The out-of-domain probe, a customer whose payouts have failed for three days, went to `Billing` with probability 0.91 and confidence 0.86, urgency 0.91, severity 1.99 on a three-level line (`blocked`). Laya's fine-tuned checkpoint sent the same message to `technical` at 0.45. The judgment is sensible as well as well-formed, which no mock can show.
- **No undocumented top-level fields.** `Response::extra` was empty: the hosted response is exactly the documented shape. Laya adds `routing`.
- **A request id comes back.** The response carries one (`req_…`), on success and on the 401 and 400 below. Laya sends none.
- **The model alias resolves and says to what.** `jev-latest` was answered by `jev-1.13.0`, named in `Response::model`. On Laya the model name routes silently and `model` is the same for every checkpoint.
- **`GET /v1/models` is served.** It listed `jev-latest` and `jev-preview`. Laya answers 404. signalman's `models` command works against the hosted API.
- **Structured Score levels are echoed verbatim.** The object level came back as a JSON object, the array level as a JSON array, the string as a string, and `Response::verify` passed. This is what Laya does too, so the compact-JSON-in-a-string form `verify` also accepts has still not been seen from any server.
- **A Choice option without a description is accepted**, as TypeSafe documents.
- **An unknown extra field is refused, not answered.** A request with an unknown `CallOptions::extra` field got a 400, decoded as `Error::InvalidRequest`, with the message `Invalid request.` and an empty `issues` list; only the request id identifies the call. Laya answered the same request. The crate sends extra fields as given and leaves the choice to the server, so both outcomes pass, but a caller adding a field the API does not know gets no hint which field was wrong. Nothing in signalman sends an extra field.
- **A wrong key is a 401, classified as `Error::Unauthorized` and not retried; the right key is answered.**
- **The builder's limits hold without a request** (255 options, at least two levels, no null level); this test needs no server and is listed for completeness.

## What this does and does not establish

It establishes that the crate's wire matches the hosted API on every edge the live tests cover, with no change to the crate. Together with the 2026-09-23 triage run, it means both the crate's general surface and signalman's own questions have been answered by Jev at least once.

It does not establish anything about thresholds or accuracy on alerts: that needs labelled history through the [evaluation harness](evaluation.md) (`signalman-ufg.6`). And `jev-latest` is an alias. When it moves past `jev-1.13.0`, the answers these tests and the committed run saw may change without any code changing; pin `typesafe.model` once the thresholds are tuned ([Configuration](configuration.md)), and repeat this run when the alias moves.

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
