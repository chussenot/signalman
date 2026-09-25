//! One error type for the client and the typed answer layer.
//!
//! A caller has to pick a remedy from the error alone, so the variants are
//! grouped by what fixes them rather than by HTTP status:
//!
//! * Configuration: [`Error::MissingApiKey`], [`Error::InvalidApiKey`],
//!   [`Error::Unauthorized`] and [`Error::PermissionDenied`]. Fix the key, or
//!   the account's access for a 403; no retry helps.
//! * Request: [`Error::InvalidRequest`] (the API's 400 or 422, with the
//!   server's message and the fields it names as [`ValidationIssue`]s),
//!   [`Error::InvalidQuestion`] and [`Error::DuplicateQuestionId`] (caught by
//!   the builder before anything is sent), [`Error::ReservedHeader`] and
//!   [`Error::ReservedField`] (a per-call option or a default header that
//!   would replace something the client sets itself, refused before anything
//!   is sent) and [`Error::Url`]. Fix the request; no retry helps either.
//! * Transient, retries exhausted: [`Error::RateLimited`] (429, carrying the
//!   server's `Retry-After` when it sent one) and [`Error::Overloaded`] (529,
//!   which carries no header). Both are returned only after the retry policy
//!   ran out and both carry the attempt count. They are kept apart because
//!   the remedies differ: 429 is the account's rate limit and the remedy is
//!   to slow down; 529 is TypeSafe's capacity and the remedy is to wait.
//! * Transport and decode: [`Error::Transport`] (network, TLS or timeout,
//!   after retries), [`Error::Http`] (any other non-success status, body
//!   truncated) and [`Error::Decode`] (a body that is not the documented
//!   shape). Look at the network, the base URL and the API changelog. An
//!   answer of a kind this release does not know is not a decode error: it
//!   is kept as [`crate::Answer::Unknown`], and reading it is the
//!   [`Error::AnswerTypeMismatch`] below.
//! * An answer that does not fit its question: [`Error::MissingAnswer`],
//!   [`Error::AnswerTypeMismatch`], [`Error::UnknownOption`] and
//!   [`Error::InvalidAnswer`] ([`Error::is_unfit`] is true for these four).
//!   [`crate::Response::verify`] raises them for a response that does not
//!   answer the questions it was sent, and every backend in this crate
//!   verifies before it returns, so from the client they mean the server
//!   answered something else than it was asked: report it with the request
//!   id they carry. Reading through a handle raises them too, when the
//!   handle, the option set or a recording does not match the answer; fix
//!   the code or record again. When the answer is of a kind this release
//!   does not know, upgrade the crate instead. [`Error::NotAProbability`] is
//!   a value outside `[0, 1]` on the wire or in a [`crate::Fake`].
//! * Recordings: [`Error::Io`] and [`Error::NoRecording`]. Fix the path, or
//!   record the request before replaying it.
//!
//! Reading and writing recording files by case id, in [`crate::eval`], has
//! its own smaller [`crate::eval::Error`]: a harness handles a missing file
//! differently from a missing answer, and the backends map it into this type
//! where the two meet.
//!
//! # Request id
//!
//! Every variant built from an HTTP response carries TypeSafe's
//! `x-typesafe-request-id` when the response had one: [`Error::Unauthorized`],
//! [`Error::PermissionDenied`], [`Error::InvalidRequest`],
//! [`Error::RateLimited`], [`Error::Overloaded`], [`Error::Http`] and a
//! [`Error::Decode`] of a 2xx body. So do the four errors of an answer that
//! does not fit its question, which carry the id of the response they were
//! read from ([`crate::Response::request_id`]): a 2xx that answered
//! something else than it was asked is a call TypeSafe can look up. It is
//! the one link from a failure to TypeSafe's own logs, so it is on the value
//! ([`Error::request_id`]) and at the end of the message (` [request_id …]`),
//! where a log line that keeps only the message still has it. It is the last
//! attempt's id and optional, because the API does not promise the header.
//! [`Error::Transport`] never has one, and the variants raised before a
//! request is sent have none either.
//!
//! The enum is `#[non_exhaustive]`: new variants may arrive in minor
//! releases, so a `match` outside the crate needs a wildcard arm.

