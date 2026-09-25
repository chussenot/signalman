//! Answers: wire shapes, validated probabilities, and the typed views that
//! [`Response::get`] produces from a [`crate::Handle`].
//!
//! # Two newtypes, not one `f64`
//!
//! The API returns two kinds of number in `[0, 1]`. A [`Probability`] is the
//! model's estimate for one outcome: that the answer is yes, that this option
//! is the right one. A [`Confidence`] is a summary of a whole Choice or Score
//! distribution: how concentrated it is on one outcome. A caller thresholds
//! them differently (act when the probability of yes is above 0.6; refuse to
//! act when the confidence is below 0.5), and swapping them is a silent bug,
//! so they are distinct types. Both refuse values outside `[0, 1]` on
//! construction and on deserialisation: a wire value of 1.2 is
//! [`Error::NotAProbability`] at decode time, never a number downstream. The
//! cost is a `.value()` call wherever the raw `f64` is wanted.
//!
//! # What confidence means, and does not mean
//!
//! TypeSafe's [confidence page](https://docs.typesafe.ai/confidence) defines
//! `confidence` as a statistic computed from the answer's own `probabilities`:
//! concentrated on one option or level means high, spread out means low. It
//! is a convenience the API computes so a caller can threshold without doing
//! the arithmetic, and the full distribution is returned so a caller can
//! compute a different measure. Noul answers carry none; `max(p, 1 - p)` is
//! the usual stand-in and what [`crate::eval`] uses.
//!
//! Confidence is a measure of the model's certainty, not a permission to act.
//! The same page's guidance is that the threshold is the caller's risk
//! tolerance: gate a destructive action higher than a read-only one, treat
//! low confidence as "route to a person", and set the numbers from observed
//! results rather than by intuition. This crate supplies
//! [`Confidence::at_least`] and nothing else; the policy is the caller's.
//!
//! # Decoding is tolerant, reading is strict
//!
//! A response is decoded whole, so a strict decoder fails it whole: one
//! answer of a primitive this release does not know, a server that reports
//! no `usage`, and every answer in the response is lost, the ones the caller
//! asked for and could have read included. TypeSafe adds primitives and
//! fields over time and a compatible server adds fields of its own (Laya's
//! `routing`, the `id` and `provider` of `OpenRouter`'s decisions endpoint),
//! so a strict decoder turns each addition into an outage until this crate
//! is upgraded.
//!
//! The decoder therefore keeps what it does not know, and reading refuses it
//! where it matters:
//!
//! * An answer whose `type` is a string other than `noul`, `choice` or
//!   `score` decodes as [`Answer::Unknown`], the answer as it came. The
//!   client logs it at `warn`, naming the question and the kind. Reading it
//!   through a handle is [`Error::AnswerTypeMismatch`], so the question that
//!   needed it fails and the others are still read.
//! * A known kind is decoded as strictly as before: `{"type": "noul",
//!   "noul": 1.2}` claims a shape and breaks it, and is [`Error::Decode`],
//!   not an unknown answer. So is an answer with no `type`, a `type` that is
//!   not a string, or an answer that is not an object.
//! * An absent or `null` `usage`, and an absent or `null` count inside it,
//!   read as zero ([`Usage`]). A negative, fractional or string count is
//!   still [`Error::Decode`].
//! * Top-level fields other than `model`, `answers`, `usage` and
//!   `request_id` are kept in [`Response::extra`] and written back where
//!   they were, so a recording keeps them. Fields inside an answer that this
//!   crate does not model are ignored, as in the Python SDK.
//!
//! The cost is that an answer this release cannot read, from a later API or
//! from a server that answers something else, no longer fails the response
//! at decode time: it fails where it is read, or not at all when nothing
//! reads it. The `warn` line is what shows it, and upgrading this crate (or
//! fixing the server) is the remedy. The Python SDK
//! logs a warning and skips such an answer; this crate keeps it, so a caller
//! or a recording can still look at what the server sent.
//!
//! # What `Response::verify` checks
//!
//! Nothing on the wire ties a response to the request it answers: the
//! answers come back as a map of id to answer, and a server can leave a
//! question out, answer it with another primitive, choose an option it was
//! never offered or describe a Score on another scale. Decoding cannot catch
//! any of that, since it does not know the questions. [`Response::verify`]
//! does: it holds a response against the [`Questions`] it was sent for, and
//! every backend in this crate (the client, [`crate::Fake`],
//! [`crate::Replay`] and [`crate::Recorder`]) calls it before it returns, so
//! a response that reaches the caller answers what was asked. After it
//! succeeds, [`Response::get`] with any handle from the same questions cannot
//! fail.
//!
//! For each question, in id order, stopping at the first failure:
//!
//! * There is an answer under its id ([`Error::MissingAnswer`]).
//! * The answer is of the question's primitive
//!   ([`Error::AnswerTypeMismatch`]). An answer of a kind this release does
//!   not know ([`Answer::Unknown`]) is a mismatch here, named by its escaped
//!   kind: under an asked question it is an answer the caller cannot read.
//! * A Choice names only options the question offered: the chosen option,
//!   then every key of its distribution ([`Error::UnknownOption`]).
//! * A Score is on the scale the question sent ([`Error::InvalidAnswer`]):
//!   its legend has one entry per level, keyed `"0"` to `"n-1"`, each the
//!   level as it was sent; its probabilities are keyed by those same indices
//!   and nothing else (`"01"` is not a level); and its score lies within
//!   `0..=n-1`, with a margin of `1e-9` for float error and nothing more. A
//!   NaN score fails. The message never quotes a level's text.
//!
//! Every error carries the response's [`Response::request_id`], because a
//! response that does not fit is a call TypeSafe can look up.
//!
//! What it leaves out, on purpose:
//!
//! * Sums. A distribution that sums to 0.97 or to 1.0002 is rounding, and
//!   the API does not promise a tolerance to check it against.
//! * An offered option missing from a Choice's distribution reads as zero
//!   ([`Choice::probability_of`]), as it would if the server had sent it with
//!   zero. That is a deliberate gap: the check is for keys the question never
//!   offered, not for a response to a narrower question.
//! * Which option is chosen. The chosen option is the most probable one on
//!   the wire, but ties and rounding make the argmax a poor check.
//! * Whether a Score's value is the expectation of its distribution. The
//!   server rounds both (a recorded Jev answer reports 2.23 where its rounded
//!   probabilities give 2.22), so only the scale is checked.
//! * Answers under ids nobody asked, [`Response::extra`], the model name and
//!   the usage.
//!
//! An off-list option fails rather than being read as the question's
//! no-match option: an option nobody offered names nothing the code can act
//! on, and reading it as another option would decide on an answer the model
//! did not give. That is the rule the typed handles were designed around
//! for a typed Choice, now applied to every question: an unknown option is
//! an explicit error naming the question, never a default. Nothing is
//! snapped or clamped either: a score of 3.001 on a four-level scale is
//! refused, not read as 3.
//!
//! A structured level (an object or an array) may be echoed as itself or as
//! the string of its compact JSON, which is what [`Score::levels`] labels it
//! with. Only Laya's verbatim echo has been observed; the HTTP API reference
//! types the legend as a map of strings while the OpenAPI document and the
//! Python SDK allow any value, so the hosted API may stringify a structured
//! level. A number that is written differently inside a structured level
//! (`1.0` for `1`) still fails; the ignored live test
//! `a_structured_score_level_is_echoed` prints what a server echoes, so the
//! rule can be tightened once it has been seen. A string level must come back
//! as that exact string.
//!
//! This goes beyond both official SDKs, which check the shape of each answer
//! and not whether it answers the question it is filed under. The cost is a
//! walk over the questions per response, and that a server which answers
//! more loosely than it is asked fails where an SDK would have returned the
//! answer.
//!
//! # What `Response::get` checks
//!
//! [`Response::get`] takes the handle a question was added with and returns
//! the answer as that handle's type. It fails with [`Error::MissingAnswer`]
//! when the response has no answer under the handle's id; with
//! [`Error::AnswerTypeMismatch`] when the answer is a different primitive
//! than the handle was created for, or one this release does not know
//! ([`Answer::Unknown`], named by its escaped `type`); with
//! [`Error::UnknownOption`] when a typed Choice's chosen option, or any key
//! in its distribution, is not in the Rust option set; and with
//! [`Error::Decode`] when a Score's legend keys are not level indices. Each
//! of the first three carries the response's request id. A
//! [`Choice<String>`] from a dynamic choice passes its keys through
//! unchecked, since there is no set to check them against; a verified
//! response has had them checked against the options the question offered.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::Hash;

