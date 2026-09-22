//! Layered configuration: built-in defaults, then the configuration file,
//! then environment variables, then command-line flags. Later layers win.
//!
//! This module is the only place that reads a non-secret environment
//! variable, so the precedence table in `docs/configuration.md` describes the
//! code rather than approximating it. Secrets (API keys, tokens, the webhook
//! signing secret) are read by the clients that need them and are refused
//! from the file: a file is reviewed and mounted from a `ConfigMap`; a secret
//! is not.
//!
//! [`Settings`] is the file schema: every field optional, unknown keys
//! rejected so a typo in a `ConfigMap` fails at start-up rather than silently
//! keeping a default. [`Config`] is the effective result after all layers.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::triage::{OwnerCandidate, OwnerCandidates, Policy, Texts};

/// Environment variable naming the configuration file.
pub const FILE_ENV: &str = "SIGNALMAN_CONFIG";

/// Paths tried in order when no file is named: the working directory for
/// development, `/etc/signalman` for a mounted `ConfigMap`.
pub const DEFAULT_PATHS: [&str; 2] = ["signalman.toml", "/etc/signalman/config.toml"];

/// Default listen address.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Default alert attribute or label names that carry the component identity.
pub const DEFAULT_COMPONENT_KEYS: [&str; 6] = [
    "component",
    "service",
    "app",
    "application",
    "Service",
    "Component",
];

/// The environment variables this module reads, with the file key each one
/// overrides. One table, so the documentation can be checked against it.
pub const ENV_VARS: &[(&str, &str)] = &[
    ("SIGNALMAN_ADDR", "server.addr"),
    (
        "SIGNALMAN_MAX_CONCURRENT_TRIAGES",
        "server.max_concurrent_triages",
    ),
    ("SIGNALMAN_MAX_QUEUED_TRIAGES", "server.max_queued_triages"),
    (
        "SIGNALMAN_TRIAGE_TIMEOUT_SECONDS",
        "server.triage_timeout_seconds",
    ),
    ("TYPESAFE_BASE_URL", "typesafe.base_url"),
    ("TYPESAFE_DEFAULT_MODEL", "typesafe.model"),
    ("TYPESAFE_TIMEOUT_SECONDS", "typesafe.timeout_seconds"),
    ("INCIDENTIO_BASE_URL", "incidentio.base_url"),
    ("SIGNALMAN_MAX_CANDIDATES", "incidentio.max_candidates"),
    ("BACKSTAGE_BASE_URL", "backstage.base_url"),
    ("BACKSTAGE_APP_URL", "backstage.app_url"),
    ("BACKSTAGE_NAMESPACE", "backstage.namespace"),
    ("SIGNALMAN_COMPONENT_KEYS", "backstage.component_keys"),
    ("BACKSTAGE_NOTIFY", "backstage.notify"),
    ("SIGNALMAN_NOTE", "flow.note"),
    (
        "SIGNALMAN_RELATED_WINDOW_MINUTES",
        "flow.related_window_minutes",
    ),
    ("SIGNALMAN_RELATED_MAX", "flow.related_max"),
    (
        "SIGNALMAN_CHANGE_WINDOW_MINUTES",
        "flow.change_window_minutes",
    ),
    ("SIGNALMAN_CHANGE_MAX", "flow.change_max"),
];

/// Configuration failure. Every variant names what to fix.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The named file does not exist or cannot be read.
    #[error("cannot read configuration file {path}: {source}")]
    Read {
        /// The path tried.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML for [`Settings`] (unknown key, wrong type).
    #[error("invalid configuration file {path}:\n{source}")]
    Parse {
        /// The path parsed.
        path: PathBuf,
        /// TOML error with line and column.
        #[source]
        source: toml::de::Error,
    },
    /// An environment variable holds a value of the wrong shape.
    #[error("environment variable {var}={value:?} is not valid for {key}: {reason}")]
    Env {
        /// The variable.
        var: &'static str,
        /// Its raw value.
        value: String,
        /// The file key it maps to.
        key: &'static str,
        /// Why it was rejected.
        reason: String,
    },
    /// A resolved value is outside its valid range.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// File schema
// ---------------------------------------------------------------------------

/// The configuration file. Every field is optional; a missing field means
/// "use the next layer". Unknown keys are an error.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// `[server]`
    pub server: ServerFile,
    /// `[typesafe]`
    pub typesafe: TypesafeFile,
    /// `[incidentio]`
    pub incidentio: IncidentioFile,
    /// `[backstage]`
    pub backstage: BackstageFile,
    /// `[flow]`
    pub flow: FlowFile,
    /// `[policy]`: routing thresholds, file only.
    pub policy: Option<Policy>,
    /// `[triage]`: rubric text and the fallback team list, file only.
    pub triage: TriageFile,
}

