//! CLI: triage alerts with TypeSafe, integrate with incident.io and Backstage,
//! serve webhooks.
//!
//! Every setting is resolved once by [`Config`] in this order, later wins:
//! built-in default, configuration file, environment variable, command-line
//! flag. Flags here are therefore all optional; their absence means "use the
//! next layer". Secrets never come from the file.

// Doc comments on the CLI types are `--help` text, shown verbatim: no backticks.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand};
use jiff::Timestamp;
use rmcp::ServiceExt;
use signalman::backstage::enrich::hints_from_labels;
use signalman::backstage::{self, Enricher};
use signalman::changes::{ChangeLog, FeedToken};
use signalman::config::{Config, McpTransport, Overrides};
use signalman::eval;
use signalman::incidentio::types::{AlertEvent, AlertEventAck, AlertStatus};
use signalman::incidentio::webhook::WebhookSecret;
use signalman::incidentio::{self, Triager, WriteBack};
use signalman::mcp::http::McpToken;
use signalman::outcome::{self, AlertRef, IncidentRef, Outcome};
use signalman::readiness::Probe;
use signalman::serve::{AppState, Limits, router};
use signalman::telemetry::Providers;
use signalman::triage::{Alert, Decision, OpenIncident, TriageAnswers, TriageQuestions, decide};
use signalman::{Client, Request, Response};

#[derive(Parser)]
#[command(
    name = "signalman",
    version,
    about = "TypeSafe-powered alert triage with incident.io and Backstage",
    after_help = "Precedence, lowest to highest: built-in default, configuration file, \
environment variable, flag. Secrets (TYPESAFE_API_KEY, INCIDENTIO_API_KEY, \
INCIDENTIO_WEBHOOK_SECRET, BACKSTAGE_TOKEN, SIGNALMAN_CHANGES_TOKEN, \
SIGNALMAN_MCP_TOKEN) are environment only."
)]
struct Cli {
    /// Configuration file (TOML). Otherwise SIGNALMAN_CONFIG, then
    /// ./signalman.toml, then /etc/signalman/config.toml when present.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decide what to do with an alert (JSON file, or `-` for stdin).
    Triage(TriageArgs),
    /// Replay labelled alerts and grade every judgment and decision.
    Eval(EvalArgs),
    /// List the TypeSafe model names this account may use.
    Models,
    /// Receive incident.io webhooks and triage new alerts.
    Serve {
        /// Listen address [file: server.addr, env: SIGNALMAN_ADDR, default: 127.0.0.1:8080].
        #[arg(long)]
        addr: Option<SocketAddr>,
        /// Accept unsigned deliveries. Local development only.
        #[arg(long)]
        insecure_skip_verify: bool,
        /// Compute decisions but write nothing back to incident.io or Backstage.
        #[arg(long)]
        dry_run: bool,
        /// Notify the owning group through Backstage after page, ticket and
        /// human-triage decisions [file: backstage.notify, env: BACKSTAGE_NOTIFY].
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_name = "BOOL")]
        notify_owners: Option<bool>,
        #[command(flatten)]
        flow: FlowArgs,
    },
    /// Serve signalman's tools over the Model Context Protocol (decision
    /// 0008): qualify_alert, related_alerts, recent_changes, lookup_owner,
    /// open_incidents, all read-only, plus apply_qualification when
    /// mcp.allow_write is on. Over stdio, or over Streamable HTTP at /mcp on
    /// mcp.bind_address behind SIGNALMAN_MCP_TOKEN. `serve` also mounts /mcp
    /// when that token is set. [file: mcp.enabled, mcp.transport,
    /// mcp.bind_address, mcp.allow_write; env: SIGNALMAN_MCP_ENABLED,
    /// SIGNALMAN_MCP_TRANSPORT, SIGNALMAN_MCP_BIND_ADDRESS,
    /// SIGNALMAN_MCP_ALLOW_WRITE].
    Mcp,
    /// incident.io utilities.
    #[command(subcommand)]
    Incidentio(IncidentIoCommand),
    /// Backstage catalog utilities.
    #[command(subcommand)]
    Backstage(BackstageCommand),
    /// Configuration utilities.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Print a JSON Schema this tool publishes.
    #[command(subcommand)]
    Schema(SchemaCommand),
}

