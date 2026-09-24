//! Questions: the three System One primitives, a request builder, and the
//! typed handles that connect each question to the shape of its answer.
//!
//! The API is a map of `id -> Question`; the response is a map of
//! `id -> Answer`. Nothing on the wire ties a Noul question to a Noul answer.
//! [`Questions::noul`], [`Questions::choice`] and [`Questions::score`] return a
//! [`Handle<A>`] whose type parameter records what the answer must be, and
//! [`crate::Response::get`] checks it, so mismatches surface as errors instead
//! of a silently misread number.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::hash::Hash;
use std::marker::PhantomData;

use serde::Serialize;
use serde_json::Value;

use crate::answer::{Choice, Noul, Score};
use crate::error::{Error, Result};

/// Maximum options a Choice may define (API limit).
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Maximum levels a Score may define (API limit).
pub const MAX_SCORE_LEVELS: usize = 10;

/// One question, as sent on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes/no; the answer is the probability of yes.
    Noul {
        /// What to decide. String, object or array.
        instructions: Value,
        /// Optional meaning of yes and no.
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// One option from a defined set.
    Choice {
        /// What to decide.
        instructions: Value,
        /// Option key to rubric description (`null` allowed).
        criteria: BTreeMap<String, Value>,
    },
    /// A position along ordered levels.
    Score {
        /// What to rate.
        instructions: Value,
        /// Ordered level descriptions, lowest first.
        criteria: Vec<Value>,
    },
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

    /// Add a yes/no question.
    pub fn noul(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
        criteria: Option<NoulCriteria>,
    ) -> Result<Handle<Noul>> {
        self.insert(
            id.into(),
            Question::Noul {
                instructions: instructions.into(),
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

    /// Add a Choice whose options are only known at runtime (for example a
    /// set of record ids). The answer is a [`Choice<String>`].
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

    /// Add a Score over ordered levels (2 to 10, lowest first).
    pub fn score(
        &mut self,
        id: impl Into<String>,
        instructions: impl Into<Value>,
        levels: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Result<Handle<Score>> {
        let id = id.into();
        let criteria: Vec<Value> = levels.into_iter().map(Into::into).collect();
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
