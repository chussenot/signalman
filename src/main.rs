//! CLI: triage an alert with TypeSafe, or list available models.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rustsafe::triage::{Alert, Policy, TriageQuestions, decide};
use rustsafe::{Client, Request};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "rustsafe",
    version,
    about = "Typed TypeSafe client and alert triage"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decide what to do with an alert (JSON file, or `-` for stdin).
    Triage {
        /// Path to an alert JSON document.
        alert: PathBuf,
        /// Model name or alias (env: TYPESAFE_DEFAULT_MODEL).
        #[arg(long, env = "TYPESAFE_DEFAULT_MODEL", default_value = rustsafe::client::DEFAULT_MODEL)]
        model: String,
        /// Print the request body and exit without calling the API. Paste it
        /// into the Playground at console.typesafe.ai/playground to inspect answers.
        #[arg(long)]
        print_request: bool,
        /// Emit the decision and raw answers as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the model names this account may use.
    Models,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
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

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Models => {
            let client = Client::from_env()?;
            for m in client.list_models().await? {
                println!("{:<14} {:<12} {}", m.name, m.release_date, m.description);
            }
            Ok(())
        }
        Command::Triage {
            alert,
            model,
            print_request,
            json,
        } => {
            let raw = if alert.as_os_str() == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                tokio::fs::read_to_string(&alert).await?
            };
            let alert: Alert = serde_json::from_str(&raw)?;
            let questions = TriageQuestions::for_alert(&alert)?;
            let state = TriageQuestions::state(&alert);
            let request = Request {
                state: &state,
                model: &model,
                questions: &questions.questions,
            };

            if print_request {
                println!("{}", serde_json::to_string_pretty(&request)?);
                return Ok(());
            }

            let client = Client::from_env()?;
            let response = client.evaluate(&request).await?;
            let answers = questions.read(&response)?;
            let decision = decide(&answers, &Policy::default());

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "decision": decision,
                        "model": response.model,
                        "usage": response.usage,
                        "answers": response.answers,
                    }))?
                );
            } else {
                print_report(&alert, &answers, &decision, &response);
            }
            Ok(())
        }
    }
}

fn print_report(
    alert: &Alert,
    answers: &rustsafe::triage::TriageAnswers,
    decision: &rustsafe::triage::Decision,
    response: &rustsafe::Response,
) {
    use rustsafe::Options;

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
