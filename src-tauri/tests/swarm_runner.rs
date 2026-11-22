//! The headless runner, as a process.
//!
//! The end-to-end suite drives the engine in-process, which cannot see the two
//! things that only a real process shows: whether the run released its agents
//! before `main` returned, and whether a member survives an engine that is killed
//! rather than shut down. Both were live defects. `swarm-run` returning from
//! `main` used to drop the runtime and cancel the drive loop before its next tick
//! could release anything, so a finished run left four agent processes and its
//! checkout directory behind; and a member can be killed at any moment, so the
//! promise that no member outlives the engine has to be the kernel's.
//!
//! Everything here is set up from code: a temporary `VIBE_HOME`, its agent
//! payload, a `config.toml` with two models and the managed MCP servers, and a
//! capability file with every capability on. No file from the user's install is
//! read, which is what makes this a test rather than a check of this machine.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

/// One run at a time: the two cases share the machine's process table.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Bed {
    root: PathBuf,
    workspace: PathBuf,
    vibe_home: PathBuf,
    data: PathBuf,
}

impl Bed {
    fn new(tag: &str, script: serde_json::Value) -> Self {
        let root = std::env::temp_dir().join(format!("vd-runner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let vibe_home = root.join("vibe");
        let data = root.join("data");
        let workspace = root.join("project");
        for dir in [&vibe_home, &data, &workspace] {
            std::fs::create_dir_all(dir).unwrap();
        }

        // The agent payload, exactly as provisioning writes it. Without these
        // files the lead runs on Vibe's coding prompt and implements the goal
        // itself, which is the failure the payload exists to prevent.
        for file in vibedesk_lib::agent_payload::FILES {
            let dir = vibe_home.join(file.dir.name());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file.name), file.body).unwrap();
        }

        let mut config = String::from("active_model = \"sim-a\"\n\n");
        for (alias, name) in [("sim-a", "sim-a-model"), ("sim-b", "sim-b-model")] {
            config.push_str(&format!(
                "[[models]]\nname = \"{name}\"\nprovider = \"mistral\"\nalias = \"{alias}\"\n\n"
            ));
        }
        // The preconditions compare the declared server names against the
        // capabilities that are on, and a capability that is on but undeclared is
        // blocked. The commands are never started by this harness: the sim is the
        // agent, and the servers are only names to the preflight.
        for server in vibedesk_lib::capabilities::external::SERVERS {
            config.push_str(&format!(
                "[[mcp_servers]]\nname = \"{}\"\ntransport = \"stdio\"\ncommand = \"cmd\"\n\n",
                server.name
            ));
        }
        std::fs::write(vibe_home.join("config.toml"), config).unwrap();

        let mut enabled = serde_json::Map::new();
        for id in vibedesk_lib::capabilities::CapabilityId::ALL {
            enabled.insert(id.key().to_string(), json!(true));
        }
        enabled.insert("swarm".into(), json!(true));
        std::fs::write(
            data.join("capabilities.json"),
            serde_json::to_string_pretty(&json!({ "enabled": enabled })).unwrap(),
        )
        .unwrap();

        let script_path = root.join("script.json");
        std::fs::write(&script_path, script.to_string()).unwrap();

        let bed = Self {
            root,
            workspace,
            vibe_home,
            data,
        };
        bed.seed_repo();
        std::env::set_var("VIBEDESK_SIM_SCRIPT", &script_path);
        std::env::set_var("VIBEDESK_SIM_STATE", bed.root.join("simstate"));
        bed
    }

    fn seed_repo(&self) {
        std::fs::create_dir_all(self.workspace.join("src")).unwrap();
        std::fs::create_dir_all(self.workspace.join("test")).unwrap();
        std::fs::write(self.workspace.join("src/a.js"), "export const a = 0;\n").unwrap();
        std::fs::write(self.workspace.join("src/b.js"), "export const b = 0;\n").unwrap();
        std::fs::write(self.workspace.join("test/README.md"), "// placeholder\n").unwrap();
        git(&self.workspace, &["init", "-q", "."]);
        git(&self.workspace, &["add", "-A"]);
        git(
            &self.workspace,
            &[
                "-c",
                "user.email=sim@local",
                "-c",
                "user.name=sim",
                "commit",
                "-qm",
                "seed",
            ],
        );
    }

