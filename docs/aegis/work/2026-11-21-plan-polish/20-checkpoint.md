# Execute docs/PLAN-polish.md (12 swarm-harness defects, 11 tasks) - Checkpoint

## Current Checkpoint

- Current todo: t1-t10 done and pushed; t11 skipped per user (games tested in build); cleanup+upload done
- Active slice: PLAN-polish execution complete except t11; repo published
- Completed todos:
- t1-t10 verified (649 rust tests targeted, 291 frontend, tsc, release build); folder cleaned (.scratch .shots .codebase-memory .repos target/debug target/tmp deleted, user-confirmed); release rebuilt 2026-11-22 10:19-10:21 (0 sources newer than exe); git init+commit c550d05+push main to public santera-pixel/vibedesk (289 tracked files, 0 junk); github MCP server added to ~/.vibe/config.toml (stdio npx @modelcontextprotocol/server-github, token via ~/.vibe/.env GITHUB_PERSONAL_ACCESS_TOKEN - user adds)
- Evidence refs:
- gh repo view santera-pixel/vibedesk => PUBLIC pushed 2026-11-22T08:28:34Z main; git status clean; git ls-files 289, 0 ignored-paths tracked; cargo build --release Finished 5m07s; find sources -newer release exe => 0
- Blocked on: none
- Next step: User runs the games (t11) in the build; re-index vibedesk in codebase-memory on next session (.codebase-memory was deleted)

## Recent Checkpoint History

## Checkpoint Update

- Current todo: t8-t10 done; t11 (end-to-end games) NOT started - needs live model runs (CPU-heavy), awaiting user go-ahead
- Active slice: Tasks 1-10 of docs/PLAN-polish.md complete and verified
- Completed todos:
- t1 stop_process deadlock; t2 retire_verifier; t3 AmendError::NoOp; t4 scratch briefs; t5 one-wide send-back; t6 destructive() quote-aware; t7 denial message; t8 live process count in panel + live_count; t9 checkout retention (reap, keep 10); t10 game.sh builds first
- Evidence refs:
- cargo test --lib -- --test-threads=4 swarm:: acp:: => 649 passed 0 failed; node --test tests/*.test.ts => 291 pass 0 fail; npx tsc --noEmit clean; npm run build clean; cargo build --bin swarm-run clean; bash -n game.sh ok; each fix test proven RED before fix
- Blocked on: t11 blocked on user approval (CPU constraint): live swarm runs peg the machine
- Next step: Task 11: re-run cpp-terrain-flyer from base cd1e776 + 4 games via game.sh - NOT started on user's no-lag constraint; needs explicit go-ahead (3 live agent processes, up to 3600s each)
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
