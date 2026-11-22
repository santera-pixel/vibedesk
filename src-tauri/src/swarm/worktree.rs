//! The git lifecycle, behind a trait so it can be tested without a repository.
//!
//! `vibe-acp` cannot create a worktree — its whole command line is `-h`, `-v`
//! and `--setup` — so the client owns this. That puts the ownership and cleanup
//! rules in VibeDesk, which is where they have to live anyway: a worktree that
//! outlives its run is a directory the user cannot find and cannot safely
//! delete.
//!
//! Every operation goes through [`Git`] rather than calling `git` directly, so
//! the engine's decisions are testable against a fake and the real
//! implementation stays a thin shell.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The one thing this module needs from the outside world.
pub trait Git {
    /// Run `git` with these arguments in this directory.
    fn run(&self, args: &[&str], cwd: &Path) -> Result<String, String>;
}

/// The real one.
pub struct CommandGit;

impl Git for CommandGit {
    fn run(&self, args: &[&str], cwd: &Path) -> Result<String, String> {
        let mut command = Command::new("git");
        command.args(args).current_dir(cwd);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|error| format!("could not run git: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Err(if stderr.is_empty() { stdout } else { stderr })
        }
    }
}

/// Where worktrees live. Beside the sessions, under `VIBE_HOME`.
pub fn worktree_root() -> PathBuf {
    crate::paths::vibe_home().join("worktrees")
}

/// The branch a task's work lands on.
///
/// Namespaced by run so two runs of the same plan never collide, and by task so
/// the merge order is readable in `git log`.
pub fn branch_name(run_id: &str, task_id: &str) -> String {
    // A dash, not a slash. The integration branch is `swarm/<run>`, and git
    // cannot have `refs/heads/swarm/<run>` and `refs/heads/swarm/<run>/<task>` at
    // the same time — a ref is either a file or a directory. Nested task branches
    // made every worker fail with "cannot lock ref", which is how the first real
    // dispatch died.
    format!("swarm/{run_id}-{task_id}")
}

/// The worktree path for one member.
pub fn worktree_path(run_id: &str, member_id: &str) -> PathBuf {
    worktree_root().join(run_id).join(member_id)
}

/// The run's base checkout: one per run, detached at the commit the run started
/// from.
///
/// This is what makes failing-first provable. Without it the engine has nowhere
/// to run a verifier's command *without* the change, so "this test fails on the
/// base" stays an instruction the verifier can skip. One shared checkout rather
/// than one per task: it is read-only in intent, and a per-task copy would be N
/// checkouts of the same commit.
pub fn base_checkout_path(run_id: &str) -> PathBuf {
    worktree_root().join(run_id).join("base")
}

/// Who owns what, for resolving a dependency conflict.
///
/// The plan already says which task owns which file. That declaration is the
/// authority a conflict needs: two branches disagreeing about a file is only a
/// question when nobody owns it, or when two tasks do.
pub struct Ownership<'a> {
    /// The task whose worktree is being prepared.
    pub current: &'a str,
    /// Every task in the plan and the files it was granted.
    pub tasks: &'a [(String, Vec<String>)],
}

impl Ownership<'_> {
    fn declares(&self, task_id: &str, file: &str) -> bool {
        self.tasks
            .iter()
            .any(|(id, files)| id == task_id && files.iter().any(|f| f == file))
    }
}

/// A conflict the plan itself resolved, and the file it was resolved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub file: String,
    /// The task whose version of the file was kept.
    pub owner: String,
}

/// Start a task's worktree from its dependencies, and return the commit it now
/// starts from.
///
/// A task's acceptance criterion almost always needs its dependency's files:
/// `node --test test/gates.test.js` cannot run without the `package.json` that
/// t1 wrote. Branching from the run's base commit makes that criterion
/// unrunnable, and the first live run showed both ways it goes wrong — two
/// workers merged their dependency by hand, and a third did not and failed its
/// verifier twice for a reason that was the engine's fault.
///
/// The merges land on the task's own branch, so integration still sees one
/// branch per task and git dedupes the shared history. The returned commit is
/// the base side of the task's failing-first proof.
///
/// A conflict between two dependencies is resolved from the plan's own ownership
/// declaration rather than by stopping the run. The twelfth live run died on
/// exactly this: t1 and t2 both verified, t1 had committed `src/player.js`
/// outside its grant, t2 owned it, and the run failed with "no task can make
/// progress" while the lead sat parked. The plan already said who owned the
/// file; nothing read it.
///
/// Three cases, and only the third is a question:
///
/// - exactly one dependency owns the file — that dependency's version is kept,
///   and the other branch's out-of-grant edit to it is dropped;
/// - no dependency owns it but the task being prepared does — the incoming
///   dependency's version is kept and the worker reconciles it, because the
///   worker's own work has not happened yet and its grant covers the file;
/// - nobody owns it, or two tasks do — the merge is aborted and the caller
///   escalates, because there is no answer the plan implies.
pub fn merge_deps(
    git: &dyn Git,
    worktree: &Path,
    deps: &[(String, String)],
    ownership: &Ownership<'_>,
) -> Result<(String, Vec<Resolved>), String> {
    let mut resolved: Vec<Resolved> = Vec::new();
    for (task_id, branch) in deps {
        match merge(git, worktree, branch) {
            Err(error) => return Err(format!("could not start from {task_id}: {error}")),
            Ok(MergeOutcome::Clean) => {}
            Ok(MergeOutcome::Conflict { files }) => {
                let mut unresolved: Vec<String> = Vec::new();
                for file in &files {
                    // The dependency that owns it, if exactly one does.
                    let owner = deps
                        .iter()
                        .map(|(id, _)| id)
                        .find(|id| ownership.declares(id, file));
                    let two_owners = deps
                        .iter()
                        .filter(|(id, _)| ownership.declares(id, file))
                        .count()
                        > 1;
                    if let (Some(owner), false) = (owner, two_owners) {
                        let owner_branch = deps
                            .iter()
                            .find(|(id, _)| id == owner)
                            .map(|(_, branch)| branch.clone())
                            .unwrap_or_else(|| branch.clone());
                        git.run(&["checkout", &owner_branch, "--", file], worktree)?;
                        // `checkout <tree-ish> -- <path>` writes the file and the
                        // index entry, but git still counts the path as unmerged
                        // until it is staged. Without this the merge cannot be
                        // committed and the resolution is a file on disk that the
                        // next `git status` calls a conflict.
                        git.run(&["add", "--", file], worktree)?;
                        resolved.push(Resolved {
                            file: file.clone(),
                            owner: owner.clone(),
                        });
                    } else if !two_owners && ownership.declares(ownership.current, file) {
                        // The task being prepared owns it, so the reconciliation
                        // is its work. Keep the incoming dependency's version and
                        // let the worker's own commit be the change.
                        git.run(&["checkout", "--theirs", "--", file], worktree)?;
                        git.run(&["add", "--", file], worktree)?;
                        resolved.push(Resolved {
                            file: file.clone(),
                            owner: ownership.current.to_string(),
                        });
                    } else {
                        unresolved.push(file.clone());
                    }
                }
                if !unresolved.is_empty() {
                    // Leave the tree usable: an aborted merge is a clean tree,
                    // and the caller is about to escalate with a reason it can
                    // read.
                    let _ = abort_merge(git, worktree);
                    return Err(format!(
                        "could not start from {task_id}: {branch} conflicts on {}, and no task owns {}",
                        unresolved.join(", "),
                        if unresolved.len() == 1 { "it" } else { "them" }
                    ));
                }
                // Every conflicted path was resolved from the plan, so the merge
                // can be completed. `--no-edit` keeps git's own merge message.
                git.run(&["commit", "--no-edit"], worktree)?;
            }
        }
    }
    Ok((head(git, worktree)?, resolved))
}

/// The checkout a task's failing-first proof compares against.
///
/// One per task, not one per run. A task that depends on t1 is proven against
/// t1's work, because that is the tree its criterion was written for; proving it
/// against the run's base would fail only because the dependency is missing, and
/// a proof that fails for the wrong reason is not a proof.
pub fn task_base_path(run_id: &str, task_id: &str) -> PathBuf {
    worktree_root().join(run_id).join(format!("{task_id}-base"))
}

