//! Questions: the three System One primitives, a request builder, and the
//! typed handles that connect each question to the shape of its answer.
//!
//! The API is a map of `id -> Question`; the response is a map of
//! `id -> Answer`. Nothing on the wire ties a Noul question to a Noul answer.
//! [`Questions::noul`], [`Questions::choice`] and [`Questions::score`] return a
//! [`Handle<A>`] whose type parameter records what the answer must be, and
//! [`crate::Response::get`] checks it, so mismatches surface as errors instead
//! of a silently misread number.
//!
//! # Ids are for code, not for the model
//!
//! The id is the key the answer comes back under. The API does not send it to
//! the model and does not use it in inference, so an id such as `is_urgent`
//! tells the model nothing. The `instructions` must stand alone as the
//! complete question. When it is about one part of the state, name that part
//! by its backticked path (`` `message` ``, `` `alert.title` ``) so the model
//! knows what to judge.
//!
//! The instructions may be `null`, when the criteria carry the whole
//! question: a Choice whose option descriptions already say what is being
//! decided, a Noul whose meaning of yes and no does. Every builder accepts
//! `()`, [`Value::Null`], `None::<&str>` or an `Option<String>` for them.
//! Leave them null only then; with neither instructions nor criteria a Noul
//! asks the model nothing, and [`Questions::noul`] refuses it, because the
//! answer would still be a confident-looking probability.
//!
//! A null is sent as `"instructions": null`, not left out. The OpenAPI
//! document requires only `type` (and `criteria` for a Choice or a Score)
//! and accepts a null for all three primitives; the HTTP API reference page
//! marks `instructions` required. The Python SDK omits a null field, the JS
//! SDK sends null, and this crate sends null because Laya reads the key
//! unconditionally (`qdef["instructions"]` in its `agent.py`), so a server
//! that follows the reference page or Laya's code still gets the field, and
//! the request hash of a recording does not change.
//!
//! # A Choice should carry a no-match option
//!
//! A Choice answer is always one of the options given, with the probability
//! mass spread over them. When no option fits, the model still has to pick
//! one, and a confident-looking wrong answer is the result. An `other` or
//! `none_of_these` option gives the model a way to say so and gives the code a
//! branch to route on. The [`options!`](crate::options) macro does not add one
//! for you: what "none" means is part of the question's design, and the crate
//! cannot know it for your set.
//!
//! # Static and dynamic option sets
//!
//! [`Questions::choice`] takes a Rust enum implementing [`Options`], so the
//! option keys the API sees and the variants the code matches on are one
//! definition and cannot drift. Some option sets exist only at runtime: the
//! candidate records fetched from another system, the ids of the passages
//! retrieved for a query. [`Questions::dynamic_choice`] takes those as
//! `(key, description)` pairs and returns a handle to a [`Choice<String>`].
//! That loses the enum and keeps everything else: the id check, the
//! primitive check and the validated probabilities.
//!
//! # Limits are checked here
//!
//! The HTTP API reference page allows at most 255 options per Choice, and
//! says a Score "should have at least two levels; the API accepts up to
//! 10". The OpenAPI document TypeSafe publishes is looser: it bounds a
//! Score's levels only below (`minItems: 1`) and a Choice's options not at
//! all. The builder follows the reference page ([`MAX_CHOICE_OPTIONS`],
//! [`MAX_SCORE_LEVELS`], at least 2 levels) and adds a minimum of 2 options,
//! since a Choice of one option decides nothing. It is deliberately stricter
//! than the schema, and it rejects a duplicate id too, before anything is
//! sent: the error names the question, and no round trip, retry or token is
//! spent finding out. What a server does past the reference page's limits
//! (a 422, or an answer) has not been observed, so the stricter bound is the
//! safe one. The cost is that the limits are duplicated here and must follow
//! the API when it changes them; `tests/contract.rs` pins the difference
//! from the schema in both directions, so a refreshed OpenAPI document that
//! adds or moves a bound fails there.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::hash::Hash;
use std::marker::PhantomData;

use serde::Serialize;
use serde_json::Value;

use crate::answer::{Choice, Noul, Score};
use crate::error::{Error, Result};

/// Maximum options a Choice may define: the HTTP API reference page's limit
/// ("a maximum of 255 options per Choice"), checked by the builder so a
/// violation is an error here rather than whatever a server does with it.
/// The OpenAPI document states no maximum; the builder is deliberately the
/// stricter of the two (module docs, `# Limits are checked here`).
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Maximum levels a Score may define: the HTTP API reference page's limit
/// ("the API accepts up to 10"), checked by the builder. The minimum is 2,
/// also the reference page's ("at least two levels"), since one level cannot
/// be a scale; the OpenAPI document says only `minItems: 1` and no maximum
/// (module docs, `# Limits are checked here`).
pub const MAX_SCORE_LEVELS: usize = 10;

