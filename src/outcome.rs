//! The outcome contract: one JSON document per triaged alert.
//!
//! Every triage, whichever way it was started, ends in the same document.
//! [`Outcome`] is that document's Rust shape and the only place the wire
//! format is defined; the domain types it is built from ([`crate::triage`],
//! [`crate::answer`], [`crate::changes`]) keep their own shapes and are
//! mapped here, never re-serialised with wire attributes.
//!
//! # Who reads it
//!
//! * `signalman triage <file> --json` prints it, so a shell script can pipe
//!   it through `jq`.
//! * The webhook receiver logs it as one compact line
//!   ([`Outcome::to_json_line`]), so a log pipeline can index every field of
//!   every decision without parsing prose.
//! * Agents consume it: today by reading the JSON, later over MCP. signalman
//!   is a tool for agents, not an agent (decision 0008), so the contract is
//!   the boundary.
//! * The future `apply_qualification` write tool takes this document back
//!   and replays the decision it describes; [`Outcome::decision`] is that
//!   inverse, and [`Outcome::validate`] is what a reader checks first.
//!
//! # Shape rules
//!
//! * `schema_version` is the integer `1`.
//! * Names are `snake_case`. **No field is ever omitted**: an unknown
//!   optional value is `null`, an empty list is `[]`. A consumer can index
//!   any documented path without checking whether the key exists.
//! * Probabilities and confidences are numbers in `[0, 1]` ([`Unit`]),
//!   enforced by the schema and on read.
//! * Links are absolute URLs or `null`. Timestamps are RFC 3339.
//! * Reading is strict: unknown keys are rejected, and a `schema_version`
//!   other than `1` is an error naming the version found.
//!
//! # Evolution
//!
//! The schema is generated from these types, committed at
//! `docs/schema/outcome.v1.json` (`mise run schema`), and a test fails when
//! the emitted shape drifts from the committed file.
//!
//! *Additive*, `schema_version` stays `1`: adding a field (always emitted,
//! `null` or empty when unknown), adding a nested object, adding a value to
//! a free-form string. Regenerate the schema file and update the docs
//! example; the drift test is the review point.
//!
//! *Breaking*, bump to `2` with a new `docs/schema/outcome.v2.json` and the
//! v1 file kept: renaming or removing a field; changing a type, unit or
//! nullability; adding, renaming or removing a value of any enum
//! ([`Action`], [`ImpactLevel`], [`OwnerStatus`], [`WriteMode`],
//! [`NoteStatus`]); changing the order or meaning of
//! [`ImpactJudgment::distribution`].
//!
//! Consumers other than signalman ignore fields they do not know. signalman
//! as a reader rejects unknown fields and other versions.
//!
//! The `metadata.ai` block the CLI forwards to an incident.io alert source
//! is a separate, unchanged shape: incident.io attribute templates read flat
//! paths, so it stays flat on purpose.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::answer;
use crate::changes::Change;
use crate::triage::{
    ComponentContext, Decision, Impact, NO_DUPLICATE, Owner, Policy, RelatedAlert, TriageAnswers,
};

/// The version this module reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Prefix of every tag signalman writes. Mirrors
/// `crate::incidentio::sync::TAG_PREFIX`, which this module must not depend
/// on; a test asserts the two agree.
const TAG_PREFIX: &str = "ai";

/// The tag namespace signalman owns, prefix and separator. A tag outside it
/// belongs to somebody else and this module says nothing about it.
const TAG_NAMESPACE: &str = "ai-";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a document could not be read, or does not hold together.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// A probability or confidence outside `[0, 1]`, or not a number.
    #[error("{value} is not a probability in 0..=1")]
    NotAUnit {
        /// The rejected value.
        value: f64,
    },
    /// The document is well-formed JSON of the right version but its fields
    /// contradict each other.
    #[error("outcome invariant violated: {0}")]
    Invariant(String),
}

fn invariant(message: impl Into<String>) -> Error {
    Error::Invariant(message.into())
}

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

/// A number in `[0, 1]`: a probability or a confidence. On the wire it is a
/// plain JSON number; the schema constrains it with `minimum` and `maximum`
/// and reading a value outside the range fails.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Unit(f64);

impl Unit {
    /// Validate a raw value. Rejects NaN and anything outside `[0, 1]`.
    pub fn new(value: f64) -> Result<Self, Error> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::NotAUnit { value })
        }
    }

    /// The raw value.
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Unit {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self, Error> {
        Self::new(value)
    }
}

impl From<Unit> for f64 {
    fn from(u: Unit) -> Self {
        u.0
    }
}

impl From<answer::Probability> for Unit {
    fn from(p: answer::Probability) -> Self {
        Self(p.value())
    }
}

impl From<answer::Confidence> for Unit {
    fn from(c: answer::Confidence) -> Self {
        Self(c.value())
    }
}

impl JsonSchema for Unit {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Unit".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "signalman::outcome::Unit".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "minimum": 0.0,
            "maximum": 1.0,
        })
    }

    /// Inlined: a probability reads as a plain number wherever it appears,
    /// not as a reference a consumer has to resolve.
    fn inline_schema() -> bool {
        true
    }
}

/// The contract version. Serialises as the integer `1`; reading any other
/// value fails with a message naming what was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchemaV1;

impl Serialize for SchemaV1 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(SCHEMA_VERSION)
    }
}

impl<'de> Deserialize<'de> for SchemaV1 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let found = i64::deserialize(d)?;
        if found == i64::from(SCHEMA_VERSION) {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported outcome schema_version {found}: this signalman reads version {SCHEMA_VERSION}"
            )))
        }
    }
}

