//! The plan, and every reason a plan is refused.
//!
//! The strongest argument against a swarm is that most coding tasks do not
//! decompose. So the gate is not a warning the user can click past — it is a
//! validation pass that returns named violations, and a plan with any violation
//! does not start a run.
//!
//! Everything here is pure. No filesystem, no processes, no clock. That is what
//! makes the gate testable, and the gate is the part that decides whether the
//! feature is worth having.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::Task;

/// What the lead submits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// One sentence: what the run produces.
    #[serde(default)]
    pub goal: String,
    pub tasks: Vec<Task>,
    /// The command that proves the *integrated* result, not one task.
    ///
    /// This is the lead's own acceptance criterion, and it is separate from the
    /// per-task ones on purpose: every task can pass its verifier and the
    /// combination still be broken, and that is exactly the failure a swarm is
    /// most likely to produce.
    #[serde(default)]
    pub integration_suite: Option<String>,
    /// Anything the lead wants the user to see before approving.
    #[serde(default)]
    pub notes: String,
    /// Skills the lead writes for this run, because the machine does not have
    /// the method the work needs.
    ///
    /// The lead is the only member that can see the whole plan, so it is the
    /// only one that can know a method is missing before the work starts. A
    /// worker that discovers the gap mid-task can only report it, and by then
    /// the task has already been attempted without it.
    ///
    /// These are written into the run's own directory and handed to this run's
    /// members. They are not installed: keeping one is a click, through the same
    /// proposal path the reflection pass uses. A run that could write to
    /// `~/.vibe/skills` could change how every future run behaves.
    #[serde(default)]
    pub skills_to_write: Vec<super::reflect::SkillDraft>,
}

/// How many skills one plan may write.
///
/// Two, because a lead that writes five has stopped planning and started
/// authoring, and the tasks are what the run is for. The cap is also what keeps
/// the gate cheap: every draft is validated before a single file is written.
pub const MAX_PLAN_SKILLS: usize = 2;

/// One reason a plan cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Violation {
    /// A swarm of one task is a slower chat with a roster.
    TooFewTasks { count: usize },
    /// A task with no observable criterion cannot be verified, only approved.
    MissingAcceptance { task: String },
    /// The criterion is a sentence, not a command.
    ///
    /// The engine runs the criterion verbatim with `bash -c`; the verifier reads
    /// it as English. When the field holds prose — "`node --test a b c` passes,
    /// where the new tests assert ..." — the two disagree about the same string,
    /// and the run spends every attempt discovering it. The sixth live run's t1
    /// was refused three times for exactly that: the engine's proof said the
    /// criterion failed on the change while the verifier said it passed.
    CriterionIsProse { task: String, marker: String },
    /// A task that declares no files has no isolation boundary to enforce.
    EmptyFileSet { task: String },
    UnknownDep { task: String, dep: String },
    SelfDep { task: String },
    DuplicateId { task: String },
    /// The dependency graph has no valid order.
    Cycle { tasks: Vec<String> },
    /// Two tasks that are not ordered by the DAG claim the same file, so their
    /// worktrees would produce a merge that cannot be resolved mechanically.
    OverlappingFiles {
        a: String,
        b: String,
        files: Vec<String>,
    },
    /// Without this, "the integrated result works" has no test.
    NoIntegrationSuite,
    /// A skill the plan writes is not a skill a directory can hold.
    ///
    /// The name becomes a directory under the run's own `skills/`, so a name with
    /// a separator in it would write outside that directory, and a name with a
    /// space would produce a directory the member cannot name in a brief.
    BadSkillDraft { name: String, why: String },
    /// The plan writes a skill this machine already has.
    ///
    /// Shadowing an installed skill would make the run's brief describe a method
    /// the user did not write and cannot see, under the name of one they did.
    SkillAlreadyInstalled { name: String },
    /// A task's acceptance criterion reaches into files another task owns, so no
    /// amount of correct work by this task can make it pass.
    ///
    /// The eighth live run's t2 was told to make `advanceGateField` appear
    /// nowhere in `src/` or `test/` while owning only `src/logic/gates.js`.
    /// `src/main.js` still imported it and `src/main.js` is t3's file, so the
    /// criterion was unsatisfiable by construction. The worker said so in as many
    /// words — "those two instructions were unsatisfiable together" — and then
    /// spent two attempts and two verifier turns proving it.
    CriterionOutsideOwnership {
        task: String,
        path: String,
        owner: String,
    },
    /// The criterion already passes on the commit the run starts from, so it
    /// cannot tell the change from the base.
    ///
    /// The engine proves every verdict against two trees and refuses a pass whose
    /// base side also passed. That check is correct and it is far too late: the
    /// thirteenth live run spent three worker attempts and two verifier turns on
    /// `node --test test/gates.test.js`, which passed on the base commit, and by
    /// the time the engine saw it the lead had spent both amendments and the
    /// question fell to the user. The same fact is knowable before a single
    /// worker spawns — the base tree exists, the criterion is a command, and one
    /// run of it settles the question — so it is a plan violation, not an
    /// escalation. A plan violation goes back to the lead on `plan_attempts`,
    /// which is a budget the criterion question never touches.
    CriterionPassesOnBase { task: String, command: String },
    /// A task names a skill this machine does not have.
    ///
    /// The same discipline as a model alias: a name the catalogue does not hold
    /// is refused rather than dropped, because a brief that tells a worker to
    /// load a skill that does not exist spends the worker's first turn finding
    /// that out, and a plan that silently lost the assignment looks like a plan
    /// that never made it.
    UnknownSkill { task: String, skill: String },
}

impl Violation {
    /// The one line the user reads. Written as an instruction, not a code.
    pub fn message(&self) -> String {
        match self {
            Violation::TooFewTasks { count } => format!(
                "{count} task{} — this is one task, so run it as a chat",
                if *count == 1 { "" } else { "s" }
            ),
            Violation::MissingAcceptance { task } => format!(
                "{task} has no acceptance criterion — a verifier cannot test \"it works\""
            ),
            Violation::CriterionIsProse { task, marker } => format!(
                "{task}'s criterion is a sentence, not a command — it contains {marker}, \
                 and the engine runs the field verbatim with bash. Put the command in \
                 `acceptance` and the explanation in `detail`"
            ),
            Violation::EmptyFileSet { task } => {
                format!("{task} declares no files — there is nothing to isolate")
            }
            Violation::UnknownDep { task, dep } => {
                format!("{task} depends on {dep}, which is not in the plan")
            }
            Violation::SelfDep { task } => format!("{task} depends on itself"),
            Violation::DuplicateId { task } => format!("{task} appears twice"),
            Violation::Cycle { tasks } => format!(
                "{} form a dependency cycle — no task can start",
                tasks.join(" → ")
            ),
            Violation::OverlappingFiles { a, b, files } => format!(
                "{a} and {b} both claim {} and neither depends on the other — \
                 their worktrees would conflict",
                files.join(", ")
            ),
            Violation::CriterionOutsideOwnership { task, path, owner } => format!(
                "{task}'s criterion reaches into {path}, which {owner} owns — {task} cannot make it pass on its own"
            ),
            Violation::CriterionPassesOnBase { task, command } => format!(
                "{task}'s criterion `{command}` already passes on the commit the run starts \
                 from, so it cannot tell the change from the base — rewrite it to fail \
                 before the work and pass after it"
            ),
            Violation::NoIntegrationSuite => {
                "no integration suite — every task can pass and the result still be broken"
                    .to_string()
            }
            Violation::UnknownSkill { task, skill } => format!(
                "{task} names the skill `{skill}`, which is not installed — name one from \
                 the catalogue in your brief, or leave `skills` empty and the engine will \
                 rank one for the task"
            ),
            Violation::BadSkillDraft { name, why } => format!(
                "the plan writes a skill called `{name}`, which cannot be one: {why}"
            ),
            Violation::SkillAlreadyInstalled { name } => format!(
                "the plan writes a skill called `{name}`, which this machine already has"
            ),
        }
    }

