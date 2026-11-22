//! Who answers the agent when nobody is watching.
//!
//! A member runs as its own agent (`vibedesk-swarm-*`, written by `provision`),
//! whose `safety` is `neutral` or `safe` rather than `yolo`. Vibe therefore
//! derives `ToolGate.PROMPT` for it, so the first command it runs raises
//! `session/request_permission` and the bridge waits `INTERACTION_TIMEOUT` —
//! fifteen minutes — for a human to click. Nobody is watching a swarm member;
//! that is the whole point of a swarm. Every member turn therefore stalled for
//! fifteen minutes and was then answered `cancelled`, which is why no run ever
//! finished: the worker was not thinking, it was waiting for a click that could
//! not come.
//!
//! This module is the missing answerer. It sits between a member's bridge and
//! the real event sink, decides from the role and from the *effects* the agent
//! declared, answers immediately, and records the decision on the board and in
//! the run's `permissions.jsonl` so the user can audit what was allowed on their
//! behalf.
//!
//! It is also why a member does not need `smart-approve`. Vibe derives its
//! classifier gate from the agent's *name*, so a custom agent cannot inherit it;
//! this gate is stricter (it denies a verifier any write) and costs no model
//! call per tool.
//!
//! ## What a request actually contains
//!
//! Not a tool kind. `vibe-acp` sends a `toolCall` with only an id, and puts the
//! real question in `options[]._meta.required_permissions`: a list of effects,
//! each either a command pattern (`{"scope":"command_pattern","label":"node *",
//! "invocation_pattern":"node --test"}`) or a path outside the worktree
//! (`{"scope":"outside_directory","path_scope_root":"C:\\...","label":"outside
//! workdir (...)"}`). A request is all-or-nothing: allowing it allows every
//! effect in the list, so one denied effect denies the request.
//!
//! ## The policy
//!
//! | effect | lead | worker | verifier | reflector |
//! | --- | --- | --- | --- | --- |
//! | a command that is not destructive | allow | allow | allow | allow |
//! | a destructive command (`git push`, `rm -rf`, `git reset --hard`, …) | deny | deny | deny | deny |
//! | a path inside the member's own worktree | allow | allow | allow | allow |
//! | a path inside the run directory (its inbox, the board) | allow | allow | allow | allow |
//! | a path inside the workspace repository | deny | deny | deny | deny |
//! | another member's worktree, or a base checkout | deny | deny | deny | deny |
//! | a file write, when the agent reports a tool kind | allow | allow | deny | deny |
//!
//! The two denies that are not about safety are about the adversarial guarantee.
//! A verifier that may edit is a verifier that can repair what it is judging and
//! then report a pass — the verdict would describe a tree the verifier itself
//! changed. A reflector that may edit is a reflector that changes the repository
//! it is supposed to be learning about. Both write through their inbox instead.
//!
//! The lead is allowed to write on purpose. Its brief says "you do not
//! implement", but that is an instruction to the model, not a boundary the
//! engine should enforce: the lead is the user's proxy in the run and the only
//! member with the whole picture, and a lead that is refused a write has no way
//! to say why. A lead that oversteps is visible — its worktree is the workspace
//! and the engine detects a dirty base. A verifier that oversteps is invisible.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use crate::acp::bridge::{AcpBridge, EventSink, EVENT_REQUEST, EVENT_USAGE};
use crate::swarm::board::{BoardEntry, BoardKind};
use crate::swarm::{Role, Usage};

/// What a role may do without a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Deny => "deny",
        }
    }

    /// The ACP option-kind prefix that expresses this verdict.
    fn prefix(self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Deny => "reject",
        }
    }
}

/// One thing a request would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A command the member wants to run. `pattern` is the real invocation,
    /// `label` is the glob the agent shows the user.
    Command { pattern: String, label: String },
    /// A path outside the member's worktree.
    Outside { root: String, label: String },
    /// A file write, when the agent reports a tool kind rather than effects.
    Write { kind: String, title: String },
    /// A call into an MCP server the user registered.
    ///
    /// ACP gives this request no metadata at all — no tool kind, no
    /// `required_permissions`, not even a title — so the only way to know what
    /// it is comes from the `tool_call` announcement that precedes it. See
    /// [`mcp_tool`].
    Mcp { server: String, tool: String },
    /// Something this module cannot reason about.
    Unknown,
}

impl Effect {
    /// What the board calls it.
    pub fn label(&self) -> String {
        match self {
            Effect::Command { label, .. } => format!("command {label}"),
            Effect::Outside { label, .. } => label.clone(),
            Effect::Write { kind, title } if title.is_empty() => format!("{kind}"),
            Effect::Write { kind, title } => format!("{kind} ({title})"),
            Effect::Mcp { server, tool } => format!("{server}.{tool}"),
            Effect::Unknown => "an effect it did not describe".into(),
        }
    }
}

/// The server and tool an MCP tool call names, from its announced title.
///
/// `vibe-acp` names an MCP tool `mcp_<server>.<tool>` — observed live as
/// `mcp_codebase_memory.list_projects` and `mcp_donsetch.web_search`. The
/// permission request for that call carries only a `toolCallId`, so this title,
/// taken from the `tool_call` notification that precedes the request, is the
/// only description of the call that exists anywhere in the protocol.
///
/// `None` for anything that is not an MCP call, which leaves the request
/// exactly as undescribed as it was before.
pub fn mcp_tool(title: &str) -> Option<(String, String)> {
    let rest = title.trim().strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('.')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// Where a member is allowed to be.
///
/// Derived from the run rather than passed in, so a member spawned before the
/// run was saved still gets the right answer.
#[derive(Debug, Clone)]
pub struct Scope {
    /// The member's own worktree.
    pub worktree: PathBuf,
    /// The run's directory: the inbox, the board, the receipts.
    pub run_dir: PathBuf,
    /// The repository the run is working on. Nobody but the engine merges here.
    pub workspace: PathBuf,
    /// Every worktree the run owns. A member may only touch its own.
    pub worktree_root: PathBuf,
    /// Paths this member may enter that the rules below would otherwise refuse.
    ///
    /// Exactly one thing lives here: a verifier's base checkout. The verifier's
    /// brief tells it to prove the criterion fails on the base commit, and the
    /// base checkout is inside `worktree_root`, so without this the gate refuses
    /// the `cd` and the failing-first proof becomes an instruction the member is
    /// not allowed to follow. The fifth live run failed a task on exactly that:
    /// the verifier was refused `cd .../base`, could not run the criterion on
    /// the base, and reported `failed` on a change that passed.
    pub readable: Vec<PathBuf>,
}

impl Scope {
    pub fn new(worktree: PathBuf, run_dir: PathBuf, workspace: PathBuf) -> Self {
        Self {
            worktree,
            run_dir,
            workspace,
            worktree_root: crate::swarm::worktree::worktree_root(),
            readable: Vec::new(),
        }
    }

    /// Paths this member may enter even though they are inside the run's
    /// worktree root.
    pub fn readable(mut self, paths: Vec<PathBuf>) -> Self {
        self.readable = paths;
        self
    }
}

/// Commands a swarm member must never run, and why.
///
/// Short and named on purpose. A long list would be a policy nobody reads, and a
/// member that is refused an ordinary command wastes a turn discovering it. Each
/// entry is a needle matched against the lowercased invocation.
const DESTRUCTIVE: &[(&str, &str)] = &[
    ("git push", "a swarm member must never push"),
    ("git reset --hard", "it would discard work the run is measuring"),
    ("git clean -f", "it would delete untracked work"),
    ("rm -rf", "it would delete a tree"),
    ("rm -fr", "it would delete a tree"),
    ("rmdir /s", "it would delete a tree"),
    ("del /f", "it would delete files"),
    ("del /s", "it would delete files"),
    ("format ", "it would format a volume"),
    ("shutdown", "it would stop the machine"),
];

/// The invocation with every quoted span blanked out: the command and its
/// unquoted arguments. Destructive needles match against this, so the words a
/// member *prints* are not the words it *runs*.
fn unquoted(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None => out.push(ch),
        }
    }
    out
}

