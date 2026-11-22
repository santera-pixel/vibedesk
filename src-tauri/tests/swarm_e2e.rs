//! The swarm, driven end to end by scripted agents.
//!
//! This is the test the suite was missing. Every other swarm test exercises a
//! helper: `plan::validate`, `worktree::merge_deps`, `schedule::apply_verdict`,
//! `roles::worker_brief`. None of them reaches `drive()`, which is 1457 lines and
//! needs a live agent to run. So the suite was green while a real run could not
//! finish, and the green was measuring the parts rather than the assembly.
//!
//! `swarm-sim` closes that. It is a scripted ACP agent, and `VIBEDESK_ACP_BIN`
//! already points the engine at any binary. So these tests run the real drive
//! loop, the real worktrees, the real git, the real two-tree proof and the real
//! integration gate, with no model and no tokens.
//!
//! The four tests are the four shapes a run takes:
//!
//! * `a_run_reaches_done` — the happy path, all the way to a reflection.
//! * `a_worker_that_commits_outside_its_grant_is_corrected` — the worker strays,
//!   the verifier fails it, the retry stays in grant, the run still finishes.
//! * `a_criterion_that_passes_on_base_is_refused_before_a_worker_spawns` — the
//!   plan gate catches an unprovable criterion on `plan_attempts`, not on the
//!   amendment budget.
//! * `a_verifier_that_escalates_goes_to_the_lead_not_the_user` — a question the
//!   worker cannot answer reaches the lead as an amendment, and the run resumes.
//!
//! Serialized, because the engine is a process-wide singleton and the
//! environment is process-wide too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vibedesk_lib::acp::bridge::NullSink;
use vibedesk_lib::swarm::board::BoardKind;
use vibedesk_lib::swarm::engine::engine;
use vibedesk_lib::swarm::plan::Plan;
use vibedesk_lib::swarm::roles::{RoleModels, ThinkingByRole};
use vibedesk_lib::swarm::{Run, RunState};

