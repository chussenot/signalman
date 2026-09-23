//! Answers: wire shapes, validated probabilities, and the typed views that
//! [`Response::get`] produces from a [`crate::Handle`].

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::question::Options;

/// A number in `[0, 1]`, validated on construction and deserialisation.
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

/// Model certainty derived from a Choice or Score distribution. Same domain
/// as [`Probability`] but a distinct type: a confidence is not the probability
/// of any particular outcome.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
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
        /// Level index (as string) to level description.
        legend: BTreeMap<String, String>,
        /// Level index (as string) to probability.
        probabilities: BTreeMap<String, Probability>,
        /// Distribution concentration.
        confidence: Confidence,
    },
}

impl Answer {
    /// Human-readable primitive name, for error messages.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }
}

/// Token usage for one request. Output tokens are free; input tokens are billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    /// Tokens in `state` plus all questions.
    pub input_tokens: u64,
    /// Tokens in the answers.
    pub output_tokens: u64,
}

/// The full response to one evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The versioned model that answered (e.g. `jev-1.13.0`), even when the
    /// request used an alias. Log it: thresholds are tuned per version.
    pub model: String,
    /// One answer per question id.
    pub answers: BTreeMap<String, Answer>,
    /// Token accounting.
    pub usage: Usage,
}

impl Response {
    /// Read the answer for `handle` as its typed view.
    ///
    /// Fails if the id is missing, the primitive differs from what the handle
    /// was created for, or (for a typed Choice) the option is unknown.
    pub fn get<A: FromAnswer>(&self, handle: &crate::Handle<A>) -> Result<A> {
        let id = handle.id();
        let answer = self
            .answers
            .get(id)
            .ok_or_else(|| Error::MissingAnswer(id.to_owned()))?;
        A::from_answer(id, answer)
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
    /// Level descriptions, lowest first, as echoed by the API.
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
        let mut indexed: Vec<(usize, &String)> = legend
            .iter()
            .map(|(k, v)| {
                k.parse::<usize>().map(|i| (i, v)).map_err(|_| {
                    Error::Decode(serde::de::Error::custom(format!(
                        "score legend key {k:?} is not an index"
                    )))
                })
            })
            .collect::<Result<_>>()?;
        indexed.sort_by_key(|(i, _)| *i);
        let levels: Vec<String> = indexed.iter().map(|(_, v)| (*v).clone()).collect();
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

fn mismatch(id: &str, expected: &'static str, actual: &Answer) -> Error {
    Error::AnswerTypeMismatch {
        id: id.to_owned(),
        expected,
        actual: actual.kind(),
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
                actual: "score",
                ..
            })
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
    fn missing_answer_is_reported_by_id() {
        let mut q = Questions::new();
        let h = q.noul("absent", "?", None).unwrap();
        let r = response(&json!({}));
        assert!(matches!(r.get(&h), Err(Error::MissingAnswer(id)) if id == "absent"));
    }
}