/// Why a command is refused, if it is.
///
/// The match is against the command and its unquoted arguments, not the whole
/// string: `echo 'rm -rf /'` prints the words, `rm -rf /` runs them.
pub fn destructive(pattern: &str) -> Option<&'static str> {
    let lowered = unquoted(pattern).to_ascii_lowercase();
    DESTRUCTIVE
        .iter()
        .find(|(needle, _)| lowered.contains(needle))
        .map(|(_, why)| *why)
}

/// Whether `path` is `root` or inside it.
///
/// Lexical, not canonical: the path may not exist yet, and a member asking about
/// a directory it is about to create is the normal case. Windows paths arrive
/// with either separator and either case.
fn is_inside(path: &str, root: &Path) -> bool {
    let normalise = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let path = normalise(path);
    let root = normalise(&root.to_string_lossy());
    if root.is_empty() {
        return false;
    }
    path == root || path.starts_with(&format!("{root}/"))
}

/// Every path in a command's text that starts with `root`.
///
/// A command carries its paths as arguments, and the gate's path rules only see
/// paths the agent *declared*. This is the only place an undeclared one is
/// visible.
fn named_paths(command: &str, root: &Path) -> Vec<String> {
    let normalise = |value: &str| value.replace('\\', "/").to_ascii_lowercase();
    let hay = normalise(command);
    let needle = normalise(&root.to_string_lossy()).trim_end_matches('/').to_string();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay[from..].find(&needle) {
        let start = from + at;
        let rest = &hay[start..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | ';' | '&' | '|' | ')' | '(' | ',')
            })
            .unwrap_or(rest.len());
        out.push(rest[..end].to_string());
        from = start + end.max(1);
    }
    out
}

/// The forbidden root a command's text names, if any.
///
/// The sixth live run's w-t1 committed straight onto the integration branch with
/// `git -C <workspace> commit`. The request declared only `command git *`, so the
/// gate saw no path at all and allowed it — the workspace rule never ran. A
/// command that names a path declares nothing, and scanning the invocation is the
/// only way to see it.
///
/// The rule is the same one the declared-path rule uses, not a stricter one: the
/// workspace is refused, another member's tree is refused, the member's own tree
/// and anything it was granted are allowed.
fn names_a_forbidden_root(command: &str, scope: &Scope) -> Option<String> {
    for path in named_paths(command, &scope.workspace) {
        if is_inside(&path, &scope.workspace) {
            return Some(scope.workspace.to_string_lossy().to_string());
        }
    }
    for path in named_paths(command, &scope.worktree_root) {
        if is_inside(&path, &scope.worktree) {
            continue;
        }
        if scope.readable.iter().any(|granted| is_inside(&path, granted)) {
            continue;
        }
        return Some(path);
    }
    None
}