#[derive(Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
struct TriageArgs {
    /// Path to an alert JSON document.
    alert: PathBuf,
    /// Model name or alias [file: typesafe.model, env: TYPESAFE_DEFAULT_MODEL, default: jev-latest].
    #[arg(long)]
    model: Option<String>,
    /// Replace `open_incidents` with the live incidents from incident.io.
    #[arg(long)]
    dedup_from_incidentio: bool,
    /// Resolve the component named by the alert labels in the Backstage
    /// catalog; use its owner group and neighbours as owner candidates and
    /// its TechDocs runbook as `runbook`.
    #[arg(long)]
    enrich_from_backstage: bool,
    /// After deciding, post the alert with its judgments as metadata to the
    /// incident.io HTTP alert source [file: incidentio.alert_source_config_id,
    /// env: INCIDENTIO_ALERT_SOURCE_CONFIG_ID], authenticated with
    /// INCIDENTIO_ALERT_SOURCE_TOKEN.
    #[arg(long)]
    forward_to_incidentio: bool,
    /// Print the TypeSafe request body and exit without calling the model.
    #[arg(long)]
    print_request: bool,
    /// Emit the outcome contract (schema v1) as JSON instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct EvalArgs {
    /// JSON Lines file of cases: {id, alert, expected}. See examples/eval/cases.jsonl.
    cases: PathBuf,
    /// Model name or alias [file: typesafe.model, env: TYPESAFE_DEFAULT_MODEL, default: jev-latest].
    #[arg(long)]
    model: Option<String>,
    /// Write every graded model response to DIR/<id>.json, and the cases
    /// whose answer did not fit to DIR/failed.jsonl, for later --replay.
    #[arg(long, value_name = "DIR", conflicts_with = "replay")]
    record: Option<PathBuf>,
    /// Grade recorded responses from DIR under the current configuration
    /// instead of calling the model. Tune [policy] and question or guidance
    /// wording this way, but not impact_levels or team keys: the recorded
    /// answers echo the levels and choose among the keys they were asked
    /// with, so a replay under others fails naming the question; changing
    /// those needs a new recording.
    #[arg(long, value_name = "DIR")]
    replay: Option<PathBuf>,
    /// Emit the full report as JSON instead of text.
    #[arg(long)]
    json: bool,
}

/// Knobs of the incident.io flow shared by `serve` and `incidentio triage-alert`.
#[derive(Args, Clone, Default)]
struct FlowArgs {
    /// Do not write the qualification note [file: flow.note, env: SIGNALMAN_NOTE].
    #[arg(long, action = ArgAction::SetTrue)]
    no_note: bool,
    /// Minutes back to look for other firing alerts; 0 disables the lookup
    /// [file: flow.related_window_minutes, env: SIGNALMAN_RELATED_WINDOW_MINUTES, default: 30].
    #[arg(long, value_name = "MINUTES")]
    related_window_minutes: Option<u64>,
}

impl FlowArgs {
    fn overrides(&self) -> Overrides {
        Overrides {
            note: self.no_note.then_some(false),
            related_window_minutes: self.related_window_minutes,
            ..Overrides::default()
        }
    }
}

#[derive(Subcommand)]
enum IncidentIoCommand {
    /// Show which API key is configured and its roles.
    Whoami,
    /// List the incidents offered as dedup candidates.
    OpenIncidents {
        /// Maximum to fetch [file: incidentio.max_candidates, env: SIGNALMAN_MAX_CANDIDATES, default: 40].
        #[arg(long)]
        max: Option<usize>,
    },
    /// Run the webhook flow for one existing alert by id.
    TriageAlert {
        /// incident.io alert id.
        alert_id: String,
        /// Compute but write nothing back.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        flow: FlowArgs,
    },
}

#[derive(Subcommand)]
enum BackstageCommand {
    /// Resolve a component name or entity ref and show what the catalog
    /// contributes to a triage: context, owner candidates, runbook excerpt.
    Lookup {
        /// Component name, `kind:namespace/name`, or a title.
        hint: String,
        /// Alert text used to pick the runbook page and mentioned components.
        #[arg(long, default_value = "")]
        text: String,
    },
}