/// Put a disposable checkout back to the commit it was created at.
///
/// A verifier is allowed to enter its task's base checkout, because the
/// failing-first proof its brief demands cannot be run anywhere else. That
/// permission is a path permission, not a read permission: a verifier that
/// leaves a file behind would be measuring its own edit. The engine resets the
/// checkout before it proves, so a dirty base cannot turn a real failure into a
/// pass. `clean -fd` is safe here and only here — the directory is a throwaway
/// checkout the engine created and can recreate.
pub fn reset_checkout(git: &dyn Git, path: &Path) -> Result<(), String> {
    git.run(&["checkout", "--", "."], path)?;
    git.run(&["clean", "-fd"], path)?;
    Ok(())
}

/// Whether a directory is inside a git work tree.
pub fn is_repo(git: &dyn Git, repo: &Path) -> bool {
    git.run(&["rev-parse", "--git-dir"], repo).is_ok()
}

/// The commit a worktree is based on.
pub fn head(git: &dyn Git, repo: &Path) -> Result<String, String> {
    git.run(&["rev-parse", "HEAD"], repo)
        .map(|out| out.trim().to_string())
}

/// What [`ensure_repo`] did to make a folder git-ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPrep {
    /// Already a repository with a root commit; nothing was done.
    Ready,
    /// `git init` was run.
    Initialized,
    /// `git init` ran and an empty root commit was added.
    InitializedWithRootCommit,
    /// The repository existed but had no commits; an empty root commit was added.
    RootCommitAdded,
}

impl RepoPrep {
    pub fn describe(self) -> &'static str {
        match self {
            RepoPrep::Ready => "already a git repository",
            RepoPrep::Initialized => "ran git init",
            RepoPrep::InitializedWithRootCommit => "ran git init and added an empty root commit",
            RepoPrep::RootCommitAdded => "added an empty root commit to the existing repository",
        }
    }
}

/// Make a folder usable as a swarm workspace: `git init` when it is not inside
/// a repository, and an empty root commit when there is no HEAD to base
/// worktrees on. Never stages or commits the user's files — an empty commit is
/// all a worktree needs, and the identity is passed with `-c` so a machine with
/// no git config still works.
pub fn ensure_repo(git: &dyn Git, repo: &Path) -> Result<RepoPrep, String> {
    const ROOT_COMMIT: &[&str] = &[
        "-c",
        "user.name=VibeDesk",
        "-c",
        "user.email=vibedesk@localhost",
        "commit",
        "--allow-empty",
        "-m",
        "vibedesk: root commit so swarm worktrees have a base",
    ];
    if !is_repo(git, repo) {
        git.run(&["init"], repo)
            .map_err(|error| format!("git init failed: {error}"))?;
        if head(git, repo).is_ok() {
            return Ok(RepoPrep::Initialized);
        }
        git.run(ROOT_COMMIT, repo)
            .map_err(|error| format!("could not create a root commit: {error}"))?;
        return Ok(RepoPrep::InitializedWithRootCommit);
    }
    if head(git, repo).is_err() {
        git.run(ROOT_COMMIT, repo)
            .map_err(|error| format!("could not create a root commit: {error}"))?;
        return Ok(RepoPrep::RootCommitAdded);
    }
    Ok(RepoPrep::Ready)
}

/// The commit a ref points at.
///
/// A branch name resolves to its tip, which is what a dependency contributed to
/// a dependent's base. Read from the repository rather than from the worktree,
/// because the branch a task merged in is not the branch its own worktree is on.
pub fn rev_parse(git: &dyn Git, repo: &Path, reference: &str) -> Result<String, String> {
    git.run(&["rev-parse", reference], repo)
        .map(|out| out.trim().to_string())
}

/// Create a worktree on a new branch.
///
/// `-b` rather than checking out an existing branch: two workers must never
/// share a branch, or their commits interleave and the merge order stops
/// meaning anything.
pub fn add_worktree(
    git: &dyn Git,
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let path_str = path.to_string_lossy().to_string();
    git.run(
        &["worktree", "add", "-b", branch, &path_str, base],
        repo,
    )
    .map(|_| ())?;
    // The worktree's directory name is the member id, so every checkout commits
    // under the member that owns it. Without this every commit in every worktree
    // carried the machine-wide identity, and the audited run's verifier commit
    // (`e18875b`) was indistinguishable from its worker's.
    if let Some(member) = path.file_name().and_then(|name| name.to_str()) {
        set_identity(git, path, member)?;
    }
    Ok(())
}

/// The git identity a member commits under.
///
/// A guard, not a security boundary: a worker that sets its own `GIT_AUTHOR_*`
/// still wins. It catches the case that actually happened — a verifier
/// committing into its worker's branch — which needed no forgery at all.
pub fn identity(member: &str) -> (String, String) {
    (format!("swarm-{member}"), format!("{member}@swarm.local"))
}

/// Point one worktree's git config at one member.
///
/// `--worktree` plus `extensions.worktreeConfig` is what makes this per-worktree.
/// Without them `git config user.email` in a linked worktree writes the *shared*
/// `.git/config`, so the last member to spawn owns every commit in the run: the
/// end-to-end suite caught a t1 commit authored by `w-t2@swarm.local` because
/// w-t2's worktree was set up second. The integration gate then refused the
/// branch, correctly, for a commit its own worker had not authored.
///
/// The extension is written to the common config — that is where git reads it
/// from — and is a one-time flag per repository, so setting it from a worktree is
/// the intended way to do this.
pub fn set_identity(git: &dyn Git, path: &Path, member: &str) -> Result<(), String> {
    let (name, email) = identity(member);
    git.run(&["config", "extensions.worktreeConfig", "true"], path)?;
    git.run(&["config", "--worktree", "user.name", &name], path)?;
    git.run(&["config", "--worktree", "user.email", &email], path)?;
    Ok(())
}

