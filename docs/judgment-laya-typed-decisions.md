---
title: judgment against Laya typed-decisions
description: How the judgment crate was tested against Laya's typed-decisions checkpoint through laya-serve, what each test asserts and why, the one decoding bug the run caught, the benchmark numbers, and how to repeat the run.
status: experiment
last_reviewed: 2026-09-25
tags: [judgment, typesafe, laya, evaluation, compatibility]
---

# judgment against Laya typed-decisions

The [judgment crate](typesafe-client.md) was written against TypeSafe's documented wire and tested against mocks of it. A mock encodes what the client author believed about the wire; only a real server can contradict that belief. This page records the first such contradiction test: the crate against [Laya](laya.md)'s `typed-decisions` checkpoint, served by Laya's own HTTP server, on the benchmark the checkpoint was fine-tuned for. The question was narrow: does the crate work, unchanged, against a second implementation of the System One wire, and if not, what has to change on which side.

The short answer: yes, after one fix in the crate that TypeSafe's own API would have needed too. Everything the crate sends is accepted, everything Laya returns decodes through the typed handles, retries and error classification behave as documented, and the benchmark replays through the crate's evaluation module to numbers in line with the model card. Four differences between the two servers were found; none needs code, all are recorded below so the next person does not rediscover them.

## What was tested, and with what

