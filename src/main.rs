//! CLI: triage alerts with TypeSafe, integrate with incident.io and Backstage,
//! serve webhooks.
//!
//! Every setting is resolved once by [`Config`] in this order, later wins:
//! built-in default, configuration file, environment variable, command-line
//! flag. Flags here are therefore all optional; their absence means "use the
//! next layer". Secrets never come from the file.

// Doc comments on the CLI types are `--help` text, shown verbatim: no backticks.
#![allow(clippy::doc_markdown)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand};
use signalman::backstage::enrich::hints_from_labels;
use signalman::backstage::{self, Enricher};
use signalman::changes::{ChangeLog, FeedToken};
use signalman::config::{Config, Overrides};
use signalman::eval;
use signalman::incidentio::types::{AlertEvent, AlertStatus};
use signalman::incidentio::webhook::WebhookSecret;
use signalman::incidentio::{self, Triager, WriteBack};
use signalman::serve::{AppState, router};
use signalman::triage::{Alert, Decision, OpenIncident, TriageAnswers, TriageQuestions, decide};
use signalman::{Client, Request, Response};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "signalman",
    version,
    about = "TypeSafe-powered alert triage with incident.io and Backstage",
    after_help = "Precedence, lowest to highest: built-in default, configuration file, \
environment variable, flag. Secrets (TYPESAFE_API_KEY, INCIDENTIO_API_KEY, \
INCIDENTIO_WEBHOOK_SECRET, BACKSTAGE_TOKEN) are environment only."
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
    /// incident.io utilities.
    #[command(subcommand)]
    Incidentio(IncidentIoCommand),
    /// Backstage catalog utilities.
    #[command(subcommand)]
    Backstage(BackstageCommand),
    /// Configuration utilities.
    #[command(subcommand)]
    Config(ConfigCommand),
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
    /// incident.io HTTP alert source named by INCIDENTIO_ALERT_SOURCE_CONFIG_ID
    /// (authenticated with INCIDENTIO_ALERT_SOURCE_TOKEN).
    #[arg(long)]
    forward_to_incidentio: bool,
    /// Print the TypeSafe request body and exit without calling the model.
    #[arg(long)]
    print_request: bool,
    /// Emit the decision and raw answers as JSON instead of text.
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
    /// Write every raw model response to DIR/<id>.json for later --replay.
    #[arg(long, value_name = "DIR", conflicts_with = "replay")]
    record: Option<PathBuf>,
    /// Grade recorded responses from DIR under the current configuration
    /// instead of calling the model. Tune [policy] and [triage.text] this way.
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
enum ConfigCommand {
    /// Print the effective configuration as TOML, after every layer, with the
    /// file and environment variables that contributed. Exits non-zero when
    /// the file or an environment value is invalid: run it in CI against the
    /// ConfigMap before rolling it out.
    Show,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run(Cli::parse()).await {
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
        Command::Models | Command::Incidentio(_) | Command::Backstage(_) | Command::Config(_) => {
            Overrides::default()
        }
    }
}

async fn run(cli: Cli) -> Result<(), AnyError> {
    let (cfg, file) = Config::load(cli.config.as_deref(), &overrides(&cli.command))?;
    tracing::debug!(file = ?file, "configuration resolved");
    match cli.command {
        Command::Triage(args) => triage(&cfg, args).await,
        Command::Eval(args) => evaluate(&cfg, args).await,
        Command::Models => {
            let client = typesafe_client(&cfg)?;
            for m in client.list_models().await? {
                println!("{:<16} {:<12} {}", m.name, m.release_date, m.description);
            }
            Ok(())
        }
        Command::Serve {
            insecure_skip_verify,
            dry_run,
            ..
        } => serve(&cfg, insecure_skip_verify, dry_run).await,
        Command::Incidentio(cmd) => incidentio_cmd(&cfg, cmd).await,
        Command::Backstage(cmd) => backstage_cmd(&cfg, cmd).await,
        Command::Config(ConfigCommand::Show) => {
            println!("# signalman effective configuration");
            match &file {
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
            print!("{}", toml::to_string_pretty(&cfg)?);
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
    let mut e = Enricher::new(client).with_namespace(cfg.backstage.namespace.clone());
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

    if args.dedup_from_incidentio {
        let client = io_client.as_ref().ok_or("incident.io client missing")?;
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
    }

    let mut candidates = cfg.triage.fallback_candidates();
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
        candidates = enrichment.candidates;
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
        let source_id = std::env::var("INCIDENTIO_ALERT_SOURCE_CONFIG_ID")
            .map_err(|_| "set INCIDENTIO_ALERT_SOURCE_CONFIG_ID to forward alerts")?;
        let token = std::env::var("INCIDENTIO_ALERT_SOURCE_TOKEN")
            .map_err(|_| "set INCIDENTIO_ALERT_SOURCE_TOKEN to forward alerts")?;
        let event = alert_event(&alert, &answers, &decision, &response.model);
        let ack = client.send_alert_event(&source_id, &token, &event).await?;
        tracing::info!(dedup_key = %ack.deduplication_key, status = %ack.status, "forwarded to incident.io");
        forwarded = Some(ack);
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "decision": decision,
                "component": alert.component,
                "model": response.model,
                "usage": response.usage,
                "answers": response.answers,
                "forwarded": forwarded,
            }))?
        );
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
    let mut state = AppState::new(secret, triager);
    state.changes_token = changes_token;
    let app = router(Arc::new(state));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, dry_run, note = cfg.flow.note, related_window_minutes = cfg.flow.related_window_minutes, "listening for incident.io webhooks at /webhooks/incidentio");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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