    fn runner(&self, timeout_secs: u64) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_swarm-run"));
        command
            .current_dir(&self.root)
            .env("VIBE_HOME", &self.vibe_home)
            .env("VIBEDESK_DATA_DIR", &self.data)
            .env("VIBEDESK_ACP_BIN", env!("CARGO_BIN_EXE_swarm-sim"))
            .args(["--workspace", &self.workspace.display().to_string()])
            .args(["--goal", "Raise a and b to 2, each with its own test."])
            .args(["--model", "sim-a"])
            .args(["--budget-micros", "1500000"])
            .args(["--concurrency", "2"])
            .args(["--timeout", &timeout_secs.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn runs(&self) -> Vec<PathBuf> {
        let dir = self.data.join("swarm");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut runs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        runs.sort();
        runs
    }

    fn board(&self, run: &Path) -> Vec<serde_json::Value> {
        let Ok(body) = std::fs::read_to_string(run.join("board.jsonl")) else {
            return Vec::new();
        };
        body.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

impl Drop for Bed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

/// Every `swarm-sim.exe` on the machine, by pid.
fn sim_pids() -> Vec<u32> {
    let out = Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq swarm-sim.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split("\",\"").nth(1))
        .filter_map(|pid| pid.trim_matches('"').parse().ok())
        .collect()
}

fn script() -> serde_json::Value {
    let body = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/swarm-sim-script.json"
    ))
    .expect("the fixture script");
    serde_json::from_str(&body).expect("valid json")
}

#[test]
fn a_finished_run_releases_its_agents_before_the_runner_exits() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bed = Bed::new("release", script());
    let started = Instant::now();
    let out = bed.runner(200).output().expect("the runner runs");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "the runner exited {:?}\n{stdout}\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("released 4 agent processes"),
        "the runner reported its release\n{stdout}"
    );

    // The regression: the process used to linger until its children happened to
    // die, six minutes on a live run. A run whose deadline is 200 s finishing in
    // well under half that is the observable difference.
    assert!(
        elapsed < Duration::from_secs(120),
        "the runner took {elapsed:?}, which is the hang this test exists for"
    );

    let runs = bed.runs();
    assert_eq!(runs.len(), 1, "one run, and it is on disk: {runs:?}");
    let run = &runs[0];

    let notes: Vec<String> = bed
        .board(run)
        .iter()
        .filter_map(|entry| entry.get("text").and_then(|t| t.as_str()).map(str::to_string))
        .filter(|text| text.contains("released") || text.contains("reaped"))
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "one release, not one per caller: {notes:?}"
    );
    assert!(notes[0].contains("released 4 agent processes"), "{notes:?}");

    let id = run.file_name().unwrap().to_string_lossy().to_string();
    let checkout = bed.vibe_home.join("worktrees").join(&id);
    assert!(
        !checkout.exists(),
        "a finished run removed its checkouts at {}",
        checkout.display()
    );
    assert!(
        sim_pids().is_empty(),
        "no agent process outlived the run"
    );
}

#[test]
fn a_member_dies_with_the_engine_that_started_it() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // A member busy inside a command, not idle on its stdin. That is the state
    // that leaked: `kill_on_drop` never runs, and a process blocked in a child
    // call is not reading its pipe, so closing stdin does not reach it either.
    let bed = Bed::new(
        "kill",
        json!({ "members": { "lead": [{ "sh": ["sleep 120"] }] } }),
    );
    let before = sim_pids();
    let mut child = bed.runner(600).spawn().expect("the runner starts");

    let deadline = Instant::now() + Duration::from_secs(30);
    let spawned: Vec<u32> = loop {
        let fresh: Vec<u32> = sim_pids()
            .into_iter()
            .filter(|pid| !before.contains(pid))
            .collect();
        if !fresh.is_empty() {
            break fresh;
        }
        assert!(
            Instant::now() < deadline,
            "no member was spawned within 30s — the runner is blocked at preflight"
        );
        std::thread::sleep(Duration::from_millis(200));
    };

    // Kill the engine the way a user's task manager does: no unwinding, no
    // destructors, no runtime shutdown. `kill_on_drop` cannot run, so only the
    // job object can stop the members.
    // No `/T`: killing the tree would prove nothing about the job object.
    let _ = Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/F"])
        .status();
    let _ = child.wait();

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let alive: Vec<u32> = sim_pids()
            .into_iter()
            .filter(|pid| spawned.contains(pid))
            .collect();
        if alive.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "members {alive:?} outlived the engine that started them"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}