/// The effects a request would have.
pub fn effects(params: &Value) -> Vec<Effect> {
    // An agent that reports a tool kind is answering the question directly.
    let kind = params
        .pointer("/toolCall/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if matches!(kind, "edit" | "delete" | "move") {
        return vec![Effect::Write {
            kind: kind.to_string(),
            title: params
                .pointer("/toolCall/title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }];
    }

    // Otherwise the effects are in an option's metadata — but not necessarily
    // the first one. `vibe-acp` puts `_meta` on the *second* option (the
    // session-scoped allow) and leaves the plain `allow_once` bare, so reading
    // index 0 found nothing and every request looked undescribed. The second
    // live run refused a verifier three times for that reason alone.
    let mut out = Vec::new();
    let required = params
        .get("options")
        .and_then(|v| v.as_array())
        .and_then(|options| {
            options
                .iter()
                .find_map(|option| option.pointer("/_meta/required_permissions"))
        })
        .and_then(|v| v.as_array());
    for entry in required.into_iter().flatten() {
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match entry.get("scope").and_then(|v| v.as_str()).unwrap_or("") {
            "command_pattern" => out.push(Effect::Command {
                pattern: entry
                    .get("invocation_pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                label,
            }),
            "outside_directory" => out.push(Effect::Outside {
                root: entry
                    .get("path_scope_root")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                label,
            }),
            _ => out.push(Effect::Unknown),
        }
    }

    if out.is_empty() {
        out.push(Effect::Unknown);
    }
    out
}

/// The policy table, for one effect.
pub fn verdict(role: Role, effect: &Effect, scope: &Scope) -> Verdict {
    match effect {
        // A verifier that may edit can repair what it is judging.
        Effect::Write { .. } => match role {
            Role::Verifier | Role::Reflector => Verdict::Deny,
            Role::Lead | Role::Worker => Verdict::Allow,
        },

        Effect::Command { pattern, .. } => {
            if destructive(pattern).is_some() {
                return Verdict::Deny;
            }
            if names_a_forbidden_root(pattern, scope).is_some() {
                return Verdict::Deny;
            }
            Verdict::Allow
        }

        Effect::Outside { root, .. } => {
            // Its own worktree, its inbox, the board: the places a member is
            // supposed to be.
            if is_inside(root, &scope.worktree) || is_inside(root, &scope.run_dir) {
                return Verdict::Allow;
            }
            // The workspace is the integration branch. Only the engine merges
            // there, and a member that writes to it corrupts the run's result.
            if is_inside(root, &scope.workspace) {
                return Verdict::Deny;
            }
            // A path this member was explicitly given. The verifier's base
            // checkout is the only one, and it is given because the proof the
            // brief demands cannot be run anywhere else.
            if scope.readable.iter().any(|path| is_inside(root, path)) {
                return Verdict::Allow;
            }
            // Another member's worktree, or a base checkout it was not given. A
            // verifier that can write to the baseline can change what it is
            // measuring.
            if is_inside(root, &scope.worktree_root) {
                return Verdict::Deny;
            }
            // A scratch directory of its own, a system path it needs to read.
            Verdict::Allow
        }

        // A call into an MCP server the user registered.
        //
        // Allowed for every role, including the two that refuse unknown effects.
        // The gate's jurisdiction is file and command effects: where a member
        // writes, what it runs, which checkout it can reach. An MCP call has
        // none of those — it does whatever its server does, and the server is
        // the user's own registration. Refusing it for a verifier did not
        // protect the repository; it removed the code graph and web search from
        // the one role whose job is checking a change against the codebase.
        //
        // The call is still recorded, so "the verifier queried the code graph"
        // is on the board rather than invisible.
        Effect::Mcp { .. } => Verdict::Allow,

        // An effect the agent did not describe. Conservative for the roles whose
        // whole value is that they do not change things.
        Effect::Unknown => match role {
            Role::Verifier | Role::Reflector => Verdict::Deny,
            Role::Lead | Role::Worker => Verdict::Allow,
        },
    }
}

/// One answered request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    /// Every effect the request would have had.
    pub effects: Vec<Effect>,
    /// The option id to answer with.
    ///
    /// `None` means "answer cancelled", which is the only honest answer when the
    /// agent offers no option of the wanted kind — and still an answer, unlike a
    /// request left hanging for fifteen minutes.
    pub option: Option<String>,
    /// Why, when the answer is a refusal.
    pub reason: Option<String>,
}

impl Decision {
    /// The line the board carries.
    pub fn sentence(&self) -> String {
        let what = self
            .effects
            .iter()
            .map(Effect::label)
            .collect::<Vec<_>>()
            .join(", ");
        match self.verdict {
            Verdict::Allow => format!("allowed {what}"),
            Verdict::Deny => match &self.reason {
                Some(reason) => format!("refused {what} — {reason}"),
                None => format!("refused {what}"),
            },
        }
    }
}

/// Pick the option id that expresses `verdict`, by kind rather than by id.
///
/// The ids belong to the agent and differ between agents; the kinds are the one
/// thing ACP fixes. `allow_once` is preferred over `allow_always` because a
/// member that is allowed forever stops asking, and the board would then be
/// missing exactly the decisions the user audits.
pub fn choose(verdict: Verdict, options: &Value) -> Option<String> {
    let options = options.as_array()?;
    fn kind_of(option: &Value) -> &str {
        option.get("kind").and_then(|k| k.as_str()).unwrap_or("")
    }
    fn id_of(option: &Value) -> Option<String> {
        option
            .get("optionId")
            .and_then(|id| id.as_str())
            .map(str::to_string)
    }

    let wanted = verdict.prefix();
    let once = format!("{wanted}_once");

    let found = options
        .iter()
        .find(|option| kind_of(option) == once)
        .or_else(|| options.iter().find(|option| kind_of(option).starts_with(wanted)));

    // A deny never falls back to an allow. If the agent offers no reject, the
    // request is answered `cancelled`, which the agent reads as a refusal.
    found.and_then(id_of)
}

/// Decide one permission request.
///
/// A request is all-or-nothing, so one denied effect denies the request. The
/// reason names the first refusal, because that is the one the member has to
/// work around.
pub fn decide(role: Role, params: &Value, scope: &Scope) -> Decision {
    decide_announced(role, params, scope, None)
}

/// The same decision, told what the agent announced it was about to call.
///
/// `announced` is the title from the `tool_call` notification that preceded the
/// request. It exists because an MCP tool call is the one request ACP cannot
/// describe: the permission request carries a `toolCallId` and nothing else, so
/// without this every MCP call reads as `Effect::Unknown` and is refused for the
/// two roles that refuse unknown effects. That refusal protected nothing — an
/// MCP call has no file or command effect for the gate to judge — and it made
/// the code graph and web capabilities unreachable from a verifier.
pub fn decide_announced(
    role: Role,
    params: &Value,
    scope: &Scope,
    announced: Option<&str>,
) -> Decision {
    let mut effects = effects(params);
    // Only when the request itself said nothing. A request that describes its
    // effects is judged on them, whatever the announcement claimed.
    if effects == [Effect::Unknown] {
        if let Some((server, tool)) = announced.and_then(mcp_tool) {
            effects = vec![Effect::Mcp { server, tool }];
        }
    }
    let mut verdict = Verdict::Allow;
    let mut reason = None;
    for effect in &effects {
        if self::verdict(role, effect, scope) == Verdict::Deny {
            verdict = Verdict::Deny;
            reason = Some(match effect {
                Effect::Command { pattern, .. } => {
                    if let Some(why) = destructive(pattern) {
                        why.to_string()
                    } else if let Some(root) = names_a_forbidden_root(pattern, scope) {
                        // Name the path. A member refused with "this role may not
                        // run that" has no idea which argument was the problem,
                        // and the next thing it does is try a different spelling
                        // of the same command.
                        if is_inside(&root, &scope.workspace) {
                            format!(
                                "that command names the integration branch ({root}) — only the engine merges there"
                            )
                        } else {
                            format!("that command names {root}, which is not yours")
                        }
                    } else {
                        "this role may not run that".into()
                    }
                }
                Effect::Outside { root, .. } => {
                    if is_inside(root, &scope.workspace) {
                        "the workspace is the integration branch — only the engine merges there"
                            .into()
                    } else if is_inside(root, &scope.worktree_root) {
                        "that is another member's worktree, or a base checkout".into()
                    } else {
                        "this role may not write there".into()
                    }
                }
                Effect::Write { .. } => match role {
                    Role::Verifier => "a verifier that edits can repair what it is judging".into(),
                    Role::Reflector => "a reflector writes to its inbox, not the repository".into(),
                    _ => "this role may not write here".into(),
                },
                Effect::Mcp { server, tool } => {
                    format!("this role may not call {server}.{tool}")
                }
                Effect::Unknown => "the request did not say what it would do".into(),
            });
            break;
        }
    }

    // Fail-closed is unchanged; only the message moves. A member that cannot
    // tell which effect was refused retries the whole batch with one change and
    // loses a round trip each time, so a denial names what was fine.
    if let Some(text) = reason.as_mut() {
        let allowed = effects
            .iter()
            .filter(|effect| self::verdict(role, effect, scope) == Verdict::Allow)
            .count();
        if allowed > 0 {
            *text = format!(
                "{text} — the other {allowed} effect{} in this request {} acceptable",
                if allowed == 1 { "" } else { "s" },
                if allowed == 1 { "was" } else { "were" }
            );
        }
    }

    let option = choose(verdict, params.get("options").unwrap_or(&Value::Null));
    Decision {
        verdict,
        effects,
        option,
        reason,
    }
}

/// A request this gate answered, kept so a test can read it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub request_id: String,
    pub decision: Decision,
}

/// The sink a member's bridge emits through.
///
/// It forwards everything to the real sink and answers the two requests that
/// would otherwise wait for a human: a permission request and an elicitation.
/// A member has no human, so an elicitation is answered `cancel` — the briefs
/// tell members to speak through their inbox, and a form nobody fills in is a
/// turn that never ends.
pub struct Gate {
    inner: Arc<dyn EventSink>,
    member: String,
    role: Role,
    /// The member's own worktree. The rest of the scope is read from the run at
    /// answer time, so a gate built before the run was saved still works.
    worktree: PathBuf,
    /// Paths this member may enter that the default rules refuse. See
    /// `Scope::readable`.
    readable: Vec<PathBuf>,
    /// The watch that saw this member's tool call announcements.
    ///
    /// Optional because a gate can be built without one — the tests do, and a
    /// gate with no watch answers exactly as it did before the watch existed.
    /// When it is there, it is the only source for what a permission request is
    /// about: the request itself carries a tool call id and nothing else.
    watch: Option<Arc<super::watch::MemberWatch>>,
    /// Weak on purpose: the bridge holds this gate as its sink, so a strong
    /// handle here would be a cycle and every member's process handle would
    /// outlive the run that spawned it.
    bridge: Mutex<Option<Weak<AcpBridge>>>,
    answered: Mutex<Vec<Answered>>,
    /// One board line per (verdict, effect label), not one per command run.
    announced: Mutex<HashSet<String>>,
    /// What this member has spent so far.
    ///
    /// The gate owns the write side because it is the only object that sees
    /// every event its member's bridge emits, and the drive loop owns the read
    /// side because it is the only place that saves the run. A shared handle is
    /// the seam: the gate accumulates without touching the run, and the loop
    /// copies the totals onto the member record on its next tick.
    usage: Arc<Mutex<Usage>>,
}

impl Gate {
    pub fn new(
        inner: Arc<dyn EventSink>,
        member: impl Into<String>,
        role: Role,
        worktree: PathBuf,
    ) -> Self {
        Self {
            inner,
            member: member.into(),
            role,
            worktree,
            readable: Vec::new(),
            watch: None,
            bridge: Mutex::new(None),
            answered: Mutex::new(Vec::new()),
            announced: Mutex::new(HashSet::new()),
            usage: Arc::new(Mutex::new(Usage::default())),
        }
    }

    /// The handle the drive loop reads this member's spend from.
    pub fn usage(&self) -> Arc<Mutex<Usage>> {
        self.usage.clone()
    }

    /// Paths this member may enter even though they are inside the run's
    /// worktree root. A verifier's base checkout is the only caller.
    pub fn readable(mut self, paths: Vec<PathBuf>) -> Self {
        self.readable = paths;
        self
    }

    /// Hand the gate the watch that sees this member's announcements.
    ///
    /// Without it the gate cannot tell an MCP call from a request that describes
    /// nothing, and refuses both for a verifier.
    pub fn watching(mut self, watch: Arc<super::watch::MemberWatch>) -> Self {
        self.watch = Some(watch);
        self
    }

    /// Write into an existing ledger instead of starting a new one.
    ///
    /// A respawn is a new session, not a new member. The run's ledger is keyed by
    /// member for the life of the run, so the new gate accumulates into what the
    /// previous session left behind rather than replacing it — which is what made
    /// a respawned member's tokens fall from 1,189,646 to 461,743 on the
    /// cancelled run, and the run's total with them.
    pub fn continuing(mut self, usage: Arc<Mutex<Usage>>) -> Self {
        self.usage = usage;
        self
    }

    /// Hand the gate the bridge it answers through.
    ///
    /// Called immediately after the bridge is built and before it is started, so
    /// there is no window in which a request can arrive with nowhere to go.
    pub fn attach(&self, bridge: &Arc<AcpBridge>) {
        *self.bridge.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(bridge));
    }

    pub fn member(&self) -> &str {
        &self.member
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Every request this gate answered, in order.
    pub fn answered(&self) -> Vec<Answered> {
        self.answered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Fold a usage reading into this member's spend.
    ///
    /// Every reading is a cost, not only the `final` one. `observe` is
    /// delta-based — it keeps the last cumulative reading per counter and adds
    /// the difference — so folding the same reading twice adds nothing and
    /// folding a live one early only moves the total forward sooner. Waiting for
    /// `final` is what made a verifier that had been working for thirteen
    /// minutes report zero tokens: the only reading that counted was the one at
    /// the end of a turn that had not ended.
    ///
    /// The gauge is the other way round: only the live reading carries one. The
    /// end-of-turn reading has no window size, and treating its cumulative total
    /// as a gauge is what made the panel report a run at 3.6M of a zero-token
    /// window.
    ///
    /// `finish_turn` stays behind `final`, because a turn is not finished until
    /// it is: counting turns and wall clock from a live reading would report a
    /// turn per notification.
    fn record_usage(&self, payload: &Value) {
        let count = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if payload.get("gauge").and_then(Value::as_bool) == Some(true) {
            usage.observe_context(count("contextUsed"), count("contextSize"));
        }
        usage.observe(count("input"), count("output"), count("cached"));
        if payload.get("final").and_then(Value::as_bool) != Some(true) {
            return;
        }
        let secs = payload.get("secs").and_then(Value::as_f64).unwrap_or(0.0);
        usage.finish_turn(secs);
    }

    /// Say on the roster that this member is blocked on a request, or is not.
    ///
    /// The gate is the only object that sees both ends of a request — it arrives
    /// here and the answer leaves here — so it is the only one that can say a
    /// member is `waiting` and then that it is not. It records the fact on the
    /// watch rather than on the run because the drive loop is the only object
    /// that saves the run, and a write from this path would be overwritten by
    /// the loop's next save of a snapshot taken before it.
    fn blocked(&self, blocked: bool) {
        let Some(watch) = self.watch.as_ref() else {
            return;
        };
        if blocked {
            watch.request_opened();
        } else {
            watch.request_answered();
        }
    }

    fn bridge(&self) -> Option<Arc<AcpBridge>> {
        self.bridge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// Where this member is allowed to be, read from the live run.
    fn scope(&self) -> Scope {
        let run = crate::swarm::engine::engine().run();
        let (run_dir, workspace) = match &run {
            Some(run) => (run.dir(), PathBuf::from(&run.workspace)),
            None => (PathBuf::new(), PathBuf::new()),
        };
        Scope::new(self.worktree.clone(), run_dir, workspace).readable(self.readable.clone())
    }

    /// Write the request and the answer to the run's audit trail.
    ///
    /// The board carries one line per effect, which is the right density for
    /// reading. This file carries every request in full, which is the right
    /// density for auditing: an autonomous run that allowed something on the
    /// user's behalf has to be able to show exactly what it was asked and exactly
    /// what it said. It is also the only way to find out what an agent actually
    /// puts in a permission request, since ACP makes most of the tool call
    /// optional and agents differ in what they fill in.
    fn audit(&self, request_id: &str, params: &Value, decision: &Decision) {
        let Some(run) = crate::swarm::engine::engine().run() else {
            return;
        };
        let line = serde_json::json!({
            "at": crate::swarm::now(),
            "member": self.member,
            "role": self.role.label(),
            "requestId": request_id,
            "verdict": decision.verdict.label(),
            "option": decision.option,
            "reason": decision.reason,
            "effects": decision.effects.iter().map(Effect::label).collect::<Vec<_>>(),
            "toolCall": params.get("toolCall").cloned().unwrap_or(Value::Null),
            "options": params.get("options").cloned().unwrap_or(Value::Null),
        });
        let path = run.dir().join("permissions.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }

    /// Answer a permission request, and say so on the board once per effect.
    fn answer_permission(&self, request_id: String, params: Value) {
        // The request names only a tool call id. The watch saw the announcement
        // that carried the name, so ask it — an MCP call is otherwise
        // indistinguishable from a request that describes nothing.
        let announced = params
            .pointer("/toolCall/toolCallId")
            .and_then(Value::as_str)
            .and_then(|id| self.watch.as_ref().and_then(|watch| watch.title_of(id)));
        let decision = decide_announced(
            self.role,
            &params,
            &self.scope(),
            announced.as_deref(),
        );
        self.audit(&request_id, &params, &decision);

        let Some(bridge) = self.bridge() else {
            // Cannot happen: the bridge emits the request, so it exists. Named
            // rather than silent, because a request nobody answers is a turn
            // that stalls for fifteen minutes. The member stays `waiting` for
            // the same reason: nothing has answered it, and the spawn path is
            // what fails a member with no bridge.
            self.inner.emit(
                EVENT_REQUEST,
                serde_json::json!({
                    "unanswered": request_id,
                    "member": self.member,
                    "reason": "the gate has no bridge to answer through",
                }),
            );
            return;
        };

        self.answered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Answered {
                request_id: request_id.clone(),
                decision: decision.clone(),
            });

        let member = self.member.clone();
        let option = decision.option.clone();
        let sentence = decision.sentence();
        let watch = self.watch.clone();
        // Only a refusal goes on the board.
        //
        // The board is the run's debate log, and a grant is not a debate: it is
        // the policy doing what it was written to do. A live run put five
        // "allowed outside workdir (…)" lines on a thirty-one entry board, and
        // because they carry full paths they were the longest lines on it —
        // sixteen percent of the record, all of it routine, crowding out the
        // verdicts the panel exists to show. A refusal is the opposite: it is
        // the one permission event a post-mortem needs, because it is the only
        // one that changed what the run could do.
        let announce = decision.verdict == Verdict::Deny && {
            let key = format!("{}-{}", decision.verdict.label(), sentence);
            let mut announced = self.announced.lock().unwrap_or_else(|e| e.into_inner());
            announced.insert(key)
        };

        tokio::spawn(async move {
            let outcome = bridge.respond_permission(&request_id, option).await;
            // The member is unblocked the moment the answer is on the wire,
            // whether or not the agent took it: a request that timed out on the
            // agent's side is not outstanding any more.
            if let Some(watch) = watch.as_ref() {
                watch.request_answered();
            }
            if let Err(error) = outcome {
                // The request timed out or was answered elsewhere. Worth a line,
                // not worth failing the member over.
                crate::swarm::engine::engine().post(BoardEntry::new(
                    member.clone(),
                    BoardKind::Note,
                    format!("could not answer {request_id}: {error}"),
                ));
                return;
            }
            if announce {
                crate::swarm::engine::engine()
                    .post(BoardEntry::new(member, BoardKind::Note, sentence));
            }
        });
    }

    /// Answer an elicitation with `cancel`.
    ///
    /// A member that needs a human decision writes an `escalation` to its inbox;
    /// the engine reads that and the run stops for the user. A form is not a
    /// channel a member has, so leaving one open would only burn the timeout.
    fn answer_elicitation(&self, request_id: String) {
        let Some(bridge) = self.bridge() else {
            return;
        };
        let member = self.member.clone();
        let watch = self.watch.clone();
        tokio::spawn(async move {
            let _ = bridge
                .respond_elicitation(&request_id, "cancel".to_string(), None)
                .await;
            // Answered, so no longer outstanding — the same moment as a
            // permission, because from the member's side it is the same wait.
            if let Some(watch) = watch.as_ref() {
                watch.request_answered();
            }
            crate::swarm::engine::engine().post(BoardEntry::new(
                member,
                BoardKind::Note,
                "cancelled a form nobody could fill in — members speak through their inbox",
            ));
        });
    }
}

impl EventSink for Gate {
    fn emit(&self, event: &str, payload: Value) {
        // Forward first: the app's Status panel and the headless console both
        // want to see the request even though neither will answer it.
        self.inner.emit(event, payload.clone());

        if event.starts_with(EVENT_USAGE) {
            self.record_usage(&payload);
            return;
        }
        if !event.starts_with(EVENT_REQUEST) {
            return;
        }
        let Some(request_id) = payload
            .get("requestId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return;
        };
        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = payload.get("params").cloned().unwrap_or(Value::Null);

        // The member is blocked from the moment the request arrives until the
        // answer is on the wire. Both moments are in this object, which is why
        // the roster learns about them from the gate rather than from a clock.
        self.blocked(true);
        match method {
            "session/request_permission" => self.answer_permission(request_id, params),
            "elicitation/create" => self.answer_elicitation(request_id),
            // A request this gate does not know how to answer is not one it can
            // hold open: nothing here will ever clear it.
            _ => self.blocked(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::bridge::RecordingSink;
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            worktree: PathBuf::from("C:/Users/x/.vibe/worktrees/run-1/w-t1"),
            run_dir: PathBuf::from("D:/VibeDesk/data/swarm/run-1"),
            workspace: PathBuf::from("D:/VibeDesk/VibeDeskProjects/neon-drift"),
            worktree_root: PathBuf::from("C:/Users/x/.vibe/worktrees"),
            readable: Vec::new(),
        }
    }

    /// The scope a verifier actually gets: its own worktree plus its task's base
    /// checkout, which the engine created for it.
    fn verifier_scope() -> Scope {
        scope().readable(vec![PathBuf::from(
            "C:/Users/x/.vibe/worktrees/run-1/t1-base",
        )])
    }

    /// A request shaped like the ones `vibe-acp` actually sends: no tool kind,
    /// the effects in the first option's metadata.
    fn request(effects: Value) -> Value {
        json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "effect-1" },
            "options": [
                {
                    "optionId": "allow_once",
                    "name": "Allow once",
                    "kind": "allow_once",
                    "_meta": { "required_permissions": effects }
                },
                { "optionId": "allow_always", "name": "Always", "kind": "allow_always" },
                { "optionId": "reject_once", "name": "Reject", "kind": "reject_once" }
            ]
        })
    }

    fn command(pattern: &str) -> Value {
        json!([{
            "invocation_pattern": pattern,
            "label": format!("{} *", pattern.split(' ').next().unwrap_or(pattern)),
            "scope": "command_pattern",
            "session_pattern": format!("{} *", pattern.split(' ').next().unwrap_or(pattern))
        }])
    }

    fn outside(root: &str) -> Value {
        json!([{
            "invocation_pattern": root,
            "label": format!("outside workdir ({root})"),
            "path_scope_root": root,
            "scope": "outside_directory"
        }])
    }

    /* -------------------------------------------------------------- *
     * Commands
     * -------------------------------------------------------------- */

    #[test]
    fn an_ordinary_command_is_allowed_for_every_role() {
        for role in [Role::Lead, Role::Worker, Role::Verifier, Role::Reflector] {
            let decision = decide(role, &request(command("node --test")), &scope());
            assert_eq!(
                decision.verdict,
                Verdict::Allow,
                "{} must be able to run the suite",
                role.label()
            );
            assert_eq!(decision.option.as_deref(), Some("allow_once"));
        }
    }

    #[test]
    fn a_push_is_refused_for_every_role_because_a_swarm_never_pushes() {
        for role in [Role::Lead, Role::Worker, Role::Verifier, Role::Reflector] {
            let decision = decide(role, &request(command("git push origin main")), &scope());
            assert_eq!(decision.verdict, Verdict::Deny, "{}", role.label());
            assert_eq!(decision.option.as_deref(), Some("reject_once"));
            assert!(decision.sentence().contains("never push"), "{}", decision.sentence());
        }
    }

    #[test]
    fn a_destructive_command_is_refused_even_inside_the_members_own_worktree() {
        // The worktree is the member's to change, but not to delete.
        for pattern in [
            "rm -rf node_modules",
            "git reset --hard HEAD~3",
            "git clean -fd",
            "rmdir /s /q build",
            "del /f /q out.txt",
        ] {
            let decision = decide(Role::Worker, &request(command(pattern)), &scope());
            assert_eq!(decision.verdict, Verdict::Deny, "{pattern}");
        }
    }

    #[test]
    fn a_command_that_merely_mentions_a_destructive_word_is_still_refused() {
        // `cd x && rm -rf y` is one invocation. Matching the whole string is the
        // only way to catch it, and the cost of a false refusal is one turn.
        let decision = decide(Role::Worker, &request(command("cd build && rm -rf dist")), &scope());
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_denied_batch_names_the_offending_effect_and_says_the_rest_were_fine() {
        // Run 3's seq 165 refused a batch without saying which effect was the
        // problem. The verdict stays fail-closed; the reason says what was fine.
        let mut batch = command("ls").as_array().unwrap().clone();
        batch.extend(command("rm -rf build").as_array().unwrap().clone());
        let decision = decide(
            Role::Worker,
            &request(serde_json::Value::Array(batch)),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
        let reason = decision.reason.as_deref().unwrap();
        assert!(
            reason.contains("it would delete a tree"),
            "the offending effect is named: {reason}"
        );
        assert!(
            reason.contains("the other 1 effect in this request was acceptable"),
            "the rest of the batch is declared fine: {reason}"
        );
    }

    #[test]
    fn a_destructive_word_inside_a_quoted_argument_is_not_a_command() {
        // Run 3's v-t7 was refused with "it would format a volume" for
        // `echo '=== other verdicts for format ==='`: the needle matched the
        // word inside the echoed text. Both directions, because a fix that only
        // widens the allow is a regression in the other direction.
        let printed = decide(
            Role::Verifier,
            &request(command("echo '=== other verdicts for format ==='")),
            &scope(),
        );
        assert_eq!(printed.verdict, Verdict::Allow, "{}", printed.sentence());

        let grep = decide(
            Role::Verifier,
            &request(command("grep \"rm -rf\" notes.txt")),
            &scope(),
        );
        assert_eq!(grep.verdict, Verdict::Allow, "{}", grep.sentence());

        let refused = decide(Role::Verifier, &request(command("format C:")), &scope());
        assert_eq!(refused.verdict, Verdict::Deny, "format C: is still refused");
        assert!(
            refused.sentence().contains("format a volume"),
            "{}",
            refused.sentence()
        );
    }

    /* -------------------------------------------------------------- *
     * Paths
     * -------------------------------------------------------------- */

    #[test]
    fn a_path_inside_the_members_own_worktree_is_allowed() {
        let decision = decide(
            Role::Worker,
            &request(outside("C:\\Users\\x\\.vibe\\worktrees\\run-1\\w-t1\\src")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn a_path_inside_the_run_directory_is_allowed_because_that_is_how_a_member_speaks() {
        let decision = decide(
            Role::Worker,
            &request(outside("D:\\VibeDesk\\data\\swarm\\run-1\\inbox\\w-t1")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn the_workspace_is_refused_because_only_the_engine_merges_there() {
        for role in [Role::Lead, Role::Worker, Role::Verifier] {
            let decision = decide(
                role,
                &request(outside("D:\\VibeDesk\\VibeDeskProjects\\neon-drift")),
                &scope(),
            );
            assert_eq!(decision.verdict, Verdict::Deny, "{}", role.label());
            assert!(
                decision.sentence().contains("integration branch"),
                "{}",
                decision.sentence()
            );
        }
    }

    #[test]
    fn another_members_worktree_is_refused() {
        let decision = decide(
            Role::Verifier,
            &request(outside("C:\\Users\\x\\.vibe\\worktrees\\run-1\\w-t2")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
        assert!(decision.sentence().contains("another member"), "{}", decision.sentence());
    }

    #[test]
    fn a_base_checkout_it_was_not_given_is_refused() {
        // Another task's base, or a base from a run this member is not on. The
        // grant is per member and per task, not "any base checkout".
        let decision = decide(
            Role::Verifier,
            &request(outside("C:/Users/x/.vibe/worktrees/run-1/t2-base")),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_verifier_may_enter_the_base_checkout_it_was_given() {
        // The brief tells it to prove the criterion fails on the base. Refusing
        // the `cd` made that instruction impossible to follow, and the fifth
        // live run failed a passing change because of it.
        let decision = decide(
            Role::Verifier,
            &request(outside("C:/Users/x/.vibe/worktrees/run-1/t1-base")),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn a_grant_does_not_open_the_rest_of_the_worktree_root() {
        // The grant is a path, not a directory prefix. A verifier given t1-base
        // must still be refused another task's worker tree next door.
        let decision = decide(
            Role::Verifier,
            &request(outside("C:/Users/x/.vibe/worktrees/run-1/w-t2")),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_grant_does_not_open_the_workspace() {
        // The integration branch is refused before the grant is consulted, and
        // stays refused: a verifier that can write there can change the result
        // it is judging.
        let decision = decide(
            Role::Verifier,
            &request(outside("D:/VibeDesk/VibeDeskProjects/neon-drift")),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_worker_is_never_given_a_base_checkout() {
        // Workers get an empty grant. The rule that a worker may not enter
        // another member's tree is unchanged by the verifier's exception.
        let decision = decide(
            Role::Worker,
            &request(outside("C:/Users/x/.vibe/worktrees/run-1/t1-base")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    /* -------------------------------------------------------------- *
     * Commands that name a path instead of declaring one
     * -------------------------------------------------------------- */

    #[test]
    fn a_command_that_names_the_workspace_is_refused() {
        // The sixth live run's w-t1 committed straight onto the integration
        // branch with `git -C <workspace> commit`. The request declared only
        // `command git *`, so the gate saw no path and allowed it. This is the
        // shape that broke isolation, and it is a command, not a path effect.
        let decision = decide(
            Role::Worker,
            &request(command(
                "git -C D:/VibeDesk/VibeDeskProjects/neon-drift commit -m \"wip\"",
            )),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
        assert!(
            decision.sentence().contains("integration branch"),
            "{}",
            decision.sentence()
        );
    }

    #[test]
    fn a_command_that_names_another_members_worktree_is_refused() {
        let decision = decide(
            Role::Worker,
            &request(command(
                "git -C C:/Users/x/.vibe/worktrees/run-1/w-t2 commit -m \"wip\"",
            )),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_command_that_names_its_own_worktree_is_allowed() {
        // The member's own tree is where it is supposed to be. A rule that
        // refused this would refuse every command that names its own cwd.
        let decision = decide(
            Role::Worker,
            &request(command(
                "cd C:/Users/x/.vibe/worktrees/run-1/w-t1 && node --test",
            )),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn a_command_that_names_a_granted_base_checkout_is_allowed() {
        let decision = decide(
            Role::Verifier,
            &request(command(
                "cd C:/Users/x/.vibe/worktrees/run-1/t1-base && node --test",
            )),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn a_command_that_names_a_base_checkout_it_was_not_granted_is_refused() {
        let decision = decide(
            Role::Verifier,
            &request(command(
                "cd C:/Users/x/.vibe/worktrees/run-1/t2-base && node --test",
            )),
            &verifier_scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn an_ordinary_command_with_no_path_is_still_allowed() {
        // The scan must not turn every command into a refusal. `node --test` in
        // the member's own cwd names nothing.
        for text in ["node --test", "npm test", "git status", "git diff HEAD~1"] {
            let decision = decide(Role::Worker, &request(command(text)), &scope());
            assert_eq!(decision.verdict, Verdict::Allow, "{text}");
        }
    }

    #[test]
    fn a_path_that_only_shares_a_prefix_with_the_workspace_is_not_named() {
        // `neon-drift-old` is not `neon-drift`. The scan uses the same boundary
        // rule as the declared-path check.
        let decision = decide(
            Role::Worker,
            &request(command(
                "cd D:/VibeDesk/VibeDeskProjects/neon-drift-old && node --test",
            )),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn the_scan_reads_backslash_paths_too() {
        // A Windows command line is written with backslashes and the workspace
        // is stored with forward slashes. Both have to match.
        let decision = decide(
            Role::Worker,
            &request(command(
                r"git -C D:\VibeDesk\VibeDeskProjects\neon-drift commit -m wip",
            )),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
    }

    #[test]
    fn a_scratch_directory_of_its_own_is_allowed() {
        // Outside every tree the run owns: a temp dir the member made for itself.
        let decision = decide(
            Role::Verifier,
            &request(outside("C:/Users/x/AppData/Local/Temp/vt1-base")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    #[test]
    fn a_path_that_only_shares_a_prefix_with_the_workspace_is_not_inside_it() {
        // `neon-drift-old` is not `neon-drift`. A prefix test without a separator
        // boundary would refuse it, and the member would have no idea why.
        let decision = decide(
            Role::Worker,
            &request(outside("D:\\VibeDesk\\VibeDeskProjects\\neon-drift-old")),
            &scope(),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    /* -------------------------------------------------------------- *
     * Writes, for agents that report a tool kind
     * -------------------------------------------------------------- */

    #[test]
    fn a_verifier_may_run_anything_and_edit_nothing() {
        let write = json!({
            "toolCall": { "kind": "edit", "title": "Write src/main.rs" },
            "options": [
                { "optionId": "a-once", "kind": "allow_once" },
                { "optionId": "r-once", "kind": "reject_once" }
            ]
        });
        let decision = decide(Role::Verifier, &write, &scope());
        assert_eq!(decision.verdict, Verdict::Deny);
        assert_eq!(decision.option.as_deref(), Some("r-once"));
        assert!(decision.sentence().contains("repair what it is judging"));
    }

    #[test]
    fn a_worker_may_write_because_writing_is_its_job() {
        let write = json!({
            "toolCall": { "kind": "edit", "title": "Write src/main.rs" },
            "options": [{ "optionId": "a-once", "kind": "allow_once" }]
        });
        assert_eq!(decide(Role::Worker, &write, &scope()).verdict, Verdict::Allow);
    }

    #[test]
    fn the_lead_may_write_because_it_is_the_users_proxy() {
        let write = json!({
            "toolCall": { "kind": "edit" },
            "options": [{ "optionId": "a-once", "kind": "allow_once" }]
        });
        assert_eq!(decide(Role::Lead, &write, &scope()).verdict, Verdict::Allow);
    }

    /* -------------------------------------------------------------- *
     * Bundles and unknowns
     * -------------------------------------------------------------- */

    #[test]
    fn one_denied_effect_denies_the_whole_request() {
        // A request is all-or-nothing: allowing it allows every effect in it.
        let bundle = json!([
            { "invocation_pattern": "node --test", "label": "node *", "scope": "command_pattern" },
            { "invocation_pattern": "git push", "label": "git *", "scope": "command_pattern" }
        ]);
        let decision = decide(Role::Worker, &request(bundle), &scope());
        assert_eq!(decision.verdict, Verdict::Deny);
        assert_eq!(decision.effects.len(), 2);
    }

    #[test]
    fn a_request_that_describes_nothing_is_refused_for_a_verifier() {
        let bare = json!({
            "toolCall": { "toolCallId": "x" },
            "options": [{ "optionId": "a-once", "kind": "allow_once" }]
        });
        assert_eq!(decide(Role::Verifier, &bare, &scope()).verdict, Verdict::Deny);
        assert_eq!(decide(Role::Worker, &bare, &scope()).verdict, Verdict::Allow);
    }

    /// The exact request `vibe-acp` sends for an MCP tool call: a tool call id
    /// and four options, and nothing else. Captured from a live session.
    fn mcp_request() -> Value {
        json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "effect-eb0bed3b" },
            "options": [
                { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
                { "optionId": "allow_always", "name": "Allow for remainder of this session", "kind": "allow_always" },
                { "optionId": "allow_always_permanent", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "reject_once", "name": "Deny", "kind": "reject_once" }
            ]
        })
    }

    #[test]
    fn an_mcp_tool_call_is_read_from_the_title_the_agent_announced() {
        // Both names were observed live, from two different servers.
        assert_eq!(
            mcp_tool("mcp_codebase_memory.list_projects"),
            Some(("codebase_memory".into(), "list_projects".into()))
        );
        assert_eq!(
            mcp_tool("mcp_donsetch.web_search"),
            Some(("donsetch".into(), "web_search".into()))
        );
        // Not an MCP call: the gate must keep treating these as undescribed.
        assert_eq!(mcp_tool("Searching for relevant tools"), None);
        assert_eq!(mcp_tool("mcp_"), None);
        assert_eq!(mcp_tool("mcp_server"), None);
        assert_eq!(mcp_tool("mcp_.tool"), None);
        assert_eq!(mcp_tool("mcp_server."), None);
    }

    #[test]
    fn a_verifier_may_call_an_mcp_server_the_user_registered() {
        // The bug this pins: every MCP request carries no effects, so it read as
        // `Effect::Unknown` and was refused for a verifier — which removed the
        // code graph and web search from the one role whose job is checking a
        // change against the codebase. The gate's jurisdiction is file and
        // command effects; an MCP call has neither.
        let decision = decide_announced(
            Role::Verifier,
            &mcp_request(),
            &scope(),
            Some("mcp_codebase_memory.list_projects"),
        );
        assert_eq!(decision.verdict, Verdict::Allow);
        assert_eq!(
            decision.effects,
            vec![Effect::Mcp {
                server: "codebase_memory".into(),
                tool: "list_projects".into(),
            }]
        );
        assert_eq!(decision.option.as_deref(), Some("allow_once"));
    }

    #[test]
    fn an_unannounced_request_is_still_undescribed() {
        // The watch is the only source of the name. Without it the gate must not
        // guess, and a verifier keeps the conservative answer.
        let decision = decide_announced(Role::Verifier, &mcp_request(), &scope(), None);
        assert_eq!(decision.verdict, Verdict::Deny);
        assert_eq!(decision.effects, vec![Effect::Unknown]);
    }

    #[test]
    fn a_request_that_describes_its_effects_is_judged_on_them_not_the_announcement() {
        // An announcement cannot launder a write. If the request says what it
        // would do, that is what the gate answers.
        let decision = decide_announced(
            Role::Verifier,
            &request(command("rm -rf build")),
            &scope(),
            Some("mcp_donsetch.web_search"),
        );
        assert_eq!(decision.verdict, Verdict::Deny);
        assert!(
            decision
                .effects
                .iter()
                .all(|effect| !matches!(effect, Effect::Mcp { .. })),
            "the announcement must not replace described effects"
        );
    }

    #[test]
    fn the_board_names_an_mcp_call_rather_than_calling_it_undescribed() {
        assert_eq!(
            Effect::Mcp {
                server: "donsetch".into(),
                tool: "web_search".into(),
            }
            .label(),
            "donsetch.web_search"
        );
    }

    #[test]
    fn the_effects_are_found_on_whichever_option_carries_them() {
        // The exact shape `vibe-acp` sends: the plain allow_once is bare and the
        // session-scoped allow carries the metadata. Reading only the first
        // option made every request look undescribed, and a verifier was refused
        // three times in a row for it.
        let real = json!({
            "sessionId": "s1",
            "toolCall": { "toolCallId": "effect-c76ba39e" },
            "options": [
                { "kind": "allow_once", "name": "Allow once", "optionId": "allow_once" },
                {
                    "kind": "allow_always",
                    "name": "Allow for remainder of this session",
                    "optionId": "allow_always",
                    "_meta": { "required_permissions": [{
                        "invocation_pattern": "git worktree add --detach /c/x/v-t2-base 28e9ed9",
                        "label": "git worktree add --detach /c/x/v-t2-base 28e9ed9",
                        "scope": "command_pattern",
                        "session_pattern": "git worktree add --detach /c/x/v-t2-base 28e9ed9"
                    }]}
                }
            ]
        });
        let decision = decide(Role::Verifier, &real, &scope());
        assert_eq!(
            decision.effects,
            vec![Effect::Command {
                pattern: "git worktree add --detach /c/x/v-t2-base 28e9ed9".into(),
                label: "git worktree add --detach /c/x/v-t2-base 28e9ed9".into(),
            }],
            "the effect must be read, not guessed"
        );
        assert_eq!(decision.verdict, Verdict::Allow);
        assert_eq!(decision.option.as_deref(), Some("allow_once"));
    }

    #[test]
    fn an_unknown_scope_is_named_rather_than_guessed() {
        let odd = json!([{ "scope": "teleport", "label": "somewhere" }]);
        let decision = decide(Role::Worker, &request(odd), &scope());
        assert_eq!(decision.effects, vec![Effect::Unknown]);
        assert_eq!(decision.verdict, Verdict::Allow);
    }

    /* -------------------------------------------------------------- *
     * Option picking
     * -------------------------------------------------------------- */

    #[test]
    fn allow_once_is_preferred_over_allow_always_so_the_board_keeps_the_decisions() {
        let options = json!([
            { "optionId": "always", "kind": "allow_always" },
            { "optionId": "once", "kind": "allow_once" }
        ]);
        assert_eq!(choose(Verdict::Allow, &options).as_deref(), Some("once"));
    }

    #[test]
    fn an_agent_that_offers_only_always_is_still_answerable() {
        let options = json!([{ "optionId": "always", "kind": "allow_always" }]);
        assert_eq!(choose(Verdict::Allow, &options).as_deref(), Some("always"));
    }

    #[test]
    fn a_deny_never_falls_back_to_an_allow() {
        // The one bug that would silently break the adversarial guarantee.
        let options = json!([{ "optionId": "yes", "kind": "allow_once" }]);
        assert_eq!(choose(Verdict::Deny, &options), None);
    }

    #[test]
    fn an_empty_option_list_is_answered_cancelled_rather_than_hanging() {
        assert_eq!(choose(Verdict::Allow, &json!([])), None);
        assert_eq!(choose(Verdict::Allow, &Value::Null), None);
    }

    /* -------------------------------------------------------------- *
     * The gate, against a real bridge
     * -------------------------------------------------------------- */

    #[tokio::test]
    async fn a_permission_request_reaches_the_bridge_with_the_policys_option() {
        // The whole point of the gate: the request is answered, not left to the
        // fifteen-minute timeout. The bridge is real; only the pending
        // interaction is injected, so this proves the wiring end to end.
        let sink = Arc::new(RecordingSink::default());
        let gate = Arc::new(Gate::new(
            sink.clone(),
            "w-t1",
            Role::Worker,
            scope().worktree,
        ));
        let bridge = Arc::new(AcpBridge::with_sink(gate.clone()));
        gate.attach(&bridge);

        let mut reply = bridge.expect_interaction("perm-1").await;
        gate.emit(
            "acp://request/swarm/w-t1",
            json!({
                "requestId": "perm-1",
                "method": "session/request_permission",
                "params": request(command("node --test")),
            }),
        );

        let answer = tokio::time::timeout(std::time::Duration::from_secs(2), &mut reply)
            .await
            .expect("the gate must answer without waiting for a human")
            .expect("the reply channel must not close");
        match answer {
            crate::acp::bridge::InteractionReply::Permission(option) => {
                assert_eq!(option.as_deref(), Some("allow_once"));
            }
            _ => panic!("a permission request must be answered with a permission"),
        }

        let answered = gate.answered();
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].request_id, "perm-1");
        assert_eq!(answered[0].decision.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn a_push_is_refused_through_the_real_bridge() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Arc::new(Gate::new(
            sink,
            "w-t1",
            Role::Worker,
            scope().worktree,
        ));
        let bridge = Arc::new(AcpBridge::with_sink(gate.clone()));
        gate.attach(&bridge);

        let mut reply = bridge.expect_interaction("perm-2").await;
        gate.emit(
            "acp://request/swarm/w-t1",
            json!({
                "requestId": "perm-2",
                "method": "session/request_permission",
                "params": request(command("git push origin main")),
            }),
        );

        let answer = tokio::time::timeout(std::time::Duration::from_secs(2), &mut reply)
            .await
            .expect("a refusal is still an answer")
            .expect("the reply channel must not close");
        match answer {
            crate::acp::bridge::InteractionReply::Permission(option) => {
                assert_eq!(option.as_deref(), Some("reject_once"));
            }
            _ => panic!("a permission request must be answered with a permission"),
        }
    }

    #[tokio::test]
    async fn an_elicitation_is_cancelled_rather_than_left_open() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Arc::new(Gate::new(
            sink,
            "w-t1",
            Role::Worker,
            scope().worktree,
        ));
        let bridge = Arc::new(AcpBridge::with_sink(gate.clone()));
        gate.attach(&bridge);

        let mut reply = bridge.expect_interaction("elicit-1").await;
        gate.emit(
            "acp://request/swarm/w-t1",
            json!({
                "requestId": "elicit-1",
                "method": "elicitation/create",
                "params": { "message": "which database?" },
            }),
        );

        let answer = tokio::time::timeout(std::time::Duration::from_secs(2), &mut reply)
            .await
            .expect("a form nobody can fill in must not burn the timeout")
            .expect("the reply channel must not close");
        match answer {
            crate::acp::bridge::InteractionReply::Elicitation { action, .. } => {
                assert_eq!(action, "cancel");
            }
            _ => panic!("an elicitation must be answered with an elicitation"),
        }
    }

    #[tokio::test]
    async fn a_notification_is_forwarded_and_never_answered() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Arc::new(Gate::new(
            sink.clone(),
            "w-t1",
            Role::Worker,
            scope().worktree,
        ));
        let bridge = Arc::new(AcpBridge::with_sink(gate.clone()));
        gate.attach(&bridge);

        gate.emit(
            "acp://notification/swarm/w-t1",
            json!({ "sessionUpdate": "agent_message_chunk" }),
        );

        assert_eq!(sink.on("acp://notification/swarm/w-t1").len(), 1);
        assert!(gate.answered().is_empty());
    }

    #[tokio::test]
    async fn a_request_with_no_bridge_is_named_rather_than_dropped() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Gate::new(sink.clone(), "w-t1", Role::Worker, scope().worktree);

        gate.emit(
            "acp://request/swarm/w-t1",
            json!({
                "requestId": "perm-9",
                "method": "session/request_permission",
                "params": request(command("node --test")),
            }),
        );

        let named = sink.on(EVENT_REQUEST);
        assert!(
            named.iter().any(|payload| payload.get("unanswered").is_some()),
            "a request the gate cannot answer must be visible, not silent"
        );
    }

    /// The live reading is the only one that knows how full the window is.
    #[tokio::test]
    async fn a_live_reading_sets_the_context_gauge() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Gate::new(sink, "w-t1", Role::Worker, scope().worktree);

        gate.emit(
            EVENT_USAGE,
            json!({
                "final": false,
                "gauge": true,
                "input": 400_000,
                "output": 1_000,
                "cached": 300_000,
                "contextUsed": 416_540,
                "contextSize": 1_048_576,
                "secs": 0.0,
            }),
        );

        let usage = gate.usage();
        let usage = usage.lock().unwrap();
        assert_eq!(usage.context_used, 416_540);
        assert_eq!(usage.context_size, 1_048_576);
        assert_eq!(usage.turns, 0, "a live reading is not a finished turn");
    }

    /// The end-of-turn reading is a cost, and its `totalTokens` is cumulative
    /// spend — not window fill. Folding it into the gauge is what made the panel
    /// report a run at 3.6M of a zero-token window.
    #[tokio::test]
    async fn a_final_reading_does_not_overwrite_the_context_gauge() {        let sink = Arc::new(RecordingSink::default());
        let gate = Gate::new(sink, "w-t1", Role::Worker, scope().worktree);

        gate.emit(
            EVENT_USAGE,
            json!({
                "final": false,
                "gauge": true,
                "contextUsed": 416_540,
                "contextSize": 1_048_576,
            }),
        );
        gate.emit(
            EVENT_USAGE,
            json!({
                "final": true,
                "input": 3_676_558,
                "output": 12_000,
                "cached": 3_000_000,
                "secs": 41.5,
            }),
        );

        let usage = gate.usage();
        let usage = usage.lock().unwrap();
        assert_eq!(
            usage.context_used, 416_540,
            "the cumulative total must not be read as window fill"
        );
        assert_eq!(usage.context_size, 1_048_576);
        assert_eq!(usage.input, 3_676_558, "the cost is still counted");
        assert_eq!(usage.turns, 1);
    }

    /// A member mid-turn has spent tokens, and the roster has to say so before
    /// the turn ends. The live run showed a verifier that had been working for
    /// thirteen minutes reporting zero.
    #[tokio::test]
    async fn a_live_reading_is_a_cost_before_the_turn_ends() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Gate::new(sink, "v-t1", Role::Verifier, scope().worktree);

        gate.emit(
            EVENT_USAGE,
            json!({
                "final": false,
                "gauge": true,
                "input": 400_000,
                "output": 1_000,
                "cached": 300_000,
                "contextUsed": 416_540,
                "contextSize": 1_048_576,
                "secs": 0.0,
            }),
        );

        let usage = gate.usage();
        let usage = usage.lock().unwrap();
        assert_eq!(usage.input, 400_000, "a turn in flight has spent tokens");
        assert_eq!(usage.output, 1_000);
        assert_eq!(usage.cached, 300_000);
        assert_eq!(usage.turns, 0, "but the turn is not finished");
    }

    /// The live readings are cumulative snapshots, so folding several of them in
    /// must not multiply the bill by the number of notifications.
    #[tokio::test]
    async fn repeated_live_readings_are_counted_once() {
        let sink = Arc::new(RecordingSink::default());
        let gate = Gate::new(sink, "w-t1", Role::Worker, scope().worktree);

        for input in [120_000, 240_000, 240_000, 360_000] {
            gate.emit(
                EVENT_USAGE,
                json!({
                    "final": false,
                    "gauge": true,
                    "input": input,
                    "output": 500,
                    "cached": 0,
                    "contextUsed": 1,
                    "contextSize": 2,
                    "secs": 0.0,
                }),
            );
        }

        let usage = gate.usage();
        let usage = usage.lock().unwrap();
        assert_eq!(usage.input, 360_000, "the last cumulative reading, not the sum");
        assert_eq!(usage.output, 500);
        assert_eq!(usage.turns, 0);
    }

    /// A respawn is a new session, not a new member. The ledger is keyed by
    /// member for the life of the run, so the second session adds to the first
    /// rather than replacing it — the cancelled run showed `w-t1` falling from
    /// 1,189,646 input tokens to 461,743.
    #[tokio::test]
    async fn a_respawned_member_keeps_the_spend_of_its_first_session() {
        let sink = Arc::new(RecordingSink::default());
        let ledger = Arc::new(Mutex::new(Usage::default()));

        let first = Gate::new(sink.clone(), "w-t1", Role::Worker, scope().worktree)
            .continuing(ledger.clone());
        first.emit(
            EVENT_USAGE,
            json!({
                "final": true,
                "input": 1_189_646,
                "output": 9_000,
                "cached": 1_000_000,
                "secs": 41.5,
            }),
        );
        drop(first);

        // The process is replaced. The new gate writes into the same ledger.
        let second = Gate::new(sink, "w-t1", Role::Worker, scope().worktree)
            .continuing(ledger.clone());
        second.emit(
            EVENT_USAGE,
            json!({
                "final": true,
                "input": 461_743,
                "output": 3_000,
                "cached": 400_000,
                "secs": 12.0,
            }),
        );

        let usage = ledger.lock().unwrap();
        assert_eq!(
            usage.input,
            1_189_646 + 461_743,
            "a new session adds to the ledger, it does not replace it"
        );
        assert_eq!(usage.output, 9_000 + 3_000);
        assert_eq!(usage.turns, 2);
    }

    /// The gate is the only object that sees both ends of a request, so it is the
    /// only one that can say a member is blocked and then that it is not.
    #[tokio::test]
    async fn a_request_marks_the_member_blocked() {
        let sink = Arc::new(RecordingSink::default());
        let watch = Arc::new(super::super::watch::MemberWatch::new(sink.clone()));
        let gate = Gate::new(watch.clone(), "w-t1", Role::Worker, scope().worktree)
            .watching(watch.clone());

        assert!(!watch.is_blocked());
        gate.emit(
            "acp://request/swarm/w-t1",
            json!({
                "requestId": "perm-1",
                "method": "session/request_permission",
                "params": request(command("node --test")),
            }),
        );
        // No bridge, so nothing answered it: the member is still blocked, which
        // is the truth. The spawn path fails a member with no bridge; this is not
        // the place that decides it.
        assert!(
            watch.is_blocked(),
            "a request the gate has not answered is a member that is waiting"
        );
    }

    /// A request the gate does not know how to answer is not one it can hold
    /// open, because nothing here will ever clear it.
    #[tokio::test]
    async fn an_unknown_request_does_not_leave_the_member_blocked() {
        let sink = Arc::new(RecordingSink::default());
        let watch = Arc::new(super::super::watch::MemberWatch::new(sink.clone()));
        let gate = Gate::new(watch.clone(), "w-t1", Role::Worker, scope().worktree)
            .watching(watch.clone());

        gate.emit(
            "acp://request/swarm/w-t1",
            json!({ "requestId": "x-1", "method": "session/something_else" }),
        );
        assert!(!watch.is_blocked());
    }
}