impl JsonSchema for SchemaV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SchemaV1".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "signalman::outcome::SchemaV1".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Version of the outcome contract. Always 1 in this document.",
            "type": "integer",
            "const": 1,
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// What signalman decided to do with the alert. The vocabulary of the
/// `decision` field and of the `ai-action-*` tag (which is hyphenated:
/// `human_triage` is tagged `ai-action-human-triage`, and
/// `attach_to_incident` is tagged `ai-action-attach`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Nobody needs to act; log and drop.
    Suppress,
    /// The same underlying problem as an incident that is already open.
    AttachToIncident,
    /// Wake the owning team now.
    Page,
    /// Open a ticket for the owning team; no page.
    Ticket,
    /// A person decides: the owner is not clear enough to route.
    HumanTriage,
}

impl Action {
    /// The `snake_case` wire name.
    pub fn key(self) -> &'static str {
        match self {
            Self::Suppress => "suppress",
            Self::AttachToIncident => "attach_to_incident",
            Self::Page => "page",
            Self::Ticket => "ticket",
            Self::HumanTriage => "human_triage",
        }
    }

    /// The value used in the `ai-action-*` tag, which predates this contract
    /// and stays hyphenated and abbreviated.
    pub fn tag_value(self) -> &'static str {
        match self {
            Self::Suppress => "suppress",
            Self::AttachToIncident => "attach",
            Self::Page => "page",
            Self::Ticket => "ticket",
            Self::HumanTriage => "human-triage",
        }
    }
}

impl From<&Decision> for Action {
    fn from(d: &Decision) -> Self {
        match d {
            Decision::Suppress { .. } => Self::Suppress,
            Decision::AttachToIncident { .. } => Self::AttachToIncident,
            Decision::Page { .. } => Self::Page,
            Decision::Ticket { .. } => Self::Ticket,
            Decision::HumanTriage { .. } => Self::HumanTriage,
        }
    }
}

/// User-facing impact, lowest first. The same vocabulary as the
/// `ai-impact-*` tag and as `policy.page_at`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImpactLevel {
    /// Internal, informational, or non-production only.
    None,
    /// A small subset of users, or one non-critical feature, degraded.
    Minor,
    /// Most users degraded, or a critical feature unavailable.
    Major,
    /// The product is unavailable, or data integrity is at risk, for everyone.
    Outage,
}

impl ImpactLevel {
    /// Every level, lowest first. The order of
    /// [`ImpactJudgment::distribution`].
    pub const ALL: [Self; 4] = [Self::None, Self::Minor, Self::Major, Self::Outage];

    /// Position on the scale, `0..=3`.
    pub fn index(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Minor => 1,
            Self::Major => 2,
            Self::Outage => 3,
        }
    }

    /// The `snake_case` wire name, also the `ai-impact-*` tag value.
    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Outage => "outage",
        }
    }
}

impl From<Impact> for ImpactLevel {
    fn from(i: Impact) -> Self {
        match i {
            Impact::None => Self::None,
            Impact::Minor => Self::Minor,
            Impact::Major => Self::Major,
            Impact::Outage => Self::Outage,
        }
    }
}

impl From<ImpactLevel> for Impact {
    fn from(i: ImpactLevel) -> Self {
        match i {
            ImpactLevel::None => Self::None,
            ImpactLevel::Minor => Self::Minor,
            ImpactLevel::Major => Self::Major,
            ImpactLevel::Outage => Self::Outage,
        }
    }
}

/// How firmly the owner is attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnerStatus {
    /// Routed to this team; owner confidence cleared the automatic threshold.
    Assigned,
    /// Routed to this team, but the responder should confirm ownership:
    /// confidence was middling.
    Confirm,
    /// Nobody was confident enough to route; this is the best guess offered
    /// to whoever triages by hand.
    BestGuess,
}

/// Whether the side effects the document describes actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Tags, attachment, note and notification were written back.
    Applied,
    /// Everything was computed, nothing was written (evaluation runs).
    DryRun,
    /// The file-based CLI: there is no incident.io alert to write back to.
    Detached,
}

/// What became of the qualification note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    /// A new note was created on the alert.
    Created,
    /// The note signalman left on an earlier pass was rewritten in place.
    Replaced,
    /// Writing the note failed; `error` says why. The tags were still written.
    Failed,
    /// The note is switched off in the configuration.
    Disabled,
    /// No note was attempted: a dry run, or the detached CLI.
    Skipped,
}

// ---------------------------------------------------------------------------
// Nested objects
// ---------------------------------------------------------------------------

/// The alert as the model saw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlertRef {
    /// incident.io alert id (a ULID). `null` when the alert came from a file.
    #[serde(deserialize_with = "Option::deserialize")]
    pub id: Option<String>,
    /// The alert's title.
    pub title: String,
    /// Emitting system, exactly as it was sent in the state: `prometheus`, or
    /// `incident.io alert source <id>` on the webhook path.
    pub source: String,
    /// Link back to the monitor or query that fired.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_url: Option<String>,
    /// When the alert was created, RFC 3339. `null` when the source does not
    /// say.
    #[schemars(extend("format" = "date-time"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub created_at: Option<String>,
    /// The labels the model saw, after merging the alert's own tags.
    pub labels: BTreeMap<String, String>,
}

/// The team the decision addresses. `null` unless the decision routes to
/// somebody (`page`, `ticket`, `human_triage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerRef {
    /// Candidate key, also the suffix of the `ai-team-*` tag.
    pub key: String,
    /// Display name.
    pub label: String,
    /// Software-catalog reference (`group:default/payments`) when the
    /// candidate came from Backstage; `null` for the built-in team list.
    #[serde(deserialize_with = "Option::deserialize")]
    pub entity_ref: Option<String>,
    /// The team's page in the Backstage catalog.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
    /// How firmly the team is attributed.
    pub status: OwnerStatus,
}

