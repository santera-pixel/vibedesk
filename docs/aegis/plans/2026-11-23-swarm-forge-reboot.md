# Forge — ground-up replacement for the swarm runtime

Status: **plan, not implementation.** Nothing in this document is built. No phase has been
executed, no capability claimed. Approval to build is a separate decision from the design.

- Date: 2026-11-23
- Supersedes: the swarm runtime in `src-tauri/src/swarm/` (24 modules, ~9k lines)
- Evidence: three live runs, two code audits, one research digest (sources in §13)
- Design process: three passes — a first draft from first principles, a second pass that
  attacked that draft against the code audit and the research and changed it (no branches,
  no long-lived members, tolerance instead of pixel hashes, speculation gated on oracle
  strength), and this document as the third, consolidated. The revisions the attack forced
  are recorded in §12 rather than silently applied.

---

## 1. Goal and non-goals

**Goal.** A system that takes one goal and a repository, and returns an integrated,
verified result, where:

- a run that stops making progress says so, names the node, and keeps the rest of the
  graph alive;
- no artifact is accepted because an agent wrote "passed";
- the judge cannot produce what it judges;
- cost and time are predicted from measured history, not constants;
- a killed run leaves nothing behind;
- every state the UI shows is derived, so a state that did not happen cannot be displayed.

**Non-goals.** Not a chat orchestrator. Not a resumable multi-day session manager. Not a
replacement for the single-agent path — VibeDesk's normal chat stays the default; Forge is
what runs when a goal is decomposed into independently verifiable work. No multi-repo, no
remote execution, no cloud CAS.

**Measured baseline it must beat** (run `run-20261122-232040-000`, workspace
`D:\VibeDesk\VibeDeskProjects\LivingMiniture`):

| | Observed |
|---|---|
| Tasks planned / verdicted / passed | 19 / 7 / 3 (t1–t3, each only on retry) |
| Tasks failed outright | 4 (t4, t5, t6, t11) |
| Tasks never verdicted | 12 (t7–t10, t12–t19) |
| Integration | `state: pending`, `merged: []` — the suite node never ran |
| Largest worker branch | 10 files (t8); integration branch held none |
| Working tree at failure | only `SPEC.md` and `.gitignore` |
| Tokens | 131,678,775 input, 124,363,392 cached (94.4% hit rate), **7,315,383 billed** |
| Money ceiling | `spendMicros: null`, `spendPct: null` — inactive, `unpricedModels: ["vibedesk/deepseek"]` |
| Clock | died at 2964 s of a 3600 s wall clock — 636 s of its budget unspent |
| Workers that ever took a turn | 8 of 19 (`w-t1`–`w-t6`, `w-t9`, `w-t11`) |
| Verifiers that ever took a turn | 1 of 19 (`v-t11`), yet 7 verdicts exist |
| Death | `w-t7` turn in flight 1801 s, zero restarts, run failed 7 s after the last board entry |

The run did not exhaust budget and did not exhaust wall clock. It exhausted **liveness
bookkeeping**, and the last three rows of that table are why. The verifier row is not a
typo: six verdicts were produced by members whose turn counters did not survive their own
restart, which is a measurement defect in its own right (§8.3).

---

## 2. The diagnosis, in one paragraph

Every **orchestration** defect across three live runs and two code audits is a consequence of
one choice: **the unit of truth is a state word.** `working` is a word, so five of eight
worktrees held nothing while reporting it. `passed` is a word, so a verifier could author
the artifact and then pass it. `pending` is a word, so integration could be the end state of
a run that produced one artifact in thirty. `state: failed` is a word written by a code
path, so a wedged turn could end a run that twelve tasks were still waiting on. Because the
truth was a word, liveness had to be *inferred* from proxies — commit cadence, HEAD probes,
timestamps — and each proxy is both false-positive (a looping agent commits garbage) and
false-negative (a 30-minute render commits nothing). Forge replaces the word with a hash and
the prose plan with a graph, and derives every displayed state from an append-only event log.

The qualifier matters. Behavioural defects are a **different class** and this architecture
does not fix them: a material factory emitting `emissive: undefined` on every call, a sun
direction inverted between dawn and noon, a bokeh focus written where the renderer does not
read it. Those were the last run's real product defects, and no amount of orchestration
correctness catches them — only an oracle that executes the code can. Conflating the two
classes is how a rewrite convinces itself it fixed something it did not touch, so §6 treats
oracle strength separately and §11.1 names it as the binding constraint.

### 2.1 The exact mechanism, with line numbers

Four clocks govern liveness: `MEMBER_IDLE_LIMIT_SECS` 600, `MODEL_SILENCE_SECS` 300,
`MEMBER_TURN_CEILING_SECS` 1800, `LEAD_NUDGE_SECS` 180 with 2 nudges. The defect is not
that there are four. It is the **guard order**, and it produces a rule the system's own
authors would not have chosen:

> A member with no work in flight is restartable. A member with real work in flight is not.

Two functions own liveness, and they disagree about what a turn in flight means.