| | |
|---|---|
| Model | [convaiinnovations/laya-typed-decisions](https://huggingface.co/convaiinnovations/laya-typed-decisions), revision `1a793eb` (2026-09-24), Apache 2.0. ModernBERT-large encoder, 421M parameters, fine-tuned on the typed-decisions training split with RLCD |
| Server | `laya-serve` from the `laya` PyPI package, 0.3.20, with `LAYA_MODELS=typed-decisions`; torch 2.14 CPU, transformers 5.17 |
| Weights served | The server loads the checkpoint from the `typed-decisions/` folder of the `convaiinnovations/laya` bundle; its `model.safetensors` has the same SHA-256 as the standalone repository's (`4fa56de7…07a24e`, 842,609,220 bytes), so the two are one model |
| Machine | 4 CPUs, 15 GB, no GPU, in the development container; every latency below is CPU latency |
| Crate | `judgment` at the commit this page was added in, built with its default `http` feature |
| Benchmark | [LocalLLaMA/typed-decisions](https://huggingface.co/datasets/LocalLLaMA/typed-decisions), `test` split: 400 cases, 5 questions each, across four workflows, with a gold label and distribution per question, Apache 2.0 |

Nothing here touched TypeSafe's hosted API: no key was available in the environment, so the comparison with Jev is against the numbers the benchmark and the model card publish, not a run of our own. That gap is stated where it matters.

## Why a test, rather than reading the two specifications

Laya's README says its payload is schema-identical to Jev's and lists three differences. The crate's rustdoc says which of TypeSafe's limits it enforces before sending. Comparing the two documents would have found the option-count difference and missed everything else the run found: the model list endpoint that the README documents and the server does not serve, the object level that neither document says is echoed verbatim, the model name that the router silently redirects. Each of those is a fact about running code, discoverable only by running it. The tests are kept so the next server, or the next Laya release, is checked the same way instead of by re-reading.

## The tests

The live checks are in `crates/judgment/tests/live.rs`. Every one is `#[ignore]`, so the gate stays hermetic and they run only by hand:

```sh
JUDGMENT_LIVE_BASE_URL=http://127.0.0.1:8000 JUDGMENT_LIVE_MODEL=typed-decisions \
  cargo test -p judgment --test live -- --ignored --nocapture
```

Each test exists for one belief the mocks could not check.

| Test | Asserts | Why it is there |
|---|---|---|
| `the_three_primitives_round_trip_through_typed_handles` | A Choice over an `options!` enum, a Noul with criteria and a three-level Score come back through their handles: the chosen option is the arg max of a distribution that sums to 1 within rounding, the legend echoes the levels in order, the score lies on the level line, confidence and probability are in `[0, 1]`, the response names a model and counts input tokens | The whole promise of the crate is that the handle fixes the answer's type. Laya rounds every probability to four decimals and adds fields the API does not have (`answer_confidence`, `action`, `routing`); the test shows the typed views survive both |
| `a_structured_score_level_comes_back_decoded` | A level sent as an object (`{"what": …, "examples": […]}`) decodes, and its label is the object's JSON | This is the test that failed first. See [the bug](#the-bug-the-run-caught) |
| `a_choice_option_without_a_description_is_accepted` | `dynamic_choice` with `None` descriptions is answered | TypeSafe documents `null` descriptions; Laya's README says every option needs a description. The server accepts them |
| `the_model_list_is_either_served_or_absent` | `list_models` returns a non-empty list or exactly `Error::Http { status: 404 }`; anything else fails | `GET /v1/models` is in TypeSafe's OpenAPI document and both SDKs call it, but the HTTP API reference page leaves it out, and a compatible server may not serve it. The crate must fail cleanly rather than hang or misdecode |
| `a_model_name_the_server_does_not_know_is_still_answered` | A client sending the crate's default `jev-latest` gets an answer | A consumer that only changed `base_url` must not be broken by the model name it never set |
| `a_live_answer_replays_offline_from_its_recording` | `Recorder` over the live client, then `Replay` over the directory, return the same `Response` for the same state and questions | The request hash that keys a recording is computed on the crate side; this proves it is stable across a real round trip, which the mocks could not, since they never see a serialised request |
| `the_builder_refuses_what_the_wire_would_reject` | 256 options, one level and a null level are refused by the builder before any request | Listed with the live tests so a run shows the limits next to the behaviour they guard; it needs no server |
| `a_wrong_bearer_token_is_unauthorized_and_the_right_one_is_not` | Against a second server started with `LAYA_API_KEY`, a wrong key is `Error::Unauthorized` and the right one is answered; skipped unless `JUDGMENT_LIVE_AUTH_BASE_URL` is set | The retry loop must not retry a 401 and the error must be the one whose remedy is "check the key". Laya returns FastAPI's `{"detail": …}` body rather than TypeSafe's `{"error": …}`; the classification is by status, so the body shape does not matter |

All eight passed on 2026-09-24 against the two servers described above.

The benchmark replay is `crates/judgment/examples/typed_decisions.rs`. It reads one case per line, builds the crate's `Questions` from each case's wire JSON through the public builders, sends every case through one `dyn SystemOne`, and grades each answer against the gold label with `judgment::eval`:

```sh
# live, recording every answer
TYPESAFE_BASE_URL=http://127.0.0.1:8000 TYPESAFE_MODEL=typed-decisions TYPESAFE_API_KEY=unused \
  cargo run -p judgment --example typed_decisions -- \
    crates/judgment/examples/typed-decisions/sample.jsonl --record /tmp/laya-run

# offline, from the recordings: no server, no key, same numbers
cargo run -p judgment --example typed_decisions -- \
  crates/judgment/examples/typed-decisions/sample.jsonl --replay /tmp/laya-run
```

`examples/typed-decisions/sample.jsonl` is the first ten test cases of each workflow, committed so the example runs from a checkout; `export.py` next to it writes the full split from the Parquet files. The example is a compatibility test at scale before it is an evaluation: 400 cases is 2,000 questions through the builder, 400 requests through the client, 2,000 answers through the decoder, and every combination of primitive and criteria shape the benchmark uses (Noul with and without criteria, Choice with three to five described options, Score with three to five levels).

## The bug the run caught

TypeSafe documents that a Score level may be a string or an object, and that the answer's `legend` maps each level number "back to its description". The crate typed the legend as `BTreeMap<String, String>` and the builder accepted any JSON value as a level. Laya, given an object level, echoes the object; the crate then failed to decode the whole response, not just that question, because one legend value was not a string. Reading TypeSafe's wording again, Jev would echo the object too: the legend is the level, not a summary of it.

The fix is in the crate. The legend now holds JSON values, and the typed `Score` view labels a level by its string or, for a structured level, by its compact JSON, so a log line or a note always has text without re-deriving it from the question. The builder also refuses a `null` level before sending: a level is "described in words" and a null has no meaning the API defines, but Laya accepts it and echoes `null` into the legend, which is one more way a response can fail to decode. Two unit tests cover the change without a server; the live test above covers it with one. Nothing in signalman changed: its Score levels are strings.

## What the two servers do differently

None of these needs a code change; each is a fact to know when pointing the crate at Laya.

- **The model name routes, and the response does not say where.** `laya-serve` maps `model` onto a checkpoint by name: `typed-decisions` or the full repository id picks the fine-tuned one, and any other value, including the crate's default `jev-latest`, lets Laya's router choose by language, which for English text means the base checkpoint, loaded on demand. With `LAYA_MODELS=typed-decisions` preloaded, a request that named no model still took 78 s the first time while the base checkpoint downloaded. The response's `model` field is `laya-rl-agent` for every checkpoint; which one answered is in a `routing` object the crate ignores. Consequence: set `.model("typed-decisions")` on the builder, or `TYPESAFE_MODEL` in signalman, and do not rely on `Response::model` to tell checkpoints apart.
- **No `GET /v1/models`.** Laya's README documents it; the 0.3.20 server has `/health` and `POST /v1/systemone` only. `list_models` returns `Error::Http { status: 404 }`. signalman's `models` command is therefore the one command that does not work against `laya-serve`; the shim under `examples/laya/` still serves it.
- **Latency is seconds, and the default timeout is ten.** A five-question case takes about two seconds on four CPU cores, warm. The crate's default per-attempt timeout mirrors the hosted API's hundreds of milliseconds; the example and the live tests set 120 s. In signalman, `typesafe.timeout_seconds` is the setting.
- **Confidence means something else.** TypeSafe computes `confidence` from the spread of the distribution; Laya reports one minus the normalised entropy under that name and adds `answer_confidence`, the probability of the reported answer, which its model card says to gate on. Both are in `[0, 1]`, so the typed `Confidence` accepts either, but a threshold tuned on Jev's confidence does not carry over. The server also warned at start-up that the checkpoint's temperature for Choice questions with eleven or more options is outside the range it trusts and was clamped; confidence on such questions is uncalibrated by Laya's own account.

Limits were probed as well. Laya answered 200 options and 11 levels, past its README's stated budget; the crate's builder stops at TypeSafe's 255 and 10 regardless, so the stricter of the two bounds applies. A malformed body is a 400 with a FastAPI `detail`. The run reported it as `Error::Http` with the body; the crate now reports it as `Error::InvalidRequest { status: 400, .. }`, the same variant as TypeSafe's 422, with the message read from `detail` (or `error`, which the shim under `examples/laya/` sends), and still not retried.

## What the benchmark measured

The full `test` split, 400 cases and 2,000 questions, through the crate against the server above, on 2026-09-24. Accuracy is the share of questions whose reported answer is the gold label. Calibration is graded on the probability of the reported answer (for a Noul, `max(p, 1 − p)`), not on the wire's `confidence` field, because that field is a different quantity on each server ([above](#what-the-two-servers-do-differently)) and the model card's own ECE is on the answer's probability. Brier is the crate's multi-class form, the squared distance between the reported distribution and the one-hot label summed over options, which is not the model card's definition and is not compared with it.

| Slice | n | Accuracy | Model card | ECE | Brier (crate) |
|---|---|---|---|---|---|
| All questions | 2,000 | 0.766 | 0.766 | 0.213 | 0.400 |
| Noul | 600 | 0.857 | 0.857 | 0.192 | 0.287 |
| Choice | 600 | 0.733 | 0.733 | 0.255 | 0.465 |
| Score | 800 | 0.723 | 0.723 | 0.198 | 0.435 |
| agent_trace_observability | 500 | 0.730 | 0.730 | 0.229 | 0.461 |
| customer_service | 500 | 0.764 | 0.764 | 0.210 | 0.396 |
| invoice_processing | 500 | 0.804 | 0.804 | 0.165 | 0.302 |
| security_incidents | 500 | 0.766 | 0.766 | 0.249 | 0.439 |

Every accuracy, overall, per primitive and per workflow, is the model card's figure to the third decimal, and the overall ECE (0.213) is the card's too. That is the compatibility result in one line: the crate's builders, client, decoder and grading reproduce the publisher's own evaluation of the model, so nothing between the case file and the number is lost or misread on the crate side. The card's Brier (0.062) is not reproduced because it is not the same statistic; the crate's is documented on `judgment::eval::metrics::brier`.

The numbers also say what the card says about the model. Mean answer probability was 0.58 when the answer was right and 0.46 when it was wrong, a usable gap, but an ECE of 0.2 means a reported 0.8 was right about 60% of the time: over-confident, as the card states. Noul is the strong primitive, Choice the weak one, and `invoice_processing` the easy workflow.

Latency, per five-question case on four CPU cores, warm, with nothing else running: p50 2.1 s, p95 2.2 s. During the full run, which shared the cores with the build gate and a second server, it was p50 4.1 s and p95 6.2 s; the run is I/O-free, so the difference is CPU contention. The card's tens of milliseconds are on a T4.

The 40-case sample committed with the example was recorded from the same server into `examples/typed-decisions/recordings/`; replaying it offline produces the identical report (accuracy 0.725 on 200 questions, the same to every digit as the live run that wrote it), which is the recording round trip checked at benchmark scale rather than on one request.

## What this does and does not establish

It establishes that the crate is not TypeSafe-specific in any way that matters: a second, independently written server was driven through the public API with one fix, and that fix corrected a reading of TypeSafe's own documentation. It establishes that the evaluation module and the recording backends work against real answers, not only the `Fake`. And it gives a number for the fine-tuned checkpoint on its own benchmark, through this crate, that the model card's number can be compared with.

It does not establish anything about signalman's alerts. The checkpoint is fine-tuned for four workflows that are not alert triage, and the one out-of-domain probe in the live test, a customer's message about failing payouts, was routed to `technical` with 0.45 probability, which is the model card's warning made concrete. The path to self-hosting is still the one [Laya as a model provider](laya.md) describes: labelled alert history through the [evaluation harness](evaluation.md), both models, per-question numbers. What this page adds is that the crate side of that path is proven; the remaining work is data.

## Repeating the run

```sh
python -m venv .venv
.venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
.venv/bin/pip install "laya[serve]" pyarrow huggingface_hub

# the server, typed-decisions only, on CPU
USE_TF=0 LAYA_MODELS=typed-decisions LAYA_DEVICE=cpu LAYA_THREADS=4 LAYA_PORT=8000 .venv/bin/laya-serve
# a second one that checks keys, for the bearer test
USE_TF=0 LAYA_MODELS=typed-decisions LAYA_DEVICE=cpu LAYA_PORT=8001 LAYA_API_KEY=secret .venv/bin/laya-serve

# the live tests
JUDGMENT_LIVE_BASE_URL=http://127.0.0.1:8000 JUDGMENT_LIVE_MODEL=typed-decisions \
JUDGMENT_LIVE_AUTH_BASE_URL=http://127.0.0.1:8001 JUDGMENT_LIVE_AUTH_API_KEY=secret \
  cargo test -p judgment --test live -- --ignored --nocapture --test-threads=1

# the full benchmark
.venv/bin/python crates/judgment/examples/typed-decisions/export.py --split test --out /tmp/typed-decisions-test.jsonl
TYPESAFE_BASE_URL=http://127.0.0.1:8000 TYPESAFE_MODEL=typed-decisions TYPESAFE_API_KEY=unused \
  cargo run -p judgment --example typed_decisions -- /tmp/typed-decisions-test.jsonl --record /tmp/laya-full --json
```

`--test-threads=1` keeps the tests from queueing behind each other on a single-worker server. Expect the first request to a checkpoint to include its load time, and the benchmark to take about a quarter of an hour on four cores.
