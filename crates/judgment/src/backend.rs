//! Where the answers come from.
//!
//! A [`SystemOne`] is anything that takes a state and a set of questions
//! and returns calibrated answers: the hosted model behind
//! [`Client`](crate::client::Client), an
//! open-weights model behind the same wire, a recording of an earlier run,
//! or a fake with the answers a test wants. The trait exists so that the
//! code consuming judgments never has to know which; an application, a
//! harness or a test holds a `&dyn SystemOne` and the choice is made at
//! construction. It is also the seam that makes an official SDK, should
//! one appear, an adapter rather than a rewrite.
//!
//! The state is a [`serde_json::Value`] rather than a generic `Serialize`
//! so the trait can be used as a trait object; a typed state is one
//! `serde_json::to_value` away, and [`SystemOne::answer_typed`] does it.
//! The cost is one boxed future per call and that one conversion, both
//! small next to a network round trip.
//!
//! Three implementations ship here besides the client: [`Fake`] answers
//! from a table and remembers what it was asked; [`Recorder`] wraps another
//! backend and writes every response to a directory; [`Replay`] answers
//! from such a directory without any model, keyed by a content hash of the
//! request, so a suite can run against yesterday's real answers offline.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::answer::{Answer, Confidence, Probability, Response, Usage};
use crate::error::{Error, Result};
use crate::eval::{Recording, request_hash};
use crate::question::Questions;

/// A boxed, sendable future: what a trait object can return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Something that answers typed questions about a state.
pub trait SystemOne: Send + Sync {
    /// Answer `questions` about `state` with `model` (a name or alias; a
    /// backend that has one model may ignore it and report its own).
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>>;

    /// [`Self::answer`] over any serialisable state.
    fn answer_typed<'a, S: Serialize + Sync>(
        &'a self,
        state: &'a S,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>>
    where
        Self: Sized,
    {
        Box::pin(async move {
            let state = serde_json::to_value(state)?;
            self.answer(&state, model, questions).await
        })
    }
}

impl<T: SystemOne + ?Sized> SystemOne for &T {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        (**self).answer(state, model, questions)
    }
}

impl<T: SystemOne + ?Sized> SystemOne for std::sync::Arc<T> {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        (**self).answer(state, model, questions)
    }
}

impl<T: SystemOne + ?Sized> SystemOne for Box<T> {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        (**self).answer(state, model, questions)
    }
}

/// Through [`Client::evaluate`](crate::client::Client::evaluate), with the
/// client's own settings and default headers: per-call options do not cross
/// the trait (`client` module docs, `# Per-call options`). If extra body
/// fields ever do, they must enter [`request_hash`], with an empty set
/// hashing as it does today, or a [`Replay`] would answer a different
/// request.
#[cfg(feature = "http")]
impl SystemOne for crate::client::Client {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            self.evaluate(&crate::client::Request {
                state,
                model,
                questions,
            })
            .await
        })
    }
}

// ---------------------------------------------------------------------------
// Fake
// ---------------------------------------------------------------------------

/// One request a [`Fake`] received.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The state as sent.
    pub state: Value,
    /// The model asked for.
    pub model: String,
    /// The question ids, in order.
    pub question_ids: Vec<String>,
}

/// A backend that answers from a table and remembers what it was asked.
///
/// Every question in a request must have an answer, or the call fails with
/// [`Error::MissingAnswer`] naming it: a test that forgets a question learns
/// so from the fake, not from a wrong decision downstream. The answers are
/// returned as given; whether they fit the question's primitive is checked
/// where every answer is checked, in [`Response::get`].
#[derive(Debug, Default)]
pub struct Fake {
    model: String,
    answers: BTreeMap<String, Answer>,
    usage: Usage,
    calls: Mutex<Vec<Call>>,
}

impl Fake {
    /// An empty fake reporting model `fake`.
    pub fn new() -> Self {
        Self {
            model: "fake".to_owned(),
            ..Self::default()
        }
    }

    /// The model name every response reports.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The usage every response reports.
    #[must_use]
    pub fn usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    /// The wire answer for question `id`, stored as given: for a shape the
    /// helpers below do not build, or to feed a real recorded answer back
    /// through a fake.
    #[must_use]
    pub fn with_answer(mut self, id: impl Into<String>, answer: Answer) -> Self {
        self.answers.insert(id.into(), answer);
        self
    }

    /// A Noul answer: the probability of yes. Fails when `p_yes` is outside
    /// `[0, 1]`, the same check the wire gets.
    pub fn noul(self, id: impl Into<String>, p_yes: f64) -> Result<Self> {
        let noul = Probability::new(p_yes)?;
        Ok(self.with_answer(id, Answer::Noul { noul }))
    }

    /// A Choice answer from `(option, probability)` pairs; the chosen option
    /// is the most probable one and the confidence is as given. Fails when a
    /// probability or the confidence is outside `[0, 1]`. Whether the
    /// probabilities sum to 1 is not checked, since the wire layer does not
    /// check it either.
    pub fn choice<'p>(
        self,
        id: impl Into<String>,
        probabilities: impl IntoIterator<Item = (&'p str, f64)>,
        confidence: f64,
    ) -> Result<Self> {
        let mut probs = BTreeMap::new();
        let mut best: Option<(String, f64)> = None;
        for (option, p) in probabilities {
            probs.insert(option.to_owned(), Probability::new(p)?);
            if best.as_ref().is_none_or(|(_, b)| p > *b) {
                best = Some((option.to_owned(), p));
            }
        }
        let choice = best.map(|(k, _)| k).unwrap_or_default();
        Ok(self.with_answer(
            id,
            Answer::Choice {
                choice,
                probabilities: probs,
                confidence: Confidence::new(confidence)?,
            },
        ))
    }

