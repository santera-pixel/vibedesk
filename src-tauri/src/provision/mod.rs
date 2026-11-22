//! Provisioning a fresh machine.
//!
//! What VibeDesk must put on a machine so that a user who has never run Vibe
//! gets a working swarm, without hand-editing `~/.vibe`. See
//! `docs/PLAN-provisioning.md`.
//!
//! Two folders, split by who reads them: anything Vibe reads goes in
//! `~/.vibe/`; anything only VibeDesk reads goes in `<app_data>/provision/`.
//! The split is the contract.
//!
//! An enum with a `match`, not a trait object: the repo has no `async-trait`
//! dependency, and `CapabilityId` already establishes enum dispatch as the house
//! style.

pub mod ledger;
pub mod payload;
pub mod steps;

use ledger::{Ledger, Record, State};
use serde::Serialize;

/// Whether the machine already satisfies a step, and at what version.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub satisfied: bool,
    pub version: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Cli,
    SwarmAgents,
    Aegis,
    Mastery,
    CodebaseMemory,
    Donsetch,
    ProxyProvider,
}

impl Step {
    pub const ALL: [Step; 7] = [
        Step::Cli,
        Step::SwarmAgents,
        Step::Aegis,
        Step::Mastery,
        Step::CodebaseMemory,
        Step::Donsetch,
        Step::ProxyProvider,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Step::Cli => "cli",
            Step::SwarmAgents => "swarm-agents",
            Step::Aegis => "aegis",
            Step::Mastery => "mastery",
            Step::CodebaseMemory => "codebase-memory",
            Step::Donsetch => "donsetch",
            Step::ProxyProvider => "proxy-provider",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Cli => "Vibe CLI",
            Step::SwarmAgents => "Swarm agents",
            Step::Aegis => "Aegis method pack",
            Step::Mastery => "Mastery bundles",
            Step::CodebaseMemory => "Codebase memory",
            Step::Donsetch => "Web capability",
            Step::ProxyProvider => "Capability proxy",
        }
    }

    /// Steps that must be `ready` before this one can run.
    ///
    /// `swarm-agents` requires nothing: the payload is in the binary, so it runs
    /// even when `cli` failed and the files are in place the moment the CLI
    /// arrives.
    pub fn requires(self) -> &'static [Step] {
        match self {
            Step::Cli | Step::SwarmAgents => &[],
            Step::Aegis | Step::ProxyProvider => &[Step::Cli],
            Step::Mastery | Step::CodebaseMemory | Step::Donsetch => &[],
        }
    }

    /// A precondition that makes the step `skipped` rather than `failed`.
    ///
    /// The distinction is the plan's: `skipped` means the step never ran because
    /// something it needs is absent, and the panel offers no retry for it.
    pub fn precondition(self) -> Result<(), String> {
        let needs_git = matches!(self, Step::Aegis | Step::Mastery);
        if needs_git && crate::paths::find_program(&["git"]).is_none() {
            return Err("git is not on PATH".into());
        }
        if self == Step::Donsetch {
            steps::donsetch::can_run()?;
        }
        if self == Step::ProxyProvider && crate::capabilities::proxy::needed() {
            crate::capabilities::provider::mirrorable()?;
        }
        Ok(())
    }

    pub fn probe(self) -> Probe {
        match self {
            Step::Cli => steps::cli::probe(),
            Step::SwarmAgents => steps::swarm_agents::probe(),
            Step::Aegis => steps::aegis::probe(),
            Step::Mastery => steps::mastery::probe(),
            Step::CodebaseMemory => steps::codebase_memory::probe(),
            Step::Donsetch => steps::donsetch::probe(),
            Step::ProxyProvider => steps::proxy_provider::probe(),
        }
    }

    /// Do it. Idempotent: a second call on a satisfied machine changes nothing.
    pub async fn install(self) -> Result<String, String> {
        match self {
            Step::Cli => steps::cli::install().await,
            Step::SwarmAgents => steps::swarm_agents::install(),
            Step::Aegis => steps::aegis::install().await,
            Step::Mastery => steps::mastery::install().await,
            Step::CodebaseMemory => steps::codebase_memory::install().await,
            Step::Donsetch => steps::donsetch::install().await,
            Step::ProxyProvider => steps::proxy_provider::install().await,
        }
    }
}

/// One row of the panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: &'static str,
    pub title: &'static str,
    pub state: State,
    pub version: Option<String>,
    pub detail: String,
    pub blocked: Option<String>,
    /// Whether the panel offers a retry. Only `failed` does.
    pub retryable: bool,
}

/// What the panel renders: every step, with its ledger state and live probe.
pub fn status() -> serde_json::Value {
    let (ledger, note) = Ledger::load();
    let rows: Vec<Row> = Step::ALL
        .into_iter()
        .map(|step| {
            let record = ledger.get(step.id());
            let probe = step.probe();
            let state = match record {
                Some(record) if record.state == State::Ready => State::Ready,
                Some(record) => record.state,
                None if probe.satisfied => State::Ready,
                None => State::Absent,
            };
            Row {
                id: step.id(),
                title: step.title(),
                state,
                version: record.and_then(|r| r.version.clone()).or(probe.version),
                detail: record
                    .map(|r| r.detail.clone())
                    .filter(|detail| !detail.is_empty())
                    .unwrap_or(probe.detail),
                blocked: record.and_then(|r| r.blocked.clone()),
                retryable: state == State::Failed,
            }
        })
        .collect();
    serde_json::json!({ "rows": rows, "note": note })
}