/// One run at a time. The engine holds a single run in a `static`, and the
/// scripted agent reads its script from an environment variable, so two tests
/// running at once would drive each other's agents.
static HARNESS: Mutex<()> = Mutex::new(());

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// A repository with two independent modules and a passing base suite.
fn base_repo(root: &Path) -> PathBuf {
    let repo = root.join("project");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "sim@local"]);
    git(&repo, &["config", "user.name", "sim"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    write(&repo.join("src/a.js"), "export const a = 1;\n");
    write(&repo.join("src/b.js"), "export const b = 1;\n");
    write(
        &repo.join("test/base.test.js"),
        "import test from 'node:test';\nimport assert from 'node:assert/strict';\n\ntest('base', () => assert.equal(1, 1));\n",
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    repo
}

fn plan_json() -> Value {
    json!({
        "goal": "Raise a and b to 2, each with its own test.",
        "tasks": [
            {
                "id": "t1",
                "title": "Raise a",
                "detail": "Set src/a.js to export a = 2 and add test/a.test.js asserting it.",
                "files": ["src/a.js", "test/a.test.js"],
                "deps": [],
                "acceptance": "node --test test/a.test.js"
            },
            {
                "id": "t2",
                "title": "Raise b",
                "detail": "Set src/b.js to export b = 2 and add test/b.test.js asserting it.",
                "files": ["src/b.js", "test/b.test.js"],
                "deps": [],
                "acceptance": "node --test test/b.test.js"
            }
        ],
        "integrationSuite": "node --test",
        "notes": ""
    })
}

fn evidence(task: &str, command: &str) -> Value {
    json!({
        "kind": "evidence",
        "task": task,
        "text": format!("Ran `{command}` in the worktree: pass 1, fail 0."),
        "evidence": [format!("{command} -> pass 1 fail 0")]
    })
}

fn verdict(task: &str, command: &str) -> Value {
    json!({
        "kind": "verdict",
        "task": task,
        "text": "passed. The criterion fails on the base commit and passes on the change.",
        "evidence": [format!("{command} -> exit 1 on base, exit 0 on the branch")],
        "command": command
    })
}

fn failed_verdict(task: &str, command: &str, why: &str) -> Value {
    json!({
        "kind": "verdict",
        "task": task,
        "text": format!("failed. {why}"),
        "evidence": [format!("{command} -> exit 1 on base, exit 1 on the branch")],
        "command": command
    })
}

fn reflection() -> Value {
    json!({
        "summary": "Two independent modules, one test each.",
        "gotchas": [
            {
                "file": "test/base.test.js",
                "text": "The base suite runs with `node --test test/`, so a new test file is picked up without a runner change.",
                "source": "verdict t1"
            }
        ]
    })
}

/// The steps that make t1 and t2 real: write the module, write its test, commit.
fn raise_a() -> Value {
    json!({
        "sh": [
            "printf 'export const a = 2;\\n' > src/a.js",
            "printf \"import test from 'node:test';\\nimport assert from 'node:assert/strict';\\nimport { a } from '../src/a.js';\\n\\ntest('a is 2', () => assert.equal(a, 2));\\n\" > test/a.test.js",
            "git add -A && git commit -qm 't1: a is 2'"
        ],
        "files": { "<inbox>/evidence.json": evidence("t1", "node --test test/a.test.js") }
    })
}

fn raise_b() -> Value {
    json!({
        "sh": [
            "printf 'export const b = 2;\\n' > src/b.js",
            "printf \"import test from 'node:test';\\nimport assert from 'node:assert/strict';\\nimport { b } from '../src/b.js';\\n\\ntest('b is 2', () => assert.equal(b, 2));\\n\" > test/b.test.js",
            "git add -A && git commit -qm 't2: b is 2'"
        ],
        "files": { "<inbox>/evidence.json": evidence("t2", "node --test test/b.test.js") }
    })
}

/// The happy path: two tasks, two verifiers, one reflection.
fn happy_script() -> Value {
    json!({
        "members": {
            "lead": [{ "files": { "<inbox>/plan.json": plan_json() } }],
            "w-t1": [raise_a()],
            "v-t1": [{ "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/a.test.js") } }],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    })
}

struct Harness {
    root: PathBuf,
    repo: PathBuf,
    sink: Arc<NullSink>,
    models: RoleModels,
}

impl Harness {
    /// Point the engine at a scripted agent and a throwaway repository.
    ///
    /// `VIBEDESK_DATA_DIR` is cached on first use, so every test in this binary
    /// shares one data directory. That is fine: each run gets its own id, and the
    /// directory is never deleted out from under a later test.
    fn new(tag: &str, script: Value) -> Self {
        let root = std::env::temp_dir().join(format!("vd-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        std::env::set_var(
            "VIBEDESK_DATA_DIR",
            std::env::temp_dir().join("vd-e2e-data"),
        );
        std::env::set_var("VIBE_HOME", root.join("vibe"));
        std::env::set_var("VIBEDESK_ACP_BIN", env!("CARGO_BIN_EXE_swarm-sim"));
        std::env::set_var("VIBEDESK_SIM_STATE", root.join("simstate"));
        let script_path = root.join("script.json");
        write(&script_path, &script.to_string());
        std::env::set_var("VIBEDESK_SIM_SCRIPT", &script_path);

        let repo = base_repo(&root);
        Self {
            root,
            repo,
            sink: Arc::new(NullSink),
            models: RoleModels {
                lead: "sim".into(),
                worker: "sim".into(),
                verifier: "sim".into(),
                diverse: false,
                thinking: ThinkingByRole {
                    lead: "high".into(),
                    worker: "high".into(),
                    verifier: "high".into(),
                },
            },
        }
    }

    async fn start(&self) {
        // The engine is a process-wide singleton, so a run the previous test left
        // live refuses this one with "run … is still planning — stop it before
        // starting another". The mutex serializes the tests; this makes each one
        // start from a stopped run instead of inheriting the last one's state.
        let stale = engine().run().filter(|run| run.state.is_live());
        if let Some(run) = stale {
            let sink: Arc<dyn vibedesk_lib::acp::bridge::EventSink> = self.sink.clone();
            let _ = engine().stop(&sink);
            wait_for(
                &format!("run {} to stop", run.id),
                Duration::from_secs(30),
                || engine().run().map(|r| !r.state.is_live()).unwrap_or(true),
            );
        }
        engine()
            .start(
                self.sink.clone(),
                "Raise a and b to 2, each with its own test.".into(),
                self.repo.clone(),
                self.models.clone(),
                5_000_000,
                None,
                2,
            )
            .await
            .expect("the run starts");
    }

    /// Wait for the lead's plan and approve it, the way the panel's button does.
    async fn approve(&self) -> Run {
        wait_for("the plan", Duration::from_secs(60), || {
            engine()
                .run()
                .map(|r| r.state == RunState::AwaitingApproval)
                .unwrap_or(false)
        });

        let planned = engine().run().expect("a run");
        let plan = Plan {
            goal: planned.goal.clone(),
            tasks: planned.tasks.clone(),
            integration_suite: planned.integration.suite.clone(),
            notes: String::new(),
            skills_to_write: Vec::new(),
        };
        engine()
            .approve(self.sink.clone(), plan, self.models.clone(), 2)
            .await
            .expect("the plan is approved");
        planned
    }

    async fn finish(&self, limit: Duration) -> Run {
        wait_for("the run to finish", limit, || {
            engine()
                .run()
                .map(|r| r.state.is_terminal())
                .unwrap_or(false)
        });
        engine().run().expect("a run")
    }

    /// Wait for the run to release its agents.
    ///
    /// Not housekeeping. The bridges live in the engine's `'static` map, so a
    /// terminal run that does not release them leaves one agent process per
    /// member alive, and those processes hold their worktree directories open.
    /// The harness found it the hard way: the test body finished and the process
    /// would not exit, because six children still held it.
    ///
    /// Waiting here is also what keeps the suite fast. A test that ends while the
    /// drive loop is still in its last tick tears the runtime down first, and the
    /// runtime drop then waits out every child's reaper — thirty seconds of dead
    /// time per test, and no proof that the release happened at all.
    async fn release(&self) {
        wait_for("the agents to be released", Duration::from_secs(30), || {
            engine()
                .board_tail()
                .iter()
                .any(|e| e.text.contains("released") && e.text.contains("agent process"))
        });
    }

    fn trail(&self) -> String {
        engine()
            .board_tail()
            .iter()
            .map(|e| format!("{} {}: {}", e.at, e.kind.label(), e.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_done(&self, run: &Run) {
        assert_eq!(
            run.state,
            RunState::Done,
            "the run finished as {} — {}\n\n{}",
            run.state.label(),
            run.failure.clone().unwrap_or_default(),
            self.trail()
        );
    }

    fn cleanup(self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Wait for a condition, on the calling thread.
///
/// Deliberately not `tokio::time::sleep`. The drive loop is async but its work is
/// not: it shells out to `git` and to the criterion command synchronously, so a
/// tick can hold a worker thread for as long as a test suite takes to run. With
/// every worker busy, the runtime's timer never fires, and an async wait here
/// stops advancing — the test hangs with the run already finished and nothing
/// left to observe. Blocking the calling thread instead leaves the workers free
/// to drive the run, which is the thing under test.
fn wait_for<F: Fn() -> bool>(what: &str, limit: Duration, check: F) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}");
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    HARNESS.lock().unwrap_or_else(|e| e.into_inner())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_reaches_done() {
    let _guard = lock();
    let h = Harness::new("done", happy_script());
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);

    // The work is really in the repository, not only in the run document.
    let a = std::fs::read_to_string(h.repo.join("src/a.js")).unwrap();
    let b = std::fs::read_to_string(h.repo.join("src/b.js")).unwrap();
    assert!(a.contains("a = 2"), "t1's change landed: {a}");
    assert!(b.contains("b = 2"), "t2's change landed: {b}");
    assert!(h.repo.join("test/a.test.js").is_file());
    assert!(h.repo.join("test/b.test.js").is_file());

    // The reflection ran and recorded what it learned.
    assert!(
        finished.reflection.is_some(),
        "the reflection pass produced a record"
    );

    // A finished run releases its agents.
    h.release().await;

    h.cleanup();
}

/// A worker that commits a file nobody granted it.
///
/// The engine reads the branch rather than asking the worker, records the stray
/// path on the task, and the verifier fails the attempt. The retry has to stay in
/// grant, and the run has to finish — a stray commit is a retry, not a question
/// for the user.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_that_commits_outside_its_grant_is_corrected() {
    let _guard = lock();
    let script = json!({
        "members": {
            "lead": [{ "files": { "<inbox>/plan.json": plan_json() } }],
            "w-t1": [
                {
                    "sh": [
                        "printf 'export const a = 2;\\n' > src/a.js",
                        "printf \"import test from 'node:test';\\nimport assert from 'node:assert/strict';\\nimport { a } from '../src/a.js';\\n\\ntest('a is 2', () => assert.equal(a, 2));\\n\" > test/a.test.js",
                        "printf 'export const stray = true;\\n' > stray.js",
                        "git add -A && git commit -qm 't1: a is 2, and a stray file'"
                    ],
                    "files": { "<inbox>/evidence.json": evidence("t1", "node --test test/a.test.js") }
                },
                {
                    "sh": [
                        "git rm -q stray.js",
                        "git commit -qm 't1: drop the stray file'"
                    ],
                    "files": { "<inbox>/evidence.json": evidence("t1", "node --test test/a.test.js") }
                }
            ],
            "v-t1": [
                {
                    "files": {
                        "<inbox>/verdict.json": failed_verdict(
                            "t1",
                            "node --test test/a.test.js",
                            "The branch commits stray.js, which the plan did not grant t1."
                        )
                    }
                },
                { "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/a.test.js") } }
            ],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    });

    let h = Harness::new("grant", script);
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);

    // The engine named the stray file on the board, from the branch, before any
    // verifier spoke.
    let trail = h.trail();
    assert!(
        trail.contains("outside its grant") && trail.contains("stray.js"),
        "the board names the out-of-grant file:\n{trail}"
    );

    // The retry really removed it, and the run did not carry it into the result.
    assert!(
        !h.repo.join("stray.js").exists(),
        "the stray file is not in the integrated result"
    );
    let a = std::fs::read_to_string(h.repo.join("src/a.js")).unwrap();
    assert!(a.contains("a = 2"), "t1's real change survived the retry: {a}");

    // A stray commit is a retry, not a question for the user: the amendment
    // budget is untouched.
    assert_eq!(
        finished.amendments, 0,
        "an out-of-grant commit spends an attempt, not an amendment:\n{trail}"
    );

    h.release().await;
    h.cleanup();
}

/// A criterion that already passes on the base commit.
///
/// No worker can satisfy it, so it is the plan's fault. The engine proves it
/// against the base tree before a single worker spawns and sends the plan back on
/// `plan_attempts` — a budget the amendment question never touches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_criterion_that_passes_on_base_is_refused_before_a_worker_spawns() {
    let _guard = lock();

    // The first plan's t1 criterion is the base suite, which passes at HEAD.
    let mut bad = plan_json();
    bad["tasks"][0]["acceptance"] = json!("node --test test/base.test.js");

    let script = json!({
        "members": {
            "lead": [
                { "files": { "<inbox>/plan.json": bad } },
                { "files": { "<inbox>/plan.json": plan_json() } }
            ],
            "w-t1": [raise_a()],
            "v-t1": [{ "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/a.test.js") } }],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    });

    let h = Harness::new("base", script);
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);

    let trail = h.trail();
    assert!(
        trail.contains("already passes on the commit the run starts from"),
        "the board says why the plan was refused:\n{trail}"
    );
    assert_eq!(
        finished.plan_attempts, 1,
        "one refusal, one revision:\n{trail}"
    );
    assert_eq!(
        finished.amendments, 0,
        "a plan violation is not an amendment:\n{trail}"
    );

    h.release().await;
    h.cleanup();
}