use std::time::Duration;

/// Everything that can go wrong talking to TypeSafe or reading its answers.
///
/// Non-exhaustive: new variants may arrive in minor releases, so a `match`
/// outside this crate needs a wildcard arm. The variants themselves are not,
/// so their fields can be matched and built as they stand.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No usable key: none was passed to the builder and `TYPESAFE_API_KEY`
    /// is unset or blank, or the key passed to the builder is blank.
    ///
    /// A blank key passed explicitly lands here and never falls back to the
    /// environment, as in the Python SDK (0.7.1): a caller that passed a key
    /// meant that key, and silently using another one from the environment
    /// would authenticate as someone else. A key is blank when nothing is
    /// left after trimming the characters Python's `str.isspace` accepts.
    #[error("no API key: pass a non-blank key to the client builder, or set TYPESAFE_API_KEY")]
    MissingApiKey,
    /// The key cannot be a TypeSafe key: it has whitespace inside it, a
    /// control character or a non-ASCII character (a byte-order mark
    /// included), or `TYPESAFE_API_KEY` is not valid UTF-8. Refused when the
    /// client is built, before anything is sent.
    ///
    /// The rule is the Python SDK's (0.7.1): printable ASCII with no space,
    /// after trimming. Refusing here turns a copy-paste that picked up a
    /// non-breaking space, or a key file with a stray tab, into an error at
    /// start-up that names the cause, instead of a 401 on the first call or
    /// a key that cannot be put in a header at all. The cost is that this
    /// crate, like that SDK, refuses a key a lenient server might have
    /// accepted; the JS SDK has no such check. `reason` names where the key
    /// came from and what is wrong with it, and never contains any part of
    /// the key, so the message is safe to log.
    #[error("invalid API key: {reason}")]
    InvalidApiKey {
        /// Where the key came from and what is wrong, without the key.
        reason: String,
    },
    /// The API rejected the key (HTTP 401). Not retried: a retry cannot fix
    /// a key. Check that the key is the right one and still valid.
    #[error("authentication failed (401): check the API key{}", request_id_suffix(.request_id.as_deref()))]
    Unauthorized {
        /// TypeSafe's `x-typesafe-request-id`, when the response had one.
        request_id: Option<String>,
    },
    /// The API accepted the key but refused the call (HTTP 403). Not
    /// retried: the account lacks access to what was asked, a model for
    /// instance, and a second attempt is refused the same way.
    ///
    /// It is kept apart from [`Error::Unauthorized`] because the remedy
    /// differs: a new key does not help, the account's access does. Both
    /// official SDKs name this status `PermissionDenied` too. `detail` is the
    /// server's message, read from the body as for [`Error::InvalidRequest`].
    #[error("permission denied (403): {detail}{}", request_id_suffix(.request_id.as_deref()))]
    PermissionDenied {
        /// The server's message, or the body truncated when it has none.
        detail: String,
        /// TypeSafe's `x-typesafe-request-id`, when the response had one.
        request_id: Option<String>,
    },
    /// The API refused the request body (HTTP 400 or 422). Not retried: a
    /// retry cannot fix a body. Fix the question or the state the issues
    /// point at.
    ///
    /// Both statuses mean the same thing to the caller: the API reference
    /// documents a 422 for a body that fails validation, both official SDKs
    /// have a bad-request error for a 400, and a compatible server such as
    /// Laya's answers 400 for a body it cannot use. They share the variant,
    /// and `status` tells them apart. `detail` is a
    /// readable summary rather than the raw body: the server's own message
    /// when it sent one, otherwise the parsed issues joined as
    /// `path: msg; …`, otherwise the body itself, truncated. `issues` keeps
    /// the fields a validation body names, so code can point at the
    /// question at fault without parsing the message. The value echoed back
    /// under each issue's `input` is dropped, since it can be a piece of the
    /// state.
    #[error("request rejected by the API ({status}): {detail}{}", request_id_suffix(.request_id.as_deref()))]
    InvalidRequest {
        /// HTTP status: 400 or 422.
        status: u16,
        /// The server's message, the issues joined, or the body truncated.
        detail: String,
        /// The validation issues the body listed; empty when it listed none.
        issues: Vec<ValidationIssue>,
        /// TypeSafe's `x-typesafe-request-id`, when the response had one.
        request_id: Option<String>,
    },
    /// Rate limited (HTTP 429), returned after the retry policy stopped
    /// (retries used up, the budget reached, or a status or transport
    /// failure the policy does not retry). The remedy is to slow down: wait
    /// `retry_after` when the server gave one, and lower the request rate or
    /// raise the account's limit if it keeps happening.
    #[error(
        "rate limited (429) after {attempts} attempts{}{}",
        crate::error::retry_after_suffix(*.retry_after),
        request_id_suffix(.request_id.as_deref())
    )]
    RateLimited {
        /// Total attempts made, including the first.
        attempts: u32,
        /// The last response's wait, when it named one: from
        /// `retry-after-ms`, or `Retry-After` in seconds or as a date
        /// (`http::parse_retry_after`). It can be longer than any wait the
        /// policy took, when the policy's cap or budget refused it.
        retry_after: Option<Duration>,
        /// The last response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// TypeSafe overloaded (HTTP 529), returned after the retry policy
    /// stopped (retries used up, the budget reached, or a status or
    /// transport failure the policy does not retry). Nothing on the caller's
    /// side is wrong; wait and try again later.
    #[error("service overloaded (529) after {attempts} attempts{}", request_id_suffix(.request_id.as_deref()))]
    Overloaded {
        /// Total attempts made, including the first.
        attempts: u32,
        /// The last response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// Any other non-success HTTP status: a proxy in the way, a base URL
    /// that is not the API, or a status the API did not have when this crate
    /// was written. The truncated body says which.
    #[error("unexpected HTTP status {status}: {body}{}", request_id_suffix(.request_id.as_deref()))]
    Http {
        /// Status code.
        status: u16,
        /// Response body, truncated.
        body: String,
        /// The last response's `x-typesafe-request-id`, when it had one. A
        /// proxy or a server that is not the API usually sends none.
        request_id: Option<String>,
    },
    /// Network failure, TLS failure, timeout or a body that could not be
    /// read, returned after the retry policy stopped (retries used up, the
    /// budget reached, or a status or transport failure the policy does not
    /// retry). Check connectivity, the base URL and the per-attempt timeout;
    /// `attempts` says how many times it was tried.
    ///
    /// It never carries a request id: either no response came back, or its
    /// body could not be read and the shared loop drops that response's
    /// headers, as the SDKs do.
    #[cfg(feature = "http")]
    #[error("transport error after {attempts} attempts: {source}")]
    Transport {
        /// Total attempts made, including the first.
        attempts: u32,
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
    /// The response was not the JSON shape the API documents, or a
    /// recording was not one. The API changed, the base URL points at
    /// something else, or the file is corrupt; the message says where.
    ///
    /// Decoding is tolerant of what the API may add (`answer` module docs,
    /// `# Decoding is tolerant, reading is strict`), so some bodies that
    /// used to land here no longer do: an answer of a kind this release does
    /// not know is [`crate::Answer::Unknown`], a missing `usage` or count is
    /// zero, and an undocumented top-level field is kept in
    /// [`crate::Response::extra`]. What still lands here: a known answer
    /// that breaks its own shape (a Noul of 1.2, a Choice without
    /// `probabilities`), an answer with no `type` or a `type` that is not a
    /// string, a missing `model` or `answers`, and a count that is negative,
    /// fractional or a string.
    ///
    /// A 2xx whose body does not decode is still an HTTP response, so it
    /// keeps that response's `x-typesafe-request-id`, as the Python SDK's
    /// `TypeSafeAPIResponseValidationError` does; a recording, a state that
    /// does not serialise or a legend read from an answer has none. It is
    /// not retried: the loop retries by status, and a 2xx is final. `?` on
    /// a [`serde_json::Error`] builds this variant with no id.
    #[error("could not decode API response: {source}{}", request_id_suffix(.request_id.as_deref()))]
    Decode {
        /// What serde could not read.
        #[source]
        source: serde_json::Error,
        /// The response's `x-typesafe-request-id`, when the body came from an
        /// HTTP response that had one.
        request_id: Option<String>,
    },
    /// A recording could not be read or written. Check the directory and its
    /// permissions; `context` names what was being accessed.
    #[error("cannot access {context}: {source}")]
    Io {
        /// What was being accessed.
        context: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A replay had no recording for the request. Run it once through a
    /// [`crate::Recorder`] over the same directory, then replay.
    #[error("no recording for request {0}; record it first")]
    NoRecording(String),
    /// A question id was added twice to one request. Rename one; ids are the
    /// keys answers come back under, so they must be unique.
    #[error("duplicate question id {0:?}")]
    DuplicateQuestionId(String),
    /// A question was built with criteria the API would reject: too few or
    /// too many options or levels. Caught before sending; `reason` says which
    /// limit.
    #[error("invalid question {id:?}: {reason}")]
    InvalidQuestion {
        /// Question id.
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// The response has no answer under a question's id. From
    /// [`crate::Response::verify`], and so from every backend in this crate,
    /// it means the server left a question it was sent unanswered (or a
    /// [`crate::Fake`] was built without that answer): report it with the
    /// request id. From [`crate::Response::get`] it can also mean the handle
    /// belongs to a different request.
    #[error("no answer for question {id:?}{}", request_id_suffix(.request_id.as_deref()))]
    MissingAnswer {
        /// Question id.
        id: String,
        /// The response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// The answer under this id is a different primitive than its question
    /// or its handle: the server answered another kind than it was asked
    /// (report it with the request id), the handle was made for another
    /// question with the same id, or a recording was made with a different
    /// question set (fix the code or record again).
    ///
    /// It is also how an answer of a kind this release does not know
    /// ([`crate::Answer::Unknown`]) reads under a question that was asked:
    /// `actual` is then the server's own `type`, escaped and cut to 64
    /// characters so it cannot break a log line. The remedy is to upgrade
    /// this crate, if the API has a primitive it does not know yet, or to
    /// check what the server answers, if it is not the API.
    #[error(
        "answer {id:?} is a {actual} but a {expected} was requested{}",
        request_id_suffix(.request_id.as_deref())
    )]
    AnswerTypeMismatch {
        /// Question id.
        id: String,
        /// Primitive the question or handle expected.
        expected: &'static str,
        /// The kind the server returned: `noul`, `choice`, `score`, or an
        /// unknown kind's escaped `type`.
        actual: String,
        /// The response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// A Choice answer named an option its question did not offer, as the
    /// chosen option or as a key of its distribution. Raised by
    /// [`crate::Response::verify`] against the options the question was
    /// sent with, and by [`crate::Response::get`] for a key outside a typed
    /// Choice's enum.
    ///
    /// It is an error, not a guess: an option nobody offered names nothing
    /// the code can act on, and reading it as some other option would decide
    /// on an answer the model did not give. From the client it means the
    /// server answered a different question; report it with the request id.
    /// From `get` on a recording, the enum changed since it was recorded;
    /// fix the enum or record again.
    #[error(
        "answer {id:?} names option {option:?}, which its question does not offer{}",
        request_id_suffix(.request_id.as_deref())
    )]
    UnknownOption {
        /// Question id.
        id: String,
        /// The option string the API returned.
        option: String,
        /// The response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// The answer is of the right primitive but does not describe the
    /// question it answers. [`crate::Response::verify`] raises it for a
    /// Score that is not on the scale the question sent: a legend with
    /// another number of levels or another level text, a probability keyed
    /// by something that is not a level, or a score off the ends of the
    /// scale. From the client, report it with the request id. An application
    /// may raise it for its own reading of an answer too, so its errors share
    /// one type with the crate's.
    #[error(
        "answer {id:?} does not fit its question: {reason}{}",
        request_id_suffix(.request_id.as_deref())
    )]
    InvalidAnswer {
        /// Question id.
        id: String,
        /// What does not fit. It never quotes a level's text, which is the
        /// caller's own question and can be long.
        reason: String,
        /// The response's `x-typesafe-request-id`, when it had one.
        request_id: Option<String>,
    },
    /// A probability or confidence was outside `[0, 1]`: the wire sent one,
    /// or a [`crate::Fake`] was built with one. The value is reported.
    #[error("value {value} is not a probability in [0, 1]")]
    NotAProbability {
        /// The offending value.
        value: f64,
    },
    /// A header the client sets itself was given as a per-call header
    /// ([`crate::client::CallOptions::header`]) or a default header
    /// ([`crate::client::ClientBuilder::default_header`]); the value is its
    /// lowercase name. Refused before anything is sent, so no attempt is
    /// made or counted.
    ///
    /// The reserved headers are `authorization`, `content-type`,
    /// `user-agent` and `x-typesafe-retry-count`. The first three are the
    /// client's own: a per-call `authorization` would really replace the key,
    /// and the other two describe the body and the client. The last one is
    /// not sent by this release, but both official SDKs own it and strip a
    /// caller's value, so it is reserved now and sending it later breaks no
    /// caller. To use another key, build another client with it
    /// ([`crate::client::ClientBuilder::api_key`]).
    #[error("header {0:?} is set by the client and cannot be overridden")]
    ReservedHeader(String),
    /// A body field the client sets itself (`state`, `model` or
    /// `questions`) was given as a per-call extra field
    /// ([`crate::client::CallOptions::extra`]); the value is the name.
    /// Refused before anything is sent, so no attempt is made or counted.
    ///
    /// Replacing `questions` or `state` would send a request the response is
    /// not read against and a recording is not keyed by, and replacing
    /// `model` would make the span name a model that was not asked for. Put
    /// the model on the [`crate::client::Request`], or send another request.
    /// The names are matched exactly, so `Model` is an extra field like any
    /// other.
    #[error("body field {0:?} is set by the client and cannot be an extra field")]
    ReservedField(String),
    /// The base URL does not parse or cannot be joined with a path. Fix the
    /// URL given to the builder.
    #[error("invalid URL: {0}")]
    Url(String),
}

impl From<serde_json::Error> for Error {
    /// A serde failure with no response behind it: no request id.
    fn from(source: serde_json::Error) -> Self {
        Self::Decode {
            source,
            request_id: None,
        }
    }
}

impl Error {
    /// TypeSafe's `x-typesafe-request-id` for the response this error came
    /// from, when there was one and it carried the header: the id to quote
    /// to TypeSafe support. The four errors of an answer that does not fit
    /// its question carry the id of the response they were read from. `None`
    /// for an error raised before anything was sent, for one about a
    /// recording, or on a transport failure.
    ///
    /// The match names every variant, so a new one has to choose.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Unauthorized { request_id }
            | Self::PermissionDenied { request_id, .. }
            | Self::InvalidRequest { request_id, .. }
            | Self::RateLimited { request_id, .. }
            | Self::Overloaded { request_id, .. }
            | Self::Http { request_id, .. }
            | Self::Decode { request_id, .. }
            | Self::MissingAnswer { request_id, .. }
            | Self::AnswerTypeMismatch { request_id, .. }
            | Self::UnknownOption { request_id, .. }
            | Self::InvalidAnswer { request_id, .. } => request_id.as_deref(),
            #[cfg(feature = "http")]
            Self::Transport { .. } => None,
            Self::MissingApiKey
            | Self::InvalidApiKey { .. }
            | Self::Io { .. }
            | Self::NoRecording(_)
            | Self::DuplicateQuestionId(_)
            | Self::InvalidQuestion { .. }
            | Self::ReservedHeader(_)
            | Self::ReservedField(_)
            | Self::NotAProbability { .. }
            | Self::Url(_) => None,
        }
    }

    /// This error with the request id of the response it was read from, when
    /// it is one of the four answer-fit errors ([`Error::is_unfit`]) and has
    /// none yet; any other error is returned as it is. The typed views read
    /// an answer without its response, so [`crate::Response::get`] adds the
    /// id here.
    pub(crate) fn with_request_id(mut self, id: Option<&str>) -> Self {
        if let Self::MissingAnswer { request_id, .. }
        | Self::AnswerTypeMismatch { request_id, .. }
        | Self::UnknownOption { request_id, .. }
        | Self::InvalidAnswer { request_id, .. } = &mut self
            && request_id.is_none()
        {
            *request_id = id.map(str::to_owned);
        }
        self
    }

    /// True when the response did not fit the questions it answers:
    /// [`Error::MissingAnswer`], [`Error::AnswerTypeMismatch`],
    /// [`Error::UnknownOption`] or [`Error::InvalidAnswer`].
    ///
    /// These are what [`crate::Response::verify`] raises, so from a backend
    /// they mean the call itself went through and its answer cannot be
    /// used, as opposed to a call that failed (a transport error, an HTTP
    /// status) or a request that was never sent. A caller that runs many
    /// independent calls, such as an evaluation harness, uses this to record
    /// the case as failed, with the request id, and carry on with the next
    /// one, where any other error stops the run. It is false for
    /// [`Error::Decode`]: a body that does not decode is not an answer at
    /// all.
    pub fn is_unfit(&self) -> bool {
        matches!(
            self,
            Self::MissingAnswer { .. }
                | Self::AnswerTypeMismatch { .. }
                | Self::UnknownOption { .. }
                | Self::InvalidAnswer { .. }
        )
    }
}