/// An incident this alert belongs to. Its dedup confidence is not repeated
/// here: it is `judgments.duplicate_of.confidence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IncidentRef {
    /// incident.io incident id (a ULID), when it was fetched from the API.
    #[serde(deserialize_with = "Option::deserialize")]
    pub id: Option<String>,
    /// Human reference, `INC-4821`. Always present.
    pub reference: String,
    /// The incident in the incident.io app.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
}

/// The alerting component as the software catalog knows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentRef {
    /// Catalog name.
    pub name: String,
    /// The component's page in the Backstage catalog.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
    /// `spec.type`: service, website, library, ...
    #[serde(rename = "type")]
    #[serde(deserialize_with = "Option::deserialize")]
    pub component_type: Option<String>,
    /// `spec.lifecycle`: production, experimental, deprecated.
    #[serde(deserialize_with = "Option::deserialize")]
    pub lifecycle: Option<String>,
    /// The system it belongs to.
    #[serde(deserialize_with = "Option::deserialize")]
    pub system: Option<String>,
    /// The owner the catalog records, as a display name. Not the same thing
    /// as `owner`: this is what the catalog says, `owner` is what signalman
    /// decided.
    #[serde(deserialize_with = "Option::deserialize")]
    pub catalog_owner: Option<String>,
    /// What it depends on, as `kind name` strings.
    pub depends_on: Vec<String>,
    /// What depends on it, as `kind name` strings: the blast radius.
    pub dependents: Vec<String>,
}

/// One probability, as its own object so the field reads the same wherever
/// a yes/no judgment appears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Probability {
    /// Probability of yes.
    pub probability: Unit,
}

/// One option of the owner question, with the probability the model gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerOption {
    /// Candidate key. `none_of_these` is the no-match option, always offered.
    pub key: String,
    /// Display name.
    pub label: String,
    /// Software-catalog reference, when the candidate came from Backstage.
    #[serde(deserialize_with = "Option::deserialize")]
    pub entity_ref: Option<String>,
    /// The team's page in the Backstage catalog.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
    /// Probability this is the owning team.
    pub probability: Unit,
}

/// Who should own the first response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerJudgment {
    /// The option the model chose. May be `none_of_these`.
    pub chosen: String,
    /// How concentrated the distribution is. Not the probability of
    /// `chosen`: that is in `options`.
    pub confidence: Unit,
    /// Every option offered, including `none_of_these`, most probable first.
    pub options: Vec<OwnerOption>,
}

/// One level of the impact scale with its probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactOption {
    /// The level.
    pub level: ImpactLevel,
    /// Probability the impact is at this level.
    pub probability: Unit,
}

/// How bad it is for users.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactJudgment {
    /// The level nearest to `score`. This is what the policy compares
    /// against `policy.page_at`, and what the top-level `impact` repeats.
    pub level: ImpactLevel,
    /// Index of `level` on the scale, `0..=3`.
    #[schemars(range(min = 0, max = 3))]
    pub index: u8,
    /// Probability-weighted position on the scale, `0.0..=3.0`. It may fall
    /// between levels: 2.6 is "worse than major, not quite an outage".
    #[schemars(range(min = 0.0, max = 3.0))]
    pub score: f64,
    /// How concentrated the distribution is.
    pub confidence: Unit,
    /// Probability of every level, always four rows, lowest level first.
    #[schemars(length(equal = 4))]
    pub distribution: Vec<ImpactOption>,
}

/// One open incident offered as a duplicate candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IncidentOption {
    /// Human reference, `INC-4821`.
    pub reference: String,
    /// incident.io incident id (a ULID), when it was fetched from the API.
    #[serde(deserialize_with = "Option::deserialize")]
    pub id: Option<String>,
    /// The incident in the incident.io app.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
    /// Probability this alert is the same underlying problem.
    pub probability: Unit,
}

/// Whether the alert duplicates an open incident. The whole object is `null`
/// when the question was not asked because no incident was open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateJudgment {
    /// The incident reference the model chose, or `null` when it chose the
    /// "new, separate problem" option.
    #[serde(deserialize_with = "Option::deserialize")]
    pub chosen: Option<String>,
    /// How concentrated the distribution is. Compared against
    /// `policy.attach_confidence`.
    pub confidence: Unit,
    /// Probability the alert is a new, separate problem.
    pub none_probability: Unit,
    /// Every incident offered, most probable first. Excludes the "none"
    /// option, whose probability is `none_probability`.
    pub candidates: Vec<IncidentOption>,
}

/// Everything the model judged. One question, one field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Judgments {
    /// Who should own the first response.
    pub owner: OwnerJudgment,
    /// How bad it is for users.
    pub impact: ImpactJudgment,
    /// Probability a person must act now rather than let it resolve itself.
    pub actionable: Probability,
    /// Whether it duplicates an open incident; `null` when no incident was
    /// open, so the question was never asked.
    #[serde(deserialize_with = "Option::deserialize")]
    pub duplicate_of: Option<DuplicateJudgment>,
    /// Probability a listed recent change caused it; `null` when no change
    /// was listed, so the question was never asked.
    #[serde(deserialize_with = "Option::deserialize")]
    pub caused_by_change: Option<Probability>,
}

/// Another alert firing in the same window: the blast radius as the alert
/// hub saw it when the decision was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelatedAlertRef {
    /// Its title.
    pub title: String,
    /// Minutes since it was created.
    pub age_minutes: u64,
    /// The component it names, when it names one.
    #[serde(deserialize_with = "Option::deserialize")]
    pub component: Option<String>,
}

