# Execute docs/PLAN-polish.md (12 swarm-harness defects, 11 tasks) - Checkpoint

## Current Checkpoint

- Current todo: t8-t10 done; t11 (end-to-end games) NOT started - needs live model runs (CPU-heavy), awaiting user go-ahead
- Active slice: Tasks 1-10 of docs/PLAN-polish.md complete and verified
- Completed todos:
- t1 stop_process deadlock; t2 retire_verifier; t3 AmendError::NoOp; t4 scratch briefs; t5 one-wide send-back; t6 destructive() quote-aware; t7 denial message; t8 live process count in panel + live_count; t9 checkout retention (reap, keep 10); t10 game.sh builds first
- Evidence refs:
- cargo test --lib -- --test-threads=4 swarm:: acp:: => 649 passed 0 failed; node --test tests/*.test.ts => 291 pass 0 fail; npx tsc --noEmit clean; npm run build clean; cargo build --bin swarm-run clean; bash -n game.sh ok; each fix test proven RED before fix
- Blocked on: t11 blocked on user approval (CPU constraint): live swarm runs peg the machine
- Next step: Task 11: re-run cpp-terrain-flyer from base cd1e776 + 4 games via game.sh - NOT started on user's no-lag constraint; needs explicit go-ahead (3 live agent processes, up to 3600s each)

## Recent Checkpoint History

## Checkpoint Update

- Current todo: t8 in progress: report live process count beside concurrency ceiling
- Active slice: Task 8 of docs/PLAN-polish.md: panel reports live process count next to the ceiling
- Completed todos:
- t1 stop_process deadlock; t2 retire_verifier; t3 AmendError::NoOp; t4 scratch briefs; t5 one-wide send-back; t6 destructive() quote-aware; t7 denial message names offending effect (each RED->GREEN proven; targeted swarm+acp run 646 passed 0 failed, --test-threads=4)
- Evidence refs:
- cargo test --lib -- --test-threads=4 swarm:: acp:: => 646 passed 0 failed (after t7)
- Blocked on: none; full-suite runs avoided on user request (CPU) - targeted filters only
- Next step: Task 8 in schedule.rs/panel, then t9 retention, t10 game.sh; t11 needs live model runs (CPU-heavy) - flag at end
## Checkpoint Update

- Current todo: t4 done; t5 in progress: OneWideFirstRank one-shot send-back
- Active slice: Task 5 of docs/PLAN-polish.md: ShapeWarning::OneWideFirstRank promoted from note to one-shot send-back
- Completed todos:
- t1 stop_process deadlock; t2 retire_verifier; t3 AmendError::NoOp; t4 briefs name scratch + release_checkouts removes it (RED brief-empty -> GREEN; full suite 1496 passed 0 failed)
- Evidence refs:
- cargo test --lib 1496 passed 0 failed after t4
- Blocked on: none
- Next step: Task 5 edits in engine.rs plan-acceptance path + plan.rs if needed, then t6 permissions.rs destructive()
## Checkpoint Update

- Current todo: t3 done; t4 in progress: roles.rs briefs name scratch dir, engine removes it
- Active slice: Task 4 of docs/PLAN-polish.md: briefs name <run>/scratch/<member>, engine removes scratch with checkouts (defects 6+9)
- Completed todos:
- t1 stop_process deadlock; t2 retire_verifier; t3 AmendError::NoOp + handle_lead_amendment (RED Escalated->GREEN Consumed; full suite 1494 passed 0 failed)
- Evidence refs:
- cargo test --lib: 1494 passed 0 failed; no_op tests: amend NoOp x2 + engine replanning no-op
- Blocked on: none
- Next step: Task 4 edits in roles.rs + engine.rs release_checkouts, then Task 5
## Checkpoint Update

- Current todo: t2 done (retire_verifier, RED->GREEN proven); full lib suite running; t3 next: amend.rs typed NoOp
- Active slice: Task 2 of docs/PLAN-polish.md completed; verifying with full cargo test --lib
- Completed todos:
- t1: stop_process kill-before-lock + blocked-pipe test (RED 2.53s timeout -> GREEN 0.53s; suite 1492 passed 0 failed); t2: retire_verifier drops+stops verifier bridge on verdict, parks member (RED 'bridge must be dropped' -> GREEN)
- Evidence refs:
- cargo test --lib stop_returns_while (RED Elapsed then GREEN 0.53s); cargo test --lib retires_the_verifiers (RED bridge_gone then GREEN); full suite tail pending
- Blocked on: none
- Next step: Confirm full suite green, then Task 3: typed NoOp error in amend.rs + benign handling in engine.rs
## Checkpoint Update

- Current todo: t1: bridge.rs stop_process deadlock fix + blocked-pipe test (RED running)
- Active slice: Task 1 of docs/PLAN-polish.md: reorder stop_process to kill before stdin lock; remove 10s timeout in engine.rs release_bridges
- Completed todos:
- none
- Evidence refs:
- cargo test --lib stop_returns_while (RED expected pre-fix); tree did not compile pre-fix: engine.rs:6155 bare Duration
- Blocked on: none
- Next step: Confirm RED, apply reorder fix, confirm GREEN, run full cargo test --lib
