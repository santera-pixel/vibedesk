# Plan: swarm — three roles, a peer board, isolated worktrees

Status: **proposed, not built.** Written 2026-11-17, revised twice: once after
rehearsal, once after crossing the plan against the harness's own features.
Nothing here is implemented. Every claim about the runtime or the harness is
cited to a file that was read; every claim about behaviour that has not run is
marked.

**Revision 2 note.** The first draft routed everything through the engine and
woke the lead per wave — the exact bottleneck Google's announcement names as
what `/swarm` replaces. The second draft fixed the topology but invented a
loopback MCP server as the control plane. **The harness already has a control
plane**, and it is not MCP: it is the proposal mailbox in `sources.rs`, polled
every 400 ms, validated, receipted, and documented to every agent in
`preamble.ts`. This revision uses it. §4, §6, §7, §11 and §12 changed
materially; §16 is the cross-reference that drove the change.

---

## 1. The concrete task this must complete end to end

A user types `/swarm add rate limiting to the http client: middleware, config,
and tests` in a git repository.

1. The lead reads the repo and writes a plan into the run's mailbox: three
   implementation tasks with disjoint file sets and one acceptance criterion
   each, plus its own integration task.
2. The engine validates the plan. Two tasks claim `src/http/client.rs`; the
   proposal is refused with that violation, the receipt says so, and the lead
   re-plans with the middleware task depending on the client task.
3. The user approves the plan once, in the existing proposal UI. That is the
   only approval the run needs.
4. Three workers start in three worktrees on three branches, on a cheaper model
   than the lead. The middleware task waits for the client task; it starts the
   moment that task is verified, not when a wave ends.
5. A verifier runs the suite against each worker's worktree. Task 2 fails: the
   middleware does not handle a burst. The verdict carries the command that
   fails on the base and still fails on the change.
6. The worker reads the verdict from its inbox, fixes it, and posts a
   re-verification request. The lead is not involved.
7. The lead merges in dependency order into the run's integration branch, as
   its own task in the graph, then runs the full suite on the integrated
   branch.
8. The run ends. Reflection writes one gotcha, citing the failing verdict:
   *the http client's retry loop swallows 429s, so a rate-limit test must
   assert on the retry count, not the status code.*
9. The next run's task packet for `src/http/client.rs` carries that gotcha.

**What the shallow version cannot do**, and therefore what this plan is not:
run three agents in parallel and show their outputs. That version has no
verification, no merge, no memory, no refusal, and no peer coordination.

---

## 2. What is reachable today

### The runtime

| Fact | Evidence |
| --- | --- |
| `vibe-acp` takes no `--worktree`, `--model`, `--agent` or `--trust` flag | `vibe-acp --help` prints only `-h`, `-v`, `--setup` |
| ACP `session/new` accepts `cwd`, `additional_directories`, `mcp_servers` | `vibe/acp/agent.py:545-567` |
| One `vibe-acp` process hosts many sessions | `vibe/acp/agent.py:650` |
| Per-session config options are `mode` (agent), `model`, `thinking` | `vibe/acp/agent.py:1771-1776` |
| `session/fork`, `session/list`, `session/close` advertised | `vibe/acp/agent.py:522-528` |
| `session/cancel` is in the ACP surface | `acp/meta.py:14` |
| ACP exposes **no** subagent method and **no** subagent update | `grep -rn subagent vibe/acp/` returns nothing |
| `session/list` filters on exact `cwd` **or** `origin_directory` match | `core/session/session_index.py:185-192` |
| Sessions persist under `session_logging.save_dir` | `app_server/_runtime.py:1324-1326` |
| Agent profiles support `allowlist` with `vibe-path:directory_recursive:` | `vibe` skill, Outside-Workdir Access |

### The harness

| Fact | Evidence |
| --- | --- |
| A proposal mailbox exists: `pending/` polled every 400 ms, validated, applied or rejected, receipt written back | `sources.rs:40-100, 642-810` |
| The mailbox is idempotent by digest, so an invalid file does not re-fire | `sources.rs:197-201` |
| The mailbox is covered by hostile-input tests | `battletests.rs:1043-1130` |
| Every agent is told about the mailbox in its first turn | `preamble.ts:20-50` |
| The manifest publishes the mailbox's state and the protected surfaces | `manifest.rs:97-120, 329-345` |
| `PanelBlock::Diagram` lays out a DAG: rank by longest path, order by barycentre, columns | `panels.rs:477-560` |
| Diagram nodes carry `label`, `detail`, `tone`, `when`, and an `action` fired on click | `panels.rs:477-500` |
| `PanelBlock::Table` renders a bound array of rows | `panels.rs:355-362` |
| `BuiltinName` is a closed set of six feeds with `ALL`, `parse`, `token` | `data.rs:116-150` |
| `BuiltinCommand` is a closed set of fourteen with a test that fails on an unlisted variant | `actions.rs:54-91` |
| `PROTECTED_SELECTORS` names the surfaces a theme may never restyle | `customize.rs:50-56` |
| The chat store has `state_path`, `load_at`, `save_at`, `valid_id`, `delete_at`, trash | `chats.rs:107-225` |
| `Settings` already carries workstations | `settings.rs:12-35` |
| `TerminalRegistry` lives inside `Inner`, so it is per-bridge | `bridge.rs:223-240` |
| The bridge is method-generic: `request`, `turn`, `set_session` | `bridge.rs:425-505` |
| The bridge emits events under global names, with no agent identity | `bridge.rs:685-712` |
| The app has no git code at all | grep for `Command::new("git")` finds only tokenjuice fixtures |
| A reflective-memory consolidation pass already exists | `capabilities/memory/mod.rs:1-30` |
| Six capabilities each own a live panel, a ledger row, a boundary and `ready`/`blocked` preconditions | `capabilities/mod.rs:60-100`, `src/lib/capabilities.ts:29-73` |
| The capability ledger's headline is `Math.max(0, bytesIn - bytesOut)` summed, and "N of 6 on" is derived from the array length | `CapabilityPanels.tsx:459-462, 470-480` |
| `PanelId` is `SettingsSectionId \| 'settings' \| CapabilityPanelId \| null`, and `setPanel` is generic over it | `src/store/app.ts:56, 664` |
| The rail is "navigation, not a directory of features" | `Rail.tsx:1-20` |
| The app is single-session: one `sessionId`, one transcript | `src/store/app.ts:109` |

Three consequences decide the architecture:

**The CLI's own subagents are invisible over ACP.** A member cannot be an
`agents.spawn` subagent — there would be no roster, no per-agent transcript, no
per-agent model, no per-agent worktree. A member must be a real ACP session.

**`vibe-acp` cannot create a worktree.** The client must, which puts the git
lifecycle in VibeDesk, where ownership and cleanup have to live anyway.

**The control plane already exists.** The harness's answer to "the agent needs
to reach the app" is a validated, receipted proposal mailbox, and the preamble
already teaches it to every agent. A second idiom — an MCP server — would be a
duplicate owner of the same job, resting on an ACP capability that was read,
not run.

---

## 3. The three roles, mapped to primitives that exist

| Role | Primitive | Model | Tools | Writes |
| --- | --- | --- | --- | --- |
| Lead | ACP session, profile `vibedesk-swarm-lead`, `cwd` = workstation | strongest configured alias | read, shell (read-only), mailbox, run dir allowlisted | the integration branch only |
| Worker | ACP session, profile `vibedesk-swarm-worker`, `cwd` = its worktree | cheaper alias | edit, shell, mailbox | its own worktree only |
| Verifier | ACP session, profile `vibedesk-swarm-verifier`, `cwd` = the worker's worktree | a **different** alias from the worker | read, shell, mailbox; no edit | nothing |

Profiles are custom agent TOMLs written into `~/.vibe/agents/`, namespaced and
removable, following the rule `capabilities/provider.rs:1-20` already sets:
*additive, reversible, `.bak` first, never edit the user's entries.* They
appear as ACP `mode` values because modes are built from the agent registry
(`agent.py:1741-1745`).

**Every role owns graph nodes.** The lead owns the integration task and the
audit task; each verifier owns a verification task; each worker owns an
implementation task. The lead is not a router sitting idle between waves — it
has work of its own to bear, and that work is visible in the graph.

The lead's task is **coherence**, with its own acceptance criterion: the
integrated branch builds and its full suite passes. That is not the same claim
as "every task passed its own verifier". N changes verified in isolation can
still compose badly. So the run carries two kinds of verdict, per-task and
integrated, and the integrated one is the lead's to earn. If it fails, the run
is `integration_failed` and the lead must attribute the failure to a task
before the run can close.

**Isolation is enforced by the profile, not by instruction.** A worker's agent
TOML sets `outside_workdir = "deny"` and an allowlist scoped to its worktree,
so a worker cannot reach the main checkout. The engine never passes
`additional_directories` to a worker session — that parameter would re-open the
boundary the worktree just closed.