/// A deploy, configuration change or feature flag offered to the model as a
/// possible cause. Everything but `summary` is `null` when the alert carried
/// its changes as plain lines instead of through the change feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeRef {
    /// When it happened, RFC 3339.
    #[schemars(extend("format" = "date-time"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub at: Option<String>,
    /// `deploy`, `config`, `flag`, `infra`, `migration`, or anything short.
    #[serde(deserialize_with = "Option::deserialize")]
    pub kind: Option<String>,
    /// The component it touched; `null` means platform-wide.
    #[serde(deserialize_with = "Option::deserialize")]
    pub component: Option<String>,
    /// One line: what changed, which version, who did it.
    pub summary: String,
    /// The tool that reported it: `argocd`, `gitlab`, ...
    #[serde(deserialize_with = "Option::deserialize")]
    pub source: Option<String>,
    /// Link to the deploy, merge request or commit.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub url: Option<String>,
}

/// How far back the two lookups reached. `null` means the lookup was not
/// performed at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Windows {
    /// Seconds of history searched for other firing alerts.
    #[serde(deserialize_with = "Option::deserialize")]
    pub related_seconds: Option<u64>,
    /// Seconds of history searched in the change feed.
    #[serde(deserialize_with = "Option::deserialize")]
    pub change_seconds: Option<u64>,
}

/// The thresholds in force when the decision was made. Kept in the document
/// because a decision is only interpretable against the policy that produced
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicySnapshot {
    /// Below this `actionable` probability the alert is suppressed.
    pub suppress_below: Unit,
    /// Minimum dedup confidence to attach to an existing incident.
    pub attach_confidence: Unit,
    /// Owner confidence at or above which routing is automatic.
    pub auto_route_confidence: Unit,
    /// Owner confidence below which a person triages instead.
    pub human_below_confidence: Unit,
    /// Impact at or above which the owner is paged rather than ticketed.
    pub page_at: ImpactLevel,
    /// `caused_by_change` probability above which the change is flagged.
    pub flag_change_above: Unit,
}

/// Token accounting for the one TypeSafe request behind this decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Tokens in the state plus all questions. These are the billed ones.
    pub input_tokens: u64,
    /// Tokens in the answers.
    pub output_tokens: u64,
}

impl From<answer::Usage> for Usage {
    fn from(u: answer::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }
    }
}

/// The qualification note signalman writes on the incident.io alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteWrite {
    /// What happened to the note.
    pub status: NoteStatus,
    /// The note's id when one was written.
    #[serde(deserialize_with = "Option::deserialize")]
    pub id: Option<String>,
    /// Why it failed, when `status` is `failed`.
    #[serde(deserialize_with = "Option::deserialize")]
    pub error: Option<String>,
}

impl NoteWrite {
    /// A note that was never attempted.
    pub fn skipped() -> Self {
        Self {
            status: NoteStatus::Skipped,
            id: None,
            error: None,
        }
    }
}

/// The alert the CLI forwarded to an incident.io HTTP alert source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Forwarded {
    /// The key incident.io deduplicated the event under.
    pub deduplication_key: String,
    /// The status incident.io reported back.
    pub status: String,
}

/// What signalman actually did, as opposed to what it decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Writes {
    /// Whether the side effects happened at all.
    pub mode: WriteMode,
    /// Whether `tags` were written on the alert.
    pub tags_applied: bool,
    /// Whether the alert was attached to `incident`.
    pub attached: bool,
    /// What became of the qualification note.
    pub note: NoteWrite,
    /// Catalog reference of the group notified through Backstage, when one
    /// was notified.
    #[serde(deserialize_with = "Option::deserialize")]
    pub notified: Option<String>,
    /// The forwarded alert event, when the CLI ran with
    /// `--forward-to-incidentio`.
    #[serde(deserialize_with = "Option::deserialize")]
    pub forwarded: Option<Forwarded>,
}