#[derive(Subcommand)]
enum SchemaCommand {
    /// The outcome contract every triage emits (schema v1). Regenerate the
    /// committed copy with `mise run schema`.
    Outcome,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the effective configuration as TOML, after every layer, with the
    /// file and environment variables that contributed. Exits non-zero when
    /// the file or an environment value is invalid: run it in CI against the
    /// ConfigMap before rolling it out.
    Show,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Answered before the configuration is resolved: `mise run schema`
    // regenerates the committed contract, and an unrelated configuration
    // error must not be able to fail it.
    if let Command::Schema(cmd) = &cli.command {
        return exit(schema_cmd(cmd));
    }
    let (cfg, file) = match Config::load(cli.config.as_deref(), &overrides(&cli.command)) {
        Ok(loaded) => loaded,
        Err(err) => return exit(Err(err.into())),
    };
    // Logging and, when an endpoint is configured, OTLP export: installed
    // once the configuration says how, before the first span.
    let providers = match Providers::init(&cfg.telemetry.settings()) {
        Ok(p) => p,
        Err(err) => return exit(Err(err.into())),
    };
    tracing::debug!(file = ?file, "configuration resolved");

    let result = run(cli.command, &cfg, file.as_deref()).await;
    // Flush the last spans and measurements before the process ends, or a
    // one-shot `triage` run exports nothing. Blocking, so off the runtime.
    let _ = tokio::task::spawn_blocking(move || providers.shutdown()).await;
    exit(result)
}