/// `[server]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerFile {
    /// Listen address for `serve`.
    pub addr: Option<SocketAddr>,
    /// Triages running at once.
    pub max_concurrent_triages: Option<usize>,
    /// Triages waiting for a slot before deliveries are refused with 503.
    pub max_queued_triages: Option<usize>,
    /// Deadline for one triage, all upstream calls included.
    pub triage_timeout_seconds: Option<u64>,
}

/// `[typesafe]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypesafeFile {
    /// API base URL.
    pub base_url: Option<String>,
    /// Model name or alias.
    pub model: Option<String>,
    /// Per-attempt timeout.
    pub timeout_seconds: Option<u64>,
}

/// `[incidentio]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IncidentioFile {
    /// API base URL.
    pub base_url: Option<String>,
    /// Open incidents offered as dedup candidates.
    pub max_candidates: Option<usize>,
}

/// `[backstage]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackstageFile {
    /// Backend URL without `/api`. Setting it enables enrichment.
    pub base_url: Option<String>,
    /// Frontend URL for links; defaults to `base_url`.
    pub app_url: Option<String>,
    /// Namespace tried first for bare component names.
    pub namespace: Option<String>,
    /// Alert attribute or label names carrying the component identity.
    pub component_keys: Option<Vec<String>>,
    /// Notify the owning group after page, ticket and human-triage decisions.
    pub notify: Option<bool>,
}

/// `[flow]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlowFile {
    /// Write the qualification note.
    pub note: Option<bool>,
    /// Window for related firing alerts; `0` disables the lookup.
    pub related_window_minutes: Option<u64>,
    /// Cap on related alerts put in the state.
    pub related_max: Option<usize>,
    /// How far back a posted change may lie to be offered as a cause; `0`
    /// disables the lookup.
    pub change_window_minutes: Option<u64>,
    /// Cap on changes put in the state.
    pub change_max: Option<usize>,
}

/// `[triage]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriageFile {
    /// Question text overrides; any subset of [`Texts`].
    pub text: Option<TextsFile>,
    /// Fallback owner candidates when no catalog resolves. Replaces the
    /// built-in list entirely when present.
    pub teams: Option<Vec<OwnerCandidate>>,
}

/// `[triage.text]`: each field overrides the matching field of [`Texts`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextsFile {
    /// Owner question.
    pub owner_question: Option<String>,
    /// Owner guidance.
    pub owner_guidance: Option<String>,
    /// Owner guidance added when the catalog resolved the component.
    pub owner_catalog_guidance: Option<String>,
    /// Impact question.
    pub impact_question: Option<String>,
    /// Impact context added when related alerts exist.
    pub impact_related_context: Option<String>,
    /// The four impact levels, lowest first.
    pub impact_levels: Option<Vec<String>>,
    /// Actionable question.
    pub actionable_question: Option<String>,
    /// What yes means for actionable.
    pub actionable_yes: Option<String>,
    /// What no means for actionable.
    pub actionable_no: Option<String>,
    /// Duplicate question.
    pub duplicate_question: Option<String>,
    /// Rubric of the `none` dedup option.
    pub duplicate_none: Option<String>,
    /// Caused-by-change question.
    pub change_question: Option<String>,
}

impl TextsFile {
    /// Apply the overrides onto `base`.
    pub fn apply(self, mut base: Texts) -> Texts {
        macro_rules! take {
            ($($f:ident),+) => { $( if let Some(v) = self.$f { base.$f = v; } )+ };
        }
        take!(
            owner_question,
            owner_guidance,
            owner_catalog_guidance,
            impact_question,
            impact_related_context,
            impact_levels,
            actionable_question,
            actionable_yes,
            actionable_no,
            duplicate_question,
            duplicate_none,
            change_question
        );
        base
    }
}

