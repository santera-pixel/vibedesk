//! The swarm as the webview sees it.
//!
//! One snapshot command, like the capability ledger and for the same reason: the
//! panel is a single account of a run, and an account assembled from six round
//! trips can disagree with itself. Everything else here is an action on that
//! snapshot.
//!
//! The rehearsal is deliberately a first-class command rather than a preview
//! inside `start`. It schedules and costs a plan with no processes and no spend,
//! which is what lets the panel be useful before the user has committed 15× the
//! tokens of a chat.

use serde_json::{json, Value};
use tauri::Manager;

use crate::error::{AppError, AppResult};
use crate::swarm::catalog::Catalog;
use crate::swarm::engine::{self, RehearsalView};
use crate::swarm::plan::Plan;
use crate::swarm::roles::{self, CrewChoice, DiversityPolicy, RoleModels};
use crate::swarm::{Run, SwarmSnapshot};

/// The token ceiling a run gets when the caller does not set one.
///
/// Two million was the old default and it was a lie: one worker turn on a real
/// repository reports about 1.3M input tokens, because the agent re-sends its
/// whole context on every step. A four-task run therefore blew the ceiling
/// during its first task and died with "budget exceeded" while the work was
/// going fine. Twenty-five million fits a small run with retries and still
/// stops a runaway one.
///
/// The number counts raw tokens, cached ones included, because that is what the
/// agent reports and inventing a discount would make the guard unfalsifiable.
/// The cache share is shown beside it so a run that is mostly cache reads is
/// visibly cheap rather than silently expensive.
pub const DEFAULT_BUDGET_TOKENS: u64 = 25_000_000;

/// Everything the panel renders, in one round trip.
///
/// `selected` is the member the panel is showing in detail. It travels with the
/// read rather than being remembered by the engine because it is a fact about the
/// window: the feed is two hundred items, and sending one for every member of a
/// twenty-one member run every two seconds would be four thousand items per poll
/// for a pane that can only show one.
#[tauri::command]
pub async fn swarm_snapshot(
    state: tauri::State<'_, crate::AppState>,
    selected: Option<String>,
) -> AppResult<SwarmSnapshot> {
    let cwd = state.bridge.cwd().await;
    Ok(engine::engine().snapshot(cwd.as_deref(), selected.as_deref()))
}

/// Schedule and cost a plan without running it.
#[tauri::command]
pub async fn swarm_rehearse(plan: Plan, concurrency: Option<usize>) -> AppResult<RehearsalView> {
    Ok(engine::engine().rehearse(&plan, concurrency.unwrap_or(4)))
}

/// Rehearse the built-in example plan.
///
/// The panel offers this on an empty state so the feature is demonstrable
/// without a live agent and without a repository.
#[tauri::command]
pub async fn swarm_rehearse_example(concurrency: Option<usize>) -> AppResult<RehearsalView> {
    Ok(engine::engine().rehearse_example(concurrency.unwrap_or(4)))
}

/// The example plan itself, so the panel can show it and the user can edit it.
#[tauri::command]
pub async fn swarm_example_plan() -> AppResult<Plan> {
    Ok(roles::example_plan())
}