fn exit(result: Result<(), AnyError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            let mut source = err.source();
            while let Some(s) = source {
                eprintln!("  caused by: {s}");
                source = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

type AnyError = Box<dyn std::error::Error>;

/// The flags of each command that override the lower layers.
fn overrides(command: &Command) -> Overrides {
    match command {
        Command::Serve {
            addr,
            notify_owners,
            flow,
            ..
        } => Overrides {
            addr: *addr,
            notify_owners: *notify_owners,
            ..flow.overrides()
        },
        Command::Triage(TriageArgs { model, .. }) | Command::Eval(EvalArgs { model, .. }) => {
            Overrides {
                model: model.clone(),
                ..Overrides::default()
            }
        }
        Command::Incidentio(IncidentIoCommand::TriageAlert { flow, .. }) => flow.overrides(),
        Command::Models
        | Command::Mcp
        | Command::Incidentio(_)
        | Command::Backstage(_)
        | Command::Config(_)
        | Command::Schema(_) => Overrides::default(),
    }
}

/// Print a JSON Schema this tool publishes.
///
/// Generated from the wire types; it reads no configuration, which is why
/// `run` answers it before resolving one.
fn schema_cmd(cmd: &SchemaCommand) -> Result<(), AnyError> {
    match cmd {
        SchemaCommand::Outcome => {
            println!("{}", serde_json::to_string_pretty(&Outcome::schema())?);
            Ok(())
        }
    }
}

async fn run(command: Command, cfg: &Config, file: Option<&Path>) -> Result<(), AnyError> {
    match command {
        Command::Triage(args) => triage(cfg, args).await,
        Command::Eval(args) => evaluate(cfg, args).await,
        Command::Models => {
            let client = typesafe_client(cfg)?;
            for m in client.list_models().await? {
                println!("{:<16} {:<12} {}", m.name, m.release_date, m.description);
            }
            Ok(())
        }
        Command::Serve {
            insecure_skip_verify,
            dry_run,
            ..
        } => serve(cfg, insecure_skip_verify, dry_run).await,
        Command::Mcp => mcp_cmd(cfg).await,
        Command::Incidentio(cmd) => incidentio_cmd(cfg, cmd).await,
        Command::Backstage(cmd) => backstage_cmd(cfg, cmd).await,
        // Unreachable: `main` answers it before resolving a configuration.
        // Dispatched through the same function anyway, so this arm cannot
        // drift from that one.
        Command::Schema(cmd) => schema_cmd(&cmd),
        Command::Config(ConfigCommand::Show) => {
            println!("# signalman effective configuration");
            match file {
                Some(p) => println!("# file: {}", p.display()),
                None => println!("# file: none (defaults, environment and flags only)"),
            }
            let env = Config::env_in_effect();
            if env.is_empty() {
                println!("# environment: none of the configuration variables is set");
            } else {
                println!("# environment: {}", env.join(", "));
            }
            println!("# secrets are read from the environment and never shown here");
            println!();
            print!("{}", toml::to_string_pretty(cfg)?);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Clients from configuration
// ---------------------------------------------------------------------------

fn typesafe_client(cfg: &Config) -> Result<Client, AnyError> {
    Ok(Client::builder()
        .base_url(cfg.typesafe.base_url.clone())
        .model(cfg.typesafe.model.clone())
        .timeout(Duration::from_secs(cfg.typesafe.timeout_seconds))
        .build()?)
}

fn incidentio_client(cfg: &Config) -> Result<incidentio::Client, AnyError> {
    Ok(incidentio::Client::builder()
        .base_url(cfg.incidentio.base_url.clone())
        .build()?)
}

/// The catalog enricher, when Backstage is configured.
fn enricher(cfg: &Config) -> Result<Option<Enricher>, AnyError> {
    let Some(base_url) = &cfg.backstage.base_url else {
        return Ok(None);
    };
    let client = backstage::Client::builder()
        .base_url(base_url.clone())
        .build()?;
    let mut e = Enricher::new(client)
        .with_namespace(cfg.backstage.namespace.clone())
        .with_group_types(cfg.backstage.group_types.clone());
    if let Some(app) = &cfg.backstage.app_url {
        e = e.with_app_url(app.clone());
    }
    Ok(Some(e))
}

/// The incident.io flow, fully configured.
fn triager(cfg: &Config, io: incidentio::Client, dry_run: bool) -> Result<Triager, AnyError> {
    let mut t = Triager::new(typesafe_client(cfg)?, io);
    t.backstage = enricher(cfg)?;
    if t.backstage.is_none() && cfg.backstage.notify {
        return Err("backstage.notify needs backstage.base_url (or BACKSTAGE_BASE_URL)".into());
    }
    t.notify_owner = cfg.backstage.notify;
    t.policy = cfg.policy.clone();
    t.texts = cfg.triage.text.clone();
    t.fallback_owners = cfg.triage.fallback_candidates();
    t.max_candidates = cfg.incidentio.max_candidates;
    t.component_keys.clone_from(&cfg.backstage.component_keys);
    t.note = cfg.flow.note;
    t.related_window = cfg.flow.related_window();
    t.related_max = cfg.flow.related_max;
    t.change_window = cfg.flow.change_window();
    t.change_max = cfg.flow.change_max;
    if dry_run {
        t.write_back = WriteBack::DryRun;
    }
    Ok(t)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn triage(cfg: &Config, args: TriageArgs) -> Result<(), AnyError> {
    let raw = if args.alert.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        tokio::fs::read_to_string(&args.alert).await?
    };
    let mut alert: Alert = serde_json::from_str(&raw)?;

    let io_client = if args.dedup_from_incidentio || args.forward_to_incidentio {
        Some(incidentio_client(cfg)?)
    } else {
        None
    };

    // Kept so the outcome can carry each candidate's id and permalink; the
    // alert itself only ever names references.
    let mut fetched_incidents: Vec<incidentio::Incident> = Vec::new();
    if args.dedup_from_incidentio {
        let client = io_client.as_ref().ok_or("incident.io client missing")?;
        fetched_incidents = dedup_candidates(cfg, client, &mut alert).await?;
    }

    let mut candidates = cfg.triage.fallback_candidates();
    let mut component_url = None;
    let mut runbook_url = None;
    let mut candidate_urls = BTreeMap::new();
    if args.enrich_from_backstage {
        let enricher = enricher(cfg)?.ok_or(
            "--enrich-from-backstage needs backstage.base_url in the file or BACKSTAGE_BASE_URL",
        )?;
        let hints = hints_from_labels(&alert.labels, &cfg.backstage.component_keys);
        let text = format!("{} {}", alert.title, alert.description);
        let enrichment = enricher.enrich(&hints, &text).await?;
        tracing::info!(
            component = ?enrichment.component.as_ref().map(|c| &c.name),
            matched_by = ?enrichment.matched_by,
            owner_candidates = enrichment.candidates.len(),
            runbook = enrichment.runbook.is_some(),
            "catalog enrichment"
        );
        if enrichment.runbook.is_some() {
            alert.runbook = enrichment.runbook;
        }
        alert.component = enrichment.component;
        component_url = enrichment.component_url;
        runbook_url = enrichment.runbook_url;
        candidates = enrichment.candidates;
        candidate_urls = enricher.candidate_urls(&candidates);
    }

    let questions = TriageQuestions::for_alert_with_texts(&alert, candidates, &cfg.triage.text)?;
    let state = TriageQuestions::state(&alert);
    let request = Request {
        state: &state,
        model: &cfg.typesafe.model,
        questions: &questions.questions,
    };

    if args.print_request {
        println!("{}", serde_json::to_string_pretty(&request)?);
        return Ok(());
    }

    let client = typesafe_client(cfg)?;
    let response = client.evaluate(&request).await?;
    let answers = questions.read(&response)?;
    let decision = decide(&answers, &cfg.policy);

    let mut forwarded = None;
    if args.forward_to_incidentio {
        let client = io_client.as_ref().ok_or("incident.io client missing")?;
        forwarded = Some(forward(cfg, client, &alert, &answers, &decision, &response.model).await?);
    }

    if args.json {
        let outcome = triage_outcome(TriageOutcome {
            cfg,
            alert: &alert,
            answers: &answers,
            decision: &decision,
            response: &response,
            fetched_incidents: &fetched_incidents,
            candidate_urls: &candidate_urls,
            component_url: component_url.clone(),
            runbook_url: runbook_url.clone(),
            forwarded: forwarded.as_ref(),
        });
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        print_report(&alert, &answers, &decision, &response);
        if let Some(ack) = forwarded {
            println!(
                "forwarded  incident.io accepted event (dedup key {})",
                ack.deduplication_key
            );
        }
    }
    Ok(())
}

/// Replace the alert's `open_incidents` with the incidents that are live in
/// incident.io right now, and hand them back so the outcome can carry each
/// candidate's id and permalink.
async fn dedup_candidates(
    cfg: &Config,
    client: &incidentio::Client,
    alert: &mut Alert,
) -> Result<Vec<incidentio::Incident>, AnyError> {
    let incidents = client
        .list_open_incidents(cfg.incidentio.max_candidates)
        .await?;
    tracing::info!(
        count = incidents.len(),
        "loaded dedup candidates from incident.io"
    );
    alert.open_incidents = incidents
        .iter()
        .map(|i| OpenIncident {
            id: i.reference.clone(),
            summary: i.candidate_summary(),
        })
        .collect();
    Ok(incidents)
}

/// Post the alert with its judgments to the configured incident.io HTTP
/// alert source, and let incident.io's alert routes escalate.
async fn forward(
    cfg: &Config,
    client: &incidentio::Client,
    alert: &Alert,
    answers: &TriageAnswers,
    decision: &Decision,
    model: &str,
) -> Result<AlertEventAck, AnyError> {
    let source_id = cfg.incidentio.alert_source_config_id.as_deref().ok_or(
        "set incidentio.alert_source_config_id in the file or INCIDENTIO_ALERT_SOURCE_CONFIG_ID to forward alerts",
    )?;
    let token = incidentio::Client::alert_source_token()
        .ok_or("set INCIDENTIO_ALERT_SOURCE_TOKEN to forward alerts")?;
    let event = alert_event(alert, answers, decision, model);
    let ack = client.send_alert_event(source_id, &token, &event).await?;
    tracing::info!(dedup_key = %ack.deduplication_key, status = %ack.status, "forwarded to incident.io");
    Ok(ack)
}

/// What the file-based CLI has gathered by the time it can build an
/// [`Outcome`]. A struct because the pieces come from four optional flags.
struct TriageOutcome<'a> {
    cfg: &'a Config,
    alert: &'a Alert,
    answers: &'a TriageAnswers,
    decision: &'a Decision,
    response: &'a Response,
    fetched_incidents: &'a [incidentio::Incident],
    candidate_urls: &'a BTreeMap<String, String>,
    component_url: Option<String>,
    runbook_url: Option<String>,
    forwarded: Option<&'a AlertEventAck>,
}

/// The same contract the webhook flow emits, built from a file-based run.
/// Nothing was written back, so `writes.mode` is `detached`.
fn triage_outcome(input: TriageOutcome<'_>) -> Outcome {
    let TriageOutcome {
        cfg,
        alert,
        answers,
        decision,
        response,
        fetched_incidents,
        candidate_urls,
        component_url,
        runbook_url,
        forwarded,
    } = input;

    let candidate_incidents: Vec<IncidentRef> = alert
        .open_incidents
        .iter()
        .map(|open| {
            let known = fetched_incidents.iter().find(|i| i.reference == open.id);
            IncidentRef {
                id: known.map(|i| i.id.clone()),
                reference: open.id.clone(),
                url: known.and_then(|i| i.permalink.clone()),
            }
        })
        .collect();
    let incident = match decision {
        Decision::AttachToIncident { incident_id, .. } => Some(
            candidate_incidents
                .iter()
                .find(|i| &i.reference == incident_id)
                .cloned()
                .unwrap_or_else(|| IncidentRef {
                    id: None,
                    reference: incident_id.clone(),
                    url: None,
                }),
        ),
        _ => None,
    };
    let owner_url = decision
        .owner()
        .and_then(|owner| candidate_urls.get(&owner.key).cloned());
    let tags = incidentio::sync::tags_for(answers, decision);

    Outcome::build(outcome::Input {
        version: env!("CARGO_PKG_VERSION"),
        decided_at: Timestamp::now(),
        // A file carries no creation time, so there is nothing to measure from.
        time_to_qualify: None,
        alert: AlertRef {
            id: None,
            title: alert.title.clone(),
            source: alert.source.clone(),
            source_url: alert.labels.get("source_url").cloned(),
            created_at: None,
            labels: alert.labels.clone(),
        },
        answers,
        decision,
        policy: &cfg.policy,
        owner_url,
        candidate_urls,
        incident,
        candidate_incidents: &candidate_incidents,
        component: alert.component.as_ref(),
        component_url,
        runbook_url,
        related: &alert.related_alerts,
        changes: outcome::Changes::Lines(&alert.recent_changes),
        // Neither lookup exists on this path: the file is the whole world.
        windows: outcome::Windows {
            related_seconds: None,
            change_seconds: None,
        },
        tags: &tags,
        model: &response.model,
        usage: response.usage.into(),
        writes: outcome::Writes {
            forwarded: forwarded.map(|ack| outcome::Forwarded {
                deduplication_key: ack.deduplication_key.clone(),
                status: ack.status.clone(),
            }),
            ..outcome::Writes::detached()
        },
    })
}

async fn evaluate(cfg: &Config, args: EvalArgs) -> Result<(), AnyError> {
    let cases = eval::read_cases(&args.cases)?;
    let candidates = cfg.triage.fallback_candidates();
    let setup = eval::Setup {
        texts: &cfg.triage.text,
        candidates: &candidates,
        policy: &cfg.policy,
    };
    let report = if let Some(dir) = &args.replay {
        eval::replay(dir, &cases, &setup)?
    } else {
        let client = typesafe_client(cfg)?;
        eval::run(
            &client,
            &cfg.typesafe.model,
            &cases,
            &setup,
            args.record.as_deref(),
        )
        .await?
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render());
        if let Some(dir) = &args.record {
            println!(
                "\nrecorded {} responses under {}",
                report.cases,
                dir.display()
            );
            if !report.failed.is_empty() {
                println!(
                    "listed {} failed cases in {}, which a replay reports as failed",
                    report.failed.len(),
                    dir.join(eval::FAILED_FILE).display()
                );
            }
        }
    }
    Ok(())
}

/// Shape the alert for an incident.io HTTP alert source. The judgments go in
/// `metadata`, where the source's attribute templates can map them to alert
/// attributes for routing.
fn alert_event(
    alert: &Alert,
    answers: &TriageAnswers,
    decision: &Decision,
    model: &str,
) -> AlertEvent {
    let dedup = alert
        .labels
        .get("dedup_key")
        .cloned()
        .unwrap_or_else(|| format!("{}:{}", alert.source, alert.title));
    AlertEvent {
        title: alert.title.clone(),
        description: Some(alert.description.clone()),
        status: AlertStatus::Firing,
        deduplication_key: Some(dedup),
        source_url: alert.labels.get("source_url").cloned(),
        metadata: Some(serde_json::json!({
            "source": alert.source,
            "labels": alert.labels,
            "component": alert.component.as_ref().map(|c| &c.name),
            "ai": {
                "team": answers.owner.chosen,
                "team_confidence": answers.owner.confidence.value(),
                "impact_level": answers.impact.nearest_level(),
                "impact_label": answers.impact.nearest_label(),
                "impact_score": answers.impact.value,
                "actionable": answers.actionable.yes.value(),
                "decision": decision,
                "model": model,
            }
        })),
    }
}

async fn serve(cfg: &Config, insecure_skip_verify: bool, dry_run: bool) -> Result<(), AnyError> {
    let secret = if insecure_skip_verify {
        None
    } else {
        Some(WebhookSecret::from_env()?)
    };
    let mut triager = triager(cfg, incidentio_client(cfg)?, dry_run)?;
    if triager.backstage.is_some() {
        tracing::info!(
            notify_owners = triager.notify_owner,
            "Backstage catalog enrichment enabled"
        );
    }
    let changes_token = FeedToken::from_env();
    if changes_token.is_some() {
        triager.changes = Some(ChangeLog::default());
        tracing::info!(
            window_minutes = cfg.flow.change_window_minutes,
            "change feed enabled at POST /changes"
        );
    } else {
        tracing::info!("change feed disabled: SIGNALMAN_CHANGES_TOKEN is not set");
    }
    let addr = cfg.server.addr;
    let limits = Limits {
        max_concurrent: cfg.server.max_concurrent_triages,
        max_queued: cfg.server.max_queued_triages,
        timeout: cfg.server.triage_timeout(),
    };
    // The MCP server gets its own clone: `Server::new` forces dry run on it,
    // and the change log inside is shared, so `recent_changes` over HTTP sees
    // what `POST /changes` records here.
    let mcp_triager = triager.clone();
    let mut state = AppState::with_limits(secret, triager, limits);
    state.changes_token = changes_token;
    state.readiness = Probe::new(&state.triager, cfg.server.readiness());
    let mut app = router(Arc::new(state));
    match (cfg.mcp.enabled, McpToken::from_env()) {
        (true, Some(token)) => {
            app = app.merge(signalman::mcp::http::router(
                signalman::mcp::Server::new(mcp_triager, cfg.mcp.allow_write),
                token,
                &cfg.mcp.allowed_hosts,
            ));
            tracing::info!(
                allow_write = cfg.mcp.allow_write,
                "MCP server mounted at /mcp over Streamable HTTP (decision 0008)"
            );
        }
        (true, None) => {
            tracing::info!("MCP over HTTP not mounted: SIGNALMAN_MCP_TOKEN is not set");
        }
        (false, _) => tracing::info!("MCP over HTTP not mounted: mcp.enabled is false"),
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, dry_run, note = cfg.flow.note, related_window_minutes = cfg.flow.related_window_minutes, max_concurrent = limits.max_concurrent, max_queued = limits.max_queued, timeout_seconds = limits.timeout.as_secs(), readiness_cache_seconds = cfg.server.readiness_cache_seconds, "listening for incident.io webhooks at /webhooks/incidentio; /healthz and /readyz");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Serve the MCP tools on their own, over stdio or Streamable HTTP.
/// `qualify_alert` is always dry run: [`signalman::mcp::Server::new`] forces
/// it on the wrapped `Triager` regardless of `cfg`, and this function never
/// passes a `dry_run` flag to `triager` either, so the invariant holds twice
/// over. `apply_qualification` is gated separately by `cfg.mcp.allow_write`.
///
/// The HTTP transport here is MCP-only: no webhook route and no change
/// feed, so `recent_changes` is empty exactly as over stdio. A process that
/// needs the feed runs `serve`, which mounts `/mcp` next to `POST /changes`.
async fn mcp_cmd(cfg: &Config) -> Result<(), AnyError> {
    if !cfg.mcp.enabled {
        return Err("mcp.enabled is false".into());
    }
    let triager = triager(cfg, incidentio_client(cfg)?, true)?;
    let server = signalman::mcp::Server::new(triager, cfg.mcp.allow_write);
    match cfg.mcp.transport {
        McpTransport::Stdio => {
            tracing::info!(
                allow_write = cfg.mcp.allow_write,
                "MCP server listening on stdio (decision 0008)"
            );
            let service = server
                .serve(rmcp::transport::stdio())
                .await
                .inspect_err(|e| tracing::error!(error = %e, "MCP server failed to start"))?;
            service.waiting().await?;
        }
        McpTransport::Http => {
            let Some(addr) = cfg.mcp.bind_address else {
                return Err(concat!(
                    "mcp.transport = \"http\" needs a listen address: set mcp.bind_address ",
                    "(SIGNALMAN_MCP_BIND_ADDRESS), for example \"0.0.0.0:8081\""
                )
                .into());
            };
            let token = McpToken::from_env().ok_or_else(|| {
                format!(
                    "{} is not set; the HTTP transport requires a bearer token",
                    signalman::mcp::http::TOKEN_ENV
                )
            })?;
            let app = axum::Router::new()
                .route("/healthz", axum::routing::get(|| async { "ok" }))
                .merge(signalman::mcp::http::router(
                    server,
                    token,
                    &cfg.mcp.allowed_hosts,
                ));
            let listener = tokio::net::TcpListener::bind(addr).await?;
            tracing::info!(
                %addr,
                allow_write = cfg.mcp.allow_write,
                "MCP server listening at /mcp over Streamable HTTP (decision 0008); no change feed here, run `serve` to share one"
            );
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

async fn incidentio_cmd(cfg: &Config, cmd: IncidentIoCommand) -> Result<(), AnyError> {
    let io = incidentio_client(cfg)?;
    match cmd {
        IncidentIoCommand::Whoami => {
            let id = io.identity().await?;
            println!("key    {}", id.name);
            println!("roles  {}", id.roles.join(", "));
            if let Some(url) = id.dashboard_url {
                println!("org    {url}");
            }
        }
        IncidentIoCommand::OpenIncidents { max } => {
            for inc in io
                .list_open_incidents(max.unwrap_or(cfg.incidentio.max_candidates))
                .await?
            {
                println!(
                    "{:<10} {:<10} {}",
                    inc.reference,
                    inc.incident_status.name,
                    inc.candidate_summary()
                );
            }
        }
        IncidentIoCommand::TriageAlert {
            alert_id, dry_run, ..
        } => {
            let triager = triager(cfg, io, dry_run)?;
            let outcome = triager.triage_alert_by_id(&alert_id).await?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
    }
    Ok(())
}

async fn backstage_cmd(cfg: &Config, cmd: BackstageCommand) -> Result<(), AnyError> {
    match cmd {
        BackstageCommand::Lookup { hint, text } => {
            let enricher = enricher(cfg)?.ok_or(
                "Backstage is not configured: set backstage.base_url in the file or BACKSTAGE_BASE_URL",
            )?;
            let enrichment = enricher.enrich(std::slice::from_ref(&hint), &text).await?;
            match &enrichment.component {
                Some(c) => {
                    println!(
                        "component  {} ({})",
                        c.name,
                        enrichment.matched_by.as_deref().unwrap_or("-")
                    );
                    println!("owner      {}", c.owner.as_deref().unwrap_or("-"));
                    println!(
                        "lifecycle  {}  type {}  system {}",
                        c.lifecycle.as_deref().unwrap_or("-"),
                        c.component_type.as_deref().unwrap_or("-"),
                        c.system.as_deref().unwrap_or("-")
                    );
                    println!(
                        "depends on {}",
                        if c.depends_on.is_empty() {
                            "-".to_owned()
                        } else {
                            c.depends_on.join(", ")
                        }
                    );
                    println!(
                        "dependents {}",
                        if c.dependents.is_empty() {
                            "-".to_owned()
                        } else {
                            c.dependents.join(", ")
                        }
                    );
                }
                None => println!("component  not found for {hint:?}; offering all teams"),
            }
            println!("owner candidates ({}):", enrichment.candidates.len());
            for cand in enrichment.candidates.iter() {
                println!(
                    "  {:<24} {}",
                    cand.key,
                    cand.description.chars().take(90).collect::<String>()
                );
            }
            match &enrichment.runbook {
                Some(r) => println!("runbook    {}", r.chars().take(300).collect::<String>()),
                None => println!("runbook    none (no TechDocs or no matching page)"),
            }
        }
    }
    Ok(())
}

fn print_report(alert: &Alert, answers: &TriageAnswers, decision: &Decision, response: &Response) {
    println!("alert     {}  ({})", alert.title, alert.source);
    println!("model     {}", response.model);
    if let Some(c) = &alert.component {
        println!(
            "component {}  catalog owner: {}",
            c.name,
            c.owner.as_deref().unwrap_or("-")
        );
    }
    let owner_label = answers
        .candidates
        .get(&answers.owner.chosen)
        .map_or(answers.owner.chosen.as_str(), |c| c.label.as_str());
    println!(
        "owner     {:<24} confidence {}",
        owner_label, answers.owner.confidence
    );
    println!(
        "impact    {:.2} → {}  (confidence {})",
        answers.impact.value,
        answers.impact.nearest_label(),
        answers.impact.confidence
    );
    println!("actionable p={}", answers.actionable.yes);
    if let Some(d) = &answers.duplicate_of {
        println!("duplicate {:<24} confidence {}", d.chosen, d.confidence);
    }
    if let Some(c) = &answers.caused_by_change {
        println!("caused by recent change p={}", c.yes);
    }
    println!(
        "tokens    in={} out={}",
        response.usage.input_tokens, response.usage.output_tokens
    );
    println!();
    println!("decision  {decision:?}");
}