impl Settings {
    /// Parse a TOML document.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        toml::from_str(text).map_err(|source| Error::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Load the file: `explicit` if given (must exist), else `SIGNALMAN_CONFIG`
    /// if set (must exist), else the first of [`DEFAULT_PATHS`] that exists,
    /// else empty settings. Returns the path used, if any.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let named = explicit.map(Path::to_path_buf).or_else(|| {
            std::env::var(FILE_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from)
        });
        let path = match named {
            Some(p) => p,
            None => match DEFAULT_PATHS.iter().map(Path::new).find(|p| p.is_file()) {
                Some(p) => p.to_owned(),
                None => return Ok((Self::default(), None)),
            },
        };
        let text = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        Ok((Self::parse(&text, &path)?, Some(path)))
    }
}

// ---------------------------------------------------------------------------
// Command-line layer
// ---------------------------------------------------------------------------

/// Values a command-line flag may set. `None` means the flag was absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    /// `--addr`
    pub addr: Option<SocketAddr>,
    /// `--model`
    pub model: Option<String>,
    /// `--notify-owners`
    pub notify_owners: Option<bool>,
    /// `--no-note` sets `Some(false)`.
    pub note: Option<bool>,
    /// `--related-window-minutes`
    pub related_window_minutes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Effective configuration
// ---------------------------------------------------------------------------

/// Everything the binary needs, after all layers. Serialises back to the
/// file schema so `config show` prints what is in effect.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// `[server]`
    pub server: Server,
    /// `[typesafe]`
    pub typesafe: Typesafe,
    /// `[incidentio]`
    pub incidentio: Incidentio,
    /// `[backstage]`
    pub backstage: Backstage,
    /// `[flow]`
    pub flow: Flow,
    /// `[policy]`
    pub policy: Policy,
    /// `[triage]`
    pub triage: Triage,
}

/// Effective `[server]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Server {
    /// Listen address.
    pub addr: SocketAddr,
    /// Triages running at once.
    pub max_concurrent_triages: usize,
    /// Triages waiting for a slot before deliveries are refused with 503.
    pub max_queued_triages: usize,
    /// Deadline for one triage in seconds.
    pub triage_timeout_seconds: u64,
}

impl Server {
    /// The triage deadline as a duration.
    pub fn triage_timeout(&self) -> Duration {
        Duration::from_secs(self.triage_timeout_seconds)
    }
}

/// Effective `[typesafe]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Typesafe {
    /// API base URL.
    pub base_url: String,
    /// Model name or alias.
    pub model: String,
    /// Per-attempt timeout.
    pub timeout_seconds: u64,
}

/// Effective `[incidentio]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Incidentio {
    /// API base URL.
    pub base_url: String,
    /// Dedup candidates offered.
    pub max_candidates: usize,
}

/// Effective `[backstage]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Backstage {
    /// Backend URL; `None` disables enrichment.
    pub base_url: Option<String>,
    /// Frontend URL for links.
    pub app_url: Option<String>,
    /// Namespace tried first.
    pub namespace: String,
    /// Component identity keys.
    pub component_keys: Vec<String>,
    /// Notify owners.
    pub notify: bool,
}

impl Backstage {
    /// Whether enrichment is on.
    pub fn enabled(&self) -> bool {
        self.base_url.is_some()
    }
}

/// Effective `[flow]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flow {
    /// Write the qualification note.
    pub note: bool,
    /// Related-alert window in minutes; `0` disables.
    pub related_window_minutes: u64,
    /// Cap on related alerts.
    pub related_max: usize,
    /// Change window in minutes; `0` disables.
    pub change_window_minutes: u64,
    /// Cap on changes.
    pub change_max: usize,
}

impl Flow {
    /// The related-alert window as a duration.
    pub fn related_window(&self) -> Duration {
        Duration::from_secs(self.related_window_minutes * 60)
    }

    /// The change window as a duration.
    pub fn change_window(&self) -> Duration {
        Duration::from_secs(self.change_window_minutes * 60)
    }
}

/// Effective `[triage]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Triage {
    /// Question text.
    pub text: Texts,
    /// Fallback owner candidates (without the no-match option, which
    /// [`OwnerCandidates::new`] appends).
    pub teams: Vec<OwnerCandidate>,
}