/// Start a run: spawn the lead, which writes a plan.
#[tauri::command]
pub async fn swarm_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    goal: String,
    concurrency: Option<usize>,
    budget_tokens: Option<u64>,
    budget_micros: Option<u64>,
    catalog: Option<Catalog>,
    crew: Option<CrewChoice>,
) -> AppResult<Run> {
    let goal = goal.trim().to_string();
    if goal.is_empty() {
        return Err(AppError::message("a swarm needs a goal"));
    }

    let cwd = state
        .bridge
        .cwd()
        .await
        .ok_or_else(|| AppError::message("no workspace is open"))?;

    // A swarm isolates workers in git worktrees, so the workspace is made
    // git-ready here instead of refused: git init plus an empty root commit
    // when needed, never staging or committing the user's files.
    let _ = engine::prepare_workspace(&cwd);

    // The gate is checked here as well as in the panel. The panel is a view, and
    // a view must not be able to start a run the gate would refuse.
    let catalog = resolve_catalog(Some(&cwd), catalog);
    let preconditions = engine::preconditions_with(Some(&cwd), Some(&catalog));
    if !engine::ready(&preconditions) {
        let blocked: Vec<String> = preconditions
            .iter()
            .filter(|p| !p.ready)
            .map(|p| format!("{}: {}", p.label, p.detail))
            .collect();
        return Err(AppError::message(blocked.join("; ")));
    }

    let models = resolve_models(Some(&cwd), Some(&catalog), crew.as_ref())?;
    let budget = budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS);

    engine::engine()
        .start(
            sink(app),
            goal,
            cwd,
            models,
            budget,
            budget_micros,
            concurrency.unwrap_or(4),
        )
        .await
        .map_err(AppError::message)
}

/// Approve the lead's plan and start the workers.
#[tauri::command]
pub async fn swarm_approve(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    plan: Plan,
    concurrency: Option<usize>,
    catalog: Option<Catalog>,
    crew: Option<CrewChoice>,
) -> AppResult<Run> {
    let cwd = state.bridge.cwd().await;
    let catalog = resolve_catalog(cwd.as_deref(), catalog);
    let models = resolve_models(cwd.as_deref(), Some(&catalog), crew.as_ref())?;
    engine::engine()
        .approve(sink(app), plan, models, concurrency.unwrap_or(4))
        .await
        .map_err(AppError::message)
}

/// Stop the live run. Worktrees are kept and named.
#[tauri::command]
pub async fn swarm_stop(app: tauri::AppHandle) -> AppResult<Run> {
    engine::engine().stop(&sink(app)).map_err(AppError::message)
}

/// Ask the process driving an adopted run to stop it.
///
/// A separate command from `swarm_stop` because the two make different promises.
/// One stops the run; the other writes a request the driver honours on its next
/// tick. Folding them together would mean one command that sometimes reports a
/// stop it did not perform, which is the button this phase exists to remove.
#[tauri::command]
pub async fn swarm_request_stop(app: tauri::AppHandle) -> AppResult<Run> {
    engine::engine()
        .request_stop(&sink(app))
        .map_err(AppError::message)
}

/// Delete a run and everything it owns.
#[tauri::command]
pub async fn swarm_delete(app: tauri::AppHandle, id: String) -> AppResult<()> {
    engine::engine()
        .delete(&sink(app), &id)
        .await
        .map_err(AppError::message)
}

/// Load a finished run into the panel without resuming it.
#[tauri::command]
pub async fn swarm_open(app: tauri::AppHandle, id: String) -> AppResult<Run> {
    engine::engine().open(&sink(app), &id).map_err(AppError::message)
}

/// The installed skills, and what a run with this goal would be handed.
///
/// The panel needs both before a run starts: the catalogue so the user can see
/// what the machine knows, and the ranking so they can see what this goal would
/// select. The ranking is the same function the engine calls, so the preview is
/// the run's own decision rather than a second opinion about it.
#[tauri::command]
pub async fn swarm_skills(
    state: tauri::State<'_, crate::AppState>,
    goal: Option<String>,
) -> AppResult<Value> {
    let cwd = state.bridge.cwd().await;
    let catalogue = crate::swarm::skills::Catalogue::load(cwd.as_deref());
    let goal = goal.unwrap_or_default();
    let ranked = if goal.trim().is_empty() {
        Vec::new()
    } else {
        crate::swarm::skills::rank(&catalogue, &goal, 6)
    };
    Ok(json!({
        "catalogue": catalogue,
        "ranked": ranked,
        "enabled": crate::settings::load().swarm_skills,
    }))
}

