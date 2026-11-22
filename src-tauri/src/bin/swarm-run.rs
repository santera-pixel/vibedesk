//! Run a swarm from a terminal.
//!
//! This is the same engine the panel drives — same scheduler, same worktrees,
//! same board, same verifiers — with a sink that prints instead of a window that
//! renders. It exists for two reasons: a run can be driven on a machine with no
//! display, and the panel's behaviour can be checked against a run that was
//! observed from outside it.
//!
//! ```text
//! cargo run --bin swarm-run -- --workspace D:/game --goal "build a 3D game"
//! ```
//!
//! The run is not a rehearsal: it spawns real `vibe-acp` processes, spends real
//! tokens, and merges real branches.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use vibedesk_lib::acp::bridge::EventSink;
use vibedesk_lib::swarm::catalog::{Catalog, ModelOption};
use vibedesk_lib::swarm::engine;
use vibedesk_lib::swarm::roles::{self, CrewChoice, DiversityPolicy, ThinkingByRole};

/// A sink that prints the run as it moves.
struct ConsoleSink;

impl EventSink for ConsoleSink {
    fn emit(&self, event: &str, payload: Value) {
        // Only the run's own event is interesting here; a member's notifications
        // are its transcript, and the board is the compressed version of them.
        if event == engine::EVENT_SWARM {
            let state = payload.get("state").and_then(Value::as_str).unwrap_or("?");
            let tasks = payload.get("tasks").and_then(Value::as_u64).unwrap_or(0);
            let verified = payload.get("verified").and_then(Value::as_u64).unwrap_or(0);
            println!("  · {state} — {verified}/{tasks} verified");
        }
    }
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(next) = args.next() {
        if next == name {
            return args.next();
        }
        if let Some(value) = next.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

/// Every flag this runner reads.
///
/// A flag it does not know is a typo, and a typo that is ignored is a run that
/// spends real tokens on a goal nobody asked for.
const FLAGS: [&str; 12] = [
    "--workspace",
    "--goal",
    "--budget",
    "--budget-micros",
    "--concurrency",
    "--timeout",
    "--models",
    "--model",
    "--thinking",
    "--lead-model",
    "--worker-model",
    "--verifier-model",
];

const USAGE: &str = "\
swarm-run — run a swarm headlessly

  --workspace <dir>         the repository the run works in (default: the current directory)
  --goal <text>             what the run must produce (required)
  --budget <tokens>         the token budget for the whole run (default: 4000000)
  --budget-micros <usd>     the money ceiling, in millionths of a dollar — the
                            run stops on either ceiling; spend is reported
                            either way (default: none)
  --concurrency <n>         how many tasks run at once (default: 4)
  --timeout <seconds>       how long the run may take (default: 1800)
  --model <alias>           the workspace's active model
  --models <a,b,c>          the models the run may choose from
  --lead-model <alias>      pin the lead's model
  --worker-model <alias>    pin the workers' model
  --verifier-model <alias>  pin the verifiers' model
  --thinking <spec>         one level for every role, or lead,worker,verifier
";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return;
    }
    if let Some(unknown) = args.iter().find(|a| {
        a.starts_with('-')
            && !FLAGS
                .iter()
                .any(|flag| *a == *flag || a.starts_with(&format!("{flag}=")))
    }) {
        eprintln!("swarm-run: unknown flag {unknown}\n\n{USAGE}");
        std::process::exit(2);
    }

