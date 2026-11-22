# Swarm runtime: fix, speed, polish

Revision 3. Revision 1 was source-only and said the run data was unavailable —
wrong, it is at `D:/VibeDesk/data/swarm`. Revision 2 measured the run but carried
three claims that were still inference, and it mislabelled uncached tokens as
"billable". This revision marks every claim `[V]` verified against the run's own
artifacts or source, `[I]` inferred and named, and it records the corrections.

Scope: `src-tauri/src/swarm/` (17 modules, 29,768 lines).
Evidence: `run-20261122-131108-001/{run.json, board.jsonl, permissions.jsonl}` and
`~/.vibe/worktrees/run-20261122-131108-001/` (all still on disk).

---

## 0. Corrections to my own revisions

| Revision 1/2 said | Corrected | How it was caught |
|---|---|---|
| "Billable tokens 4,473,075 = 17.9% of cap" | **Cache reads are not free** (0.1× input), and output bills ~4× input at 1:1 in that figure. Cost view is **57.4%** of cap, and the metric understates real cost **3.2×**. | You challenged the label; recomputed from per-member counters |
| "The restart ceiling is not enforced" | **Wrong.** `after_turn_error` increments at engine.rs:2502, `respawn_dead` at :3064, both guarding `>= MAX_RESPAWNS`. `resp = 3` is two real restarts plus one increment on the terminal path. The ceiling holds. The bug is **one death charging two increments in 29 ms**. | Read 2490-2520 |
| "Verifier bypasses the write gate" | **Stronger.** `toolCall` carries only `toolCallId` in **318 of 318** requests, so `Effect::Write` — the only rule that denies a verifier (permissions.rs:387) — is **unreachable**. | Parsed every permission row |
| "Silence was 1004 s" | **Unknowable from the log**, which prints a constant. 1004 s is the board gap, an upper bound, because `last_at` is also refreshed by HEAD probes. That is the defect. | Source, then board seq 26→39 |
| "Nothing offers a discard" | `discard_worktree` exists (worktree.rs:465); the user-facing command is what is missing | Review |
| "Tokens were never derived" | `role_medians` exists (history.rs:175-206); only its consumer at schedule.rs:541-549 is missing | Review |

The pattern worth naming: every claim that survived was read in a file or a log
line; every claim that failed was reconstructed from a plausible mechanism. The
plan below is written to that standard.

---

## 1. What the run actually did `[V]`

| Measurement | Value | Source |
|---|---|---|
| Duration | 13:11:08 → 13:49:50 (38 min) | board seq 1 → 67 |
| Final state | `cancelled`, no failure recorded | run.json |
| Tasks verified at cancel | **0 of 18** | run.json task states |
| Tasks ever started | 5 (t1–t5); 13 never left `blocked` | run.json |
| Member input | 54,429,671, of which **51,534,208 cached** (94.7%) | run.json members |
| Uncached input | **2,895,463** | 54,429,671 − 51,534,208 |
| Output | 1,577,612 | run.json members |
| Worst member | `w-t2`: 39,151,829 in over **2 turns** ≈ 19.6M/turn | run.json |
| Restarts | w-t1 = 2, w-t3 = 3, w-t4 = 2, w-t5 = 3, cap 2 | run.json |
| Permission requests | 318: lead 17✓/1✗, verifier 63✓/7✗, worker 203✓/27✗ | permissions.jsonl |
| Worktrees on disk | 120, against `RETAINED_RUNS = 10` | `~/.vibe/worktrees` |

### The budget number, honestly `[V]`

`spent_tokens()` is `used − cached` (mod.rs:1135-1137) = `uncached_in + output` =
4,473,075. That counts a cache read at **zero** and an output token at **the same
price as an input token**. Cache reads bill at 0.1× input; output at roughly 4×.

| View | Figure | Share of the 25M cap |
|---|---|---|
| The engine's (`used − cached`) | 4,473,075 | 17.9% |
| Input-equivalents at 1.0 / 0.1 / 4.0 | 14,359,332 | **57.4%** |

The second row is a pricing model, not billing data — `run.json` records tokens,
not rates. But the direction is not in doubt: the run was **past half its budget
at 38 minutes with 13 tasks never started**, while the status payload reported
17.9%. The cap is denominated in a unit that is neither tokens nor money, which is
why one reader called a healthy run blown and another could not see the real burn.

---

## 2. The defects

### D1 — One death, two increments, budget gone in 29 ms `[V]`

Board seq 39–42, one member:

```
39 13:33:12.203  no evidence after 180s — asked w-t1 again
40 13:33:12.207  w-t1 stopped responding — restarted it on t1
41 13:33:12.208  w-t1 could not get an answer: the Vibe agent is not running
42 13:33:12.232  w-t1 could not get an answer — restarted it on t1: …
```

`respawn_dead` (guard :3054, increment :3064) and `after_turn_error` (increment
:2502) both charge `member.respawns` for the same death. `w-t1` finished at 2 —
its whole budget — from one event, so it could never be restarted again and sat
`Working` with no process for the remaining 16 minutes.

`w-t3` and `w-t5` ended at 3 because `after_turn_error` increments **before** the
`spent` branch takes effect. That overshoot is cosmetic; the exhaustion is real.

**Fix.** One charging authority: `after_turn_error` classifies a cause and returns
it; `respawn_dead` alone increments, under the same lock as the check. Causes:
`process-gone` (spawn failure / agent not running / connection closed),
`model-silence`, `tool-hung`. Exhaustion fails **that task**, never the run — the
run was still integrating around t3 and t5 when they died, and one bad provider
must not end 17 healthy tasks.