/// One question, as sent on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes/no; the answer is the probability of yes.
    Noul {
        /// What to decide. String, object or array; `null` when the criteria
        /// say it all (module docs, `# Ids are for code, not for the model`).
        /// Sent as `null`, never omitted.
        instructions: Value,
        /// Optional meaning of yes and no.
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// One option from a defined set.
    Choice {
        /// What to decide; `null` when the option descriptions say it all.
        /// Sent as `null`, never omitted.
        instructions: Value,
        /// Option key to rubric description (`null` allowed).
        criteria: BTreeMap<String, Value>,
    },
    /// A position along ordered levels.
    Score {
        /// What to rate; `null` when the levels say it all. Sent as `null`,
        /// never omitted.
        instructions: Value,
        /// Ordered level descriptions, lowest first.
        criteria: Vec<Value>,
    },
}

impl Question {
    /// The primitive's wire `type`: `noul`, `choice` or `score`. It is what
    /// the answer to this question must be, and what
    /// [`crate::Response::verify`] names as `expected` when it is not.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }
}

/// What yes and no mean for a Noul question.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct NoulCriteria {
    /// Meaning of a value near 1.
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    /// Meaning of a value near 0.
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

impl NoulCriteria {
    /// Describe both outcomes.
    pub fn new(yes: impl Into<Value>, no: impl Into<Value>) -> Self {
        Self {
            yes: Some(yes.into()),
            no: Some(no.into()),
        }
    }
}

/// The closed set of options for a typed [`Choice`].
///
/// Implement this for a Rust enum (the [`options!`](crate::options) macro writes it for you).
/// The wire `criteria` map is generated from [`Options::ALL`], and the answer's
/// `choice` string is mapped back with [`Options::from_key`], so the Rust type
/// and the API contract cannot disagree.
pub trait Options: Copy + Eq + Hash + std::fmt::Debug + Send + Sync + 'static {
    /// Every option, in a stable order.
    const ALL: &'static [Self];
    /// The key sent to and returned by the API.
    fn key(self) -> &'static str;
    /// The rubric description for this option, or `None` for no detail.
    fn describe(self) -> Option<&'static str>;
    /// Parse a key returned by the API.
    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|o| o.key() == key)
    }
}

/// Define an enum implementing [`Options`] with its wire keys and rubric.
///
/// ```
/// judgment::options! {
///     /// Which team owns first response.
///     pub enum Team {
///         Platform = "platform" => "Kubernetes, CI/CD, internal developer platform",
///         Database = "database" => "PostgreSQL, Redis, storage",
///         None = "none_of_these" => "Not clearly any listed team",
///     }
/// }
/// # use judgment::Options;
/// assert_eq!(Team::from_key("database"), Some(Team::Database));
/// assert_eq!(Team::ALL.len(), 3);
/// ```
#[macro_export]
macro_rules! options {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident = $key:literal => $desc:expr
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $crate::Options for $name {
            const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            fn key(self) -> &'static str {
                match self { $( Self::$variant => $key, )+ }
            }

            fn describe(self) -> Option<&'static str> {
                match self { $( Self::$variant => $crate::question::__desc($desc), )+ }
            }
        }
    };
}

/// Implementation detail of [`options!`]: accepts `&str` or `Option<&str>`.
#[doc(hidden)]
pub fn __desc(d: impl IntoDescription) -> Option<&'static str> {
    d.into_description()
}

/// Implementation detail of [`options!`].
#[doc(hidden)]
pub trait IntoDescription {
    /// Convert to an optional description.
    fn into_description(self) -> Option<&'static str>;
}
impl IntoDescription for &'static str {
    fn into_description(self) -> Option<&'static str> {
        Some(self)
    }
}
impl IntoDescription for Option<&'static str> {
    fn into_description(self) -> Option<&'static str> {
        self
    }
}

/// A typed reference to one question in a request.
///
/// `A` is the answer type ([`Noul`], [`Choice<O>`] or [`Score`]); it is only
/// a compile-time marker, nothing about it is sent on the wire.
#[derive(Debug, Clone)]
pub struct Handle<A> {
    id: String,
    _answer: PhantomData<fn() -> A>,
}

impl<A> Handle<A> {
    /// The question id, as used in the request and response maps.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The `questions` map of one request, built through typed constructors.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct Questions {
    map: BTreeMap<String, Question>,
}

impl Questions {
    /// Empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of questions.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no question was added.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Borrow a question by id.
    pub fn get(&self, id: &str) -> Option<&Question> {
        self.map.get(id)
    }