impl Writes {
    /// The file-based CLI: nothing to write back to.
    pub fn detached() -> Self {
        Self {
            mode: WriteMode::Detached,
            tags_applied: false,
            attached: false,
            note: NoteWrite::skipped(),
            notified: None,
            forwarded: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// One triaged alert: what was judged, what was decided, and what was
/// written back.
///
/// See `docs/triage.md`, section "The outcome contract", for the shape rules
/// and the evolution policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Version of this contract. Always `1`.
    pub schema_version: SchemaV1,
    /// The signalman build that produced the document.
    pub signalman_version: String,
    /// When the decision was reached, RFC 3339.
    #[schemars(extend("format" = "date-time"))]
    pub decided_at: String,
    /// Seconds from the alert's creation to the decision. `null` when the
    /// alert carries no creation time, as on the file-based CLI.
    #[serde(deserialize_with = "Option::deserialize")]
    pub time_to_qualify_seconds: Option<f64>,
    /// The alert as the model saw it.
    pub alert: AlertRef,
    /// What to do with it.
    pub decision: Action,
    /// The impact level the policy used. Always equals
    /// `judgments.impact.level`; repeated at the top level because it is
    /// half of the routing decision.
    pub impact: ImpactLevel,
    /// Whether a listed recent change is flagged as the likely cause. Only
    /// ever true for `page` and `ticket`.
    pub suspected_change: bool,
    /// The team the decision addresses; `null` for `suppress` and
    /// `attach_to_incident`.
    #[serde(deserialize_with = "Option::deserialize")]
    pub owner: Option<OwnerRef>,
    /// The incident to attach to; `null` unless the decision is
    /// `attach_to_incident`. It names the target even in a dry run:
    /// `writes.attached` says whether the attachment happened.
    #[serde(deserialize_with = "Option::deserialize")]
    pub incident: Option<IncidentRef>,
    /// The alerting component in the software catalog; `null` when Backstage
    /// is not configured or nothing matched.
    #[serde(deserialize_with = "Option::deserialize")]
    pub component: Option<ComponentRef>,
    /// The runbook page the alert was read against.
    #[schemars(extend("format" = "uri"))]
    #[serde(deserialize_with = "Option::deserialize")]
    pub runbook_url: Option<String>,
    /// Everything the model judged.
    pub judgments: Judgments,
    /// Other alerts firing in the window, newest first.
    pub related_alerts: Vec<RelatedAlertRef>,
    /// Recent changes offered to the model as possible causes.
    pub recent_changes: Vec<ChangeRef>,
    /// How far back the two lookups reached.
    pub windows: Windows,
    /// The thresholds in force.
    pub policy: PolicySnapshot,
    /// The `ai-*` tags written on the alert, or that would be written. Every
    /// one is derivable from the fields above.
    pub tags: Vec<String>,
    /// The versioned model that answered, e.g. `jev-1.13.0`. Thresholds are
    /// tuned per version.
    pub model: String,
    /// Token accounting for the request.
    pub usage: Usage,
    /// What was actually written.
    pub writes: Writes,
}

/// Where the recent changes came from.
#[derive(Debug, Clone, Copy)]
pub enum Changes<'a> {
    /// Typed changes from the change feed: every field is known.
    Typed(&'a [Change]),
    /// Plain lines the alert carried itself: only the summary is known.
    Lines(&'a [String]),
}

/// Everything [`Outcome::build`] needs, gathered by the caller.
///
/// The caller does the lookups (Backstage URLs, incident.io ids); this
/// struct is the seam, so the wire module needs no client of its own.
#[derive(Debug, Clone)]
pub struct Input<'a> {
    /// Crate version of the build that decided, `env!("CARGO_PKG_VERSION")`.
    pub version: &'a str,
    /// The instant the decision was reached.
    pub decided_at: Timestamp,
    /// From the alert's creation to `decided_at`, when it is known.
    pub time_to_qualify: Option<Duration>,
    /// The alert as the model saw it.
    pub alert: AlertRef,
    /// The typed answers.
    pub answers: &'a TriageAnswers,
    /// The decision the policy reached from those answers.
    pub decision: &'a Decision,
    /// The thresholds it used.
    pub policy: &'a Policy,
    /// Backstage page of the owner the decision names.
    pub owner_url: Option<String>,
    /// Candidate key to Backstage page; empty without Backstage.
    pub candidate_urls: &'a BTreeMap<String, String>,
    /// The incident the decision attaches to, when it attaches.
    pub incident: Option<IncidentRef>,
    /// The incidents offered as duplicate candidates, for their ids and
    /// links; the probabilities come from `answers`.
    pub candidate_incidents: &'a [IncidentRef],
    /// Catalog context for the alerting component.
    pub component: Option<&'a ComponentContext>,
    /// Backstage page of that component.
    pub component_url: Option<String>,
    /// TechDocs page the runbook excerpt came from.
    pub runbook_url: Option<String>,
    /// Other alerts firing in the window.
    pub related: &'a [RelatedAlert],
    /// Recent changes offered to the model.
    pub changes: Changes<'a>,
    /// How far back the two lookups reached.
    pub windows: Windows,
    /// The tags written, or that would be written.
    pub tags: &'a [String],
    /// The versioned model that answered.
    pub model: &'a str,
    /// Token accounting.
    pub usage: Usage,
    /// What was actually written.
    pub writes: Writes,
}

impl Outcome {
    /// Map domain values onto the wire document. The only place that
    /// mapping lives.
    #[allow(clippy::too_many_lines)] // one flat mapping reads better than fragments
    pub fn build(input: Input<'_>) -> Self {
        let answers = input.answers;
        let action = Action::from(input.decision);
        let impact = ImpactLevel::from(Impact::from_level(answers.impact.nearest_level()));

        let owner = input.decision.owner().map(|o| OwnerRef {
            key: o.key.clone(),
            label: o.label.clone(),
            entity_ref: o.entity_ref.clone(),
            url: input.owner_url.clone(),
            status: owner_status(input.decision),
        });

        let mut options: Vec<OwnerOption> = answers
            .candidates
            .iter()
            .map(|c| OwnerOption {
                key: c.key.clone(),
                label: c.label.clone(),
                entity_ref: c.entity_ref.clone(),
                url: input.candidate_urls.get(&c.key).cloned(),
                probability: unit(answers.owner.probability_of(&c.key)),
            })
            .collect();
        options.sort_by(|a, b| by_probability_then(a.probability, b.probability, &a.key, &b.key));

        let distribution = ImpactLevel::ALL
            .iter()
            .map(|level| ImpactOption {
                level: *level,
                probability: answers
                    .impact
                    .probabilities
                    .get(level.index() as usize)
                    .map_or_else(|| unit(0.0), |p| Unit::from(*p)),
            })
            .collect();

        let duplicate_of = answers.duplicate_of.as_ref().map(|dup| {
            let none_key = NO_DUPLICATE.to_owned();
            // One row per incident the question offered, exactly as the
            // owner judgment lists one row per candidate team: the request
            // supplies the option set and the answer supplies the
            // probabilities. A probability the model left out reads as 0,
            // and `chosen` is always one of these rows, which is what lets
            // `validate` require the attached incident to be among them.
            let mut candidates: Vec<IncidentOption> = input
                .candidate_incidents
                .iter()
                .map(|offered| IncidentOption {
                    reference: offered.reference.clone(),
                    id: offered.id.clone(),
                    url: offered.url.clone(),
                    probability: unit(dup.probability_of(&offered.reference)),
                })
                .collect();
            candidates.sort_by(|a, b| {
                by_probability_then(a.probability, b.probability, &a.reference, &b.reference)
            });
            DuplicateJudgment {
                chosen: (dup.chosen != NO_DUPLICATE).then(|| dup.chosen.clone()),
                confidence: Unit::from(dup.confidence),
                none_probability: unit(dup.probability_of(&none_key)),
                candidates,
            }
        });

        let recent_changes = match input.changes {
            Changes::Typed(changes) => changes
                .iter()
                .map(|c| ChangeRef {
                    at: c.at.clone(),
                    kind: Some(c.kind.clone()),
                    component: c.component.clone(),
                    summary: c.summary.clone(),
                    source: c.source.clone(),
                    url: c.url.clone(),
                })
                .collect(),
            Changes::Lines(lines) => lines
                .iter()
                .map(|line| ChangeRef {
                    at: None,
                    kind: None,
                    component: None,
                    summary: line.clone(),
                    source: None,
                    url: None,
                })
                .collect(),
        };

        Self {
            schema_version: SchemaV1,
            signalman_version: input.version.to_owned(),
            decided_at: input.decided_at.to_string(),
            time_to_qualify_seconds: input.time_to_qualify.map(|d| d.as_secs_f64()),
            alert: input.alert,
            decision: action,
            impact,
            suspected_change: suspected_change(input.decision),
            owner,
            incident: input.incident,
            component: input.component.map(|c| ComponentRef {
                name: c.name.clone(),
                url: input.component_url.clone(),
                component_type: c.component_type.clone(),
                lifecycle: c.lifecycle.clone(),
                system: c.system.clone(),
                catalog_owner: c.owner.clone(),
                depends_on: c.depends_on.clone(),
                dependents: c.dependents.clone(),
            }),
            runbook_url: input.runbook_url,
            judgments: Judgments {
                owner: OwnerJudgment {
                    chosen: answers.owner.chosen.clone(),
                    confidence: Unit::from(answers.owner.confidence),
                    options,
                },
                impact: ImpactJudgment {
                    level: impact,
                    index: impact.index(),
                    score: answers.impact.value,
                    confidence: Unit::from(answers.impact.confidence),
                    distribution,
                },
                actionable: Probability {
                    probability: Unit::from(answers.actionable.yes),
                },
                duplicate_of,
                caused_by_change: answers.caused_by_change.map(|n| Probability {
                    probability: Unit::from(n.yes),
                }),
            },
            related_alerts: input
                .related
                .iter()
                .map(|r| RelatedAlertRef {
                    title: r.title.clone(),
                    age_minutes: r.age_minutes,
                    component: r.component.clone(),
                })
                .collect(),
            recent_changes,
            windows: input.windows,
            policy: PolicySnapshot {
                suppress_below: unit(input.policy.suppress_below),
                attach_confidence: unit(input.policy.attach_confidence),
                auto_route_confidence: unit(input.policy.auto_route_confidence),
                human_below_confidence: unit(input.policy.human_below_confidence),
                page_at: ImpactLevel::from(input.policy.page_at),
                flag_change_above: unit(input.policy.flag_change_above),
            },
            tags: input.tags.to_vec(),
            model: input.model.to_owned(),
            usage: input.usage,
            writes: input.writes,
        }
    }

    /// The tags this document implies, in the order
    /// `crate::incidentio::sync::tags_for` writes them.
    ///
    /// Everything signalman tags is derivable from the document, which is
    /// what makes the tags a lossy view of it rather than a second source of
    /// truth.
    pub fn expected_tags(&self) -> Vec<String> {
        let mut tags = vec![
            tag("team", &self.judgments.owner.chosen),
            tag("impact", self.impact.key()),
            tag("action", self.decision.tag_value()),
        ];
        if self.suspected_change {
            tags.push(tag("suspected-change", ""));
        }
        if let Some(incident) = &self.incident {
            tags.push(tag("dup", &incident.reference));
        }
        tags
    }

    /// Check the invariants a reader may rely on.
    ///
    /// Serialisation cannot break them, so this is for documents that came
    /// from somewhere else: a file, a log line, an agent.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_impact()?;
        self.validate_owner()?;
        self.validate_incident()?;
        self.validate_tags()
    }

    /// `impact` repeats `judgments.impact.level`, whose index and
    /// distribution describe the same scale.
    fn validate_impact(&self) -> Result<(), Error> {
        let judged = &self.judgments.impact;
        if self.impact != judged.level {
            return Err(invariant(format!(
                "impact {} does not match judgments.impact.level {}",
                self.impact.key(),
                judged.level.key()
            )));
        }
        if judged.index != judged.level.index() {
            return Err(invariant(format!(
                "judgments.impact.index {} does not match level {}",
                judged.index,
                judged.level.key()
            )));
        }
        // `score` is a position on the same scale as `index`, so it may
        // fall between two levels but never off either end. A value outside
        // the range means the answer it came from was not the four-level
        // scale the question defines.
        let top = f64::from(ImpactLevel::Outage.index());
        if !(0.0..=top).contains(&judged.score) {
            return Err(invariant(format!(
                "judgments.impact.score {} is outside 0..={top} on the impact scale",
                judged.score
            )));
        }
        if judged.distribution.len() != ImpactLevel::ALL.len() {
            return Err(invariant(format!(
                "judgments.impact.distribution has {} rows, expected {}",
                judged.distribution.len(),
                ImpactLevel::ALL.len()
            )));
        }
        for (row, level) in judged.distribution.iter().zip(ImpactLevel::ALL) {
            if row.level != level {
                return Err(invariant(
                    "judgments.impact.distribution is not in level order",
                ));
            }
        }
        Ok(())
    }

    /// An owner is named exactly by the decisions that route to somebody,
    /// with the status that decision implies. `suspected_change` rides along
    /// with the same two decisions that can carry it.
    fn validate_owner(&self) -> Result<(), Error> {
        let routes = matches!(
            self.decision,
            Action::Page | Action::Ticket | Action::HumanTriage
        );
        match (&self.owner, routes) {
            (Some(owner), true) => {
                let best_guess = owner.status == OwnerStatus::BestGuess;
                if best_guess != (self.decision == Action::HumanTriage) {
                    return Err(invariant(format!(
                        "owner.status {:?} does not match decision {}",
                        owner.status,
                        self.decision.key()
                    )));
                }
            }
            (None, false) => {}
            (Some(_), false) => {
                return Err(invariant(format!(
                    "decision {} must not name an owner",
                    self.decision.key()
                )));
            }
            (None, true) => {
                return Err(invariant(format!(
                    "decision {} needs an owner",
                    self.decision.key()
                )));
            }
        }
        if self.suspected_change && !matches!(self.decision, Action::Page | Action::Ticket) {
            return Err(invariant(format!(
                "suspected_change is only carried by page and ticket, not {}",
                self.decision.key()
            )));
        }
        Ok(())
    }

    /// An incident is named exactly by `attach_to_incident`, and it is the
    /// one the dedup judgment chose from the candidates it was offered.
    fn validate_incident(&self) -> Result<(), Error> {
        let attaches = self.decision == Action::AttachToIncident;
        match (&self.incident, attaches) {
            (Some(incident), true) => {
                let dup = self.judgments.duplicate_of.as_ref().ok_or_else(|| {
                    invariant("attach_to_incident without judgments.duplicate_of")
                })?;
                if dup.chosen.as_deref() != Some(incident.reference.as_str()) {
                    return Err(invariant(format!(
                        "judgments.duplicate_of.chosen {:?} is not incident.reference {:?}",
                        dup.chosen, incident.reference
                    )));
                }
                if !dup
                    .candidates
                    .iter()
                    .any(|c| c.reference == incident.reference)
                {
                    return Err(invariant(format!(
                        "incident {:?} was not among the duplicate candidates",
                        incident.reference
                    )));
                }
                Ok(())
            }
            (None, false) => Ok(()),
            (Some(_), false) => Err(invariant(format!(
                "decision {} must not name an incident",
                self.decision.key()
            ))),
            (None, true) => Err(invariant("attach_to_incident without an incident")),
        }
    }

    /// Every tag signalman wrote is one the document implies.
    fn validate_tags(&self) -> Result<(), Error> {
        let expected = self.expected_tags();
        for t in &self.tags {
            if t.starts_with(TAG_NAMESPACE) && !expected.contains(t) {
                return Err(invariant(format!(
                    "tag {t:?} is not derivable from the document (expected one of {expected:?})"
                )));
            }
        }
        Ok(())
    }

    /// Rebuild the routing decision the document describes.
    ///
    /// The inverse of the `decision`, `owner` and `judgments` half of
    /// [`Outcome::build`]. It is what makes the document a contract rather
    /// than a report: a reader can act on it without re-running the model.
    pub fn decision(&self) -> Result<Decision, Error> {
        match self.decision {
            Action::Suppress => Ok(Decision::Suppress {
                actionable: self.judgments.actionable.probability.value(),
            }),
            Action::AttachToIncident => {
                let incident = self
                    .incident
                    .as_ref()
                    .ok_or_else(|| invariant("attach_to_incident without an incident"))?;
                let dup = self.judgments.duplicate_of.as_ref().ok_or_else(|| {
                    invariant("attach_to_incident without judgments.duplicate_of")
                })?;
                Ok(Decision::AttachToIncident {
                    incident_id: incident.reference.clone(),
                    confidence: dup.confidence.value(),
                })
            }
            Action::Page => Ok(Decision::Page {
                owner: self.owner_domain()?,
                impact: self.impact.into(),
                confirm_owner: self.confirm_owner()?,
                suspected_change: self.suspected_change,
            }),
            Action::Ticket => Ok(Decision::Ticket {
                owner: self.owner_domain()?,
                impact: self.impact.into(),
                confirm_owner: self.confirm_owner()?,
                suspected_change: self.suspected_change,
            }),
            Action::HumanTriage => Ok(Decision::HumanTriage {
                best_guess: self.owner_domain()?,
                confidence: self.judgments.owner.confidence.value(),
                impact: self.impact.into(),
            }),
        }
    }

    /// The document as one compact JSON line, for a log field.
    ///
    /// Infallible on purpose: a log line must never take the process down.
    /// Serialisation cannot fail for these types, but if it ever did the
    /// line carries the error instead of the document.
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "error": format!("outcome serialisation failed: {e}"),
            })
            .to_string()
        })
    }

    /// The JSON Schema of this document, as committed at
    /// `docs/schema/outcome.v1.json` and printed by `signalman schema
    /// outcome`.
    ///
    /// Every property is listed in `required`, including the nullable ones:
    /// no field is ever omitted, so a consumer may index any path.
    pub fn schema() -> Schema {
        let mut schema = schemars::schema_for!(Self);
        require_every_property(&mut schema);
        schema
    }

    fn owner_domain(&self) -> Result<Owner, Error> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| invariant(format!("decision {} needs an owner", self.decision.key())))?;
        Ok(Owner {
            key: owner.key.clone(),
            label: owner.label.clone(),
            entity_ref: owner.entity_ref.clone(),
        })
    }

    fn confirm_owner(&self) -> Result<bool, Error> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| invariant(format!("decision {} needs an owner", self.decision.key())))?;
        Ok(owner.status == OwnerStatus::Confirm)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Clamp a raw threshold or probability into the unit interval.