/// Install a skill the reflection pass proposed.
///
/// The one write in this feature that reaches the user's own skill store, and it
/// is a click rather than a side effect. The proposal is read from the run's
/// directory — never from the caller — so the body that lands in
/// `~/.vibe/skills` is the one the run wrote and the user read, not one a caller
/// supplied.
#[tauri::command]
pub async fn swarm_skill_accept(
    state: tauri::State<'_, crate::AppState>,
    run_id: String,
    name: String,
) -> AppResult<Value> {
    let run = crate::swarm::load_run(&run_id)
        .ok_or_else(|| AppError::message(format!("no run {run_id}")))?;
    let proposal = run
        .skill_proposals
        .iter()
        .find(|proposal| proposal.name == name)
        .ok_or_else(|| {
            AppError::message(format!("run {run_id} proposed no skill called `{name}`"))
        })?;
    if !proposal.valid_name() {
        return Err(AppError::message(format!(
            "`{name}` is not a name a skill directory can hold"
        )));
    }

    let root = crate::paths::vibe_home().join("skills").join(&proposal.name);
    std::fs::create_dir_all(&root)
        .map_err(|e| AppError::message(format!("could not create {}: {e}", root.display())))?;
    let path = root.join("SKILL.md");
    std::fs::write(&path, proposal.render())
        .map_err(|e| AppError::message(format!("could not write {}: {e}", path.display())))?;

    // The catalogue the panel shows is read from disk, so it picks this up on the
    // next read. Nothing is cached here on purpose: a cache would have to be
    // invalidated by a write that can also happen outside this process.
    let _ = state;
    Ok(json!({
        "name": proposal.name,
        "path": path.to_string_lossy(),
    }))
}

/// The preconditions alone, for the panel's header.
#[tauri::command]
pub async fn swarm_preconditions(
    state: tauri::State<'_, crate::AppState>,
    catalog: Option<Catalog>,
) -> AppResult<Vec<crate::swarm::Precondition>> {
    let cwd = state.bridge.cwd().await;
    let catalog = resolve_catalog(cwd.as_deref(), catalog);
    Ok(engine::preconditions_with(cwd.as_deref(), Some(&catalog)))
}

/// The agent's own catalogue, with each model's reasoning levels.
///
/// This is the expensive read: the session reports the levels of the model it is
/// on, so learning the rest means switching to each one — one process and seven
/// round trips, about four seconds. It is cached for the life of the process and
/// keyed by `config.toml`'s modification time, so a panel that opens twice pays
/// once and a config edit pays again.
///
/// The panel calls this in the background on first open. Until it answers, the
/// panel uses the catalogue the window already holds from its own session, which
/// is correct for the active model and unknown for the rest — and unknown is
/// reported as unknown rather than guessed.
#[tauri::command]
pub async fn swarm_catalog() -> AppResult<Catalog> {
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static CACHE: OnceLock<Mutex<Option<(u64, Catalog)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    let stamp = crate::config::user_config_path()
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if let Some((cached_stamp, catalog)) = cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *cached_stamp == stamp {
            return Ok(catalog.clone());
        }
    }

    let catalog = crate::swarm::catalog::probe().await.map_err(AppError::message)?;
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, catalog.clone()));
    Ok(catalog)
}

/// The crew: which models the agent offers, and which one each role would get.
///
/// The catalogue is the agent's, not `config.toml`'s. The window already holds
/// it — `session/new` returns it as `configOptions` — so the panel passes it in
/// and this command costs nothing. A caller with no session passes nothing and
/// gets the config fallback, which is what a headless run uses.
///
/// `thinking` is per model, so the answer carries the levels each model accepts
/// rather than one global list. A panel that showed a single list would offer a
/// level the agent refuses for the model beside it.
#[tauri::command]
pub async fn swarm_models(
    state: tauri::State<'_, crate::AppState>,
    catalog: Option<Catalog>,
    crew: Option<CrewChoice>,
) -> AppResult<Value> {
    let cwd = state.bridge.cwd().await;
    let (config, _) = crate::config::load_effective(cwd.as_deref());
    let catalog = resolve_catalog(cwd.as_deref(), catalog);
    let wanted = crew.clone().unwrap_or_default();
    let picked = roles::assign(
        &catalog,
        config.active_model.as_deref(),
        DiversityPolicy::Require,
        &wanted,
    );
    Ok(json!({
        "catalog": catalog,
        "picked": picked,
        "defaults": {
            "lead": roles::default_thinking(crate::swarm::Role::Lead),
            "worker": roles::default_thinking(crate::swarm::Role::Worker),
            "verifier": roles::default_thinking(crate::swarm::Role::Verifier),
        },
    }))
}