    let workspace = PathBuf::from(
        arg("--workspace").unwrap_or_else(|| std::env::current_dir().unwrap().display().to_string()),
    );
    // A run with no goal used to invent one and spend the budget on it. The goal
    // is the only thing that says what the tokens are for, so it is required.
    let Some(goal) = arg("--goal")
        .map(|goal| goal.trim().to_string())
        .filter(|goal| !goal.is_empty())
    else {
        eprintln!("swarm-run: --goal is required\n\n{USAGE}");
        std::process::exit(2);
    };
    let budget: u64 = arg("--budget")
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000_000);
    let budget_micros: Option<u64> = arg("--budget-micros").and_then(|v| v.parse().ok());
    let concurrency: usize = arg("--concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let timeout_secs: u64 = arg("--timeout")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);

    // The capability layer reads its own file. The app loads it at startup; a
    // headless run has to do the same, or the swarm's first precondition reads
    // the defaults instead of the user's choices and blocks a run that is ready.
    let capability_settings = vibedesk_lib::capabilities::settings::load();
    vibedesk_lib::capabilities::registry().load(&capability_settings);

    let sink: Arc<dyn EventSink> = Arc::new(ConsoleSink);
    let engine = engine::engine();

    println!("swarm · {}", workspace.display());
    println!("goal  · {goal}");

    // Preconditions first: a run that cannot start should say why, not fail
    // halfway through a plan.
    let preconditions = engine::preconditions_with(Some(&workspace), None);
    let blocked: Vec<String> = preconditions
        .iter()
        .filter(|p| !p.ready)
        .map(|p| format!("{}: {}", p.label, p.detail))
        .collect();
    for precondition in &preconditions {
        println!(
            "  {} {} — {}",
            if precondition.ready { "ok  " } else { "BLOCK" },
            precondition.label,
            precondition.detail
        );
    }
    if !blocked.is_empty() {
        eprintln!("\nblocked:\n  {}", blocked.join("\n  "));
        std::process::exit(1);
    }

    // A headless run has no session to ask, so the catalogue is `config.toml`'s
    // `[[models]]` — the same fallback the window uses before its first session.
    // `--models` names aliases the file does not declare, for a run against a
    // provider the app is not currently pointed at.
    let (config, _) = vibedesk_lib::config::load_effective(Some(&workspace));
    let mut catalog = Catalog::from_config(&config);
    if let Some(extra) = arg("--models") {
        for alias in extra.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            if catalog.get(alias).is_none() {
                catalog.models.push(ModelOption {
                    value: alias.to_string(),
                    name: alias.to_string(),
                    detail: None,
                    thinking: Vec::new(),
                    default: None,
                });
            }
        }
    }
    // `--model` overrides the workspace's active model. A headless run is often
    // used to check the engine against a provider the app is not currently on,
    // and re-pointing the whole workspace to do that would change what the user
    // sees in the app.
    let active = arg("--model").or(config.active_model.clone());
    // `--thinking` takes one level for every role, or `lead,worker,verifier`.
    let thinking = match arg("--thinking") {
        Some(spec) => {
            let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
            match parts.as_slice() {
                [one] => ThinkingByRole {
                    lead: one.to_string(),
                    worker: one.to_string(),
                    verifier: one.to_string(),
                },
                [lead, worker, verifier, ..] => ThinkingByRole {
                    lead: lead.to_string(),
                    worker: worker.to_string(),
                    verifier: verifier.to_string(),
                },
                // `--thinking lead,worker` names two roles and leaves the third
                // on its default, which is a real request rather than a typo.
                [lead, worker] => ThinkingByRole {
                    lead: lead.to_string(),
                    worker: worker.to_string(),
                    verifier: String::new(),
                },
                [] => ThinkingByRole::default(),
            }
        }
        None => ThinkingByRole::default(),
    };
    let choice = CrewChoice {
        // Per-role pins, so a headless run can be pointed at a provider the
        // diversity policy would not otherwise choose. `--model` sets the
        // workspace's active model and `assign` fills the rest from the
        // catalogue; these override one role at a time, which is what a run
        // against a second provider needs.
        lead: arg("--lead-model").unwrap_or_default(),
        worker: arg("--worker-model").unwrap_or_default(),
        verifier: arg("--verifier-model").unwrap_or_default(),
        thinking,
    };
    let Some(models) = roles::assign(&catalog, active.as_deref(), DiversityPolicy::Require, &choice)
    else {
        eprintln!("no model is configured");
        std::process::exit(1);
    };
    println!(
        "models · lead {} ({}) · worker {} ({}) · verifier {} ({})",
        models.lead,
        models.thinking.lead,
        models.worker,
        models.thinking.worker,
        models.verifier,
        models.thinking.verifier
    );

    println!("\nplanning…");
    if let Err(error) = engine
        .start(
            sink.clone(),
            goal,
            workspace.clone(),
            models.clone(),
            budget,
            budget_micros,
            concurrency,
        )
        .await
    {
        eprintln!("could not start: {error}");
        std::process::exit(1);
    }

    // Wait for the lead to write a plan.
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let plan = loop {
        if std::time::Instant::now() > deadline {
            eprintln!("the lead did not produce a plan in time");
            std::process::exit(1);
        }
        if let Some(current) = engine.run() {
            if current.state == vibedesk_lib::swarm::RunState::AwaitingApproval {
                break current.tasks.clone();
            }
            if current.state.is_terminal() {
                eprintln!("the run ended while planning: {:?}", current.failure);
                std::process::exit(1);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    println!("\nplan · {} tasks", plan.len());
    for task in &plan {
        println!(
            "  {} {} [{}]{}",
            task.id,
            task.title,
            task.files.join(", "),
            if task.deps.is_empty() {
                String::new()
            } else {
                format!(" after {}", task.deps.join(", "))
            }
        );
    }

    // The suite comes from the run, not from this file: the engine carried it
    // there when it accepted the plan, and rebuilding the plan without it is a
    // plan the gate refuses a second time.
    let current = engine.run();
    let plan = vibedesk_lib::swarm::plan::Plan {
        goal: current.as_ref().map(|r| r.goal.clone()).unwrap_or_default(),
        integration_suite: current.as_ref().and_then(|r| r.integration.suite.clone()),
        notes: String::new(),
        tasks: plan,
        // The run already wrote whatever its plan authored, and its catalogue
        // holds them. Re-sending the drafts would write them a second time.
        skills_to_write: Vec::new(),
    };

    println!("\napproving…");
    if let Err(error) = engine
        .approve(sink.clone(), plan, models, concurrency)
        .await
    {
        eprintln!("could not approve: {error}");
        std::process::exit(1);
    }

    // Drive to a terminal state.
    loop {
        if std::time::Instant::now() > deadline {
            eprintln!("\ntimed out after {timeout_secs}s");
            let _ = engine.stop(&sink);
            // A cancelled run keeps its worktrees on purpose, but not its agents.
            engine::end_of_run().await;
            break;
        }
        let Some(current) = engine.run() else { break };
        if current.state.is_terminal() {
            println!("\n{} · {}", current.state.label(), current.id);
            if let Some(failure) = &current.failure {
                println!("failure · {failure}");
            }
            for task in &current.tasks {
                let verdict = task
                    .verdict
                    .as_ref()
                    .map(|v| {
                        format!(
                            "{} (base {:?} → change {:?})",
                            if v.is_earned() { "earned" } else { "not earned" },
                            v.base_exit,
                            v.change_exit
                        )
                    })
                    .unwrap_or_else(|| "no verdict".into());
                println!("  {} {:?} · {verdict}", task.id, task.state);
            }
            // Release here rather than waiting for the engine's next tick: this
            // process returns from `main`, which drops the runtime and cancels the
            // drive loop. That race lost once, leaving four members running with
            // nothing left to stop them.
            let released = engine::end_of_run().await;
            if released > 0 {
                println!(
                    "\nreleased {released} agent process{}",
                    if released == 1 { "" } else { "es" }
                );
            }
            if let Some(reflection) = &current.reflection {
                println!("\nreflection · {}", reflection.summary);
                for gotcha in &reflection.gotchas {
                    println!("  {} — {} ({})", gotcha.file, gotcha.text, gotcha.source);
                }
                if let Some(degraded) = &reflection.degraded {
                    println!("  degraded: {degraded}");
                }
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