/// A verifier cannot weaken the criterion by naming a different command.
///
/// The plan's acceptance is the authority; the verifier's `command` says which one
/// it ran. A verifier that names a command which passes on the base commit has
/// proved nothing, and the engine used to take that at face value: it escalated as
/// "the criterion is unprovable", the lead rewrote a criterion that was already
/// right, and the amendment was refused for changing nothing. The run then stopped
/// with the plan gate charged for the verifier's mistake.
///
/// So the engine proves the task's own criterion instead, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verifier_cannot_weaken_the_criterion_by_naming_an_easier_command() {
    let _guard = lock();

    let script = json!({
        "members": {
            "lead": [{ "files": { "<inbox>/plan.json": plan_json() } }],
            "w-t1": [raise_a()],
            "v-t1": [
                // The base suite passes at HEAD, so this proves nothing.
                { "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/base.test.js") } }
            ],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    });

    let h = Harness::new("weaken", script);
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);

    let trail = h.trail();
    assert!(
        trail.contains("the plan's acceptance is the authority"),
        "the board says which criterion was proved:
{trail}"
    );
    assert_eq!(
        finished.plan_attempts, 0,
        "the plan was not at fault, so the plan gate paid nothing:
{trail}"
    );
    assert_eq!(
        finished.amendments, 0,
        "and the lead was not asked to rewrite a criterion that was already right:
{trail}"
    );

    h.release().await;
    h.cleanup();
}