    /// A Score answer from one probability per level, lowest level first;
    /// the score is the probability-weighted position and the legend names
    /// the levels `level 0`, `level 1` and so on. Fails when a probability or
    /// the confidence is outside `[0, 1]`.
    #[allow(clippy::cast_precision_loss)] // at most ten levels
    pub fn score(
        self,
        id: impl Into<String>,
        probabilities: impl IntoIterator<Item = f64>,
        confidence: f64,
    ) -> Result<Self> {
        let mut probs = BTreeMap::new();
        let mut legend = BTreeMap::new();
        let mut score = 0.0;
        for (i, p) in probabilities.into_iter().enumerate() {
            probs.insert(i.to_string(), Probability::new(p)?);
            legend.insert(i.to_string(), Value::String(format!("level {i}")));
            score += p * i as f64;
        }
        Ok(self.with_answer(
            id,
            Answer::Score {
                score,
                legend,
                probabilities: probs,
                confidence: Confidence::new(confidence)?,
            },
        ))
    }

    /// Every request received so far, oldest first: what a test asserts to
    /// check that the code under test asked the right questions about the
    /// right state with the right model.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl SystemOne for Fake {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let mut answers = BTreeMap::new();
            for id in questions.ids() {
                let answer = self
                    .answers
                    .get(id)
                    .ok_or_else(|| Error::MissingAnswer(id.to_owned()))?;
                answers.insert(id.to_owned(), answer.clone());
            }
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(Call {
                    state: state.clone(),
                    model: model.to_owned(),
                    question_ids: questions.ids().map(str::to_owned).collect(),
                });
            }
            Ok(Response {
                model: self.model.clone(),
                answers,
                usage: self.usage,
                request_id: None,
                extra: BTreeMap::new(),
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Recorder and Replay
// ---------------------------------------------------------------------------

/// A backend that passes every request to another and writes the response
/// to `dir/<request hash>.json` as a [`Recording`], so a [`Replay`] over the
/// same directory answers the same requests later without a model.
///
/// The response is written as received, [`Response::request_id`] included,
/// so a recording still names the call TypeSafe can look up.
#[derive(Debug)]
pub struct Recorder<B> {
    inner: B,
    dir: PathBuf,
}

impl<B: SystemOne> Recorder<B> {
    /// Record `inner`'s answers under `dir`, created on first use.
    pub fn new(inner: B, dir: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            dir: dir.into(),
        }
    }

    /// The wrapped backend, for reading what it saw: a [`Fake`]'s calls, for
    /// example.
    pub fn inner(&self) -> &B {
        &self.inner
    }
}

impl<B: SystemOne> SystemOne for Recorder<B> {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let hash = request_hash(state, questions);
            let started = Instant::now();
            let response = self.inner.answer(state, model, questions).await?;
            let recording = Recording {
                case: hash.clone(),
                response: response.clone(),
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                request_hash: Some(hash),
            };
            crate::eval::write_recording(&self.dir, &recording).map_err(|e| Error::Io {
                context: format!("recording under {}", self.dir.display()),
                source: std::io::Error::other(e),
            })?;
            Ok(response)
        })
    }
}

/// A backend that answers from recordings keyed by request hash, and
/// nothing else: a request nobody recorded is [`Error::NoRecording`].
///
/// A replayed response carries the recorded call's
/// [`Response::request_id`], not a new one: it is that call's answers, and
/// the id is how to find that call in TypeSafe's logs. A recording made
/// before the field existed replays with `None`.
#[derive(Debug, Default)]
pub struct Replay {
    by_hash: BTreeMap<String, Response>,
}

impl Replay {
    /// Load every `*.json` recording under `dir` that carries a request
    /// hash. Recordings without one (a harness's, keyed by case) are
    /// skipped; a file that is not a recording is an error.
    pub fn open(dir: &Path) -> Result<Self> {
        let mut by_hash = BTreeMap::new();
        let entries = std::fs::read_dir(dir).map_err(|source| Error::Io {
            context: dir.display().to_string(),
            source,
        })?;
        for entry in entries {
            let path = entry
                .map_err(|source| Error::Io {
                    context: dir.display().to_string(),
                    source,
                })?
                .path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|source| Error::Io {
                context: path.display().to_string(),
                source,
            })?;
            let recording: Recording = serde_json::from_str(&text)?;
            if let Some(hash) = recording.request_hash {
                by_hash.insert(hash, recording.response);
            }
        }
        Ok(Self { by_hash })
    }

    /// Number of distinct requests this replay can answer.
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// True when the directory held no hashed recording: it was never
    /// recorded, or it was recorded by a harness keyed by case id, which
    /// [`Replay::open`] skips.
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

impl SystemOne for Replay {
    fn answer<'a>(
        &'a self,
        state: &'a Value,
        _model: &'a str,
        questions: &'a Questions,
    ) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let hash = request_hash(state, questions);
            self.by_hash
                .get(&hash)
                .cloned()
                .ok_or(Error::NoRecording(hash))
        })
    }
}