/// Every author email in `base..tip`, merge commits excluded.
///
/// Merge commits are excluded because `merge_deps` puts dependency merges in
/// this range: without `--no-merges` a dependent task fails on its
/// dependencies' merges, which is a false refusal of work it did not do.
pub fn authors(git: &dyn Git, repo: &Path, base: &str, tip: &str) -> Result<Vec<String>, String> {
    let out = git.run(
        &["log", "--no-merges", "--format=%ae", &format!("{base}..{tip}")],
        repo,
    )?;
    Ok(out
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

/// Create a detached worktree, for a verifier.
///
/// Detached because a verifier must not be able to commit: it reads the change
/// and runs the test, and a branch would give it somewhere to write. It is not
/// *enforced* — nothing stops a process from writing to a detached worktree —
/// so the engine detects a dirty base instead of claiming it prevented one.
pub fn add_detached(git: &dyn Git, repo: &Path, path: &Path, base: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let path_str = path.to_string_lossy().to_string();
    git.run(&["worktree", "add", "--detach", &path_str, base], repo)
        .map(|_| ())
}

/// Make sure a detached checkout exists at `base`, and is clean.
///
/// `add_detached` fails when the path already exists, and two callers legitimately
/// want the same checkout: the plan gate makes the run's base tree to prove a
/// criterion against, and `approve` makes it again to prove the same thing from
/// the panel. The second call failed with "already exists", which the engine
/// reported as "no base checkout, so failing-first cannot be proven" — a false
/// alarm on a run whose base tree was right there, and one that made a working
/// run look broken. One owner for the question "is this checkout ready", so the
/// two answers cannot disagree.
///
/// A checkout at the wrong commit is worse than no checkout: it would prove a
/// criterion against a tree the run is not starting from. So the commit is
/// checked, not assumed, and a stale tree is replaced rather than reused.
pub fn ensure_detached(git: &dyn Git, repo: &Path, path: &Path, base: &str) -> Result<(), String> {
    if path.is_dir() {
        let wanted = rev_parse(git, repo, base);
        let at = head(git, path);
        if let (Ok(wanted), Ok(at)) = (wanted, at) {
            if wanted == at {
                return reset_checkout(git, path);
            }
        }
        remove_worktree(git, repo, path)?;
    }
    add_detached(git, repo, path, base)
}

/// Remove a directory, retrying briefly.
///
/// Windows refuses to delete a directory a process still holds open, and the
/// handle outlives the process by a moment: `bridge.stop()` returns when the
/// child has been signalled, not when the OS has finished tearing it down. The
/// first version of the release ran at the `Done` transition, before the member
/// processes were stopped at all, and every run left its directories behind with
/// a board note saying they were in use. Five attempts over a second covers the
/// gap and is short enough that a genuinely stuck directory still reports.
fn remove_dir_retrying(path: &Path) -> std::io::Result<()> {
    let mut last = Ok(());
    for attempt in 0..5u32 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            // Already gone is the outcome that was wanted.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                last = Err(error);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
    last
}

/// Remove a worktree and its directory.
///
/// `--force` because a worker may have left uncommitted files, and a cleanup
/// that refuses to clean up is not a cleanup. The branch is kept: it is the
/// evidence of what the task did, and the run's record points at it.
pub fn remove_worktree(git: &dyn Git, repo: &Path, path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().to_string();
    let result = git.run(&["worktree", "remove", "--force", &path_str], repo);
    // A worktree git has already forgotten still has a directory on disk.
    if path.exists() {
        remove_dir_retrying(path)
            .map_err(|error| format!("could not remove {}: {error}", path.display()))?;
    }
    result.map(|_| ())
}

/// Remove a directory that is not a worktree.
///
/// `discard_worktree` removes the registration and the contents, but on Windows
/// a process that still holds the directory open leaves it behind empty: the
/// children delete and the final `RemoveDirectory` fails. Git has already
/// forgotten the path, so `git worktree remove` fails on it and there is nothing
/// to unregister — only a directory to delete. `remove_worktree` cannot serve
/// here because it reports git's failure even when the directory is gone.
pub fn remove_stale_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    remove_dir_retrying(path)
        .map_err(|error| format!("could not remove {}: {error}", path.display()))
}

/// Delete a branch. Used when a run is deleted, never during a run.
pub fn delete_branch(git: &dyn Git, repo: &Path, branch: &str) -> Result<(), String> {
    git.run(&["branch", "-D", branch], repo).map(|_| ())
}

/// Throw away a worktree that never got as far as a worker.
///
/// `prepare_worker` short-circuits on a directory that already exists — it
/// returns the base commit it was given the first time, so a retry does not move
/// the proof's base forward. That is right for a retry and wrong for a task that
/// failed to start: its worktree exists, its dependencies were never merged, and
/// the short-circuit would hand the worker a tree with none of its dependency's
/// files in it. The branch has to go too, because `worktree add -b` refuses a
/// branch that already exists.
///
/// Best effort on both halves. A directory that will not delete is a directory
/// the next attempt will reuse, which is worse than a failed cleanup but not
/// worse than a run that cannot continue.
pub fn discard_worktree(git: &dyn Git, repo: &Path, path: &Path, branch: &str) {
    let _ = remove_worktree(git, repo, path);
    let _ = delete_branch(git, repo, branch);
}

/// Remove every checkout a run created, keeping every branch.
///
/// Nothing did this. A run created a base checkout, a base checkout per task, a
/// worktree per worker and a detached worktree per verifier, and then left all
/// of them registered in the repository forever: fourteen runs of three small
/// projects had accumulated fourteen run directories under `~/.vibe/worktrees`,
/// each a full checkout, and `git worktree list` had grown to the point where it
/// was no longer readable. The leak is invisible while a run is interesting and
/// obvious once the disk fills.
///
/// The branches stay. They are the record of what each task did, the run's
/// document points at them, and a merged branch costs a few kilobytes where a
/// checkout costs the whole tree. Only the checkouts go.
///
/// Best effort, and it says what it could not remove rather than pretending:
/// a checkout held open by a process that has not exited yet is a real outcome
/// on Windows, and the next run's `worktree add` will fail on that path.
pub fn release_run(git: &dyn Git, repo: &Path, run_id: &str) -> Vec<String> {
    release_run_at(git, repo, &worktree_root().join(run_id))
}

fn release_run_at(git: &dyn Git, repo: &Path, root: &Path) -> Vec<String> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut problems = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                paths.push(entry.path());
            }
        }
    }
    // Deepest first: a nested checkout inside another would otherwise be
    // removed twice, and the second removal reports a missing directory.
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in paths {
        if let Err(error) = remove_worktree(git, repo, &path) {
            problems.push(error);
        }
    }
    // The run directory itself, now empty. `remove_worktree` prunes the git
    // registration; this is the parent that held them.
    let _ = std::fs::remove_dir(&root);
    problems
}

/// How many non-terminal runs keep their checkouts.
pub const RETAINED_RUNS: usize = 10;

/// How long a checkout directory whose run cannot be found is left alone.
///
/// A run that is starting has a checkout before it has a `run.json`, so an
/// unresolvable directory is not proof that nothing owns it. An hour is longer
/// than any start-up, and shorter than the retention the user would notice.
pub const ORPHAN_GRACE_SECS: u64 = 60 * 60;

/// Remove the checkout directories under `root` of every run that is over, and of
/// every live run past the `RETAINED_RUNS` most recent.
///
/// Three cases, and the old code had two of them wrong.
///
/// A **finished** run's leftover checkouts are removed. `Done` releases its own
/// as it stops, so a directory still there belongs to a cleanup that failed — the
/// audited `swarm_stop` left four worktrees because `git worktree remove --force`
/// failed on three of them while the killed workers still held the directories.
///
/// A run that ended any other way — cancelled, failed, escalated, interrupted —
/// keeps its checkouts, because they are the only place its half-finished work
/// exists, and the user can still discard them explicitly. Those are retained
/// newest-first along with the live runs, capped at [`RETAINED_RUNS`].
///
/// An **unresolvable** directory is removed once it is older than
/// [`ORPHAN_GRACE_SECS`]. This was the leak: `run_info` returned `None` for a run
/// whose `run.json` was gone — deleted by hand, or never written — and the loop
/// `continue`d, so those directories were never reaped and never counted. The
/// machine had 120 of them against a retention of 10.
///
/// Returns the run ids whose directories are gone — decided by what is on disk,
/// not by git's verdict, because `remove_worktree` reports git's failure even when
/// it then removed the directory anyway.
fn reap_at(
    git: &dyn Git,
    root: &Path,
    run_info: &dyn Fn(&str) -> Option<(super::RunState, PathBuf)>,
    grace: std::time::Duration,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let mut live: Vec<(String, std::time::SystemTime, PathBuf)> = Vec::new();
    // `None` is a directory with no run behind it: there is no workspace to run
    // git in, so it is deleted without unregistering anything. Git forgets it on
    // the next `git worktree prune` in whichever repository registered it, and a
    // stale registration costs a line in `git worktree list` — cheaper than
    // guessing a repository and pruning someone else's.
    let mut reapable: Vec<(String, Option<PathBuf>)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let at = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match run_info(&id) {
            // Finished: nothing will read these again.
            Some((super::RunState::Done, workspace)) => reapable.push((id, Some(workspace))),
            // Everything else keeps its checkouts, live or not: a cancelled run's
            // worktree is the only copy of the work its worker had done.
            Some((_, workspace)) => live.push((id, at, workspace)),
            None => {
                // No run, so no one can be driving it — but check the age rather
                // than trusting that: a run writes its directory before its
                // `run.json`, and reaping a checkout mid-start would kill it.
                let age = now.duration_since(at).unwrap_or_default();
                if age > grace {
                    reapable.push((id, None));
                }
            }
        }
    }
    // Most recent first, ties broken by id so the choice is deterministic;
    // everything past the retained count is reaped.
    live.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    reapable.extend(
        live.into_iter()
            .skip(RETAINED_RUNS)
            .map(|(id, _, workspace)| (id, Some(workspace))),
    );
    let mut removed = Vec::new();
    for (id, workspace) in reapable {
        match workspace {
            Some(workspace) => {
                let _ = release_run_at(git, &workspace, &root.join(&id));
            }
            None => {
                let _ = remove_stale_dir(&root.join(&id));
            }
        }
        if !root.join(&id).exists() {
            removed.push(id);
        }
    }
    removed
}

/// Bounded retention for the checkouts of runs that did not finish.
///
/// A failed or interrupted run keeps its checkouts on purpose — they are the
/// only place the half-finished work exists — but "keeps them" used to mean
/// "forever": 105 directories under `~/.vibe/worktrees`. Keep the ten most
/// recent, reap the rest, and return what was reaped so the caller can post it.
pub fn reap_checkouts(git: &dyn Git) -> Vec<String> {
    reap_at(
        git,
        &worktree_root(),
        &|id: &str| {
            super::Run::load(id)
                .ok()
                .map(|run| (run.state, PathBuf::from(run.workspace)))
        },
        std::time::Duration::from_secs(ORPHAN_GRACE_SECS),
    )
}