**`respawn_dead`** (`engine.rs:3391`) never touches a busy member. Its guards, in order:
`!watchable(member) || member.respawns >= MAX_RESPAWNS` (`engine.rs:3396`) → skip; the task
is not in the state this role serves → skip; `!expected || busy(&member.id)` →
**skip**; `silence_secs` under `idle_limit()` → skip. So a member whose turn is in flight is
not eligible for restart at any respawn count. The five members that took one
`restartsTimeout` and kept working were all *not* in flight — their silence exceeded 600 s
with nothing running, which is the only case this path handles.

**`wedge`** (`engine.rs:3723`) is the path that meets the busy member, and it has no restart
branch at all. It computes `idle` from `last_at`, which only moves when a member writes to
the board; `in_flight = busy(&member.id)`; `model_silent = in_flight && !pulse.tool &&
quiet > MODEL_SILENCE_SECS`; then:

- `limit = if in_flight { MEMBER_TURN_CEILING_SECS } else { idle_limit() }`
- if `model_silent || idle > limit` → the member is `Failed` **and the run is set to
  `Failed`** (`engine.rs:3824`).

`w-t7`'s turn was in flight 1801 s with `model_silent` false — a *tool* was running, per the
pulse. So it got the 1800 s ceiling, exceeded it by one second, and ended the run. Six
verifiers produced seven verdicts while showing `turns: 0`, so a restart also loses the
counters that a cost model would read.

That is the mechanism in one sentence: **the ceiling is a kill switch, not a supervision
boundary.** There is no state in which a long legitimate turn is supervised — it is either
invisible to the restart path or fatal to the run. And the workload makes it inevitable
rather than unlucky: a 40-second film render is exactly 30 minutes of quiet work.

The other two paths compound it. `infra_overloaded` (`engine.rs:2689`,
`MAX_INFRA_RESTARTS = 3` at `:2612`) charges a *shared* cause per member but punishes
globally, so one flaky provider window can fail the run through members that did nothing
wrong. `fail_member` (`engine.rs:2931`) marks a member `Failed` and leaves its task
`Running`, which `is_stuck` reads as movement — the run stalls instead of failing.

`wedge`'s run-kill is not gratuitous; it exists because there is no re-plan path. Failing a
node with no way to re-plan its subtree leaves the graph unsatisfiable, so ending the run is
the only consistent action available to it. L4 in §3 exists to give it that path, and §7
deletes `wedge` rather than raising its ceiling: a longer ceiling only moves the cliff,
and the film's render time is not knowable in advance.

---

## 3. The five laws

L1. **Satisfied only by evidence.** A node is satisfied when its artifact is in the store
*and* its oracle returned pass, with evidence recorded: artifact hash, oracle id, exit
status, evidence bytes, timestamp. No other path to satisfied exists in the type system.

L2. **Inputs are hashes.** A node's inputs are artifact hashes. Re-producing an input to a
different hash invalidates its dependents transitively; re-producing the same hash
invalidates nothing (early cutoff). Cutoff is defined on **artifacts**, never on calls —
a model call at temperature > 0 is non-deterministic and can never be assumed to repeat.

L3. **Approve the graph, not the transcript.** A run cannot enter `running` without an
`Approved` event. The approval surface is three numbers and a diffable interface set:
predicted cost, predicted critical path, node count — not 19 prose specs. Approval is
invalidated by an interface change or by projected cost exceeding the approved figure by
more than 25%; neither requires re-approving the whole graph.

L4. **Liveness is per-attempt silence, classified.** Each attempt has three budgets —
`queue_wait`, `no_event`, `total` — and each failure has a class — `infra`, `timeout`,
`semantic`. An `infra` failure does not consume the node's semantic attempt budget.
Exhausting a node's semantic budget fails **that node** and re-plans its subtree; it aborts
the run only when the sink becomes unreachable.

L5. **The judge has no write capability.** The oracle runner materialises a read-only view
of the artifact and its only output channel is evidence bytes keyed to the artifact hash.
It is not a policy refusal — there is no writable tree in its process's world.

---

## 4. Architecture

| Component | Owns | Replaces from the old engine |
|---|---|---|
| **Store** | content-addressed trees, manifests, evidence blobs, GC policy | worktree-per-member, per-member branches |
| **Graph** | typed nodes: interface, spec, input hashes, owned paths, oracle | prose plan + `Task`/`Member` documents |
| **Compiler** | goal → graph, plus the checks in §5.5 | lead chat that emits task prose |
| **Scheduler** | admission control, critical path, speculative fan-out, re-plan | rank/FIFO over a fixed slot pool |
| **Executor** | one supervised attempt per node; job-object adoption; tree production | per-member liveness clocks, `respawn_dead`, `wedge` |
| **Oracle runner** | command / mutation / visual / human; evidence capture | `prove.rs` two-sided proof, verifier agents |
| **Ledger** | append-only events; snapshot as a fold; replay; fork | `run.json` whole-document rewrite per tick |
| **Surface** | panel = fold rendering: per-node pulse, units/total, spend, evidence | `swarm://changed` payload + status words |

### 4.1 Data model

```
Artifact  { hash, manifest: [path -> blob_hash], producer: {node, attempt, model} }
Node      { id, kind, spec_hash, interface_hash, inputs: [artifact_hash], owns: [glob],
            oracle: OracleSpec, attempts: budget, deps: [node_id] }
Attempt   { id, node, model, started_at, budgets, events, outcome, artifact_hash?, evidence? }
Event     { seq, at, kind, subject, payload }        // the only writer of truth
Snapshot  { fold(events[0..n]) }                     // materialised view, never authored
```