    /// The question ids, in wire order (sorted): what a backend must answer
    /// and what a test asserts was asked.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// The questions with their ids, in wire order, for a backend or a
    /// harness that needs the questions themselves (a Choice's option keys, a
    /// Score's levels) rather than only their ids.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Question)> {
        self.map.iter().map(|(k, q)| (k.as_str(), q))
    }

    /// Add a yes/no question.
    ///
    /// `instructions` may be null when `criteria` say what yes and no mean;
    /// with neither, the question is refused with [`Error::InvalidQuestion`].
    /// The id is never shown to the model, so such a Noul asks it nothing,
    /// yet the answer would come back as a probability that reads like a
    /// judgment. A Choice and a Score always carry criteria, so they have no
    /// such check.
    pub fn noul(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
        criteria: Option<NoulCriteria>,
    ) -> Result<Handle<Noul>> {
        let id = id.into();
        let instructions = instructions.into();
        if instructions.is_null() && criteria.is_none() {
            return Err(Error::InvalidQuestion {
                id,
                reason: "a Noul needs instructions or criteria: the id is never shown to the model"
                    .to_owned(),
            });
        }
        self.insert(
            id,
            Question::Noul {
                instructions,
                criteria,
            },
        )
    }

    /// Add a Choice over a Rust enum implementing [`Options`].
    pub fn choice<O: Options>(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
    ) -> Result<Handle<Choice<O>>> {
        let id = id.into();
        let criteria: BTreeMap<String, Value> = O::ALL
            .iter()
            .map(|o| {
                (
                    o.key().to_owned(),
                    o.describe().map_or(Value::Null, Value::from),
                )
            })
            .collect();
        validate_choice(&id, &criteria)?;
        self.insert(
            id,
            Question::Choice {
                instructions: instructions.into(),
                criteria,
            },
        )
    }

    /// Add a Choice whose options are only known at runtime: candidate
    /// records fetched from another system, passage ids for a query.
    ///
    /// Options are `(key, description)` pairs; a `None` description sends
    /// `null`, as the API allows. The answer is a [`Choice<String>`] keyed by
    /// those strings, so the check that a typed Choice gets from its enum
    /// (every returned key is one the code knows) is the caller's to do. The
    /// same 2 to [`MAX_CHOICE_OPTIONS`] limit applies and is checked here.
    pub fn dynamic_choice(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
        options: impl IntoIterator<Item = (String, Option<String>)>,
    ) -> Result<Handle<Choice<String>>> {
        let id = id.into();
        let criteria: BTreeMap<String, Value> = options
            .into_iter()
            .map(|(k, d)| (k, d.map_or(Value::Null, Value::from)))
            .collect();
        validate_choice(&id, &criteria)?;
        self.insert(
            id,
            Question::Choice {
                instructions: instructions.into(),
                criteria,
            },
        )
    }

    /// Add a Score over ordered levels, lowest first: 2 to
    /// [`MAX_SCORE_LEVELS`], checked here. The answer is a [`Score`] whose
    /// value is a probability-weighted position on these levels, so the order
    /// given here is the meaning of that number.
    pub fn score(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
        levels: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Result<Handle<Score>> {
        let id = id.into();
        let criteria: Vec<Value> = levels.into_iter().map(Into::into).collect();
        // A level is "described in words", a string or an object; the API
        // has no meaning for a null level and a lenient backend would echo
        // the null into the legend. Refuse it here, where the question id is
        // known, rather than let it fail a round trip.
        if let Some(index) = criteria.iter().position(Value::is_null) {
            return Err(Error::InvalidQuestion {
                id,
                reason: format!("Score level {index} is null; every level needs a description"),
            });
        }
        if criteria.len() < 2 || criteria.len() > MAX_SCORE_LEVELS {
            return Err(Error::InvalidQuestion {
                id,
                reason: format!(
                    "a Score needs between 2 and {MAX_SCORE_LEVELS} levels, got {}",
                    criteria.len()
                ),
            });
        }
        self.insert(
            id,
            Question::Score {
                instructions: instructions.into(),
                criteria,
            },
        )
    }

    fn insert<A>(&mut self, id: String, question: Question) -> Result<Handle<A>> {
        match self.map.entry(id.clone()) {
            Entry::Occupied(_) => Err(Error::DuplicateQuestionId(id)),
            Entry::Vacant(slot) => {
                slot.insert(question);
                Ok(Handle {
                    id,
                    _answer: PhantomData,
                })
            }
        }
    }
}