use serde::de::{self, Deserializer, Unexpected};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::question::{Options, Question, Questions};

/// How far a Score's value may fall outside `0..=n-1` and still be on the
/// scale: float error in the server's arithmetic, and nothing a server could
/// mean. Private, so no caller comes to depend on a looser scale.
const SCORE_EPSILON: f64 = 1e-9;

/// The model's estimate for one outcome, in `[0, 1]`, validated on
/// construction and on deserialisation so an out-of-range wire value is an
/// error and never a number downstream.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Probability(f64);

impl Probability {
    /// Validate a raw value.
    pub fn new(value: f64) -> Result<Self> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::NotAProbability { value })
        }
    }

    /// The raw value.
    pub const fn value(self) -> f64 {
        self.0
    }

    /// True when at or above `threshold`.
    pub fn at_least(self, threshold: f64) -> bool {
        self.0 >= threshold
    }
}

impl TryFrom<f64> for Probability {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Probability> for f64 {
    fn from(p: Probability) -> Self {
        p.0
    }
}

impl fmt::Display for Probability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// How concentrated a Choice or Score distribution is on one outcome, from 0
/// (flat) to 1 (all on one option). Same domain as [`Probability`] but a
/// distinct type: a confidence is not the probability of any particular
/// outcome, and a caller thresholds the two differently. The module docs say
/// what it means and does not mean.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Confidence(f64);

impl Confidence {
    /// Validate a raw value.
    pub fn new(value: f64) -> Result<Self> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::NotAProbability { value })
        }
    }

    /// The raw value.
    pub const fn value(self) -> f64 {
        self.0
    }

    /// True when at or above `threshold`.
    pub fn at_least(self, threshold: f64) -> bool {
        self.0 >= threshold
    }
}

impl TryFrom<f64> for Confidence {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Confidence> for f64 {
    fn from(c: Confidence) -> Self {
        c.0
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// One answer as returned on the wire, discriminated by `type`.
///
/// Non-exhaustive: TypeSafe adds primitives, and each one this crate learns
/// becomes a variant in a minor release, so a `match` outside this crate
/// needs a wildcard arm (an `if let` needs nothing). Until a kind is learnt
/// it decodes as [`Answer::Unknown`] (module docs, `# Decoding is tolerant,
/// reading is strict`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum Answer {
    /// Probability of yes.
    Noul {
        /// 0 is no, 1 is yes.
        noul: Probability,
    },
    /// The chosen option and the full distribution.
    Choice {
        /// Highest-probability option key.
        choice: String,
        /// Option key to probability; sums to 1.
        probabilities: BTreeMap<String, Probability>,
        /// Distribution concentration.
        confidence: Confidence,
    },
    /// A probability-weighted position on the levels.
    Score {
        /// Weighted position; may fall between levels.
        score: f64,
        /// Level index (as string) to the level as it was sent: a string,
        /// or the object a structured level was described with. The API
        /// echoes the level, it does not summarise it, so the value is kept
        /// as JSON rather than forced into a string that a structured level
        /// would fail to decode into.
        legend: BTreeMap<String, Value>,
        /// Level index (as string) to probability.
        probabilities: BTreeMap<String, Probability>,
        /// Distribution concentration.
        confidence: Confidence,
    },
    /// An answer whose `type` this release does not know: a primitive the
    /// API added after it, or a server that answers something else. It holds
    /// the JSON object as it came, `type` included; [`Answer::kind`] reads
    /// that `type`, and reading it through a handle is
    /// [`Error::AnswerTypeMismatch`]. The client logs one at `warn`.
    ///
    /// It serialises back as that object, so a [`crate::Recorder`] keeps it
    /// and a [`crate::Replay`] returns it. A hand-built `Unknown` whose
    /// `type` is `noul`, `choice` or `score` serialises as that kind, and
    /// decodes back as the known variant (or fails to). When a later release
    /// learns a kind, answers of it stop decoding as `Unknown`; that is a
    /// change in behaviour for code that inspects `Unknown`, so such code
    /// should look at [`Answer::kind`] rather than rely on a kind staying
    /// unknown.
    #[serde(untagged)]
    Unknown(Value),
}

impl Answer {
    /// The answer's wire `type`: `noul`, `choice` or `score`, or an unknown
    /// answer's own `type` string, as the server sent it (`unknown` when it
    /// has none, which only a hand-built [`Answer::Unknown`] can lack).
    ///
    /// An unknown kind is whatever string the server chose, so this crate
    /// escapes it and cuts it to 64 characters before it goes into an error
    /// message, a log line or a graded judgment; a caller that logs it should
    /// do the same.
    pub fn kind(&self) -> &str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
            Self::Unknown(raw) => raw.get("type").and_then(Value::as_str).unwrap_or("unknown"),
        }
    }
}

/// The kinds [`KnownAnswer`] decodes; any other string `type` is
/// [`Answer::Unknown`].
const KNOWN_KINDS: [&str; 3] = ["noul", "choice", "score"];

/// Longest server-chosen string, in characters once escaped, that reaches an
/// error message, a log line, a span event or a graded judgment.
const SERVER_STR_MAX_CHARS: usize = 64;

/// A string the server chose, made safe to print: control characters, quotes
/// and anything else `escape_debug` escapes are escaped, and the result is
/// cut to 64 characters, with no marker. Without this a hostile or broken
/// server could put a line break or an unbounded string into a log line or
/// an exported span event.
///
/// The one bound for every such string, so they all read the same way: an
/// unknown answer's kind, and an answer's key in the client's warning about
/// it, which is the server's own string when no question by that id was
/// asked.
pub(crate) fn sanitize_server_str(text: &str) -> String {
    text.escape_debug().take(SERVER_STR_MAX_CHARS).collect()
}

/// The known answers, decoded strictly: the derive `Answer` had before
/// `Unknown` existed. [`Answer`]'s `Deserialize` goes through it for a known
/// `type`, and `From` below turns it into the public enum.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum KnownAnswer {
    Noul {
        noul: Probability,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, Probability>,
        confidence: Confidence,
    },
    Score {
        score: f64,
        legend: BTreeMap<String, Value>,
        probabilities: BTreeMap<String, Probability>,
        confidence: Confidence,
    },
}

impl From<KnownAnswer> for Answer {
    /// One arm per variant, each naming every field, so a field added to
    /// [`Answer`] and not to the mirror (or the reverse) does not compile.
    fn from(known: KnownAnswer) -> Self {
        match known {
            KnownAnswer::Noul { noul } => Self::Noul { noul },
            KnownAnswer::Choice {
                choice,
                probabilities,
                confidence,
            } => Self::Choice {
                choice,
                probabilities,
                confidence,
            },
            KnownAnswer::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => Self::Score {
                score,
                legend,
                probabilities,
                confidence,
            },
        }
    }
}