`Artifact`, `Node`, `Attempt`, `Event` are append-only or immutable. Nothing is mutated in
place, so there is no "the save overwrote the intermediate state" class of loss and no
reconciliation pass.

### 4.2 Tree production replaces branches

An attempt runs in a directory seeded from its input artifact trees plus read-only
dependency trees. On completion the executor walks the resulting tree, **rejects any path
outside the node's `owns` set at production time**, and stores the result. There is no
branch, no worktree identity, no `-b`, and nothing registered with git.

Each live defect this removes:

| Old defect | Why it cannot recur |
|---|---|
| Verifier wrote the missing files, committed them as `e18875b`, repointed the worker's branch (`run-20261122-131108-001`, t2) | there is no branch to repoint; oracle output is evidence, not a tree |
| Two out-of-grant files stayed on the branch and entered the reviewed diff (fault 2) | out-of-grant paths are rejected before any oracle sees the artifact |
| `git worktree remove --force` → "Permission denied", four orphaned worktrees and branches (fault 6) | attempts are directories, deleted once their artifact is stored |
| Foreign-author check needed at integration (`integrate.rs:96-108`) | integration is one materialisation and one commit, authored by the executor |
| `delivered_nothing` / `untouched` heuristics (`worktree.rs:788,802`) | an attempt that changed nothing produces its input's hash; "no change" is an identity, not an inference |
| `Task::is_committed = Verifying|Verified` starts dependents on unverified commits (`mod.rs:280-282`) | dependents consume a hash only after that node is satisfied (L1) |

Cost of the change, stated plainly: per-node git history leaves the working repo. The
ledger holds the producer event for every artifact, and integration can emit one commit per
satisfied node in dependency order with the node id and artifact hash in the trailer if a
reviewer wants commits. The panel diffs input tree → output tree from the store.

### 4.3 Attempts, not members

The old system holds 38 long-lived actors (19 workers + 19 verifiers) over a 4-slot pool:
slots are held by parked agents, and every worker competes with its own judge for the pool.
Forge schedules **node attempts**; a satisfied node holds nothing. Throughput becomes a
policy over attempts.

What the run file says about those 38, measured rather than assumed:

| | Observed |
|---|---|
| Workers that ever took a turn | 8 of 19 |
| Verifiers that ever took a turn | 1 of 19 (`v-t11`) |
| Worker turns | 15, costing 3,950,030 billed tokens |
| The one verifier turn | 5,961,805 input, 717,290 billed |
| Billed per worker turn | ~263,000 |
| Billed per verifier turn | ~717,000 |

So a verifier turn cost **2.7x** a worker turn, and `v-t11`'s single turn cost more than the
three cheapest workers' entire tasks combined. But verifiers account for 15% of billed spend
while workers account for 84%, because workers ran more turns. Retirement of
verifier-as-agent still saves a real quarter of the per-turn cost and removes the authorship
fault, but it is **not** the money story, and an earlier draft of this plan claimed it was.
The money story is that 11 of 19 workers never started a turn while the run spent 4.7M
billed and died with 636 s of wall clock left: the system lost its throughput to
bookkeeping, not to a judge.

One further defect surfaces from the same table, and it matters because §8.3 derives cost
from exactly these counters: six verifiers produced verdicts while reporting `turns: 0` and
zero tokens. Per-member accounting does not survive a restart, so the per-role attribution
above is a **floor** — 129.1M of 131.7M input is attributable to members, and the residual
2.58M is spend the fold saw and no member owns. An append-only usage event per turn is not
a nicety; it is the difference between a cost model and a guess.

---

## 5. The five mechanisms, in their revised form

### 5.1 Interfaces are nodes, so freezing is cheap and invalidation is exact

An interface (types, units, invariants, error taxonomy) is produced by its own node and
hashed before consumers run. Edges are therefore input-addressed by construction, while
leaves are content-addressed — which is how the standard conflict between content
addressing and contract-first design resolves, per layer rather than globally.

The reason this is a mechanism and not a formality: a frozen interface is *wrong some of
the time by construction*. Maven data shows 27–30 breaking changes even in minor and patch
releases, and 6.8% of update pairs contain one. So the freeze must be cheap to revise:
changing an interface produces a new interface hash that invalidates exactly its transitive
closure and nothing else. In the last run, 15 modules were planned on top of `t1`/`t2`
contracts that were still failing (`emissive: undefined` on every material call; sun
direction inverted at `clock.js:124-141`) — that is the class this closes.

One thing this would otherwise break, stated because it is the obvious objection: re-running
an interface node at temperature > 0 produces different bytes, so "invalidate exactly its
closure and nothing else" would be false. It holds because **an attempt is created only when
its key is absent from the store.** A key is `(spec hash, interface hashes, input hashes,
model, prompt version, tool scope)`. If the key exists, the artifact is reused and nothing
re-runs. A second execution therefore only ever happens when someone deliberately re-planned
the node — in which case the invalidation is the intent, not a side effect. Churn comes from
non-determinism only where the system has chosen to re-plan, never from a scheduler that
re-runs for its own reasons.