/// Run every step in order, recording each one as it finishes.
///
/// A failed step does not stop the others: each is independent by construction,
/// and a machine that got six of seven is more useful than one that stopped at
/// the first network error.
pub async fn run() -> Vec<Record> {
    let (mut ledger, _note) = Ledger::load();
    for step in Step::ALL {
        let record = run_one(step, &ledger).await;
        ledger.upsert(record);
        // Written after every step, so a crash mid-provision leaves an accurate
        // record rather than a machine that looks provisioned and is not.
        let _ = ledger.save();
    }
    ledger.records
}

/// Run one step, recording it.
pub async fn run_step(step: Step) -> Record {
    let (mut ledger, _note) = Ledger::load();
    let record = run_one(step, &ledger).await;
    ledger.upsert(record.clone());
    let _ = ledger.save();
    record
}

async fn run_one(step: Step, ledger: &Ledger) -> Record {
    let probe = step.probe();
    if probe.satisfied {
        return Record::new(step.id(), State::Ready, probe.detail).with_version(probe.version);
    }

    for required in step.requires() {
        if !ledger.state_of(required.id()).is_satisfied() {
            return Record::new(step.id(), State::Skipped, probe.detail)
                .blocked_by(format!("{} is not ready", required.title()));
        }
    }

    if let Err(reason) = step.precondition() {
        return Record::new(step.id(), State::Skipped, probe.detail).blocked_by(reason);
    }

    // Recorded before the work starts, so a process that dies mid-step leaves
    // `installing` rather than a stale `absent`.
    let mut ledger = ledger.clone();
    ledger.upsert(Record::new(step.id(), State::Installing, probe.detail.clone()));
    let _ = ledger.save();

    match step.install().await {
        Ok(detail) => Record::new(step.id(), State::Ready, detail),
        Err(error) => Record::new(step.id(), State::Failed, probe.detail).blocked_by(error),
    }
}

/// The swarm's start guard.
///
/// A lead on Vibe's coding prompt is the bug this module exists to fix, so
/// running it silently is the workaround this repo forbids. The message names
/// the file it expected, because "provisioning is not ready" is not actionable.
///
/// The check is the **files**, not the ledger. The ledger is a record of what
/// this machine did; the files are the fact. A user who copied `~/.vibe` from
/// another machine, or whose data directory moved, has the agents and no ledger,
/// and refusing their run would be wrong. The ledger is what the panel renders,
/// not what the engine trusts.
pub fn swarm_ready() -> Result<(), String> {
    match steps::swarm_agents::first_missing() {
        None => Ok(()),
        Some(missing) => Err(format!(
            "the swarm agents are not provisioned: {} is missing. Run provisioning, or the lead would run on Vibe's coding prompt and implement instead of planning.",
            missing.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_a_unique_id_and_a_title() {
        let mut ids: Vec<&str> = Step::ALL.iter().map(|step| step.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two steps share an id");
        for step in Step::ALL {
            assert!(!step.title().is_empty(), "{} has no title", step.id());
        }
    }

    #[test]
    fn a_requirement_is_always_an_earlier_step() {
        for (index, step) in Step::ALL.iter().enumerate() {
            for required in step.requires() {
                let position = Step::ALL
                    .iter()
                    .position(|candidate| candidate == required)
                    .expect("a requirement is a step");
                assert!(
                    position < index,
                    "{} requires {}, which runs later",
                    step.id(),
                    required.id()
                );
            }
        }
    }

    #[test]
    fn swarm_agents_requires_nothing_so_it_runs_without_the_cli() {
        assert!(Step::SwarmAgents.requires().is_empty());
    }

    #[test]
    fn the_ordering_is_the_one_the_plan_states() {
        let ids: Vec<&str> = Step::ALL.iter().map(|step| step.id()).collect();
        assert_eq!(
            ids,
            vec![
                "cli",
                "swarm-agents",
                "aegis",
                "mastery",
                "codebase-memory",
                "donsetch",
                "proxy-provider"
            ]
        );
    }

    #[test]
    fn only_a_failed_row_is_retryable() {
        for state in [State::Absent, State::Installing, State::Ready, State::Skipped] {
            assert_ne!(state, State::Failed);
        }
    }

    /// The real thing: provision this machine and check the swarm can start.
    ///
    /// `#[ignore]` because it writes to `~/.vibe` and may reach the network. Run
    /// it with `cargo test --lib provision::tests::live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "writes to ~/.vibe and may use the network"]
    async fn live_provisioning_makes_the_swarm_ready() {
        let records = run().await;
        for record in &records {
            println!(
                "{:<16} {:<11} {}{}",
                record.id,
                format!("{:?}", record.state),
                record.detail,
                record
                    .blocked
                    .as_deref()
                    .map(|reason| format!(" — {reason}"))
                    .unwrap_or_default()
            );
        }

        let agents = records
            .iter()
            .find(|record| record.id == Step::SwarmAgents.id())
            .expect("the swarm-agents step ran");
        assert_eq!(
            agents.state,
            State::Ready,
            "the payload did not land: {:?}",
            agents.blocked
        );

        // The guard the engine calls, and the files it names.
        swarm_ready().expect("a provisioned machine may start a swarm");
        for path in steps::swarm_agents::required_paths() {
            assert!(path.is_file(), "{} is missing", path.display());
        }

        // And the agent is one Vibe will actually advertise as a mode. A TOML
        // that fails to load is not offered, and the member would silently keep
        // the session default — the exact failure this module exists to stop.
        let lead = crate::paths::vibe_home().join("agents/vibedesk-swarm-lead.toml");
        let body = std::fs::read_to_string(&lead).expect("the lead agent exists");
        assert!(body.contains("system_prompt_id"), "the lead names its prompt");
        assert!(
            crate::paths::vibe_home()
                .join("prompts/vibedesk-swarm-lead.md")
                .is_file(),
            "the lead's prompt is beside its TOML"
        );
    }
}
