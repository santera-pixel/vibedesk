# Plan: provisioning a fresh machine

What VibeDesk must put on a machine so that a user who has never run Vibe gets a
working swarm, without hand-editing `~/.vibe`.

Written 2026-11-20. Supersedes nothing; extends `docs/PLAN-capabilities.md`,
which owns the capability layer this plan feeds.

## Why this exists

The swarm's lead is a real `vibe-acp` session. ACP has no field for a
client-supplied system prompt — `session/new` takes `cwd`,
`additional_directories` and `mcp_servers` and discards the rest — so the lead
inherits Vibe's own coding-agent prompt. That prompt says "Act directly and
proactively… If the task requires an edit, edit", and it outranks the brief,
which is only a user message.

The evidence is in the run that failed on 2026-11-20 at 22:22. Its session log
(`~/.vibe/logs/session/unified/59a009bf-…/generations/…/runtime-state.json`)
holds `core_system_instructions` beginning:

```
You are Mistral Vibe, a coding agent built by Mistral AI.
```

The lead was told "You are the lead of a swarm run. You plan, you integrate,
and you audit. You do not implement." It implemented anyway, because the system
prompt said to. That is the bug.

The fix is a custom agent: a TOML in `~/.vibe/agents/` naming a
`system_prompt_id`, and an MD in `~/.vibe/prompts/` holding the text. Vibe
replaces its own instructions with that text and appends project context and
AGENTS.md after it. ACP then advertises the agent as a `mode`, so
`session/set_config_option { configId: "mode", value: "vibedesk-swarm-lead" }`
switches the lead onto it. Verified end to end on 2026-11-20: a probe agent
appeared in `session/new`'s mode options and the session's
`core_system_instructions` became the probe text.

That mechanism is worthless if the user has to create those files by hand. So
this plan is about the files, and about the fact that a fresh machine has none
of them.

## Goal

A machine with no `~/.vibe` at all, and a fresh `mistral-vibe` install, reaches
a working swarm after one VibeDesk install and one launch. Every step is
recorded, every failure is named, and nothing is silently degraded.

## Non-goals

- Reimplementing Aegis, mastery, `codebase-memory-mcp` or `donsetch`. Each is
  installed through its own upstream mechanism.
- Bundling the Vibe CLI. `SetupScreen.tsx` already states the stance: VibeDesk
  wraps what the user has, so the agent always matches their `~/.vibe` config.
  That stance survives; this plan adds what goes *beside* the CLI.
- Provisioning on every launch. The ledger makes a second launch a probe, not
  an install.

## Architecture

### Two folders, split by who reads them

| Folder | Read by | Holds |
| --- | --- | --- |
| `~/.vibe/` (`paths::vibe_home()`, honours `VIBE_HOME`) | Vibe | `agents/`, `prompts/`, `skills/`, `aegis/`, `mastery/`, `plugins/`, `config.toml` |
| `<app_data>/provision/` (`paths::app_data_dir()`) | VibeDesk only | `ledger.json`, `payload/`, `downloads/` |

The split is the contract. Anything Vibe reads goes in the first; anything only
VibeDesk reads goes in the second. A file that both need is written to the first
and its hash recorded in the second.

### Module layout

```
src-tauri/src/provision/
  mod.rs          the Step enum, the run loop, the public status/run API
  ledger.rs       Record, State, Ledger (load, save, upsert)
  payload.rs      include_str! of the vendored agent TOMLs and prompt MDs
  steps/
    cli.rs              wraps install.rs
    swarm_agents.rs     writes payload into ~/.vibe/agents and ~/.vibe/prompts
    aegis.rs            clone + aegis-update.py register
    mastery.rs          plugin copy + partial sparse clones
    codebase_memory.rs  release download
    donsetch.rs         npm global install
    proxy_provider.rs   wraps capabilities::provider::enable
  payload/
    vibedesk-swarm-lead.md
    vibedesk-swarm-worker.md
    vibedesk-swarm-verifier.md
    vibedesk-swarm-reflector.md
    vibedesk-swarm-lead.toml
    vibedesk-swarm-worker.toml
    vibedesk-swarm-verifier.toml
    vibedesk-swarm-reflector.toml
```

An enum with a `match`, not a trait object: the repo has no `async-trait`
dependency, and `CapabilityId` already establishes enum dispatch as the house
style.

