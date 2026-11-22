//! What each role is told, and which model it runs on.
//!
//! ACP has no field for a client-supplied system prompt — `session/new` takes
//! `cwd`, `additional_directories` and `mcp_servers`, and discards everything
//! else — so a role is expressed three ways: the **worktree** it runs in, the
//! **model** it is switched to, and the **brief** that rides on its first turn.
//! The brief is the one that actually shapes behaviour, so it is the one this
//! module takes seriously.
//!
//! The briefs are deliberately short. A worker's whole advantage over a single
//! agent is that its context is small; a brief that recites the repository would
//! spend the advantage before the first tool call.

use super::{plan, Gotcha, Task};
use super::prove::Proof;

/// How hard the run tries to give the verifier a different model from the
/// worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiversityPolicy {
    /// Refuse to start without a second model. The strongest setting, and the
    /// one the plan proposes as the default.
    #[default]
    Require,
    /// Use a second model when one is configured, and say so when there is not.
    Prefer,
    /// Use whatever is configured. A verifier on the same model is a second
    /// opinion, not an independent one.
    Allow,
}

impl DiversityPolicy {
    pub fn key(self) -> &'static str {
        match self {
            DiversityPolicy::Require => "require",
            DiversityPolicy::Prefer => "prefer",
            DiversityPolicy::Allow => "allow",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "require" => Some(DiversityPolicy::Require),
            "prefer" => Some(DiversityPolicy::Prefer),
            "allow" => Some(DiversityPolicy::Allow),
            _ => None,
        }
    }
}

/// Which model each role runs on, and how hard it thinks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleModels {
    pub lead: String,
    pub worker: String,
    pub verifier: String,
    /// Whether the verifier is genuinely a different model.
    pub diverse: bool,
    /// The reasoning level each role asks for.
    ///
    /// A level is legal only for the model that accepts it — `glm-5-3` takes
    /// `low, high, max` and `mistral-medium-3.5` takes `off, high` — so this is
    /// settled against the catalogue rather than trusted, and the roster records
    /// what the session actually applied.
    #[serde(default)]
    pub thinking: ThinkingByRole,
}

/// A reasoning level per role.
///
/// Empty means "leave the session's default", which is a real choice and not a
/// missing one: the agent picks a level per model, and a run that overrides
/// nothing is a run that inherits the model's own judgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingByRole {
    #[serde(default)]
    pub lead: String,
    #[serde(default)]
    pub worker: String,
    #[serde(default)]
    pub verifier: String,
}

impl ThinkingByRole {
    /// The level a role asks for.
    pub fn for_role(&self, role: super::Role) -> &str {
        match role {
            super::Role::Lead => &self.lead,
            super::Role::Worker => &self.worker,
            super::Role::Verifier => &self.verifier,
            // The reflector writes one file from a digest. It has no reasoning
            // to spend, so it inherits whatever the session defaulted to.
            super::Role::Reflector => "",
        }
    }

    /// Whether every role inherits the session default.
    pub fn is_inherited(&self) -> bool {
        self.lead.is_empty() && self.worker.is_empty() && self.verifier.is_empty()
    }
}

/// The reasoning level a role should ask for, given what the user chose.
///
/// The defaults are the point of the feature rather than a convenience: a lead
/// plans and integrates, which is the work that benefits from thinking; a worker
/// implements against a brief that already names its files and its criterion; a
/// verifier has to find the flaw the worker missed, which is the hardest read in
/// the run. So the verifier is never the cheapest role.
pub fn default_thinking(role: super::Role) -> &'static str {
    match role {
        super::Role::Lead => "high",
        super::Role::Worker => "medium",
        super::Role::Verifier => "high",
        super::Role::Reflector => "",
    }
}

/// Choose a model per role.
///
/// The lead and the worker share a model on purpose: the lead's job is planning
/// and integration, which is the same kind of work, and splitting them would
/// spend a second model's context on the same reasoning. The verifier is the one
/// that must differ, because a verifier on the worker's model shares its blind
/// spots — the error that produced the change also produces the approval.
pub fn pick_models(
    available: &[String],
    active: Option<&str>,
    policy: DiversityPolicy,
) -> Option<RoleModels> {
    let first = available.first()?.clone();
    let worker = active
        .map(str::to_string)
        .filter(|name| available.iter().any(|m| m == name))
        .unwrap_or_else(|| first.clone());

    let alternative = available.iter().find(|m| **m != worker).cloned();

    let (verifier, diverse) = match (policy, alternative) {
        (DiversityPolicy::Allow, _) => (worker.clone(), false),
        (_, Some(other)) => (other, true),
        (_, None) => (worker.clone(), false),
    };

    Some(RoleModels {
        lead: worker.clone(),
        worker,
        verifier,
        diverse,
        thinking: ThinkingByRole::default(),
    })
}

/// What the user chose about the crew.
///
/// Every field is optional in the sense that matters: an empty string means "let
/// the picker decide", which is a real answer and not a missing one. The panel
/// sends this on every start and approve, so the run that begins is the run the
/// panel showed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrewChoice {
    /// A model for the lead, or empty to share the worker's.
    #[serde(default)]
    pub lead: String,
    /// A model for the workers, or empty to use the active model.
    #[serde(default)]
    pub worker: String,
    /// A model for the verifiers, or empty to take the first different one.
    #[serde(default)]
    pub verifier: String,
    /// The reasoning level per role, or empty for the role's default.
    #[serde(default)]
    pub thinking: ThinkingByRole,
}

impl CrewChoice {
    /// Whether the user chose nothing at all.
    pub fn is_empty(&self) -> bool {
        self.lead.is_empty()
            && self.worker.is_empty()
            && self.verifier.is_empty()
            && self.thinking.is_inherited()
    }
}

/// Choose a model per role *and* settle a reasoning level for each.
///
/// This is the function the panel and the engine both call, so that the model a
/// role is shown on and the level it is shown at are decided in one place. The
/// level is settled against the catalogue because a level legal for one model is
/// refused by another: asking `mistral-medium-3.5` for `max` is a request the
/// agent answers by keeping its own value, and a roster that showed `max` would
/// be describing the intent rather than the run.
///
/// A model the catalogue does not list is ignored rather than passed through.
/// The agent would refuse it, the member would run on the session default, and
/// the roster would then disagree with the plan — so the picker's answer stands
/// and the panel shows what it settled on.
pub fn assign(
    catalog: &super::catalog::Catalog,
    active: Option<&str>,
    policy: DiversityPolicy,
    choice: &CrewChoice,
) -> Option<RoleModels> {
    let available = catalog.aliases();
    let mut models = pick_models(&available, active, policy)?;

    let pick = |wanted: &str, fallback: &str| {
        let wanted = wanted.trim();
        if !wanted.is_empty() && catalog.get(wanted).is_some() {
            wanted.to_string()
        } else {
            fallback.to_string()
        }
    };

    models.worker = pick(&choice.worker, &models.worker);
    models.lead = pick(&choice.lead, &models.worker);
    models.verifier = pick(&choice.verifier, &models.verifier);
    // Recomputed rather than inherited: an override can make the verifier the
    // worker's model, and a run that claimed independence it no longer has is
    // the one lie this field exists to prevent.
    models.diverse = models.verifier != models.worker;

    let settle = |role: super::Role, model: &str, asked: &str| {
        // Whether the user named this level or the role default supplied it.
        // `settle` needs the difference: a default is this program's guess and
        // gives way to the model's declared level, an explicit request does not.
        let explicit = !asked.trim().is_empty();
        let asked = if explicit {
            asked
        } else {
            default_thinking(role)
        };
        catalog.settle(model, asked, explicit).unwrap_or_default()
    };

    let lead = settle(super::Role::Lead, &models.lead, &choice.thinking.lead);
    let worker = settle(super::Role::Worker, &models.worker, &choice.thinking.worker);
    let verifier = settle(
        super::Role::Verifier,
        &models.verifier,
        &choice.thinking.verifier,
    );
    models.thinking = ThinkingByRole {
        lead,
        worker,
        verifier,
    };

    Some(models)
}

/// The agent a role runs as.
///
/// These four names are the payload `provision` writes to
/// `~/.vibe/agents/NAME.toml`, and they are what the engine selects on the
/// session before the first turn. Selecting them is what makes the payload
/// matter: without it the member keeps the session default, which is Vibe's
/// coding prompt, and the lead implements the goal instead of planning it.
///
/// Where the name lives on the session depends on the agent version. `vibe-acp`
/// 3.1.2 advertised a custom agent in `availableModes` beside the permission
/// modes; 3.1.4 splits them — `mode` is `ask`, `accept-edits`, `auto-approve`,
/// and the agent is a separate `agent` option that lists these four. Probed
/// against the installed 3.1.4: `session/new` returns an `agent` option whose
/// values are exactly the four below. The engine asks the session which option
/// carries it rather than assuming, so both versions run.
pub fn agent_for(role: super::Role) -> &'static str {
    match role {
        super::Role::Lead => "vibedesk-swarm-lead",
        super::Role::Worker => "vibedesk-swarm-worker",
        super::Role::Verifier => "vibedesk-swarm-verifier",
        super::Role::Reflector => "vibedesk-swarm-reflector",
    }
}