/// One invalid value a 400 or 422 body named: an entry of the `detail` list
/// in the OpenAPI document's `HTTPValidationError`, which is what `FastAPI`
/// sends when a body fails validation.
///
/// The server's message says the same thing in prose; this keeps it as data,
/// so code can point at the question at fault, and a person reading a log
/// line gets a dotted path instead of a JSON array. The entry's `input` (the
/// offending value, echoed back) and `ctx` are not kept: `input` can be a
/// piece of the state, and neither is needed to find the field.
///
/// For a question error the path runs `questions.<id>.<type>.<field>`, the
/// question id second and its type third (`questions.urgency.score.criteria`
/// in the OpenAPI document's example), because `FastAPI` puts the tag of the
/// question's discriminated union in the location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Where the invalid value is, as the server sent it: the request
    /// location (`body`) followed by field names and array indices. An
    /// integer segment is kept as its decimal string, so an array index and
    /// a map key that happens to be numeric read the same.
    pub loc: Vec<String>,
    /// The server's explanation, for example `Field required`.
    pub msg: String,
    /// The server's machine-readable code (its `type`), for example
    /// `missing`; empty when the entry had none.
    pub kind: String,
}

impl ValidationIssue {
    /// The location as a dotted path, without the leading `body` segment
    /// every body error starts with: `questions.urgency.score.criteria`.
    ///
    /// Only a leading `body` is dropped. The Python SDK drops every `body`
    /// segment, which would also remove a question whose id is `body`.
    pub fn path(&self) -> String {
        let segments = match self.loc.split_first() {
            Some((first, rest)) if first == "body" => rest,
            _ => self.loc.as_slice(),
        };
        segments.join(".")
    }
}