    /// A stable key, for the panel to group by.
    pub fn key(&self) -> &'static str {
        match self {
            Violation::TooFewTasks { .. } => "too-few-tasks",
            Violation::MissingAcceptance { .. } => "missing-acceptance",
            Violation::CriterionIsProse { .. } => "criterion-is-prose",
            Violation::EmptyFileSet { .. } => "empty-file-set",
            Violation::UnknownDep { .. } => "unknown-dep",
            Violation::SelfDep { .. } => "self-dep",
            Violation::DuplicateId { .. } => "duplicate-id",
            Violation::Cycle { .. } => "cycle",
            Violation::OverlappingFiles { .. } => "overlapping-files",
            Violation::NoIntegrationSuite => "no-integration-suite",
            Violation::CriterionOutsideOwnership { .. } => "criterion-outside-ownership",
            Violation::CriterionPassesOnBase { .. } => "criterion-passes-on-base",
            Violation::UnknownSkill { .. } => "unknown-skill",
            Violation::BadSkillDraft { .. } => "bad-skill-draft",
            Violation::SkillAlreadyInstalled { .. } => "skill-already-installed",
        }
    }
}

/// The first unquoted prose marker in a criterion, if it has one.
///
/// A shell command does not contain an unquoted `, ` or `: `. Both are argument
/// separators in prose and neither survives quoting in a real command, so
/// stripping quoted regions first is what separates `awk '{print $1, $2}'` (a
/// command) from "passes, where the tests assert: x" (a sentence).
///
/// This is a structural rule, not a guess at intent: it says the field is not a
/// command, which is a fact about the string rather than a judgement about the
/// plan. A criterion that is prose without either marker — "the tests pass" —
/// slips through here and is caught by the verifier, which is the right place
/// for it: the engine cannot tell a bad command from a bad idea.
pub fn prose_marker(criterion: &str) -> Option<String> {
    if criterion.contains('\n') {
        return Some("a newline".to_string());
    }
    let mut unquoted = String::with_capacity(criterion.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in criterion.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if ch == '\\' && q == '"' {
                    escaped = true;
                } else if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else {
                    unquoted.push(ch);
                }
            }
        }
    }
    for marker in [", ", ": "] {
        if unquoted.contains(marker) {
            return Some(format!("`{marker}`"));
        }
    }
    None
}

/// Normalise a declared path so two spellings of one file compare equal.
///
/// `./src/a.rs`, `src/a.rs` and `src//a.rs` are the same file, and a plan that
/// declares them differently must not slip past the overlap check.
pub fn normalize_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut last_slash = false;
    for ch in path.trim().chars() {
        if ch == '\\' {
            if !last_slash {
                out.push('/');
                last_slash = true;
            }
            continue;
        }
        if ch == '/' {
            if !last_slash {
                out.push('/');
                last_slash = true;
            }
            continue;
        }
        out.push(ch);
        last_slash = false;
    }
    let trimmed = out.trim_start_matches("./").trim_end_matches('/');
    trimmed.to_string()
}

/// The task ids in a valid execution order, or `None` when there is a cycle.
///
/// Kahn's algorithm with a deterministic tie-break: among the tasks that are
/// ready at the same moment, the one declared first goes first. A scheduler that
/// reorders equal candidates produces a different run every time, which makes
/// both the rehearsal and the tests meaningless.
pub fn topological_order(tasks: &[Task]) -> Option<Vec<String>> {
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let declared: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();

    for task in tasks {
        indegree.entry(task.id.as_str()).or_insert(0);
        for dep in &task.deps {
            // A dep that is not in the plan is a validation error, not a cycle;
            // it is ignored here so the two failures stay distinguishable.
            if !declared.contains(&dep.as_str()) {
                continue;
            }
            *indegree.entry(task.id.as_str()).or_insert(0) += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(task.id.as_str());
        }
    }

    let mut queue: VecDeque<&str> = declared
        .iter()
        .copied()
        .filter(|id| indegree.get(id).copied().unwrap_or(0) == 0)
        .collect();

    let mut order = Vec::with_capacity(tasks.len());
    while let Some(id) = queue.pop_front() {
        order.push(id.to_string());
        if let Some(children) = dependents.get(id) {
            for child in children {
                if let Some(entry) = indegree.get_mut(child) {
                    *entry = entry.saturating_sub(1);
                    if *entry == 0 {
                        queue.push_back(child);
                    }
                }
            }
        }
    }

    if order.len() == tasks.len() {
        Some(order)
    } else {
        None
    }
}

/// Whether `a` transitively depends on `b`.
///
/// This is the rule that makes file overlap legal: two tasks may touch the same
/// file when one waits for the other, because then the second sees the first's
/// change and the merge is a fast-forward rather than a conflict.
pub fn depends_on(tasks: &[Task], a: &str, b: &str) -> bool {
    // A task never waits for itself. Without this, a cycle makes `depends_on`
    // true for every pair inside it, and the file-overlap rule — which exists to
    // let two tasks share a file when one waits for the other — would wave
    // through exactly the plans it is meant to refuse.
    if a == b {
        return false;
    }
    let by_id: BTreeMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = vec![a];
    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        }
        let Some(task) = by_id.get(current) else {
            continue;
        };
        for dep in &task.deps {
            if dep == b {
                return true;
            }
            stack.push(dep.as_str());
        }
    }
    false
}

/// Validate a plan. `Ok` means it may run; `Err` is every reason it may not.
///
/// All violations are collected rather than the first one returned: a lead that
/// fixes one problem at a time costs a turn per problem.
/// Paths a task's criterion names that belong to another task.
///
/// The criterion is a command, so the paths in it are arguments. A path is
/// "outside" when another task owns a file at or under it and this task does not.
/// A directory counts: `grep -rq foo src/` reaches every file under `src/`, and a
/// task that owns one file in `src/` cannot make a repo-wide grep pass.
///
/// Deliberately conservative in one direction only: it reports a path only when
/// another task's file is genuinely under it, so a criterion that names nothing —
/// `node --test`, `cargo test` — is never refused.
///
/// A path owned by a task this one depends on is not outside: the dependency's
/// work is in this task's base, so reading it is the point of the dependency.
fn criterion_paths_outside(tasks: &[Task], task: &Task) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for token in task.acceptance.split_whitespace() {
        let token = token.trim_matches(|c: char| {
            matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | ',' | ';' | '&' | '|')
        });
        if token.is_empty() || !token.contains('/') && !token.contains('\\') {
            continue;
        }
        let token = token.replace('\\', "/");
        let token = token.trim_end_matches('/');
        if token.is_empty() || token.starts_with('-') {
            continue;
        }
        for other in tasks {
            if other.id == task.id {
                continue;
            }
            // A dependent may read what it depends on. The dependency's work is
            // merged into this task's base before it starts, so a criterion that
            // runs the dependency's test file is a criterion this task can
            // satisfy — and refusing it cost the eighth live run seventeen
            // escalations and a 2.7-minute revision round over
            // `tiny-harbor/tools`. Only a write is a violation, and a write is
            // what the file-overlap check is for.
            //
            // `depends_on` is transitive, which is what this needs: t8 depends on
            // t1 only through t2, and t1's files are in t8's base all the same.
            if depends_on(tasks, &task.id, &other.id) {
                continue;
            }
            for file in &other.files {
                let file = file.replace('\\', "/");
                let inside = file == token || file.starts_with(&format!("{token}/"));
                if !inside {
                    continue;
                }
                // This task owning the same file is the overlap check's job, not
                // this one's.
                if task.files.iter().any(|own| {
                    let own = own.replace('\\', "/");
                    own == file
                }) {
                    continue;
                }
                if seen.insert(format!("{token}|{}", other.id)) {
                    out.push((token.to_string(), other.id.clone()));
                }
            }
        }
    }
    out
}