/// The lead's brief.
///
/// The lead is the only member that writes a plan, and the only one whose
/// acceptance criterion is the *integrated* result rather than one task. Both
/// facts are stated, because a lead that plans per-task and never integrates is
/// the failure mode that makes a swarm slower than a chat.
///
/// `history` is what the last runs of this workspace learned, from
/// [`super::history::History::lead_section`]. It is a parameter rather than a
/// read because the brief is a pure function of what it is handed, and the
/// workspace's history is the caller's to load. Empty on the first run here, and
/// the section is then absent rather than empty: a heading with nothing under it
/// reads as "there is nothing to know", which is a different claim from "nothing
/// has run here yet".
pub fn lead_brief(
    goal: &str,
    workspace: &str,
    run_id: &str,
    inbox: &str,
    skills: &str,
    history: &str,
) -> String {
    format!(
        r#"You are the lead of a swarm run. You plan, you integrate, and you audit. You do not implement.

## The goal
{goal}

## The workspace
{workspace}

{history}## What you produce
A plan, written as one JSON file into `{inbox}/plan.json`, with this shape:

{{
  "goal": "one sentence: what the run produces",
  "integrationSuite": "the command that proves the INTEGRATED result",
  "notes": "anything the user should see before approving",
  "tasks": [
    {{
      "id": "t1",
      "title": "short imperative",
      "detail": "what to do, in the worker's own words",
      "files": ["src/a.rs"],
      "deps": [],
      "acceptance": "one shell command that exits 0 when the task is done",
      "skills": ["the-name-of-a-skill-from-the-catalogue-below"]
    }}
  ],
  "skillsToWrite": [
    {{
      "name": "lowercase-hyphenated",
      "description": "one line: what method this is",
      "body": "the method, as markdown, the way you would explain it to the next worker",
      "because": "which task needs it and why this machine does not have it"
    }}
  ]
}}

{skills}
## The rules your plan is checked against
- At least two tasks. One task is a chat, and the run is refused.
- Every task declares the files it will touch. Two tasks that are not ordered by
  `deps` may not claim the same file — their worktrees would conflict.
- Every task has an acceptance criterion that can be *run*. "It works" is
  refused. "`cargo test rate_limit` passes, and fails on the base commit" is not.
- `acceptance` is **one shell command**, run verbatim with `bash -c`. Not a
  description of one, not a sentence containing one. The engine executes the
  field; the verifier reads it. When it holds prose the two disagree about the
  same string and the run burns every attempt discovering it. Write
  `node --test test/gates.test.js` and put what the test asserts in `detail`.
  A criterion containing an unquoted `, ` or `: ` is refused as a sentence.
- `skillsToWrite` is for a method this machine does not have and the work needs.
  At most two. They are written into the run's own directory and handed to this
  run's workers; they are not installed on the machine, and keeping one is the
  user's click. Do not use it to restate a skill that is already in the
  catalogue — assign that one instead. A name that already exists is refused.
- The criterion must be satisfiable using **only the files that task owns**. A
  criterion that greps a directory another task owns is unsatisfiable by
  construction: the task cannot make it pass, and the run spends two attempts and
  two verifier turns discovering that. A repo-wide invariant belongs in
  `integrationSuite`, not in a task's criterion.
- The **work** must be completable using only the files that task owns, not just
  its criterion. A task that extracts behaviour into a new module has to own the
  file it extracts *from*, or the old copy stays behind and the new module is
  imported by nothing but its own test. The fourteenth live run's t2 did exactly
  that: it wrote `src/logic/ship.js` correctly, `src/player.js` still held the
  inline copy, and the verifier escalated because the missing edit was outside
  the grant. If a task moves behaviour, it owns both ends of the move.
- `integrationSuite` is required. Every task can pass its own verifier and the
  combination still be broken; that is the failure this field exists to catch.
- `deps` must name tasks in the plan, and must not form a cycle.

## Your own acceptance criterion
The integrated suite passing on the integration branch. Not the sum of the
per-task verdicts. If the suite fails after every task passed, you attribute it
to a task and say which.

## The board
Write JSON files into `{inbox}/` to speak. One file per entry, any name ending
in `.json`:

{{"kind": "decision", "text": "what you decided and why", "task": "run"}}

`kind` is one of `claim`, `decision`, `evidence`, `verdict`, `conflict`,
`escalation`, `note`. Only `decision` crosses a dependency edge — a worker
downstream of a task receives that task's decisions and nothing else. Write
decisions as constraints the next worker must honour, not as a narrative.

Read `{inbox}/receipts/` to see what the engine accepted. A rejected entry
carries the reason; fix it rather than repeating it.

## Run id
{run_id}
"#,
        goal = goal,
        workspace = workspace,
        inbox = inbox,
        skills = skills,
        history = history,
        run_id = run_id,
    )
}

/// The lead's second turn: the gate refused the plan, and here is why.
///
/// Written as a correction, not a rejection. The violations are the whole
/// message — a lead told "the plan was refused" cannot fix anything, and a lead
/// told exactly which rule it broke can. The rest of the brief is not repeated:
/// the lead still has it in its own context, and re-sending it would spend the
/// tokens the revision exists to save.
pub fn revision_brief(reasons: &str, inbox: &str) -> String {
    format!(
        r#"The plan you wrote was refused by the gate. Nothing has been spawned and nothing has been spent beyond this turn.

## Why
  - {reasons}

## What to do
Rewrite `{inbox}/plan.json` in full, with the same shape as before, fixing every
reason above. Do not write a second file and do not explain the fix in prose —
the corrected plan is the whole answer.

The rules, for reference:
- At least two tasks.
- Every task declares the files it will touch, and two tasks that are not ordered
  by `deps` may not claim the same file.
- Every task has an acceptance criterion that can be run, and that the task can
  satisfy using only the files it owns. A criterion that greps a directory
  another task owns is unsatisfiable — put repo-wide invariants in
  `integrationSuite`.
- The work, not just the criterion, has to be completable from the files the task
  owns. A task that extracts behaviour into a new module owns the file it
  extracts from, or the old copy stays behind.
- `acceptance` is one shell command, run verbatim with `bash -c`. A sentence
  containing a command is not a command: an unquoted `, ` or `: ` is refused.
- `acceptance` must FAIL on the commit the run starts from and pass after the
  work. The engine runs it against the base tree before it spawns anything, so a
  criterion that already passes there is refused — it cannot tell the change from
  the base. "The existing suite still passes" is not a criterion; write the
  command that observes the behaviour you are adding, even if that means the
  command is a new test file the task also owns.
- `integrationSuite` is required: the command that proves the INTEGRATED result.
- `deps` must name tasks in the plan and must not form a cycle.
"#
    )
}

/// A worker's turn ended without evidence. Ask it again, in its own session.
///
/// Short on purpose: the worker still has its brief, its worktree and its
/// context. Re-sending the brief would spend the tokens the retry exists to save,
/// and the one thing it needs to know is that the run is waiting on it.
pub fn worker_reminder_brief(task: &Task, inbox: &str) -> String {
    format!(
        r#"Your turn ended and the run is waiting on you. Nothing has been lost: your worktree and your commits are where you left them.

## What is missing
One `evidence` entry in `{inbox}/`, naming the command you ran and the line that proves it passed. The verifier cannot start without it.

## The task
{id} — {title}

## The criterion
{acceptance}

If you are blocked, write an `escalation` instead and say what is blocking you. Silence is the one answer the run cannot act on.
"#,
        id = task.id,
        title = task.title,
        acceptance = task.acceptance.trim(),
    )
}

/// A worker stopped because the plan does not cover something it broke.
///
/// This is the lead's question, not the user's. The worker named the files; the
/// lead decides which task owns them. The contract is narrow on purpose — see
/// `amend` — and the brief says so, because a lead that rewrites the plan under a
/// running worker can lose the run's result.
pub fn amendment_brief(question: &str, inbox: &str, tasks: &str) -> String {
    format!(
        r#"The run stopped on a question it cannot answer mechanically. It is not a failure and nothing is lost: the work is committed on its branches and the worktrees are kept.

## What it said
{question}

## The plan
{tasks}

## What to do
Answer the question, and write `{inbox}/amendment.json`. There are two things you
may change, and you may change both at once.

**Assign files to a task that already exists**, when a worker broke a file no
task owns:

{{"files": {{"t2": ["test/integration.test.js"]}}, "reason": "t1 changed the gate contract and t2 rewires the renderer"}}

**Assign files to a task that already exists**, when two dependencies disagree
about a file and neither owns it. The engine resolves a conflict in favour of the
task the plan gives the file to; if no task owns it, there is nothing to resolve
from and the run stops. Grant the file to the task that has to reconcile it, and
that task starts from the incoming dependency's version with the reconciliation
as its work:

{{"files": {{"t3": ["src/player.js"]}}, "reason": "t1 and t2 both changed src/player.js and neither owns it; t3 is the task that has to make it right"}}

**Rewrite a task's acceptance criterion**, when the criterion cannot be proven.
A criterion that passes on the base commit as well as on the change proves
nothing: the verifier runs it on both, and if both pass it cannot tell the work
from the starting point. That is your writing, not the worker's, so you are the
one who fixes it. Write a command that exercises the behaviour the change
introduces and fails without it:

{{"acceptance": {{"t1": "node --test test/gates.test.js && node -e \"import('./src/logic/gates.js').then(m => process.exit(m.fieldExtent ? 0 : 1))\""}}, "reason": "the old criterion ran the base test file, which passes on base"}}

You may only **add files to tasks that already exist** and **rewrite the
criterion of a task that is not yet verified**. You may not add tasks, remove
tasks, change `deps`, or touch a task that is already verified — those changes
would invalidate work the run has already paid for and already proved. A
rewritten criterion is still proved failing-first by the verifier, so weakening
one to buy a pass does not work.

If the narrow answer is not enough, write `{{"files": {{}}, "reason": "..."}}`
explaining what the plan actually needs, and the run will stop and ask the user.

The run resumes as soon as the amendment is applied. Do not re-plan from
scratch and do not write `plan.json` again.
"#
    )
}