**The lead is the only member with the run directory allowlisted**, via
`allowlist = ["vibe-path:directory_recursive:<run-dir>"]` written into its
profile at run start. That is what lets it read and grep the board. Workers
never read the board; they receive what was addressed to them in a prompt.

---

## 4. Architecture

```
                    ┌────────────────────────────────────────────────┐
   /swarm prompt ──▶│  SwarmEngine  (src-tauri/src/swarm/)           │
                    │  substrate: run doc · board · scheduler ·      │
                    │  worktrees · budget · persistence              │
                    └───────┬────────────────────────┬───────────────┘
                            │                        │
              ┌─────────────▼───┐          ┌─────────▼────────────┐
              │ Member(lead)    │          │ Member(worker|verifier) × N
              │ AcpBridge #1    │          │ AcpBridge #2..N+1    │
              └─────────┬───────┘          └──────────┬───────────┘
                        │                             │
        writes plan ────┤                             ├──── writes claim,
        reads board     │                             │     evidence, verdict
                        ▼                             ▼
              ┌──────────────────────────────────────────────────┐
              │  run mailbox  ($VIBE_HOME/swarm/<run>/mail/)     │
              │  pending/swarm.json  → validated → receipt       │
              │  inbox/<member>/<seq>.json  → delivered by prompt │
              │  board.jsonl  ← appended by the engine, in order  │
              └──────────────────────────────────────────────────┘
```

**The engine is substrate, not router.** It persists the board, schedules
tasks, creates worktrees, enforces the budget, and wakes addressed members. It
does not relay messages between agents: a verifier's verdict reaches the worker
because the verifier addressed it, and the engine's only role is to put it in
the worker's next prompt.

**The mailbox is the control plane, and it is the harness's own.** The lead
writes a plan or a spawn request into `pending/`; the engine validates it with
the same discipline `panels.json` gets, writes a receipt, and the user approves
the plan once in the existing proposal UI. Workers write claims, evidence and
verdicts into their own inbox directory; the engine appends them to
`board.jsonl` in arrival order.

### One process per member, not one process with N sessions

Both are reachable. The plan chooses one `vibe-acp` process per member:

- `AcpBridge` is reused verbatim. Its `start`, `request`, `turn`,
  `set_session`, `respond_permission` all work unchanged per instance. The
  1299-line file needs one additive change (event scoping), not a
  session-scoping refactor.
- `TerminalRegistry` lives inside `Inner` (`bridge.rs:223-240`), so per-process
  members get per-member terminals with no keying change at all.
- A worker that crashes takes down its own process, not the lead's.
- Two other agents are working on this harness. Refactoring `bridge.rs` into a
  multi-session router is the single most likely merge conflict in the
  repository. Not doing it is the merge-safe choice.

The cost is one Python process per member. **Unmeasured.** Phase 0 measures
resident memory and cold start for N = 1..6 before the concurrency default is
fixed. Until then the default is 3 and the ceiling is 6.

### The run document

`$VIBE_HOME/swarm/<run-id>/run.json` — derived state: plan, members, task
states, integration branch, budget, outcome, each member's `session_id`.

`$VIBE_HOME/swarm/<run-id>/board.jsonl` — append-only, one JSON object per
line. The board is the source of truth; `run.json` is rebuilt from it plus the
plan. Same discipline as the session logs, and the same shape as `chats.rs`'s
state-plus-directory store (`chats.rs:107-225`), which the run store reuses
rather than reinventing.

```json
{ "seq": 14, "at": "2026-11-17T14:02:11Z",
  "author": { "role": "verifier", "member": "v2", "model": "…" },
  "kind": "verdict", "task": "t2", "to": ["w2"],
  "body": "burst case fails: 12 requests, 0 rejected",
  "refs": [11, 13] }
```

`kind` is a closed set: `task`, `claim`, `evidence`, `verdict`, `decision`,
`conflict`, `budget`, `note`. `to` is the addressing field: an entry addressed
to a parked member wakes it. That is the whole wake mechanism — no polling for
wakes, no heartbeat, no timer.

---

## 5. The mechanisms, in depth

### 5.1 Isolation — worktrees the engine owns

`vibe --worktree` exists in the CLI but not in `vibe-acp`, so the engine runs
git itself:

```
git worktree add -b swarm/<run>/<task> <path> <integration-branch>
```

- **Path**: `$VIBE_HOME/worktrees/<repo>/<run-id>/<task-slug>` — the CLI's own
  convention, so `vibe --worktree` and VibeDesk do not accumulate two parallel
  worktree farms.
- **Base**: the run's integration branch, created from the workstation's `HEAD`
  when the run starts. Workers never branch from each other.
- **Ownership**: the run owns every worktree it created. Cleanup follows the
  CLI's rule — remove only what this run created, keep anything dirty, keep
  anything unmerged, and say which and why.
- **Non-git folder**: a write-parallel swarm is refused with
  `isolation_unavailable`. The offer is a read-only swarm (investigation, no
  writes, genuinely parallel-safe) or a single chat.

The engine never runs `git worktree prune`, never force-removes, and never
touches a worktree it did not create.

### 5.2 Parking and waking — what "zero token" honestly means

An idle agent already costs nothing. The costs that are real:

1. **An open turn.** A model mid-generation waiting on a tool result is burning
   tokens. No member holds an open turn while waiting.
2. **The context re-send on wake.** Waking a parked agent re-bills its history.
   That is not zero and cannot be made zero.

So parking is the default and the wake cost is visible:

- A worker is prompted with its task packet, runs to completion, and its turn
  ends. It is parked.
- The lead posts tasks and **ends its turn**. It does not wait.
- **A wake is an addressed board entry.** A verifier posts a verdict `to: ["w2"]`
  and the engine wakes `w2` with that entry alone — not the verifier's
  transcript, not the run's history. A dependency completing wakes its
  dependents the same way.
- The lead is woken when an entry is addressed to it: a conflict, a budget
  event, a task that exhausted its retries, or the run's final task becoming
  mergeable.

The board is the compression boundary. Worker traces stay in their own session
logs; the board carries decisions, evidence pointers and verdicts. A member's
context grows by what was addressed to it, not by what happened in the run.

The Roster shows each member's context size and cost, so the price of the next
wake is on screen before the user triggers it.

### 5.3 Hot-swapping — role routing and escalation

`session/set_config_option` sets `model` per session (`agent.py:1771-1776`).
The engine assigns lead → strongest, worker → cheapest non-lead, verifier → an
alias **different from the worker's**.

Escalation is a policy, not a label: a task that fails verification twice is
re-dispatched to a worker on the next tier up, and the board records the
escalation with the two verdicts that caused it.

The role→alias table lives in `Settings` (`settings.rs:12-35`), beside the
workstations, not in a new file. The app never invents a model name; it reads
the configured catalogue (`src/types/acp.ts:383`) and refuses to save a mapping
to an alias that is not in it.

### 5.4 Adversarial verification — independence is structural

A verifier that reads a diff and approves it is a second opinion, not a test.
Four rules, and the fourth is the one that produces quality:

1. **Failing-first evidence.** Produce a command that fails on the base commit
   and passes on the change. If no such command exists, the verdict is
   `cannot_verify` — a first-class outcome, never a pass.
2. **Run it.** The verdict carries the command, the exit code, and the output
   tail. A verdict without a command is refused by the engine.
3. **Different model.** Enforced by the role table, reported when unavailable.
4. **Different information.** The verifier receives the diff, the acceptance
   criterion, and the run's base checkout. It does **not** receive the worker's
   transcript, reasoning, or task packet. A verifier that reads the worker's
   framing inherits the worker's blind spot. The information asymmetry is the
   mechanism; the model difference is the backup.

The base checkout is one shared worktree per run, created detached at the
integration branch's start commit (`git worktree add --detach`). Without it the
verifier cannot prove failing-first, and rule 1 becomes an instruction it can
skip.