/// A verifier that asks a question the worker cannot answer.
///
/// The question goes to the lead as an amendment, the lead widens the grant, the
/// worker carries on, and the run finishes. The user is not asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verifier_that_escalates_goes_to_the_lead_not_the_user() {
    let _guard = lock();

    let script = json!({
        "members": {
            "lead": [
                { "files": { "<inbox>/plan.json": plan_json() } },
                {
                    "files": {
                        "<inbox>/amendment.json": {
                            "files": { "t1": ["src/a.js", "test/a.test.js", "src/shared.js"] },
                            "reason": "t1 also needs src/shared.js to wire the export through."
                        }
                    }
                }
            ],
            "w-t1": [
                raise_a(),
                {
                    "sh": [
                        "printf 'export const shared = 1;\\n' > src/shared.js",
                        "git add -A && git commit -qm 't1: wire through shared'"
                    ],
                    "files": { "<inbox>/evidence.json": evidence("t1", "node --test test/a.test.js") }
                }
            ],
            "v-t1": [
                {
                    "files": {
                        "<inbox>/verdict.json": {
                            "kind": "verdict",
                            "task": "t1",
                            "text": "The change is correct but t1 cannot finish it: the export has to be wired through src/shared.js, which the plan did not grant it.",
                            "evidence": ["node --test test/a.test.js -> exit 1 on base, exit 0 on the branch"],
                            "command": "node --test test/a.test.js",
                            "escalate": "t1 needs src/shared.js in its grant to wire the export through."
                        }
                    }
                },
                { "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/a.test.js") } }
            ],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    });

    let h = Harness::new("escalate", script);
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);

    let trail = h.trail();
    assert_eq!(
        finished.amendments, 1,
        "the lead answered once:\n{trail}"
    );
    assert!(
        trail.contains("src/shared.js"),
        "the board carries the question and the answer:\n{trail}"
    );
    // The amendment really widened the grant: the file the verifier asked for is
    // in the integrated result.
    assert!(
        h.repo.join("src/shared.js").is_file(),
        "the granted file landed:\n{trail}"
    );

    h.release().await;
    h.cleanup();
}