**Test that fails today:** drive both decisions with one dead-member fixture;
assert exactly one increment and `respawns == 1`. Fails at 2 now.

### D2 — The verifier's write-deny is unreachable `[V]`

`Effect::Write` denies a verifier (permissions.rs:387). `effects()` builds a
`Write` only from `toolCall.kind` (permissions.rs:323). **`toolCall` contains
nothing but `toolCallId` in 318 of 318 logged requests**, so that branch never
runs on a live request. Everything arrives as `command <text>`,
`outside workdir (<path>)`, or `shell syntax requiring approval: …`.

What that let through, from the log:

```
13:31:07 … 13:34:07  allow  redirection                      (six times)
13:34:16             allow  command git add index.html src/style.css tools/t/shell.test.mjs
13:34:23             allow  command substitution, redirection
```

`git add` is not in `DESTRUCTIVE` (permissions.rs:197-208: push, reset --hard,
clean -f, rm -rf, rmdir /s, del, format, shutdown) and names no absolute path, so
`named_paths` finds nothing and the Command arm allows it. The word list describes
*outward* damage; a judge repairing its own subject is inward damage, and the
table has no row for it.

All commits in every worktree are authored `Mistral Vibe <vibe@mistral.ai>`, and
`e18875b` appears on **both** `v-t2` and `w-t2` — the repoint, still on disk.

**Fix, in this order:**

1. **Merge the commit that was judged.** `integrate()` merges `task.branch` by
   name (integrate.rs:71-79); `Task` and `Verdict` have no commit field. Record
   the verified SHA and merge it. One field, closes the repoint for every actor,
   no prerequisite. Do this first.
2. **Per-member identity, then the authorship assertion.** Identity does not exist
   today, so the assertion must not ship before it. Assert on `base..branch` with
   `--no-merges` — `merge_deps` puts dependency merges in that range, and without
   it every dependent task fails on its dependencies.
3. **Classify command text, path-scoped.** Derive a write effect from
   redirection targets, `tee`, `sed -i`, `cp`/`mv` into the repo, and `git add` /
   `commit` / `update-ref` / `branch -f` / `symbolic-ref` / `checkout -B` /
   `reset`. Deny for verifier and reflector **when the target is inside the
   workspace or a worktree**; allow the inbox, scratch and temp paths.
   *Path scoping is required:* the verdict channel is itself
   `outside workdir (…/inbox/v-t2)` + `redirection` (13:38:15, 13:38:26). A
   blanket deny stops verifiers posting verdicts.
4. **Route the dirty judge.** A verdict gains a typed `fix_request`; the engine
   turns it into a worker retry. The verifier never commits.

**Test that fails today:** replay the three logged effect shapes as a verifier and
assert deny; replay the inbox redirection and assert allow. Today the first fails.

### D3 — Two checks that cannot fail `[V]`

- **`base_dirtied`** (engine.rs:4619) runs *after* `ensure_detached`
  (engine.rs:4541), whose own comment says it "resets it if the verifier left
  something in it". The evidence is erased, then looked for. Only dirt created
  during the proof run can set the flag, so `Verdict::is_earned()`'s
  `!base_dirtied` clause (mod.rs:609-611) is effectively dead while roles.rs
  promises the verifier it is detected.
- **The grant** is enforced by telling the verifier "this is a fail, not a note"
  (roles.rs:958-980) — delegated to the one role the run proved was writing the
  branch.

**Fix.** Check dirtiness against the pinned commit before the reset, or diff the
base tree; add the engine-side grant gate at the evidence post, before
`state = Verifying` and the verifier is assigned (engine.rs:4434-4436). Keep the
commit, fail the task, record the paths — do **not** `git reset --hard`, which is
in `DESTRUCTIVE` for the stated reason and races a live worker.

Note honestly: a path-based grant check would **not** have caught t2. Its
out-of-grant files were `.gitignore`, `tools/browser.mjs`, `tools/check-module.mjs`;
the verifier authored exactly the granted ones. Grant checking is its own defect,
not a fix for D2.

### D4 — The log cannot tell you what happened `[V]`

Three separate faults:

1. **A constant printed as a measurement.** engine.rs:4833-4836 prints
   `LEAD_NUDGE_SECS` (180, :100) as "no evidence after {n}s". The member had been
   silent far longer; the true value is unknowable from the log. Fix: print the
   measured idle and name the clock.
2. **The nudge stamps the member healthy before delivery.** engine.rs:4774-4777
   and 4824-4827 set `last_at = now()` and `state = Working` before calling
   `nudge_member`, which can fail (seq 41). A dead member is stamped alive for
   another full interval. Fix: stamp on successful delivery only; a failure is
   `process-gone` and goes to D1's authority.
3. **A refused prompt restarts the quiet clock.** `drive_turn` calls
   `watch.turn_started()` (engine.rs:2436) before `bridge.turn`; a
   `TurnInProgress` refusal returns at :2461-2466 without cleanup, having
   overwritten the pulse and cleared the tool-in-flight marker `wedge` uses.
   Fix: start the clock after the turn is accepted.

`last_at` is an RFC3339 wall clock (engine.rs:6143): intervals move to `Instant`,
wall clock stays for display only.

### D5 — Money is measured in an indefensible unit `[V]/[I]`

- `spent_tokens()` counts cache reads at zero and output at input price. Real cost
  on this run is 3.2× the reported figure `[I]` — multiplier-dependent, direction
  certain.
