# Plan: polishing the swarm

What the three failed `cpp-terrain-flyer` runs actually broke on, and the smallest
set of changes that stops each one recurring.

Written 2026-11-21. Extends `docs/PROGRESS-harness-test.md`, which records the
runs; this plan owns the repairs. `docs/PLAN-swarm.md` owns the design.

## Why this exists

Game 1 passed. Game 2 failed three times, and the three failures had three
different causes. That is the useful signal: the harness does not have one bug,
it has a set of failure modes that only appear under a real multi-language build
with a real verifier. Each one is cheap to fix and expensive to leave.

The evidence below is from the run artifacts, not from reading code and guessing.
Board entries are cited by `seq`; permission denials by count.

## What failed, run by run

| Run | State | Failure string | Cause |
| --- | --- | --- | --- |
| `run-20261121-081416-000` | escalated | `the lead could not amend the plan: the amendment changed nothing those tasks did not already have` | A no-op amendment is treated as fatal during replanning |
| `run-20261121-083157-000` | failed | `the lead could not get an answer: the Vibe agent is not running` | My `taskkill //IM vibe-acp.exe`. Not a product defect |
| `run-20261121-090406-000` | failed | `t7 is verified but has no branch` | `invalidate_dependents` clears `branch` and nothing writes it back |

Run 3 is the one worth studying. Seven of seven tasks verified, six merged, and
the run died on a bookkeeping field. Its board shows the whole shape of the
problem: 12 verdicts for 7 tasks, 40 permission denials, and 53m43s of wall
clock. Game 1 was 23m30s for five tasks, so the two are not a like-for-like
comparison — but run 3 re-verified two tasks it had already passed, and that
re-verification is the part of the difference this plan can account for.

## The defects

Twelve. Each names its owner, the fix, and the single test that catches it.

### 1. `stop_process` deadlocks on a full stdin pipe

**Symptom.** A finished run prints its result and never exits, leaving every
member process alive. `release_bridges` hangs.

**Evidence.** `bridge.rs:994` `write_value` holds `self.stdin.lock()` across
`write_all` and `flush`. `bridge.rs:1004` `stop_process` takes
`self.stdin.lock().await.take()` *before* `child.start_kill()`. A child that
stops reading fills the pipe, `write_all` never returns, the lock is held
forever, and the kill that would break the pipe waits behind it.

**Currently in the tree.** `engine.rs:6155` wraps the stop in a 10s timeout with
`let _ =`. That is a workaround: it hides the hang, it is silent, and the limit
is our own lock ordering rather than an external one.

**Fix.** In `stop_process`, take the child, `start_kill()`, `wait()`, and only
then take the stdin lock. Killing closes the pipe, the blocked write fails, the
lock releases. Remove the timeout from `release_bridges`, keeping it `pub`.

**The one test.** Spawn a real child that never reads stdin, fill its pipe with
a write that blocks, then assert `stop_process` returns within a second. It
fails today by timing out and passes after the reorder. No mock: the deadlock is
in the pipe, so the test needs a pipe.

### 2. A re-queued task verifies with no branch

**Symptom.** `integrate.rs:75` refuses the run: `t7 is verified but has no
branch`.

**Evidence.** `run-20261121-090406-000` `run.json`: `t7` has `attempts: 2` and
`branch: null`. `invalidate_dependents` sets `task.branch = None`; the re-run
recreates the branch from the deterministic name but nothing wrote it back.

**Fix.** Already written: `WorkerPrep` carries `branch`, `prepare_worker` sets it
in both return paths, the caller writes `record.branch`. Compiled and tested,
**not in the binary** — `swarm-run.exe` is from 11:03, `engine.rs` from 12:05.

**The one test.** Already extended:
`a_failed_verdict_discards_the_dependents_branch_and_the_requeued_task_builds_on_the_new_tip`
asserts `again.branch == branch_name(&run.id, "t2")` and that the record stays
`None` until the caller writes it.

### 3. An accepted verdict does not stop its verifier

**Symptom.** The same verifier posts a byte-identical verdict again, minutes
later, after the task is already verified.

**Evidence.** Board `seq` 161, 168 and 170 are three verdicts from `v-t6` with
identical text length (1606) and identical evidence count (6), at 09:39:34,
09:43:11 and 09:43:35. `seq` 162 is `task verified`, between the first and the
second. The engine's `task.state != TaskState::Verifying` guard skips the
duplicates, so no state is corrupted — but each one is a full verifier turn.

**Cause.** `engine.rs:4678` ends the verifier's turn with `end_member_turn`,
which only calls `bridge.cancel()` through `tokio::spawn` and returns. The
process stays alive with its session, and a session asked again re-emits the
answer it gave. The worker handoff at `engine.rs:4398` does the right thing:
it removes the bridge, which kills the process.