fn validate_choice(id: &str, criteria: &BTreeMap<String, Value>) -> Result<()> {
    if criteria.len() < 2 {
        return Err(Error::InvalidQuestion {
            id: id.to_owned(),
            reason: "a Choice needs at least 2 options".to_owned(),
        });
    }
    if criteria.len() > MAX_CHOICE_OPTIONS {
        return Err(Error::InvalidQuestion {
            id: id.to_owned(),
            reason: format!("a Choice allows at most {MAX_CHOICE_OPTIONS} options"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    options! {
        enum Colour {
            Red = "red" => "Warm",
            Blue = "blue" => None,
        }
    }

    #[test]
    fn choice_criteria_come_from_the_enum() {
        let mut q = Questions::new();
        let h = q.choice::<Colour>("colour", "Which colour?").unwrap();
        assert_eq!(h.id(), "colour");
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json["colour"],
            serde_json::json!({
                "type": "choice",
                "instructions": "Which colour?",
                "criteria": { "red": "Warm", "blue": null }
            })
        );
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut q = Questions::new();
        q.noul("a", "x?", None).unwrap();
        assert!(matches!(
            q.noul("a", "y?", None),
            Err(Error::DuplicateQuestionId(id)) if id == "a"
        ));
    }

    #[test]
    fn score_level_bounds_are_enforced() {
        let mut q = Questions::new();
        assert!(q.score("one", "?", ["only"]).is_err());
        assert!(q.score("many", "?", vec!["l"; 11]).is_err());
        assert!(q.score("ok", "?", ["low", "high"]).is_ok());
    }

    #[test]
    fn a_null_score_level_is_refused_before_the_wire() {
        let mut q = Questions::new();
        let err = q
            .score("s", "?", vec![json!("low"), json!(null), json!("high")])
            .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidQuestion { id, reason } if id == "s" && reason.contains("level 1 is null")),
            "{err}"
        );
    }

    #[test]
    fn null_instructions_are_sent_as_null() {
        let mut q = Questions::new();
        q.choice::<Colour>("unit", ()).unwrap();
        q.score("none", None::<&str>, ["low", "high"]).unwrap();
        q.dynamic_choice(
            "value",
            Value::Null,
            [
                ("a".to_owned(), Some("A".to_owned())),
                ("b".to_owned(), None),
            ],
        )
        .unwrap();
        q.noul(
            "option",
            None::<String>,
            Some(NoulCriteria::new("spam", "not spam")),
        )
        .unwrap();
        let json = serde_json::to_value(&q).unwrap();
        for id in ["unit", "none", "value", "option"] {
            let question = json[id].as_object().unwrap();
            assert_eq!(
                question.get("instructions"),
                Some(&Value::Null),
                "{id}: the key is present and null"
            );
        }
        assert_eq!(
            serde_json::to_string(&json["none"]).unwrap(),
            r#"{"criteria":["low","high"],"instructions":null,"type":"score"}"#
        );
    }

    #[test]
    fn a_noul_with_neither_instructions_nor_criteria_is_refused() {
        let mut q = Questions::new();
        for (id, instructions) in [("unit", Value::from(())), ("null", Value::Null)] {
            let err = q.noul(id, instructions, None).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidQuestion { id: got, reason }
                    if got == id && reason == "a Noul needs instructions or criteria: the id is never shown to the model"),
                "{err}"
            );
        }
        assert!(q.is_empty(), "a refused question is not added");

        // Criteria alone carry the question: accepted.
        q.noul(
            "spam",
            (),
            Some(NoulCriteria::new(
                "unsolicited advertising",
                "a real message",
            )),
        )
        .unwrap();
        // Instructions alone: accepted, as always.
        q.noul("urgent", "Is `message` urgent?", None).unwrap();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn question_kind_names_the_primitive() {
        let mut q = Questions::new();
        q.noul("n", "?", None).unwrap();
        q.choice::<Colour>("c", "?").unwrap();
        q.score("s", "?", ["low", "high"]).unwrap();
        let json = serde_json::to_value(&q).unwrap();
        for (id, question) in q.iter() {
            // The kind is the `type` the question is sent with.
            assert_eq!(json[id]["type"], question.kind(), "{id}");
        }
        let kinds: Vec<&str> = q.iter().map(|(_, question)| question.kind()).collect();
        assert_eq!(kinds, ["choice", "noul", "score"]);
    }

    #[test]
    fn noul_criteria_serialise_with_true_false_keys() {
        let mut q = Questions::new();
        q.noul(
            "u",
            "Urgent?",
            Some(NoulCriteria::new("time-sensitive", "no urgency")),
        )
        .unwrap();
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["u"]["criteria"]["true"], "time-sensitive");
        assert_eq!(json["u"]["criteria"]["false"], "no urgency");
    }
}