/// The worker that stopped, once the lead has answered.
pub fn amended_brief(changed: &str) -> String {
    format!(
        r#"The lead amended the plan. The run is continuing and you are still the owner of your task.

## What changed
{changed}

Carry on from where you stopped. The files above now belong to the tasks named,
so a break in one of them is that task's job, not yours. Do not re-do work you
have already committed.
"#
    )
}

/// A verifier refused this task's work. Tell the worker why, in its own session.
///
/// A retry reuses the member rather than spawning a new one: its session still
/// holds the worktree, the files it read and the decisions it made, so the
/// cheapest correct thing is to say what was wrong. A fresh process would pay
/// for the whole brief again and lose the context that produced the work.
pub fn retry_brief(task: &Task, reason: &str, inbox: &str) -> String {
    format!(
        r#"A verifier refused your work on {id}. The task is back with you.

## What the verifier said
{reason}

## What to do
Fix the work in this worktree and commit it. Then post one `evidence` entry to
`{inbox}/` naming the command you ran and the line that settled it, exactly as
before. Do not re-post the old evidence: it is what was refused.

Read the refusal carefully. The most common shape is that the change is real but
nothing uses it — a new export that no module imports, a new file that no entry
point loads, a fix applied to a copy of the constant instead of the constant the
game reads. If the verifier says the criterion passes on the base commit, the
criterion is not a test and you have to make it one.

## The board
Write JSON files into `{inbox}/` to speak. One file per entry, any name ending
in `.json`:

{{"kind": "evidence", "text": "what you ran and what it printed", "task": "{id}", "evidence": ["the command -> the line that settled it"]}}

Read `{inbox}/receipts/` to see what the engine accepted. A rejected entry is
not a posted entry: the task stays with you until one is accepted.

## The task
{title}

{detail}

## Acceptance
{acceptance}
"#,
        id = task.id,
        title = task.title,
        detail = task.detail,
        acceptance = task.acceptance,
        inbox = inbox,
    )
}

/// The lead's turn ended without a plan. Ask again, briefly.
///
/// A turn can end with nothing written — a provider that drops its tool calls, a
/// model that answers in prose. The run then waits for a file that is never
/// coming. This is the cheapest possible recovery: one short turn, no new
/// process, no re-sent brief.
pub fn reminder_brief(inbox: &str) -> String {
    format!(
        r#"Your turn ended and `{inbox}/plan.json` does not exist. Nothing has been spawned and nothing is waiting on prose.

Write the plan file now, in the shape your brief gave you. The file is the whole answer: no summary, no explanation, no questions.
"#
    )
}

/// The verifier's turn ended without a verdict. Ask again, briefly.
///
/// The same failure as the lead's: a turn can end with nothing written, and the
/// task then sits in `Verifying` until the idle limit fails the whole run. The
/// reminder names the one file and the one shape, and nothing else — a verifier
/// that has already done the work needs a nudge, not a second brief.
pub fn verifier_reminder_brief(task: &Task, inbox: &str) -> String {
    format!(
        r#"Your turn ended and `{inbox}/` has no verdict for `{id}`. The task is still waiting on you and nothing else is running.

Write one file now:

{{"kind": "verdict", "passed": true, "text": "why", "task": "{id}",
  "command": "the one command that fails on the base and passes on the change",
  "evidence": ["the command you ran → the line that settled it"]}}

`passed` is a boolean — `true` or `false` — and the reasoning goes in `text`. A verdict with no `command` or no `evidence` is discarded and the task is re-queued.
"#,
        id = task.id,
        inbox = inbox,
    )
}

/// A worker's brief.
///
/// The file list is stated as a hard boundary rather than a hint, because it is
/// the only thing standing between two workers and a merge conflict. The
/// acceptance criterion is stated as a test the verifier will run *both ways*,
/// because a worker that knows the test must fail without its change writes a
/// different change.
/// The plan's file ownership, as one worker sees it.
///
/// A worker that breaks a file it does not own is in one of two very different
/// situations and has no way to tell them apart: the file belongs to a task that
/// runs later and will fix it, or nobody owns it and the run is broken. The sixth
/// live run's w-t1 escalated on the first — it deleted `advanceGateField` from
/// `src/logic/gates.js` and correctly noticed that `src/main.js` still imported
/// it — and the run stopped for a question the plan had already answered, because
/// `src/main.js` is t3's file and t3 depends on t1. The map is what makes the
/// difference visible to the worker that has to decide whether to stop.
pub fn ownership_map(tasks: &[Task], me: &str) -> String {
    let mut lines = Vec::new();
    for task in tasks {
        if task.id == me {
            continue;
        }
        let files = if task.files.is_empty() {
            "(no files declared)".to_string()
        } else {
            task.files.join(", ")
        };
        lines.push(format!("- {} — {}: {files}", task.id, task.title));
    }
    if lines.is_empty() {
        "(this is the only task in the plan)".to_string()
    } else {
        lines.join("
")
    }
}

/// What past runs of this workspace learned about the files this task owns.
///
/// The reflection pass is the swarm's only learning mechanism, and until this
/// section existed nothing consumed it: a gotcha was written onto a run's
/// `reflection` and read by nobody. It is matched on the task's own files —
/// [`super::engine::gotchas_for`] does the matching — because that is what a
/// worker can act on. A gotcha about `src/rate.rs` is useful to the worker
/// editing `src/rate.rs` and noise to everyone else.
///
/// Empty when nothing recorded touches this task, and the brief then has no
/// section at all. A heading with nothing under it reads as "there is nothing to
/// know", which is a different claim from "nothing was recorded here".
///
/// Bounded at [`super::history::HISTORY_GOTCHAS`], and the cut is stated. A
/// worker handed forty gotchas skims them, and a brief that is skimmed is a brief
/// that was not read.
pub fn gotcha_section(gotchas: &[Gotcha]) -> String {
    if gotchas.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## What past runs learned about these files\n\
         A previous run here paid for each of these. They are facts, not instructions to\n\
         stop: if one is wrong about this repository, say so in a `decision` and carry on.\n\n",
    );
    for gotcha in gotchas.iter().take(super::history::HISTORY_GOTCHAS) {
        out.push_str(&format!(
            "- `{}` — {} ({})\n",
            gotcha.file, gotcha.text, gotcha.source
        ));
    }
    if gotchas.len() > super::history::HISTORY_GOTCHAS {
        out.push_str(&format!(
            "\n{} more were recorded for these files and are not shown.\n",
            gotchas.len() - super::history::HISTORY_GOTCHAS
        ));
    }
    // A blank line before whatever section follows, so the list does not run into
    // the next heading.
    out.push('\n');
    out
}