pub fn validate(plan: &Plan) -> Result<(), Vec<Violation>> {
    validate_with(plan, None)
}

/// Validate a plan, optionally against the installed skill catalogue.
///
/// The catalogue is optional because most callers do not have one — the
/// rehearsal, the panel's preview, the tests. A caller that does have one gets
/// the skill check; a caller that does not is not told a plan is fine when it
/// names a skill nobody has, it is simply not asked the question. The engine
/// always passes the catalogue, so the check that matters always runs.
pub fn validate_with(
    plan: &Plan,
    catalogue: Option<&crate::swarm::skills::Catalogue>,
) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();

    if plan.tasks.len() < 2 {
        violations.push(Violation::TooFewTasks {
            count: plan.tasks.len(),
        });
    }

    // Duplicate ids first: every later check keys on the id, so a duplicate
    // would make the rest of the report nonsense.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for task in &plan.tasks {
        if !seen.insert(task.id.as_str()) {
            violations.push(Violation::DuplicateId {
                task: task.id.clone(),
            });
        }
    }

    let ids: BTreeSet<&str> = plan.tasks.iter().map(|t| t.id.as_str()).collect();

    for task in &plan.tasks {
        if task.acceptance.trim().is_empty() {
            violations.push(Violation::MissingAcceptance {
                task: task.id.clone(),
            });
        } else if let Some(marker) = prose_marker(&task.acceptance) {
            violations.push(Violation::CriterionIsProse {
                task: task.id.clone(),
                marker,
            });
        }
        if task.files.is_empty() {
            violations.push(Violation::EmptyFileSet {
                task: task.id.clone(),
            });
        }
        for dep in &task.deps {
            if dep == &task.id {
                violations.push(Violation::SelfDep {
                    task: task.id.clone(),
                });
            } else if !ids.contains(dep.as_str()) {
                violations.push(Violation::UnknownDep {
                    task: task.id.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    // A criterion that names another task's file is unsatisfiable by the task
    // that has to satisfy it. Checked before the DAG so the report reads in the
    // order the lead can fix it.
    for task in &plan.tasks {
        for (path, owner) in criterion_paths_outside(&plan.tasks, task) {
            violations.push(Violation::CriterionOutsideOwnership {
                task: task.id.clone(),
                path,
                owner,
            });
        }
    }

    if topological_order(&plan.tasks).is_none() {
        // Name the tasks that never reached indegree zero: those are the cycle.
        let ordered: BTreeSet<String> = topological_order(&plan.tasks).unwrap_or_default().into_iter().collect();
        let stuck: Vec<String> = plan
            .tasks
            .iter()
            .map(|t| t.id.clone())
            .filter(|id| !ordered.contains(id))
            .collect();
        violations.push(Violation::Cycle { tasks: stuck });
    }

    // File overlap, only between tasks the DAG does not order.
    for (index, a) in plan.tasks.iter().enumerate() {
        for b in plan.tasks.iter().skip(index + 1) {
            if depends_on(&plan.tasks, &a.id, &b.id) || depends_on(&plan.tasks, &b.id, &a.id) {
                continue;
            }
            let a_files: BTreeSet<String> = a.files.iter().map(|f| normalize_path(f)).collect();
            let shared: Vec<String> = b
                .files
                .iter()
                .map(|f| normalize_path(f))
                .filter(|f| a_files.contains(f))
                .collect();
            if !shared.is_empty() {
                violations.push(Violation::OverlappingFiles {
                    a: a.id.clone(),
                    b: b.id.clone(),
                    files: shared,
                });
            }
        }
    }

    if plan
        .integration_suite
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        violations.push(Violation::NoIntegrationSuite);
    }

    // Skills last, because it is the only check that needs something outside the
    // plan, and a report that leads with it would bury the structural reasons the
    // lead can fix without looking anything up.
    if let Some(catalogue) = catalogue {
        for task in &plan.tasks {
            for name in &task.skills {
                let known = catalogue
                    .get(name)
                    .map(|skill| skill.enabled)
                    .unwrap_or(false);
                if !known {
                    violations.push(Violation::UnknownSkill {
                        task: task.id.clone(),
                        skill: name.clone(),
                    });
                }
            }
        }
    }

    // The skills the plan writes. Checked here rather than at the write, because
    // a draft that fails this check must not leave a file behind: the run's
    // directory is the only place a rejected plan is allowed to be invisible.
    if plan.skills_to_write.len() > MAX_PLAN_SKILLS {
        violations.push(Violation::BadSkillDraft {
            name: format!("{} skills", plan.skills_to_write.len()),
            why: format!("a plan may write at most {MAX_PLAN_SKILLS}"),
        });
    }
    let mut written: BTreeSet<&str> = BTreeSet::new();
    for draft in &plan.skills_to_write {
        if let Some(why) = draft_problem(draft) {
            violations.push(Violation::BadSkillDraft {
                name: draft.name.clone(),
                why,
            });
            continue;
        }
        if !written.insert(draft.name.as_str()) {
            violations.push(Violation::BadSkillDraft {
                name: draft.name.clone(),
                why: "the plan writes it twice".to_string(),
            });
            continue;
        }
        // Installed, not merely present. The caller hands this gate a catalogue
        // that already contains the plan's own drafts, so that a task naming one
        // is not refused as unknown — which means "is the name in the catalogue"
        // answers yes for every draft, and every plan that wrote a skill was
        // refused for shadowing one the machine did not have. A run-scoped entry
        // is the plan's own work, not an installation.
        if catalogue
            .map(|catalogue| {
                catalogue
                    .get(&draft.name)
                    .is_some_and(|skill| skill.scope != super::skills::RUN_SCOPE)
            })
            .unwrap_or(false)
        {
            violations.push(Violation::SkillAlreadyInstalled {
                name: draft.name.clone(),
            });
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Why a draft cannot become a skill, or `None` when it can.
///
/// The same rules the reflection pass applies when it accepts a proposal, minus
/// the ones that need a directory: a name a directory can hold, a body with
/// something in it, and a description the panel can show. A draft with no
/// description gets one from its body's first line rather than being refused,
/// because the body is the part that matters and the description is a label.
pub fn draft_problem(draft: &super::reflect::SkillDraft) -> Option<String> {
    if draft.name.is_empty() {
        return Some("it has no name".to_string());
    }
    if draft.name.len() > 64 {
        return Some("the name is longer than 64 characters".to_string());
    }
    if !draft
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Some(
            "the name must be lowercase letters, digits, hyphens and underscores".to_string(),
        );
    }
    if draft.body.trim().is_empty() {
        return Some("it has no body".to_string());
    }
    None
}

/// The plan's shape, for the panel and the rehearsal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanShape {
    /// Tasks in execution order.
    pub order: Vec<String>,
    /// The longest chain of dependent tasks, by count. This is the floor on
    /// wall-clock: no amount of concurrency beats it.
    pub critical_path: Vec<String>,
    /// How many tasks can run at once if nothing is serialised.
    pub max_parallel: usize,
    /// How many tasks have no dependencies.
    ///
    /// The first rank is the only part of a plan that can start at once, so this
    /// is the width the run actually begins at. `max_parallel` is the width it
    /// might reach later, and the two are not the same claim: the observed
    /// ten-task plan had a widest rank of six and a first rank of one, so its
    /// first six minutes were a single worker and the 2.0 speedup hid it.
    pub first_rank_width: usize,
    /// Total declared files across the plan.
    pub files: usize,
}

/// Describe a valid plan.
pub fn shape(plan: &Plan) -> PlanShape {
    let order = topological_order(&plan.tasks).unwrap_or_default();

    // Longest path to each task, by task count, in topological order.
    let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
    let mut parent: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for id in &order {
        let Some(task) = plan.tasks.iter().find(|t| &t.id == id) else {
            continue;
        };
        let mut best = 0usize;
        let mut best_parent = None;
        for dep in &task.deps {
            let candidate = depth.get(dep.as_str()).copied().unwrap_or(0);
            if candidate >= best {
                best = candidate;
                best_parent = Some(dep.as_str());
            }
        }
        depth.insert(task.id.as_str(), best + 1);
        parent.insert(task.id.as_str(), best_parent);
    }

    let tail = depth
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(id, _)| *id);

    let mut critical_path = Vec::new();
    let mut cursor = tail;
    while let Some(id) = cursor {
        critical_path.push(id.to_string());
        cursor = parent.get(id).copied().flatten();
    }
    critical_path.reverse();

    // Widest rank: how many tasks share a depth, which is the most that could
    // ever run at once.
    let mut per_depth: BTreeMap<usize, usize> = BTreeMap::new();
    for value in depth.values() {
        *per_depth.entry(*value).or_insert(0) += 1;
    }
    let max_parallel = per_depth.values().copied().max().unwrap_or(0);

    PlanShape {
        order,
        critical_path,
        max_parallel,
        first_rank_width: plan.tasks.iter().filter(|t| t.deps.is_empty()).count(),
        files: plan
            .tasks
            .iter()
            .map(|t| t.files.len())
            .sum(),
    }
}

/// How many files one task may own before the plan is told it is too big.
///
/// Eight, because a task is one worker's turn: it has to read every file it
/// owns, change them coherently, and prove the change. The observed `t1` owned
/// 22 and everything depended on it, which is a rank of one wearing a task's
/// name — the plan looked ten wide and was one wide for as long as that task
/// took.
pub const MAX_TASK_FILES: usize = 8;

/// The speedup below which the swarm is not worth running.
///
/// The same threshold the rehearsal's doc comment commits to and the panel's
/// `worthRunning` applies. It is a constant here so the gate and the panel
/// cannot drift apart about what "worth it" means.
pub const MIN_SPEEDUP: f64 = 1.5;

/// A reason a plan is accepted but should not be.
///
/// Deliberately not a [`Violation`]. A violation refuses the plan and sends it
/// back to the lead, and a lead cannot make a genuinely sequential plan parallel
/// without lying about its dependencies — so a shape refusal is a loop the lead
/// cannot escape, spending its revision budget to arrive at the same plan. The
/// shape is therefore a warning the plan is accepted with, and the decision to
/// run a sequential plan is the user's: the plan review shows the warning beside
/// the approve button, which is the approve-anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ShapeWarning {
    /// The plan's own best case is below the threshold the rehearsal commits to.
    NotWorthIt {
        speedup: f64,
        serial_steps: usize,
        parallel_steps: usize,
    },
    /// Only one task can start, so the run begins single-threaded.
    OneWideFirstRank { task: String },
    /// A task that owns more files than one worker can hold at once.
    TaskTooLarge { task: String, files: usize },
}

impl ShapeWarning {
    /// The one line the user reads. Written as a fact about the plan, because
    /// the decision it informs is theirs.
    pub fn message(&self) -> String {
        match self {
            ShapeWarning::NotWorthIt {
                speedup,
                serial_steps,
                parallel_steps,
            } => format!(
                "this plan's best case is {speedup:.1}× — {serial_steps} steps serially, \
                 {parallel_steps} at its widest. Below {MIN_SPEEDUP}× the coordination costs \
                 more than it saves, and one agent would finish sooner"
            ),
            ShapeWarning::OneWideFirstRank { task } => format!(
                "only {task} can start — the first rank is one task wide, so the run begins \
                 single-threaded however many workers it is given"
            ),
            ShapeWarning::TaskTooLarge { task, files } => format!(
                "{task} owns {files} files — a task that large is a rank of one wearing a \
                 task's name, and everything behind it waits for one worker"
            ),
        }
    }

    /// A stable key, for the panel to group by.
    pub fn key(&self) -> &'static str {
        match self {
            ShapeWarning::NotWorthIt { .. } => "not-worth-it",
            ShapeWarning::OneWideFirstRank { .. } => "one-wide-first-rank",
            ShapeWarning::TaskTooLarge { .. } => "task-too-large",
        }
    }
}

/// Every reason this plan is accepted but should not be.
///
/// The speedup is computed at the plan's own best width — its widest rank — so
/// the warning is about the plan rather than about the concurrency the user
/// happened to ask for. A plan that cannot beat 1.5× with every worker it could
/// use cannot beat it with fewer, and a warning that moved when the user dragged
/// a slider would be a warning about the slider.
///
/// This is the one place in this module that reaches into [`super::schedule`],
/// and it does so because the speedup is the scheduler's own simulation: a
/// second estimate here would be a second answer to the same question.
pub fn shape_warnings(plan: &Plan) -> Vec<ShapeWarning> {
    let mut warnings = Vec::new();
    let shape = shape(plan);

    // Only `speedup` is read here, so the wall clock is not this function's
    // question and the median is not threaded in: a shape warning that moved
    // when the workspace got slower would be a warning about the workspace.
    let rehearsal = super::schedule::rehearse(&plan.tasks, shape.max_parallel.max(1), None);
    if rehearsal.speedup < MIN_SPEEDUP {
        warnings.push(ShapeWarning::NotWorthIt {
            speedup: rehearsal.speedup,
            serial_steps: rehearsal.serial_steps,
            parallel_steps: rehearsal.parallel_steps,
        });
    }

    if shape.first_rank_width == 1 {
        // The one task with no dependencies. A plan with no tasks at all has a
        // first rank of zero and is refused by `TooFewTasks`, so the fallback
        // here is unreachable rather than a second rule.
        let task = plan
            .tasks
            .iter()
            .find(|t| t.deps.is_empty())
            .map(|t| t.id.clone())
            .unwrap_or_default();
        warnings.push(ShapeWarning::OneWideFirstRank { task });
    }

    for task in &plan.tasks {
        if task.files.len() > MAX_TASK_FILES {
            warnings.push(ShapeWarning::TaskTooLarge {
                task: task.id.clone(),
                files: task.files.len(),
            });
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, files: &[&str], deps: &[&str]) -> Task {
        Task::new(id, format!("task {id}"), "do it", "cargo test")
            .with_files(files)
            .with_deps(deps)
    }

    fn plan(tasks: Vec<Task>) -> Plan {
        Plan {
            goal: "ship it".into(),
            tasks,
            integration_suite: Some("cargo test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
        }
    }

    fn draft(name: &str) -> crate::swarm::reflect::SkillDraft {
        crate::swarm::reflect::SkillDraft {
            name: name.into(),
            description: "a method".into(),
            body: "Do the thing in this order.".into(),
            because: "t1 needed it".into(),
        }
    }

    fn two_tasks() -> Vec<Task> {
        vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]
    }

    #[test]
    fn a_plan_may_write_a_skill_this_machine_does_not_have() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("scene-graph-basics")];
        assert_eq!(validate_with(&p, Some(&catalogue(&["assay"]))), Ok(()));
    }

    #[test]
    fn a_plan_writing_a_skill_that_is_already_installed_is_refused() {
        // Shadowing an installed skill would make the brief describe a method
        // the user did not write, under the name of one they did.
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("assay")];
        let violations = validate_with(&p, Some(&catalogue(&["assay"]))).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::SkillAlreadyInstalled { name } if name == "assay")));
    }

    /// A draft the gate was handed is not an installed skill.
    ///
    /// The engine extends the catalogue with the plan's own drafts before it
    /// validates, so that a task naming one is not refused as unknown. The
    /// installed check then asked "is this name in the catalogue" and got yes
    /// because the draft had just been put there — so every plan that wrote a
    /// skill was refused with "this machine already has it", naming a skill the
    /// machine did not have. No run has ever written one.
    #[test]
    fn a_draft_is_not_refused_because_the_gate_was_handed_the_draft() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("scene-graph-basics")];
        // Exactly the catalogue engine.rs builds: installed skills, then drafts.
        let mut with_drafts = catalogue(&["assay"]);
        with_drafts.skills.push(crate::swarm::skills::Skill {
            name: "scene-graph-basics".into(),
            description: "a method".into(),
            path: "C:/run/skills/scene-graph-basics/SKILL.md".into(),
            scope: crate::swarm::skills::RUN_SCOPE.into(),
            bundle: None,
            enabled: true,
            body: String::new(),
        });
        assert_eq!(
            validate_with(&p, Some(&with_drafts)),
            Ok(()),
            "a run-scoped draft is not an installed skill"
        );
    }

    /// A task may name a skill the plan itself writes.
    ///
    /// This is the other half of why the drafts go into the catalogue: without
    /// them the task's own skill reads as unknown and the plan is refused for
    /// naming something it is about to create.
    #[test]
    fn a_task_may_name_a_skill_the_plan_writes() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("scene-graph-basics")];
        p.tasks[0].skills = vec!["scene-graph-basics".into()];
        let mut with_drafts = catalogue(&[]);
        with_drafts.skills.push(crate::swarm::skills::Skill {
            name: "scene-graph-basics".into(),
            description: "a method".into(),
            path: "C:/run/skills/scene-graph-basics/SKILL.md".into(),
            scope: crate::swarm::skills::RUN_SCOPE.into(),
            bundle: None,
            enabled: true,
            body: String::new(),
        });
        assert_eq!(validate_with(&p, Some(&with_drafts)), Ok(()));
    }

    #[test]
    fn a_draft_with_a_name_a_directory_cannot_hold_is_refused() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("../escape")];
        let violations = validate_with(&p, Some(&catalogue(&[]))).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::BadSkillDraft { .. })));
    }

    #[test]
    fn a_draft_with_no_body_is_refused() {
        let mut p = plan(two_tasks());
        let mut empty = draft("empty-one");
        empty.body = "   ".into();
        p.skills_to_write = vec![empty];
        let violations = validate_with(&p, Some(&catalogue(&[]))).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::BadSkillDraft { why, .. } if why.contains("no body"))));
    }

    #[test]
    fn a_plan_may_not_write_more_than_two_skills() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("one"), draft("two"), draft("three")];
        let violations = validate_with(&p, Some(&catalogue(&[]))).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::BadSkillDraft { why, .. } if why.contains("at most"))));
    }

    #[test]
    fn the_same_draft_twice_is_refused() {
        let mut p = plan(two_tasks());
        p.skills_to_write = vec![draft("same"), draft("same")];
        let violations = validate_with(&p, Some(&catalogue(&[]))).unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::BadSkillDraft { why, .. } if why.contains("twice"))));
    }

    #[test]
    fn a_two_task_disjoint_plan_is_accepted() {
        let p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        assert_eq!(validate(&p), Ok(()));
    }

    fn catalogue(names: &[&str]) -> crate::swarm::skills::Catalogue {
        crate::swarm::skills::Catalogue {
            skills: names
                .iter()
                .map(|name| crate::swarm::skills::Skill {
                    name: (*name).into(),
                    description: "a method".into(),
                    path: format!("C:/skills/{name}/SKILL.md"),
                    scope: "user".into(),
                    bundle: None,
                    enabled: true,
                    body: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_task_may_name_a_skill_the_catalogue_holds() {
        let mut p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        p.tasks[0].skills = vec!["assay".into()];
        assert_eq!(validate_with(&p, Some(&catalogue(&["assay"]))), Ok(()));
    }

    #[test]
    fn a_task_naming_a_skill_that_is_not_installed_is_refused() {
        let mut p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        p.tasks[0].skills = vec!["invented".into()];
        let violations = validate_with(&p, Some(&catalogue(&["assay"]))).unwrap_err();
        assert!(violations.contains(&Violation::UnknownSkill {
            task: "t1".into(),
            skill: "invented".into(),
        }));
        assert!(violations[0].message().contains("not installed"));
    }

    #[test]
    fn a_disabled_skill_is_not_assignable() {
        let mut catalogue = catalogue(&["assay"]);
        catalogue.skills[0].enabled = false;
        let mut p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        p.tasks[0].skills = vec!["assay".into()];
        assert!(validate_with(&p, Some(&catalogue)).is_err());
    }

    #[test]
    fn without_a_catalogue_the_skill_check_is_simply_not_asked() {
        // The rehearsal and the panel's preview have no catalogue. They must not
        // be told a plan is fine when it names a skill nobody has — they are not
        // asked the question, which is different from being answered wrongly.
        let mut p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        p.tasks[0].skills = vec!["invented".into()];
        assert_eq!(validate(&p), Ok(()));
    }

    #[test]
    fn one_task_is_refused_as_a_slower_chat() {
        let p = plan(vec![task("t1", &["src/a.rs"], &[])]);
        let violations = validate(&p).unwrap_err();
        assert!(violations.contains(&Violation::TooFewTasks { count: 1 }));
        assert!(violations[0].message().contains("run it as a chat"));
    }

    #[test]
    fn an_empty_plan_is_refused() {
        let violations = validate(&plan(vec![])).unwrap_err();
        assert!(violations.contains(&Violation::TooFewTasks { count: 0 }));
    }

    #[test]
    fn a_task_without_an_acceptance_criterion_is_refused() {
        let mut t = task("t1", &["src/a.rs"], &[]);
        t.acceptance = "   ".into();
        let violations = validate(&plan(vec![t, task("t2", &["src/b.rs"], &[])])).unwrap_err();
        assert!(violations.contains(&Violation::MissingAcceptance {
            task: "t1".into()
        }));
    }

    #[test]
    fn a_task_that_declares_no_files_is_refused() {
        let violations = validate(&plan(vec![
            task("t1", &[], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]))
        .unwrap_err();
        assert!(violations.contains(&Violation::EmptyFileSet {
            task: "t1".into()
        }));
    }

    /// The sixth live run's t1, verbatim. The engine ran the whole sentence with
    /// `bash -c` and the verifier read it as English, so the two disagreed about
    /// the same field and the run spent three attempts discovering it.
    #[test]
    fn a_criterion_that_is_a_sentence_is_refused() {
        let mut t = task("t1", &["src/a.js"], &[]);
        t.acceptance = "node --test test/arena.test.js passes, where the new tests \
                        assert: ARENA_HALF.z equals GATE_FIELD.count * 2"
            .into();
        let violations = validate(&plan(vec![t, task("t2", &["src/b.js"], &[])])).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, Violation::CriterionIsProse { task, .. } if task == "t1")),
            "{violations:?}"
        );
        let message = violations
            .iter()
            .find(|v| matches!(v, Violation::CriterionIsProse { .. }))
            .unwrap()
            .message();
        assert!(
            message.contains("one shell command") || message.contains("verbatim"),
            "the refusal has to say what to write instead: {message}"
        );
    }

    /// A comma inside quotes is an argument, not a sentence.
    #[test]
    fn a_comma_inside_quotes_is_still_a_command() {
        assert_eq!(prose_marker("awk '{print $1, $2}' f.txt"), None);
        assert_eq!(prose_marker("sed 's/a, b/c/' f.txt"), None);
        assert_eq!(prose_marker("node -e \"console.log(1, 2)\""), None);
    }

    /// A colon with no space after it is a path or a separator, not prose.
    #[test]
    fn a_colon_without_a_space_is_still_a_command() {
        assert_eq!(prose_marker("test -f C:/tmp/x"), None);
        assert_eq!(prose_marker("PATH=/usr/bin:$PATH cargo test"), None);
    }

    #[test]
    fn a_multi_line_criterion_is_refused() {
        assert_eq!(
            prose_marker("node --test test/a.js\nnode --test test/b.js"),
            Some("a newline".into())
        );
    }

    #[test]
    fn a_plain_command_has_no_marker() {
        assert_eq!(prose_marker("cargo test rate_limit"), None);
        assert_eq!(
            prose_marker("node --test test/a.js && node --test test/b.js"),
            None
        );
        assert_eq!(prose_marker("grep -c 'passed = true' src/logic/gates.js"), None);
    }

    #[test]
    fn an_unknown_dependency_is_named() {
        let violations = validate(&plan(vec![
            task("t1", &["src/a.rs"], &["t9"]),
            task("t2", &["src/b.rs"], &[]),
        ]))
        .unwrap_err();
        assert!(violations.contains(&Violation::UnknownDep {
            task: "t1".into(),
            dep: "t9".into()
        }));
    }

    #[test]
    fn a_self_dependency_is_refused() {
        let violations = validate(&plan(vec![
            task("t1", &["src/a.rs"], &["t1"]),
            task("t2", &["src/b.rs"], &[]),
        ]))
        .unwrap_err();
        assert!(violations.contains(&Violation::SelfDep {
            task: "t1".into()
        }));
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let violations = validate(&plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t1", &["src/b.rs"], &[]),
        ]))
        .unwrap_err();
        assert!(violations.contains(&Violation::DuplicateId {
            task: "t1".into()
        }));
    }

    #[test]
    fn a_cycle_is_refused_and_the_stuck_tasks_are_named() {
        let violations = validate(&plan(vec![
            task("t1", &["src/a.rs"], &["t2"]),
            task("t2", &["src/b.rs"], &["t1"]),
        ]))
        .unwrap_err();
        let cycle = violations
            .iter()
            .find(|v| matches!(v, Violation::Cycle { .. }))
            .expect("a cycle violation");
        let Violation::Cycle { tasks } = cycle else {
            unreachable!()
        };
        assert_eq!(tasks.len(), 2);
        assert!(tasks.contains(&"t1".to_string()));
        assert!(tasks.contains(&"t2".to_string()));
    }

    #[test]
    fn two_unordered_tasks_claiming_one_file_are_refused() {
        let violations = validate(&plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/a.rs"], &[]),
        ]))
        .unwrap_err();
        let overlap = violations
            .iter()
            .find(|v| matches!(v, Violation::OverlappingFiles { .. }))
            .expect("an overlap violation");
        assert!(overlap.message().contains("src/a.rs"));
        assert!(overlap.message().contains("neither depends on the other"));
    }

    #[test]
    fn two_ordered_tasks_may_claim_one_file() {
        // t2 waits for t1, so it sees t1's change and the merge is a
        // fast-forward. This is the case the overlap rule exists to allow.
        let p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/a.rs"], &["t1"]),
        ]);
        assert_eq!(validate(&p), Ok(()));
    }

    #[test]
    fn a_transitive_dependency_also_permits_overlap() {
        let p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &["t1"]),
            task("t3", &["src/a.rs"], &["t2"]),
        ]);
        assert_eq!(validate(&p), Ok(()));
    }

    #[test]
    fn a_plan_without_an_integration_suite_is_refused() {
        let mut p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &[]),
        ]);
        p.integration_suite = None;
        let violations = validate(&p).unwrap_err();
        assert!(violations.contains(&Violation::NoIntegrationSuite));
    }

    #[test]
    fn every_violation_is_collected_not_just_the_first() {
        let mut bad = task("t1", &[], &["t9"]);
        bad.acceptance = String::new();
        let violations = validate(&plan(vec![bad])).unwrap_err();
        // too-few, missing-acceptance, empty-file-set, unknown-dep
        assert!(violations.len() >= 4, "{violations:?}");
    }

    #[test]
    fn paths_normalise_so_two_spellings_do_not_slip_past_the_overlap_check() {
        assert_eq!(normalize_path("./src/a.rs"), "src/a.rs");
        assert_eq!(normalize_path("src//a.rs"), "src/a.rs");
        assert_eq!(normalize_path("src\\a.rs"), "src/a.rs");
        assert_eq!(normalize_path(" src/a.rs/ "), "src/a.rs");

        let violations = validate(&plan(vec![
            task("t1", &["./src/a.rs"], &[]),
            task("t2", &["src//a.rs"], &[]),
        ]))
        .unwrap_err();
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::OverlappingFiles { .. })));
    }

    #[test]
    fn topological_order_is_deterministic_for_equal_candidates() {
        let tasks = vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &[]),
            task("t3", &["c"], &["t1"]),
        ];
        let first = topological_order(&tasks).unwrap();
        let second = topological_order(&tasks).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn topological_order_puts_dependencies_first() {
        let tasks = vec![
            task("t3", &["c"], &["t2"]),
            task("t1", &["a"], &[]),
            task("t2", &["b"], &["t1"]),
        ];
        let order = topological_order(&tasks).unwrap();
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("t1") < pos("t2"));
        assert!(pos("t2") < pos("t3"));
    }

    #[test]
    fn depends_on_follows_the_chain() {
        let tasks = vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &["t1"]),
            task("t3", &["c"], &["t2"]),
        ];
        assert!(depends_on(&tasks, "t3", "t1"));
        assert!(depends_on(&tasks, "t2", "t1"));
        assert!(!depends_on(&tasks, "t1", "t3"));
        assert!(!depends_on(&tasks, "t1", "t2"));
    }

    #[test]
    fn depends_on_terminates_on_a_cycle() {
        let tasks = vec![
            task("t1", &["a"], &["t2"]),
            task("t2", &["b"], &["t1"]),
        ];
        // Must not hang, and must not claim a task depends on itself.
        assert!(!depends_on(&tasks, "t1", "t1"));
    }

    #[test]
    fn the_shape_names_the_critical_path_and_the_widest_rank() {
        // t1 → t2 → t3 is the long chain; t4 and t5 are independent.
        let p = plan(vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &["t1"]),
            task("t3", &["c"], &["t2"]),
            task("t4", &["d"], &[]),
            task("t5", &["e"], &[]),
        ]);
        let shape = shape(&p);
        assert_eq!(shape.critical_path, vec!["t1", "t2", "t3"]);
        // depth 1: t1, t4, t5 → three at once.
        assert_eq!(shape.max_parallel, 3);
        assert_eq!(shape.files, 5);
    }

    #[test]
    fn a_fully_serial_plan_has_a_parallelism_of_one() {
        let p = plan(vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &["t1"]),
            task("t3", &["c"], &["t2"]),
        ]);
        let shape = shape(&p);
        assert_eq!(shape.max_parallel, 1);
        assert_eq!(shape.critical_path.len(), 3);
    }

    #[test]
    fn a_fully_parallel_plan_has_a_critical_path_of_one() {
        let p = plan(vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &[]),
            task("t3", &["c"], &[]),
        ]);
        let shape = shape(&p);
        assert_eq!(shape.critical_path.len(), 1);
        assert_eq!(shape.max_parallel, 3);
    }

    #[test]
    fn a_plan_round_trips_through_json_under_camel_case() {
        let p = plan(vec![
            task("t1", &["src/a.rs"], &[]),
            task("t2", &["src/b.rs"], &["t1"]),
        ]);
        let body = serde_json::to_string(&p).unwrap();
        assert!(body.contains("\"integrationSuite\""), "{body}");
        let back: Plan = serde_json::from_str(&body).unwrap();
        assert_eq!(back.tasks.len(), 2);
        assert_eq!(back.tasks[1].deps, vec!["t1".to_string()]);
    }

    #[test]
    fn a_violation_serialises_with_a_kind_tag() {
        let body = serde_json::to_string(&Violation::TooFewTasks { count: 1 }).unwrap();
        assert!(body.contains("\"kind\":\"too-few-tasks\""), "{body}");
    }
    /* -------------------------------------------------------------- *
     * A criterion the task cannot satisfy on its own
     * -------------------------------------------------------------- */

    fn task_with(id: &str, files: &[&str], acceptance: &str) -> Task {
        let mut task = Task::new(id, "title", "detail", acceptance);
        task.files = files.iter().map(|f| f.to_string()).collect();
        task
    }

    #[test]
    fn a_criterion_that_greps_another_tasks_directory_is_refused() {
        // The eighth live run's t2: make `advanceGateField` appear nowhere in
        // src/ or test/, while owning only src/logic/gates.js. src/main.js is
        // t3's file, so the criterion was unsatisfiable by construction.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with(
                    "t2",
                    &["src/logic/gates.js"],
                    "node --test test/gates.test.js && ! grep -rq advanceGateField src/ test/",
                ),
                task_with("t3", &["src/main.js"], "node --test test/step.test.js"),
            ],
        };
        let violations = validate(&plan).unwrap_err();
        assert!(
            violations.iter().any(|v| matches!(
                v,
                Violation::CriterionOutsideOwnership { task, owner, .. }
                    if task == "t2" && owner == "t3"
            )),
            "{violations:?}"
        );
    }

    #[test]
    fn a_criterion_that_names_its_own_files_is_fine() {
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with(
                    "t1",
                    &["src/logic/gates.js", "test/gates.test.js"],
                    "node --test test/gates.test.js",
                ),
                task_with("t2", &["src/main.js"], "node --test test/step.test.js"),
            ],
        };
        assert!(validate(&plan).is_ok(), "{:?}", validate(&plan).err());
    }

    #[test]
    fn a_criterion_with_no_paths_is_never_refused() {
        // `node --test` and `cargo test` name nothing. A check that refused them
        // would refuse every plan.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with("t1", &["src/a.js"], "node --test"),
                task_with("t2", &["src/b.js"], "cargo test --quiet"),
            ],
        };
        assert!(validate(&plan).is_ok(), "{:?}", validate(&plan).err());
    }

    #[test]
    fn a_criterion_naming_a_file_no_task_owns_is_not_this_violation() {
        // An unowned file is a different problem — the worker escalates and the
        // lead amends. Refusing the plan here would be the wrong diagnosis.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with("t1", &["src/a.js"], "node --test test/nobody-owns-this.js"),
                task_with("t2", &["src/b.js"], "node --test"),
            ],
        };
        assert!(validate(&plan).is_ok(), "{:?}", validate(&plan).err());
    }

    #[test]
    fn the_violation_names_the_path_and_the_owner() {
        let violation = Violation::CriterionOutsideOwnership {
            task: "t2".into(),
            path: "src/".into(),
            owner: "t3".into(),
        };
        let message = violation.message();
        assert!(message.contains("t2"), "{message}");
        assert!(message.contains("src/"), "{message}");
        assert!(message.contains("t3"), "{message}");
        assert_eq!(violation.key(), "criterion-outside-ownership");
    }

    /* -------------------------------------------------------------- *
     * A criterion that reads what it depends on
     * -------------------------------------------------------------- */

    #[test]
    fn a_criterion_that_runs_a_dependencys_file_is_accepted() {
        // t2 depends on t1, so t1's work is merged into t2's base before t2
        // starts. Running t1's test file is the point of the dependency, not a
        // reach into someone else's work — and refusing it cost the eighth live
        // run seventeen escalations and a 2.7-minute revision round over
        // `tiny-harbor/tools`.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with(
                    "t1",
                    &["src/limiter.js", "test/limiter.test.js"],
                    "node --test test/limiter.test.js",
                ),
                task_with(
                    "t2",
                    &["src/middleware.js"],
                    "node --test test/limiter.test.js test/middleware.test.js",
                )
                .with_deps(&["t1"]),
            ],
        };
        assert!(validate(&plan).is_ok(), "{:?}", validate(&plan).err());
    }

    #[test]
    fn a_criterion_that_runs_a_transitive_dependencys_file_is_accepted() {
        // t3 depends on t1 only through t2, and t1's files are in t3's base all
        // the same. `depends_on` is transitive, which is what this needs.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with(
                    "t1",
                    &["src/limiter.js", "test/limiter.test.js"],
                    "node --test test/limiter.test.js",
                ),
                task_with("t2", &["src/middleware.js"], "node --test test/middleware.test.js")
                    .with_deps(&["t1"]),
                task_with("t3", &["src/routes.js"], "node --test test/limiter.test.js")
                    .with_deps(&["t2"]),
            ],
        };
        assert!(validate(&plan).is_ok(), "{:?}", validate(&plan).err());
    }

    #[test]
    fn a_criterion_naming_a_file_owned_by_a_task_it_does_not_depend_on_is_still_refused() {
        // The exemption is per owner, not a blanket pass for a task that happens
        // to have a dependency. t2 depends on t1, so t1's file is fair game;
        // t3 is unrelated, so its file is not.
        let plan = Plan {
            goal: "g".into(),
            integration_suite: Some("node --test".into()),
            notes: String::new(),
            skills_to_write: Vec::new(),
            tasks: vec![
                task_with("t1", &["src/a.js"], "node --test src/a.js"),
                task_with("t2", &["src/b.js"], "node --test src/a.js src/c.js")
                    .with_deps(&["t1"]),
                task_with("t3", &["src/c.js"], "node --test src/c.js"),
            ],
        };
        let violations = validate(&plan).unwrap_err();
        assert!(
            violations.iter().any(|v| matches!(
                v,
                Violation::CriterionOutsideOwnership { task, owner, .. }
                    if task == "t2" && owner == "t3"
            )),
            "{violations:?}"
        );
        assert!(
            !violations.iter().any(|v| matches!(
                v,
                Violation::CriterionOutsideOwnership { owner, .. } if owner == "t1"
            )),
            "t1 is a dependency and must not be flagged: {violations:?}"
        );
    }

    /* -------------------------------------------------------------- *
     * The shape: accepted, but said out loud
     * -------------------------------------------------------------- */

    /// The plan the live run actually submitted.
    ///
    /// Ten tasks, `t1` with no dependencies and 22 files, `t2`–`t7` behind it,
    /// `t8` behind `t1`+`t2`, `t9` behind `t1`+`t2`+`t8`, `t10` behind
    /// everything. The rehearsal scored it 2.0 and nobody looked.
    fn observed_plan() -> Plan {
        let mut tasks = Vec::new();
        let mut t1 = task("t1", &[], &[]);
        t1.files = (0..22).map(|i| format!("src/t1/f{i}.rs")).collect();
        tasks.push(t1);
        for id in ["t2", "t3", "t4", "t5", "t6", "t7"] {
            let file = format!("src/{id}.rs");
            tasks.push(task(id, &[file.as_str()], &["t1"]));
        }
        let t8 = "src/t8.rs".to_string();
        tasks.push(task("t8", &[t8.as_str()], &["t1", "t2"]));
        let t9 = "src/t9.rs".to_string();
        tasks.push(task("t9", &[t9.as_str()], &["t1", "t2", "t8"]));
        let t10 = "src/t10.rs".to_string();
        tasks.push(task("t10", &[t10.as_str()], &["t1", "t2", "t8", "t9"]));
        plan(tasks)
    }

    #[test]
    fn the_first_rank_is_the_tasks_with_no_dependencies() {
        let p = plan(vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &[]),
            task("t3", &["c"], &["t1"]),
        ]);
        assert_eq!(shape(&p).first_rank_width, 2);
        // The observed plan: one task can start, six can run once it is done.
        let observed = shape(&observed_plan());
        assert_eq!(observed.first_rank_width, 1);
        assert_eq!(observed.max_parallel, 6);
    }

    #[test]
    fn the_observed_plan_is_warned_about_its_first_rank_and_its_22_file_task() {
        let warnings = shape_warnings(&observed_plan());
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ShapeWarning::OneWideFirstRank { task } if task == "t1")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ShapeWarning::TaskTooLarge { task, files } if task == "t1" && *files == 22)),
            "{warnings:?}"
        );
        // 2.0 is above the threshold, which is exactly why the score hid the
        // one-wide first rank: the overall number was fine.
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, ShapeWarning::NotWorthIt { .. })),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_same_plan_with_a_wide_first_rank_and_no_oversized_task_has_no_warning() {
        // `t2`–`t7` depend on nothing, so seven tasks start together. `t1`'s 22
        // files are spread across the seven that can now run at once, because a
        // 22-file task warns on its own merits — the point of this test is that
        // the parallelism warning is gone once the plan is genuinely parallel.
        let mut tasks = Vec::new();
        let mut t1 = task("t1", &[], &[]);
        t1.files = (0..4).map(|i| format!("src/t1/f{i}.rs")).collect();
        tasks.push(t1);
        for id in ["t2", "t3", "t4", "t5", "t6", "t7"] {
            let files: Vec<String> = (0..3).map(|i| format!("src/{id}/f{i}.rs")).collect();
            let refs: Vec<&str> = files.iter().map(String::as_str).collect();
            tasks.push(task(id, &refs, &[]));
        }
        let t8 = "src/t8.rs".to_string();
        tasks.push(task("t8", &[t8.as_str()], &["t1", "t2"]));
        let t9 = "src/t9.rs".to_string();
        tasks.push(task("t9", &[t9.as_str()], &["t1", "t2", "t8"]));
        let t10 = "src/t10.rs".to_string();
        tasks.push(task("t10", &[t10.as_str()], &["t1", "t2", "t8", "t9"]));
        let fixed = plan(tasks);

        assert_eq!(shape(&fixed).first_rank_width, 7);
        assert_eq!(shape_warnings(&fixed), Vec::new());
    }

    #[test]
    fn a_plan_that_cannot_beat_the_threshold_is_warned_about() {
        // A five-deep chain: ten steps serially, five at any width, so 2.0. A
        // chain of three is 1.0 and the swarm is strictly worse than a chat.
        let p = plan(vec![
            task("t1", &["a"], &[]),
            task("t2", &["b"], &["t1"]),
            task("t3", &["c"], &["t2"]),
        ]);
        let warnings = shape_warnings(&p);
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ShapeWarning::NotWorthIt { speedup, .. } if *speedup < MIN_SPEEDUP)),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_example_plan_is_not_warned_about() {
        // The panel's demo plan sits exactly on the threshold: three tasks, two
        // of them independent, so 1.5×. A warning here would put a red line on
        // the one plan the panel shows before anything is spawned.
        assert_eq!(shape_warnings(&super::super::roles::example_plan()), Vec::new());
    }

    #[test]
    fn a_shape_warning_serialises_with_a_kind_tag_and_a_readable_message() {
        let warning = ShapeWarning::TaskTooLarge {
            task: "t1".into(),
            files: 22,
        };
        let body = serde_json::to_string(&warning).unwrap();
        assert!(body.contains("\"kind\":\"task-too-large\""), "{body}");
        assert_eq!(warning.key(), "task-too-large");
        let message = warning.message();
        assert!(message.contains("t1"), "{message}");
        assert!(message.contains("22"), "{message}");

        let one_wide = ShapeWarning::OneWideFirstRank { task: "t1".into() };
        assert!(one_wide.message().contains("t1"), "{}", one_wide.message());
        assert_eq!(one_wide.key(), "one-wide-first-rank");
    }

    #[test]
    fn a_shape_warning_serialises_its_fields_in_camel_case() {
        // The panel reads `serialSteps` and `parallelSteps`. `rename_all` on an
        // enum renames the variants, not the fields inside them, so without
        // `rename_all_fields` these arrive as `serial_steps` and the panel shows
        // `undefined` for both numbers.
        let body = serde_json::to_string(&ShapeWarning::NotWorthIt {
            speedup: 1.2,
            serial_steps: 10,
            parallel_steps: 8,
        })
        .unwrap();
        assert!(body.contains("\"kind\":\"not-worth-it\""), "{body}");
        assert!(body.contains("\"serialSteps\":10"), "{body}");
        assert!(body.contains("\"parallelSteps\":8"), "{body}");

        let back: ShapeWarning = serde_json::from_str(&body).unwrap();
        assert_eq!(
            back,
            ShapeWarning::NotWorthIt {
                speedup: 1.2,
                serial_steps: 10,
                parallel_steps: 8,
            }
        );
    }
}