/// A two-task chain: t2 cannot start until t1 has committed.
fn chain_plan_json() -> Value {
    json!({
        "goal": "Raise a to 2, then raise b to 2 on top of it.",
        "tasks": [
            {
                "id": "t1",
                "title": "Raise a",
                "detail": "Set src/a.js to export a = 2 and add test/a.test.js asserting it.",
                "files": ["src/a.js", "test/a.test.js"],
                "deps": [],
                "acceptance": "node --test test/a.test.js"
            },
            {
                "id": "t2",
                "title": "Raise b",
                "detail": "Set src/b.js to export b = 2 and add test/b.test.js asserting it.",
                "files": ["src/b.js", "test/b.test.js"],
                "deps": ["t1"],
                "acceptance": "node --test test/b.test.js"
            }
        ],
        "integrationSuite": "node --test",
        "notes": ""
    })
}

fn chain_script() -> Value {
    json!({
        "members": {
            // Twice, because a chain's first rank is one task wide and the engine
            // sends that back once (engine.rs `shape_send_back`) before accepting
            // it whatever its shape. The scripted lead answers the second prompt
            // with the same plan, which is what a lead asked to widen a chain that
            // has nothing to widen says. One step here meant the run was sent back
            // and then never answered, so this test timed out waiting for a plan
            // that the gate had already agreed to accept on the retry.
            "lead": [
                { "files": { "<inbox>/plan.json": chain_plan_json() } },
                { "files": { "<inbox>/plan.json": chain_plan_json() } }
            ],
            "w-t1": [raise_a()],
            "v-t1": [{ "files": { "<inbox>/verdict.json": verdict("t1", "node --test test/a.test.js") } }],
            "w-t2": [raise_b()],
            "v-t2": [{ "files": { "<inbox>/verdict.json": verdict("t2", "node --test test/b.test.js") } }],
            "reflect": [{ "files": { "<inbox>/gotchas.json": reflection() } }]
        }
    })
}