pub fn worker_brief(
    task: &Task,
    digest: &str,
    run_id: &str,
    inbox: &str,
    base: &str,
    others: &str,
    skills: &str,
    gotchas: &str,
) -> String {
    let files = if task.files.is_empty() {
        "(none declared)".to_string()
    } else {
        task.files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let decisions = if digest.trim().is_empty() {
        "No decisions from upstream tasks. You are starting from the base commit.".to_string()
    } else {
        format!("These are binding. Do not re-litigate them.\n\n{digest}")
    };

    // A dependency conflict the plan resolved in this task's favour is work, not
    // a starting condition. The tree already has the incoming dependency's
    // version of the file, so a worker that does not know why will read it as
    // finished and leave it.
    let reconcile = if task.reconcile.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Reconcile these before you finish\n{}\n\n\
             Your dependencies disagreed about the files above and the engine kept the \
             incoming dependency's version, because the plan gives the file to you. That \
             version is the starting point, not the answer: making it right is part of \
             this task, and your criterion is what proves it.\n",
            task.reconcile
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let member = task
        .owner
        .clone()
        .unwrap_or_else(|| format!("w-{}", task.id));
    // Scratch lives inside the run directory, which the gate already permits,
    // so a member never has to invent a temp path outside it.
    let scratch = super::run_dir(run_id).join("scratch").join(&member);

    format!(
        r#"You are worker {member} on a swarm run. You implement one task, in your own worktree, and nothing else.

## Your task
{id} — {title}

{detail}

## Files you own
{files}

Do not edit any file outside this list. Another worker owns the rest of the
repository, and a file you both touch is a merge conflict the run has to resolve
from the plan's ownership rather than from your intent — which means your edit to
it can be dropped.
{reconcile}
{gotchas}{skills}## Who owns the rest of the repository
{others}

A file you break that appears above is not your problem: its owner runs after you
and the plan already orders it. Do not stop for it, and do not fix it — say in a
`decision` what you changed and what its owner now has to do, and carry on. A
file you break that appears nowhere above is a gap in the plan, and that is what
an `escalation` is for. Escalating on a file the plan already assigns stops the
whole run for a question it had answered.

## Acceptance criterion
{acceptance}

A verifier will test this. It will not read your reasoning. It will run the
criterion against your change, and against the base commit `{base}` without your
change. If the criterion does not fail without your change, it is not a test and
your work is rejected — so make sure the thing you build is the thing that makes
it pass.

## Decisions from the tasks you depend on
{decisions}

## The board
Write JSON files into `{inbox}/` to speak. One file per entry, any name ending
in `.json`:

{{"kind": "decision", "text": "what you chose and why", "task": "{id}"}}

`kind` is one of `claim`, `decision`, `evidence`, `verdict`, `conflict`,
`escalation`, `note`. Write a `claim` when you start and a `decision` for every
choice a downstream task would otherwise have to guess — a module boundary, a
name, a data shape. Those decisions are the only thing that crosses to the next
worker; your conversation does not.

Read `{inbox}/receipts/` to see what the engine accepted.

## Your scratch directory
`{scratch}` is yours for anything you write that is not the task's work — build
output, probe scripts, logs. The engine removes it when the run ends, so do not
clean it up yourself. The paths that are yours: your worktree, your inbox
`{inbox}/`, and `{scratch}`. Everything else belongs to another member or to
the engine.

## When you are done
Commit your work on your branch. Then write one `evidence` entry naming the
command you ran and the line that proves it passed. Do not claim the task is
complete in prose — the verifier decides that, not you.

## Run id
{run_id}
"#,
        // The owner is written when the task is dispatched, which is after the
        // brief is built on a cold start. The member id is derived from the task
        // id by the same convention the engine uses, so the fallback names the
        // worker rather than calling it "worker".
        member = member,
        scratch = scratch.to_string_lossy(),
        id = task.id,
        title = task.title,
        detail = task.detail.trim(),
        files = files,
        reconcile = reconcile,
        gotchas = gotchas,
        skills = skills,
        others = others,
        acceptance = task.acceptance.trim(),
        decisions = decisions,
        inbox = inbox,
        base = base,
        run_id = run_id,
    )
}

/// The engine's own two-sided run, written out as a result rather than a task.
///
/// Both exit codes and the line each run ended on, because an exit code alone is
/// not something a verifier can read: `exit 1` on the base is a failing assertion
/// or a missing file, and the two say different things about the change. The line
/// is quoted as output, not as a conclusion — the engine cannot tell which line of
/// a test run mattered, and claiming to would be the guess the verifier is being
/// taken off.
///
/// A proof that could not be attempted says so and says why. That is not a pass
/// and the brief must not let it read as one: the verifier still has to name a
/// `command`, and the engine runs that one itself when the verdict arrives.
fn engine_proof_block(proof: &Proof, base: &str, branch: &str) -> String {
    let (Some(base_exit), Some(change_exit)) = (proof.base_exit, proof.change_exit) else {
        return format!(
            "The engine could not run the criterion: {}.\n\n\
             That is not a pass, and it is not yours to make one. Name a `command` in\n\
             your verdict that fails on the base and passes on the change — the engine\n\
             runs that one itself, twice, and records both exit codes.",
            proof
                .note
                .as_deref()
                .unwrap_or("no reason was recorded for it")
        );
    };

    let ended_on = |side: &str, line: &Option<String>| match line {
        Some(line) => format!("The {side} run ended on: `{line}`\n"),
        None => format!("The {side} run printed nothing.\n"),
    };
    let ruling = match (&proof.note, proof.failing_first) {
        (None, true) => {
            "So the criterion is a real test: it fails on the base and passes on the change."
                .to_string()
        }
        (Some(note), _) => format!("{note}."),
        (None, false) => "The engine recorded no reason for that, which is not a pass.".to_string(),
    };

    format!(
        "The engine ran the criterion itself, twice, before you were spawned. This is\n\
         the result, given rather than asked of you:\n\n\
         - on the base commit `{base}`: exit {base_exit}\n\
         - on the change `{branch}`: exit {change_exit}\n\n\
         {}{}{ruling}",
        ended_on("base", &proof.base_line),
        ended_on("change", &proof.change_line),
    )
}

/// A verifier's brief.
///
/// The information asymmetry is the point, and it is structural rather than
/// polite: the verifier is given the diff, the acceptance criterion and the base
/// commit, and is explicitly told it will not be given the worker's reasoning.
/// A verifier that reads the worker's transcript inherits the worker's framing
/// and approves.
///
/// The mechanical proof is the engine's, and the brief says so. It used to ask
/// the verifier to prove the criterion fails on the base and passes on the
/// change — the same command the engine runs itself, twice, recording both exit
/// codes. The cancelled run is what that cost: `v-t1` spent 20.7 minutes and
/// 1,989,387 input tokens re-running a criterion the engine had already run, was
/// respawned once, and ended with `the agent process exited: the agent closed the
/// connection` — no verdict, no board entry, an empty inbox. A turn that long is
/// a turn that runs out of room before it reaches the part that matters, which is
/// the part only the verifier can do.
pub fn verifier_brief(
    task: &Task,
    run_id: &str,
    inbox: &str,
    base: &str,
    branch: &str,
    base_path: &str,
    skills: &str,
    proof: &Proof,
) -> String {
    // The engine read the branch, so this is not a claim the verifier has to
    // take on trust. A grant that nothing checks is a suggestion: the twelfth
    // live run's t1 committed four files outside its three-file grant, the
    // verifier noticed and called it "non-blocking", and the collision surfaced
    // minutes later as a branch that would not merge — in a task that had done
    // nothing wrong.
    let grant = if task.out_of_grant.is_empty() {
        format!(
            "The plan granted this task {}. The engine read the branch and found \
             nothing committed outside that list.",
            task.files.join(", ")
        )
    } else {
        format!(
            "The plan granted this task {}. The engine read the branch and found the \
             worker committed {} outside it.\n\n\
             A grant is a promise to every other task in the plan: this file is yours, \
             nobody else will touch it. A worker that writes outside it breaks that \
             promise for a task that has not run yet, and the breakage surfaces later \
             as a branch that will not merge — in a task that did nothing wrong. So \
             this is a fail, not a note.\n\n\
             The one exception is a change that cannot work without the file: a caller \
             that has to be updated, a constant that has to move. That is not a fail, \
             it is an escalation — put the `escalate` field in the verdict and name the \
             file and why.",
            task.files.join(", "),
            task.out_of_grant.join(", ")
        )
    };
    let scratch_member = task
        .verifier
        .clone()
        .unwrap_or_else(|| format!("v-{}", task.id));
    // Scratch lives inside the run directory, which the gate already permits,
    // so a verifier never invents a temp path outside it.
    let scratch = super::run_dir(run_id).join("scratch").join(&scratch_member);

    format!(
        r#"You are verifier {member} on a swarm run. Your job is to break this change, not to approve it.

## What you are given
- The task: {id} — {title}
- The acceptance criterion: {acceptance}
- The base commit: {base}
- The branch under test: {branch}
- The base checkout: {base_path}

You are deliberately not given the worker's reasoning, its conversation, or its
summary. That is not an oversight. A verifier that reads the worker's framing
inherits its blind spots, and the error that produced the change also produces
the approval.

## The mechanical proof is already done
{proof}

## What you must do
1. Read the diff: `git diff {base}..{branch}`.
2. Look for what the criterion does not cover: an unhandled input, a boundary, a
   case the worker's own test would not catch. Run it. A change that passes the
   criterion and breaks one of those is a fail, and you are the only thing in
   this run that can find it. `{base_path}` is checked out at `{base}` if you
   need to tell a case the change broke from one that was already broken — a case
   that fails on both is not this change's fault.

## What you must not do
Do not edit any file. You are in a detached worktree with no branch to commit
to, and a write here is detected and reported as `base_dirtied`, which fails the
task regardless of what the tests said. The base checkout `{base_path}` is not
yours to edit either: it is reset before the engine proves, so an edit there buys
you nothing, and a dirty base fails the task.

## Files outside the grant
{grant}

{skills}## Your scratch directory
`{scratch}` is yours for anything you write while checking the change — probe
scripts, build output, logs. The engine removes it when the run ends, so do not
clean it up yourself. The paths that are yours: `{scratch}` and your inbox
`{inbox}/`. The worktrees are the engine's; a write to one is detected and
reported.

## The board
Write JSON files into `{inbox}/`. One file per entry, any name ending in `.json`:

{{"kind": "verdict", "passed": true, "text": "why", "task": "{id}",
  "command": "the one command that fails on the base and passes on the change",
  "evidence": ["the command you ran → the line that settled it"]}}

`passed` is a boolean, not a sentence: write `true` or `false` and put the
reasoning in `text`. The engine reads the boolean. The first word of `text` is
only the fallback for a verdict that omits it, and a verdict with neither is read
as a failure — a boolean you have to commit to is harder to write ambiguously
than a sentence the engine reads the first word of.

`evidence` is required and is not prose. One entry per command, each naming the
command and the output line that decided it. A verdict with no evidence is
discarded and the task is re-queued.

`command` is required too, and it is the one field the engine does not take your
word for. It runs that command itself, twice — once in the base checkout and
once in your worktree — and records both exit codes. `failing_first` is true only
if the engine saw it fail on the base and pass on the change. A command that
passes on both sides is reported as not testing the change, and the task is
re-queued.

## When the change is right but the fix is not yours
A change can be correct and still break a file this task does not own: a caller
that has to be updated, a constant that has to move. Failing the task for that
spends an attempt on something the worker cannot fix — it owns {files}, and the
file it needs is not in that list. The worker will try, fail the same way, and
the run will die on the retry ceiling.

Put an `escalate` field in the verdict entry itself. One entry, one ruling:

{{"kind": "verdict", "passed": true, "task": "{id}", "text": "the criterion fails
  on base and passes on the change, but the fix is not this task's to finish",
  "command": "node --test test/step.test.js",
  "evidence": ["..."],
  "escalate": "the change is correct, but src/main.js:112 still calls
  advanceGateField and has to be updated; this task does not own src/main.js"}}

The lead decides. If it agrees it grants the file and the task is re-queued with
it. An escalation is for a correct change with a missing permission, not for a
bug: if the change is wrong, fail it and say why. Do not write a second file —
the `escalate` field is the whole channel, and a verdict that mentions an
escalation without the field is read as a plain ruling.

## Run id
{run_id}
"#,
        member = task.verifier.as_deref().unwrap_or("verifier"),
        scratch = scratch.to_string_lossy(),
        id = task.id,
        title = task.title,
        acceptance = task.acceptance.trim(),
        files = task.files.join(", "),
        grant = grant,
        skills = skills,
        base = base,
        branch = branch,
        base_path = base_path,
        proof = engine_proof_block(proof, base, branch),
        inbox = inbox,
        run_id = run_id,
    )
}

/// The lead's integration brief, sent once every task is verified.
pub fn integration_brief(
    run_id: &str,
    inbox: &str,
    branch: &str,
    suite: &str,
    merged: &[String],
) -> String {
    let order = if merged.is_empty() {
        "(nothing merged yet)".to_string()
    } else {
        merged.join(" → ")
    };
    format!(
        r#"Every task is verified. Now integrate.

## The integration branch
{branch}

## Merge order
{order}

The engine merged in dependency order and stopped at the first conflict. If you
are reading this, either the merges are done or one conflicted.

## Your acceptance criterion
Run `{suite}` on the integration branch. It must pass.

If it fails, every task passed its own verifier and the combination is still
broken — that is the failure this run exists to catch, and it is yours to
attribute. Name the task you blame and the evidence that points at it. Do not
fix it by editing a task's files: write an `escalation` to the board with the
task id and the failing output, and the run pauses for the user.

## The board
Write JSON files into `{inbox}/`. A `conflict` entry names the task and the
files. An `escalation` names the task and the failing line.

## Run id
{run_id}
"#,
        branch = branch,
        order = order,
        suite = suite,
        inbox = inbox,
        run_id = run_id,
    )
}

/// The reflection brief, sent after the run closes.
pub fn reflection_brief(run_id: &str, inbox: &str, board_digest: &str) -> String {
    format!(
        r#"The run is over. Review it and write down what a future run should know.

## What you have
The board, condensed:

{board_digest}

## What to produce
Write one JSON file into `{inbox}/gotchas.json`:

{{"summary": "one paragraph on how the run went",
  "gotchas": [
    {{"file": "src/a.rs", "text": "the thing a future run must know",
      "source": "verdict t2"}}
  ],
  "skills": [
    {{"name": "lowercase-hyphenated",
      "description": "one line: what method this teaches",
      "body": "the method, as instructions a worker can follow",
      "because": "which task had to invent it, and what went wrong without it"}}
  ]}}

## The rule
A gotcha with no source is not written. `source` names the verdict, conflict or
escalation that proves it — `verdict t2`, `conflict t3`, `escalation t1`. If you
cannot point at one, you are guessing, and a guessed gotcha is worse than none
because the next run will trust it.

Write about the repository, not about this run. "src/rate.rs owns the limiter;
src/middleware.rs only calls it" is a gotcha. "The run went well" is not.

## Skills
`skills` is for a *method* this machine does not have. A gotcha is a fact about
this repository; a skill is how to do a kind of work, written so the next run
does not reinvent it. Propose one only when a task had to work something out that
a future task would have to work out again — a build step, a data shape, a way of
testing something that is not obvious from the code.

- `name` becomes a directory, so it must be lowercase and hyphenated.
- `body` is the method itself, as instructions. Not a summary of what happened.
- `because` names the task and the thing that went wrong without it. A proposal
  with no reason is dropped, because nobody can judge it.
- At most three. A run that proposes eight is writing a manual, and a manual
  nobody reviewed is worse than none.

Nothing you propose is installed. It is written into this run's directory and the
user decides.

## Run id
{run_id}
"#,
        board_digest = board_digest,
        inbox = inbox,
        run_id = run_id,
    )
}

/// A condensed view of the board for the reflection pass.
///
/// Verdicts, conflicts and escalations only: those are the entries that carry
/// evidence. A reflection over the whole board would mostly read claims.
///
/// Each line opens with the exact string the gotcha's `source` must contain,
/// in quotes, because the model copies what it reads. The old format printed
/// `- [t1] verdict: passed`, and a live run cited `verdict: t1` back — the
/// colon it had just been shown — and lost every gotcha to the evidence gate.
/// A digest that shows the citation it wants is cheaper than a gate that
/// forgives the citation it gets.
pub fn reflection_digest(entries: &[super::board::BoardEntry]) -> String {
    use super::board::BoardKind;
    let mut out = String::new();
    for entry in entries {
        if !matches!(
            entry.kind,
            BoardKind::Verdict | BoardKind::Conflict | BoardKind::Escalation | BoardKind::Decision
        ) {
            continue;
        }
        let scope = entry.task.as_deref().unwrap_or("run");
        let kind = match entry.kind {
            BoardKind::Verdict => "verdict",
            BoardKind::Conflict => "conflict",
            BoardKind::Escalation => "escalation",
            _ => "decision",
        };
        out.push_str(&format!(
            "- source \"{kind} {scope}\" — {}\n",
            entry.text.trim()
        ));
        for line in &entry.evidence {
            out.push_str(&format!("    {line}\n"));
        }
        if out.len() > 6000 {
            out.push_str("- …truncated\n");
            break;
        }
    }
    out
}

/// The plan a rehearsal uses when the user has not written one.
///
/// A demo plan is not a placeholder: it is what makes the panel demonstrable
/// before a live run, and what the rehearsal tests run against. It is a real
/// three-task plan over a real shape of change.
pub fn example_plan() -> plan::Plan {
    plan::Plan {
        goal: "Add per-key rate limiting to the HTTP layer".into(),
        integration_suite: Some("cargo test".into()),
        notes: "Three disjoint files, one dependency edge. The limiter lands first \
                because the middleware calls it."
            .into(),
        skills_to_write: Vec::new(),
        tasks: vec![
            Task::new(
                "t1",
                "Add the limiter",
                "A token-bucket limiter keyed by client id, in its own module. \
                 Expose `check(key) -> Decision` and a clock seam so the tests do \
                 not sleep.",
                "cargo test rate_limit",
            )
            .with_files(&["src/rate.rs", "src/rate_test.rs"]),
            Task::new(
                "t2",
                "Wire it into the middleware",
                "Call the limiter from the request path and return 429 with a \
                 Retry-After header when it refuses.",
                "cargo test middleware_limit",
            )
            .with_files(&["src/middleware.rs"])
            .with_deps(&["t1"]),
            Task::new(
                "t3",
                "Document the limit",
                "Add the limit, the header and the failure mode to the HTTP docs.",
                "grep -q Retry-After docs/http.md",
            )
            .with_files(&["docs/http.md"]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The proof the engine hands a verifier: the criterion failed on the base
    /// and passed on the change, and both runs said so in their last line.
    fn proven() -> Proof {
        Proof {
            base_exit: Some(1),
            change_exit: Some(0),
            failing_first: true,
            note: None,
            base_line: Some("1 test failed".into()),
            change_line: Some("8 tests passed".into()),
        }
    }

    fn models(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_verifier_gets_a_different_model_when_one_exists() {
        let picked = pick_models(
            &models(&["large", "small"]),
            Some("large"),
            DiversityPolicy::Require,
        )
        .unwrap();
        assert_eq!(picked.worker, "large");
        assert_eq!(picked.verifier, "small");
        assert!(picked.diverse);
    }

    #[test]
    fn the_lead_shares_the_workers_model_on_purpose() {
        let picked = pick_models(
            &models(&["large", "small"]),
            Some("large"),
            DiversityPolicy::Require,
        )
        .unwrap();
        assert_eq!(picked.lead, picked.worker);
    }

    #[test]
    fn one_model_means_no_diversity_and_the_policy_decides_what_that_means() {
        let picked = pick_models(&models(&["only"]), Some("only"), DiversityPolicy::Require)
            .unwrap();
        assert_eq!(picked.verifier, "only");
        assert!(!picked.diverse, "the precondition reports this, not the picker");
    }

    #[test]
    fn allow_never_claims_diversity_even_with_two_models() {
        let picked = pick_models(
            &models(&["large", "small"]),
            Some("large"),
            DiversityPolicy::Allow,
        )
        .unwrap();
        assert_eq!(picked.verifier, "large");
        assert!(!picked.diverse);
    }

    #[test]
    fn an_active_model_that_is_not_configured_falls_back_to_the_first() {
        let picked = pick_models(
            &models(&["large", "small"]),
            Some("retired"),
            DiversityPolicy::Prefer,
        )
        .unwrap();
        assert_eq!(picked.worker, "large");
    }

    #[test]
    fn no_models_means_no_roles() {
        assert!(pick_models(&[], None, DiversityPolicy::Require).is_none());
    }

    /* -------------------------------------------------------------- *
     * Assigning a model and a reasoning level together
     * -------------------------------------------------------------- */

    fn catalog(models: &[(&str, &[&str])]) -> super::super::catalog::Catalog {
        super::super::catalog::Catalog {
            models: models
                .iter()
                .map(|(value, levels)| super::super::catalog::ModelOption {
                    value: (*value).to_string(),
                    name: (*value).to_string(),
                    detail: None,
                    thinking: levels.iter().map(|l| (*l).to_string()).collect(),
                    default: None,
                })
                .collect(),
            source: super::super::catalog::Source::Agent,
        }
    }

    #[test]
    fn a_role_gets_a_level_its_model_accepts() {
        // The fact that shaped the feature: `glm-5-3` takes low/high/max and
        // `mistral-medium-3.5` takes only off/high, so the same default cannot
        // be handed to both.
        let catalog = catalog(&[
            ("glm-5-3", &["low", "high", "max"]),
            ("mistral-medium-3.5", &["off", "high"]),
        ]);
        let assigned = assign(
            &catalog,
            Some("glm-5-3"),
            DiversityPolicy::Require,
            &CrewChoice::default(),
        )
        .unwrap();

        assert_eq!(assigned.worker, "glm-5-3");
        assert_eq!(assigned.verifier, "mistral-medium-3.5");
        // The worker default is `medium`, which glm does not accept, so it
        // settles to glm's own default rather than asking for a level the agent
        // would refuse.
        assert_eq!(assigned.thinking.worker, "high");
        // The verifier default is `high`, which both models accept.
        assert_eq!(assigned.thinking.verifier, "high");
    }

    #[test]
    fn a_level_the_user_chose_is_kept_when_the_model_accepts_it() {
        let catalog = catalog(&[("deepseek", &["off", "low", "medium", "high", "max"])]);
        let assigned = assign(
            &catalog,
            Some("deepseek"),
            DiversityPolicy::Require,
            &CrewChoice {
                thinking: ThinkingByRole {
                    lead: "max".into(),
                    worker: "off".into(),
                    verifier: "low".into(),
                },
                ..CrewChoice::default()
            },
        )
        .unwrap();

        assert_eq!(assigned.thinking.lead, "max");
        assert_eq!(assigned.thinking.worker, "off");
        assert_eq!(assigned.thinking.verifier, "low");
    }

    #[test]
    fn a_level_the_model_refuses_is_repaired_rather_than_requested() {
        let catalog = catalog(&[("mistral-medium-3.5", &["off", "high"])]);
        let assigned = assign(
            &catalog,
            Some("mistral-medium-3.5"),
            DiversityPolicy::Require,
            &CrewChoice {
                thinking: ThinkingByRole {
                    lead: "max".into(),
                    worker: "medium".into(),
                    verifier: "low".into(),
                },
                ..CrewChoice::default()
            },
        )
        .unwrap();

        // None of the three is legal for this model, so all three settle to the
        // model's own default instead of being sent and silently ignored.
        assert_eq!(assigned.thinking.lead, "high");
        assert_eq!(assigned.thinking.worker, "high");
        assert_eq!(assigned.thinking.verifier, "high");
    }

    #[test]
    fn a_catalogue_with_no_levels_asks_for_the_role_default_and_lets_the_agent_decide() {
        // A config-only catalogue carries no levels because the file does not
        // declare them. Unknown is not a constraint, so the role's default is
        // asked for and the session's own check is the guard — which is what
        // `spawn_member` does with `config_offers`.
        let catalog = catalog(&[("deepseek", &[])]);
        let assigned = assign(
            &catalog,
            Some("deepseek"),
            DiversityPolicy::Require,
            &CrewChoice::default(),
        )
        .unwrap();
        assert_eq!(assigned.thinking.lead, "high");
        assert_eq!(assigned.thinking.worker, "medium");
        assert_eq!(assigned.thinking.verifier, "high");
    }

    #[test]
    fn an_explicitly_empty_choice_still_gets_the_role_default() {
        // The panel sends `""` for a role the user did not touch. That is not a
        // request to turn thinking off — it is a request for the default.
        let catalog = catalog(&[("deepseek", &["off", "low", "medium", "high", "max"])]);
        let assigned = assign(
            &catalog,
            Some("deepseek"),
            DiversityPolicy::Require,
            &CrewChoice {
                thinking: ThinkingByRole {
                    lead: String::new(),
                    worker: "off".into(),
                    verifier: String::new(),
                },
                ..CrewChoice::default()
            },
        )
        .unwrap();
        assert_eq!(assigned.thinking.lead, "high");
        assert_eq!(assigned.thinking.worker, "off");
        assert_eq!(assigned.thinking.verifier, "high");
    }

    #[test]
    fn the_verifier_is_never_the_cheapest_role() {
        // A verifier's job is to find the flaw the worker missed. A default that
        // turned its thinking off would be a second opinion with nothing behind
        // it.
        assert_ne!(default_thinking(super::super::Role::Verifier), "off");
        assert_ne!(default_thinking(super::super::Role::Verifier), "low");
        assert_eq!(default_thinking(super::super::Role::Verifier), "high");
    }

    #[test]
    fn the_reflector_inherits_because_it_has_nothing_to_reason_about() {
        assert_eq!(default_thinking(super::super::Role::Reflector), "");
        let by_role = ThinkingByRole {
            lead: "high".into(),
            worker: "low".into(),
            verifier: "max".into(),
        };
        assert_eq!(by_role.for_role(super::super::Role::Reflector), "");
        assert_eq!(by_role.for_role(super::super::Role::Lead), "high");
        assert!(!by_role.is_inherited());
        assert!(ThinkingByRole::default().is_inherited());
    }

    #[test]
    fn a_model_the_user_chose_for_a_role_is_the_model_that_role_gets() {
        let catalog = catalog(&[
            ("large", &["high"]),
            ("small", &["low"]),
            ("third", &["high"]),
        ]);
        let assigned = assign(
            &catalog,
            Some("large"),
            DiversityPolicy::Require,
            &CrewChoice {
                lead: "third".into(),
                worker: "small".into(),
                verifier: "large".into(),
                ..CrewChoice::default()
            },
        )
        .unwrap();

        assert_eq!(assigned.lead, "third");
        assert_eq!(assigned.worker, "small");
        assert_eq!(assigned.verifier, "large");
        assert!(assigned.diverse);
    }

    #[test]
    fn choosing_the_workers_model_for_the_verifier_drops_the_diversity_claim() {
        // The one lie this field exists to prevent: a run that says its verifier
        // is independent while it is on the worker's model.
        let catalog = catalog(&[("large", &["high"]), ("small", &["low"])]);
        let assigned = assign(
            &catalog,
            Some("large"),
            DiversityPolicy::Require,
            &CrewChoice {
                worker: "small".into(),
                verifier: "small".into(),
                ..CrewChoice::default()
            },
        )
        .unwrap();

        assert_eq!(assigned.worker, "small");
        assert_eq!(assigned.verifier, "small");
        assert!(!assigned.diverse, "same model is not an independent verifier");
    }

    #[test]
    fn a_model_the_catalogue_does_not_list_is_ignored_rather_than_passed_through() {
        // The agent would refuse it and the member would run on the session
        // default, so the roster would disagree with the plan. The picker's
        // answer stands instead.
        let catalog = catalog(&[("large", &["high"]), ("small", &["low"])]);
        let assigned = assign(
            &catalog,
            Some("large"),
            DiversityPolicy::Require,
            &CrewChoice {
                worker: "retired-model".into(),
                ..CrewChoice::default()
            },
        )
        .unwrap();

        assert_eq!(assigned.worker, "large", "the active model, not the ghost");
    }

    #[test]
    fn a_lead_override_does_not_drag_the_worker_with_it() {
        let catalog = catalog(&[("large", &["high"]), ("small", &["low"])]);
        let assigned = assign(
            &catalog,
            Some("large"),
            DiversityPolicy::Require,
            &CrewChoice {
                lead: "small".into(),
                ..CrewChoice::default()
            },
        )
        .unwrap();

        assert_eq!(assigned.lead, "small");
        assert_eq!(assigned.worker, "large");
    }

    #[test]
    fn an_empty_choice_is_the_pickers_own_answer() {
        let catalog = catalog(&[("large", &["high"]), ("small", &["low"])]);
        let picked = pick_models(
            &catalog.aliases(),
            Some("large"),
            DiversityPolicy::Require,
        )
        .unwrap();
        let assigned = assign(
            &catalog,
            Some("large"),
            DiversityPolicy::Require,
            &CrewChoice::default(),
        )
        .unwrap();
        assert_eq!(assigned.lead, picked.lead);
        assert_eq!(assigned.worker, picked.worker);
        assert_eq!(assigned.verifier, picked.verifier);
        assert!(CrewChoice::default().is_empty());
    }

    #[test]
    fn assigning_with_no_models_is_no_assignment() {
        assert!(assign(
            &super::super::catalog::Catalog::default(),
            None,
            DiversityPolicy::Require,
            &CrewChoice::default(),
        )
        .is_none());
    }

    #[test]
    fn a_policy_round_trips_through_its_key() {
        for policy in [
            DiversityPolicy::Require,
            DiversityPolicy::Prefer,
            DiversityPolicy::Allow,
        ] {
            assert_eq!(DiversityPolicy::from_key(policy.key()), Some(policy));
        }
        assert_eq!(DiversityPolicy::from_key("nonsense"), None);
    }

    #[test]
    fn a_worker_brief_names_its_files_as_a_boundary() {
        let mut task = Task::new("t1", "Add the limiter", "Do it.", "cargo test passes")
            .with_files(&["src/rate.rs"]);
        task.owner = Some("w1".into());
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc123", "(none)", "", "");
        assert!(brief.contains("src/rate.rs"));
        assert!(brief.contains("Do not edit any file outside this list"));
        assert!(brief.contains("cargo test passes"));
        assert!(brief.contains("abc123"), "the base commit is named");
        assert!(brief.contains("escalation"), "the escape hatch is stated");
    }

    #[test]
    fn a_retry_brief_names_the_inbox_the_worker_has_to_write_to() {
        // The retry brief was the one brief that did not name an inbox. It said
        // "post one `evidence` entry to your inbox" and never said where, so a
        // retried worker did the work and had nowhere to report it. The run then
        // sat with the task `running` and the member `working` until the idle
        // limit failed it — the shape the end-to-end harness caught, and the same
        // shape that made a live run look broken while every helper test passed.
        let task = Task::new("t1", "Gates", "Do it.", "node --test test/gates.test.js");
        let brief = retry_brief(&task, "the change never touched src/main.js", "/tmp/inbox");
        assert!(brief.contains("/tmp/inbox"), "it names where to write it");
        assert!(
            brief.contains("the change never touched src/main.js"),
            "it carries the refusal"
        );
        assert!(brief.contains("node --test test/gates.test.js"));
        assert!(
            brief.contains("receipts"),
            "a rejected entry is not a posted entry, and the worker has to be able to see that"
        );
    }

    #[test]
    fn a_worker_reminder_names_the_one_thing_that_is_missing() {
        // The eighth live run's w-t1 finished its turn, posted nothing, and the
        // run died ten minutes later. The reminder exists so that silence has an
        // answer that is not the idle limit.
        let task = Task::new("t1", "Gates", "Do it.", "node --test test/gates.test.js");
        let brief = worker_reminder_brief(&task, "/tmp/inbox");
        assert!(brief.contains("evidence"), "it names the missing entry");
        assert!(brief.contains("/tmp/inbox"), "it names where to write it");
        assert!(brief.contains("node --test test/gates.test.js"));
        assert!(brief.contains("escalation"), "silence is not the only answer");
        assert!(
            !brief.contains("Do not edit any file"),
            "the whole brief is not re-sent"
        );
    }

    #[test]
    fn a_worker_brief_names_who_owns_the_files_it_does_not() {
        // The sixth live run's w-t1 deleted an export that src/main.js imported,
        // correctly noticed the break, and escalated — stopping the run for a
        // question the plan had already answered, because src/main.js is t3's
        // file and t3 depends on t1. The map is what makes that visible.
        let mut task = Task::new("t1", "Gates", "Do it.", "node --test passes");
        task.files = vec!["src/logic/gates.js".into()];
        let others = ownership_map(
            &[
                task.clone(),
                Task::new("t3", "Frame loop", "Do it.", "node --test passes"),
            ],
            "t1",
        );
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc", &others, "", "");
        assert!(brief.contains("Who owns the rest of the repository"));
        assert!(brief.contains("t3 — Frame loop"), "the other task is named");
        assert!(
            !others.contains("t1"),
            "a worker is not told about its own files twice"
        );
        assert!(
            brief.contains("is not your problem"),
            "the brief says a file another task owns is not an escalation"
        );
    }

    #[test]
    fn the_ownership_map_lists_every_other_task_and_its_files() {
        let mut a = Task::new("t1", "One", "Do it.", "x");
        a.files = vec!["src/a.js".into()];
        let mut b = Task::new("t2", "Two", "Do it.", "x");
        b.files = vec!["src/b.js".into(), "src/c.js".into()];
        let map = ownership_map(&[a, b], "t1");
        assert!(map.contains("t2 — Two: src/b.js, src/c.js"));
        assert!(!map.contains("t1"));
    }

    #[test]
    fn a_single_task_plan_says_so_rather_than_printing_nothing() {
        let task = Task::new("t1", "One", "Do it.", "x");
        let map = ownership_map(&[task], "t1");
        assert!(map.contains("only task"), "{map}");
    }

    #[test]
    fn a_worker_brief_carries_the_digest_and_says_it_is_binding() {
        let task = Task::new("t2", "Wire it", "Do it.", "cargo test passes");
        let brief = worker_brief(
            &task,
            "- [t1] used a token bucket\n",
            "run-1",
            "/tmp/inbox",
            "abc",
            "(none)",
            "",
            "",
        );
        assert!(brief.contains("token bucket"));
        assert!(brief.contains("binding"));
    }

    #[test]
    fn a_worker_brief_with_no_digest_says_so_rather_than_printing_a_heading() {
        let task = Task::new("t1", "One", "Do it.", "cargo test passes");
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc", "(none)", "", "");
        assert!(brief.contains("No decisions from upstream tasks"));
        assert!(!brief.contains("binding"));
    }

    #[test]
    fn a_verifier_brief_states_the_information_asymmetry() {
        let mut task = Task::new("t1", "Add the limiter", "Do it.", "cargo test passes");
        task.verifier = Some("v1".into());
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc123",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(brief.contains("deliberately not given"));
        assert!(brief.contains("fails"), "failing-first is stated");
        assert!(brief.contains("base_dirtied"), "the write rule is stated");
        assert!(brief.contains("evidence"), "evidence is required");
        assert!(brief.contains("swarm/run-1/t1"));
    }

    /// The brief names the scratch directory inside the run directory and says
    /// the engine removes it. Run 3's verifiers invented `C:\tmp\vt7`-style
    /// paths because the brief named none, and nothing ever cleaned them up.
    #[test]
    fn a_verifier_brief_names_its_scratch_directory_and_says_the_engine_removes_it() {
        let mut task = Task::new("t1", "One", "Do it.", "cargo test passes");
        task.verifier = Some("v-t1".into());
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc123",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        let scratch = super::super::run_dir("run-1").join("scratch").join("v-t1");
        assert!(
            brief.contains(&*scratch.to_string_lossy()),
            "the scratch path is named: {brief}"
        );
        assert!(
            brief.contains("The engine removes it"),
            "the cleanup rule is stated: {brief}"
        );
        assert!(
            !brief.contains("C:\\tmp"),
            "no invented temp path is named: {brief}"
        );
    }

    /// The worker brief names the same scratch directory and says which paths
    /// are the member's — run 3 lost nine denials to members touching worktrees
    /// that were not theirs.
    #[test]
    fn a_worker_brief_names_its_scratch_directory_and_which_paths_are_its() {
        let task = Task::new("t1", "One", "Do it.", "cargo test passes");
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc", "(none)", "", "");
        let scratch = super::super::run_dir("run-1").join("scratch").join("w-t1");
        assert!(
            brief.contains(&*scratch.to_string_lossy()),
            "the scratch path is named: {brief}"
        );
        assert!(
            brief.contains("The engine removes it"),
            "the cleanup rule is stated: {brief}"
        );
        assert!(
            brief.contains("The paths that are yours"),
            "the member is told which paths are its: {brief}"
        );
    }

    #[test]
    fn a_verifier_brief_names_the_base_checkout_it_may_use() {
        // The brief used to say "check out the base in a scratch worktree" and
        // the gate refused every path outside the verifier's own tree, so the
        // step it was told to perform was the step it was not allowed to
        // perform. The fifth live run failed a passing change on exactly that.
        let task = Task::new("t1", "One", "Do it.", "cargo test passes");
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc123",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(brief.contains("/w/run-1/t1-base"), "the path is named");
        assert!(
            brief.contains("`/w/run-1/t1-base` is checked out at `abc123`"),
            "the brief says the checkout exists and what it is at"
        );
        assert!(
            !brief.contains("scratch worktree"),
            "a verifier told to make its own worktree is a verifier the gate refuses"
        );
    }

    /// The verifier's turn is for what the criterion does not cover.
    ///
    /// The brief used to ask it to prove the criterion fails on the base and
    /// passes on the change — the same command the engine runs itself, twice,
    /// recording both exit codes. The cancelled run is what that cost: `v-t1`
    /// spent 20.7 minutes and 1,989,387 input tokens re-running a criterion the
    /// engine had already run, and ended with no verdict at all. The engine's
    /// result is stated as given, and the two proof steps are gone.
    #[test]
    fn a_verifier_brief_states_the_engine_proof_instead_of_asking_for_it() {
        let task = Task::new("t1", "One", "Do it.", "node --test test/gates.test.js");
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc123",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );

        assert!(
            brief.contains("The mechanical proof is already done"),
            "the section has to be there: {brief}"
        );
        assert!(
            brief.contains("on the base commit `abc123`: exit 1"),
            "the base exit code is stated as given: {brief}"
        );
        assert!(
            brief.contains("on the change `swarm/run-1/t1`: exit 0"),
            "the change exit code is stated as given: {brief}"
        );
        assert!(
            brief.contains("The base run ended on: `1 test failed`"),
            "the line that decided the base run is quoted: {brief}"
        );
        assert!(
            brief.contains("The change run ended on: `8 tests passed`"),
            "the line that decided the change run is quoted: {brief}"
        );
        assert!(
            !brief.contains("Prove the criterion **fails**"),
            "the verifier is not asked to re-prove what the engine proved: {brief}"
        );
        assert!(
            !brief.contains("Run the criterion against"),
            "and not to run it against the branch either: {brief}"
        );
        assert!(
            brief.contains("Look for what the criterion does not cover"),
            "the part only the verifier can do is still asked for: {brief}"
        );
    }

    /// A proof the engine could not attempt is not a pass, and the brief must not
    /// let it read as one. The verifier still has to name a command, and the
    /// engine runs that one itself when the verdict arrives.
    #[test]
    fn a_verifier_brief_says_so_when_the_engine_could_not_run_the_criterion() {
        let task = Task::new("t1", "One", "Do it.", "the tests pass");
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc123",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &Proof::unattempted("the criterion is not one shell command — it contains `, `"),
        );

        assert!(
            brief.contains("could not run the criterion"),
            "the failure is stated rather than hidden: {brief}"
        );
        assert!(
            brief.contains("it contains `, `"),
            "and the reason is the engine's, not a guess: {brief}"
        );
        assert!(
            brief.contains("That is not a pass"),
            "an unattempted proof must not read as an approval: {brief}"
        );
        assert!(
            brief.contains("Name a `command`"),
            "the verifier still owes the engine a command: {brief}"
        );
    }

    /// The verdict carries a boolean the verifier has to commit to.
    ///
    /// A sentence the engine reads the first word of is easy to write
    /// ambiguously — the first live run marked a task verified on a verdict that
    /// opened with "FAILED". The brief asks for `passed`, and says what happens
    /// to a verdict that omits it.
    #[test]
    fn a_verifier_brief_asks_for_a_boolean_ruling() {
        let task = Task::new("t1", "One", "Do it.", "cargo test");
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(
            brief.contains("\"passed\": true"),
            "the shape carries the field: {brief}"
        );
        assert!(
            brief.contains("`passed` is a boolean, not a sentence"),
            "and says it is a boolean rather than prose: {brief}"
        );
        assert!(
            brief.contains("only the fallback for a verdict that omits it"),
            "the first-word parse is named as the fallback: {brief}"
        );
        assert!(
            brief.contains("a verdict with neither is read\nas a failure"),
            "a malformed verdict is a failure, not a pass: {brief}"
        );
    }

    #[test]
    fn a_verifier_brief_forbids_editing() {
        let task = Task::new("t1", "One", "Do it.", "cargo test passes");
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(brief.contains("Do not edit any file"));
    }

    /// A verifier that can only pass or fail will fail a correct change for a
    /// missing permission, and the worker will fail the same way three times.
    /// The sixth live run's v-t1 refused t1 because `src/main.js:112` still
    /// called the function t1 removed — and `src/main.js` is t3's file, so t1
    /// could not fix it. The brief has to offer the third answer.
    #[test]
    fn a_verifier_brief_offers_escalation_when_the_fix_is_outside_the_task() {
        let task = Task::new("t1", "One", "Do it.", "cargo test")
            .with_files(&["src/logic/gates.js"]);
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc",
            "swarm/run-1/t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(
            brief.contains("escalation"),
            "the verifier has to know it can escalate"
        );
        assert!(
            brief.contains("\"escalate\""),
            "the escalation is a field on the verdict, not a second file"
        );
        assert!(
            brief.contains("src/logic/gates.js"),
            "the brief has to name the files the task does own"
        );
        assert!(
            brief.contains("missing permission"),
            "an escalation is not a way to avoid failing a bug"
        );
    }

    #[test]
    fn an_amendment_brief_gives_the_lead_the_lever_that_resolves_a_conflict() {
        // The lead cannot change `deps` and cannot touch a verified task, so
        // `files` is the only lever it has. A brief that does not say so leaves
        // the lead with a question it cannot answer, and the run stops for the
        // user on something the plan could have fixed.
        let brief = amendment_brief(
            "t3 could not start: t2 conflicts on src/player.js, and no task owns it",
            "/tmp/inbox",
            "- t1 — gates: src/logic/gates.js",
        );
        assert!(
            brief.contains("Grant the file to the task that has to reconcile it"),
            "the lever has to be named: {brief}"
        );
        assert!(
            brief.contains("incoming dependency's version"),
            "and so does what the task will start from"
        );
        assert!(
            !brief.contains("A worker stopped the run"),
            "the engine raises this question too, so the opening cannot assume a worker"
        );
    }

    #[test]
    fn a_worker_brief_names_the_files_it_has_to_reconcile() {
        // The engine resolved a dependency conflict in this task's favour, so the
        // tree already has the incoming dependency's version. A worker that is
        // not told why reads that file as finished and leaves it.
        let mut task = Task::new("t3", "Three", "Do it.", "node --test test/step.test.js")
            .with_files(&["src/player.js", "src/logic/step.js"]);
        task.reconcile = vec!["src/player.js".into()];
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc", "", "", "");
        assert!(
            brief.contains("Reconcile these before you finish"),
            "the section has to be there: {brief}"
        );
        assert!(brief.contains("- src/player.js"));
        assert!(
            brief.contains("the starting point, not the answer"),
            "the worker has to know the file is not finished"
        );
    }

    #[test]
    fn a_worker_brief_with_nothing_to_reconcile_has_no_section() {
        let task = Task::new("t1", "One", "Do it.", "cargo test").with_files(&["src/a.js"]);
        let brief = worker_brief(&task, "", "run-1", "/tmp/inbox", "abc", "", "", "");
        assert!(!brief.contains("Reconcile these before you finish"));
    }

    #[test]
    fn a_verifier_brief_names_the_files_the_worker_committed_outside_its_grant() {
        // The engine read the branch, so the verifier is not asked to trust a
        // claim. The twelfth live run's verifier noticed the out-of-grant files
        // and called them "non-blocking"; the collision then killed the run in a
        // task that had done nothing wrong.
        let mut task = Task::new("t1", "One", "Do it.", "node --test test/gates.test.js")
            .with_files(&["src/logic/gates.js"]);
        task.out_of_grant = vec!["src/main.js".into(), "src/player.js".into()];
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc",
            "swarm/run-1-t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(
            brief.contains("src/main.js, src/player.js"),
            "the brief has to name the files: {brief}"
        );
        assert!(
            brief.contains("this is a fail, not a note"),
            "an out-of-grant commit is a ruling, not an observation"
        );
        assert!(
            brief.contains("it is an escalation"),
            "a change that cannot work without the file is the lead's question"
        );
    }

    #[test]
    fn a_verifier_brief_says_so_when_the_worker_stayed_inside_its_grant() {
        let task = Task::new("t1", "One", "Do it.", "node --test test/gates.test.js")
            .with_files(&["src/logic/gates.js"]);
        let brief = verifier_brief(
            &task,
            "run-1",
            "/tmp/inbox",
            "abc",
            "swarm/run-1-t1",
            "/w/run-1/t1-base",
            "",
            &proven(),
        );
        assert!(
            brief.contains("nothing committed outside that list"),
            "the check happened, and the verifier is told it happened: {brief}"
        );
        assert!(!brief.contains("this is a fail, not a note"));
    }

    /// Three consecutive live runs under-granted a task that moved behaviour:
    /// the new module was written, the old copy stayed, and the verifier
    /// escalated because the missing edit was outside the grant. The rule has to
    /// be in the brief, because the engine cannot tell an extraction from a
    /// rewrite.
    #[test]
    fn the_lead_brief_says_a_task_that_moves_behaviour_owns_both_ends() {
        let brief = lead_brief("ship it", "/tmp/ws", "run-1", "/tmp/inbox", "", "");
        assert!(
            brief.contains("owns both ends of the move"),
            "the extraction rule has to be in the plan brief: {brief}"
        );
        assert!(
            brief.contains("The **work** must be completable"),
            "and it has to be about the work, not only the criterion"
        );
    }

    #[test]
    fn the_lead_brief_requires_an_integration_suite_and_two_tasks() {        let brief = lead_brief("ship it", "/tmp/ws", "run-1", "/tmp/inbox", "", "");
        assert!(brief.contains("integrationSuite"));
        assert!(brief.contains("At least two tasks"));
        assert!(brief.contains("plan.json"));
        assert!(brief.contains("INTEGRATED"));
    }

    #[test]
    fn the_integration_brief_names_the_merge_order_and_the_suite() {
        let brief = integration_brief(
            "run-1",
            "/tmp/inbox",
            "swarm/run-1",
            "cargo test",
            &["t1".into(), "t2".into()],
        );
        assert!(brief.contains("t1 → t2"));
        assert!(brief.contains("cargo test"));
        assert!(brief.contains("attribute"));
    }

    #[test]
    fn the_reflection_brief_refuses_a_gotcha_with_no_source() {
        let brief = reflection_brief("run-1", "/tmp/inbox", "- [t1] verdict: passed\n");
        assert!(brief.contains("A gotcha with no source is not written"));
        assert!(brief.contains("gotchas.json"));
    }

    #[test]
    fn the_reflection_digest_keeps_evidence_and_drops_claims() {
        use super::super::board::{BoardEntry, BoardKind};
        let entries = vec![
            BoardEntry::new("w1", BoardKind::Claim, "taking t1").about("t1"),
            BoardEntry::new("v1", BoardKind::Verdict, "passed").about("t1"),
            BoardEntry::new("w2", BoardKind::Note, "a note").about("t2"),
        ];
        let digest = reflection_digest(&entries);
        assert!(digest.contains("verdict"));
        assert!(!digest.contains("taking t1"));
        assert!(!digest.contains("a note"));
    }

    /// The digest has to print the citation the gate will accept.
    ///
    /// It used to print `- [t1] verdict: passed`, and a live run cited
    /// `verdict: t1` back — the colon it had just been shown — and lost all six
    /// of its gotchas. The digest is the only place the model learns the
    /// syntax, so it has to show the exact string, not a rendering of it.
    #[test]
    fn the_reflection_digest_prints_the_citation_it_wants_back() {
        use super::super::board::{BoardEntry, BoardKind};
        let entries = vec![
            BoardEntry::new("v1", BoardKind::Verdict, "passed").about("t2"),
            BoardEntry::new("e1", BoardKind::Escalation, "the criterion is weak").about("run"),
        ];
        let digest = reflection_digest(&entries);
        assert!(
            digest.contains("source \"verdict t2\""),
            "the digest must show the exact source string: {digest}"
        );
        assert!(
            digest.contains("source \"escalation run\""),
            "a run-scoped entry must cite `run`: {digest}"
        );
        assert!(
            !digest.contains("verdict:"),
            "a colon in the digest is what the model copies: {digest}"
        );
    }

    #[test]
    fn the_example_plan_is_valid() {
        let example = example_plan();
        assert_eq!(plan::validate(&example), Ok(()));
        assert_eq!(example.tasks.len(), 3);
    }

    #[test]
    fn the_example_plan_has_a_real_dependency_edge() {
        let example = example_plan();
        let shape = plan::shape(&example);
        assert_eq!(shape.critical_path, vec!["t1", "t2"]);
        assert_eq!(shape.max_parallel, 2);
    }

    /// The one rule a lead cannot infer from a refusal message.
    ///
    /// A criterion that reads perfectly well can still pass on the base commit,
    /// and the gate refuses it before anything spawns. The revision brief is the
    /// only place the lead is told what that means, so it has to say it.
    #[test]
    fn the_revision_brief_says_a_criterion_has_to_fail_on_the_base() {
        let brief = revision_brief("t2's criterion passes on the base", "/tmp/inbox");
        assert!(brief.contains("must FAIL on the commit the run starts from"));
        assert!(
            brief.contains("cannot tell the change from"),
            "the brief has to say why, not just that"
        );
    }
}