/// Every path a worker committed on its own branch, since `base`.
///
/// `--no-merges` is the whole point: `merge_deps` merges the dependency branches
/// into the task's branch, so a plain diff would report every dependency's files
/// as the worker's work and the grant check would fire on correct behaviour.
/// What is left is the commits the worker itself made.
///
/// One git call, not one per commit. The output is a commit header line per
/// commit and a path per line under it; `--format=` empties the header, so the
/// non-empty lines are exactly the paths.
pub fn changed_paths(git: &dyn Git, path: &Path, base: &str) -> Result<Vec<String>, String> {
    let range = format!("{base}..HEAD");
    let out = git.run(
        &["log", "--no-merges", "--name-only", "--format=", &range],
        path,
    )?;
    let mut seen: Vec<String> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() || seen.iter().any(|s| s == line) {
            continue;
        }
        seen.push(line.to_string());
    }
    Ok(seen)
}

/// How many non-merge commits a branch holds since `base`.
///
/// The number that separates "working" from "produced nothing". Merge commits are
/// excluded because `prepare_worker` merges the dependency branches into the
/// task's own, and those are not the worker's work.
pub fn commit_count(git: &dyn Git, path: &Path, base: &str) -> Result<u32, String> {
    let range = format!("{base}..HEAD");
    let out = git.run(&["rev-list", "--count", "--no-merges", &range], path)?;
    out.trim()
        .parse::<u32>()
        .map_err(|error| format!("git reported {out:?} commits: {error}"))
}

/// When the branch's last commit was written, as an RFC 3339 stamp.
///
/// Author date rather than committer date: a rebase rewrites the second, and the
/// question this answers is when the worker did the work.
pub fn last_commit_at(git: &dyn Git, path: &Path, base: &str) -> Result<String, String> {
    let range = format!("{base}..HEAD");
    let out = git.run(&["log", "-1", "--no-merges", "--format=%aI", &range], path)?;
    let out = out.trim().to_string();
    if out.is_empty() {
        return Err("the branch holds no commits".into());
    }
    Ok(out)
}

/// The paths a worker committed that the plan did not grant it.
///
/// Pure, so the rule is testable without a repository. A grant is a promise the
/// engine makes to every other task in the plan: "this file is yours, nobody
/// else will touch it". A worker that writes outside it breaks that promise for
/// a task that has not run yet, and the breakage surfaces as an unmergeable
/// branch minutes later, in a task that did nothing wrong.
pub fn outside_grant(changed: &[String], granted: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|path| !granted.iter().any(|g| g == *path))
        .cloned()
        .collect()
}

/// The granted paths a branch never touched.
///
/// The other half of the grant check. [`outside_grant`] catches a worker that
/// wrote where it was not allowed; this catches the one that did not write what
/// it was asked for. The audited run's t2 is why: granted `index.html`,
/// `src/style.css` and `tools/t/shell.test.mjs`, w-t2 committed
/// `tools/browser.mjs` and `tools/check-module.mjs` instead — two files outside
/// the grant and none of the three deliverables. The engine noted the breach and
/// handed the branch to a verifier anyway, and the verifier responded by writing
/// the three missing files herself and committing them onto the worker's branch.
/// A judge asked to rule on a deliverable that is not there will supply it.
pub fn untouched(granted: &[String], changed: &[String]) -> Vec<String> {
    granted
        .iter()
        .filter(|path| !changed.iter().any(|c| c == *path))
        .cloned()
        .collect()
}

/// Whether a branch contains none of the paths the plan granted the task.
///
/// The refusal rule at handoff, in one place and testable without a repository.
/// A worker that touched none of its grant has not delivered the task, whatever
/// else is on the branch — and that is a question the engine can answer itself,
/// so it must not spend a verifier turn asking it.
pub fn delivered_nothing(granted: &[String], changed: &[String]) -> bool {
    !granted.is_empty() && untouched(granted, changed).len() == granted.len()
}

/// Whether a worktree has uncommitted changes.
///
/// This is how a dirtied base is detected. `git status --porcelain` prints one
/// line per changed path and nothing when clean, so an empty result is the
/// answer rather than a parse.
pub fn is_dirty(git: &dyn Git, path: &Path) -> Result<bool, String> {
    git.run(&["status", "--porcelain"], path)
        .map(|out| !out.trim().is_empty())
}

/// The outcome of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Clean,
    /// The files git could not reconcile.
    Conflict { files: Vec<String> },
}

/// Merge a task's branch into the integration branch.
///
/// `--no-ff` so every task is one commit on the integration branch: a
/// fast-forward would erase the boundary between tasks, and the whole point of
/// the integration branch is that the user can see which task broke what.
pub fn merge(git: &dyn Git, repo: &Path, branch: &str) -> Result<MergeOutcome, String> {
    match git.run(&["merge", "--no-ff", "--no-edit", branch], repo) {
        Ok(_) => Ok(MergeOutcome::Clean),
        Err(error) => {
            // A failed merge is either a conflict or something else. Ask git
            // which, rather than guessing from the message.
            let conflicted = git
                .run(&["diff", "--name-only", "--diff-filter=U"], repo)
                .unwrap_or_default();
            let files: Vec<String> = conflicted
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            if files.is_empty() {
                Err(error)
            } else {
                Ok(MergeOutcome::Conflict { files })
            }
        }
    }
}

/// Abort a merge that conflicted, so the integration branch is usable again.
pub fn abort_merge(git: &dyn Git, repo: &Path) -> Result<(), String> {
    git.run(&["merge", "--abort"], repo).map(|_| ())
}

/// Create the integration branch at `base`.
pub fn create_branch(git: &dyn Git, repo: &Path, branch: &str, base: &str) -> Result<(), String> {
    git.run(&["branch", "-f", branch, base], repo).map(|_| ())
}

/// Check out a branch in the main checkout.
pub fn checkout(git: &dyn Git, repo: &Path, branch: &str) -> Result<(), String> {
    git.run(&["checkout", branch], repo).map(|_| ())
}

/// The current branch, or `None` when detached.
pub fn current_branch(git: &dyn Git, repo: &Path) -> Option<String> {
    git.run(&["rev-parse", "--abbrev-ref", "HEAD"], repo)
        .ok()
        .map(|out| out.trim().to_string())
        .filter(|name| !name.is_empty() && name != "HEAD")
}