///
/// The domain types that reach here are validated already
/// ([`answer::Probability`], [`answer::Confidence`]); a [`Policy`] threshold
/// is validated by `Policy::validate` at load. Clamping rather than failing
/// keeps a mis-set threshold from turning a decision into an error: the
/// document still says what was decided and with which policy.
fn unit(value: f64) -> Unit {
    Unit(if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    })
}

/// Most probable first, then by key so the order is total and stable.
fn by_probability_then(a: Unit, b: Unit, a_key: &str, b_key: &str) -> Ordering {
    b.value()
        .total_cmp(&a.value())
        .then_with(|| a_key.cmp(b_key))
}

fn owner_status(decision: &Decision) -> OwnerStatus {
    match decision {
        Decision::Page { confirm_owner, .. } | Decision::Ticket { confirm_owner, .. } => {
            if *confirm_owner {
                OwnerStatus::Confirm
            } else {
                OwnerStatus::Assigned
            }
        }
        _ => OwnerStatus::BestGuess,
    }
}

fn suspected_change(decision: &Decision) -> bool {
    match decision {
        Decision::Page {
            suspected_change, ..
        }
        | Decision::Ticket {
            suspected_change, ..
        } => *suspected_change,
        _ => false,
    }
}

/// One tag, as incident.io stores it: lowercase, hyphenated, prefixed.
fn tag(kind: &str, value: &str) -> String {
    let v = value.to_ascii_lowercase().replace([' ', '_'], "-");
    if v.is_empty() {
        format!("{TAG_PREFIX}-{kind}")
    } else {
        format!("{TAG_PREFIX}-{kind}-{v}")
    }
}