**Fix.** On verdict acceptance, drop the verifier's bridge and stop it, the same
way the worker handoff does. A verifier is per-task and has nothing to do after
its verdict, so killing it is correct rather than merely convenient. This also
removes the largest source of live processes.

**The one test.** Drive a task to `Verifying`, post a verdict, assert the
verifier's bridge is gone from `inner.bridges` and its member state is `Parked`.
Fails today because the bridge is still there.

### 4. A no-op amendment escalates the whole run

**Symptom.** The lead restates what the plan already says, and the run dies.

**Evidence.** `run-20261121-081416-000` failure string. `amend.rs:176` returns
`Err("the amendment changed nothing those tasks did not already have")`.
`engine.rs:4058` matches `Err(error) if replanning` and sets
`RunState::Escalated`. The non-replanning branch at `engine.rs:4074` correctly
posts a note and lets the lead try again.

**Cause.** A no-op amendment is not an invalid amendment. It is the lead saying
the plan is already right. The caller cannot tell the two apart because
`amend::apply` returns `Result<Vec<String>, String>` and both arrive as `Err`.

**Fix.** Give `amend::apply` a typed error with `NoOp` distinct from
`Invalid(String)`. Handle `NoOp` as benign in both caller branches: post a note
saying the plan already says that, and continue. One caller
(`engine.rs:3953`), so the change is contained.

**The one test.** A run in `replanning` whose lead answers with an amendment
that changes nothing stays out of `Escalated` and keeps its tasks. Fails today
with `state == Escalated`.

### 5. `concurrency` bounds workers, not processes

**Symptom.** Fifteen live `vibe-acp` processes for a seven-task plan at
`concurrency 3`.

**Evidence.** `schedule.rs:223` `let busy = running_count(tasks)` counts only
`TaskState::Running`. A `Verifying` task holds a live verifier and does not
count. Workers hand off and free their slot while their verifier is still
running, so verifications accumulate.

**Fix.** Two parts, and the second is the one that matters. Defect 3 kills
verifiers on verdict, which removes the accumulation at its source. Then the
panel reports live process count next to the ceiling, so the number is visible
instead of inferred. Do **not** serialise verifications to make the ceiling
literally true: verification is the long pole (15-25 minutes each) and
serialising it would make the run slower, which is the opposite of the goal.

**The one test.** `advance` with three `Verifying` tasks and a ceiling of three
starts three new workers, and the reported live count is six. The test asserts
the current semantics so the number is documented rather than assumed; it is not
a test of a fix, and it is the one test here that would pass today.

### 6. Verifiers write outside the worktree and never clean up

**Symptom.** `C:\tmp\vt7` survives the run, holding `conpty_test.exe` and
`conpty_test2.exe`. Earlier runs left `C:\tmp\verify-t3` and `C:\tmp\v2probe`.

**Evidence.** `C:\tmp\vt7` exists now, dated 11:47-11:55, from run 3. The
verifier brief in `roles.rs` names no scratch directory, so each verifier
invents one.

**Fix.** The brief names a scratch directory inside the run directory
(`<run>/scratch/<member>`) and says to use it. The gate already permits the run
directory. `release_checkouts` removes it with the rest.

**The one test.** The verifier brief contains the scratch path it was given and
does not contain `C:\tmp`. Fails today because the brief names no path at all.

### 7. The first rank is one task wide

**Symptom.** The run starts single-threaded however many workers it is given.

**Evidence.** Board `seq` 3, posted by the engine itself: `only t1 can start —
the first rank is one task wide, so the run begins single-threaded however many
workers it is given`. `t1` ran alone for 2.5 minutes before `t2`, `t3` and `t5`
started together.

**Cause.** The plan's dependency shape. The engine already knows: `plan.rs:694`
carries `first_rank_width`, documented with this exact observation, and
`plan.rs:868` raises `ShapeWarning::OneWideFirstRank`. `engine.rs:4220` posts
every shape warning to the board as a note and then accepts the plan anyway.
The warning is recorded and ignored.

**Fix.** Promote `OneWideFirstRank` from a note to a one-shot send-back: the
plan goes to the lead once with the reason, and is accepted on the second
attempt whatever its shape. Not a refusal — a plan that genuinely starts with
one foundation task is legitimate, and blocking it outright would be a worse
bug than the one being fixed. The machinery exists; only the disposition
changes.

**The one test.** A plan whose first rank is one wide is sent back once and
accepted on the retry; a plan whose first rank is three wide is accepted
immediately.

### 8. `destructive()` refuses innocent commands

**Symptom.** A verifier was refused for echoing a heading.

