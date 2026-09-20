//! CLI: triage alerts with TypeSafe, integrate with incident.io, serve webhooks.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use signalman::incidentio::types::{AlertEvent, AlertStatus};
use signalman::incidentio::webhook::WebhookSecret;
use signalman::incidentio::{self, Triager, WriteBack};
use signalman::serve::{AppState, router};
use signalman::triage::{
    Alert, Decision, OpenIncident, Policy, TriageAnswers, TriageQuestions, decide,
};
use signalman::{Client, Options, Request, Response};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "signalman",
    version,
    about = "TypeSafe-powered alert triage with incident.io"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decide what to do with an alert (JSON file, or `-` for stdin).
    Triage(TriageArgs),
    /// List the TypeSafe model names this account may use.
    Models,
    /// Receive incident.io webhooks and triage new alerts.
    Serve {
        /// Listen address.
        #[arg(long, env = "SIGNALMAN_ADDR", default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
        /// Accept unsigned deliveries. Local development only.
        #[arg(long)]
        insecure_skip_verify: bool,
        /// Compute decisions but write nothing back to incident.io.
        #[arg(long)]
        dry_run: bool,
    },
    /// incident.io utilities.
    #[command(subcommand)]
    Incidentio(IncidentIoCommand),
}

#[derive(Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
struct TriageArgs {
    /// Path to an alert JSON document.
    alert: PathBuf,
    /// Model name or alias (env: TYPESAFE_DEFAULT_MODEL).
    #[arg(long, env = "TYPESAFE_DEFAULT_MODEL", default_value = signalman::client::DEFAULT_MODEL)]
    model: String,
    /// Replace `open_incidents` with the live incidents from incident.io.
    #[arg(long)]
    dedup_from_incidentio: bool,
    /// After deciding, post the alert with its judgments as metadata to the
    /// incident.io HTTP alert source named by INCIDENTIO_ALERT_SOURCE_CONFIG_ID
    /// (authenticated with INCIDENTIO_ALERT_SOURCE_TOKEN).
    #[arg(long)]
    forward_to_incidentio: bool,
    /// Print the TypeSafe request body and exit without calling any API.
    #[arg(long)]
    print_request: bool,
    /// Emit the decision and raw answers as JSON instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum IncidentIoCommand {
    /// Show which API key is configured and its roles.
    Whoami,
    /// List the incidents offered as dedup candidates.
    OpenIncidents {
        /// Maximum to fetch.
        #[arg(long, default_value_t = 40)]
        max: usize,
    },
    /// Run the webhook flow for one existing alert by id.
    TriageAlert {
        /// incident.io alert id.
        alert_id: String,
        /// Compute but write nothing back.
        #[arg(long)]
        dry_run: bool,
    },
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

async fn run(cli: Cli) -> Result<(), AnyError> {
    match cli.command {
        Command::Models => {
            let client = Client::from_env()?;
            for m in client.list_models().await? {
                println!("{:<14} {:<12} {}", m.name, m.release_date, m.description);
            }
            Ok(())
        }
        Command::Triage(args) => triage(args).await,
        Command::Serve {
            addr,
            insecure_skip_verify,
            dry_run,
        } => serve(addr, insecure_skip_verify, dry_run).await,
        Command::Incidentio(cmd) => incidentio_cmd(cmd).await,
    }
}

async fn triage(args: TriageArgs) -> Result<(), AnyError> {
    let raw = if args.alert.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        tokio::fs::read_to_string(&args.alert).await?
    };
    let mut alert: Alert = serde_json::from_str(&raw)?;

    let io_client = if args.dedup_from_incidentio || args.forward_to_incidentio {
        Some(incidentio::Client::from_env()?)
    } else {
        None
    };

    if args.dedup_from_incidentio {
        let client = io_client.as_ref().ok_or("incident.io client missing")?;
        let incidents = client
            .list_open_incidents(incidentio::sync::DEFAULT_CANDIDATES)
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

    let questions = TriageQuestions::for_alert(&alert)?;
    let state = TriageQuestions::state(&alert);
    let request = Request {
        state: &state,
        model: &args.model,
        questions: &questions.questions,
    };

    if args.print_request {
        println!("{}", serde_json::to_string_pretty(&request)?);
        return Ok(());
    }

    let client = Client::from_env()?;
    let response = client.evaluate(&request).await?;
    let answers = questions.read(&response)?;
    let decision = decide(&answers, &Policy::default());

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
            "ai": {
                "team": answers.owner.chosen.key(),
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

async fn serve(
    addr: SocketAddr,
    insecure_skip_verify: bool,
    dry_run: bool,
) -> Result<(), AnyError> {
    let secret = if insecure_skip_verify {
        None
    } else {
        Some(WebhookSecret::from_env()?)
    };
    let mut triager = Triager::new(Client::from_env()?, incidentio::Client::from_env()?);
    if dry_run {
        triager.write_back = WriteBack::DryRun;
    }
    let app = router(Arc::new(AppState::new(secret, triager)));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, dry_run, "listening for incident.io webhooks at /webhooks/incidentio");
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

async fn incidentio_cmd(cmd: IncidentIoCommand) -> Result<(), AnyError> {
    let io = incidentio::Client::from_env()?;
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
            for inc in io.list_open_incidents(max).await? {
                println!(
                    "{:<10} {:<10} {}",
                    inc.reference,
                    inc.incident_status.name,
                    inc.candidate_summary()
                );
            }
        }
        IncidentIoCommand::TriageAlert { alert_id, dry_run } => {
            let mut triager = Triager::new(Client::from_env()?, io);
            if dry_run {
                triager.write_back = WriteBack::DryRun;
            }
            let outcome = triager.triage_alert_by_id(&alert_id).await?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
    }
    Ok(())
}

fn print_report(alert: &Alert, answers: &TriageAnswers, decision: &Decision, response: &Response) {
    println!("alert     {}  ({})", alert.title, alert.source);
    println!("model     {}", response.model);
    println!(
        "owner     {:<14} confidence {}",
        answers.owner.chosen.key(),
        answers.owner.confidence
    );
    println!(
        "impact    {:.2} → {}  (confidence {})",
        answers.impact.value,
        answers.impact.nearest_label(),
        answers.impact.confidence
    );
    println!("actionable p={}", answers.actionable.yes);
    if let Some(d) = &answers.duplicate_of {
        println!("duplicate {:<14} confidence {}", d.chosen, d.confidence);
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