/// Decodes a known `type` strictly and keeps any other string `type` as
/// [`Answer::Unknown`] (module docs, `# Decoding is tolerant, reading is
/// strict`).
///
/// The answer is buffered as a [`Value`] and dispatched on its `type` by
/// hand, rather than derived with an untagged fallback variant. A derived
/// fallback catches every answer the tagged variants refuse, so `{"type":
/// "noul", "noul": 1.2}` would become an unknown answer instead of an error
/// (checked with serde 1.0.229): a broken Noul would be read as a kind this
/// crate does not know, and the remedy the error suggests, upgrading, would
/// be wrong. By hand, a string `type` of `noul`, `choice` or `score` decodes
/// through the strict derive and its errors stand; any other string is
/// `Unknown`; a `type` that is not a string, a missing `type`, and an answer
/// that is not an object are errors.
///
/// Buffering through a [`Value`] means the last of two duplicate keys wins,
/// as it does for every JSON object `serde_json` reads into a map. A
/// duplicate `type` can therefore make a known answer `Unknown`, or the
/// reverse. That is accepted, as `kunobi-jev` accepts it: JSON leaves
/// duplicate keys undefined, and a server that sends them has no one
/// answer to read. A unit test pins the behaviour.
impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let Value::Object(object) = &raw else {
            return Err(de::Error::invalid_type(
                unexpected(&raw),
                &"an answer object",
            ));
        };
        match object.get("type") {
            None => Err(de::Error::missing_field("type")),
            Some(Value::String(kind)) if KNOWN_KINDS.contains(&kind.as_str()) => {
                KnownAnswer::deserialize(raw)
                    .map(Self::from)
                    .map_err(de::Error::custom)
            }
            Some(Value::String(_)) => Ok(Self::Unknown(raw)),
            Some(other) => Err(de::Error::invalid_type(
                unexpected(other),
                &"a string answer type",
            )),
        }
    }
}

/// What serde calls `value` in an "invalid type" message.
fn unexpected(value: &Value) -> Unexpected<'_> {
    match value {
        Value::Null => Unexpected::Unit,
        Value::Bool(b) => Unexpected::Bool(*b),
        Value::Number(n) => n
            .as_u64()
            .map(Unexpected::Unsigned)
            .or_else(|| n.as_i64().map(Unexpected::Signed))
            .unwrap_or_else(|| Unexpected::Float(n.as_f64().unwrap_or(f64::NAN))),
        Value::String(s) => Unexpected::Str(s),
        Value::Array(_) => Unexpected::Seq,
        Value::Object(_) => Unexpected::Map,
    }
}

/// Reads an absent or `null` field as the type's default: a missing
/// `usage`, or a missing or `null` count inside it, is zero.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// Token usage for one request. Output tokens are free; input tokens are billed.
///
/// A count the server did not report reads as zero: an absent or `null`
/// `usage` object ([`Response::usage`]), and an absent or `null` count in
/// it. TypeSafe always reports both counts (the OpenAPI document marks them
/// required), so from a compatible server zero means "not reported", and
/// every consumer of these numbers is a counter or a sum, which a zero
/// leaves right. A missing `usage` object is tolerated beyond both the
/// schema and the Python SDK, whose `SystemOneResponse.usage` has no default
/// although its counts are optional. A negative, fractional or string count
/// is still [`Error::Decode`]; the schema's integer counts have no minimum,
/// so a negative count is valid against it and still refused here. Other
/// keys inside `usage` (`cost`, from `OpenRouter`'s decisions endpoint) are
/// ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    /// Tokens in `state` plus all questions; zero when the server reports
    /// none.
    #[serde(default, deserialize_with = "null_as_default")]
    pub input_tokens: u64,
    /// Tokens in the answers; zero when the server reports none.
    #[serde(default, deserialize_with = "null_as_default")]
    pub output_tokens: u64,
}

/// The full response to one evaluation.
///
/// `model` and `answers` are required, although the Python SDK defaults
/// `answers` to empty: the OpenAPI document requires both, and a response
/// with nothing to read is a server error, not an empty result. Everything
/// else is tolerated (module docs, `# Decoding is tolerant, reading is
/// strict`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The versioned model that answered (e.g. `jev-1.13.0`), even when the
    /// request used an alias. Log it: thresholds are tuned per version.
    pub model: String,
    /// One answer per question id. An answer of a kind this release does not
    /// know is [`Answer::Unknown`], not a failed response.
    pub answers: BTreeMap<String, Answer>,
    /// Token accounting; zero for a count, or a whole `usage`, the server
    /// did not report ([`Usage`]).
    #[serde(default, deserialize_with = "null_as_default")]
    pub usage: Usage,
    /// TypeSafe's request id for the call that produced this response: the
    /// value of the `x-typesafe-request-id` response header (the client's
    /// `REQUEST_ID_HEADER`), the one link from a surprising answer to
    /// TypeSafe's own logs. It is a header, not part of the documented body:
    /// the client sets it from the last attempt's header after decoding, and
    /// overwrites any `request_id` key the body had, with `None` when the
    /// header was absent.
    ///
    /// `None` from a [`crate::Fake`], from recordings made before 0.2, and
    /// from a server that sends no such header (the OpenAPI document lists
    /// no response headers, and a self-hosted server may not send one). It
    /// is serialised only when present, so a [`crate::Recorder`] keeps it, a
    /// [`crate::Replay`] returns the recorded call's id, and a response
    /// without one serialises as it did before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Every top-level field of the body other than `model`, `answers`,
    /// `usage` and `request_id`, as it came: what a server adds beyond the
    /// documented shape, such as Laya's `routing` (which checkpoint
    /// answered) or the `id` and `provider` of `OpenRouter`'s decisions
    /// endpoint. Kept so an operator can log it without a second decoder.
    ///
    /// It is written back at the top level, beside the known fields, so a
    /// recording keeps it, and nothing is written when it is empty, so a
    /// response without extras serialises as it did before the field
    /// existed. The client does not warn about extras: they are expected
    /// from a compatible server. A misspelt field in a hand-written body
    /// lands here too, which is why tests assert it is empty. Fields inside
    /// an answer are not kept.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Response {
    /// Read the answer for `handle` as its typed view.
    ///
    /// Fails with [`Error::MissingAnswer`] if the id is absent,
    /// [`Error::AnswerTypeMismatch`] if the primitive differs from what the
    /// handle was created for, [`Error::UnknownOption`] if a typed Choice
    /// names an option outside the enum, and [`Error::Decode`] if a Score's
    /// legend is not indexed. The module docs explain each. The first three
    /// carry this response's [`Response::request_id`].
    ///
    /// On a response that [`Response::verify`] accepted for the questions
    /// the handle came from, it cannot fail.
    pub fn get<A: FromAnswer>(&self, handle: &crate::Handle<A>) -> Result<A> {
        let id = handle.id();
        let answer = self.answers.get(id).ok_or_else(|| Error::MissingAnswer {
            id: id.to_owned(),
            request_id: self.request_id.clone(),
        })?;
        A::from_answer(id, answer).map_err(|e| e.with_request_id(self.request_id.as_deref()))
    }

    /// Check that this response answers `questions` as they were asked: an
    /// answer under every id, of the question's primitive, a Choice naming
    /// only offered options, a Score on the scale the question sent (module
    /// docs, `# What Response::verify checks`).
    ///
    /// Pure and idempotent: it reads the response and changes nothing. It
    /// walks the questions in id order and returns the first failure, as
    /// [`Error::MissingAnswer`], [`Error::AnswerTypeMismatch`],
    /// [`Error::UnknownOption`] or [`Error::InvalidAnswer`]
    /// ([`Error::is_unfit`]), each carrying this response's request id.
    ///
    /// Every backend in this crate calls it before returning, so a caller of
    /// one never needs to; it is public for a response that did not come
    /// through a backend: one read from a recording by case id
    /// ([`crate::eval::read_recording`]), one decoded by a caller's own
    /// transport, or one built by hand. After it succeeds, [`Response::get`]
    /// with any handle from the same `questions` cannot fail.
    pub fn verify(&self, questions: &Questions) -> Result<()> {
        for (id, question) in questions.iter() {
            self.verify_answer(id, question)?;
        }
        Ok(())
    }

    /// [`Response::verify`] for one question.
    fn verify_answer(&self, id: &str, question: &Question) -> Result<()> {
        let request_id = || self.request_id.clone();
        let Some(answer) = self.answers.get(id) else {
            return Err(Error::MissingAnswer {
                id: id.to_owned(),
                request_id: request_id(),
            });
        };
        match (question, answer) {
            (Question::Noul { .. }, Answer::Noul { .. }) => Ok(()),
            (
                Question::Choice { criteria, .. },
                Answer::Choice {
                    choice,
                    probabilities,
                    ..
                },
            ) => {
                let offered = |key: &&String| criteria.contains_key(key.as_str());
                match std::iter::once(choice)
                    .chain(probabilities.keys())
                    .find(|key| !offered(key))
                {
                    Some(option) => Err(Error::UnknownOption {
                        id: id.to_owned(),
                        option: option.clone(),
                        request_id: request_id(),
                    }),
                    None => Ok(()),
                }
            }
            (
                Question::Score {
                    criteria: levels, ..
                },
                Answer::Score {
                    score,
                    legend,
                    probabilities,
                    ..
                },
            ) => score_fits(levels, *score, legend, probabilities).map_err(|reason| {
                Error::InvalidAnswer {
                    id: id.to_owned(),
                    reason,
                    request_id: request_id(),
                }
            }),
            (question, answer) => {
                Err(mismatch(id, question.kind(), answer)
                    .with_request_id(self.request_id.as_deref()))
            }
        }
    }
}

