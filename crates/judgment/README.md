# judgment

Typed, calibrated judgments from [TypeSafe](https://docs.typesafe.ai) System One
models (Jev) and any backend that speaks the same wire.

A generative model asked to classify something answers in prose, or in JSON it
was told to produce; a `confidence` field in that JSON is generated text, not a
measured probability. A System One model does not generate text. It evaluates a
`state` (any JSON) against typed questions and returns calibrated answers: a
probability of yes, one option out of a defined set with the full distribution
and a confidence, or a position on ordered levels. Code owns the workflow; the
model supplies the judgment. This crate keeps that boundary typed end to end:
a question returns a handle that fixes its answer's type, probabilities are
validated newtypes, limits are checked before sending, and reading an answer
through its handle yields a Rust enum or a number, never a misread one.

```rust
use judgment::{Client, Questions, options};

options! {
    enum Department {
        Billing = "billing" => "Payments, invoicing, refunds",
        Technical = "technical" => "Bugs, outages, integrations",
    }
}

async fn run() -> judgment::Result<()> {
    let mut questions = Questions::new();
    let dept = questions.choice::<Department>("department", "Which team should handle `message`?")?;
    let urgent = questions.noul("is_urgent", "Does `message` convey urgency?", None)?;

    let client = Client::from_env()?;
    let state = serde_json::json!({ "message": "My payouts have been failing for 3 days." });
    let response = client.system_one(&state, &questions).await?;

    let dept = response.get(&dept)?;     // Choice<Department>
    let urgent = response.get(&urgent)?; // Noul
    if dept.chosen == Department::Billing && dept.confidence.at_least(0.7) && urgent.is_yes(0.6) {
        // page billing on-call
    }
    Ok(())
}
```

## Why this crate exists

It was the client half of [signalman](https://github.com/chussenot/signalman),
an alert triager, where it has been run against the hosted model and against
an open-weights model through a shim, with the wire shape verified live (and
a 401 seen not to be retried). The retry rules this release rewrote or added
(the status set, the jitter, `retry-after-ms` and `Retry-After` parsing, the
budget) are checked offline only, against wiremock in `tests/client.rs` and
by the unit tests in `src/http.rs`, until a live 408, 429 or 529 is recorded.
A second project wanting the same layer had to depend on the whole
application. Decision record
[0010](https://github.com/chussenot/signalman/blob/main/docs/decisions/0010-extract-the-judgment-core-into-a-crate.md)
records why it became a crate, what stayed behind, and why the existing
crates for the same API were not adopted.

## What is in it

- `question`: the builder, one typed `Handle` per question, the `options!`
  macro for enum-backed choices, `dynamic_choice` for option sets known only
  at runtime, and the HTTP API reference page's limits (255 options, 2 to 10
  levels, stricter than the OpenAPI document) checked before anything is
  sent.
- `answer`: `Probability` and `Confidence` newtypes that refuse values outside
  `[0, 1]`, the wire `Answer`, and `Response::get(&handle)` returning `Noul`,
  `Choice<T>` or `Score`. Decoding is tolerant and reading is strict: an
  answer of a kind this release does not know decodes as `Answer::Unknown`
  (the client logs it at `warn`), a missing `usage` reads as zero, and
  undocumented top-level fields (Laya's `routing`, say) are kept in
  `Response::extra`; a known answer that breaks its shape is still an error.
  Decoding does not fail on an unknown answer: the response is refused, as
  `AnswerTypeMismatch` naming its kind, only when the answer sits under a
  question that was asked, and under an unasked id it is kept.
  `Response::verify(&questions)` holds a response against the questions it
  was sent for: an answer under every id, of the question's primitive, a
  Choice naming only options it offered, a Score whose legend is the levels
  sent and whose value is on their scale. The SDKs check the shape of an
  answer and stop there; an answer that names an option nobody offered is an
  error here, never read as a guess.
- `client` (feature `http`, default): `Client` with the official SDKs'
  defaults and `RetryPolicy`: two retries of 408, 429, 5xx and transport
  failures, exponential backoff whose jitter only shortens a wait, and the
  server's wait (`retry-after-ms`, or `Retry-After` in seconds or as an HTTP
  date) honoured up to a cap. An overall retry budget is available and off by
  default, and `RetryPolicy::conservative()` retries only what cannot have
  been billed twice. The rustdoc of `RetryPolicy` lists where it matches the
  SDKs and where it deliberately differs. `Client::evaluate_with` takes a
  `CallOptions` for one call's timeout, retry policy, headers and extra body
  fields, and `ClientBuilder::default_header` sets a header on every call.
  What the client sets itself (the key, the content type, the user agent, the
  retry count both SDKs own, and the `state`, `model` and `questions` fields)
  is refused with an error before anything is sent, where the SDKs silently
  keep or overwrite it. Options stay off the `SystemOne` trait, so a
  recording's key is unchanged.
- `http` (feature `http`): the retry loop behind `Client`, shareable by other
  `reqwest` clients.
- `error`: one enum grouped by remedy: configuration, request, transient
  (after the retries stopped), transport, decode, and reading an answer. A 400
  or a 422 is `InvalidRequest` with the server's message and the fields it names
  as `ValidationIssue`s with dotted paths (`questions.urgency.score.criteria`);
  a 403 is `PermissionDenied`, apart from a 401, because a new key does not
  fix it; a malformed API key (whitespace inside, a control or non-ASCII
  character) is `InvalidApiKey` when the client is built, before any request,
  and no message quotes the key. Every error that came from an HTTP response,
  and every `Response`, carries TypeSafe's request id (`x-typesafe-request-id`)
  when the API sent one, the id its support asks for; the `typesafe.*` spans
  record it too.
- `backend`: `SystemOne`, the one-method trait every source of answers
  implements, so the code consuming judgments never knows which. `Client` is
  one; `Fake` answers from a table and remembers what it was asked; `Recorder`
  writes another backend's responses to a directory; `Replay` answers from
  that directory offline, keyed by a content hash of the request. Every one
  of them verifies its response before returning it, so a response that
  reaches the caller answers what was asked, whichever backend is behind the
  trait. The client does not retry a response that does not fit (it was
  billed), still reports its usage, and counts it as a failed attempt,
  `unfit`, beside `decode` for a 2xx body that does not decode.
- `eval`: what makes calibrated probabilities trustworthy rather than assumed:
  recordings for replay, one `Judgment` per answer and label, per-question
  accuracy, Brier score, calibration error and confidence when right or wrong.
- `observer`: the seam an application uses to count tokens and failed attempts
  in its own metrics. The crate emits `tracing` spans and nothing else.

Without the `http` feature the crate is the questions, the answers, the fake
and replay backends and the metrics, for a project with its own transport.

## Testing without the model

A `Fake` answers from a table, refuses a question it has no answer for, and
remembers every call, so a test checks the decision and what was asked. It
verifies its response like the client, so a scripted option the question
does not offer, or an answer of the wrong primitive, fails the call (and is
not remembered) instead of passing a test the real client would fail. A
scripted Score is only its probabilities: its legend is the levels of the
question it answers, echoed as a server echoes them.

```rust
let backend = Fake::new()
    .choice("department", [("billing", 0.9), ("technical", 0.1)], 0.8)?
    .noul("is_urgent", 0.2)?;
let response = backend.answer(&state, "any-model", &questions).await?;
assert_eq!(backend.calls()[0].question_ids, ["department", "is_urgent"]);
```

A `Recorder` writes what a real backend answered; a `Replay` over the same
directory answers the same requests later, with no key and no network:

```rust
let recorder = Recorder::new(Client::from_env()?, "recordings");
let live = recorder.answer(&state, "jev-latest", &questions).await?;
let replay = Replay::open(Path::new("recordings"))?;
let again = replay.answer(&state, "jev-latest", &questions).await?;
assert_eq!(live, again);
```

## Patterns

TypeSafe documents four [patterns](https://docs.typesafe.ai/patterns): the
shapes a System One call takes inside a larger program. Each has a runnable
example here, written to the documentation page's own scenario and
thresholds, in a domain of its own.

| Pattern | The shape | What carries it in this crate | Run |
|---|---|---|---|
| [Speculative fan-out](https://docs.typesafe.ai/patterns/fan-out) | Every question the decision tree might need goes in one request; the branch that is taken reads its answers and the others go unread. Questions are answered in parallel, so the extra ones cost input tokens, not latency | One `Questions` with a typed `Handle` per question; each branch reads only its handles | `cargo run -p judgment --example fan_out` |
| [Confidence-gated routing](https://docs.typesafe.ai/patterns/confidence-routing) | The answer says what, the confidence says whether to act: a floor sends uncertainty to a person, and each action sets its own bar by what a wrong one would cost | `Choice::confidence`, a `Confidence` that cannot be thresholded as a `Probability`; `confidence_from_probabilities` for the formula behind it | `cargo run -p judgment --example confidence_routing` |
| [Composite scoring](https://docs.typesafe.ai/patterns/composite-scoring) | Several atomic Scores, normalised and combined with weights the code owns; a new weighting needs no new inference | `Score::value` per dimension; `Recorder` and `Replay`, so the weights change over recorded answers | `cargo run -p judgment --example composite_scoring -- --weights 0.5,0.1,0.3,0.1` |
| [Intent routing](https://docs.typesafe.ai/patterns/intent-routing) | A cheap classifier in front of expensive handlers, so code, a specialist model or a person each get only what needs them; a second question gates the escalation | A `Choice` and a `Score` in one request, a `Confidence` read off each | `cargo run -p judgment --example intent_routing` |

Each example replays the recordings committed beside it
(`examples/<name>/recordings/`, `jev-1.13.0`'s answers of 2026-10-03) by
default, so it runs with no key and no network and prints every answer next
to the decision it led to. `-- --live` sends the same requests to the hosted
API (`TYPESAFE_API_KEY`; `TYPESAFE_BASE_URL` for another server,
`TYPESAFE_MODEL` for another model), and `-- --record` does that and
rewrites the recordings. Each example ends in a test over its recordings
that `cargo test` runs: the request hash covers the questions, so a question
changed without re-recording fails the gate rather than the next reader.

Three things the pages say that the examples make concrete. Thresholds are
starting points to tune on your own data, not constants. The hosted model's
probabilities are not deterministic, and the spread grows with ambiguity:
identical requests moved by up to 0.05 on a clear-cut input and by 0.19 in
probability (0.28 in confidence) on an ambiguous one, the decision holding
every time
([judgment against the hosted TypeSafe API](docs/verification/hosted-typesafe.md)).
A recording is one draw, and a threshold needs its margin most where the
input is least clear. A Noul
carries no confidence, so it is thresholded on its probability, where a
Choice or a Score has both. And a Score's confidence falls fast when
probability splits between neighbouring levels (a 60/40 split on three
levels is 0.40), so the intent example's second gate, a 0.5 floor on the
complexity's confidence, sends mild complaints to a person on `jev-1.13.0`;
that floor, or the number of levels, is the first thing to tune.

The same primitives take other shapes, each with a
[cookbook](https://docs.typesafe.ai/cookbooks): select a value or a span
from candidates found in code rather than generate it; rerank retrieved
passages with one question per pair; verify a claim against its evidence
and escalate what fails; turn scores into features for a classical model.
None has an example here yet. They are the same `Questions`, handles and
backends arranged differently.

## Checking a real server

The unit and integration tests never leave the process, so they cannot tell
whether a server speaks the wire the way the mocks assume. Two things can:

- `tests/live.rs` holds `#[ignore]` tests that run the three primitives, a
  structured level (and print how the server echoes it), the model list, an unknown model name, an unknown extra
  body field, a bearer check and a record-then-replay against whatever
  `JUDGMENT_LIVE_BASE_URL` points at. `cargo test` skips them; run them by hand with `-- --ignored`.
- `examples/typed_decisions.rs` replays the
  [typed-decisions](https://huggingface.co/datasets/LocalLLaMA/typed-decisions)
  benchmark (400 cases, 2,000 typed decisions) through the crate and scores it
  with `judgment::eval`, live or from recordings. `examples/typed-decisions/`
  holds a 40-case sample and the script that exports the full split.

Both were run against Laya's `typed-decisions` checkpoint through
`laya-serve` 0.3.20, and the live tests again through 0.3.24; what they
found, including the decoding bug the first run caught and the legend
comparison the second one loosened, is in
[Against Laya typed-decisions](docs/verification/laya-typed-decisions.md).
`examples/laya/serve_laya.py` is a System One-compatible shim over the
`laya` package for when `laya-serve` is not wanted; it is the one Laya
server that also answers `GET /v1/models`.

### Checking against the published contract

TypeSafe publishes an OpenAPI document for the System One API at
<https://api.typesafe.ai/openapi.json>. A copy is vendored at
`tests/fixtures/typesafe-openapi.json` (OpenAPI 3.1.0, API version 0.2.0;
an application that validates its own traffic against the same document
reads it as `judgment::contract::OPENAPI_DOCUMENT` with the `openapi`
feature), and `tests/contract.rs` validates against it, as JSON Schema
2020-12, offline and in every `cargo test`:

- every request shape the builders produce (each primitive, string, object,
  array and null instructions, one-sided and structured Noul criteria,
  undescribed options, 255 options, 2 and 10 levels of every level shape),
  sent through each client entry point, with the method, path, content type
  and bearer scheme the document names, and also against a closed copy of
  the request components, so a renamed or misspelt field fails even where
  the published schema, which closes no object, would take it as an extra
  key;
- every response a `Fake` builds, and all 40 committed recordings under
  `examples/typed-decisions/recordings`, as committed and after a decode and
  re-serialise;
- the document's own examples, which decode through `Response` and read
  through typed handles, and its model list and validation error, through
  `list_models` and `Error::InvalidRequest`.

Where the crate and the schema disagree the test pins the difference, each
at its own path, so a refreshed document that closes one fails loudly:

- The crate sends what the schema refuses: a null or numeric state, numeric
  instructions, a boolean Noul criterion, a numeric Score level, an empty
  question set. The builders take any JSON value there and the client any
  `Serialize` state, so such a request reaches the server, which is expected
  to refuse it with a 422.
- The builder refuses what the schema allows: 1 or 256 options, 1 or 11
  levels, an empty question id, an empty option key. It follows the HTTP
  API reference page, which is stricter than the schema. The hosted API
  was probed past those limits on 2026-10-03: it refuses 256 options and
  11 levels with a 400, refuses an empty question id with a 400, and
  answers one option or one level with probability 1 and accepts an empty
  option key, so the upper bounds are the server's and the lower ones
  this crate's alone (the rustdoc of `question` says why).
- The crate decodes differently: it refuses a probability or confidence
  outside `[0, 1]` and a negative token count, which the schema types as
  bare numbers, because a value no threshold can use is better an error;
  and it accepts a response without `usage`, with `usage` or a token count
  null, with no answers, or with a legend entry that is null or a scalar,
  which the schema refuses, because decoding is tolerant and
  `Response::verify` is what holds a response to its questions. A `Fake`
  asked nothing answers `answers: {}`, which the schema refuses.

An answer of a kind the document does not name decodes as `Answer::Unknown`
and is refused by the schema. That case is not run through the validator:
`the_schema_has_the_kinds_and_paths_the_crate_has` compares the document's
discriminator mappings with the crate's kinds, so a document that adds a kind
fails there.

The copy is refreshed only through `tests/openapi_drift.rs`, an ignored test
that needs the network and no key. It compares the copy with the live
document and names what differs; with `JUDGMENT_OPENAPI_WRITE` set it
rewrites the copy canonically instead:

```sh
cargo test -p judgment --test openapi_drift -- --ignored          # stale?
JUDGMENT_OPENAPI_WRITE=1 cargo test -p judgment --test openapi_drift -- --ignored
cargo test -p judgment --test contract                            # review the refresh
```

This checks the published schema, not a live account: a server can accept
or refuse what its schema does not say, which is what `tests/live.rs` is
for.

## Documentation

The crate's documentation lives with it, under [`docs/`](docs/index.md):

| Page | What it answers |
|---|---|
| [How judgment works](docs/design.md) | How a handle ties a question to its answer, how a response is checked before it is read, how the retry loop decides, what the crate leaves out |
| [Against the hosted TypeSafe API](docs/verification/hosted-typesafe.md) | What `jev-1.13.0` did with the live tests and with fifty probes past the builder's limits |
| [Against Laya typed-decisions](docs/verification/laya-typed-decisions.md) | The same tests against an open-weights server, the bug they caught, the benchmark numbers |
| [System One client libraries](docs/research/system-one-client-libraries.md) | What the other Rust clients and the official SDKs do, and what the crate adopted |
| [Decisions](docs/decisions/README.md) | Why the API is shaped as it is |
| [llms.txt](docs/llms.txt) | The index for agents and models; [llms-full.txt](docs/llms-full.txt) is every page in one file |

The rustdoc (`cargo doc -p judgment --open`) is the reference for every type, error and default. How signalman, the application the crate came from, uses it is signalman's documentation: [TypeSafe client](https://github.com/chussenot/signalman/blob/main/docs/typesafe-client.md).

## Status

`0.2.0`, a workspace member of the signalman repository, not yet on crates.io.
What changed in each release, breaking changes listed, is in
[CHANGELOG.md](CHANGELOG.md).
Take it by git until a second consumer settles the API:

```toml
[dependencies]
judgment = { git = "https://github.com/chussenot/signalman", package = "judgment" }
```

Live behaviour has been verified only under signalman's own account; the
vendored OpenAPI document and the wiremock tests are the contract in this
repository. Licensed MIT.