### The step contract

```rust
pub enum Step {
    Cli, SwarmAgents, Aegis, Mastery, CodebaseMemory, Donsetch, ProxyProvider,
}

impl Step {
    pub fn id(self) -> &'static str;
    pub fn title(self) -> &'static str;
    /// Steps that must be Ready before this one can run.
    pub fn requires(self) -> &'static [Step];
    /// Whether the machine already satisfies it, and at what version.
    pub fn probe(self) -> Probe;
    /// Do it. Idempotent: a second call on a satisfied machine changes nothing.
    pub async fn install(self, ctx: &Context) -> Result<String, String>;
}
```

`Probe` is `{ satisfied: bool, version: Option<String>, detail: String }`.
`install` returns the detail line the ledger records on success.

### The ledger

`<app_data>/provision/ledger.json`:

```json
{
  "version": 1,
  "records": [
    {
      "id": "swarm-agents",
      "state": "ready",
      "version": "0.1.0",
      "at": "2026-11-20T23:22:57Z",
      "detail": "wrote 8 files into ~/.vibe/agents and ~/.vibe/prompts",
      "blocked": null
    }
  ]
}
```

`State` is `absent | installing | ready | failed | skipped`. Written after every
step, so a crash mid-provision leaves an accurate record rather than a machine
that looks provisioned and is not.

`skipped` and `failed` are different answers and stay different. `skipped` means
a precondition was not met — no node, no network, the user declined. `failed`
means the step ran and did not work. The panel renders them differently and the
retry button only appears on `failed`.

### Ordering

```
cli → swarm-agents → aegis → mastery → codebase-memory → donsetch → proxy-provider
```

`swarm-agents` requires nothing: the payload is in the binary. It runs even when
`cli` failed, so the files are in place the moment the CLI arrives.

`aegis` requires `cli`, because `aegis-update.py` is Python and the interpreter
comes from the same toolchain the CLI install used.

`mastery` requires `git` for the clones and `node` for the plugin's server. Both
are probed; a missing one yields `skipped` with the reason, because a machine
without git can still run a swarm — it just cannot have the mastery bundles.

`proxy-provider` requires `cli`, because it edits the config the CLI reads.

### Failure states

- A failed step does not stop the others. Each is independent by construction.
- An unmet `requires` yields `skipped` with the reason, never `failed`.
- The swarm refuses to start when `swarm-agents` is not `ready`, naming the file
  it expected. A lead on Vibe's coding prompt is the bug this plan exists to
  fix; running it silently is the workaround this repo forbids.
- Every step is additive and reversible. `config.toml` gets a `.bak` first,
  which `provider.rs` already does.

## The vendored payload

Eight files, a few KB, `include_str!`'d into the binary. They are VibeDesk's own
contract with the swarm, so a version skew between them and the engine is a
correctness bug, not a preference. That is why they are vendored and Aegis is
not.

Written content-addressed: read the destination, compare bytes, write only when
they differ. A matching file is left alone, so a user who edited one keeps their
edit until VibeDesk's payload changes — and then it is overwritten, which the
ledger records.

### What the lead prompt must say

The brief already owns the plan shape and the gate rules. The system prompt owns
behaviour, and only behaviour, so the two do not become two owners of one rule.

1. Identity: you are the lead of a swarm run; you plan, integrate and audit.
2. The prohibition, with its reason: you do not implement. You have no worktree
   and no task; a file you edit is a file no verifier will check.
3. What it may do: read, search, run read-only commands, write JSON to its inbox.
4. Aegis and mastery, conditionally. The user's `~/.vibe/AGENTS.md` follows the
   custom prompt, so on a machine with Aegis installed the lead inherits the
   routing prefix automatically. On a machine without it, the prompt must still
   work: "if the Aegis pack is installed, route each task through it; if it is
   not, assign from the skill catalogue you were given." Same for mastery — name
   `mcp_mastery.load` only as the mechanism when the bundle store exists.
5. Nothing else. No plan shape, no gate rules, no repository recitation.

## Compatibility boundary

- `install.rs` keeps its job and its public functions. The `cli` step calls
  `install::status()` and `install::run()`; it does not replace them.
- `capabilities/external.rs` keeps find/register/report and gains an install
  half. Its `blocked` field stays the reason a capability is unavailable.