**Evidence.** `permissions.rs:211` `destructive()` lowercases the whole
invocation and does `lowered.contains(needle)` against a needle list that
includes `"format "`. Run 3's `v-t7` was denied with reason `it would format a
volume` for the command `echo '=== other verdicts for format ==='`. The needle
matched the word inside the echoed text.

**Cause.** The match is against the entire command string, arguments included,
so any command that mentions a destructive word is refused. `grep "rm -rf" notes`
and `cat why-we-never-rm -rf.txt` are refused the same way. The list is short
and named on purpose, which is right; the matching is what is wrong.

**Fix.** Match the command word and its own arguments, not the whole string.
The cheapest correct shape: split the invocation into the command and its
arguments, and match the needle against the command plus any argument that is
not inside quotes. A `rm -rf` typed as a command still matches; the same text
inside an `echo` does not.

**The one test.** `echo 'format the disk'` is allowed and `format C:` is
refused. Both directions, because a fix that only widens the allow is a
regression in the other direction.

### 9. A member has no sanctioned way to clean up

**Symptom.** 23 of run 3's 40 denials are `it would delete a tree`.

**Evidence.** Reason counts across the 40 denials: 23 `it would delete a tree`,
9 `that is another member's worktree, or a base checkout`, 7 `the request did
not say what it would do`, 1 the false positive above.

**Cause.** The brief never says how to remove a scratch directory, so the member
reaches for `rm -rf`, the gate refuses it, and the member tries again. The gate
is right every time. The brief is what is missing.

**Fix.** The brief names the scratch directory and says the engine removes it,
so the member never cleans up. This is the same brief change as defect 6 and
should land with it. The 9 worktree denials are the same class: the brief should
say which paths are the member's.

**The one test.** The brief contains the scratch path and the sentence that the
engine removes it. No test for the denial count — that is measured by the next
run's `permissions.jsonl`, not by a unit test.

### 10. One bad effect denies the whole batch

**Symptom.** A single refused effect denies the `cd`, `echo`, `ls` and `cat` in
the same request.

**Evidence.** `permissions.rs:502` `decide_announced` iterates effects and sets
`Verdict::Deny` if any one denies. Board `seq` 165: `refused outside workdir
(C:\tmp\cb.log), command shell syntax requiring approval: redirection, variable
expansion`.

**Assessment.** Fail-closed is correct and stays. The cost is that the member
cannot tell which effect was the problem, so it retries the batch with one
change and loses a round trip each time.

**Fix.** The denial reason names the offending effect and states that the other
effects in the request were acceptable. No change to the verdict, only to what
the member is told.

**The one test.** A request with one allowed effect and one denied effect
produces a reason naming the denied one and saying the rest were fine.

### 11. The binary is not part of the loop

**Symptom.** Bug 2's fix was compiled and tested but absent from the binary that
ran game 2, because `swarm-run.exe` was built before the edit.

**Evidence.** `target/debug/swarm-run.exe` 11:03, `src/swarm/engine.rs` 12:05.

**Fix.** `game.sh` builds before it runs, and refuses to start if the build
fails. A stale binary is a test that measures the wrong code, which is the
failure mode the repository's own capabilities section exists to prevent.

**The one test.** None. This is a script change, and the proof is that the next
run's first line is a build.

### 12. Failed runs leak their checkouts forever

**Symptom.** 105 directories under `~/.vibe/worktrees`, 36 MB.

**Evidence.** `engine.rs:6178` `release_checkouts` returns early unless
`run.state == RunState::Done`. Keeping a failed run's checkouts is deliberate
and documented — they are the only place the half-finished work exists. Nothing
ever removes them afterwards.

**Fix.** Bounded retention: keep the checkouts of the ten most recent
non-`Done` runs, remove older ones, and post what was removed. The decision to
keep a failed run's tree is preserved; the decision to keep it forever is not.

**The one test.** Eleven failed runs leave ten directories, and the removal is
posted.

### Also

`engine.rs:6178` reads `fn release_checkouts() {    let run = {` — the brace and
the `let` share a line. `rustfmt` is not installed on this toolchain
(`cargo fmt --check` reports `cargo-fmt.exe is not installed`), so this is a
defect I read rather than one a tool confirmed. Fix it while the file is open.

## Test strategy

Ten tests for twelve defects. Two defects share one test because they share one
fix (the brief), and one is a script change with no test at all.

The rule: **one test per defect, and every test must be proven to bite.** Proven
means the fix is temporarily reverted, the test is run, and it fails for the
stated reason. A test that passes both before and after the fix is deleted, not
kept. That is the standard the repository already sets and the reason game 2's
bug 1 test was worth writing.

What this plan deliberately does not do:

- No test that asserts a constant, a string literal, or the shape of a struct.
- No test for a code path the run never reaches.
- No test that restates the implementation in a second language.
- No test whose only purpose is coverage.

The real proof is not the unit tests. It is game 2 passing end to end, then
games 3, 4 and 5. The unit tests exist to make a failure there diagnosable in
seconds instead of fifty minutes.

## Tasks

Ordered so each one is verifiable on its own. Every task ends with a build.

**Task 1 — the deadlock.** `src-tauri/src/acp/bridge.rs` `stop_process`: reorder
to kill before locking stdin, with a comment naming the deadlock. Remove the 10s
timeout from `release_bridges` in `engine.rs`, keeping it `pub`. Test: the
blocked-pipe test above. Verify: `cargo test --lib` plus the new test, then a
manual `swarm-run` that exits on its own.

**Task 2 — the verifier.** `engine.rs` verdict-accepted path: drop the bridge and
stop it, mirroring the worker handoff at 4398. Test: bridge gone, member parked.
Verify: a run whose board shows one verdict per task.

**Task 3 — the amendment.** `amend.rs`: typed error with `NoOp` separate from
`Invalid`. `engine.rs:4058` and `:4074`: handle `NoOp` as benign. Update the two
tests at `amend.rs:268` and `:301` that encode the current behaviour. Test: the
replanning no-op test. Verify: `cargo test --lib`.

**Task 4 — the brief.** `roles.rs` verifier and worker briefs: name
`<run>/scratch/<member>`, say the engine removes it, and say which paths are the
member's. This closes defects 6 and 9 together. `engine.rs` `release_checkouts`:
remove the scratch directory with the rest. Test: the brief names the path and
the sentence. Verify: a run leaves nothing in `C:\tmp`.

**Task 5 — the plan shape.** `engine.rs:4220`: treat
`ShapeWarning::OneWideFirstRank` as a one-shot send-back rather than a note.
`plan.rs` needs no change — `first_rank_width` and the warning already exist.
Test: one-wide sent back once, three-wide accepted. Verify: a run whose board
has no `only t1 can start` note.

**Task 6 — the false positive.** `permissions.rs:211` `destructive()`: match the
command word and its unquoted arguments, not the whole invocation. Test:
`echo 'format the disk'` allowed, `format C:` refused. Verify: `cargo test
--lib`, then a run whose `permissions.jsonl` has no `it would format a volume`.

**Task 7 — the denial message.** `permissions.rs` `decide_announced`: name the
offending effect and say the rest were acceptable. Test: mixed request produces
the new reason. Verify: `cargo test --lib`.

**Task 8 — the ceiling report.** `schedule.rs` / the panel: report live process
count beside the ceiling. Test: the six-of-three case. Verify: the panel shows
both numbers during a run.

**Task 9 — retention.** `engine.rs` `release_checkouts` plus a reaper: keep ten
non-`Done` runs. Test: eleven leave ten. Verify: `~/.vibe/worktrees` stops
growing across runs.

**Task 10 — the build step.** `game.sh`: build first, refuse to run on failure.
Verify: the next run's first line is a build.

**Task 11 — the end-to-end proof.** Re-run `cpp-terrain-flyer` from base
`cd1e776`. It must reach `done` with seven merged branches and a green suite.
Then run `rust-voxel-world`, `data-driven-arena` and `procedural-terrain`.

## Not in scope

- Loosening the permission policy. Fail-closed stays; the false positive is a
  matching bug, not a policy question, and the message change is a message
  change.
- Serialising verifications to make `concurrency` literally true. It would make
  the run slower.
- The server-routed-model gap in `provider::enable`, tracked as task 10 of
  `docs/PLAN-provisioning.md`.
- Rewriting the scheduler. The defects above are local.

## Open decisions

**Retention count.** Ten is a guess. The number that matters is how many failed
runs a person wants to inspect after the fact. Ten keeps 3-4 MB.

**Whether a verifier may fail a task for a defect outside its acceptance
criterion.** Run 3's `v-t7` did exactly that: the criterion passed, and it failed
the task anyway because `parse_count` did not check `errno`. The catch was
correct and the bug was real. The cost was an 18-minute re-verification. I would
keep the behaviour — a real bug in a file the task owns is worth a re-run — but
it is a policy choice, not an obvious one, and it belongs in the verifier brief
either way so the verifier knows which it is doing.

## Verification

The plan is done when `cpp-terrain-flyer` reaches `done` from base `cd1e776`
with seven merged branches and a green suite, and the four remaining games run
without a defect that this plan did not already name. A new defect is a finding,
not a failure of the plan; a defect from this list recurring is a failure.