/// Whether `git` is on `PATH` at all.
pub fn git_available() -> bool {
    crate::paths::find_program(&["git"]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A git that records what it was asked and answers from a script.
    struct FakeGit {
        calls: Mutex<Vec<String>>,
        responses: Mutex<Vec<(String, Result<String, String>)>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(Vec::new()),
            }
        }

        fn on(&self, prefix: &str, result: Result<&str, &str>) -> &Self {
            self.responses.lock().unwrap().push((
                prefix.to_string(),
                result.map(str::to_string).map_err(str::to_string),
            ));
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Git for FakeGit {
        fn run(&self, args: &[&str], _cwd: &Path) -> Result<String, String> {
            let joined = args.join(" ");
            self.calls.lock().unwrap().push(joined.clone());
            let mut responses = self.responses.lock().unwrap();
            if let Some(index) = responses
                .iter()
                .position(|(prefix, _)| joined.starts_with(prefix.as_str()))
            {
                return responses.remove(index).1;
            }
            Ok(String::new())
        }
    }

    fn repo() -> PathBuf {
        PathBuf::from("/tmp/repo")
    }

    #[test]
    fn a_repo_is_detected_by_rev_parse() {
        let git = FakeGit::new();
        git.on("rev-parse --git-dir", Ok(".git"));
        assert!(is_repo(&git, &repo()));

        let bare = FakeGit::new();
        bare.on("rev-parse --git-dir", Err("not a git repository"));
        assert!(!is_repo(&bare, &repo()));
    }

    #[test]
    fn ensure_repo_leaves_a_ready_repository_alone() {
        let git = FakeGit::new();
        git.on("rev-parse --git-dir", Ok(".git"));
        git.on("rev-parse HEAD", Ok("abc123"));
        assert_eq!(ensure_repo(&git, &repo()).unwrap(), RepoPrep::Ready);
        let calls = git.calls();
        assert!(!calls.iter().any(|c| c.starts_with("init")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("commit")), "{calls:?}");
    }

    #[test]
    fn ensure_repo_inits_a_plain_folder_and_adds_an_empty_root_commit() {
        let git = FakeGit::new();
        git.on("rev-parse --git-dir", Err("not a git repository"));
        git.on("init", Ok("Initialized empty Git repository"));
        git.on("rev-parse HEAD", Err("ambiguous argument 'HEAD'"));
        git.on("-c user.name=VibeDesk", Ok("[main (root-commit) def456] vibedesk"));
        assert_eq!(
            ensure_repo(&git, &repo()).unwrap(),
            RepoPrep::InitializedWithRootCommit
        );
        let calls = git.calls();
        assert!(calls.iter().any(|c| c == "init"), "{calls:?}");
        let commit = calls
            .iter()
            .find(|c| c.contains("commit"))
            .expect("an empty root commit");
        assert!(commit.contains("--allow-empty"), "{commit}");
        assert!(commit.contains("-c user.name="), "{commit}");
    }

    #[test]
    fn ensure_repo_adds_a_root_commit_to_a_repository_without_commits() {
        let git = FakeGit::new();
        git.on("rev-parse --git-dir", Ok(".git"));
        git.on("rev-parse HEAD", Err("ambiguous argument 'HEAD'"));
        git.on("-c user.name=VibeDesk", Ok("[main (root-commit) def456] vibedesk"));
        assert_eq!(
            ensure_repo(&git, &repo()).unwrap(),
            RepoPrep::RootCommitAdded
        );
        let calls = git.calls();
        assert!(!calls.iter().any(|c| c == "init"), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("--allow-empty")), "{calls:?}");
    }

    #[test]
    fn ensure_repo_reports_a_failed_init_and_never_commits_user_files() {
        let git = FakeGit::new();
        git.on("rev-parse --git-dir", Err("not a git repository"));
        git.on("init", Err("permission denied"));
        let error = ensure_repo(&git, &repo()).unwrap_err();
        assert!(error.contains("git init failed"), "{error}");
        let calls = git.calls();
        assert!(!calls.iter().any(|c| c.contains("commit")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("add")), "{calls:?}");
    }

    #[test]
    fn a_worktree_is_created_on_a_new_branch_from_the_base() {
        let git = FakeGit::new();
        add_worktree(
            &git,
            &repo(),
            Path::new("/tmp/wt/w1"),
            "swarm/run-1/t1",
            "abc123",
        )
        .unwrap();
        let calls = git.calls();
        assert!(
            calls[0].starts_with("worktree add -b swarm/run-1/t1"),
            "{}",
            calls[0]
        );
        assert!(calls[0].ends_with("abc123"), "{}", calls[0]);
        // Then the member's own identity, written *per worktree*. The checkout's
        // directory name is the member id, so a commit made in this worktree is
        // attributable: the audited run's verifier commit `e18875b` was
        // indistinguishable from its worker's, because every worktree committed
        // under the machine-wide identity.
        //
        // `extensions.worktreeConfig` comes first because without it `git config`
        // in a linked worktree edits the shared `.git/config` and every member
        // inherits whoever was set up last.
        assert_eq!(
            calls[1..].to_vec(),
            vec![
                "config extensions.worktreeConfig true".to_string(),
                "config --worktree user.name swarm-w1".to_string(),
                "config --worktree user.email w1@swarm.local".to_string(),
            ]
        );
    }

    #[test]
    fn a_verifier_worktree_is_detached_so_it_has_no_branch_to_commit_to() {
        let git = FakeGit::new();
        add_detached(&git, &repo(), Path::new("/tmp/wt/v1"), "abc123").unwrap();
        let calls = git.calls();
        assert!(calls[0].starts_with("worktree add --detach"), "{}", calls[0]);
        assert!(!calls[0].contains("-b "), "{}", calls[0]);
    }

    /// A checkout that is already there at the right commit is reset, not added.
    ///
    /// This is the shape that broke a live run: the plan gate made the base tree,
    /// `approve` asked for the same tree, `worktree add` refused with "already
    /// exists", and the engine reported "no base checkout, so failing-first cannot
    /// be proven" about a tree that was sitting right there.
    #[test]
    fn a_checkout_that_is_already_at_the_base_is_reset_not_added_again() {
        let dir = std::env::temp_dir().join(format!("vd-wt-ensure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = FakeGit::new();
        git.on("rev-parse abc123", Ok("abc123"));
        git.on("rev-parse HEAD", Ok("abc123"));

        ensure_detached(&git, &repo(), &dir, "abc123").unwrap();

        let calls = git.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("worktree add")),
            "an existing checkout is not added again: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "checkout -- ."),
            "it is reset instead: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "clean -fd"),
            "and cleaned, because a verifier may have written to it: {calls:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A checkout at the wrong commit is replaced, not reused.
    ///
    /// Reusing it would prove a criterion against a tree the run is not starting
    /// from, which is the one thing a base tree exists to rule out.
    #[test]
    fn a_checkout_at_the_wrong_commit_is_replaced() {
        let dir = std::env::temp_dir().join(format!("vd-wt-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = FakeGit::new();
        git.on("rev-parse abc123", Ok("abc123"));
        git.on("rev-parse HEAD", Ok("def456"));

        ensure_detached(&git, &repo(), &dir, "abc123").unwrap();

        let calls = git.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("worktree remove --force")),
            "the stale tree goes: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("worktree add --detach")),
            "and a fresh one is made: {calls:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_worktree_is_not_dirty() {
        let git = FakeGit::new();
        git.on("status --porcelain", Ok(""));
        assert!(!is_dirty(&git, Path::new("/tmp/wt")).unwrap());
    }
    #[test]
    fn a_worktree_with_changes_is_dirty() {
        let git = FakeGit::new();
        git.on("status --porcelain", Ok(" M src/a.rs\n"));
        assert!(is_dirty(&git, Path::new("/tmp/wt")).unwrap());
    }

    #[test]
    fn a_clean_merge_reports_clean() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Ok("Merge made by the 'ort' strategy."));
        assert_eq!(
            merge(&git, &repo(), "swarm/run-1/t1").unwrap(),
            MergeOutcome::Clean
        );
    }

    #[test]
    fn a_conflicted_merge_names_the_files() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Err("CONFLICT (content): Merge conflict in src/a.rs"));
        git.on("diff --name-only --diff-filter=U", Ok("src/a.rs\nsrc/b.rs\n"));
        let outcome = merge(&git, &repo(), "swarm/run-1/t1").unwrap();
        assert_eq!(
            outcome,
            MergeOutcome::Conflict {
                files: vec!["src/a.rs".into(), "src/b.rs".into()]
            }
        );
    }

    #[test]
    fn a_merge_that_fails_without_conflicts_is_an_error_not_a_conflict() {
        let git = FakeGit::new();
        git.on("merge --no-ff", Err("fatal: not something we can merge"));
        git.on("diff --name-only --diff-filter=U", Ok(""));
        assert!(merge(&git, &repo(), "nope").is_err());
    }

    #[test]
    fn a_branch_name_is_namespaced_by_run_and_task() {
        assert_eq!(branch_name("run-1", "t2"), "swarm/run-1-t2");
        // Two runs of the same plan must not collide.
        assert_ne!(branch_name("run-1", "t1"), branch_name("run-2", "t1"));
        // The integration branch is `swarm/<run>`. A task branch nested under it
        // is a ref that cannot exist, so the separator has to be a dash.
        assert!(
            !branch_name("run-1", "t1").starts_with("swarm/run-1/"),
            "a task branch may not be nested under the integration branch"
        );
    }

    #[test]
    fn a_worktree_path_is_namespaced_by_run_and_member() {
        let path = worktree_path("run-1", "w1");
        assert!(path.ends_with(Path::new("run-1").join("w1")), "{}", path.display());
        assert!(path.starts_with(worktree_root()));
    }

    #[test]
    fn the_base_checkout_is_one_per_run_and_never_a_member_path() {
        let base = base_checkout_path("run-1");
        assert!(base.ends_with(Path::new("run-1").join("base")), "{}", base.display());
        // A member id can never collide with it, because no member is called
        // `base` — the engine names them `lead`, `w-<task>`, `v-<task>`,
        // `reflect`.
        assert_ne!(base, worktree_path("run-1", "w-t1"));
        assert_ne!(base, worktree_path("run-1", "v-t1"));
        assert_ne!(base, worktree_path("run-1", "reflect"));
    }

    #[test]
    fn removing_a_worktree_forces_and_keeps_the_branch() {
        let git = FakeGit::new();
        git.on("worktree remove", Ok(""));
        remove_worktree(&git, &repo(), Path::new("/tmp/does-not-exist")).unwrap();
        let calls = git.calls();
        assert!(calls[0].starts_with("worktree remove --force"), "{}", calls[0]);
        assert!(
            !calls.iter().any(|c| c.starts_with("branch -D")),
            "the branch is the evidence of what the task did"
        );
    }

    /* -------------------------------------------------------------- *
     * The dependency merge, against a real repository
     * -------------------------------------------------------------- */

    /// Run git in a directory, and fail the test with its stderr if it refuses.
    fn git_in(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A plan that grants nothing to anyone, so every conflict is a question.
    fn nobody() -> Ownership<'static> {
        Ownership {
            current: "t9",
            tasks: &[],
        }
    }

    /// A real repo with one base commit.
    fn real_repo(name: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!(
            "vd-deps-{name}-{}",
            crate::swarm::new_run_id()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["config", "user.email", "deps@local"]);
        git_in(&repo, &["config", "user.name", "deps"]);
        git_in(&repo, &["config", "commit.gpgsign", "false"]);
        // A test that compares file contents must not be reading the platform's
        // line endings back out of git.
        git_in(&repo, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("base.txt"), "base
").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-qm", "base"]);
        repo
    }

    /// A branch that has committed nothing counts zero, and its last commit can
    /// be dated.
    ///
    /// This is the difference the audited run could not see: five of eight
    /// worktrees held README.md and .git after thirty minutes while the board said
    /// `working`. `rev-list --count base..HEAD` is zero for that, and one or more
    /// for a worker that has committed something.
    #[test]
    fn commits_ahead_of_base_are_counted_and_dated() {
        let repo = real_repo("progress");
        let git = CommandGit;
        let base = git_in(&repo, &["rev-parse", "HEAD"]);

        // Nothing on top of base: zero commits, and no date to report.
        assert_eq!(commit_count(&git, &repo, &base).unwrap(), 0);
        assert!(last_commit_at(&git, &repo, &base).is_err());

        // Two commits with dates set apart, so "the last one" is identifiable
        // rather than whatever the clock happened to say.
        std::fs::write(repo.join("a.txt"), "a
").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(
            &repo,
            &["commit", "-qm", "one", "--date", "2001-02-03T04:05:06+00:00"],
        );
        assert_eq!(commit_count(&git, &repo, &base).unwrap(), 1);

        std::fs::write(repo.join("b.txt"), "b
").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(
            &repo,
            &["commit", "-qm", "two", "--date", "2002-03-04T05:06:07+00:00"],
        );
        assert_eq!(commit_count(&git, &repo, &base).unwrap(), 2);
        assert_eq!(
            last_commit_at(&git, &repo, &base).unwrap(),
            // git prints a UTC author date as `Z`, not `+00:00`.
            "2002-03-04T05:06:07Z",
            "the date is the latest commit's, not the first"
        );
    }

    #[test]
    fn a_reset_puts_a_dirty_checkout_back_to_its_commit() {
        // The verifier may enter its base checkout, so it may leave something in
        // it. The engine resets before proving; without that a verifier could
        // make a real failure look like a pass by editing the baseline.
        let repo = real_repo("reset");
        let base = git_in(&repo, &["rev-parse", "HEAD"]);
        let path = repo.join("t1-base");
        add_detached(&CommandGit, &repo, &path, &base).unwrap();

        std::fs::write(path.join("base.txt"), "edited by the verifier
").unwrap();
        std::fs::write(path.join("planted.txt"), "a file that was never in the base
").unwrap();
        assert!(is_dirty(&CommandGit, &path).unwrap(), "the checkout is dirty");

        reset_checkout(&CommandGit, &path).unwrap();

        assert!(!is_dirty(&CommandGit, &path).unwrap(), "the checkout is clean again");
        assert_eq!(
            std::fs::read_to_string(path.join("base.txt")).unwrap().trim(),
            "base",
            "the tracked file is back to the commit"
        );
        assert!(
            !path.join("planted.txt").exists(),
            "an untracked file the verifier left is removed"
        );
    }

    #[test]
    fn a_task_with_no_dependencies_starts_from_the_base_commit() {        let repo = real_repo("nodeps");
        let base = git_in(&repo, &["rev-parse", "HEAD"]);
        let path = repo.join("w-t1");
        add_worktree(&CommandGit, &repo, &path, "swarm/r-t1", &base).unwrap();

        let (head, resolved) = merge_deps(&CommandGit, &path, &[], &nobody()).unwrap();
        assert_eq!(head, base, "no dependencies means no merge");
        assert!(resolved.is_empty(), "nothing to resolve");
    }

    #[test]
    fn a_dependent_task_starts_from_its_dependencys_work() {
        // The bug that failed a whole live run: t2's criterion needed t1's
        // package.json, and t2's branch was cut from the base commit, so the
        // criterion could not run at all.
        let repo = real_repo("deps");
        let base = git_in(&repo, &["rev-parse", "HEAD"]);

        // t1 writes a file and commits on its own branch.
        let t1 = repo.join("w-t1");
        add_worktree(&CommandGit, &repo, &t1, "swarm/r-t1", &base).unwrap();
        std::fs::write(t1.join("package.json"), "{\"type\":\"module\"}
").unwrap();
        git_in(&t1, &["add", "-A"]);
        git_in(&t1, &["commit", "-qm", "t1: package.json"]);

        // t2 starts from the base, then merges t1 in.
        let t2 = repo.join("w-t2");
        add_worktree(&CommandGit, &repo, &t2, "swarm/r-t2", &base).unwrap();
        assert!(
            !t2.join("package.json").exists(),
            "the worktree really does start without t1's file"
        );

        let (head, _) = merge_deps(
            &CommandGit,
            &t2,
            &[("t1".to_string(), "swarm/r-t1".to_string())],
            &nobody(),
        )
        .unwrap();

        assert!(
            t2.join("package.json").exists(),
            "a dependent task must be able to run its own criterion"
        );
        assert_ne!(head, base, "the base side of the proof moved with the merge");
        assert_eq!(head, git_in(&t2, &["rev-parse", "HEAD"]));
    }

    #[test]
    fn a_dependent_task_merges_every_dependency_not_just_the_first() {
        let repo = real_repo("many");
        let base = git_in(&repo, &["rev-parse", "HEAD"]);

        for (id, file) in [("t2", "gates.js"), ("t3", "collision.js"), ("t4", "ship.js")] {
            let path = repo.join(format!("w-{id}"));
            add_worktree(&CommandGit, &repo, &path, &format!("swarm/r-{id}"), &base).unwrap();
            std::fs::write(path.join(file), "export {};
").unwrap();
            git_in(&path, &["add", "-A"]);
            git_in(&path, &["commit", "-qm", id]);
        }

        let t5 = repo.join("w-t5");
        add_worktree(&CommandGit, &repo, &t5, "swarm/r-t5", &base).unwrap();
        merge_deps(
            &CommandGit,
            &t5,
            &[
                ("t2".to_string(), "swarm/r-t2".to_string()),
                ("t3".to_string(), "swarm/r-t3".to_string()),
                ("t4".to_string(), "swarm/r-t4".to_string()),
            ],
            &nobody(),
        )
        .unwrap();

        for file in ["gates.js", "collision.js", "ship.js"] {
            assert!(t5.join(file).exists(), "t5 must see {file}");
        }
    }

    #[test]
    fn a_conflicting_dependency_is_named_and_leaves_a_usable_tree() {
        let repo = real_repo("conflict");
        let base = git_in(&repo, &["rev-parse", "HEAD"]);

        let t1 = repo.join("w-t1");
        add_worktree(&CommandGit, &repo, &t1, "swarm/r-t1", &base).unwrap();
        std::fs::write(t1.join("base.txt"), "t1 changed this
").unwrap();
        git_in(&t1, &["add", "-A"]);
        git_in(&t1, &["commit", "-qm", "t1"]);

        let t2 = repo.join("w-t2");
        add_worktree(&CommandGit, &repo, &t2, "swarm/r-t2", &base).unwrap();
        std::fs::write(t2.join("base.txt"), "t2 changed this
").unwrap();
        git_in(&t2, &["add", "-A"]);
        git_in(&t2, &["commit", "-qm", "t2"]);

        let error = merge_deps(
            &CommandGit,
            &t2,
            &[("t1".to_string(), "swarm/r-t1".to_string())],
            &nobody(),
        )
        .unwrap_err();

        assert!(error.contains("t1"), "the error names the dependency: {error}");
        assert!(error.contains("base.txt"), "and the file: {error}");
        assert!(
            !is_dirty(&CommandGit, &t2).unwrap(),
            "an aborted merge leaves a tree the user can look at"
        );
    }

    /// Two dependency branches that both changed `base.txt`, and a third task
    /// that depends on both.
    fn two_conflicting_deps(name: &str) -> (PathBuf, PathBuf) {
        let repo = real_repo(name);
        let base = git_in(&repo, &["rev-parse", "HEAD"]);
        for (id, text) in [("t1", "t1 changed this\n"), ("t2", "t2 changed this\n")] {
            let path = repo.join(format!("w-{id}"));
            add_worktree(&CommandGit, &repo, &path, &format!("swarm/r-{id}"), &base).unwrap();
            std::fs::write(path.join("base.txt"), text).unwrap();
            git_in(&path, &["add", "-A"]);
            git_in(&path, &["commit", "-qm", id]);
        }
        let t3 = repo.join("w-t3");
        add_worktree(&CommandGit, &repo, &t3, "swarm/r-t3", &base).unwrap();
        (repo, t3)
    }

    #[test]
    fn a_conflict_on_a_file_one_dependency_owns_keeps_that_dependencys_version() {
        // The twelfth live run: t1 committed `src/player.js` outside its grant,
        // t2 owned it, and the run died with "no task can make progress" while
        // the lead sat parked. The plan already said who owned the file and
        // nothing read it.
        let (repo, t3) = two_conflicting_deps("owner-wins");
        let owners = vec![
            ("t1".to_string(), vec!["src/gates.js".to_string()]),
            ("t2".to_string(), vec!["base.txt".to_string()]),
            ("t3".to_string(), vec!["src/step.js".to_string()]),
        ];
        let ownership = Ownership {
            current: "t3",
            tasks: &owners,
        };

        let (head, resolved) = merge_deps(
            &CommandGit,
            &t3,
            &[
                ("t1".to_string(), "swarm/r-t1".to_string()),
                ("t2".to_string(), "swarm/r-t2".to_string()),
            ],
            &ownership,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(t3.join("base.txt")).unwrap(),
            "t2 changed this\n",
            "the owner's version wins, and the out-of-grant edit is dropped"
        );
        assert_eq!(
            resolved,
            vec![Resolved {
                file: "base.txt".into(),
                owner: "t2".into()
            }],
            "the decision is reported, not silent"
        );
        assert_eq!(head, git_in(&t3, &["rev-parse", "HEAD"]));
        assert!(
            !is_dirty(&CommandGit, &t3).unwrap(),
            "the merge is committed, so the worker starts from a clean tree"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_conflict_on_a_file_the_task_itself_owns_is_left_for_the_worker() {
        // The lead's lever is `files`: granting the file to the task that has to
        // reconcile it. Without this the amendment would be applied and the
        // conflict would persist, burning the repair budget on nothing.
        let (repo, t3) = two_conflicting_deps("current-owns");
        let owners = vec![
            ("t1".to_string(), vec!["src/gates.js".to_string()]),
            ("t2".to_string(), vec!["src/movement.js".to_string()]),
            ("t3".to_string(), vec!["base.txt".to_string()]),
        ];
        let ownership = Ownership {
            current: "t3",
            tasks: &owners,
        };

        let (_, resolved) = merge_deps(
            &CommandGit,
            &t3,
            &[
                ("t1".to_string(), "swarm/r-t1".to_string()),
                ("t2".to_string(), "swarm/r-t2".to_string()),
            ],
            &ownership,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(t3.join("base.txt")).unwrap(),
            "t2 changed this\n",
            "the incoming dependency's version is the starting point"
        );
        assert_eq!(
            resolved,
            vec![Resolved {
                file: "base.txt".into(),
                owner: "t3".into()
            }],
            "and the task is told the reconciliation is its work"
        );
        assert!(!is_dirty(&CommandGit, &t3).unwrap());
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_conflict_on_a_file_nobody_owns_is_still_a_question() {
        let (repo, t3) = two_conflicting_deps("nobody-owns");
        let owners = vec![
            ("t1".to_string(), vec!["src/gates.js".to_string()]),
            ("t2".to_string(), vec!["src/movement.js".to_string()]),
            ("t3".to_string(), vec!["src/step.js".to_string()]),
        ];
        let ownership = Ownership {
            current: "t3",
            tasks: &owners,
        };

        let error = merge_deps(
            &CommandGit,
            &t3,
            &[
                ("t1".to_string(), "swarm/r-t1".to_string()),
                ("t2".to_string(), "swarm/r-t2".to_string()),
            ],
            &ownership,
        )
        .unwrap_err();

        assert!(error.contains("base.txt"), "the file is named: {error}");
        assert!(
            error.contains("no task owns"),
            "and so is the reason it could not be resolved: {error}"
        );
        assert!(
            !is_dirty(&CommandGit, &t3).unwrap(),
            "an aborted merge leaves a tree the user can look at"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_task_base_checkout_is_per_task_not_per_run() {        let one = task_base_path("run-1", "t2");
        let two = task_base_path("run-1", "t5");
        assert_ne!(one, two, "each task is proven against its own base");
        assert_ne!(one, base_checkout_path("run-1"));
    }

    #[test]
    fn a_detached_head_has_no_current_branch() {
        let git = FakeGit::new();
        git.on("rev-parse --abbrev-ref HEAD", Ok("HEAD\n"));
        assert_eq!(current_branch(&git, &repo()), None);

        let on_branch = FakeGit::new();
        on_branch.on("rev-parse --abbrev-ref HEAD", Ok("main\n"));
        assert_eq!(current_branch(&on_branch, &repo()), Some("main".into()));
    }

    #[test]
    fn a_path_outside_the_grant_is_named() {
        // The twelfth live run's t1 was granted three files and committed four
        // more. Nothing checked, the verifier called it "non-blocking", and the
        // collision surfaced as an unmergeable branch in a task that had done
        // nothing wrong.
        let changed = vec![
            "src/logic/gates.js".to_string(),
            "src/main.js".to_string(),
            "src/player.js".to_string(),
        ];
        let granted = vec!["src/logic/gates.js".to_string()];
        assert_eq!(
            outside_grant(&changed, &granted),
            vec!["src/main.js".to_string(), "src/player.js".to_string()]
        );
    }

    #[test]
    fn a_worker_that_stayed_inside_its_grant_has_nothing_to_answer_for() {
        let changed = vec!["src/logic/gates.js".to_string()];
        let granted = vec![
            "src/logic/gates.js".to_string(),
            "test/gates.test.js".to_string(),
        ];
        assert!(outside_grant(&changed, &granted).is_empty());
    }

    #[test]
    fn a_worker_that_committed_nothing_has_nothing_to_answer_for() {
        assert!(outside_grant(&[], &["src/a.js".to_string()]).is_empty());
    }

    #[test]
    fn a_branch_that_touched_none_of_the_grant_delivered_nothing() {
        // The audited run's t2: granted index.html, src/style.css and
        // tools/t/shell.test.mjs; w-t2 committed tools/browser.mjs and
        // tools/check-module.mjs. Two files outside the grant, zero of three
        // deliverables — and the engine handed it to a verifier, who wrote the
        // three missing files herself and committed them onto the worker's
        // branch.
        let granted = vec![
            "index.html".to_string(),
            "src/style.css".to_string(),
            "tools/t/shell.test.mjs".to_string(),
        ];
        let changed = vec![
            "tools/browser.mjs".to_string(),
            "tools/check-module.mjs".to_string(),
        ];
        assert!(delivered_nothing(&granted, &changed));
        assert_eq!(
            untouched(&granted, &changed),
            granted,
            "every granted path is named, so the retry can spell them out"
        );
    }

    #[test]
    fn a_branch_that_touched_one_granted_file_is_judged_not_refused() {
        // Partial delivery is the verifier's question, not the engine's: the
        // plan may grant a file the task turns out not to need, and a refusal on
        // one untouched path would fail correct work.
        let granted = vec!["index.html".to_string(), "src/style.css".to_string()];
        let changed = vec!["index.html".to_string(), "tools/browser.mjs".to_string()];
        assert!(!delivered_nothing(&granted, &changed));
        assert_eq!(untouched(&granted, &changed), vec!["src/style.css".to_string()]);
    }

    #[test]
    fn a_task_with_no_grant_is_never_refused_for_delivering_nothing() {
        // Without a grant there is nothing to miss, and `untouched([], [])` is
        // `[]` — which is "nothing missed" and "nothing granted" at once.
        assert!(!delivered_nothing(&[], &[]));
        assert!(!delivered_nothing(&[], &["src/a.js".to_string()]));
    }

    #[test]
    fn the_paths_a_worker_committed_skip_the_dependency_merges() {
        // `prepare_worker` merges the dependency branches into the task's own
        // branch, so a plain diff would report every dependency's files as the
        // worker's work and the grant check would fire on correct behaviour.
        // `--no-merges` is what makes the list the worker's own commits.
        let git = FakeGit::new();
        git.on(
            "log --no-merges --name-only --format= base..HEAD",
            Ok("\nsrc/logic/gates.js\n\ntest/gates.test.js\nsrc/logic/gates.js\n"),
        );
        let paths = changed_paths(&git, &repo(), "base").unwrap();
        assert_eq!(
            paths,
            vec![
                "src/logic/gates.js".to_string(),
                "test/gates.test.js".to_string()
            ],
            "deduplicated, and the blank header lines dropped"
        );
        assert!(
            git.calls().iter().any(|c| c.contains("--no-merges")),
            "a merge commit's files are the dependency's, not the worker's"
        );
    }

    #[test]
    fn a_worktree_that_never_got_a_worker_is_discarded_with_its_branch() {
        // `prepare_worker` short-circuits on a directory that exists, so a task
        // that failed to start must not leave one behind: the retry would get a
        // tree with none of its dependencies merged in, and `worktree add -b`
        // refuses a branch that already exists.
        let repo_dir = real_repo("discard");
        let base = git_in(&repo_dir, &["rev-parse", "HEAD"]);
        let path = repo_dir.join("w-t3");
        add_worktree(&CommandGit, &repo_dir, &path, "swarm/r-t3", &base).unwrap();
        assert!(path.is_dir());

        discard_worktree(&CommandGit, &repo_dir, &path, "swarm/r-t3");

        assert!(!path.exists(), "the directory is gone");
        let branches = git_in(&repo_dir, &["branch", "--list", "swarm/r-t3"]);
        assert!(
            branches.trim().is_empty(),
            "the branch is gone too, or the retry cannot create it: {branches}"
        );

        // And the retry really can start again.
        add_worktree(&CommandGit, &repo_dir, &path, "swarm/r-t3", &base).unwrap();
        assert!(path.is_dir());
    }

    /// A finished run must not leave its checkouts behind.
    ///
    /// Nothing removed them. Fourteen runs of three small projects left fourteen
    /// run directories under `~/.vibe/worktrees`, each a full checkout, and
    /// `git worktree list` grew until it stopped being readable. The branches
    /// stay — they are the record of what each task did — but the checkouts are
    /// dead weight the moment the work is merged.
    #[test]
    fn a_finished_run_releases_every_checkout_and_keeps_every_branch() {
        let repo_dir = real_repo("release");
        let base = git_in(&repo_dir, &["rev-parse", "HEAD"]);
        let run_id = crate::swarm::new_run_id();

        // The shape a real run creates: a base checkout, a base checkout per
        // task, a worker worktree and a detached verifier worktree.
        let base_checkout = base_checkout_path(&run_id);
        add_detached(&CommandGit, &repo_dir, &base_checkout, &base).unwrap();
        let task_base = task_base_path(&run_id, "t1");
        add_detached(&CommandGit, &repo_dir, &task_base, &base).unwrap();
        let worker = worktree_path(&run_id, "w-t1");
        add_worktree(&CommandGit, &repo_dir, &worker, &branch_name(&run_id, "t1"), &base).unwrap();
        let verifier = worktree_path(&run_id, "v-t1");
        add_detached(&CommandGit, &repo_dir, &verifier, &base).unwrap();

        let problems = release_run(&CommandGit, &repo_dir, &run_id);

        assert!(problems.is_empty(), "cleanup reported problems: {problems:?}");
        for path in [&base_checkout, &task_base, &worker, &verifier] {
            assert!(!path.exists(), "{} should be gone", path.display());
        }
        assert!(
            !worktree_root().join(&run_id).exists(),
            "the run's directory should be gone too"
        );
        let branches = git_in(&repo_dir, &["branch", "--list", &branch_name(&run_id, "t1")]);
        assert!(
            !branches.trim().is_empty(),
            "the branch is the record of what the task did and must survive"
        );
        let listed = git_in(&repo_dir, &["worktree", "list"]);
        assert!(
            !listed.contains(&run_id),
            "git must have forgotten the checkouts: {listed}"
        );
    }

    /// A run that never created a checkout is not an error.
    #[test]
    fn releasing_a_run_that_never_started_is_a_no_op() {
        let repo_dir = real_repo("release-empty");
        let problems = release_run(&CommandGit, &repo_dir, &crate::swarm::new_run_id());
        assert!(problems.is_empty());
    }

    /// Eleven failed runs leave ten directories: retention keeps the ten most
    /// recent non-terminal runs and reaps the rest. The reaped id is reported
    /// so the engine can post it.
    #[test]
    fn eleven_failed_runs_leave_ten_directories() {
        let repo_dir = real_repo("reap");
        let root = std::env::temp_dir().join(format!("vd-reap-{}", crate::swarm::new_run_id()));
        let mut ids = Vec::new();
        for i in 0..(RETAINED_RUNS + 1) {
            let id = format!("run-reap-{i:02}");
            let checkout = root.join(&id).join("w-t1");
            std::fs::create_dir_all(&checkout).unwrap();
            std::fs::write(checkout.join("work.txt"), "x").unwrap();
            ids.push(id);
        }
        // A failed run keeps its checkouts like a live one, so retention applies.
        let info = |_id: &str| Some((super::super::RunState::Failed, repo_dir.clone()));
        let removed = reap_at(&CommandGit, &root, &info, std::time::Duration::from_secs(ORPHAN_GRACE_SECS));

        assert_eq!(removed.len(), 1, "exactly one run is past the retained count");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            RETAINED_RUNS,
            "ten directories remain"
        );
        assert!(
            !root.join(&removed[0]).exists(),
            "the reaped run's directory is gone"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&repo_dir).ok();
    }

    /// A terminal run's leftover checkouts are reaped, because "its own cleanup"
    /// is the thing that failed.
    ///
    /// The audited `swarm_stop` left four worktrees behind: `git worktree remove
    /// --force` failed on three of them because the stopped workers still held the
    /// directories, and the state directory was then deleted by hand — which made
    /// those directories unresolvable as well as abandoned. A run that stops
    /// cleanly releases its own checkouts, so a directory still there at reap time
    /// belongs to a stop that did not finish.
    #[test]
    fn a_terminal_runs_leftover_checkouts_are_reaped() {
        let repo_dir = real_repo("reap-terminal");
        let root = std::env::temp_dir().join(format!("vd-reapt-{}", crate::swarm::new_run_id()));
        for i in 0..(RETAINED_RUNS + 1) {
            let id = format!("run-done-{i:02}");
            std::fs::create_dir_all(root.join(&id).join("w-t1")).unwrap();
        }
        let info = |_id: &str| Some((super::super::RunState::Done, repo_dir.clone()));
        let removed = reap_at(
            &CommandGit,
            &root,
            &info,
            std::time::Duration::from_secs(ORPHAN_GRACE_SECS),
        );
        assert_eq!(
            removed.len(),
            RETAINED_RUNS + 1,
            "every leftover of a finished run is removed, not just the excess"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&repo_dir).ok();
    }

    /// A checkout directory with no run behind it is reaped once it is old
    /// enough — and left alone until then.
    ///
    /// This is the leak that put 120 directories under `~/.vibe/worktrees`
    /// against a retention of 10. The old loop asked `run_info` for the run,
    /// got nothing for a directory whose `run.json` had been deleted, and
    /// `continue`d — so those directories were never counted and never reaped.
    #[test]
    fn a_checkout_with_no_run_behind_it_is_reaped_after_the_grace_period() {
        let root = std::env::temp_dir().join(format!("vd-orphan-{}", crate::swarm::new_run_id()));
        for id in ["run-orphan-one", "run-orphan-two"] {
            std::fs::create_dir_all(root.join(id).join("w-t1")).unwrap();
        }
        let no_run = |_id: &str| None;

        // Inside the grace period nothing is touched: a run writes its checkout
        // before it writes `run.json`, and reaping a start would kill it.
        let kept = reap_at(
            &CommandGit,
            &root,
            &no_run,
            std::time::Duration::from_secs(ORPHAN_GRACE_SECS),
        );
        assert!(kept.is_empty(), "a fresh directory may still belong to a start");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);

        let reaped = reap_at(&CommandGit, &root, &no_run, std::time::Duration::ZERO);
        assert_eq!(reaped.len(), 2);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

        std::fs::remove_dir_all(&root).ok();
    }
}
