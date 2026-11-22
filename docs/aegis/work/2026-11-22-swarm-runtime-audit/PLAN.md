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

Landed and verified in this pass (plan order):

- **D2.1 merge the judged SHA** — `Task.verified_commit` pinned at handoff
  (engine.rs handoff block), asserted unchanged when the verdict arrives (a moved
  branch refuses the verdict and says which commits), and merged in place of the
  branch tip (integrate.rs). Regression test
  `integration_merges_the_commit_the_verdict_was_earned_on` passes.
- **D1 one restart authority** — `charge_restart` is now the only place
  `respawns` moves; `after_turn_error` and `respawn_dead` both go through it, and
  `Starting` makes it idempotent so one death spends one slot. `respawn_dead`
  persists after charging. Regression test `one_death_charges_one_restart` passes.
- **D2.2 member identity** — `worktree::identity` / `set_identity`, applied by
  `add_worktree` from the checkout's directory name, so every worktree commits as
  `swarm-<member>` / `<member>@swarm.local`. `worktree::authors` reads author
  emails over `base..tip`; `integrate` refuses a branch carrying an author that
  is not that task's worker. Tests:
  `a_commit_the_worker_did_not_author_is_refused`,
  `the_workers_own_commits_integrate`, and the add-worktree test now asserts the
  two identity calls.
- **D2.3 verifier cannot author** — `permissions::authorship` refuses `git add`,
  `git commit`, `git checkout -b`, `git stash`, shell redirection and shell
  writes for the verifier role; the denial reason is set before the effect loop
  and the loop stops on Deny. Five tests including
  `a_verifier_cannot_stage_the_change_it_judges`.
- **D3.1 grant breach escalated, not noted** — the outside-grant board entry is
  `Escalation` instead of `Note`; the commit is kept and the verifier still rules
  on it, which the verifier brief already frames as fail-or-escalate.
- **D3.2 handoff refused when the branch delivered nothing** —
  `worktree::delivered_nothing` (`untouched(granted, changed).len() ==
  granted.len()`), checked at handoff before a verifier is given the branch. The
  work and the branch are kept; the task goes back through
  `schedule::apply_verdict` with a refusal naming the missing paths, so the
  attempt ceiling is the same one a refused verdict answers to. Tests:
  `a_branch_that_touched_none_of_the_grant_delivered_nothing`,
  `a_branch_that_touched_one_granted_file_is_judged_not_refused`,
  `a_task_with_no_grant_is_never_refused_for_delivering_nothing`.
- **D3.3 `base_dirtied` read before the reset** — `base_checkout_for` captures
  `is_dirty` before `ensure_detached` wipes it; the post-proof read is kept as the
  second window. Real-repository test
  `a_base_the_verifier_left_dirty_is_reported_before_the_reset_hides_it`.

Verification: `cargo test --lib swarm::` → **646 passed, 0 failed, 6 ignored,
4.18s**; `cargo check --all-targets` clean. Commit `b9d14c0`.

Not yet started: D5 (money budget), D4 (clock attribution), D6 (reap by run id),
D7 (per-task progress).


---

## 6. Not proven

- Cost multipliers are a model; `run.json` has tokens, not rates. The 57.4% needs
  the provider's per-model rates to become a number rather than a ratio.
- The 1004 s figure is an upper bound on silence, not `last_at` delta; the log
  cannot produce the real value until D4.1 lands.
- **I have not run the swarm or its test suite.** Every claim here is a reading of
  run artifacts and source. The D1 and D2 tests described are assertions about
  code as written, not executed results.
- Why `busy` held for ~16 minutes before the first nudge is still unexplained:
  the pulse is not logged in the run directory. D4.3 names one mechanism;
  confirming it needs the turn-clock instrumentation first.

## 7. Research basis

Kubernetes liveness/readiness/startup and `podFailurePolicy: Ignore`; Erlang/OTP
restart intensity; ACP cancellation and `session/update`; MCP `ping` and per-request
timeouts; JSON-RPC notifications are unconfirmable; RFC 9293 MUST-29; Rust
`Instant` monotonicity; SWIM suspicion; AWS backoff-with-jitter; Google SRE retry
budgets; Fowler circuit breaker; Anthropic/OpenAI usage-field conventions and cache
multipliers (1.25×/2× write, 0.1× read); OpenHands and Claude Code cost/worktree
practice.