/// List every property of every object schema in `required`.
///
/// schemars leaves `Option<T>` fields out of `required`, which is right for
/// a format that omits them and wrong for this one: the fields are always
/// present, `null` when unknown. Applied to the root and, recursively, to
/// every subschema including each `$defs` entry.
fn require_every_property(schema: &mut Schema) {
    let required: Option<Vec<serde_json::Value>> = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|props| {
            props
                .keys()
                .map(|k| serde_json::Value::String(k.clone()))
                .collect()
        });
    if let Some(required) = required {
        schema.insert("required".to_owned(), serde_json::Value::Array(required));
    }
    schemars::transform::transform_subschemas(&mut require_every_property, schema);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn schema_version_reads_only_version_one() {
        assert_eq!(serde_json::to_string(&SchemaV1).unwrap(), "1");
        assert!(serde_json::from_str::<SchemaV1>("1").is_ok());
        let err = serde_json::from_str::<SchemaV1>("2")
            .unwrap_err()
            .to_string();
        assert!(err.contains('2'), "{err}");
        assert!(err.contains("schema_version"), "{err}");
    }

    #[test]
    fn unit_rejects_anything_outside_the_interval() {
        assert!(Unit::new(0.0).is_ok());
        assert!(Unit::new(1.0).is_ok());
        assert!(Unit::new(1.5).is_err());
        assert!(Unit::new(-0.1).is_err());
        assert!(Unit::new(f64::NAN).is_err());
        assert!(serde_json::from_str::<Unit>("1.5").is_err());
        assert_eq!(
            serde_json::to_string(&Unit::new(0.25).unwrap()).unwrap(),
            "0.25"
        );
    }

    #[test]
    fn impact_levels_round_trip_through_the_domain_enum() {
        for level in ImpactLevel::ALL {
            assert_eq!(ImpactLevel::from(Impact::from(level)), level);
            assert_eq!(
                Impact::from_level(level.index() as usize),
                Impact::from(level)
            );
        }
    }

    #[test]
    fn action_keys_match_the_decision_tag() {
        let all = [
            (Action::Suppress, "suppress"),
            (Action::AttachToIncident, "attach_to_incident"),
            (Action::Page, "page"),
            (Action::Ticket, "ticket"),
            (Action::HumanTriage, "human_triage"),
        ];
        for (action, key) in all {
            assert_eq!(action.key(), key);
            assert_eq!(
                serde_json::to_value(action).unwrap(),
                serde_json::json!(key)
            );
        }
    }

    #[test]
    fn the_tag_prefix_is_the_one_incidentio_writes() {
        assert_eq!(TAG_PREFIX, crate::incidentio::sync::TAG_PREFIX);
    }

    #[test]
    fn every_object_in_the_schema_requires_every_property() {
        let schema = Outcome::schema();
        let value = serde_json::to_value(&schema).unwrap();
        let mut objects = 0;
        walk(&value, &mut objects);
        assert!(objects > 10, "only {objects} object schemas found");
    }

    fn walk(value: &serde_json::Value, objects: &mut usize) {
        let serde_json::Value::Object(map) = value else {
            return;
        };
        if let Some(props) = map.get("properties").and_then(|p| p.as_object()) {
            *objects += 1;
            let required: Vec<&str> = map
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let keys: Vec<&str> = props.keys().map(String::as_str).collect();
            assert_eq!(required, keys, "required != properties in {map:?}");
        }
        for v in map.values() {
            walk(v, objects);
            if let Some(arr) = v.as_array() {
                for item in arr {
                    walk(item, objects);
                }
            }
            if let Some(obj) = v.as_object() {
                for item in obj.values() {
                    walk(item, objects);
                }
            }
        }
    }
}
