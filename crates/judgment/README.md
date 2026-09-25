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
an open-weights model through a shim, with the retry behaviour and the wire
shape verified live. A second project wanting the same layer had to depend on
the whole application. Decision record
[0010](https://github.com/chussenot/signalman/blob/main/docs/decisions/0010-extract-the-judgment-core-into-a-crate.md)
records why it became a crate, what stayed behind, and why the existing
crates for the same API were not adopted.

## What is in it

- `question`: the builder, one typed `Handle` per question, the `options!`
  macro for enum-backed choices, `dynamic_choice` for option sets known only
  at runtime, and the documented limits (255 options, 2 to 10 levels) checked
  before anything is sent.
- `answer`: `Probability` and `Confidence` newtypes that refuse values outside
  `[0, 1]`, the wire `Answer`, and `Response::get(&handle)` returning `Noul`,
  `Choice<T>` or `Score`. Decoding is tolerant and reading is strict: an
  answer of a kind this release does not know is kept as `Answer::Unknown`
  (the client logs it at `warn`) instead of failing the whole response, a
  missing `usage` reads as zero, and undocumented top-level fields (Laya's
  `routing`, say) are kept in `Response::extra`; a known answer that breaks
  its shape is still an error, and reading an unknown one through a handle
  is `AnswerTypeMismatch` naming its kind.
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
  that directory offline, keyed by a content hash of the request.
- `eval`: what makes calibrated probabilities trustworthy rather than assumed:
  recordings for replay, one `Judgment` per answer and label, per-question
  accuracy, Brier score, calibration error and confidence when right or wrong.
- `observer`: the seam an application uses to count tokens and failed attempts
  in its own metrics. The crate emits `tracing` spans and nothing else.

Without the `http` feature the crate is the questions, the answers, the fake
and replay backends and the metrics, for a project with its own transport.

## Testing without the model

A `Fake` answers from a table, refuses a question it has no answer for, and
remembers every call, so a test checks the decision and what was asked:

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

## Checking a real server

The unit and integration tests never leave the process, so they cannot tell
whether a server speaks the wire the way the mocks assume. Two things can:

- `tests/live.rs` holds `#[ignore]` tests that run the three primitives, a
  structured level, the model list, an unknown model name, an unknown extra
  body field, a bearer check and a record-then-replay against whatever
  `JUDGMENT_LIVE_BASE_URL` points at. `cargo test` skips them; run them by hand with `-- --ignored`.
- `examples/typed_decisions.rs` replays the
  [typed-decisions](https://huggingface.co/datasets/LocalLLaMA/typed-decisions)
  benchmark (400 cases, 2,000 typed decisions) through the crate and scores it
  with `judgment::eval`, live or from recordings. `examples/typed-decisions/`
  holds a 40-case sample and the script that exports the full split.

Both were run against Laya's `typed-decisions` checkpoint through
`laya-serve`; what they found, including the one decoding bug they caught,
is in the signalman documentation page
[judgment against Laya typed-decisions](../../docs/judgment-laya-typed-decisions.md).

## Status

`0.1.0`, a workspace member of the signalman repository, not yet on crates.io.
Take it by git until a second consumer settles the API:

```toml
[dependencies]
judgment = { git = "https://github.com/chussenot/signalman", package = "judgment" }
```

Live behaviour has been verified only under signalman's own account; the
wiremock tests are the contract in this repository. Licensed MIT.