/// The window, as the engine sees it.
///
/// The engine reports through a sink rather than an `AppHandle`, so the same
/// code path runs headless. This is the one line that knows the difference.
fn sink(app: tauri::AppHandle) -> std::sync::Arc<dyn crate::acp::bridge::EventSink> {
    std::sync::Arc::new(app)
}

/* ------------------------------------------------------------------ *
 * The agent's door
 * ------------------------------------------------------------------ */

/// How often the queue is drained.
///
/// Fast enough that a tool call feels synchronous to the agent, slow enough that
/// an idle app is not spinning. The cost is one `read_dir` on an almost always
/// empty directory.
const DRAIN_EVERY: std::time::Duration = std::time::Duration::from_millis(300);

/// Answer swarm commands from other processes.
///
/// The agent the user is talking to is a `vibe-acp` session, and a session can
/// start a swarm only by asking the process that owns the driver lock. This is
/// that process. It drains the queue, runs each command against the engine, and
/// writes the answer back — so the run belongs to the app, survives the chat
/// that asked for it, and can still be approved or stopped from the panel.
pub fn watch(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            for (id, command) in crate::swarm::commands::pending() {
                let answer = answer(&app, command).await;
                if let Err(error) = crate::swarm::commands::reply(&id, &answer) {
                    eprintln!("vibedesk: could not answer swarm command {id}: {error}");
                }
            }
            tokio::time::sleep(DRAIN_EVERY).await;
        }
    });
}

/// Run one command and describe the outcome in the shape the caller expects.
async fn answer(app: &tauri::AppHandle, command: crate::swarm::commands::Command) -> Value {
    use crate::swarm::commands::{failed, ok};

    if let Some(reason) = command.refuse() {
        return failed(reason);
    }

    match command.op.as_str() {
        "start" => {
            let state = app.state::<crate::AppState>();
            let cwd = if command.workspace.trim().is_empty() {
                match state.bridge.cwd().await {
                    Some(cwd) => cwd,
                    None => return failed("no workspace is open, and the command named none"),
                }
            } else {
                std::path::PathBuf::from(command.workspace.trim())
            };

            // Same as the panel: make the workspace git-ready first, so the
            // folder becomes a repository instead of refusing the run.
            let _ = engine::prepare_workspace(&cwd);

            // The same gate the panel and the command line go through. A run that
            // cannot start should say why here, not fail halfway through planning.
            let preconditions = engine::preconditions(Some(&cwd));
            if !engine::ready(&preconditions) {
                let blocked: Vec<String> = preconditions
                    .iter()
                    .filter(|p| !p.ready)
                    .map(|p| format!("{}: {}", p.label, p.detail))
                    .collect();
                return failed(blocked.join("; "));
            }

            let models = match resolve_models_for(Some(&cwd), None, None, &command.model) {
                Ok(models) => models,
                Err(error) => return failed(error.to_string()),
            };

            match engine::engine()
                .start(
                    sink(app.clone()),
                    command.goal,
                    cwd,
                    models,
                    command.budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS),
                    command.budget_micros,
                    command.concurrency.unwrap_or(4),
                )
                .await
            {
                Ok(run) => ok(json!({
                    "runId": run.id,
                    "state": run.state,
                    "workspace": run.workspace,
                })),
                Err(error) => failed(error),
            }
        }

        "approve" => {
            let Some(run) = engine::engine().run() else {
                return failed("no run is open");
            };
            if run.state != crate::swarm::RunState::AwaitingApproval {
                return failed(format!(
                    "the run is {}, not awaiting approval",
                    run.state.label()
                ));
            }
            // The plan is the run's own tasks. Rebuilding it here rather than
            // carrying it in the command means an approval can never describe a
            // plan the run does not have.
            let plan = Plan {
                goal: run.goal.clone(),
                tasks: run.tasks.clone(),
                integration_suite: run.integration.suite.clone(),
                notes: String::new(),
                // The run already wrote whatever its plan authored, and the
                // catalogue it carries holds them. Re-sending the drafts would
                // write them a second time.
                skills_to_write: Vec::new(),
            };
            let models = match resolve_models(Some(std::path::Path::new(&run.workspace)), None, None) {
                Ok(models) => models,
                Err(error) => return failed(error.to_string()),
            };
            match engine::engine()
                .approve(
                    sink(app.clone()),
                    plan,
                    models,
                    command.concurrency.unwrap_or(run.concurrency),
                )
                .await
            {
                Ok(run) => ok(json!({
                    "runId": run.id,
                    "state": run.state,
                    "tasks": run.tasks.len(),
                })),
                Err(error) => failed(error),
            }
        }

        "stop" => match engine::engine().stop(&sink(app.clone())) {
            Ok(run) => ok(json!({ "runId": run.id, "state": run.state })),
            Err(error) => failed(error),
        },

        other => failed(format!("`{other}` is not a swarm command")),
    }
}