It is not "read-only" — git cannot make a checkout read-only, and a shell
command can write anywhere the profile allows. What is enforceable is
detection: the verifier's profile has no edit tool and allowlists read and
shell on exactly two paths (the worker's worktree and the base checkout), and
the engine checks the base checkout's `HEAD` and status after every verdict. A
dirty base checkout marks that verdict `base_dirtied` and the run records it,
so a verifier that mutated the baseline to make its command pass is caught
rather than trusted.

The repo already has this category: `battletests.rs` assumes the input is
hostile and checks the app still behaves. The verifier is the same idea applied
to a change instead of a config file.

### 5.5 Integration — the part that is usually faked

N worktrees into one branch. The engine does not octopus-merge and does not
auto-resolve.

1. The plan declares a **file set** per task. The engine validates that no two
   tasks that can run concurrently share a file. Overlap is not a warning; it
   is a plan rejection.
2. Merge order is the DAG's topological order.
3. A conflict **stops the run**. It is posted as a `conflict` entry naming both
   task ids and the conflicting paths, addressed to the lead, which re-plans.
4. The engine never resolves a conflict itself. A conflict is a planning
   failure, and the run reports it as one.
5. The merge is not the end. The lead's integration task runs the full suite on
   the integrated branch and posts an `integrated` verdict. A failure here is
   `integration_failed`, and the lead must name the task it blames before the
   run closes.

The honest ceiling: **the swarm cannot merge what it cannot attribute.** That
is why the file-set declaration is mandatory rather than advisory.

### 5.6 The scheduler — a ready-queue, not waves

Waves are a barrier, and a barrier is the difference between "fast" and
"lockstep". The scheduler is continuous:

- A task becomes `ready` the moment every dependency is `verified`.
- `ready` tasks enter a queue bounded by the concurrency ceiling; a queued task
  is `queued`, a distinct state from `ready`, and shown as such.
- A slot frees when a member's turn ends, and the next queued task starts
  immediately.
- Verification runs concurrently with implementation.
- The lead's integration task becomes `ready` when the last implementation task
  is `verified`, and runs while reflection is being prepared.

The graph is *laid out* by topological level, because that is the readable
shape. It is *scheduled* by readiness. Conflating them is how a swarm ends up
slower than a chat.

```
pending → ready → queued → running → verifying → verified → merged
                    │         │          │
                    └─────────┴──────────┴──▶ failed | blocked | cancelled
```

### 5.7 Dependency handoff — decisions, not transcripts

Cognition's first principle is that a subagent needs the full trace, because
actions carry implicit decisions. That is right, and unaffordable at N agents:
shipping full traces is how a swarm's context explodes.

The affordable version: when task B depends on task A, B's packet carries A's
**decision digest** — extracted from A's board entries, not its transcript:

- what A changed, by file
- what A decided that B must not contradict (naming, error shape, ordering)
- what A explicitly left open
- A's verifier's verdict and the command that proved it

The digest is generated by the engine from the board, not by a model call, so
it costs nothing and cannot hallucinate. If the digest is empty — A posted no
decisions — B's packet says so, and B is told to read A's diff before writing.

This is the one place the plan deliberately departs from Cognition's principle:
the digest is a lossy compression of the trace, and the loss is the point.

---

## 6. Observability — three views, two of them already built

The harness already renders a DAG and a table over bound data. The swarm does
not need a graph renderer or a roster renderer; it needs a **data source** and
a **panel**.

### The six capability panels, and where the swarm sits

The app already has a family of live panels: `CapabilityId::ALL` is
compaction, terminal, reading, memory, code-graph and web
(`capabilities/mod.rs:60-100`). Each carries a `key`, `name`, `repo`,
`summary`, `boundary`, `panel` and `default_enabled`; each has its own panel id
in `CapabilityPanelId` (`src/lib/capabilities.ts:29-36`), its own icon in
`PANEL_ICON` (`CapabilityPanels.tsx:446-453`), and a ledger row that clicks
through to it. The store's `PanelId` is
`SettingsSectionId | 'settings' | CapabilityPanelId | null`
(`src/store/app.ts:56`), and the comment above it states the rule the family
exists for: these are *"peers of `context` and `status`, not settings sections
— each renders data that only exists while the app is running, so there is
nothing to configure in a settings rail."*

The swarm is the same kind of surface, and it belongs in that family — as a
seventh entry in the capabilities panel, above the six, gated on them.

**It is a row in the capabilities panel, above the six.** Not a separate
drawer, not a rail icon. `CapabilitiesPanel` renders it between the `Total`
section and the `Boundaries` section, with extra top margin so it reads as
separate from the six rows below it (`CapabilityPanels.tsx:455-535`). One
insertion in a file the other agents are in, which is the smallest possible
footprint there.

**It is not a toggle.** The six have an on/off switch. The swarm has none: its
state is *derived* from theirs. `allOn = data.every(r => r.enabled)`.

| State | When | Rendering |
| --- | --- | --- |
| `dim` | not all six on | muted text and border (`--vd-text-tertiary`), no glow, icon muted; subtitle names the count — "3 of 6 on — turn on the rest to unlock" |
| `radiant` | all six on | `--vd-accent` text, border and icon, `--vd-accent-wash` background, and a soft outer glow |

The glow is the app's existing idiom, not a new one. `globals.css:1-12` caps
colour at "one hot orange accent" and "one glow on the primary action"; the
swarm entry becomes that primary action when the six are all on. `--vd-accent`
is `#ff7a1a` in ink and `#c2410c` in paper, so the glow follows the theme
without a second token.

**It shows no saved-bytes number.** The six report `bytesIn - bytesOut`. The
swarm spends. Its row shows run state instead — no run, run live, or blocked
with the reason — and it never contributes to the `Total` headline above it.

**It does not join the ledger.** `CapabilityRecord` reports
`bytesIn - bytesOut`, and the ledger's headline is *"kept out of context"*
computed as `Math.max(0, bytesIn - bytesOut)` summed over every record
(`CapabilityPanels.tsx:459-462, 470-480`). Every one of the six *reduces* what
reaches the model. The swarm *spends* — Anthropic measured multi-agent systems
at ~15× a chat. The clamp means a swarm row would not corrupt the total with a
negative; it would do something worse: render as a capability that saves
exactly zero bytes, toggleable, with a boundary chip and a ledger row, doing
nothing measurable in the ledger's terms. And `"N of 6 on"` is
`data.filter(enabled).length` over `data.length`, so it would read `"N of 7
on"`, implying a context-economy toggle the swarm is not. So the swarm is
rendered *above* the six and outside the array the ledger sums: `CapabilityId`
stays a closed set of six, and the swarm entry is a sibling element in the
panel, not a seventh record.

**It borrows the ledger's shape.** `CapabilityStats` is
`{ calls, bytesIn, bytesOut, counters, lastAt }` with named counters and a
capped recent list (`capabilities/mod.rs:198-213`, `RECENT_LIMIT`). The run
document carries the same shape — counters for tasks dispatched, verdicts
passed, verdicts failed, conflicts, escalations and wakes — so the panel reuses
the `Ledger` presentation (`CapabilityPanels.tsx:283-330`) instead of inventing
a second stats view. The board stays the unbounded record; the panel shows the
last N, exactly as the six do.

**Its preconditions are `ready` and `blocked`, computed before the run.** Every
capability carries `ready: boolean | null` and `blocked: string | null`, and
`CapabilityHeader` renders the reason in the danger colour
(`CapabilityPanels.tsx:248-280`). The swarm's preconditions are the same shape
and belong in the same place: not a git repository, no second model configured
so verifier diversity is unavailable, `git` not on `PATH`, the worktree root
not writable, no model configured at all, and **not all six capabilities on**.
Computed when the workstation opens, shown on the panel, and used to disable
`/swarm` in the composer. The user learns why the swarm cannot run *before*
typing the prompt, not at run start.

The "all six on" precondition is the one the dim/radiant state encodes. It is a
real gate, not decoration: a swarm run leans on compaction to keep member
contexts small, on memory for the reflection pass, on code-graph for the
dependency handoff, and on reading for the verifier's diff review. Running it
with those off is the 15×-token case with none of the mitigation. The dim state
names which are off so the fix is one click away.

**Its settings follow the same split.** `CapabilitySettings` is an `enabled`
map plus one struct per capability (`capabilities/settings.rs:18-29`), and
compaction has both a panel and a settings block. The swarm does the same:
`SwarmSettings` in `Settings` (role→model table, concurrency, budget, verifier
diversity policy), and the panel is the live surface.

**It is reached from the capabilities panel, and from a live run.** The panel
entry is the permanent door — it exists in both the dim and radiant states, so
the preconditions always have somewhere to be read. A run adds a second door:
the rail gains a run entry **while a run is live**, obeying `Rail.tsx`'s rule
that *"the rail is navigation, not a directory of features… what stays in the
rail is what is not settings: the conversation, and the panels the user
authored."* No permanent swarm icon in the rail; the capabilities panel is
where the swarm lives, and the rail only carries a run while one exists.

### The Graph is a `diagram` block

`PanelBlock::Diagram` lays out a graph with `layered` — *"rank by longest path,
order each rank by barycentre, place in columns"* (`panels.rs:477-560`). That
is the DAG layout this plan would otherwise have written from scratch. Nodes
carry `label`, `detail`, `tone` (`neutral | accent | ok | warn | danger |
muted`), `when`, and an `action` — *"an id from `actions.json`, fired when the
node is clicked. This is what makes a diagram a control surface rather than a
picture."*

So the Graph is:

```json
{ "block": "diagram", "label": "Run", "layout": "layered",
  "nodes": [ { "id": "t1", "label": "client", "detail": "worker · sonnet",
               "tone": "ok", "action": "swarm-open-task" } ],
  "edges": [ { "from": "t1", "to": "t2" } ] }
```

Node tone maps to task state. Node detail carries the model or the verdict.
Node action opens the task. Zero new rendering code, and the layout is already
deterministic and tested.

### The Roster is a `table` block

`PanelBlock::Table { columns, rows }` over a bound array (`panels.rs:355-362`).
Columns: role, model, branch, state, context, cost. The one gap is per-row
drill-down — `Table` has no `action`, while `DiagramNode` does. Two options:

- **Preferred**: add an optional `action` to `Table`, mirroring
  `DiagramNode::action`. Additive, ~10 lines, and it makes every table in the
  app clickable rather than only the swarm's.
- **Fallback**: keep drill-down in a core Roster component and let the panel
  table be read-only.

### The Board is the transcript

A board entry is a message with an author, so it renders through the existing
`Transcript` component, which already handles roles, tool cards and diffs. The
panel view of the board is a `table` block over the same source.

### One new builtin feed

`BuiltinName` is a closed set of six (`data.rs:116-150`) with `ALL`, `parse`
and `token`. Add `Swarm`:

```rust
pub enum BuiltinName { Status, Usage, Session, Chats, Plugins, Clock, Swarm }
```

Then `data.swarm.tasks`, `data.swarm.members`, `data.swarm.board` and
`data.swarm.budget` are bindable by any panel or widget — including
agent-authored ones. That is how the swarm becomes customizable without new UI
code, and it is the same extension point every other feed uses.

`manifest.rs` must publish it in `PanelBindings`, or the agent cannot use it.

### The controls are `BuiltinCommand` variants

`ActionRun` is a closed enum and `BuiltinCommand` is a closed set of fourteen
with a test that fails if a variant is added without being listed
(`actions.rs:54-91`). Add `OpenSwarm`, `StopSwarm`, `RetryTask`,
`EscalateTask`, `OpenWorktree`, `OpenMember`. Then the swarm's buttons are
`actions.json` entries, the diagram's node actions work, and a widget can fire
them through its declared `action` capability.

### The approval queue is a protected surface

`PROTECTED_SELECTORS` is `vd-protected`, `.vd-approval`, `.vd-elicitation`,
`.vd-statusbar`, `.vd-composer` (`customize.rs:50-56`), and the manifest
explains why: *"These surfaces tell the user what the agent is asking. A theme
that could restyle them could make the UI lie."* The swarm's per-member
approval queue is exactly that class of surface. Add `.vd-swarm-approval` to
the list in the same commit that adds the surface.

---

## 7. Session persistence and the panel

**The Sessions panel does not bloat.** `session/list` filters on an exact match
of `cwd` or `origin_directory` (`core/session/session_index.py:185-192`), and a
member's `cwd` is its worktree under `$VIBE_HOME/worktrees/…`, never the
workstation. A member is invisible to the workstation's session list by
construction.

**The disk does grow, quietly.** Every member persists a session directory
under `session_logging.save_dir` (`app_server/_runtime.py:1324-1326`). A
five-member run leaves five directories no surface accounts for. Invisible
growth is worse than visible bloat, because the user cannot clean it up.

So the run owns its members' traces, the same way it owns its worktrees:

| Surface | What it lists | What it owns |
| --- | --- | --- |
| Sessions panel | chats you can resume | nothing new |
| Roster | members of *this* run, live | the run's member session dirs |
| Rail | runs, as runs | the run document, board, reflection |

- The run document records each member's `session_id` and directory.
- Deleting a run deletes its member directories, and says so before doing it.
  The run's own record — `run.json`, `board.jsonl`, the reflection output — is
  the durable artifact.
- A member session that ever surfaces elsewhere carries a title prefixed with
  the run id, so it is identifiable rather than mysterious.
- `session_logging.enabled = false` means no traces on disk. The Roster
  drill-down then works live only, and reflection degrades to board-only and
  says so. The run records which mode it ran in.

The rule: **a swarm member is never presented as a chat.** Two lists, each with
one job.

---

## 8. Post-task reflection

The repo already owns the machinery: `capabilities/memory` runs
Orient → Gather → Consolidate → Prune over finished sessions and writes
memories the proxy injects into the transient system prompt
(`memory/mod.rs:1-30`). Swarm reflection is a specialization, not a second
system.

After a run ends, `swarm/reflect.rs` builds a trajectory document — board,
verdicts, conflicts, the diffs that failed — and makes one model call to
extract **gotchas**: facts about this repository that would have prevented the
failures that actually occurred.

A gotcha is written only if it cites at least one board entry of kind `verdict`
(fail) or `conflict`. No citation, no write. Without that gate the memory fills
with plausible-sounding advice and becomes noise the next run has to read.

```json
{ "text": "the http client's retry loop swallows 429s; assert on retry count",
  "files": ["src/http/client.rs"],
  "evidence": [14, 17], "confidence": "high" }
```

Injection has two paths, and the second is the one that matters:

- the existing proxy recall, when compaction is on;
- **task packets**: a new task whose file set intersects a gotcha's files
  carries that gotcha. Targeted, not global, and it works whether or not the
  proxy is enabled.

---

## 9. The gate — when the swarm refuses

The strongest argument against this feature is in Anthropic's own write-up:
*most coding tasks involve fewer truly parallelizable tasks than research, and
LLM agents are not yet great at coordinating and delegating to other agents in
real time.* Cognition's is sharper: write actions carry implicit decisions, and
conflicting decisions produce incoherent merges.

So the swarm is offered only when the plan proves it applies. Four of these
are **pre-flight** — properties of the workstation, computed when it opens,
rendered as `ready`/`blocked` on the swarm panel with the reason in the danger
colour, and used to disable `/swarm` in the composer. The other three are
**plan-time** — properties of the plan, discovered when the lead submits it,
and returned in the receipt.

| Condition | When | Otherwise |
| --- | --- | --- |
| all six capabilities on | pre-flight | `blocked`: "3 of 6 capabilities on — the swarm leans on all of them"; the panel entry is dim |
| a git repository, for any task that writes | pre-flight | `blocked`: "this folder is not a git repository — the swarm isolates workers in worktrees" |
| a verifier model different from the worker's | pre-flight | `blocked`: "one model configured — a verifier on the same model is not independent" |
| `git` on `PATH`, worktree root writable | pre-flight | `blocked` with the failing check named |
| ≥ 2 implementation tasks | plan-time | "this is one task — run it as a chat" |
| disjoint file sets, or serialized by the DAG | plan-time | plan rejected with the overlap named |
| an observable acceptance criterion per task | plan-time | plan rejected with the task named |

The split matters. A pre-flight refusal is visible before the user types
anything, which is the only point at which it is cheap. A plan-time refusal
costs one lead turn and returns a receipt the user can act on. Neither is a
silent degradation.

A blocked `/swarm` opens the swarm panel rather than doing nothing, so the
reason is always readable and the panel is never a dead end (§6).

That refusal is a feature. A swarm that accepts a single-task plan is a slower
chat with a roster.

---

## 10. Failure, interruption and restart

A desktop app is closed mid-run. Every one of these is a named state.

| Event | State | What survives |
| --- | --- | --- |
| Plan invalid | `plan_rejected` + the violation, in the receipt | nothing created |
| Worker process dies | `member_failed`; task back to `ready` within its retry budget | its worktree, its trace |
| Verifier cannot prove failing-first | `cannot_verify` on the task | the verdict and its reason |
| Verifier dirtied the base | `base_dirtied` on the verdict | the base checkout's status |
| Integration conflict | `conflict`, run paused, lead addressed | both worktrees, both branches |
| Integrated suite fails | `integration_failed`, lead must attribute it | the integration branch, every verdict |
| Budget breach | `budget_exceeded`, running turns cancelled, everyone parked | everything |
| User stops the run | `cancelled`, turns cancelled, everyone parked | worktrees kept, named |
| App closed mid-run | `interrupted` on next open; members `lost` | run doc, board, worktrees |
| Resume an interrupted run | members re-spawned with `session/load` on their ids | full traces replayed, at a cost |

Resume is offered, not automatic: replaying five members' histories is a real
token cost and the user should choose it. Abandoning keeps the worktrees and
says which ones.

**Approvals.** N members can raise N permission requests at once. The existing
`ApprovalBar` handles one. The swarm needs a per-member queue with the member
named on each request, and the worker profile's `outside_workdir = "deny"`
means the common case — a worker editing its own worktree — never asks at all.

**Cost attribution.** The run's total includes the lead's turns, every worker
and verifier turn, the plan review, and the reflection call. A budget that
counts only workers is a budget that lies.

---

## 11. Merge surface — two other agents are on this harness

Every new behaviour lives in a new file. Shared files get additive edits only,
and each one uses an extension point the file already has: a closed enum with
an `ALL` array and a test that fails when a variant is added without being
listed.

**New files**

```
src-tauri/src/swarm/mod.rs        engine, run doc, roster
src-tauri/src/swarm/plan.rs       plan schema + validation
src-tauri/src/swarm/mail.rs       the run mailbox: pending, inbox, receipts
src-tauri/src/swarm/board.rs      append-only board, addressing, digests
src-tauri/src/swarm/schedule.rs   ready-queue, retries, escalation
src-tauri/src/swarm/worktree.rs   git worktree lifecycle
src-tauri/src/swarm/roles.rs      agent TOML generation and removal
src-tauri/src/swarm/integrate.rs  DAG-order merge, conflict attribution
src-tauri/src/swarm/reflect.rs    trajectory review → gotchas
src-tauri/src/commands/swarm.rs   Tauri commands
src/components/swarm/RunHeader.tsx
src/components/swarm/MemberTranscript.tsx
src/components/swarm/ApprovalQueue.tsx
src/lib/swarm.ts
src/types/swarm.ts
```

**Shared files, additive only**

| File | Change | Size |
| --- | --- | --- |
| `src-tauri/src/acp/bridge.rs` | add a `scope` field to emitted payloads so N bridges are distinguishable | ~10 lines |
| `src-tauri/src/lib.rs` | register the swarm commands and state | ~15 lines |
| `src-tauri/src/data.rs` | `BuiltinName::Swarm` + `ALL` + `parse` + `token` | ~8 lines |
| `src-tauri/src/actions.rs` | six `BuiltinCommand` variants + `ALL` | ~12 lines |
| `src-tauri/src/customize.rs` | `.vd-swarm-approval` in `PROTECTED_SELECTORS` | 1 line |
| `src-tauri/src/manifest.rs` | publish the swarm feed and the new commands | ~10 lines |
| `src-tauri/src/settings.rs` | swarm settings beside workstations | ~15 lines |
| `src-tauri/src/panels.rs` | optional `action` on `Table` (preferred, see §6) | ~10 lines |
| `src/store/app.ts` | `'swarm'` in the `PanelId` union beside `CapabilityPanelId`, plus a `swarm` slice beside the session slice | ~40 lines |
| `src/lib/bridge.ts` | add a `swarm` namespace | ~30 lines |
| `src/lib/commands.ts` | one `/swarm` entry, `transport: 'local'`, `action: 'start-swarm'` | ~6 lines |
| `src/lib/preamble.ts` | one paragraph telling a member what it is | ~8 lines |
| `src/components/chrome/Rail.tsx` | one run entry, rendered only while a run is live | ~6 lines |
| `src/components/panels/CapabilityPanels.tsx` | one sibling element above the six rows, with its dim/radiant states | ~45 lines |
| `src/App.tsx` | one branch: `panel === 'swarm'` renders the swarm panel | ~10 lines |

`CapabilityPanels.tsx` is the highest-risk row in this table: it is the file the
other two agents are most likely to be in, and the swarm entry is the only
change in this plan that is not a new file or a one-line enum extension. It is
therefore the last thing to land, not the first — Phase 7, after the six are
stable and the panel has stopped moving. If it is still moving when Phase 7
starts, the swarm entry waits and the rest of Phase 7 ships without it.

`setPanel` needs no change: it is `(panel) => transitionTo('panel', () => set({ panel, customPanel: null }))` (`src/store/app.ts:664`), generic over the union. Adding `'swarm'` to `PanelId` is the whole store-side cost, and the panel renders as a peer of `context` and `status` for the same reason the six capability panels do.

That is more shared files than the MCP draft touched, and each touch is smaller.
The trade is deliberate: a sanctioned enum extension in six files beats a new
server stack and a second control-plane idiom in one.

---

## 12. Phases

`TDD Route: auto → strict` for the Rust modules (behaviour, shared core,
persistence, producer/consumer), `light` for the React views, where the live
suite is the real evidence. Every phase ends with a command whose output is the
proof.

### Phase 0 — Foundation: N bridges, scoped events

Make `AcpBridge` instantiable per member and give its events an identity.
Measure resident memory and cold start for N = 1..6.

**Proof**: two members run concurrently in two worktrees; each transcript
receives only its own `session/update`; the measurement table is in the phase
record and the concurrency default is set from it.

### Phase 1 — Worktree lifecycle

Create, reuse, clean up, refuse. Ownership recorded per worktree. One shared
detached base checkout per run, with write detection.

**Proof**: against a scratch git repo — N worktrees on distinct branches; a
dirty worktree survives cleanup and is named; a non-git folder returns
`isolation_unavailable`; no worktree the run did not create is touched; the
base checkout is detached at the start commit, and a write to it is detected
and reported rather than assumed away.

### Phase 2 — Plan schema and validation

The DAG, file-set disjointness, acceptance criteria, acyclicity, and the
pre-dispatch plan review by a verifier.

**Proof**: a plan with two tasks on one file is refused naming both tasks and
the file; a cyclic plan is refused naming the cycle; a task with no acceptance
criterion is refused naming the task; a valid plan round-trips byte-identically;
a plan review that rejects produces `plan_rejected` before any worktree exists.

### Phase 3 — Scheduler

Ready-queue, concurrency ceiling, retries, escalation, budget. No waves.

**Proof**: a three-task chain starts task 2 the moment task 1 is verified, not
when a wave ends — asserted from board timestamps; a queued task is
distinguishable from a ready one; a budget breach cancels and parks rather than
silently continuing; the run total includes the lead's and the reflection's
tokens.

### Phase 4 — The mailbox and the peer board

The run mailbox reuses the proposal protocol's discipline: a validated
`pending/` file, a digest so an invalid file does not re-fire, a receipt the
agent reads, and a 400 ms tick. The board is a separate append-only inbox,
because a claim is a fact, not a proposal that needs a decision.

**Proof**: the lead writes a plan, the engine refuses it with a violation, the
receipt carries the validator's message, and the lead's next plan is accepted;
a worker writes a claim and it appears on the board in arrival order; a worker
writing into another member's inbox is refused and the refusal is recorded; a
verifier's addressed verdict wakes the worker with that entry alone — the
worker's context grows by the verdict, not by the verifier's transcript.

### Phase 5 — Verifier role

The failing-first contract, the verdict schema, the refusal of a verdict with
no command, and the information asymmetry.

**Proof**: a deliberately broken change is rejected with the command that
fails; a change with no possible failing-first command returns `cannot_verify`,
not `pass`; a verifier given the worker's transcript is refused by the engine,
and the same verifier given only the diff reaches the same verdict; a verifier
that writes to the base checkout produces a `base_dirtied` verdict.

### Phase 6 — Integration and the speed measurement

DAG-order merge, conflict attribution, the integrated verdict, and the number
that decides whether this feature is worth keeping.

**Proof**: two tasks touching one file are serialized by validation; a real
conflict stops the run with both task ids and the paths; the integration branch
contains exactly the verified changes; a change that passes its own verifier
but breaks the integrated suite produces `integration_failed` and the lead
names the task it blames. **And**: wall-clock and total tokens for the same
task run as a swarm and as a single agent, recorded in the phase record. If the
swarm is not faster on a task with genuinely disjoint parts, or costs more than
the user's budget tolerates, that is the finding.

### Phase 7 — Observability, mostly by binding

`BuiltinName::Swarm`, the manifest publication, the diagram and table blocks,
the per-member transcript drill-down, the approval queue, and the swarm entry
in the capabilities panel with its dim and radiant states.

**Proof**: the swarm panel is a built-in React panel like the six capability
panels — one `App.tsx` branch over `panel === 'swarm'` — and it renders the
graph and roster from the feed; the manifest lists the swarm feed and the new
commands, so an agent-authored `panels.json` panel can bind to the same feed
and build its own view; clicking a graph node fires its action; clicking a
roster row opens that member's own tool calls; two simultaneous approval
requests are both visible and attributable; a theme that targets
`.vd-swarm-approval` is refused.

**Proof, panel family**: the swarm entry renders in the capabilities panel
above the six, with space, and is dim while any capability is off — the
subtitle names the count; turning the last one on makes it radiant in
`--vd-accent` with the glow, in both themes; it has no toggle of its own; the
capabilities ledger's "kept out of context" total is byte-identical with and
without a run, and `"N of 6 on"` still reads 6; the preconditions render as
`ready` or `blocked` with the reason in the danger colour before any run
starts, and `/swarm` is disabled while blocked; the run entry appears in the
rail only while a run is live and disappears when it ends.

### Phase 8 — Reflection

Trajectory review, the evidence gate, task-packet injection, the
`session_logging`-off degradation.

**Proof**: a gotcha written by run 1 appears in run 2's packet for the same
file; a gotcha with no verdict or conflict citation is not written; with
logging off, reflection runs board-only and says so.

### Phase 9 — The gate, the refusals, and restart

`/swarm` on a single-task prompt, on a non-git folder, on overlapping files;
stop mid-run; close the app mid-run; resume.

**Proof**: each refusal produces its named state and its explanation and starts
no run; a blocked `/swarm` opens the swarm panel with the failing precondition
named rather than doing nothing; a stopped run leaves named worktrees; a closed
app reopens to `interrupted` with the board intact and offers resume or abandon.

---

## 12a. Phases 10-17 — speed, memory and visibility

Phases 0-9 built a swarm that works. The twenty-second live run
(`run-20261120-140544-000`) showed it working slowly and silently: 33 minutes,
0 of 10 tasks verified, 18 of 21 members labelled `starting` with no process, a
verifier that spent 20.7 minutes and 1,989,387 input tokens before crashing with
no verdict, and a member whose recorded spend went *down*.

The first revision of this section named six causes. A second pass found six
more, two of them worse than anything in the first list:

- **The run could never have finished.** `Budget::default().max_wall_secs` is
  3600 seconds and `out_of_time()` sets `RunState::Failed`. The observed plan's
  projected critical path was about 95 minutes. The run was going to be killed
  mid-task by its own budget, with every worktree discarded.
- **The reflection pass writes knowledge nothing reads.** `reflect::relevant` is
  reached only through `gotchas_for`, which is called only from a test. The
  reflector spends its tokens, writes `gotchas.json`, and no future run ever
  loads it. `lead_brief` takes the goal, the workspace, the run id, the inbox
  and the skills — and no history at all, so the lead plans blind every time.

The order below is by dependency, not by value. Phases 10-12 are what the user
can see plus the cheapest speed. Phases 13-14 are the plan and the critical
path. Phase 15 is the memory the budget in 13 needs. Phases 16-17 are the
interface and the polish, and both can be dropped without weakening anything
above them.

### Phase 10 — The roster tells the truth

`MemberState` declares eight variants and the engine assigns six. `Idle` and
`Waiting` are never set anywhere, and there is no `Queued` at all, so
`Member::new`'s `Starting` default is the only thing a member waiting for a slot
can ever say. The roster is not lying about a state; it is reporting a state
that does not exist.

- Add `MemberState::Queued`. `Member::new` defaults to it.
- `spawn_for_task` sets `Starting` when the process actually launches; the turn
  start sets `Working`; `end_member_turn` sets `Idle` for a member with no task.
- The gate sets `Waiting` while a permission or elicitation is outstanding and
  clears it on the answer. It already knows both moments.
- `Member.turn_started_at` is set at turn start and cleared at end, so the panel
  can show live elapsed without inventing a second clock.
- `Gate::record_usage` calls `observe(...)` on every reading rather than only
  when `final` is true. `observe` is delta-based and idempotent, so this is safe;
  `finish_turn` stays behind `final`, because a turn is not finished until it is.
- A member's spend must never go backwards. `spawn_member` replaces the ledger
  with a fresh `Usage`, and the drive loop then overwrites the run's member usage
  from it, so a respawn erases the previous attempt's tokens. The cancelled run
  shows it: `w-t1` fell from 1,189,646 input tokens to 461,743, and `v-t1`'s
  1,989,387 is only its second session's spend. `budget.used_tokens` sums the
  survivors, so the run under-reports what it cost. The ledger is keyed by
  member for the life of the run and a new session adds to it rather than
  replacing it.

**Proof**: a 10-task run at concurrency 4 shows 4 members `working` or
`starting` and 16 `queued`, and that count equals the number of `vibe-acp`
processes; a member mid-turn reports non-zero tokens before its turn ends; a
member blocked on a permission reads `waiting` and returns to `working` when the
gate answers; a member that is respawned keeps its earlier spend and the run
total never decreases across a tick.

### Phase 11 — Live activity

The member's stream already reaches `MemberWatch`, which parses it for skill
loads and liveness. It is the right owner for activity too, and it is already in
the path.

- `MemberWatch` gains a bounded ring of 200 `ActivityItem`s — `{at, kind, title,
  status}` — fed by the `tool_call`, `tool_call_update`, `agent_message_chunk`
  and permission notifications it already inspects.
- The same ring records a token reading per item, so the panel can draw a rate
  rather than a total. A total tells the user what a member has spent; a rate
  tells them whether it is doing anything.
- `Member.activity` carries `current` for every member and `recent` only for the
  selected one. `Engine::snapshot` takes the selection so a 21-member payload
  stays bounded; the panel already polls every 2s, so no new event channel is
  needed for the first version.
- `SwarmPanel` gains a `MemberDetail` pane above the filtered board: header,
  current tool with its elapsed time, turn elapsed, tokens so far, the context
  meter, the feed newest-first, and the member's own board entries.

**Proof**: with a live run, selecting a worker shows its current tool call within
2s of it starting and the feed grows as it works; a finished tool shows
`completed`; the snapshot payload for 21 members stays under a measured bound
with one member selected.

### Phase 12 — The verifier stops re-proving

The verifier's turn is 13+ minutes against the worker's 6. Its brief asks it to
prove the criterion fails on base and passes on the change — and the engine
already runs that command itself, twice, recording both exit codes. The verifier
is duplicating the mechanical proof and has little budget left for the thing
only it can do.

The cancelled run is the strongest evidence for this phase. `v-t1` spent 20.7
minutes and 1,989,387 input tokens, was respawned once, and ended with `the
agent process exited: the agent closed the connection` — no verdict, no board
entry, and an empty `inbox/v-t1/`. A turn that long is a turn that runs out of
room before it reaches the part that matters.

- `roles::verifier_brief` drops the two proof steps and states the engine's
  result as given: both exit codes and the output line that decided each.
- The verifier's job becomes what the criterion does not cover: an unhandled
  input, a boundary, a case the worker's own test would not catch.
- The verdict entry gains a `passed: bool`. `schedule::ruling`'s first-word
  parse stays as the fallback for an entry that omits it, and a malformed
  verdict is still a failure rather than a pass. A boolean the verifier has to
  commit to is harder to write ambiguously than a sentence the engine reads the
  first word of.
- `command` and `evidence` stay required. The engine still runs the command to
  compute `failing_first`, so the verdict is still earned rather than asserted.

**Proof**: a verifier's turn on the same task is measurably shorter than the
13-minute baseline; a verdict with no `command` is still refused; a verdict with
`passed: false` and a first word of "passed" is read as a failure; a change that
passes the criterion but breaks an uncovered case is still failed.

### Phase 13 — The plan is checked, and the budget follows it

`rehearse` computes `speedup` and its own doc comment says "below 1.5 the swarm
is not worth it". Nothing enforces it. The observed plan scored 2.0 overall
while its first rank was one task wide, so the first six minutes were
single-threaded and the score hid it. And the budget that would have killed the
run was a constant that never looked at the plan at all.

- `PlanShape` gains `first_rank_width`: how many tasks have no dependencies.
- `validate_with` returns a `Shape` violation when `speedup < 1.5` or
  `first_rank_width == 1`. It is a warning, not a refusal: the plan is accepted
  with `shape_warning` on the run, and the plan review shows it with an explicit
  approve-anyway. A genuinely sequential plan is then a decision the user makes
  rather than a loop the lead cannot escape.
- A task declaring more than eight files is a `Shape` warning too. The observed
  `t1` owned 22 and everything depended on it; a task that large is a rank of
  one wearing a task's name.
- `criterion_paths_outside` stops flagging a path owned by a task the
  referencing task depends on. A dependent may read what it depends on; only a
  write is a violation. This alone would have avoided the 17 escalations and the
  2.7-minute revision round over `tiny-harbor/tools`.
- `max_wall_secs` is derived from the plan rather than fixed:
  `max(3600, parallel_steps × median_task_secs × 2)`, where `median_task_secs`
  is a constant 600 until Phase 15 supplies the measured figure. The plan review
  shows the projection and the budget side by side, so a plan that cannot finish
  says so before it starts.

  The constant is the weak link and it is worth being precise about it. At 600
  seconds per step the observed plan projects at 50 minutes and fits the flat
  hour, so the claim in this section's preamble — that the run could never have
  finished — is not established by the constant. It is established by the
  measurement: `t1`'s worker turn was 6 minutes and its verifier's 20.7, so a
  task-pair is about 27 minutes and five of them is 135. The constant is a
  placeholder that understates a real task by more than half, and Phase 15's
  measured median is what makes the warning fire on a plan like this one. Until
  then the warning is live only for plans seven or more steps deep.

**Proof**: the observed 10-task plan produces the shape warning naming the
one-wide first rank and the 22-file task; the same plan with t2-t7 depending on
nothing produces no warning; a criterion that runs a dependency's file is
accepted; a criterion that names a file owned by a task it does not depend on is
still refused; a five-deep plan at 600s per step gets a budget above 3600 and
the review shows both numbers.

### Phase 14 — The critical path

Two costs sit on the critical path that are not work: waiting for a verdict, and
waiting for a process to start.

`deps_satisfied` requires `is_verified()`, so verification sits between every
link: t1 work, t1 verify, t2 work, t2 verify. Five links is roughly 95 minutes
for a plan whose work is 30. Separately, the observed run spent 39 seconds
between "plan ready" and `w-t1`'s first claim, and 79 seconds between `w-t1`'s
evidence and `v-t1`'s first activity — about two minutes of setup per task-pair,
ten minutes over a five-link plan.

- `deps_satisfied` accepts a dependency that is `Verifying` — evidence in,
  verdict pending — as well as `Verified`. A dependency still `Running` has no
  commit and is not satisfied.
- `Task.built_on: BTreeMap<String, String>` records the commit each dependency
  contributed, read from the merge `prepare_worker` already performs.
- A failed verdict invalidates every task whose `built_on` names it: state back
  to `Blocked`, branch discarded, `built_on` cleared, any live verifier
  cancelled. The task re-runs when the dependency is verified again.
- `is_stuck` and `wedge` handle a task returning from `Running` to `Blocked`
  without reading it as a dead run.
- Worktrees are created and member processes spawned during plan approval, not
  on the first transition. A pre-warmed member is `Starting` with a live pid and
  no turn; its first turn costs the model call and nothing else. Pre-warm is
  bounded by the plan's `max_parallel`, so a 10-task plan does not boot 21
  processes to use four.

**Proof**: a two-task chain starts task 2 while task 1 is still verifying,
asserted from board timestamps; a failed verdict on task 1 discards task 2's
branch and re-queues it; the re-queued task builds on task 1's new tip; a
dependency that fails twice does not push its dependents past the attempt
ceiling; the gap between plan approval and the first claim is under five seconds
on a pre-warmed run.

**Status**: speculative start with rollback is implemented and proven — the
first four proofs hold, the fifth does not exist because pre-warm does not.

**Pre-warm is deferred, and the reason is that its proof was wrong.** The fifth
proof above measures the wrong interval. The `Claim` board entry is posted in
`advance`'s `Transition::Start` arm (`engine.rs:4491`) *before* `spawn_for_task`
runs, so "approval to first claim under five seconds" is satisfied today with no
change at all — the e2e harness measures 241 ms. It says nothing about process
boot, which is the cost pre-warm exists to remove.

What pre-warm actually needs, none of which is a one-line call from
`start_running`:

- `spawn_for_task` split into "prepare trees, spawn process, record member" and
  "build brief, drive turn", because the brief needs `base_commit` from
  `prepare_worker` and the verifier's brief needs the proof from
  `prepare_verifier`.
- A `warm: BTreeSet<String>` on `Inner`. The `Start` handler treats any existing
  bridge as a retry and drops it, so a pre-warmed member would be killed and
  respawned, losing the entire benefit.
- Pre-warm overlapping the drive loop rather than blocking `approve`, or the user
  simply waits the boot time at the approve button instead of at the first claim.
- Clearing `warm` on invalidation, `stop`, `delete` and `release_bridges`.
- A discriminating test. The scripted agent in `tests/swarm_e2e.rs` boots in
  milliseconds, so "the first turn costs the model call and nothing else" is not
  observable there. The honest assertion is that the member already has a live
  pid when its `Claim` is posted, which needs the pid recorded on the board entry
  rather than polled.

The correct proof, when this is picked up, is the interval from approval to the
worker's first *token*, not to its claim. Until that interval is measured on a
real run, the size of the win is unknown: a real `vibe-acp` boot is seconds, and
over a ten-task plan at four-wide that is tens of seconds against a run measured
in tens of minutes.

### Phase 15 — The run remembers

The reflection pass is the swarm's only learning mechanism and nothing consumes
it. `reflect::relevant` is reached only through `gotchas_for`, which is called
only from a test. `lead_brief` receives no history, so every run plans as if it
were the first.

- `lead_brief` gains the last five runs of the same workspace: the goal, the
  plan's shape, which tasks failed and the verdict that failed them. That is the
  data that would have told this lead that a 22-file first task is a rank of one.
- `gotchas_for` is wired into the worker brief, matched against the task's own
  files. The function already exists and already matches by file; it has simply
  never been called from the run.
- Per-role turn duration and tokens are recorded on the run and shown in the
  plan review. The observed run's verifier model took 20.7 minutes against the
  worker's 6 and nothing anywhere said so; a user choosing a verifier model
  should see that number before the run rather than after it.
- The measured `median_task_secs` replaces Phase 13's constant, so the budget
  stops being a guess.

**Proof**: a second run in the same workspace receives the first run's outcome
in its lead brief; a worker whose task touches a file with a recorded gotcha
receives that gotcha and a worker whose task touches nothing recorded receives
none; the plan review shows a per-role median once two runs exist; the derived
budget moves when the median moves.

### Phase 16 — The timeline

The panel's `Lanes` already draws real wall-clock spans from `view.window`, at
15 pixels per lane and one row per task. It is the right idea at the wrong
resolution: it cannot show that a task's verifier took three times as long as
its worker, which is the single most useful thing to know about a slow run.

- The timeline gains member-level rows: a worker row and a verifier row per
  task, with real spans, the current moment marked, and the critical path
  highlighted. Selecting a row selects the member.
- Each member card gains a token-rate sparkline over its turn — 40 by 12 pixels,
  drawn from the readings Phase 11 already records. A rate says whether a member
  is doing anything; a total says only what it has cost.
- The graph marks the node whose task is `Running` with the existing `vd-aura`,
  so the shape of the run and the place it is working are the same picture.

Per the anti-slop rules this adds no colour, no gradient and no glass: it reuses
the `--vd-*` tokens, the existing `vd-numeric` type and the existing motion
durations. Fancy here is resolution and honesty, not decoration.

**Proof**: on a completed run the timeline shows the verifier's span as longer
than its worker's for `t1`, matching the recorded timestamps; the sparkline is
flat for a parked member and non-flat for a working one; the critical path is
visually distinct from the other rows; the panel renders 21 members without
scroll jank.

### Phase 17 — Quality of life

- The roster header counts `queued` separately from `working`, `parked` and
  `broken`.
- A blocked task names the dependency it waits on and that dependency's state.
- `j` and `k` move the selection through the roster.
- The board shows the selected member's live activity inline.
- A run driven by another process says so in the header and offers to stop it,
  rather than showing a Stop button that cannot work.

**Proof**: each is visible in a live run and named in the phase record.

### Phase record — 10 through 17

Verified by `cargo test --lib` (1381), `cargo test --test swarm_e2e` (6),
`node --test tests/*.test.ts` (290), `npx tsc --noEmit` and `npm run build`.
The one `cargo build` warning is pre-existing (`DeleteAction.reason`).

| Phase | Landed | Proof |
| --- | --- | --- |
| 10 | `Queued`/`Idle`/`Waiting` reachable, `turn_started_at`, per-member ledger | roster states observed in a run |
| 11 | `Activity` ring, `Member.activity`, scoped `snapshot`, detail pane | 35,671 B for 21 members vs 489,451 B unscoped |
| 12 | `criterion_proof`, `VerifierPrep`, `BoardEntry.passed` | a criterion that passes on base is refused before a worker spawns |
| 13 | `ShapeWarning` channel, task-size cap, derived budget | the observed plan warns; a flat plan does not |
| 14 | speculative start, `built_on`, transitive rollback | `w-t2` claims before `v-t1` rules, from board timestamps |
| 15 | `history.rs`, gotchas in the worker brief, `role_usage`, measured median | a second run is told what the first learned |
| 16 | `Member.spans`, `Activity.series`, `memberTimeline`, `RateSparkline` | verifier row 1242 s against worker 360 s |
| 17 | `queued` count, `describeTask`, `j`/`k`, inline activity, stop request | probe output plus 24 tests |

**Not done, and named as not done:**

- **Pre-warm** (Phase 14, second half). Its stated proof measured the wrong
  interval — see the note under Phase 14. The design it needs is recorded there.
- **No live run has exercised Phases 15-17.** The proofs for those phases are
  unit tests, the e2e harness with its scripted agent, and a probe against a run
  document. Phase 17's cross-process stop is tested at the seam
  (`honour_stop_request`), not through two real processes. A real run is the
  remaining evidence, and it is the one thing this record cannot supply.

**Known costs accepted:**

- A dependent invalidated by a failed dependency has that attempt refunded, so a
  flaky dependency does not spend its dependents' retry budget. The refund is one
  attempt and only when the discarded attempt was not the task's own failure: a
  `Ready` task carrying a failed verdict keeps that attempt, or a task that
  cannot pass its own verifier would be rescued indefinitely by a dependency that
  keeps retrying.
- The 21-member snapshot's worst case is 76,978 B, up from 35,671 B, because
  every member carries a 40-point rate series. The bound is set on the worst case
  because that is the one a regression would hit.
- `Activity.series` is cleared when a turn ends, so a member that just finished a
  long turn shows a flat sparkline. That is what makes "flat means not spending"
  true.

---


## 13. What this deliberately does not do

- **No subagent visibility.** ACP exposes none. Members are real sessions.
- **No second control plane.** The mailbox is the harness's, not a new MCP
  server.
- **No auto-conflict-resolution.** A conflict stops the run.
- **No write-parallel swarm outside git.** Refused, with the read-only
  alternative offered.
- **No invented models.** Roles map to configured aliases only.
- **No token-free claim.** Parking is free; waking costs the context re-send.
- **No agent count theatre.** The roster shows real processes with real pids.
- **No new capability id.** The swarm is a seventh *entry* in the capabilities
  panel, above the six, but not a seventh `CapabilityId`. The six are a
  *reduction* ledger: the headline is `Math.max(0, bytesIn - bytesOut)` summed
  over every record (`CapabilityPanels.tsx:459-462`). The swarm *spends*.
  Because of the clamp, a swarm record would not go negative — it would render
  as a capability that saves zero bytes: a toggleable row with a boundary chip,
  in the one place the app reports what it saved, doing nothing measurable in
  that ledger's terms. And `"N of 6 on"` would become `"N of 7 on"`. So the
  entry is a sibling element outside the array the ledger sums, and it shows
  run state and expenditure instead of saved bytes.
- **No permanent swarm entry in the rail.** `Rail.tsx` is "navigation, not a
  directory of features". A run entry appears while a run is live and goes away
  when it ends.
- **No second memory system.** Reflection writes into the existing store.
- **No full-trace handoff between dependent tasks.** A decision digest, and
  §5.7 says why that is a deliberate departure from Cognition's principle.

---

## 14. The strongest case against this plan

It may not be worth building.

Anthropic measured multi-agent systems at ~15× the tokens of a chat and found
that token usage alone explains 80% of the performance variance — the gain is
mostly *spending more tokens*, and it is largest on breadth-first research, not
on code. They say plainly that most coding tasks have fewer parallelizable
parts than research. Cognition argues the same from the other side: parallel
writes carry implicit decisions that conflict, and the merge is where the
architecture fails.

The plan's answer is narrow and testable: the swarm is offered only when the
plan proves disjoint writes and independent acceptance, the verifier produces a
command rather than an opinion, and the merge is attributable or it stops.

**The quality claim, stated so it can be falsified.** The swarm's quality does
not come from more intelligence per agent. It comes from context partitioning:
each member carries its task, its worktree and what was addressed to it, and
nothing else. That is measurable — if any member's context approaches its
model's window, the partitioning failed and the run is worse than a chat, not
better. Phase 7 records per-member context size for exactly this reason.

If Phase 6 cannot show a real three-task run merging cleanly and finishing
faster than a single agent on the same task, the honest outcome is to ship the
verifier and the reflection pass as single-agent features and drop the swarm.
That is a real possible ending, and the phases are ordered so it is discovered
at Phase 6 rather than at Phase 9.

---

## 15. Decisions that need the user

**Settled by the user:**

- The swarm belongs with the six capabilities: a seventh entry in the
  capabilities panel, above them, with space, dim until all six are on and
  radiating orange once they are. Recorded in §6 and §9.
- A blocked `/swarm` opens the panel rather than only disabling the command, so
  the failing precondition is always readable.
- **Speculative dependency start, with rollback.** A dependent starts when its
  dependency commits, not when it is verified. A failed verdict discards the
  dependent's branch and re-queues it. Recorded in Phase 14.
- **The engine proves, the verifier probes.** The verifier stops re-running the
  mechanical criterion the engine already runs twice, and spends its turn on
  what the criterion does not cover. Recorded in Phase 12.
- **The plan shape warns rather than refuses.** A plan below 1.5 speedup or with
  a one-wide first rank is accepted with a visible warning and an explicit
  approve-anyway. Recorded in Phase 13.
- **The wall clock budget is derived from the plan**, not fixed at 3600s:
  `max(3600, parallel_steps × median_task_secs × 2)`, shown beside the projection
  at approval. Recorded in Phase 13, refined in Phase 15.
- **The interface work is the timeline, the live feed and the sparklines.** No
  replay scrubber. Recorded in Phases 11 and 16.

**Still open:**

1. **Concurrency default and ceiling.** Proposed 3 / 6, set from Phase 0's
   measurement.
2. **Where the integration branch lands.** Proposed: a local branch in the
   workstation, left for review, never pushed.
3. **Whether the lead may write at all.** Proposed: yes, on the integration
   branch only, for the merge commit and nothing else.
4. **Whether reflection writes to the shared memory store or a swarm-scoped
   one.** Proposed: shared, `type: gotcha`, with a source ref.
5. **Whether deleting a run deletes its member traces.** Proposed: yes, with
   the count and size shown first, and the run's own record kept.
6. **Whether `Table` gains a per-row `action`.** Proposed: yes — it is the same
   field `DiagramNode` already has, and it makes every table clickable rather
   than only the swarm's.

---

## 16. Cross-reference: what the harness already owns

This is the table that drove revision 2, and the lower block is what revision 3
added after crossing the plan against the six capability panels. Every row is a
place the plan was about to build something the app already has.

| Plan element | Harness feature | Decision |
| --- | --- | --- |
| MCP control plane | proposal mailbox, `sources.rs:40-100, 642-810` | **replaced** — reuse the protocol, drop the server |
| Agent learns the protocol | `preamble.ts:20-50` | **reuse** — one paragraph, same channel |
| Plan approval | proposal UI + receipt | **reuse** — one decision per run |
| Graph renderer | `PanelBlock::Diagram`, `panels.rs:477-560` | **reuse** — layered layout, tone, node actions |
| Roster renderer | `PanelBlock::Table`, `panels.rs:355-362` | **reuse** — plus an optional row action |
| Board renderer | `Transcript` component | **reuse** — a board entry is a message |
| Swarm data feed | `BuiltinName`, `data.rs:116-150` | **extend** — one variant |
| Swarm controls | `BuiltinCommand`, `actions.rs:54-91` | **extend** — six variants |
| Approval queue protection | `PROTECTED_SELECTORS`, `customize.rs:50-56` | **extend** — one selector |
| Agent discovers the feed | `manifest.rs` `PanelBindings` | **extend** — publish it |
| Run store | `chats.rs:107-225` state + directory + trash | **reuse the shape** |
| Swarm settings | `Settings`, `settings.rs:12-35` | **extend** |
| Per-member terminals | `TerminalRegistry` inside `Inner` | **free** — per-process members |
| Reflection | `capabilities/memory` | **reuse** |
| Adversarial test discipline | `battletests.rs` | **mirror** — the mailbox needs its own hostile cases |
| Capability ledger | `CapabilityId`, `capabilities/mod.rs` | **do not extend** — see §13 |
| Live panel family | `PanelId`, `src/store/app.ts:56` | **join** — `'swarm'` beside `CapabilityPanelId`, one union member |
| Capabilities panel entry | `CapabilitiesPanel`, `CapabilityPanels.tsx:455-535` | **insert** — one sibling element above the six, outside the summed array |
| Dim / radiant accent | `--vd-accent`, `--vd-accent-wash`, `globals.css:76-79, 122-125` | **reuse** — the app's one hot orange and its one glow |
| Panel render branch | `src/App.tsx:443-461` | **extend** — one branch, same shape as the six |
| Panel icon | `PANEL_ICON`, `CapabilityPanels.tsx:446-453` | **extend** — one entry |
| Preconditions | `CapabilityRecord.ready/blocked`, `src/lib/capabilities.ts:60-73` | **reuse the shape** — pre-flight, not run-start |
| Run accounting | `CapabilityStats`, `capabilities/mod.rs:198-213` | **reuse the shape** — counters + capped recent list |
| Stats presentation | `Ledger`, `CapabilityPanels.tsx:283-330` | **reuse** — expenditure and elapsed, not saved bytes |
| Settings split | `CapabilitySettings`, `capabilities/settings.rs:18-29` | **mirror** — `SwarmSettings` in `Settings`, panel is the live surface |
| Rail rule | `Rail.tsx:1-20` | **obey** — run entry while live, no permanent icon |
| Panel routing | `setPanel`, `src/store/app.ts:664` | **free** — generic over the union, no change |

The one thing the harness does not have and this plan must build is the git
lifecycle: there is no git code in the app at all. That is the real new
surface, and it is the one with the ownership and cleanup rules that cannot be
borrowed.