/// Verification is not on the critical path.
///
/// t2 depends on t1. The scheduler starts t2 the moment t1's worker posts its
/// evidence — the commit exists, the verdict does not — so t2's claim lands
/// before v-t1's ruling. Requiring the verdict put verification between every
/// link of a chain: t1 work, t1 verify, t2 work, t2 verify, and the observed
/// five-link run spent roughly 95 minutes on 30 minutes of work that way.
///
/// Asserted from the board's own timestamps, which is what the panel shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dependent_starts_before_its_dependency_is_verified() {
    let _guard = lock();
    let h = Harness::new("spec", chain_script());
    h.start().await;
    h.approve().await;
    let finished = h.finish(Duration::from_secs(180)).await;
    h.assert_done(&finished);
    // Released before the assertions, so a failure here does not leave a live run
    // holding its agent processes open and stall the test binary on exit.
    h.release().await;

    let board = engine().board_tail();
    let claim = board
        .iter()
        .find(|e| e.kind == BoardKind::Claim && e.from == "w-t2")
        .unwrap_or_else(|| panic!("w-t2 never claimed its task\n\n{}", h.trail()));
    let ruling = board
        .iter()
        .find(|e| e.kind == BoardKind::Verdict && e.from == "v-t1")
        .unwrap_or_else(|| panic!("v-t1 never ruled\n\n{}", h.trail()));

    assert!(
        claim.at < ruling.at,
        "t2 claimed at {} and t1's verdict landed at {} — t2 waited for the verdict\n\n{}",
        claim.at,
        ruling.at,
        h.trail()
    );

    // And the record says what it was built on, which is what makes the early
    // start reversible.
    let t2 = finished.task("t2").expect("t2");
    assert!(
        t2.built_on.contains_key("t1"),
        "t2's base names t1: {:?}",
        t2.built_on
    );
    assert!(
        t2.base_commit.is_some(),
        "and it was proven against a real base"
    );

    h.cleanup();
}