- `role_medians` already derives per-role tokens (history.rs:175-206);
  schedule.rs:541-549 still multiplies the `ESTIMATE_*` constants. Wire the
  consumer. But derive from a **billable** figure: `RoleUsage.tokens_in` includes
  cache reads (mod.rs:1087), so a median of it plans the next run at 94.7% noise.
  `RoleUsage` has no spend field; add one.
- **Assert the usage convention.** `spent = input − cached` is right only when
  cached is a subset of input (OpenAI's `prompt_tokens`). Anthropic's fields are
  disjoint; the same subtraction undercounts and the cap never trips. Record the
  convention per provider rather than inferring it — this is a budget-enforcement
  bug, not a display bug.
- Status: `spentTokens`, `cachedTokens`, `spendPct`, `hitRate`, and a priced
  `costEstimate`; never `usedTokens` alone. Fix `RunSummary.tokens`
  (mod.rs:1746).

### D6 — Retention that never ends `[V]`

`RETAINED_RUNS = 10` (worktree.rs:519) against 120 directories. `reap_at` skips
every directory whose run state cannot be resolved (worktree.rs:546-548) — which
is every leftover whose state dir is already gone. The reaper does not fail to
delete; it never selects. `remove_dir_retrying` already retries 5 attempts with
four 200 ms sleeps (worktree.rs:397-413); that is not the problem.

**Fix.** Select by run-id in the directory name rather than by loading state;
treat unresolvable as reapable; report survivors. Add the user-facing discard over
the existing `discard_worktree`/`release_run`. Verify removal by existence, not by
exit code — on Windows `git worktree remove` unregisters the worktree even when
the directory delete fails, which makes the leftover invisible to a later remove.

### D7 — Progress is invisible `[V]`

`Task` has no `filesChanged`/`commits` (mod.rs:462-520). Five members sat at
tree-at-base for 38 minutes reporting `working`. Add the fields; zero commits past
a per-task deadline is `stalled`.

---

## 3. Order

1. **D2.1 merge the judged SHA** — one field, removes the integrity hole for every
   actor while the rest is built.
2. **D1 one charging authority** — the throughput failure.
3. **D2.2 identity → D2.3 command classification** — in that order.
4. **D3** — the two checks that cannot fail.
5. **D5 costing, D4 attribution** — what makes the next audit correct.
6. **D6, D7** — lifecycle and observability.

---

## 4. What this plan will not do

- No mode that skips verification to look faster.
- Not lowering `MEMBER_IDLE_LIMIT_SECS`. No threshold change fixes a member that
  was silent for a quarter of an hour; a shorter one kills long thinking.
- No new enum variants. No fallback that silently passes a check whose input is
  missing.
- No `git reset --hard` to enforce a grant.
- **No new pricing math hidden in the status payload.** Whatever unit the budget
  ends up in, it is stated once, in the payload, with the multipliers named.

---

## 5. Decisions — settled

| Decision | Answer | Consequence |
|---|---|---|
| Budget unit | **Money with a rate table** | D5 becomes a priced ledger in integer micro-units, not a token count. |
| Member identity | **Per-worktree git config** | Cheap and catches accidental cross-authoring; a worker that sets its own env can still forge it, so the assertion is a guard, not a security boundary. |
| Grant breach | **Keep commit, escalate to lead** | The branch survives for post-mortem; the lead may amend the grant when the file was genuinely required. |
| D1 scope | Assumed **fail the member's task, not the run** | 17 healthy tasks should not die for one bad provider. |
| Discard default | Assumed **keep on cancel + explicit discard** | Retention is deliberate; the missing piece is a user-facing command. |
| Rehearsal | **One small run after the code lands** | Token spend authorized for a 2-3 task plan. |

## 5a. Implementation status

Every fault in §2 now has code, tests and a commit. The order below is the order
the work landed in.

| Fault | What it was | Commit | What proves it |
|---|---|---|---|
| 1 verifier authored the artifact | v-t2 wrote t2's files and repointed the branch | `b9d14c0` | `permissions::authorship` refuses `git add`/`commit`/`checkout -b`/`stash`/redirects for the verifier; `integrate` refuses a branch carrying a foreign author; `Task.verified_commit` (`c1136df`) is merged instead of the branch tip |
| 2 grant breach noted, not acted on | w-t2's two out-of-grant files stayed on the branch | `b9d14c0` | the board entry is an `Escalation`, and `delivered_nothing` refuses a handoff that touched none of its grant |
| 3 three liveness clocks, one path | restarts at 180 s, 600 s and process death were one event in the log | `c1136df`, `4063850`, `1037151` | one charging authority; silence measured and named; `RestartCause::{Infra,Timeout,Provider}` with separate counters; `MAX_INFRA_RESTARTS = 3` stops the run |
| 4 estimates 13× low | 60 000-token constants against 39 M observed | `d630c4c`, `85c1b19` | `Estimates::of(history)` from `role_medians`, and the median reads **fresh** tokens, so the plan is not derived from a 94.7% cache hit |
| 5 workers saw a truncated board | 12 entries / 2400 chars dropped the escalation | `6902979` | the task's own record and every escalation/conflict/verdict are pinned untruncated; only cross-task chatter is capped |
| 6 stop left worktrees locked | 4 worktrees and 4 branches survived a cancel | `9529abf`, plus `a_deleted_run_leaves_no_worktree_and_no_branch` | `delete` stops bridges → removes checkouts → deletes branches → removes the directory, and reports `cleanupProblems`; `SwarmPanel` calls it through `swarm_delete` |
| 7 budget presented as usedTokens | 56 M read as spend when 51.5 M was cache | `65b9ef1` | `price()` in µusd over a versioned rate table, `spendMicros`/`spendPct`/`hitRate`/`unpricedModels` in the payload, `exceeded()` on money |
| 8 working was not progress | five worktrees held README.md and .git | `476673c` | `filesChanged`, `commits`, `lastCommitAt`, `stalled` per task, with `stalled_secs` a pure rule |

Two findings the audit did not have, both from running the end-to-end suite
(`3eaab01`):

- **Member identity was never per-worktree.** `set_identity` used plain
  `git config`, which in a linked worktree edits the *shared* `.git/config`, so
  the last member to spawn owned every commit. The gate refused a t1 branch
  authored by `w-t2@swarm.local` — correctly — and the run failed at integration.
  Fixed with `extensions.worktreeConfig` + `git config --worktree`.
- **The end-to-end suite had never run.** `swarm-sim` offered only the `auto`
  mode, and the engine refuses a role mode the session does not offer, so all six
  tests died at the first spawn. A suite that fails before the drive loop is not
  a suite.

### Two live runs - the rehearsal, and the four defects it found

The authorised rehearsal was run twice against `swarm-run.exe` on a real
workspace (a git repo with `math.js` and `test/math.test.js`, node v24.18.0),
models `deepseek` for the lead and workers and `mistral-large-4-0-3` for the
verifiers, `--budget-micros 1500000 --concurrency 2 --timeout 600`, two-task
chains. Run 1 was the code as it stood; run 2 was the same shape after the fixes
below. Nothing here is a scripted agent: these are model calls, wall clock and
provider usage fields.

| | run 1 (`run-20261122-171008-000`) | run 2 (`run-20261122-172307-000`) |
|---|---|---|
| goal | add `subtract` + its test | add `multiply` + its test |
| outcome | `done`, 2/2 verified, merged, suite passed | `done`, 2/2 verified, merged, suite passed |
| wall clock | 356 s | **182 s** |
| plan attempts | 1 (one revision) | **0** |
| restarts | 2 infra, both after the task was already verified | **0** |
| members left `working` at the end | v-t1, v-t2 | none - every member parked or done |
| agent processes released | never announced; 6 orphaned | 4 released, announced (board seq 28) |
| runner exit | never - the process hung after the report | **exit 0, on its own** |
| traffic / cached / fresh | 3,691,367 / 3,422,592 (92.7%) / 268,775 | 1,118,879 / 960,768 (85.9%) / 158,111 |
| money | `spendMicros` null, `spendPct` **6.72** | `spendMicros` null, `spendPct` **null**, `tokenPct` 3.95 |
| per-task progress | `filesChanged` 1, `commits` 1, `verifiedCommit` set | same |

Run 1's 356 s contained 186 s of revision the plan never needed and two respawned
verifiers that burned roughly 415k of its 3.69M input tokens doing work for tasks
that were already verified. Run 2 is not 2x faster because the model is; it is
faster because that waste is gone, and it was measured going.

**Defect A - the engine's own stop was reported as a lost provider.** Board
seq 19, 20, 21 and 22 in run 1: `v-t1` posted its verdict at `17:15:06.365`, the
task was marked verified at `.904`, and at `.909` the engine wrote "v-t1 could
not get an answer: the agent process exited", then at `.912` restarted it. The
verifier had been retired on purpose - `retire_verifier` stops its bridge - and
the turn still awaiting that bridge read the close as a dropped provider. The
respawned `v-t1` then posted evidence at `17:15:50` for a task verified 25 s
earlier, and the same thing happened to `v-t2`. Because nothing stopped the
respawned verifiers, `swarm-run` could not exit at all: six `vibe-acp` processes
were still alive a minute after the run reported `done`, and the runtime waits
on the bridge reader threads that wait on those children. Fixed by one owner for
the stop - `Inner::take_bridge` records it, `put_bridge` clears it, every removal
and every insert goes through them - plus a hard rule that a terminal run
restarts nothing (`after_turn_error` parks the member and charges nothing).
`release_bridges` now releases through `take_bridge`, so the end-of-run release
is recorded too.

**Defect B - `spendPct` measured a ceiling nobody asked about.** Run 1 reported
`spendMicros: null` (no `deepseek` row in `RATES`) beside `spendPct: 6.72`. That
6.72 was `spentTokens / maxTokens` - a token ratio in a field documented as the
share of the money ceiling spent. It is the same class of error as reading `56M`
as spend: the number is real, the label is not, and it is exactly what a reader
would quote as "6% of the budget spent" on a run whose money figure was unknown.
Fixed: `spend_pct` is `Option<f64>` and is `None` when there is no money ceiling
or when any model is unpriced; the token ratio is its own field, `token_pct`,
computed from the same quantity `exceeded()` compares so the percentage and the
stop condition cannot disagree.

**Defect C - the shape gate revised a plan that could not be widened.** Run 1's
two-task chain was refused at `17:10:54` ("only t1 can start - the first rank is
one task wide") and accepted at `17:14:00` on revision 1: 186 s and one lead turn
spent telling the engine that a chain of two dependencies has no wider shape to
revise towards. `shape_send_back` now withholds the revision when
`shape.max_parallel <= 1`; the warning is still recorded and still shown (run 2
board seq 2 and 3), because the user should see the shape even when nothing can
be done about it. The end-to-end harness lost its second scripted lead step as a
result, and `a_chain_is_not_sent_back` pins the new rule.

**Defect D - a unit test wrote into the user's real home.** Eight empty
directories sat in `~/.vibe/worktrees` after eight `cargo test --lib` runs: the
`prepare_verifier` test removed its two checkouts and left the run directory they
lived in. A test that writes into the user's install is a test that changes the
machine it is measuring. It now calls `worktree::release_run`, and the suite was
re-run afterwards to confirm `~/.vibe/worktrees` stays empty (0 entries). The
eight directories were removed.

Correction to §2's fault 5 as written: `skills.rs:168`'s 2400-char cap is the
*ranking* body, not the brief. A `Skill`'s body is stripped from the serialized
catalogue (`skills.rs:1113`), so nothing truncates an instruction a member
receives. The board digest was the only real truncation, and it is fixed.

### Verification actually run

| Command | Result |
|---|---|
| `cargo test --lib -- swarm::` | 668 passed, 0 failed, 6 ignored, 5.17 s |
| `cargo test --lib` | 1541 passed, 0 failed, 10 ignored, 6.37 s |
| `cargo test --test swarm_e2e` | 6 passed, 0 failed, 49.81 s (was 0 passed, 6 failed at spawn) |
| `cargo check --all-targets` | clean |
| `~/.vibe/worktrees` after the whole suite | 0 entries (was one empty run directory per invocation) |
| `swarm-run` rehearsal, run 1 | `done` in 356 s, 2/2 verified, 2 bogus restarts, process hung |
| `swarm-run` rehearsal, run 2 | `done` in 182 s, 2/2 verified, 0 restarts, exit 0 |

The four tests added or rewritten by this pass are `a_chain_is_not_sent_back`,
`the_engines_own_stop_is_recorded_and_a_new_bridge_clears_it`,
`a_finished_run_restarts_nothing_and_charges_nothing`, and
`a_one_wide_first_rank_is_a_one_shot_send_back`, which now uses a plan with a
wide later rank instead of a chain.

### The priced run, and the three defects the suite found afterwards

The rehearsal above priced nothing, because `deepseek` had no `rates::RATES` row.
The user supplied the figures — **$0.30/M input, $1.20/M output, $0.007/M cached
input** — and `RATES_VERSION` is now 2 with that row. Cache writes are not quoted
by the provider, so they bill as input; that is stated in `rates::price` rather
than assumed to be free.

A live run on the priced table (`swarm-run`, model `deepseek`, `--budget-micros
1500000`, real agent, goal "add divide + its test") finished `done` in 247 s with
`spendMicros 345260` ($0.3453, 23.0% of the $1.50 ceiling), `unpricedModels []`,
`ratesVersion 2`, 0 restarts, 2/2 verified and the integration suite passing. The
money ceiling is now exercised against a real price, which is what the earlier
"Still not done" bullets were waiting for.

Three defects came out of the work that followed, none of them visible in the
audit's material:

**Defect E — the runner released nothing, and the log said nothing.** That same
priced run left four `vibe-acp` processes alive and the run's checkout directory
behind, and the runner then lingered for six minutes until its children happened
to die. `swarm-run` returns from `main` on its own poll loop, which drops the
runtime and cancels the drive loop before its next tick can release anything. The
release now happens on the runner's own path through one function,
`engine::end_of_run`, and the drive loop's terminal branch calls the same one.
`Inner::ended` records the run whose release has already happened, so the two
callers cannot both do it — they were posting the reap note twice for one run.
A sim run went from "exit after the children die" to `exit 0` in 8 s with
`released 4 agent processes` on the board.

**Defect F — the grant check re-accused a worker that had complied, forever.**
`worktree::changed_paths` read commit history (`log --no-merges --name-only`),
and a deletion commit names the file it deletes. So the corrective commit that
removed a stray file reported that stray file as delivered again: the worker did
what it was told, the engine refused the handoff again, the verifier's scripted
turns ran out, and the run stalled with a clean branch. Measured on the branch
the test leaves behind: history named `stray.js`, the tree at `HEAD` did not, and
the net diff was `src/a.js`, `test/a.test.js`. It is now history ∩ net diff — a
file the branch does not differ on is not part of what the branch delivers — with
two tests pinning both sides: a path taken back is dropped, a granted deletion is
kept.

**Defect G — the scripted agent reported evidence before doing the work.** The
double wrote `files` before running `sh`, so the engine saw `evidence.json` while
the branch still had no commit, refused the handoff as "delivered none of its
granted files", and spent an attempt on the double's ordering. Work first,
evidence second. The counter that keeps the double's position in its script was
also keyed per member rather than per run, so a second run against the same data
directory answered nothing and looked exactly like an engine stall; it is keyed
by run now.

`scripts/swarm-sim.sh` is the harness that found these: it rebuilds the workspace
from a seed commit every run, takes the capability config from the app's own data
directory rather than inventing one, runs the headless runner against the double
with `--budget-micros`, and prints the exit code, wall clock, leftover worktrees
and surviving processes.

| Command | Result |
|---|---|
| `cargo test --lib -- swarm::` | 672 passed, 0 failed, 6 ignored, 10.91 s |
| `cargo test --test swarm_e2e` | 6 passed, 0 failed, 64.55 s — six serial runs of 9-13 s each |
| `scripts/swarm-sim.sh`, twice back to back | `exit=0` in 10 s and 10 s, 2/2 verified, `released 4`, `worktrees_left=0 sim_processes=0` |
| `~/.vibe/worktrees` after the suite | 0 entries |

The e2e suite's "has been running for over 60 seconds" line is libtest counting
the mutex wait, not a slow test: the six runs are serialised by a global engine
lock, so the last one starts ~53 s after the suite does and finishes ~11 s later.

### The liveness seam — what the "600 s never fired" caveat was hiding

The caveat said the wedged clock had never fired live. Going to fire it turned up
the reason it could not: a member whose turn began and ended between two 400 ms
ticks rested in `starting`, and no clock in the engine could see it.

The evidence, from `run-20261122-201115-000` (scripted agent, step `{}` — it
answers its brief and writes nothing):

| Observation | Value |
|---|---|
| board | stops at `task verified` (t2); no nudge, no restart, no failure, no exit |
| `run.json` | `state: running`, `t1: running`, `w-t1: starting`, `turns: 1`, `turnStartedAt: null` |
| the member's ledger | 100 input / 20 output / 80 cached — **the turn ran and finished** |
| the loop | traced: it ticked every 400 ms the whole time, and the sweep skipped `w-t1` every tick |

Three defects at one seam, all three needed:

1. **`fold_live` had no "the turn ended" transition.** `turn_started` and
   `turn_ended` both run inside one prompt, so a short turn arrives at the fold as
   a start that is already over. The fold moved a member only while `turn_at` was
   `Some` and left everything else alone, so the member kept the word its spawn
   gave it — and `starting` is documented as "the process is launching", which was
   a lie the roster told for the rest of the run.
2. **`charge_restart` used `state == Starting` as its "a restart is already
   charged" flag.** That guard exists because two paths notice one death and only
   one may pay for it. Reading it from the state word also refused the restart of
   a member that was merely starting: `respawns` read 0 while every charge was
   rejected as already in flight.
3. **`wedge` watched `working` only**, so the member was invisible to the clock
   that was supposed to name it.

What changed:

- `Member::restart_pending` carries the flag and nothing else does: set by
  `charge_restart`, cleared by `spawn_for_task` when the new bridge is registered.
  Not persisted — it guards a race inside one process, and a run read back after
  that process died has no race left to guard.
- `fold_live` maps an ended turn through `state_after_turn`, the one definition of
  what a member reads between turns. It already refuses to move a member the
  engine has decided about, so `parked`/`done`/`failed`/`lost` are untouched.
- `watchable(member)` — `starting`, `idle`, `working` — is the single predicate
  both sweeps use. `expected()` (role + task state) stays the thing that decides
  whether the run still owes that member's silence an answer: a worker is expected
  while its task is `Running`, a verifier while its task is `Verifying`. The state
  word was a second, weaker gate, and it was the one that could not fail.
- The sweep's restart line names the clock through `RestartCause::label()`, because
  `stopped responding — restarted it on t3` was the sentence that could not tell a
  dead process from a slow model.

`--idle-secs` on `swarm-run` is what made the clock reachable in a test: the
threshold is a process-wide setting (`engine::set_idle_limit`), so a run can be
asked to call six seconds a wedge without touching the default.

### What two independent readers found, and what was done about it

The first version of this fix passed every test and was still wrong in five places.
Two subagents read it cold — one instructed to attack the diff, one to find every
other reader of the state word — and both landed on the same seam.

| Finding | Verdict | What was done |
|---|---|---|
| The tick kept working from the **pre-fold snapshot**, so any later `write_run` in the same tick wrote the old roster back — and the next tick folded the same finished turn again, one span per tick | real | the fold's result is now the tick's run; pinned by folding twice in `a_turn_that_ended_between_ticks_leaves_the_member_idle` and asserting one span |
| `fail_member` left `restart_pending` set, so a member whose spawn failed could never be restarted again while `respawns` read 0 | real | it clears the flag and the turn clock |
| The fold idled a member the engine had put back to `queued` (`invalidate_built_on` leaves the turn clock set) | real | the `ended` branch is guarded by `watchable`, which excludes `queued` |
| The fold idled a member whose restart had been charged but whose process did not exist yet | real | guarded by `!restart_pending`, and `charge_restart` now clears `turn_started_at` |
| The reflection timeout asked `is_turning()`, which excludes `idle` — so a pass that answered without writing its file never degraded, and `REFLECTION_LIMIT_SECS` became dead code | real, and caused by this change | it asks `is_live()` |
| `refresh_progress` was the third sweep still gated on `state == working` | real | it uses `watchable`; its rescue line says "the branch moved, which is work" rather than naming a word the member may no longer hold |
| The panel described an idle lead as "waiting for the next tick" and had no count for a member between turns, so a healthy roster read as "0 working" | real, and caused by this change | an idle lead falls through to the run stage; the roster counts `idle` as "between turns" |

Nine doc comments that the change had falsified were corrected in the same commit
(`MEMBER_IDLE_LIMIT_SECS` described a `working`-only clock, `charge_restart` still
named `starting` as its flag, `fold_live` claimed the watch had only two words to
say, `stop_turn_clock` claimed the wedge ignored `idle`, `MemberState::Starting`
claimed the process exists, and the panel's `describeAgent` claimed a lead is
`working` for the whole run).

**Not fixed, and named rather than papered over:**

- **A spawn in flight is not distinguishable from a spawn that failed.**
  `spawn_for_task`'s only dedupe is `bridges.contains_key`, so a restart charged
  while a slow spawn is still opening its session could spawn a second process.
  Reachable only by setting `--idle-secs` far below the 600 s default: the charge
  needs more silence than the spawn takes. The durable fix is a "spawn in flight"
  marker in `Inner`; it is not in this change.
- **`fail_member` leaves the task `Running`.** A spawn that fails outright leaves a
  task with no worker, and neither sweep can restart a `failed` member. The honest
  fix is a task-level failure path, which is a decision about what a failed spawn
  means for the plan, not a one-line repair.
- **A `queued` member and a `waiting` member whose bridge was taken** are outside
  both sweeps. Both are windows around a spawn rather than steady states, and both
  are invisible for the same reason as the first bullet: nothing records that a
  spawn is in flight.

### The runner, the kill path, and the job object

Two of the three open caveats were "no automated test proves it". Both have one
now, in `src-tauri/tests/swarm_runner.rs`, `#![cfg(windows)]`: a hermetic
`VIBE_HOME` laid out from `provision::payload::FILES`, a seeded repository, the
scripted agent, and the real `swarm-run.exe` driven as the user drives it.

| Test | What it proves | Measured |
|---|---|---|
| `a_finished_run_releases_its_agents_before_the_runner_exits` | exit 0, exactly one `released 4 agent processes` note, the run's checkout directory gone, no member process alive | passes; the two-test binary runs in 9.6 s |
| `a_member_dies_with_the_engine_that_started_it` | `taskkill /PID <engine> /F` **without** `/T`, and the spawned member is dead within 20 s | passes |

The second one is a new mechanism rather than a test of an old one: nothing in the
engine made its children die with it. `acp/job.rs` adopts each agent into a Windows
job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and never closes the handle, so
the children are killed by the operating system when the engine's process ends,
however it ends. **Falsified**: with `job::adopt` stubbed out the test fails with
`members [15856] outlived the engine that started them` after 21.6 s; restored, it
passes in 0.9-11.5 s.

### The installed build, proved by running it

The release build was installed over `D:\VibeDesk` (five binaries, `data/`
untouched, the previous `vibedesk.exe` kept as `vibedesk.exe.bak-2309`) and every
one of them is byte-identical to what `npm run tauri:build` produced:

| Binary | md5 build = installed |
|---|---|
| `vibedesk.exe` | `b56e9f6299baf0e07fa363338e30ba13` |
| `swarm-run.exe` | `5bd90931bfc297b30eb290c8087aed2b` |
| `swarm-mcp.exe` | `5d51b9ee9fbfaebd460b93952822aab0` |
| `swarm-sim.exe` | `cc9a7d857d91d4d62c7acb09b99b6bb1` |
| `channel-probe.exe` | `0737af56aacc2c871b9d2ec5e0167056` |

A hash is not a run, so the installed pair was driven through a real run: the
**installed** `swarm-run.exe` against the **installed** `swarm-sim.exe`, a freshly
seeded workspace, `--model deepseek --budget-micros 1500000`: `exit=0` in 7 s, 2/2
verified, reflection written, `released 4 agent processes`. `D:\VibeDesk\swarm-run.exe
--help` lists `--idle-secs`, so the installed runner is the build that carries the
liveness fix.

Two mixes were run to show neither binary is the load-bearing half:
installed runner + debug double (`exit=0`, 7 s) and debug runner + installed
double (`exit=0`, 8 s).

**A harness trap found on the way**, recorded because it reads as a product
failure: `swarm-sim` exits quietly when `VIBEDESK_SIM_SCRIPT` names a file it
cannot read — `unwrap_or_else(|_| json!({ "members": {} }))` — so the run reports
`the lead did not produce a plan in time` after 180 s with nothing on the board
that says why. A copy of `scripts/swarm-sim.sh` outside the repo resolves
`repo="$(cd "$(dirname "$0")/.." && pwd)"` to `/`, which is exactly how it was
hit. The double should fail loudly on a missing script rather than answer nothing,
and it now does: `load_script` prints the path and exits 2 on either an unreadable
file or unparseable JSON, so the trap reports itself in the first second instead of
surfacing as a planning timeout two minutes later. Verified by running the binary
against three broken setups rather than by reading the diff:

```
$ VIBEDESK_SIM_SCRIPT=D:/Temp/no-such-script.json swarm-sim.exe
sim: cannot read the script at VIBEDESK_SIM_SCRIPT="D:/Temp/no-such-script.json"   exit=2
$ VIBEDESK_SIM_SCRIPT=D:/Temp/broken-sim.json swarm-sim.exe      # file is "not json at all"
sim: "D:/Temp/broken-sim.json" is not valid JSON: expected ident at line 1 column 2  exit=2
$ VIBEDESK_SIM_SCRIPT=D:/Temp/trap/swarm-sim.sh swarm-sim.exe    # the original trap, replayed
sim: "D:/Temp/trap/swarm-sim.sh" is not valid JSON: expected value at line 1 column 1 exit=2
```

The end-to-end suite still passes against the strict double — 7/7 in 72.5 s — which
is the check that matters, because every e2e test drives the sim through this
function and a double that refused a valid script would fail all seven.

### The cleanup path, and the test that now holds it

Fault six was fixed and hand-verified, and it had no test: `delete` exists, the
order is right, `cleanupProblems` is reported — and nothing would have noticed if
a later change reordered it. `a_deleted_run_leaves_no_worktree_and_no_branch`
closes that. It starts a real run, approves a real plan, waits for the t1 worktree
to exist, asserts the pre-conditions first (a `-t1` branch, a registered checkout)
so the test cannot pass by deleting nothing, then stops the run, deletes it, and
checks each of the four things the audit found left behind: the worktree root, the
branches, the git worktree registration, and the run directory.

A green first try is not evidence, so the test was falsified: with `delete`'s
branch list replaced by `Vec::new()`, it fails on exactly the audit's symptom —

```
every branch of the run is deleted, including the integration branch:
"swarm/run-20261122-212519-000\n  swarm/run-20261122-212519-000-t1\n  swarm/run-20261122-212519-000-t2"
```

— and passes again with the line restored. Suite after: **8/8 in 82.7 s** (the new
test alone, 9.5 s), with `engine.rs` byte-identical to the committed version.

One design point worth recording, because it looks like the fault when it is not:
`stop` deliberately keeps the worktrees and names them, so a cancelled run can be
inspected; `delete` is what removes them, and `SwarmPanel` calls it. What changed
from the audit is that nothing is *locked* — the member processes are gone before
the removal is attempted, which is what made `git worktree remove` fail with
"Permission denied" and what the ordering exists to prevent.

### The app's own UI check, run for the first time

`npm run verify` ends in `live`, a CDP script that drives the real window. It had
never been run in this work, and the swarm frontend touched `SwarmPanel` and
`RosterView`, so a real-DOM pass was owed. Debug build plus
`VIBEDESK_REMOTE_DEBUGGING=9222`, then `node scripts/live-check.mjs all`.

It found three defects, all now fixed and re-run:

- **A refused widget capability was never shown to the user.** The notice is a
  chat item, and when a panel covers the chat the transcript is not rendered —
  the check's failure detail said `{"inText":false,"inTree":true}`, which is the
  sharpest statement of it: present in the DOM, invisible on screen. The block
  now renders the refused capabilities above its own frame.
- **The tween sampler counted frames and called them seconds.** 150 frames is
  2.5 s at 60 Hz and 1.04 s on this 144 Hz display; the failure line reported
  `150 frames in 1039 ms, visible, focused=true`, which ruled out the throttling
  theory it was suspected of. It samples 2.5 s of wall clock now.
- **The refusal was read once instead of waited for**, which is the anti-pattern
  the file's own `waitFor` doc calls out.

Verified by repetition, not by a single green:

| Run regime | Result |
| --- | --- |
| `all`, fresh app process | 230/230, 0 console errors (run 1 of 4) |
| `all`, back-to-back on one process | 229–230/230, the widget refusal green in every run after the fix |
| `motion` alone | 8/8 green |
| `chats` alone | 3/3 green |

**Open, and not fixed: a suite that poisons the next one.** In a fresh-app loop,
runs 2–4 failed at `chats` (`timed out waiting for the switch to fc2bac`) and then
`bulk` (`timed out waiting for the chat to open`), while run 1 passed and the same
suites pass 3/3 alone. A fresh process does the switch correctly when clicked by
hand in the same state (`marked: 1` after one click), so the app can switch chats
and something an earlier suite in the same run leaves behind stops the click from
landing. Not narrowed further. It is in the chat rail, not the swarm, and it is
recorded here rather than fixed because the honest thing is to name what was not
finished.

### Still not done (updated)

- **Money is measured now, on one model only.** `deepseek` is priced; a run on a
  model without a row still reports `spendMicros: null` and is bounded by
  `maxTokens` and the wall clock, which is the correct behaviour but leaves the
  money ceiling unexercised for that model.
- **`RATES` is a table, not a bill.** The numbers are the published per-model
  rates at the version in `rates_version`; a run on a model the table does not
  know reports itself as unpriced rather than guessing (verified by
  `an_unknown_model_is_unpriced_rather_than_free`), but a provider that changes its prices makes
  the ledger wrong until the table is updated.
- **Cache-write tokens are not counted separately.** `Usage` tracks reads and
  totals, not writes, so a write is billed at the input rate: a floor, not a
  bill, for a provider that charges more to write a cache entry than to send the
  same tokens fresh. Stated in `rates::price`, not fixed.

---

## 6. Not proven

- The throughput claim is measured on exactly one shape: two two-task chains on
  one workspace with one model pair, 356 s to 182 s. That is not a claim about
  eighteen-task runs, a slower provider, or a codebase the workers have to read
  before their first commit, and the difference includes the lost revision and the
  two bogus respawns the fixes removed. The number is an upper bound on the saving
  for this shape, not an average.
- `MAX_INFRA_RESTARTS = 3` has never fired in a live run. Both rehearsals had zero
  infra restarts after the fixes, so the ceiling is only exercised by tests.
- The clock that fails a genuinely wedged member has now fired, but only against
  the scripted agent at `--idle-secs 6` (`a_silent_worker_is_wedged_by_the_clock_
  that_names_itself`, 9.9 s). The default 600 s has still never been reached by a
  real model doing real work, and the honest statement is that the *mechanism* is
  exercised while the *number* is not.
- The job object covers an engine process that dies. It does not cover a member
  that spawns children of its own: those inherit the job too, which is the
  behaviour wanted, but nothing in this repo has measured a real `vibe-acp` tree
  dying that way — only the scripted agent.
- `MAX_INFRA_RESTARTS = 3` is a judgement, not a derived figure. Three covers a
  one-off crash of a shared process; a run that trips it on a machine where the
  agent legitimately restarts more often would need the number raised, and that
  would be evidence the threshold was wrong rather than evidence the rule is.
- `STALL_SECS = 1200` is likewise chosen: twenty minutes of no commits. A worker
  legitimately reading a large codebase before its first commit will be reported
  as stalled once. It is a report, not a kill, which is why the number can be
  generous.

## 7. Research basis

Kubernetes liveness/readiness/startup and `podFailurePolicy: Ignore`; Erlang/OTP
restart intensity; ACP cancellation and `session/update`; MCP `ping` and per-request
timeouts; JSON-RPC notifications are unconfirmable; RFC 9293 MUST-29; Rust
`Instant` monotonicity; SWIM suspicion; AWS backoff-with-jitter; Google SRE retry
budgets; Fowler circuit breaker; Anthropic/OpenAI usage-field conventions and cache
multipliers (1.25×/2× write, 0.1× read); OpenHands and Claude Code cost/worktree
practice.