- `capabilities/provider.rs` is unchanged; the `proxy-provider` step calls
  `provider::enable`.
- `SetupScreen.tsx` grows from one step to a list. Its existing single-step path
  becomes the `cli` row.
- `swarm/roles.rs::mode_for` returns the namespaced agent names. Every call site
  already passes the result to `spawn_member`, so the change is local to that
  function plus the guard in `spawn_member`.

## TDD Route

- Mode: `off` (the repo's default; no explicit TDD request in this workstream).
- Decision: `skipped`.
- Posture: a regression test per task, written alongside the change, which is
  this repo's existing convention (1383 lib tests, 290 node tests).
- Reason: no strict authority. The ledger state machine and the spawn guard are
  the two places where a test is not optional, and both are named in their task.

## Tasks

### 1. The ledger

Files: `provision/mod.rs`, `provision/ledger.rs`.

`Record`, `State`, `Ledger::load`, `Ledger::save`, `Ledger::upsert`. Load
tolerates a missing or corrupt file by starting empty and saying so in the
returned detail, rather than failing the panel.

Proves: a record round-trips; a corrupt file yields an empty ledger and a
detail; `upsert` replaces by id and preserves order.

### 2. The step contract and the run loop

Files: `provision/mod.rs`.

`Step` with `id`, `title`, `requires`, `probe`, `install`. `run_all` walks the
order, skips a step whose `requires` are not `ready`, records each outcome, and
returns the ledger. `run_one` re-runs a single step for the retry button.

Proves: a failed step does not stop the next; an unmet `requires` yields
`skipped` with the reason; `run_one` on a `ready` step is a no-op.

### 3. The payload and the `swarm-agents` step

Files: `provision/payload.rs`, `provision/steps/swarm_agents.rs`,
`provision/payload/*`.

Writes the eight files into `paths::vibe_home()/agents` and `.../prompts`,
creating the directories. Content-addressed. Returns the count written and the
count left alone.

Proves: writes when absent; leaves an identical file untouched; rewrites a
changed one; a read-only destination returns an error naming the path.

### 4. The four role prompts

Files: `provision/payload/vibedesk-swarm-{lead,worker,verifier,reflector}.md`.

The lead prompt per the section above. The worker prompt states the file
boundary and that the verifier runs the criterion both ways. The verifier prompt
states that it never sees the worker's transcript and must name a command that
fails on the base. The reflector prompt states that it writes one file.

Proves: each prompt names its role's prohibition; the lead prompt mentions Aegis
and mastery conditionally, not unconditionally; no prompt restates the plan
shape, which the brief owns.

### 5. Wire the roles to the agents and guard the spawn

Files: `swarm/roles.rs`, `swarm/engine.rs`.

`mode_for` returns `vibedesk-swarm-lead` and so on. `spawn_member` gains a guard:
after `session/new`, if the role's mode is not in the offered options, return an
error naming the agent and the file it expected. The existing
`if let Ok(result) = bridge.request(...)` around `set_config_option` becomes a
`match` that propagates the error for a swarm role.

Proves: a session that does not offer the mode makes `spawn_member` return an
error containing the agent name; a session that offers it proceeds; the
reflector's `accept-edits` path is unchanged.

### 6. The `cli` step

Files: `provision/steps/cli.rs`.

Wraps `install::status()` and `install::run()`. `probe` reports satisfied when
`resolve_acp_binary` succeeds, with the version from `vibe-acp --version`.

Proves: probe reports found and not found; install delegates and records the
outcome.

### 7. The `aegis` step

Files: `provision/steps/aegis.rs`.

Clone `https://github.com/GanyuanRan/Aegis` at the pinned tag `v2.15.2` into
`~/.vibe/aegis`, then run `python scripts/aegis-update.py register --host vibe
--json` and parse the JSON. `probe` reports satisfied when
`~/.vibe/skills/using-aegis/SKILL.md` exists and the registry names the host.

The tag is pinned rather than tracking `main` because the method pack decides
how every turn is routed: a user's swarm behaviour must not change between two
launches of the same VibeDesk build. `v2.15.2` is the version verified on
2026-11-20 and is a real tag on the remote.

Proves: probe on a missing directory; a second install is a no-op; a failed
clone leaves any existing `~/.vibe/aegis` untouched.

### 8. The `mastery` step

Files: `provision/steps/mastery.rs`.

Two halves. The plugin: copy the vendored `vibe-mastery` directory into
`~/.vibe/plugins/vibe-mastery`. It is vendored because the repo is not published
yet — `github.com/santera-pixel/dsh-mastery` goes up when VibeDesk is production
ready — so there is nothing to clone. When it is published, the step can gain a
clone path, but the vendored copy stays the default: 82 KB in the bundle beats a
network round trip on first launch.

The bundles: for each of the three, a partial sparse clone with the cone
verified on 2026-11-20.

| Bundle | Repo | Cone |
| --- | --- | --- |
| `3dviz-pro-max` | `viettranx/3dviz-pro-max` | `skills/3dviz-pro-max` |
| `Anti-Slop` | `miqdadbadjuber/Anti-Slop` | `skills rules` |
| `ui-ux-pro-max-skill` | `nextlevelbuilder/ui-ux-pro-max-skill` | `cli/assets/skills` |

`git clone --filter=blob:none --sparse --depth 1`, then
`git sparse-checkout set <cone>`. Clone to `<root>.new`, verify at least one
`SKILL.md`, then swap. 20 MB total instead of 201 MB, and `git pull` still works
because the clone is a real clone.

Proves: the cone list matches the table; a clone failure leaves the old root;
the skill count after install is 1, 6 and 6.

### 9. The `codebase-memory` and `donsetch` steps

Files: `provision/steps/codebase_memory.rs`, `provision/steps/donsetch.rs`.

`codebase-memory`: download the release asset for the platform from
`github.com/DeusData/codebase-memory-mcp` into `<app_data>/tools/`, which
`external::find` already searches.

The numbers, measured on 2026-11-21 against release v0.12.1:

| | Size |
| --- | --- |
| Windows x64 asset, `codebase-memory-mcp-ui-windows-amd64.zip` | 46.3 MB |
| extracted `codebase-memory-mcp.exe` | 318 MiB (333,716,480 bytes) |
| `~/.cache/codebase-memory-mcp/` on this machine | 135 MB |

The download is 46 MB, not 333 MB. 333 MB is the byte count of the extracted
binary, and the panel must show the download figure, because that is what the
user waits for. Windows publishes only the `ui` asset; the zip holds the single
executable and nothing else, which is why the install directory on a working
machine contains exactly one file.

**Never copy `~/.cache/codebase-memory-mcp/`.** It holds the per-machine index
databases — `vibe-home.db`, `vibedesk.db`, `activity.db`, and one `.db` per
indexed workspace — which are the user's data, not part of the tool. They are
also the bulk of the footprint: 135 MB here against 46 MB for the download. The
step writes the binary and nothing else, and the installer bundles neither.

`donsetch`: `npm i -g donsetch`. `skipped` with the reason when npm is absent.

Both then call `external::register` so the server lands in `config.toml`.

Proves: probe reports the version from `--version`; a failed download reports the
URL and the reason; a missing npm yields `skipped`, not `failed`.

### 10. The `proxy-provider` step

Files: `provision/steps/proxy_provider.rs`.

Calls `provider::enable(port)` when the proxy is needed, `provider::disable`
otherwise. `probe` reports `provider::is_registered()`.

Proves: probe reflects registration; install is idempotent, which
`provider::enable` already guarantees.

**Measured, and a gap this step exposed.** `provider::enable` mirrors the
`[[models]]` entries that use the upstream provider, because a mirrored model is
a copy of a real entry with its `provider` rewritten. A fresh `config.toml` runs
on the CLI's built-in Mistral Medium 3.5, which is *server-routed*: it has no
`[[models]]` entry, so there is nothing to copy and nothing for the proxy to
serve. `enable` therefore fails with "no active model is configured" on the
default configuration — a pre-existing bug, not one this plan introduced.

The step reports it as `skipped` with that reason rather than `failed`, because
retrying cannot change it and a retry button that cannot work is a lie. The
precondition lives in `provider::mirrorable()`.

The real fix is to take the canonical model name from the agent's own `model`
config option — its `description` is the API name, verified against
`vibe-acp` 3.1.2 — and synthesise the `[[models]]` entry from it. That needs a
live session, so it belongs where the catalog is already read, not in
`provider.rs`. Until then the three boundary capabilities (compaction, reading,
terminal) do not apply to a server-routed active model, and the panel says so.

### 11. The commands and the setup screen

Files: `commands/provision.rs`, `lib.rs`, `src/components/setup/SetupScreen.tsx`,
`src/lib/bridge.ts`, `src/types/acp.ts`.

Three commands: `provision_status`, `provision_run`, `provision_run_step`. The
screen renders one row per step with its state, its detail, a retry on `failed`
and a skip on `absent`. The `cli` row keeps the existing install button.

Proves: the screen renders every step from the ledger; a `failed` row offers
retry; a `skipped` row shows its reason and offers no retry.

### 12. The swarm start guard and the end-to-end proof

Files: `swarm/engine.rs`, `commands/swarm.rs`.

`Engine::start` reads the ledger and refuses when `swarm-agents` is not `ready`,
returning the step's detail and the retry path.

Proves, on a machine with `VIBE_HOME` pointed at an empty temp directory:

1. `provision_run` reaches `ready` for `swarm-agents` with no network.
2. `~/.vibe/agents/vibedesk-swarm-lead.toml` and
   `~/.vibe/prompts/vibedesk-swarm-lead.md` exist and match the payload.
3. An ACP `session/new` against that `VIBE_HOME` advertises
   `vibedesk-swarm-lead` in the `mode` options.
4. A swarm run's lead session log has `core_system_instructions` beginning with
   the lead prompt, not "You are Mistral Vibe".
5. The lead writes `plan.json` and the run reaches `AwaitingApproval` without the
   lead having edited a file.
6. Delete `~/.vibe/agents/`, relaunch: the step reports `absent`, the swarm
   refuses with a message naming the file, and the retry restores it.

Step 4 is the one that proves the bug is fixed, and it is read from the same
file that diagnosed it.

## Risks

| Risk | Consequence | Treatment |
| --- | --- | --- |
| `codebase-memory-mcp` is a 46 MB download that expands to 318 MiB | a fresh install pulls a large binary | shown before the step runs, skippable, and the capability panel already reports it as unavailable with a reason |
| The index cache is mistaken for part of the tool | a 135 MB user-data directory copied into an installer or a payload | the step writes the binary only; `~/.cache/codebase-memory-mcp/` is named as excluded in task 9 and is never read by provisioning |
| `vibe-mastery` lives at `github.com/santera-pixel/dsh-mastery` | a product depending on a personal repo is a supply-chain decision | vendor the plugin (82 KB) into the bundle rather than clone it. The repo is not published yet — it goes up when VibeDesk is production ready — so vendoring is the only option today, not merely the preferred one |
| Aegis is third-party with its own installer | an upstream change breaks the step | pin a tag; the step reports the version it installed |
| `npm i -g donsetch` needs node | no node means no web capability | `skipped` with the reason, not `failed` |
| No network at first run | a half-provisioned machine | every step records its own state; the panel offers per-step retry; nothing is silently degraded |
| Writing into `~/.vibe` | the user's directory | additive and reversible; `.bak` before `config.toml`; namespaced agent names so a user's own agent is never clobbered |
| A server-routed active model has no `[[models]]` entry | the proxy cannot be registered, so compaction, reading and terminal do not apply to it | `provider::mirrorable()` makes it a `skipped` step with the reason and the fix; the real fix is recorded in task 10 |

## Retirement

- `SetupScreen.tsx`'s single-step state machine is replaced by the ledger view.
  The `cli` install path it owns is kept, moved behind the `cli` row.
- `install.rs` is not retired. It becomes the `cli` step's implementation.
- `external.rs`'s "report only" stance is not retired either; it gains an install
  half and keeps its report, which is what the capability panel reads.
- No fallback path is added. A machine that cannot be provisioned says so and
  the swarm refuses to start; it does not run on the wrong prompt.

## Verification

- `cargo test --lib` — the existing 1383 plus the new provision tests.
- `cargo test --test swarm_e2e` — the existing 6 plus the guard case.
- `cargo build --all-targets` — zero warnings.
- `npx tsc --noEmit`, `node --test tests/*.test.ts`, `npm run build`.
- The six-step end-to-end proof in task 12, run against an empty `VIBE_HOME`.
