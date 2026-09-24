# judgment

Typed, calibrated judgments from [TypeSafe](https://docs.typesafe.ai) System One
models (Jev) and backends that speak the same wire.

A System One model does not generate text. It evaluates a `state` (any JSON)
against typed questions and returns calibrated answers: a probability of yes,
one option out of a defined set with the full distribution and a confidence, or
a position on ordered levels. Code owns the workflow; the model supplies the
judgment. This crate keeps that boundary typed end to end: adding a question
returns a handle that fixes the answer's type, and reading the answer through
the handle yields a Rust enum, a probability or a score, never a misread
number.

```rust
use judgment::{Client, Questions, options};

options! {
    enum Department {
        Billing = "billing" => "Payments, invoicing, refunds",
        Technical = "technical" => "Bugs, outages, integrations",
    }
}

# async fn run() -> judgment::Result<()> {
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
# Ok(()) }
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
  macro for enum-backed choices, the documented limits (255 options, 2 to 10
  levels) checked before anything is sent.
- `answer`: `Probability` and `Confidence` newtypes that refuse values outside
  `[0, 1]`, the wire `Answer`, and `Response::get(&handle)` returning `Noul`,
  `Choice<T>` or `Score`.
- `client` (feature `http`, default): `Client` with SDK-equivalent defaults
  and `RetryPolicy` (two retries, exponential backoff with jitter,
  `Retry-After` honoured up to a cap), errors by remedy (`Unauthorized`,
  `InvalidRequest`, `RateLimited`, `Overloaded`, `Transport`, `Decode`).
- `backend`: `SystemOne`, the one-method trait every source of answers
  implements, so the code consuming judgments never knows which. `Client` is
  one; `Fake` answers from a table and remembers what it was asked; `Recorder`
  writes every response of another backend to a directory; `Replay` answers
  from that directory offline, keyed by a content hash of the request.
- `eval`: what makes calibrated probabilities trustworthy rather than assumed.
  Recordings for replay, one `Judgment` per answer and label, and per-question
  `QuestionMetrics`: accuracy, Brier score, expected calibration error,
  confidence when right and when wrong.
- `observer`: the seam an application uses to count tokens and failed attempts
  in its own metrics. The crate emits `tracing` spans and nothing else.

Without the `http` feature the crate is the questions, the answers and the
errors alone, for a project that brings its own transport.

## Status

`0.1.0`, a workspace member of the signalman repository, taken by git until a
second consumer settles the API:

```toml
[dependencies]
judgment = { git = "https://github.com/chussenot/signalman", package = "judgment" }
```

Licensed MIT.