impl Triage {
    /// The fallback candidate set.
    pub fn fallback_candidates(&self) -> OwnerCandidates {
        OwnerCandidates::new(self.teams.clone())
    }
}

impl Config {
    /// Resolve every layer. `file` is the parsed configuration file (or
    /// `Settings::default()`), `cli` the flags that were given.
    #[allow(clippy::too_many_lines)] // one setting per statement, in file order; splitting hides the precedence
    pub fn resolve(file: &Settings, cli: &Overrides) -> Result<Self> {
        let server = Server {
            addr: cli
                .addr
                .or(env_parsed("SIGNALMAN_ADDR", "server.addr")?)
                .or(file.server.addr)
                .unwrap_or_else(|| {
                    DEFAULT_ADDR
                        .parse()
                        .unwrap_or_else(|_| unreachable!("default address is valid"))
                }),
            max_concurrent_triages: env_parsed(
                "SIGNALMAN_MAX_CONCURRENT_TRIAGES",
                "server.max_concurrent_triages",
            )?
            .or(file.server.max_concurrent_triages)
            .unwrap_or(crate::serve::DEFAULT_MAX_CONCURRENT),
            max_queued_triages: env_parsed(
                "SIGNALMAN_MAX_QUEUED_TRIAGES",
                "server.max_queued_triages",
            )?
            .or(file.server.max_queued_triages)
            .unwrap_or(crate::serve::DEFAULT_MAX_QUEUED),
            triage_timeout_seconds: env_parsed(
                "SIGNALMAN_TRIAGE_TIMEOUT_SECONDS",
                "server.triage_timeout_seconds",
            )?
            .or(file.server.triage_timeout_seconds)
            .unwrap_or(crate::serve::DEFAULT_TRIAGE_TIMEOUT.as_secs()),
        };
        if server.max_concurrent_triages == 0 {
            return Err(Error::Invalid(
                "server.max_concurrent_triages must be at least 1".into(),
            ));
        }
        if server.triage_timeout_seconds == 0 {
            return Err(Error::Invalid(
                "server.triage_timeout_seconds must be at least 1".into(),
            ));
        }
        let typesafe = Typesafe {
            base_url: env_string("TYPESAFE_BASE_URL")
                .or_else(|| file.typesafe.base_url.clone())
                .unwrap_or_else(|| crate::client::DEFAULT_BASE_URL.to_owned()),
            model: cli
                .model
                .clone()
                .or_else(|| env_string("TYPESAFE_DEFAULT_MODEL"))
                .or_else(|| file.typesafe.model.clone())
                .unwrap_or_else(|| crate::client::DEFAULT_MODEL.to_owned()),
            timeout_seconds: env_parsed("TYPESAFE_TIMEOUT_SECONDS", "typesafe.timeout_seconds")?
                .or(file.typesafe.timeout_seconds)
                .unwrap_or(crate::client::DEFAULT_TIMEOUT.as_secs()),
        };
        let incidentio = Incidentio {
            base_url: env_string("INCIDENTIO_BASE_URL")
                .or_else(|| file.incidentio.base_url.clone())
                .unwrap_or_else(|| crate::incidentio::client::DEFAULT_BASE_URL.to_owned()),
            max_candidates: env_parsed("SIGNALMAN_MAX_CANDIDATES", "incidentio.max_candidates")?
                .or(file.incidentio.max_candidates)
                .unwrap_or(crate::incidentio::sync::DEFAULT_CANDIDATES),
        };
        let base_url = env_string("BACKSTAGE_BASE_URL").or_else(|| file.backstage.base_url.clone());
        let backstage = Backstage {
            app_url: env_string("BACKSTAGE_APP_URL")
                .or_else(|| file.backstage.app_url.clone())
                .or_else(|| base_url.clone()),
            base_url,
            namespace: env_string("BACKSTAGE_NAMESPACE")
                .or_else(|| file.backstage.namespace.clone())
                .unwrap_or_else(|| "default".to_owned()),
            component_keys: env_list("SIGNALMAN_COMPONENT_KEYS")
                .or_else(|| file.backstage.component_keys.clone())
                .unwrap_or_else(|| DEFAULT_COMPONENT_KEYS.map(String::from).to_vec()),
            notify: cli
                .notify_owners
                .or(env_parsed("BACKSTAGE_NOTIFY", "backstage.notify")?)
                .or(file.backstage.notify)
                .unwrap_or(false),
        };
        let flow = Flow {
            note: cli
                .note
                .or(env_parsed("SIGNALMAN_NOTE", "flow.note")?)
                .or(file.flow.note)
                .unwrap_or(true),
            related_window_minutes: cli
                .related_window_minutes
                .or(env_parsed(
                    "SIGNALMAN_RELATED_WINDOW_MINUTES",
                    "flow.related_window_minutes",
                )?)
                .or(file.flow.related_window_minutes)
                .unwrap_or(crate::incidentio::sync::DEFAULT_RELATED_WINDOW.as_secs() / 60),
            related_max: env_parsed("SIGNALMAN_RELATED_MAX", "flow.related_max")?
                .or(file.flow.related_max)
                .unwrap_or(crate::incidentio::sync::DEFAULT_RELATED_MAX),
            change_window_minutes: env_parsed(
                "SIGNALMAN_CHANGE_WINDOW_MINUTES",
                "flow.change_window_minutes",
            )?
            .or(file.flow.change_window_minutes)
            .unwrap_or(crate::changes::DEFAULT_WINDOW.as_secs() / 60),
            change_max: env_parsed("SIGNALMAN_CHANGE_MAX", "flow.change_max")?
                .or(file.flow.change_max)
                .unwrap_or(crate::changes::DEFAULT_MAX),
        };
        let policy = file.policy.clone().unwrap_or_default();
        policy.validate().map_err(Error::Invalid)?;
        let text = file
            .triage
            .text
            .clone()
            .map_or_else(Texts::default, |t| t.apply(Texts::default()));
        text.validate().map_err(Error::Invalid)?;
        let teams = file
            .triage
            .teams
            .clone()
            .unwrap_or_else(crate::triage::default_teams);
        validate_teams(&teams)?;

        Ok(Self {
            server,
            typesafe,
            incidentio,
            backstage,
            flow,
            policy,
            triage: Triage { text, teams },
        })
    }