### 5.2 Oracle-typed acceptance

Each node declares one of four oracle kinds, and the scheduler knows each kind's cost and
latency profile:

| Kind | Used for | Evidence | Cost |
|---|---|---|---|
| `command` | executable behaviour | exit codes on base and on change, stdout/stderr | seconds |
| `mutation` | code correctness | mutation score on changed lines, surviving mutant ids | minutes |
| `visual` | frames, shaders, composition | score + cited frame ids against reference frames | tens of seconds |
| `human` | milestone judgement | decision + note, recorded as an event | human |

Only `visual` and `human` involve a *judging* agent or a person. Authoring an oracle is a
different act from judging with one: the suite is produced by its own node, and §5.3's role
rule keeps its author distinct from the artifact's author. The old system gave every task a
verifier agent; that uniformity is what doubled actor count and multiplied context re-reads.

### 5.3 Keep the two-sided proof, then strengthen it

`is_earned = passed && failing_first && !base_dirtied` (`mod.rs:651-653`) is the one piece of
correctness structure the old system got right, and Forge keeps it: an oracle that also
passes on the base artifact is not an oracle. But it only proves the oracle *flips* — a
single assertion is enough to earn a verdict. Forge adds **mutation score on the diff**,
which is invariant to who wrote the *patch*: mutants are applied to the artifact under test,
and the suite either kills them or does not, whoever produced the change.

It is not invariant to the suite. A suite that never exercises the changed lines scores
zero on correct work, so who wrote the oracle still matters — the answer is a role rule, not
a scoring trick: the suite is a `command`/`mutation` oracle authored by a *different node*
than the one producing the artifact, and a suite the artifact's own author wrote is scored
but flagged `self_authored` in the evidence. The research is unambiguous about why this
matters: a passing LLM-written suite is evidence only of the LLM's understanding, and
supplying a natural-language spec does not fix it — generated suites are incorrect,
overspecified or incomplete across all configurations tested.

LLM-written tests keep a role, but a narrower one: as a **filter over candidates**, where
they roughly double precision on proposed fixes. That is a selection signal (§5.4), not an
acceptance gate.

### 5.4 Speculation gated on oracle discrimination

k parallel attempts are admissible only when the node's oracle already discriminates. With a
weak oracle, best-of-N selects the candidate that best games the metric, and reward
over-optimisation degrades ground truth with a measured decay curve. Weak-oracle nodes take
the serial path with a human checkpoint instead of a speculative fan-out.

Two things this has to specify, because a citation is not a mechanism:

**Discrimination is measured at oracle registration, not at admission.** A mutation score
computed on "the diff" does not exist until an attempt has run, so gating admission on it
would be vacuous. Instead each `command`/`mutation` oracle is scored once, against a
reference artifact with known injected faults (the same technique the engine's
`fail_base` check already applies in miniature: if the oracle cannot distinguish a broken
artifact from a working one, it is not an oracle). The resulting score is stored on the
oracle and reused for every attempt of every node that declares it. A `visual` oracle gets
a discrimination score the same way, from reference frames with known defects.

**The allocation rule is a comparison, not a vibe.** Attempts are serial (`k = 1`) unless all
three hold: discrimination ≥ threshold; the node's measured first-pass rate is below 1/3;
and the projected cost of the expected serial retries exceeds k times one attempt. Then
`k = min(3, ⌈1 / first_pass_rate⌉)`, stopping at the first candidate that passes. The
1/3 and 3 are starting values, recorded as such in §14 — the rule's shape is what matters,
and the ledger's first three runs are what replaces the numbers.

### 5.5 Plan-time refusals, and the three that are actually missing

The old engine already refuses a great deal, with a typed list rather than prose:
`Violation` (`plan.rs:63`) covers `TooFewTasks`, `MissingAcceptance`, `CriterionIsProse`,
`EmptyFileSet`, `UnknownDep`, `SelfDep`, `DuplicateId`, `Cycle`, `OverlappingFiles`,
`NoIntegrationSuite`, `BadSkillDraft`, `SkillAlreadyInstalled`; `validate` is
`plan.rs:459`, and the engine separately re-proves a criterion that passes on base
(`criteria_passing_on_base`, `engine.rs:6513`). Claiming invention there would be false.
Forge keeps every one of those and adds:

1. **Contract satisfiability** — every symbol a node imports resolves to a declared
   interface *of an upstream node*, and no two siblings' `owns` sets intersect.
2. **Critical path against deadline** — the projected longest chain, from measured node
   medians, must fit the deadline; otherwise the graph is refused and the refusal names the
   chain.
3. **Predicted cost against budget** — the sum over the graph of measured medians times
   oracle cost must fit the approved budget.

**These three do not run at the same moment, and getting that wrong makes the first one
vacuous.** Interfaces are nodes (§5.1), so at compile time there is no interface artifact to
resolve imports against. Admission is therefore two-stage:

- *Stage 1, before any implementation node exists:* ownership partition (the existing
  `OverlappingFiles` check, extended to globs), shape, `NoIntegrationSuite`, and the
  interface nodes' own specs. This is the stage that refuses impossible shapes.
- *Stage 2, after the interface nodes are produced and hashed:* contract satisfiability
  against real interface artifacts, then the critical path and cost projections, which
  consume those hashes anyway. Implementation nodes are admitted only here.