impl std::fmt::Display for ValidationIssue {
    /// `path: msg`, or `msg` alone when the location is empty.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self.path();
        if path.is_empty() {
            f.write_str(&self.msg)
        } else {
            write!(f, "{path}: {}", self.msg)
        }
    }
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The request id clause of an error message: ` [request_id <id>]`, or
/// empty when there is none, so a message without an id reads as it did
/// before ids were carried.
fn request_id_suffix(id: Option<&str>) -> String {
    id.map(|id| format!(" [request_id {id}]"))
        .unwrap_or_default()
}

/// The server's-wait clause of a rate-limit error message (from
/// `retry-after-ms` or `Retry-After`), empty when the server named none.
/// Shared with the error types of other clients built on
/// [`crate::http`].
pub fn retry_after_suffix(retry_after: Option<Duration>) -> String {
    retry_after
        .map(|d| format!("; server asked to retry after {}s", d.as_secs_f64()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The variants built from an HTTP error response, each with its
    /// message as it reads without an id.
    fn http_variants(request_id: Option<&str>) -> [(Error, &'static str); 7] {
        let id = || request_id.map(str::to_owned);
        [
            (
                Error::Unauthorized { request_id: id() },
                "authentication failed (401): check the API key",
            ),
            (
                Error::PermissionDenied {
                    detail: "model not enabled".into(),
                    request_id: id(),
                },
                "permission denied (403): model not enabled",
            ),
            (
                Error::InvalidRequest {
                    status: 422,
                    detail: "bad".into(),
                    issues: Vec::new(),
                    request_id: id(),
                },
                "request rejected by the API (422): bad",
            ),
            (
                Error::InvalidRequest {
                    status: 400,
                    detail: "model mismatch".into(),
                    issues: Vec::new(),
                    request_id: id(),
                },
                "request rejected by the API (400): model mismatch",
            ),
            (
                Error::RateLimited {
                    attempts: 3,
                    retry_after: Some(Duration::from_secs(2)),
                    request_id: id(),
                },
                "rate limited (429) after 3 attempts; server asked to retry after 2s",
            ),
            (
                Error::Overloaded {
                    attempts: 3,
                    request_id: id(),
                },
                "service overloaded (529) after 3 attempts",
            ),
            (
                Error::Http {
                    status: 500,
                    body: "boom".into(),
                    request_id: id(),
                },
                "unexpected HTTP status 500: boom",
            ),
        ]
    }

    #[test]
    fn request_id_is_read_from_the_http_variants_and_ends_their_message() {
        // Without an id the messages read exactly as before ids existed.
        for (err, message) in http_variants(None) {
            assert_eq!(err.request_id(), None, "{err:?}");
            assert_eq!(err.to_string(), message);
        }
        for (err, message) in http_variants(Some("req_1")) {
            assert_eq!(err.request_id(), Some("req_1"), "{err:?}");
            assert_eq!(err.to_string(), format!("{message} [request_id req_1]"));
        }

        // `?` on a serde error: a Decode with no response behind it.
        let decode = Error::from(serde_json::from_str::<u8>("x").unwrap_err());
        assert!(matches!(
            decode,
            Error::Decode {
                request_id: None,
                ..
            }
        ));
        assert_eq!(decode.request_id(), None);
        assert!(!decode.to_string().contains("request_id"), "{decode}");
        let decode = Error::Decode {
            source: serde_json::from_str::<u8>("x").unwrap_err(),
            request_id: Some("req_2".into()),
        };
        assert_eq!(decode.request_id(), Some("req_2"));
        assert!(
            decode.to_string().ends_with(" [request_id req_2]"),
            "{decode}"
        );

        for err in [
            Error::MissingApiKey,
            Error::InvalidApiKey {
                reason: "has whitespace inside it".into(),
            },
            Error::Url("nope".into()),
            Error::NoRecording("abc".into()),
            Error::ReservedHeader("authorization".into()),
            Error::ReservedField("model".into()),
        ] {
            assert_eq!(err.request_id(), None, "{err:?}");
        }
    }

    /// The four errors of a response that does not fit its questions.
    fn unfit(request_id: Option<&str>) -> [Error; 4] {
        let id = || request_id.map(str::to_owned);
        [
            Error::MissingAnswer {
                id: "q".into(),
                request_id: id(),
            },
            Error::AnswerTypeMismatch {
                id: "q".into(),
                expected: "noul",
                actual: "score".into(),
                request_id: id(),
            },
            Error::UnknownOption {
                id: "q".into(),
                option: "sales".into(),
                request_id: id(),
            },
            Error::InvalidAnswer {
                id: "q".into(),
                reason: "score 4 is outside 0..=3, the scale the question sent".into(),
                request_id: id(),
            },
        ]
    }

    #[test]
    fn is_unfit_names_the_four_fit_errors() {
        for err in unfit(None) {
            assert!(err.is_unfit(), "{err:?}");
        }
        let (http, _) = http_variants(Some("req_1"))
            .into_iter()
            .next()
            .unwrap_or_else(|| unreachable!());
        for err in [
            http,
            Error::MissingApiKey,
            Error::NotAProbability { value: 1.5 },
            Error::NoRecording("abc".into()),
            Error::DuplicateQuestionId("q".into()),
            Error::from(serde_json::from_str::<u8>("x").unwrap_err()),
        ] {
            assert!(!err.is_unfit(), "{err:?}");
        }
    }

    #[test]
    fn an_unfit_answer_carries_its_responses_request_id() {
        let messages = [
            r#"no answer for question "q""#,
            r#"answer "q" is a score but a noul was requested"#,
            r#"answer "q" names option "sales", which its question does not offer"#,
            r#"answer "q" does not fit its question: score 4 is outside 0..=3, the scale the question sent"#,
        ];
        for (err, message) in unfit(None).into_iter().zip(messages) {
            assert_eq!(err.request_id(), None, "{err:?}");
            assert_eq!(err.to_string(), message);
        }
        for (err, message) in unfit(Some("req_9")).into_iter().zip(messages) {
            assert_eq!(err.request_id(), Some("req_9"), "{err:?}");
            assert_eq!(err.to_string(), format!("{message} [request_id req_9]"));
        }
    }

    fn issue(loc: &[&str], msg: &str) -> ValidationIssue {
        ValidationIssue {
            loc: loc.iter().map(|s| (*s).to_owned()).collect(),
            msg: msg.into(),
            kind: "missing".into(),
        }
    }

    #[test]
    fn validation_issue_path_drops_only_the_leading_body() {
        // A question whose id is `body` keeps its segment; the Python SDK
        // would drop it.
        let nested = issue(&["body", "questions", "body", "noul"], "bad");
        assert_eq!(nested.path(), "questions.body.noul");
        assert_eq!(nested.to_string(), "questions.body.noul: bad");

        let question = issue(
            &["body", "questions", "urgency", "score", "criteria"],
            "short",
        );
        assert_eq!(question.path(), "questions.urgency.score.criteria");

        // Not a body location: nothing is dropped.
        assert_eq!(issue(&["query", "limit"], "x").path(), "query.limit");
        // Only `body`, or nothing at all: the message stands alone.
        assert_eq!(issue(&["body"], "Field required").path(), "");
        assert_eq!(
            issue(&["body"], "Field required").to_string(),
            "Field required"
        );
        assert_eq!(issue(&[], "Field required").to_string(), "Field required");
    }
}