    /// Load the file and resolve in one step.
    pub fn load(explicit: Option<&Path>, cli: &Overrides) -> Result<(Self, Option<PathBuf>)> {
        let (settings, path) = Settings::load(explicit)?;
        Ok((Self::resolve(&settings, cli)?, path))
    }

    /// The environment variables from [`ENV_VARS`] that are set and non-empty.
    pub fn env_in_effect() -> Vec<&'static str> {
        ENV_VARS
            .iter()
            .filter(|(var, _)| env_string(var).is_some())
            .map(|(var, _)| *var)
            .collect()
    }
}

fn validate_teams(teams: &[OwnerCandidate]) -> Result<()> {
    if teams.is_empty() {
        return Err(Error::Invalid(
            "triage.teams must list at least one team".into(),
        ));
    }
    for t in teams {
        if t.key.trim().is_empty() || t.label.trim().is_empty() {
            return Err(Error::Invalid(
                "every entry of triage.teams needs a non-empty key and label".into(),
            ));
        }
        if t.key == crate::triage::NONE_OF_THESE {
            return Err(Error::Invalid(format!(
                "triage.teams must not define `{}`; it is appended automatically",
                crate::triage::NONE_OF_THESE
            )));
        }
    }
    Ok(())
}

/// A non-empty environment string. Empty counts as unset: Kubernetes and
/// shells often export a variable with no value.
fn env_string(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// A comma-separated list.
fn env_list(var: &str) -> Option<Vec<String>> {
    env_string(var).map(|raw| {
        raw.split(',')
            .map(|x| x.trim().to_owned())
            .filter(|x| !x.is_empty())
            .collect()
    })
}

/// A parsed value; a set but unparsable variable is an error, not a default.
fn env_parsed<T: std::str::FromStr>(var: &'static str, key: &'static str) -> Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match env_string(var) {
        None => Ok(None),
        Some(raw) => raw.parse().map(Some).map_err(|e: T::Err| Error::Env {
            var,
            value: raw,
            key,
            reason: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // Environment is process-global: these tests only assert layers that do
    // not need it set, and clear what they use. Integration tests cover env.

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let c = Config::resolve(&Settings::default(), &Overrides::default()).unwrap();
        assert_eq!(c.server.addr.to_string(), DEFAULT_ADDR);
        assert_eq!(c.server.max_concurrent_triages, 8);
        assert_eq!(c.server.max_queued_triages, 64);
        assert_eq!(c.server.triage_timeout_seconds, 60);
        assert_eq!(c.typesafe.model, crate::client::DEFAULT_MODEL);
        assert!(c.flow.note);
        assert_eq!(c.flow.related_window_minutes, 30);
        assert_eq!(c.flow.change_window_minutes, 120);
        assert_eq!(c.flow.change_max, 10);
        assert!(!c.backstage.enabled());
        assert_eq!(c.policy, Policy::default());
        assert_eq!(c.triage.teams, crate::triage::default_teams());
        assert_eq!(c.triage.text, Texts::default());
    }

    #[test]
    fn file_overrides_defaults_and_flags_override_the_file() {
        let s = Settings::parse(
            r#"
            [server]
            addr = "0.0.0.0:9000"
            [typesafe]
            model = "jev-1.13.0"
            [flow]
            note = false
            related_window_minutes = 10
            [policy]
            suppress_below = 0.1
            page_at = "outage"
            [triage.text]
            actionable_question = "Must someone act on `alert` now?"
            [[triage.teams]]
            key = "sre"
            label = "SRE"
            description = "Everything on call"
            "#,
            Path::new("t.toml"),
        )
        .unwrap();
        let cli = Overrides {
            addr: Some("127.0.0.1:1".parse().unwrap()),
            note: None,
            related_window_minutes: Some(5),
            ..Overrides::default()
        };
        let c = Config::resolve(&s, &cli).unwrap();
        assert_eq!(c.server.addr.to_string(), "127.0.0.1:1");
        assert_eq!(c.typesafe.model, "jev-1.13.0");
        assert!(!c.flow.note);
        assert_eq!(c.flow.related_window_minutes, 5);
        assert!((c.policy.suppress_below - 0.1).abs() < f64::EPSILON);
        assert_eq!(c.policy.page_at, crate::triage::Impact::Outage);
        assert!(
            (c.policy.attach_confidence - Policy::default().attach_confidence).abs() < f64::EPSILON
        );
        assert_eq!(
            c.triage.text.actionable_question,
            "Must someone act on `alert` now?"
        );
        assert_eq!(
            c.triage.text.owner_question,
            Texts::default().owner_question
        );
        assert_eq!(c.triage.teams.len(), 1);
        assert_eq!(c.triage.fallback_candidates().len(), 2);
    }

    #[test]
    fn unknown_keys_and_secrets_in_the_file_are_rejected() {
        let err = Settings::parse("[typesafe]\napi_key = \"sk\"\n", Path::new("t.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("api_key"), "{err}");
        let err = Settings::parse("[serverr]\naddr = \"x\"\n", Path::new("t.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("serverr"), "{err}");
    }

    #[test]
    fn invalid_ranges_are_named() {
        let s = Settings::parse(
            "[server]\nmax_concurrent_triages = 0\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let err = Config::resolve(&s, &Overrides::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("max_concurrent_triages"), "{err}");

        let s = Settings::parse("[policy]\nsuppress_below = 1.5\n", Path::new("t.toml")).unwrap();
        let err = Config::resolve(&s, &Overrides::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("suppress_below"), "{err}");

        let s = Settings::parse(
            "[triage.text]\nimpact_levels = [\"a\", \"b\"]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let err = Config::resolve(&s, &Overrides::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("impact_levels"), "{err}");

        let s = Settings::parse(
            "[[triage.teams]]\nkey = \"none_of_these\"\nlabel = \"x\"\ndescription = \"\"\n",
            Path::new("t.toml"),
        )
        .unwrap();
        assert!(Config::resolve(&s, &Overrides::default()).is_err());
    }

    #[test]
    fn effective_config_round_trips_through_toml() {
        let c = Config::resolve(&Settings::default(), &Overrides::default()).unwrap();
        let text = toml::to_string_pretty(&c).unwrap();
        let back = Settings::parse(&text, Path::new("effective.toml")).unwrap();
        assert_eq!(back.policy, Some(Policy::default()));
        assert_eq!(back.flow.note, Some(true));
    }
}