/// The catalogue to assign from: the caller's, or `config.toml`'s.
///
/// A caller that has a live session passes the agent's own list, which is the
/// only list guaranteed to match what the agent will accept. A caller that does
/// not — a headless run, a window with no session yet — gets the config
/// fallback, and the panel says which one it is looking at.
fn resolve_catalog(cwd: Option<&std::path::Path>, supplied: Option<Catalog>) -> Catalog {
    if let Some(catalog) = supplied.filter(|c| !c.is_empty()) {
        return catalog;
    }
    let (config, _) = crate::config::load_effective(cwd);
    Catalog::from_config(&config)
}

/// Resolve the role models for a workspace.
fn resolve_models(
    cwd: Option<&std::path::Path>,
    catalog: Option<&Catalog>,
    crew: Option<&CrewChoice>,
) -> AppResult<RoleModels> {
    resolve_models_for(cwd, catalog, crew, "")
}

/// Resolve the role models, optionally overriding the active model.
///
/// An override that names a model the catalogue does not have is refused rather
/// than silently ignored: a run that quietly used a different model than the one
/// asked for is a run whose results cannot be compared.
fn resolve_models_for(
    cwd: Option<&std::path::Path>,
    catalog: Option<&Catalog>,
    crew: Option<&CrewChoice>,
    model: &str,
) -> AppResult<RoleModels> {
    let (config, _) = crate::config::load_effective(cwd);
    let catalog = match catalog {
        Some(catalog) if !catalog.is_empty() => catalog.clone(),
        _ => Catalog::from_config(&config),
    };
    let available = catalog.aliases();
    let wanted = model.trim();
    if !wanted.is_empty() && !available.iter().any(|alias| alias == wanted) {
        return Err(AppError::message(format!(
            "`{wanted}` is not a model the agent offers — one of {}",
            available.join(", ")
        )));
    }
    let active = if wanted.is_empty() {
        config.active_model.as_deref()
    } else {
        Some(wanted)
    };
    let wanted = crew.cloned().unwrap_or_default();
    roles::assign(&catalog, active, DiversityPolicy::Require, &wanted)
    .ok_or_else(|| {
        AppError::message("no model is configured — the swarm cannot spawn a member")
    })
}