/// Whether a Score answer is on the scale of `levels`, or why not. The
/// reasons never quote a level's text: it is the caller's own question, it
/// can be long, and the index says which level.
fn score_fits(
    levels: &[Value],
    score: f64,
    legend: &BTreeMap<String, Value>,
    probabilities: &BTreeMap<String, Probability>,
) -> std::result::Result<(), String> {
    let n = levels.len();
    if legend.len() != n {
        return Err(format!(
            "its legend has {} levels but the question sent {n}",
            legend.len()
        ));
    }
    for (i, level) in levels.iter().enumerate() {
        let Some(echoed) = legend.get(&i.to_string()) else {
            return Err(format!("its legend has no level {i}"));
        };
        if !legend_matches(echoed, level) {
            return Err(format!(
                "legend level {i} is not the level the question sent"
            ));
        }
    }
    if let Some(key) = probabilities.keys().find(|key| !is_level_key(key, n)) {
        return Err(format!(
            "probability key {key:?} is not a level of its question"
        ));
    }
    // A question has 2 to 10 levels (`Questions::score`), so the top index
    // is exact as a float.
    #[allow(clippy::cast_precision_loss)]
    let top = n.saturating_sub(1) as f64;
    // `contains` is false for NaN, so a NaN score is off the scale too.
    if !(-SCORE_EPSILON..=top + SCORE_EPSILON).contains(&score) {
        return Err(format!(
            "score {score} is outside 0..={top}, the scale the question sent"
        ));
    }
    Ok(())
}

/// Whether `key` is exactly the decimal index of one of `n` levels: `"0"`
/// to `"n-1"`, with no sign and no leading zero (`"01"` and `"+1"` parse as
/// 1, and are not what a server keys level 1 by).
fn is_level_key(key: &str, n: usize) -> bool {
    key.parse::<usize>()
        .is_ok_and(|i| i < n && i.to_string() == key)
}

/// Whether a legend entry echoes the level the question sent. A string
/// level must come back as that string. A structured level may come back as
/// the same JSON value or as the string of its compact JSON, the label
/// [`Score::levels`] gives it (module docs, `# What Response::verify
/// checks`).
fn legend_matches(echoed: &Value, sent: &Value) -> bool {
    match sent {
        Value::String(_) => echoed == sent,
        other => echoed == other || matches!(echoed, Value::String(s) if *s == level_label(other)),
    }
}

/// Conversion from a wire [`Answer`] into a typed view. Sealed in practice:
/// implemented for [`Noul`], [`Choice<O>`] and [`Score`].
pub trait FromAnswer: Sized {
    /// Primitive name this view expects, for error messages.
    const KIND: &'static str;
    /// Convert, or explain why the answer does not fit.
    fn from_answer(id: &str, answer: &Answer) -> Result<Self>;
}

/// Typed view of a Noul answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Noul {
    /// Probability that the answer is yes.
    pub yes: Probability,
}

impl Noul {
    /// Convenience: `yes >= threshold`.
    pub fn is_yes(self, threshold: f64) -> bool {
        self.yes.at_least(threshold)
    }
}

impl FromAnswer for Noul {
    const KIND: &'static str = "noul";
    fn from_answer(id: &str, answer: &Answer) -> Result<Self> {
        match answer {
            Answer::Noul { noul } => Ok(Self { yes: *noul }),
            other => Err(mismatch(id, Self::KIND, other)),
        }
    }
}

/// Typed view of a Choice answer. `K` is the Rust option type: an enum
/// implementing [`Options`], or `String` for a dynamic choice.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice<K: Eq + Hash> {
    /// The highest-probability option.
    pub chosen: K,
    /// Every option's probability.
    pub probabilities: HashMap<K, Probability>,
    /// Distribution concentration.
    pub confidence: Confidence,
}

impl<K: Eq + Hash> Choice<K> {
    /// Probability of a specific option (0 if absent from the response).
    pub fn probability_of(&self, option: &K) -> f64 {
        self.probabilities.get(option).map_or(0.0, |p| p.value())
    }
}

impl<O: Options> FromAnswer for Choice<O> {
    const KIND: &'static str = "choice";
    fn from_answer(id: &str, answer: &Answer) -> Result<Self> {
        let Answer::Choice {
            choice,
            probabilities,
            confidence,
        } = answer
        else {
            return Err(mismatch(id, Self::KIND, answer));
        };
        let parse = |key: &str| {
            O::from_key(key).ok_or_else(|| Error::UnknownOption {
                id: id.to_owned(),
                option: key.to_owned(),
                request_id: None,
            })
        };
        let chosen = parse(choice)?;
        let probabilities = probabilities
            .iter()
            .map(|(k, p)| Ok((parse(k)?, *p)))
            .collect::<Result<HashMap<O, Probability>>>()?;
        Ok(Self {
            chosen,
            probabilities,
            confidence: *confidence,
        })
    }
}

impl FromAnswer for Choice<String> {
    const KIND: &'static str = "choice";
    fn from_answer(id: &str, answer: &Answer) -> Result<Self> {
        let Answer::Choice {
            choice,
            probabilities,
            confidence,
        } = answer
        else {
            return Err(mismatch(id, Self::KIND, answer));
        };
        Ok(Self {
            chosen: choice.clone(),
            probabilities: probabilities.iter().map(|(k, p)| (k.clone(), *p)).collect(),
            confidence: *confidence,
        })
    }
}

/// Typed view of a Score answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Probability-weighted position, `0.0 ..= levels.len() - 1`.
    pub value: f64,
    /// Level descriptions, lowest first, as echoed by the API. A level sent
    /// as a string is that string; a structured level (an object with
    /// `what` and `examples`, say) is rendered as compact JSON, so a label
    /// is always available for a log line or a note without the caller
    /// re-deriving it from the question.
    pub levels: Vec<String>,
    /// Probability per level, same order as `levels`.
    pub probabilities: Vec<Probability>,
    /// Distribution concentration.
    pub confidence: Confidence,
}

impl Score {
    /// Index of the level nearest to the weighted value.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn nearest_level(&self) -> usize {
        let max = self.levels.len().saturating_sub(1);
        (self.value.round().max(0.0) as usize).min(max)
    }

    /// Description of the nearest level.
    pub fn nearest_label(&self) -> &str {
        self.levels
            .get(self.nearest_level())
            .map_or("", String::as_str)
    }
}