**An unmeasured projection does not pass silently.** On a workspace with no history, the
projection falls back to priors (§8.3) and is marked `unmeasured`; the approval must then
carry an explicit deadline and budget rather than inheriting one, and the surface says the
figures are priors. The failure mode this prevents is the last run's: a 45-minute assumption
nobody chose, against a graph that could not finish in that time, with the only review
happening after the spend.

Plus a wiring fix that is not a check but a state machine: `running` is unreachable without
an `Approved` event. The documented approval gate never fired on the MCP path — there is no
approval key anywhere in `run-20261122-232040-000/run.json`, and workers spawned
immediately after planning.

---

## 6. The workload this must actually satisfy

A 40-second stylized 3D film: a harbour island at 0.5x–4x speed, orbit and focus controls,
windmill and lighthouse, procedural audio, desktop and phone. It is mostly **weak-contract
work with a visual acceptance criterion**, and the last run's real defects were behavioural,
not structural: a material factory emitting `emissive: undefined` on every call, an
inverted sun, a bokeh focus written to the wrong path so it silently did nothing, and a
readiness flag publishing `{ok: true, modules: []}` for an empty app.

No import-graph check catches any of those. So:

- **Interfaces** are frozen only where a signature exists: units (metres, seconds,
  radians), coordinate conventions (Y-up, sun azimuth), the sampler's contract (`sample(t)
  -> {sun, sky, ambient}`, range-checked), and asset handles.
- **Behaviour** is caught by `command`/`mutation` oracles per module, each of which must
  fail on base: `sample()` properties (monotonic azimuth across a day, noon elevation
  above dawn), material construction with no warnings, uniforms read through the same path
  the renderer uses.
- **Frames** are caught by a `visual` oracle against three reference frames with a
  tolerance metric — structural similarity above threshold, palette distance below it —
  and by **three human checkpoints** at coarse milestones (composition, motion, final
  grade), not per node.
- **Determinism is a tolerance, not a hash.** Identical seeds across GPU, driver and driver
  version do not produce identical pixels. The determinism oracle asserts on the *inputs*
  (seed, camera path, asset hashes) exactly, and on the *output* within tolerance. Pixel
  hash equality would have failed correct work.
- **The film is a sink node with a real oracle**: fixed seed, frame count, duration within
  tolerance, zero console errors, SSIM against the approved grade. "Integration: pending"
  cannot be a terminal state, because integration is the sink and the sink is what the run
  is for.

---

## 7. What gets deleted

Anti-entropy: one owner per concern. Nothing below survives in the new path.

| Deleted | Reason |
|---|---|
| `Member` documents, `MemberState`, `wedge()`, `respawn_dead`, `fail_member`, restart counters as primary liveness | replaced by attempt budgets and failure classes; `wedge`'s run-kill exists only because there was no re-plan path |
| Worktree-per-member, per-member branches, `set_identity`, HEAD probes, `changed_paths`/`outside_grant` | replaced by tree production and store hashes; HEAD probing existed to answer "did the member do anything", which is now a hash comparison |
| Board digest truncation (`DIGEST_MAX_ENTRIES` 12, `DIGEST_MAX_CHARS` 2400, `board.rs:175`) | a node's context is its inputs and its own spec; there is no cross-task chatter to truncate |
| Verifier-as-chat-agent as the default | replaced by the four oracle kinds; an agent judges only `visual` |
| Reflection member, amendment machinery | reflection edited prose nothing consumed; re-plan replaces amendment |
| Prose plan, `PLAN_ATTEMPTS`, plan amendment text | the plan is the graph |
| `run.json` as the contract | the ledger is the contract; the snapshot is a fold |
| `DEFAULT_BUDGET_TOKENS = 25_000_000` and the fixed token/time estimates | replaced by measured medians from the ledger (§8.2) |

**Frozen behind `VIBEDESK_SWARM_LEGACY=1` for one cutover window, with a stated exit.** Two
owners of the same panel is the failure anti-entropy exists to prevent, so the flag gets a
criterion rather than an indefinite life:

- **Entry:** Forge completes three runs of the same goal on the same workspace, each
  producing the sink artifact with a passing sink oracle.
- **Exit:** the legacy modules are deleted in the PR series that follows the third
  successful run. Not "later" — the flag is removed in that series, and P6 is not closed
  while it exists. If the exit condition cannot be met, the honest outcome is that Forge
  did not replace the engine, and the plan says so rather than leaving both alive.

### 7.1 Ported, because they are proven

- Windows job-object adoption (`acp/job.rs:40,54,70`) — falsified once by stubbing `adopt`,
  then restored. Port with its test.
- Write-then-rename persistence (`mod.rs:1852`).
- File-based `stop.request`, so a run outlives the window that started it.
- Rehearsal as a pure function (`schedule.rs:566-660`) — becomes the graph's cost/latency
  projection.
- Snapshot-monolith UI with the event as a hint (`SwarmPanel.tsx:13,139-160`). What is ported
  is the **transport**: the panel re-reads a snapshot on an event, rather than maintaining
  view state from a stream. What changes is the **source**: today's snapshot is a hand-kept
  `run.json` that the engine rewrites wholesale each tick; Forge's is a fold over the ledger
  with no other writer. Keeping the transport is why §1's "every state the UI shows is
  derived" does not require a new front-end contract — the panel cannot tell the difference,
  and the difference is the point.
- The falsification tests: the job-object stub test, the `delivered_nothing` case, the
  cold-check verdict cases, `a_deleted_run_leaves_no_worktree_and_no_branch` (adapted to
  "a deleted run leaves no attempt directory and no CAS entry it does not reference").

---

## 8. Cost and money

### 8.1 Report spend, not tokens — already done, so port it

This is the one audit fault that has since been fixed in the code, and the plan records that
rather than re-fixing it. `bin/swarm-mcp.rs:353-366` now returns `cachedTokens`,
`spentTokens`, `spendPct`, `hitRate`, `unpricedModels` and `ratesVersion`; `SwarmPanel.tsx:2089`
and `swarm-view.ts:1241` both derive `spent = used − cached` for display. The last run's
`budget` object carries `spentTokens: 7,315,383` and `tokenPct: 29.26` alongside the raw pair.

Forge keeps exactly that contract: raw pair for diagnostics, billed figure and percentage as
the headline, never `usedTokens` alone as consumption. The reason it stays in the plan is
that the misreading it prevents already happened once — 131.7M against a 25M ceiling was
called a blown budget when billed spend was 7.3M — and a rewrite is precisely when a
corrected convention gets lost.

### 8.2 Never null, always derived

`spendMicros: null` on the last run is still live, and `unpricedModels: ["vibedesk/deepseek"]`
says why: `rates::price` returns `Option` (`rates.rs:150-151`, `for_model(model)?`), and one
unpriced model makes the whole figure `None`, which silently disables the money ceiling. The
run had a 25M token ceiling and no money ceiling at all.

Policy: every model resolves to a rate — for a model with no published rate, assign the
nearest published family rate and mark the entry `derived: true` in the report. A ceiling
that is off without saying so is worse than a ceiling that is approximate. Per-model rates
stay versioned (`RATES_VERSION`).

### 8.3 Estimates from history — also partly built, with the measurement itself broken

The constant 60,000 was 652x the *raw* input of the audited `w-t2` (39.1M) and about 13x its
*billed* tokens. That distinction is already in the code (`schedule.rs:419-425`), and it
matters: a forecast of raw input forecasts the cache hit rate, not the work. So the audit's
fault 4 is further along than the audit says:

- `history::role_medians` (`history.rs:175-210`) medians **fresh** tokens per role and
  refuses to report a role with fewer than two contributing runs.
- `Estimates::of` (`schedule.rs:430-475`) uses those medians where they exist, keeps the
  constants as priors where they do not, and carries `runs: Option<usize>` so a surface can
  say "not measured" instead of implying precision.

What remains, and what Forge must fix rather than port:

1. **The observations are lossy.** Six verifiers produced verdicts while reporting
   `turns: 0` and zero tokens (§4.3), so `role_medians` is computed over partial data and
   2.58M of the run's input belongs to no member. Fix: one append-only `Usage` event per
   turn, written when the turn ends, folded into spend. A counter that lives in a mutated
   document is lost by the restart it is meant to survive; an event is not.
2. **A projection that nothing enforces.** The rehearsal predicts; no gate refuses a graph
   whose projected path exceeds the deadline or whose projected cost exceeds the budget.
   Estimation exists, admission does not (§5.5).
3. **Estimates per role, not per node kind.** A render node's cost is not a worker median:
   it is dominated by a tool duration that has no model tokens at all. Forge predicts per
   (node kind, oracle kind, model), which is the only shape that can price a 30-minute
   render.
4. **Predictions are not scored.** Every projection is recorded next to its actual, so the
   system's error is a number the next run can improve — the same discipline as §5.3, applied
   to cost.

---

## 9. Phases, each with a falsifier

Order matters: a vertical slice reaches a real oracle at P3, before the scheduler grows
admission control. The old system's fatal shape was breadth before depth — 19 nodes, the
integration sink last, nothing ever merged. A scheduler built before anything can be
produced end to end cannot be falsified.

| Phase | Delivers | Acceptance evidence | Falsifier (must make it fail) |
|---|---|---|---|
| **P0** Store + Ledger + Graph types | `Artifact`/`Node`/`Attempt`/`Event`, CAS, fold, GC | hash determinism; fold reproduces state from events alone; a usage event survives a simulated restart | two GC assertions, not one: a referenced blob survives collection, and an unreferenced blob is collected. A GC that deletes nothing fails the second; a GC that ignores references fails the first |
| **P1** Executor: supervised attempts | three budgets, failure classes, job-object adoption, tree production | a killed attempt is retried per class; an `infra` failure does not consume a semantic attempt; out-of-grant path is rejected at production | force a no-event death → the *attempt* fails and the node charges one attempt, with the rest of the graph still schedulable; write a foreign path → the artifact is refused. Re-plan is P4 and P1 does not claim it |
| **P2** Oracles | `command` + `mutation`, evidence capture, `is_earned` + mutation score | an oracle that passes on base is refused; a one-assertion oracle scores below threshold and is not accepted; oracle discrimination is recorded at registration | delete the assertion → mutation score drops → node unsatisfied; feed the oracle a reference artifact with a known fault → it must reject, or it has no discrimination score and cannot gate speculation |
| **P3** Vertical slice on the real workload | interface node + one scene node + render node + visual oracle + one human checkpoint | a scene reaches a graded render through the graph, end to end, with evidence | flip the sun convention → the visual and command oracles disagree with the reference → unsatisfied |
| **P4** Scheduler | admission control, critical path, re-plan, cost-aware speculation | projected cost/path scored against actuals; a graph over deadline is refused before spend; a failed node's subtree is re-planned | submit a 19-node chain on a 4-slot pool with a 45-minute deadline → refused, naming the chain; exhaust a mid-graph node's semantic budget → its subtree is re-planned or refused by name, and the run survives |
| **P5** Surface + cutover | graph approval, per-node evidence, spend, units/total; legacy flag on | panel shows only derived state; run enters `running` only via `Approved` event; **three runs measured against the §1 baseline**, reporting artifacts produced (baseline 1), tasks verdicted (7 of 19), billed spend (4.73M), and wall clock to sink (never reached) | delete the approval event → the run cannot start; show a status word the ledger does not contain → it must not render |
| **P6** Retirement | remove legacy engine, its constants, its board, its worktrees | old modules gone; no second owner of the panel; flag deleted per §7's exit criterion | re-enable the flag → nothing listens |

Each phase is a working slice, not a scaffold. P0 is the only phase without agents, and it
is deliberately not user-facing: it is a library with tests, which is a working slice of the
store rather than a surface implying a capability.

---

## 10. TDD route and how this gets verified

**Route: strict for P0–P2, then test-first per node thereafter.** Stated explicitly because
it contradicts an earlier instruction in this project: after a series of wasteful smoke
tests, testing was forbidden for the *previous* bug fixes. That instruction does not
transfer here, because in Forge the tests **are the product** — an oracle is a test, and a
system whose oracles are unverified is the exact failure mode §5.3 describes. The ban on
smoke tests stays in force in its real form: no test that starts a process and checks it is
alive, no assertion that passes because the capability is disabled, no screenshot of zeros.

Verification for this plan itself, before any phase starts: the phase table above is the
only acceptance contract, and each falsifier must be executed when its phase lands — a
green suite that the falsifier leaves green is a dead test.

---

## 11. Red flags

1. **Oracle quality is the entire system.** Mutation scoring covers code only. A visual
   metric is a proxy; an agent can learn the metric and miss the goal. Mitigation: hold-out
   frames the agent never sees, human checkpoints, and oracle authors with no access to the
   artifact — but the risk is reduced, not removed.
2. **Removing branches removes git's conflict detection.** The plan-time glob partition and
   the production-time path rejection are *one specification applied at two moments*, not two
   independent defences — an error in the `owns` set defeats both. They are separated because
   the second catches an executor bug (a walk that misses a path, a glob that resolves
   differently at runtime), not because it catches a specification error. Independence has to
   come from somewhere else: the materialiser verifies the assembled tree against the stored
   manifests before integration, and that check reads the artifact, not the graph. Without all
   three, a silent wrong-merge becomes possible for the first time in this system.
3. **CAS growth and GC.** Free of worktrees, not free of disk. `RETAINED_RUNS = 10` has no
   direct analogue; GC must be written in P0, not deferred, and must never collect a blob a
   live or resumable run references.
4. **Replay is control-flow only.** Replay re-reduces the graph against recorded model
   responses. "Why did it choose that" is still a human read of the ledger. Do not sell
   replay as reproducibility of thought.
5. **Cost multiplies without admission control.** Speculation, mutation runs and visual
   oracles each cost more than the thing they replace. The gate is load-bearing.
6. **The rewrite can lose falsification evidence.** The old code carries tests that caught
   real bugs (job object, `delivered_nothing`, cold-check verdicts, 672 unit tests). Port
   the tests, not just the mechanisms, or the rewrite silently regresses.
7. **A dull plan is still possible.** The gate refuses impossible shapes, not boring ones.
   Shape remains a human decision, made once, on three numbers.
8. **Weak-contract work does not parallelise for free.** The measured spread between
   decomposable and sequential tasks is large and negative for sequential work; the film has
   a genuinely serial spine (contracts → scene → render → grade). Forge must not pretend
   the spine is parallel, which is why the sink is a node and the path is projected.

---

## 12. Decisions this plan makes that a reviewer may want to reverse

| Decision | Alternative | Why this way |
|---|---|---|
| No per-node git branches | keep branches, enforce authors at integration | branches were the vector for all six reconciliation defects; hashes make the class unrepresentable |
| Attempts, not persistent members | keep members with restart budgets | a resident agent that is not working still holds a slot and competes with its judge |
| Instrumentation partly in agents, not fully | all-program oracles | a "does this look right" question has no program; the honest answer is an agent with cited frames and a human backstop |
| Mutation scoring as the strength gate | coverage, or LLM judgement | coverage and LLM judgement are both defeated by the party being judged; mutation score is not |
| Tolerance metrics for frames | pixel hash equality | hardware and drivers make hash equality a false negative on correct work |
| Derived spend with `derived: true` | null until a rate is published | a null ceiling silently disables budget enforcement, which already happened |
| Per-attempt budgets sized by node kind | raise the global 1800 s ceiling | the film's render time is not knowable in advance; a longer ceiling moves the cliff without adding supervision |
| Two-stage admission | one gate at compile time | interfaces are nodes, so contract satisfiability has nothing to resolve against until they are produced |
| Usage events instead of member counters | keep counters in the snapshot | six verdicts came from members reporting zero turns; a counter living in a mutated document is lost by the restart it is meant to survive |
| Delete `wedge` rather than give it a restart branch | add the branch and keep the ceiling | the branch is only consistent once a re-plan path exists (P4); patching it earlier keeps a run-kill as the fallback and calls it supervision |
| Record the revisions this design survived | present the final design as if first proposed | the attack pass changed four load-bearing decisions (branches, members, determinism, speculation); hiding that makes the design look more obviously right than it was, which is how reviewers stop checking |

---

## 13. Research basis

Build-system semantics — action graph, verifying traces, early cutoff, CAS, action-hash →
result metadata: *Build Systems à la Carte* (Mokhov, Mitchell, Peyton Jones,
https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems.pdf);
Bazel remote caching (https://bazel.build/remote/caching) and build progress as
completed/discovered actions (https://bazel.build/run/build); Nix content-addressed paths
(RFC 62, https://raw.githubusercontent.com/NixOS/rfcs/master/rfcs/0062-content-addressed-paths.md).

Durable execution — event history as the record, replay of control flow, effects hoisted to
activities, non-determinism as a liveness bug: Temporal
(https://docs.temporal.io/encyclopedia/event-history,
https://docs.temporal.io/encyclopedia/event-history/event-history-python); Azure Durable
Functions orchestrator constraints
(https://learn.microsoft.com/en-us/azure/durable-task/common/durable-task-code-constraints).

Supervision and liveness — restart intensity and period, product of intensities, temporary
children (https://raw.githubusercontent.com/erlang/otp/master/system/doc/design_principles/sup_princ.md);
liveness vs readiness vs startup, and Job `podFailurePolicy` / `backoffLimitPerIndex` /
`maxFailedIndexes` as the right primitive for expensive non-idempotent work
(https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/,
https://kubernetes.io/docs/concepts/workloads/controllers/job/); per-phase timeouts
schedule-to-start / start-to-close / heartbeat
(https://docs.temporal.io/encyclopedia/detecting-activity-failures).

Verification that cannot be gamed — mutation testing at scale (arXiv:2102.11378); LLM-written
suites are evidence of the model's understanding only, spec does not fix it, all
configurations (DOI 10.1145/3832783.3844564); generated tests as a candidate filter
(SWT-Bench, arXiv:2406.12952); compute-optimal sampling (arXiv:2408.03314, arXiv:2203.07814);
reward over-optimisation decay (arXiv:2210.10760); design-by-contract as an external oracle
(https://www.eiffel.com/values/design-by-contract/introduction/).

Decomposition and orchestration — separate context from state, artifacts not prose between
agents, synchronous delegation as a liveness defect, token volume as the dominant variance
term (https://www.anthropic.com/research/multi-agent-research-system); conflicting implicit
decisions from split context (https://cognition.com/blog/dont-build-multi-agents); measured
architecture spread, including large negatives on sequential planning, and the advantage of
centralised verification (arXiv:2512.08296); worktrees as isolation and their leaked
worktrees/locks (https://code.claude.com/docs/en/worktrees); spec-driven checkpointing
(https://github.blog/ai-and-ml/generative-ai/spec-driven-development-with-ai-get-started-with-a-new-open-source-toolkit/);
interface churn in practice, 27–30 breaking changes even in minor/patch releases
(https://jstvssr.github.io/assets/pdf/semantic-versioning-maven.pdf).

---

## 14. What is not proven here

- No code exists. Every performance claim in §8 is arithmetic over the recorded run, not a
  measurement of Forge.
- The mutation-score threshold, the SSIM tolerance, the 25% re-approval band, the 1/3
  first-pass trigger and `k ≤ 3` are starting values with no calibration run behind them.
  The shape of each rule is argued; the numbers are placeholders that the first three runs
  replace.
- Per-role cost attribution in §4.3 is a **floor**, not a total: 129.1M of 131.7M input is
  attributable to members and 2.58M belongs to no member because six verifiers' counters did
  not survive their restart.
- Whether the film's spine can be shortened by better interfaces is unknown; the projection
  will be wrong on the first run and will say `unmeasured`.
- The claim that tree production removes the reconciliation class is a design argument. P1's
  falsifier is what turns it into evidence.
- Line citations describe the code at the commit this plan was written against. The engine
  keeps moving, and three citations were already off by two lines when checked and corrected
  here. A citation is evidence of what the code said when it was read, not a permanent fact;
  re-check before relying on one.
- The post-mortem prose and `run.json` do not fully agree. The claim that the run sat silent
  for five hours is a misreading: it died 7 s after its last board entry, 2964 s into a
  3600 s clock. Every figure in §1 and §4.3 was re-derived from `run.json` for this reason,
  and where an audit's own number was stale — fault 8.1 fixed in the payload, fault 4's
  estimate machinery partly built, the 652x/13x conflation — the plan says so instead of
  repeating it.