/// The text of a legend entry: a string as is, anything else as compact JSON.
fn level_label(level: &Value) -> String {
    match level {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl FromAnswer for Score {
    const KIND: &'static str = "score";
    fn from_answer(id: &str, answer: &Answer) -> Result<Self> {
        let Answer::Score {
            score,
            legend,
            probabilities,
            confidence,
        } = answer
        else {
            return Err(mismatch(id, Self::KIND, answer));
        };
        // Keys are level indices as strings; order them numerically.
        let mut indexed: Vec<(usize, &Value)> = legend
            .iter()
            .map(|(k, v)| {
                k.parse::<usize>()
                    .map(|i| (i, v))
                    .map_err(|_| Error::Decode {
                        source: serde::de::Error::custom(format!(
                            "score legend key {k:?} is not an index"
                        )),
                        request_id: None,
                    })
            })
            .collect::<Result<_>>()?;
        indexed.sort_by_key(|(i, _)| *i);
        let levels: Vec<String> = indexed.iter().map(|(_, v)| level_label(v)).collect();
        let probs = indexed
            .iter()
            .map(|(i, _)| {
                probabilities
                    .get(&i.to_string())
                    .copied()
                    .unwrap_or(Probability(0.0))
            })
            .collect();
        Ok(Self {
            value: *score,
            levels,
            probabilities: probs,
            confidence: *confidence,
        })
    }
}

/// The error for an answer of another primitive than `expected`, with no
/// request id (the caller that has the response adds it). An unknown
/// answer's kind is the server's own string, so it is escaped and cut
/// ([`sanitize_server_str`]) before it becomes part of a message.
fn mismatch(id: &str, expected: &'static str, actual: &Answer) -> Error {
    let kind = actual.kind();
    Error::AnswerTypeMismatch {
        id: id.to_owned(),
        expected,
        actual: if matches!(actual, Answer::Unknown(_)) {
            sanitize_server_str(kind)
        } else {
            kind.to_owned()
        },
        request_id: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::Questions;
    use serde_json::json;

    crate::options! {
        enum Dept {
            Billing = "billing" => "Money",
            Technical = "technical" => "Bugs",
        }
    }

    fn response(answers: &serde_json::Value) -> Response {
        serde_json::from_value(json!({
            "model": "jev-1.13.0",
            "answers": answers,
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        }))
        .unwrap()
    }

    #[test]
    fn probability_outside_unit_interval_fails_to_deserialise() {
        assert!(serde_json::from_str::<Probability>("1.2").is_err());
        assert!(serde_json::from_str::<Probability>("-0.1").is_err());
        assert!(serde_json::from_str::<Probability>("0.5").is_ok());
    }

    #[test]
    fn typed_choice_maps_back_to_the_enum() {
        let mut q = Questions::new();
        let dept = q.choice::<Dept>("dept", "Which?").unwrap();
        let r = response(&json!({
            "dept": { "type": "choice", "choice": "billing",
                      "probabilities": { "billing": 0.9, "technical": 0.1 }, "confidence": 0.8 }
        }));
        let c = r.get(&dept).unwrap();
        assert_eq!(c.chosen, Dept::Billing);
        assert!((c.probability_of(&Dept::Technical) - 0.1).abs() < 1e-9);
        assert!(c.confidence.at_least(0.8));
    }

    #[test]
    fn unknown_option_is_an_error_not_a_default() {
        let mut q = Questions::new();
        let dept = q.choice::<Dept>("dept", "Which?").unwrap();
        let r = response(&json!({
            "dept": { "type": "choice", "choice": "sales",
                      "probabilities": { "sales": 1.0 }, "confidence": 1.0 }
        }));
        assert!(
            matches!(r.get(&dept), Err(Error::UnknownOption { option, .. }) if option == "sales")
        );
    }

    #[test]
    fn handle_type_is_checked_against_the_answer() {
        let mut q = Questions::new();
        let h = q.noul("x", "?", None).unwrap();
        let r = response(&json!({
            "x": { "type": "score", "score": 1.0, "legend": {"0": "a", "1": "b"},
                   "probabilities": {"0": 0.0, "1": 1.0}, "confidence": 1.0 }
        }));
        assert!(matches!(
            r.get(&h),
            Err(Error::AnswerTypeMismatch {
                expected: "noul",
                actual,
                ..
            }) if actual == "score"
        ));
    }

    #[test]
    fn score_levels_are_ordered_numerically_not_lexically() {
        let mut q = Questions::new();
        let h = q.score("s", "?", vec!["l"; 10]).unwrap();
        let legend: BTreeMap<String, String> =
            (0..10).map(|i| (i.to_string(), format!("L{i}"))).collect();
        let mut probs: BTreeMap<String, f64> = (0..10).map(|i| (i.to_string(), 0.0)).collect();
        probs.insert("9".into(), 1.0);
        let r = response(&json!({
            "s": { "type": "score", "score": 9.0, "legend": legend, "probabilities": probs, "confidence": 1.0 }
        }));
        let s = r.get(&h).unwrap();
        assert_eq!(s.levels[9], "L9");
        assert_eq!(s.nearest_level(), 9);
        assert_eq!(s.nearest_label(), "L9");
    }

    #[test]
    fn a_structured_level_is_echoed_as_json_and_labelled_as_text() {
        // The API accepts a level described as an object and echoes it back
        // in the legend as that object, not as a string; a legend typed as
        // strings fails to decode the whole response.
        let mut q = Questions::new();
        let h = q
            .score(
                "s",
                "?",
                vec![json!({"what": "low", "examples": ["a"]}), json!("high")],
            )
            .unwrap();
        let r = response(&json!({
            "s": { "type": "score", "score": 0.4,
                   "legend": {"0": {"what": "low", "examples": ["a"]}, "1": "high"},
                   "probabilities": {"0": 0.6, "1": 0.4}, "confidence": 0.2 }
        }));
        let s = r.get(&h).unwrap();
        assert_eq!(s.levels, vec![r#"{"examples":["a"],"what":"low"}"#, "high"]);
        assert_eq!(s.nearest_label(), r#"{"examples":["a"],"what":"low"}"#);
    }

    #[test]
    fn a_response_request_id_is_serialised_only_when_present() {
        let mut r = response(&json!({}));
        assert_eq!(r.request_id, None, "absent from the body reads as None");
        let text = serde_json::to_string(&r).unwrap();
        assert!(!text.contains("request_id"), "{text}");
        assert_eq!(serde_json::from_str::<Response>(&text).unwrap(), r);

        r.request_id = Some("req_abc".into());
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value["request_id"], "req_abc");
        assert_eq!(serde_json::from_value::<Response>(value).unwrap(), r);
    }

    #[test]
    fn missing_answer_is_reported_by_id() {
        let mut q = Questions::new();
        let h = q.noul("absent", "?", None).unwrap();
        let r = response(&json!({}));
        assert!(matches!(r.get(&h), Err(Error::MissingAnswer { id, .. }) if id == "absent"));
    }

    /// Decode `body`'s JSON text, the way the client decodes a reply.
    fn decode<T: serde::de::DeserializeOwned>(body: &Value) -> serde_json::Result<T> {
        serde_json::from_str(&body.to_string())
    }

    #[test]
    fn an_unknown_answer_kind_is_kept_raw_and_serialises_back() {
        let raw = json!({ "type": "rank", "ranking": ["b", "a"], "confidence": 0.4 });
        let answer: Answer = decode(&raw).unwrap();
        assert_eq!(answer, Answer::Unknown(raw.clone()));
        assert_eq!(answer.kind(), "rank");
        assert_eq!(serde_json::to_value(&answer).unwrap(), raw);

        // Inside a response, and back out of it, unchanged.
        let r = response(&json!({ "later": raw }));
        assert!(r.extra.is_empty(), "{:?}", r.extra);
        let again: Response = decode(&serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(again, r);
        assert_eq!(
            serde_json::to_value(&again).unwrap()["answers"]["later"],
            raw
        );

        // The known kinds serialise as they always did.
        let noul = Answer::Noul {
            noul: Probability::new(0.25).unwrap(),
        };
        assert_eq!(
            serde_json::to_value(&noul).unwrap(),
            json!({ "type": "noul", "noul": 0.25 })
        );

        // A hand-built Unknown with a known `type` serialises as that kind,
        // and decodes back as the known variant.
        let hand = Answer::Unknown(json!({ "type": "noul", "noul": 0.5 }));
        assert_eq!(hand.kind(), "noul");
        assert_eq!(
            decode::<Answer>(&serde_json::to_value(&hand).unwrap()).unwrap(),
            Answer::Noul {
                noul: Probability::new(0.5).unwrap()
            }
        );
        // Only a hand-built one can lack a string `type`.
        assert_eq!(Answer::Unknown(json!(5)).kind(), "unknown");
        assert_eq!(Answer::Unknown(json!({ "type": 5 })).kind(), "unknown");
    }

    #[test]
    fn a_known_kind_that_does_not_decode_is_still_an_error() {
        let cases = [
            (
                json!({ "type": "noul", "noul": 1.2 }),
                "is not a probability",
            ),
            (
                json!({ "type": "choice", "choice": "billing", "confidence": 0.5 }),
                "missing field `probabilities`",
            ),
            (
                json!({ "type": "score", "score": 1.0, "legend": { "0": "a", "1": "b" },
                        "probabilities": { "0": 0.0, "1": 1.0 } }),
                "missing field `confidence`",
            ),
        ];
        for (bad, message) in cases {
            let err = decode::<Answer>(&bad).unwrap_err();
            assert!(err.to_string().contains(message), "{bad}: {err}");
            // And it fails the response, as before: a broken known answer
            // is not an unknown one.
            let body = json!({ "model": "m", "answers": { "x": bad }, "usage": {} });
            let err = decode::<Response>(&body).unwrap_err();
            assert!(err.to_string().contains(message), "{body}: {err}");
        }
    }

    #[test]
    fn an_answer_without_a_string_type_is_an_error() {
        let cases = [
            (json!({ "noul": 0.5 }), "missing field `type`"),
            (
                json!({ "type": 3 }),
                "invalid type: integer `3`, expected a string answer type",
            ),
            (
                json!({ "type": null, "noul": 0.5 }),
                "invalid type: null, expected a string answer type",
            ),
            (
                json!(0.5),
                "invalid type: floating point `0.5`, expected an answer object",
            ),
            (
                json!(["noul", 0.5]),
                "invalid type: sequence, expected an answer object",
            ),
        ];
        for (bad, message) in cases {
            let err = decode::<Answer>(&bad).unwrap_err();
            assert!(err.to_string().contains(message), "{bad}: {err}");
        }
    }

    #[test]
    fn duplicate_keys_in_an_answer_are_last_one_wins() {
        let answer: Answer =
            serde_json::from_str(r#"{"type":"noul","noul":0.2,"noul":0.9}"#).unwrap();
        assert_eq!(
            answer,
            Answer::Noul {
                noul: Probability::new(0.9).unwrap()
            }
        );
        // A duplicate `type` decides the kind by its last value, either way.
        let answer: Answer =
            serde_json::from_str(r#"{"type":"noul","noul":0.5,"type":"rank"}"#).unwrap();
        assert!(matches!(&answer, Answer::Unknown(_)), "{answer:?}");
        assert_eq!(answer.kind(), "rank");
        let answer: Answer =
            serde_json::from_str(r#"{"type":"rank","noul":0.5,"type":"noul"}"#).unwrap();
        assert_eq!(
            answer,
            Answer::Noul {
                noul: Probability::new(0.5).unwrap()
            }
        );
    }

    #[test]
    fn an_unknown_kind_does_not_fail_the_other_answers() {
        let mut q = Questions::new();
        let dept = q.choice::<Dept>("dept", "Which?").unwrap();
        let order = q.noul("order", "?", None).unwrap();
        let r = response(&json!({
            "dept": { "type": "choice", "choice": "billing",
                      "probabilities": { "billing": 0.9, "technical": 0.1 }, "confidence": 0.8 },
            "order": { "type": "rank", "ranking": ["billing", "technical"] }
        }));
        assert_eq!(r.get(&dept).unwrap().chosen, Dept::Billing);
        let err = r.get(&order).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::AnswerTypeMismatch { id, expected: "noul", actual, .. }
                    if id == "order" && actual == "rank"
            ),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            r#"answer "order" is a rank but a noul was requested"#
        );
    }

    #[test]
    fn a_hostile_unknown_kind_is_escaped_in_the_mismatch() {
        let kind = format!("a\nb{}", "x".repeat(100));
        let r = response(&json!({ "x": { "type": kind } }));
        assert_eq!(r.answers["x"].kind(), kind, "kind() is the raw string");

        let mut q = Questions::new();
        let h = q.noul("x", "?", None).unwrap();
        let err = r.get(&h).unwrap_err();
        let Error::AnswerTypeMismatch { actual, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(!actual.contains('\n'), "{actual:?}");
        assert!(actual.chars().count() <= 64, "{}", actual.chars().count());
        assert!(actual.starts_with(r"a\nb"), "{actual:?}");
        assert!(!err.to_string().contains('\n'), "{err}");
        // A kind that needs no escaping and fits is kept as it is.
        assert_eq!(sanitize_server_str("rank"), "rank");
        assert_eq!(sanitize_server_str(&"r".repeat(64)), "r".repeat(64));
        assert_eq!(sanitize_server_str(&"r".repeat(65)), "r".repeat(64));
    }

    #[test]
    fn usage_is_zero_when_absent_or_null() {
        let bodies = [
            json!({ "model": "m", "answers": {} }),
            json!({ "model": "m", "answers": {}, "usage": null }),
            json!({ "model": "m", "answers": {}, "usage": {} }),
            json!({ "model": "m", "answers": {},
                    "usage": { "input_tokens": null, "output_tokens": null, "cost": 1.7e-5 } }),
        ];
        for body in bodies {
            let r: Response = decode(&body).unwrap();
            assert_eq!(r.usage, Usage::default(), "{body}");
            assert!(r.extra.is_empty(), "{body}: {:?}", r.extra);
        }
        let r: Response = decode(&json!({ "model": "m", "answers": {},
                                          "usage": { "input_tokens": 7 } }))
        .unwrap();
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 7,
                output_tokens: 0
            }
        );
        // Written back, a defaulted usage is explicit zeros.
        assert_eq!(
            serde_json::to_value(r.usage).unwrap(),
            json!({ "input_tokens": 7, "output_tokens": 0 })
        );
    }

    #[test]
    fn negative_or_string_usage_is_still_an_error() {
        for usage in [
            json!({ "input_tokens": -1 }),
            json!({ "input_tokens": "3" }),
            json!({ "output_tokens": 1.5 }),
        ] {
            let body = json!({ "model": "m", "answers": {}, "usage": usage });
            assert!(decode::<Response>(&body).is_err(), "{body}");
        }
        // `model` and `answers` stay required.
        assert!(decode::<Response>(&json!({ "answers": {} })).is_err());
        assert!(decode::<Response>(&json!({ "model": "m" })).is_err());
    }

    #[test]
    fn undocumented_top_level_fields_are_kept_and_written_back() {
        // The shape Laya's server answers in: a `routing` object at the top,
        // and fields inside each answer that the API does not have.
        let laya = json!({
            "model": "laya-rl-agent",
            "answers": {
                "urgent": { "type": "noul", "noul": 0.8123, "confidence": 0.8123,
                            "action": { "act_probability": 0.4 } }
            },
            "usage": { "input_tokens": 120, "output_tokens": 0 },
            "routing": { "model": "typed-decisions", "reason": "explicit" }
        });
        let r: Response = decode(&laya).unwrap();
        assert_eq!(r.extra.keys().collect::<Vec<_>>(), ["routing"]);
        assert_eq!(r.extra["routing"]["model"], "typed-decisions");
        assert_eq!(
            r.answers["urgent"],
            Answer::Noul {
                noul: Probability::new(0.8123).unwrap()
            },
            "fields inside an answer are ignored"
        );
        let written = serde_json::to_value(&r).unwrap();
        assert_eq!(
            written["routing"], laya["routing"],
            "written back at the top"
        );
        assert!(written.get("extra").is_none(), "{written}");
        assert_eq!(decode::<Response>(&written).unwrap(), r);

        // A body in the shape of OpenRouter's decisions endpoint, written for
        // this test: `id` and `provider` at the top, `cost` in the usage,
        // and integer zeros among the probabilities.
        let routed = json!({
            "model": "typesafe/jev-1.13",
            "answers": {
                "department": { "type": "choice", "choice": "billing",
                                "probabilities": { "sales": 0, "billing": 0.9, "technical": 0.1 },
                                "confidence": 0.85 },
                "severity": { "type": "score", "score": 1.1,
                              "legend": { "0": "minor", "1": "major", "2": "outage" },
                              "probabilities": { "0": 0, "1": 0.9, "2": 0.1 },
                              "confidence": 0.8 }
            },
            "usage": { "input_tokens": 400, "output_tokens": 60, "cost": 0.000_02 },
            "id": "gen-dec-0001",
            "provider": "TypeSafe"
        });
        let r: Response = decode(&routed).unwrap();
        assert_eq!(r.extra.keys().collect::<Vec<_>>(), ["id", "provider"]);
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 400,
                output_tokens: 60
            }
        );
        let Answer::Choice { probabilities, .. } = &r.answers["department"] else {
            panic!("{:?}", r.answers["department"]);
        };
        assert!(probabilities["sales"].value().abs() < f64::EPSILON);
        let mut q = Questions::new();
        let severity = q
            .score("severity", "?", ["minor", "major", "outage"])
            .unwrap();
        assert_eq!(r.get(&severity).unwrap().nearest_label(), "major");
        let written = serde_json::to_value(&r).unwrap();
        assert_eq!(written["id"], "gen-dec-0001");
        assert_eq!(written["provider"], "TypeSafe");
        assert_eq!(decode::<Response>(&written).unwrap(), r);

        // A body `request_id` goes to its own field, not to the extras.
        let r: Response =
            decode(&json!({ "model": "m", "answers": {}, "request_id": "req_1" })).unwrap();
        assert_eq!(r.request_id.as_deref(), Some("req_1"));
        assert!(r.extra.is_empty(), "{:?}", r.extra);
        // A response is an object: the array form a derived struct accepted
        // is gone (nothing sent or recorded it).
        assert!(serde_json::from_str::<Response>(r#"["m", {}, {}]"#).is_err());
    }

    // -----------------------------------------------------------------------
    // Response::verify
    // -----------------------------------------------------------------------

    const LEVELS: [&str; 4] = ["none", "minor", "major", "outage"];

    /// One question of each primitive, and a typed Choice, with handles.
    struct Asked {
        q: Questions,
        dept: crate::Handle<Choice<Dept>>,
        owner: crate::Handle<Choice<String>>,
        urgent: crate::Handle<Noul>,
        impact: crate::Handle<Score>,
    }

    fn asked() -> Asked {
        let mut q = Questions::new();
        let dept = q.choice::<Dept>("dept", "Which team?").unwrap();
        let owner = q
            .dynamic_choice(
                "owner",
                "Who owns it?",
                [
                    ("payments".to_owned(), Some("Checkout".to_owned())),
                    ("platform".to_owned(), None),
                    ("none_of_these".to_owned(), None),
                ],
            )
            .unwrap();
        let urgent = q.noul("urgent", "Urgent?", None).unwrap();
        let impact = q.score("impact", "How bad?", LEVELS).unwrap();
        Asked {
            q,
            dept,
            owner,
            urgent,
            impact,
        }
    }

    fn legend() -> Value {
        json!({ "0": "none", "1": "minor", "2": "major", "3": "outage" })
    }

    /// A response that answers [`asked`] as asked.
    fn fitting() -> Value {
        json!({
            "dept": { "type": "choice", "choice": "billing",
                      "probabilities": { "billing": 0.9, "technical": 0.1 }, "confidence": 0.8 },
            "owner": { "type": "choice", "choice": "payments",
                       "probabilities": { "payments": 0.7, "platform": 0.3 }, "confidence": 0.6 },
            "urgent": { "type": "noul", "noul": 0.8 },
            "impact": { "type": "score", "score": 2.0, "legend": legend(),
                        "probabilities": { "0": 0.0, "1": 0.1, "2": 0.8, "3": 0.1 },
                        "confidence": 0.7 }
        })
    }

    /// `fitting()` with `id`'s answer replaced.
    fn with(id: &str, answer: Value) -> Response {
        let mut answers = fitting();
        answers[id] = answer;
        response(&answers)
    }

    fn score_answer(score: f64, legend: &Value, probabilities: &Value) -> Value {
        json!({ "type": "score", "score": score, "legend": legend,
                "probabilities": probabilities, "confidence": 0.5 })
    }

    fn reason(err: &Error) -> &str {
        match err {
            Error::InvalidAnswer { reason, .. } => reason,
            other => panic!("not InvalidAnswer: {other:?}"),
        }
    }

    #[test]
    fn verify_accepts_a_response_that_answers_every_question_as_asked() {
        let Asked {
            q,
            dept,
            owner,
            urgent,
            impact,
        } = asked();
        let mut answers = fitting();
        // An offered option left out of the distribution reads as zero, and
        // an answer to a question nobody asked is ignored.
        answers["owner"]["probabilities"] = json!({ "payments": 0.7, "platform": 0.3 });
        answers["unasked"] = json!({ "type": "rank", "ranking": [] });
        let r = response(&answers);
        r.verify(&q).unwrap();
        r.verify(&q).unwrap();

        // After verify, every read through the same questions' handles works.
        assert_eq!(r.get(&dept).unwrap().chosen, Dept::Billing);
        let owner = r.get(&owner).unwrap();
        assert_eq!(owner.chosen, "payments");
        assert!(owner.probability_of(&"none_of_these".to_owned()).abs() < f64::EPSILON);
        assert!(r.get(&urgent).unwrap().is_yes(0.5));
        let impact = r.get(&impact).unwrap();
        assert_eq!(impact.levels, LEVELS);
        assert_eq!(impact.nearest_label(), "major");
    }

    #[test]
    fn verify_reports_the_first_unanswered_question_in_wire_order() {
        let Asked { q, .. } = asked();
        let mut answers = fitting();
        let map = answers.as_object_mut().unwrap();
        map.remove("owner");
        map.remove("urgent");
        let err = response(&answers).verify(&q).unwrap_err();
        assert!(
            matches!(&err, Error::MissingAnswer { id, request_id: None } if id == "owner"),
            "{err:?}"
        );
        assert!(err.is_unfit());
        assert_eq!(err.to_string(), r#"no answer for question "owner""#);
    }

    #[test]
    fn verify_refuses_an_answer_of_another_kind() {
        let Asked { q, .. } = asked();
        let err = with(
            "urgent",
            json!({ "type": "choice", "choice": "billing",
                                         "probabilities": { "billing": 1.0 }, "confidence": 1.0 }),
        )
        .verify(&q)
        .unwrap_err();
        assert!(
            matches!(
                &err,
                Error::AnswerTypeMismatch { id, expected: "noul", actual, .. }
                    if id == "urgent" && actual == "choice"
            ),
            "{err:?}"
        );
        let err = with("impact", json!({ "type": "noul", "noul": 0.5 }))
            .verify(&q)
            .unwrap_err();
        assert!(
            matches!(&err, Error::AnswerTypeMismatch { expected: "score", actual, .. } if actual == "noul"),
            "{err:?}"
        );
    }

    #[test]
    fn an_unknown_answer_kind_for_an_asked_question_is_a_type_mismatch() {
        let Asked { q, .. } = asked();
        let err = with("urgent", json!({ "type": "rank", "ranking": ["a"] }))
            .verify(&q)
            .unwrap_err();
        assert!(
            matches!(
                &err,
                Error::AnswerTypeMismatch { id, expected: "noul", actual, .. }
                    if id == "urgent" && actual == "rank"
            ),
            "{err:?}"
        );
        // Its kind is escaped and cut, as it is for `get`.
        let hostile = format!("a\nb{}", "x".repeat(100));
        let err = with("urgent", json!({ "type": hostile }))
            .verify(&q)
            .unwrap_err();
        let Error::AnswerTypeMismatch { actual, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(
            !actual.contains('\n') && actual.chars().count() <= 64,
            "{actual:?}"
        );

        // Under an id nobody asked, an unknown answer is only kept.
        let mut answers = fitting();
        answers["later"] = json!({ "type": "rank" });
        response(&answers).verify(&q).unwrap();
    }

    #[test]
    fn verify_refuses_a_chosen_option_the_question_did_not_offer() {
        let Asked { q, .. } = asked();
        let err = with(
            "owner",
            json!({ "type": "choice", "choice": "made-up-team",
                                        "probabilities": { "payments": 0.3 }, "confidence": 0.9 }),
        )
        .verify(&q)
        .unwrap_err();
        assert!(
            matches!(&err, Error::UnknownOption { id, option, .. }
                if id == "owner" && option == "made-up-team"),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            r#"answer "owner" names option "made-up-team", which its question does not offer"#
        );
        // The chosen option is checked before the distribution.
        let err = with("dept", json!({ "type": "choice", "choice": "sales",
                                       "probabilities": { "billing": 0.1, "zzz": 0.9 }, "confidence": 0.9 }))
        .verify(&q)
        .unwrap_err();
        assert!(
            matches!(&err, Error::UnknownOption { id, option, .. } if id == "dept" && option == "sales"),
            "{err:?}"
        );
    }

    #[test]
    fn verify_refuses_a_distribution_key_the_question_did_not_offer() {
        let Asked { q, .. } = asked();
        let err = with(
            "owner",
            json!({ "type": "choice", "choice": "payments",
                                        "probabilities": { "payments": 0.7, "INC-9999": 0.3 },
                                        "confidence": 0.6 }),
        )
        .verify(&q)
        .unwrap_err();
        assert!(
            matches!(&err, Error::UnknownOption { id, option, .. }
                if id == "owner" && option == "INC-9999"),
            "{err:?}"
        );
    }

    #[test]
    fn verify_does_not_check_that_probabilities_sum_to_one() {
        let Asked { q, .. } = asked();
        let short = with(
            "owner",
            json!({ "type": "choice", "choice": "payments",
                                          "probabilities": { "payments": 0.67, "platform": 0.3 },
                                          "confidence": 0.6 }),
        );
        short.verify(&q).unwrap();
        let long = with(
            "impact",
            score_answer(
                2.0,
                &legend(),
                &json!({ "0": 0.1, "1": 0.1, "2": 0.7002, "3": 0.1 }),
            ),
        );
        long.verify(&q).unwrap();
    }

    #[test]
    fn verify_refuses_a_legend_that_is_not_the_levels_sent() {
        let Asked { q, .. } = asked();
        let probs = json!({ "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 });
        let cases = [
            (
                json!({ "0": "none", "1": "minor", "2": "major", "3": "outage", "4": "worse" }),
                "its legend has 5 levels but the question sent 4",
            ),
            (
                json!({ "1": "none", "2": "minor", "3": "major", "4": "outage" }),
                "its legend has no level 0",
            ),
            (
                json!({ "0": "none", "1": "minor", "2": "major (reworded)", "3": "outage" }),
                "legend level 2 is not the level the question sent",
            ),
            (
                json!({ "0": "none", "1": "a", "2": "major", "3": "outage" }),
                "legend level 1 is not the level the question sent",
            ),
        ];
        for (legend, expected) in cases {
            let err = with("impact", score_answer(2.0, &legend, &probs))
                .verify(&q)
                .unwrap_err();
            assert_eq!(reason(&err), expected, "{legend}");
            let message = err.to_string();
            assert!(
                message.starts_with(r#"answer "impact" does not fit its question: "#),
                "{message}"
            );
            for level in LEVELS {
                assert!(!message.contains(level), "{message} quotes {level:?}");
            }
        }
    }

    #[test]
    fn a_structured_level_may_be_echoed_as_itself_or_its_compact_json() {
        let level = json!({ "what": "low", "examples": ["a typo"] });
        let mut q = Questions::new();
        q.score("s", "?", vec![level.clone(), json!("high")])
            .unwrap();
        let answer = |echo: Value| {
            response(&json!({ "s": score_answer(
                0.4,
                &json!({ "0": echo, "1": "high" }),
                &json!({ "0": 0.6, "1": 0.4 }),
            ) }))
        };
        answer(level.clone()).verify(&q).unwrap();
        answer(json!(r#"{"examples":["a typo"],"what":"low"}"#))
            .verify(&q)
            .unwrap();
        let err = answer(json!("low")).verify(&q).unwrap_err();
        assert_eq!(
            reason(&err),
            "legend level 0 is not the level the question sent"
        );
        // A string level is not matched against a structured echo.
        let mut q = Questions::new();
        q.score("s", "?", [r#"{"what":"low"}"#, "high"]).unwrap();
        let err = answer(json!({ "what": "low" })).verify(&q).unwrap_err();
        assert_eq!(
            reason(&err),
            "legend level 0 is not the level the question sent"
        );
    }

    #[test]
    fn verify_refuses_a_probability_key_that_is_not_a_level() {
        let Asked { q, .. } = asked();
        for key in ["4", "01", "+1", "one"] {
            let mut probs = json!({ "0": 0.0, "1": 0.0, "2": 1.0 });
            probs[key] = json!(0.0);
            let err = with("impact", score_answer(2.0, &legend(), &probs))
                .verify(&q)
                .unwrap_err();
            assert_eq!(
                reason(&err),
                format!("probability key {key:?} is not a level of its question")
            );
        }
    }

    #[test]
    fn verify_is_strict_about_the_score_scale() {
        let Asked { q, .. } = asked();
        let probs = json!({ "0": 0.25, "1": 0.25, "2": 0.25, "3": 0.25 });
        for fits in [0.0, 3.0, 1.5, 3.0 + 1e-10, -1e-10] {
            with("impact", score_answer(fits, &legend(), &probs))
                .verify(&q)
                .unwrap();
        }
        for off in [3.001, -0.001] {
            let err = with("impact", score_answer(off, &legend(), &probs))
                .verify(&q)
                .unwrap_err();
            assert_eq!(
                reason(&err),
                format!("score {off} is outside 0..=3, the scale the question sent")
            );
        }
        // NaN never reaches the wire as JSON, but a hand-built response can
        // carry one; it is off the scale too.
        let mut r = with("impact", score_answer(1.0, &legend(), &probs));
        if let Some(Answer::Score { score, .. }) = r.answers.get_mut("impact") {
            *score = f64::NAN;
        }
        let err = r.verify(&q).unwrap_err();
        assert!(reason(&err).contains("outside 0..=3"), "{err}");
    }

    #[test]
    fn verify_and_get_errors_carry_the_responses_request_id() {
        let Asked {
            q, dept, impact, ..
        } = asked();
        let mut r = with(
            "dept",
            json!({ "type": "choice", "choice": "sales",
                                         "probabilities": { "sales": 1.0 }, "confidence": 1.0 }),
        );
        r.request_id = Some("req_unfit".into());
        let err = r.verify(&q).unwrap_err();
        assert_eq!(err.request_id(), Some("req_unfit"));
        assert!(
            err.to_string().ends_with(" [request_id req_unfit]"),
            "{err}"
        );
        let err = r.get(&dept).unwrap_err();
        assert!(matches!(err, Error::UnknownOption { .. }), "{err:?}");
        assert_eq!(err.request_id(), Some("req_unfit"));

        let mut r = with("urgent", json!({ "type": "noul", "noul": 0.5 }));
        r.request_id = Some("req_missing".into());
        r.answers.remove("impact");
        for err in [r.verify(&q).unwrap_err(), r.get(&impact).unwrap_err()] {
            assert!(matches!(err, Error::MissingAnswer { .. }), "{err:?}");
            assert_eq!(err.request_id(), Some("req_missing"));
        }
        let mut other = Questions::new();
        let wrong = other.score("urgent", "?", LEVELS).unwrap();
        let err = r.get(&wrong).unwrap_err();
        assert!(matches!(err, Error::AnswerTypeMismatch { .. }), "{err:?}");
        assert_eq!(err.request_id(), Some("req_missing"));
        // A response with no id gives errors with none.
        r.request_id = None;
        assert_eq!(r.verify(&q).unwrap_err().request_id(), None);
    }
}
