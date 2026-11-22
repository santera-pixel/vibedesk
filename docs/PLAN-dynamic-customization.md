# Plan v2: dynamic customization, live data, nested layout

Status: `Implemented` — see `docs/PLAN-STATUS.md` for what shipped, phase by
phase, with the evidence for each claim.

Supersedes v1 of this document. v1 was a feature list; this is an architecture.
The delta is in §1.

---

## 1. What changed from v1, and why

v1 proposed four new block kinds and a `{{source.path}}` string template. That
was wrong in three ways, and the research below is what showed it.

| v1 | v2 | Why |
|---|---|---|
| `{{source.path}}` string interpolation | `{"@path": "/ci/summary/state"}` binding objects | A string template cannot be validated, cannot be typed, and cannot express a condition. A2UI binds by path object; so should we. |
| A `tabs` block | A recursive layout tree: `tabs` / `split` / `stack` | "Nested tabs" is one case of a general layout tree. A block that contains blocks is a layout node, not a block. |
| Data fetched per block | One data model per panel, fed by declared sources | Per-block fetching duplicates requests and makes a panel's state incoherent. A2UI separates structure from data; that separation is what makes live updates cheap. |
| Only external HTTP sources | Built-in sources first, external second | Most of what a panel wants to show is VibeDesk's own state. That needs no network, no approval friction, and no risk. |
| `when` as a field with `equals` | A bounded expression language | `equals` covers one case. A panel needs `> 10`, `and`, `exists`. Bounded and non-Turing-complete, so it stays safe. |

---

## 2. Research findings

### 2.1 A2UI — the closest solved problem

Google's A2UI is an open protocol for exactly this: an agent describing UI that a
client renders natively, safely. Its stated philosophy is *"expressing UI as
data, validating inputs and outputs against a schema, and disallowing arbitrary
executable code. Every capability granted to the agent is either a safe standard
capability built into the framework, or a capability explicitly granted by the
app developer."*

The v1.0 spec defines four messages — `createSurface`, `updateComponents`,
`updateDataModel`, `deleteSurface` — and, critically, **a clean separation of UI
structure and application data**, so a value can change without re-sending the
structure. Bindings are `@path` objects; the client keeps a **catalog** of
trusted components and the agent may only request from it.

What VibeDesk takes from it:

- the **surface / components / data model / catalog** split
- **path bindings** rather than string templates
- **structure and data as separate update channels** — this is what makes live
  panels cheap
- the **catalog as the capability boundary** — the agent composes, it does not
  invent

What VibeDesk does not take: the wire protocol. VibeDesk panels are persisted
files, not a stream from a remote agent. We adopt the *shape*, keep
`panels.json` as the storage form, and add a streaming path later if needed.

### 2.2 Home Assistant Lovelace — the config-driven dashboard precedent

Lovelace replaced a static generated UI with a fully configurable YAML system.
Its structure is *view → sections → cards*, cards nest, and each card type is a
component loaded only when a dashboard uses it. Two lessons:

- **Nesting is the norm, not a feature.** A dashboard that cannot nest is a list.
- **Lazy component loading matters** when the catalog grows.

### 2.3 Grafana — the datasource abstraction

Grafana separates *panels* from *datasources*, and distinguishes query-based
datasources from **streaming** ones that "establish persistent connections that
push data updates to dashboards in real time". Grafana Live is a pub/sub server
where clients subscribe to named channels.

Lesson: **a panel does not know how its data arrives.** It binds to a source id.
Whether that source polls, streams, or is computed internally is the source's
business. This is why v2 has a source registry rather than per-block fetches.

### 2.4 Dockview — the layout tree

Dockview's model is *grid → groups → tabs*, where "the layout above the groups
is the grid: nested horizontal and vertical splits that the library manages and
that serialization writes out". Deep nesting, tabs, and serialisation are one
mechanism.

Lesson: **layout is a tree that serialises**, not a set of flags. v2's
`tabs`/`split`/`stack` nodes are that tree, minus drag-and-drop.

### 2.5 CEL — how to make expressions safe

The Common Expression Language is described as giving *"deterministic, bounded
evaluation in a sandboxed rule language"*, is **non-Turing-complete**, and is
side-effect free. That is the correct shape for panel conditions and computed
values: expressive enough for `queue.depth > 10 and status != "ok"`, incapable
of looping, allocating, or calling out.

Lesson: v2's expression grammar is CEL-shaped — literals, paths, comparison,
boolean, arithmetic, formatters. No loops, no recursion, no function calls, no
user-defined names.

### 2.6 SSE vs polling — and the backpressure question

SSE is one-way server-to-client over plain HTTP and is the right choice "when
the client only listens (LLM token streams, dashboards, notifications, log
tails)". Polling is fine when data changes slowly. The decisive question is
**backpressure**: *"a slow client must not let the server accumulate an
unbounded queue of unsent events, so you need a policy for dropping, coalescing,
or disconnecting slow consumers."*

Lesson: v2 supports both modes, and its backpressure policy is **coalesce** —
if a fetch is still in flight when the next tick fires, the tick is skipped.
Nothing queues. For SSE, a bounded buffer drops oldest and counts the drops.

### 2.7 Capability security for plugin UI

The recurring failure in plugin systems is **event hijacking**: *"untrusted
plugins can listen to or publish sensitive internal events across the shared
eventBus"*. The mitigation is iframe sandboxing with only the permissions
required, and scoped channels rather than a shared bus.

Lesson: a plugin-contributed panel gets its **own** data channel and its own
sources. It never sees the app's event bus, the agent's notifications, or
another plugin's data.

---

## 3. The model

### 3.1 A panel is a surface

```
Panel (surface)
├── sources[]      declared data inputs
├── data           the data model, keyed by source id
└── layout         a tree of layout nodes whose leaves are blocks
```

Structure and data are separate. A live update replaces part of `data`; the
layout tree is untouched, so nothing remounts.

### 3.2 Layout nodes

Three node kinds, recursive:

| Node | Shape | Purpose |
|---|---|---|
| `tabs` | `{ tabs: [{ label, children: [node|block] }] }` | Nested tabs |
| `split` | `{ direction: "row"\|"column", children: [node], sizes: [number] }` | Resizable panes |
| `stack` | `{ children: [node\|block] }` | Vertical flow |

A block is a leaf. A `tabs` node may contain a `split`, which may contain a
`tabs`, which may contain blocks. That is what "nested" means here.

Bounds: depth ≤ 4, total nodes + blocks ≤ 60, tabs per node ≤ 8, label ≤ 40.

### 3.3 Blocks (the catalog)

The existing six stay. Four are added. The catalog is published as JSON Schema
in the manifest so the agent composes from it rather than guessing.

| Block | Why no existing kind covers it |
|---|---|
| `data` | `text` holds a literal; nothing can display a bound value |
| `chart` | a number cannot show a trend |
| `table` | a `list` is one column of strings; a table is rows and columns |
| `progress` | a bar bound to a value is the single most common panel element |

`when` is a **field on every node and block**, not a kind.

### 3.4 Bindings and expressions

A value position accepts either a literal or an expression object:

```json
{ "kind": "data", "label": "state", "value": { "@path": "/ci/summary/state" } }
```

```json
{ "kind": "data", "label": "queue",
  "value": { "@format": "number", "@of": { "@path": "/ci/queue/depth" } } }
```

Conditions:

```json
{ "when": { "@and": [
    { "@gt": [{ "@path": "/ci/queue/depth" }, 10] },
    { "@ne": [{ "@path": "/ci/summary/state" }, "ok"] }
]}}
```

Grammar, deliberately small:

| Group | Operators |
|---|---|
| Path | `@path` |
| Compare | `@eq @ne @gt @gte @lt @lte` |
| Boolean | `@and @or @not` |
| Presence | `@exists @missing` |
| Arithmetic | `@add @sub @mul @div` |
| Format | `@format` with `number currency percent duration date truncate` |

Bounds: expression depth ≤ 8, nodes ≤ 64. No loops, no recursion, no function
calls, no user-defined names. Parsed and type-checked **in Rust at approval
time**, so a bad binding fails with a message the agent reads in its receipt
instead of rendering `undefined` at runtime.

### 3.5 Sources

Two families.

**Built-in** — VibeDesk's own state, no network, no approval friction:

| Source | Contents |
|---|---|
| `vibedesk.status` | agent state, pid, version, restarts, uptime |
| `vibedesk.usage` | used, size, cost, prompt/completion/cached tokens, hit rate |
| `vibedesk.session` | session id, title, cwd, busy, stop reason |
| `vibedesk.chats` | total, visible, archived |
| `vibedesk.plugins` | installed plugins and their state |
| `vibedesk.clock` | now, in ISO and epoch |

These are the ones that make a panel live with zero risk, and they should be
the default thing an agent reaches for.

**External** — `http`, with `mode: "poll"` (default) or `"sse"`:

```json
{ "id": "ci", "type": "http", "url": "http://127.0.0.1:8787/status",
  "mode": "poll", "intervalMs": 2000, "timeoutMs": 5000 }
```

| Rule | Value | Why |
|---|---|---|
| Scheme | `http` | loopback TLS is cert pain for no gain |
| Host | `127.0.0.1`, `::1`, `localhost` | the agent may not widen its own reach |
| Userinfo / fragment | forbidden | credential smuggling, client-side tricks |
| `intervalMs` | 1000–60000 | a panel is not a load generator |
| `timeoutMs` | 500–15000 | bounded wait |
| Sources per panel | 8 | complexity budget |
| Body | 256 KiB, JSON, depth ≤ 32, nodes ≤ 10000 | bounded parse |
| Redirects | not followed off loopback | prevents a bounce to the internet |

**Non-loopback is not expressible in `panels.json`.** It requires a
`settings.json` entry the user edits by hand. The agent may propose a panel; it
may not propose a wider network capability. This is the load-bearing safety
property.

### 3.6 The data plane

```
panels.json declares sources
        |
        v
Rust validates: scheme, loopback, caps, interval floor
        |
        v
Rust registry: one task per distinct source, coalescing
        |
        v
Rust emits  panel://data  { panelId, sourceId, value, at, error }
        |
        v
Frontend store: Map<panelId, Map<sourceId, value>>
        |
        v
Bound nodes re-render; layout tree untouched
```

Why Rust and not the webview: the CSP is `connect-src 'self' ipc:`. The webview
has **no network reach at all**. Loosening that would hand every agent-authored
panel the ability to reach any host the machine can reach. Fetching in Rust
keeps the webview's capability unchanged and confines the new capability to
code the user approved.

Backpressure: **coalesce**. A tick that fires while a fetch is in flight is
skipped, never queued. SSE uses a bounded buffer that drops oldest and counts
drops, surfaced in the Status panel.

---

## 4. What becomes possible

This is the answer to "so much more". Each item is a consequence of the model
above, not a separate feature.

| Capability | Enabled by |
|---|---|
| A panel showing live agent state | built-in `vibedesk.*` sources |
| A panel showing a local service's health | external `http` source |
| Nested tabs to depth 4 | `tabs` node |
| Resizable side-by-side panes inside a panel | `split` node |
| A block that hides when there is nothing to say | `when` |
| A value computed from two sources | expression `@add`, `@div` |
| A trend line over the last N samples | `chart` + a source that returns an array |
| A table of tool calls | `table` bound to `vibedesk.session` |
| A button that is disabled until a condition holds | `when` on an `actions` block |
| The agent building a panel on request | proposal → approve → live, no restart |
| A plugin shipping its own panel | plugin contribution, namespaced |
| A panel that survives a restart with its tab selection | panel-local state |
| Progressive panel construction | structure and data are separate channels |
| Keyboard navigation across tabs | `tabs` node with roving tabindex |

---

## 5. Safety model

| Threat | Control |
|---|---|
| Agent widens network reach | `panels.json` may only declare loopback; non-loopback is user-only in `settings.json` |
| Panel reads a local admin API | approval diff shows the exact URL; global kill switch; per-panel disable; Status lists every active source and its last fetch |
| Loopback service bounced to the internet | redirects not followed off loopback |
| Response floods memory | 256 KiB cap, depth and node caps, early abort |
| Polling melts the machine | interval floor 1s, 8 sources per panel, 4 concurrent, coalescing |
| Expression used as a language | non-Turing-complete grammar, depth ≤ 8, parsed in Rust |
| Nested layout blows the stack | depth ≤ 4, node count ≤ 60, recursive validator |
| Plugin panel hijacks app events | plugin panels get a scoped channel and their own sources; no app event bus |
| Plugin panel collides with a user panel | namespaced ids, user ids win |
| A bad source breaks the app | fetch errors become values, never panics |
| Safe mode bypassed | `VIBEDESK_SAFE_MODE=1` disables all sources and customization |

---

## 6. Phases

Each phase ends with evidence. A phase is not done until its evidence exists.

### Phase 0 — Repair the turn lifecycle — **DONE**

The 120s defect. `AcpBridge::request` applied `REQUEST_TIMEOUT` to every
request, including `session/prompt`, so a long agentic turn was abandoned while
the agent kept working, and `set_busy(false)` then let the next prompt through
to a `-32603 A turn is already running`.

What shipped:

- **Idle watchdog instead of a wall clock.** `await_turn` waits on the reply
  channel and restarts a 180s silence window every time the agent emits a line.
  A turn is bounded by silence, not by duration.
- **Three signals that make silence legitimate.** The watchdog only fires when
  the agent is quiet *and* nothing explains why: a pending approval or
  elicitation, an inbound request the agent has open against us
  (`terminal/wait_for_exit` blocks for the whole command), or a `tool_call` it
  announced and has not finished. The third one was found in testing — a 250s
  shell command emits nothing at all, and without it the watchdog called a
  working agent wedged.
- **`busy` is never cleared on a timeout.** A stall returns
  `AppError::TurnStalled` and hands the reply receiver to a reaper task that
  clears `busy` when the real answer lands. The turn is not abandoned, only the
  wait for it.
- **A second prompt is refused locally.** `turn_active` is claimed with a
  compare-and-swap, so a concurrent prompt gets `AppError::TurnInProgress`
  instead of a round trip that earns `-32603`.
- **`session/load` uses the same idle wait.** It replays the transcript as
  `session/update` notifications, so a large chat could outlast any fixed
  deadline. Every other `request()` caller was audited and left on a fixed
  timeout: they are all short metadata reads.
- **The webview follows the backend.** `setStatus` treats the agent status as
  authoritative for `busy`, so a turn that ends after the UI gave up on it
  clears the Stop control instead of leaving it stuck.

**Evidence:**

| Check | Result |
|---|---|
| 154s turn (old limit 120s) | completed, no timeout notice |
| 257s turn with a 250s silent shell command | completed, **zero** stall warnings |
| Second prompt during a turn | `a turn is already running — stop it before sending another prompt` |
| Stop during a turn | busy cleared in 1s, "Turn cancelled." |
| Stall surfaced (pre-tool-tracking build) | warning shown at 180s, turn kept running, result landed at 258s |
| `cargo test --lib` | 166 passed, 0 failed |
| `cargo check` | zero warnings |

Ten of those tests are fake-clock tests over `await_turn`: a ten-minute talking
turn, a silent turn, a late reply after a stall, a pending approval, an open
tool request, a running tool call, a finished tool call, unrelated
notifications, a closed channel, and an agent error.

#### Addendum — a prompt typed mid-turn is held, not refused

ACP has no steer method. `vibe/acp/agent.py` forwards `session/prompt` straight
to `app_server.act`, and `app_server/session.py:358` raises
`A turn is already running` for a second one. Steering exists one layer down
(`turn/steer`, `session/turn/queue/steer`) but is not reachable over ACP, so
VibeDesk cannot inject into a live turn.

What it can do is hold the message. `sendPrompt` enqueues instead of refusing,
the transcript shows the held prompt in a dashed shell with a Cancel control,
and `drainQueue` dispatches it the moment `busy` falls. The queue is keyed by
session, so a prompt typed in one chat never lands in another.

That change exposed a second defect worth recording: **a `ready` status is not
evidence that the current turn ended.** The status for the turn that just
finished can arrive after the next one has already started, and clearing `busy`
on it tore down a live turn — including the `pending` flag the echo needs to
fold into the right row, which is why a drained prompt appeared twice. `busy` is
now cleared by exactly two things: the `sendPrompt` that owns the turn, and a
new `agent://turn-ended` event the reaper emits when a stalled turn finally
answers. `setStatus` only clears it for `failed` and `stopped`, which are
unambiguous.

`VIBEDESK_TURN_IDLE_SECS` overrides the 180s window for slow models and for
testing the watchdog without waiting three minutes.

### Phase 1 — Model v2 — **DONE**

- Recursive validator: layout nodes, depth, node count, catalog.
- Expression parser and type checker in Rust.
- Catalog published as JSON Schema in the manifest.
- `panels.json` v1 files still load (migration, not breakage).

**Evidence:** tests for depth, count, nested unknown kind, bad expression, bad
path, v1 migration; a nested panel renders.

#### What was built

| Piece | Where | Notes |
|---|---|---|
| Expression language | `src-tauri/src/expr.rs` | 8 operator shapes, 6 formats, depth 8, 64 nodes, 25 tests |
| `Block` wrapper | `panels.rs` | `when` on every kind; JSON identical to v1 when absent |
| 4 bound kinds | `panels.rs` | `data`, `chart`, `table`, `progress` → 10 total |
| Layout tree | `panels.rs` | `tabs` / `split` / `stack`, hand-written `Node` deserializer |
| Migration | `CustomPanel::reconcile` | v1 `blocks` → a single `stack`; `blocks` always derived |
| Catalog | `manifest.rs` | operators, formats, node kinds, chart styles, limits, a validating example, per-panel `bindings` |

#### Decisions worth recording

**`blocks` is derived, not authored.** `CustomPanel::layout` is the source of
truth; `blocks` is its document-order flattening, recomputed on load and on
save. It is not written to disk when `layout` is present, so a hand edit to the
derived view cannot sit in the file looking authoritative while being ignored.
A v1 file has no `layout`, so it is upgraded to a single `stack` in memory and
written back as v2 on the next save.

**`Node` is deserialised by hand.** `#[serde(untagged)]` would have produced
"data did not match any variant of untagged enum Node" for a node with neither
tag. The agent reads these errors in its receipt, so it gets one sentence naming
both tags instead.

**`Type::Any` is the honest type for a path.** The checker rejects only what is
certainly wrong — `@gt` over two booleans, `@and` over a number — and lets a
path through everywhere, because the shape of the data is not known until a
source answers. A checker that guessed would reject valid panels.

**The manifest example is a test.** `the_published_example_is_a_valid_panel`
loads the example the agent is told to copy, validates it, and round-trips it.
Documentation that does not compile is worse than no documentation.

**`paths()` has a real consumer.** Rather than silence a dead-code warning, the
manifest now publishes `bindings: [{ id, paths }]` per panel — the list of paths
each panel promises to read. Phase 2 matches those against declared sources, and
until then it is the answer to "why is this panel empty".

**The four bound kinds render honestly.** They have no evaluator until Phase 2,
so the renderer shows the label and "no source is connected yet" rather than an
empty box. A blank panel looks like a panel with nothing to say, which is a lie.

#### Verification

| Check | Result |
|---|---|
| `cargo test --lib` | 219 passed, 0 failed |
| `cargo check --lib` | 0 warnings |
| `npx tsc --noEmit` | clean |
| v1 file loads and upgrades | `a_v1_panel_is_upgraded_to_a_single_stack` |
| v2 file derives `blocks` | `a_v2_panel_derives_its_blocks_from_the_layout` |
| stale `blocks` ignored | `a_hand_edited_blocks_view_is_ignored_when_a_layout_exists` |
| v1 → v2 on save, still loads | `a_v1_file_is_migrated_to_v2_on_save_and_still_loads` |
| depth / count / tabs / weights bounded | 6 tests |
| bad expression, bad path, bad `when` | 5 tests |
| manifest operators match the parser | `the_manifest_operators_are_exactly_the_parsers` |
| wire format is flat | `a_block_serialises_flat`, `a_panel_serialises_blocks_flat` |

#### Live verification (installed build, CDP)

| Check | Result |
|---|---|
| Manifest on disk carries the new surface | 10 block kinds, 3 node kinds, 17 operators, 6 formats, depth 8 / 64 nodes, 8 new limits, per-panel `bindings` |
| The three existing v1 panels still load | `workspace`, `sandbox-demo` and the third v1 panel all in the rail |
| A v1 panel renders | Workspace: text, file stub, list, actions — unchanged |
| A v2 panel with tabs loads, validates, flattens, renders | probe panel: 5 blocks from 2 tabs, in document order |
| The four bound kinds render honestly | "Bound value · jobs — no source is connected yet." |
| Zero console exceptions | confirmed after reload |

#### The bug this phase caught

The first build blanked the entire window on opening any panel:
`TypeError: Cannot read properties of undefined (reading 'kind')`.

Cause: I typed the frontend `Block` as `{ when?, body: PanelBlock }`, but
`#[serde(flatten)]` puts the body's fields **beside** `when`, not under a `body`
key. The wire format is `{ "kind": "text", "text": "…", "when": { … } }`. So
`block.body` was `undefined` and the renderer threw.

Two fixes, both kept:

1. `Block` is now `PanelBlock & { when?: Expr | null }` — the actual shape.
2. `BlockView` has a `default` case that names an unknown kind instead of
   returning nothing. A renderer that throws on unexpected data takes the whole
   window down; a renderer that says "this build cannot render it" does not.

And two tests pin the shape from the Rust side, so the next person to change
`Block` finds out from `cargo test` rather than from a blank screen.

#### Known gap, by design

`when` is parsed, type-checked, and stored, but **not evaluated** — there is no
data plane until Phase 2. A block with a `when` renders unconditionally today.
That is why the probe panel showed "Nothing is queued." even though
`/ci/queue/depth` has no value. Phase 3 evaluates it; until then the renderer
does not pretend to.

### Phase 2 — Data plane

- Source registry, built-in sources, poller, SSE reader, coalescing.
- `panel://data` event, frontend store, Status source list, kill switch.

**Evidence:** built-in sources update live; a loopback test server drives an
external source; every cap tripped by a test; safe mode disables all.

**Status: done.** `src-tauri/src/data.rs` (39 tests), `src/lib/data.ts`,
`src/store/panelData.ts`, `src-tauri/src/commands/data.rs`.

#### The split is the security boundary

Built-in sources resolve in the frontend; external sources fetch in Rust. That
is not an optimisation, it is the reason the webview CSP can stay
`connect-src 'self' ipc:`. Loosening it would hand every agent-authored panel
reach to any host. Built-ins read state the app already holds — `usage` lives
only in the frontend store — so routing them through IPC would be a round trip
plus a second copy of the truth.

#### The load-bearing rule

A `panels.json` source may only address loopback. Non-loopback is not
expressible in the file at all: `check_loopback_url` accepts eight spellings of
`127.0.0.1` / `::1` / `localhost` and refuses eight lookalikes including
`127.0.0.1.evil.com` and `localhost.evil.com`. `https` is refused, because
loopback TLS needs a certificate the app cannot verify. Redirects are refused
outright with `Policy::none()` rather than followed-then-checked.

#### Coalesce, never queue

The poll loop sleeps *after* the fetch completes, so a slow endpoint lowers its
own rate. A global `Semaphore(4)` is taken with `try_acquire_owned()`: a tick
that cannot get a permit is skipped, not queued, and the previous value stands.

#### Two evaluators, one contract

Rust parses and type-checks a binding at approval time; TypeScript evaluates it
at render time. Drift protection is `src/lib/expr-vectors.json` — 42 vectors.
The Rust test asserts every vector parses and type-checks and that the table
covers every operator; the frontend check runs the same table against the real
`evaluate` (esbuild the module, import it in Node, compare to `expect`).

#### Evidence

| Check | Result |
|---|---|
| `cargo test --lib` | 268 passed, 0 failed |
| `cargo check --lib` | 0 warnings |
| `npx tsc --noEmit` | clean |
| 42 shared vectors against the real evaluator | 42/42 |
| Live poll source | queue depth 292 → 295 over 2.5s |
| Live SSE source | tick 182 → 184; plane reports `2 running / 2 declared` |
| Live built-in source | `clock` epoch 1,794,801,291 → 1,794,801,293 |
| Kill switch | header `paused`, value frozen at 295, `0 running / 1 declared` |
| Resume | header `live`, 299 → 301 |
| Safe mode | `0 running / 0 declared`, no custom panels in the rail |
| Non-loopback refused | `source host example.com is not loopback`; live config kept, counter never reset |
| `when` true | guarded block rendered |
| `when` false | guarded block absent |
| Caps tripped | body >256 KiB, JSON depth >32, nodes >10000, interval 999/60001, timeout 499/15001, 9 sources, duplicate ids |
| Redirect refused | `a_redirect_is_refused_rather_than_followed` asserts `HTTP 302` |

#### Three bugs found while building this

1. **`rename_all` on the enum renames variants, not fields.** `intervalMs` and
   `timeoutMs` were silently ignored and the defaults applied — a panel polling
   at the wrong rate with no error. Fixed with `#[serde(rename_all =
   "camelCase")]` on the `Http` variant. Caught by a round-trip test.
2. **`status()` read the subscriber list off the running task.** Pausing emptied
   the list, so the user could not see what they had paused. The declared set is
   now recorded independently of whether a task runs: stopping is not forgetting.
3. **`Client::timeout` is a deadline for the whole request.** An event-stream was
   cut every `timeoutMs` and reconnected, dropping whatever was sent in the gap.
   A stream now gets `connect_timeout`; a poll keeps the total deadline. Proven
   by reverting the fix and watching `a_stream_outlives_its_timeout` fail with
   `TimedOut` at 500ms.

#### One honesty fix in the renderer

A rejected `panels.json` stops every source while the frontend keeps the last
good config, so the panel header claimed `live` over a value that had stopped
moving. The badge now cross-checks `dataPlane.status()`: if Rust reports no
running source for this panel it says `stopped` and names the likely cause. The
status is re-read on every config event, because Rust reconciles before it
emits.

### Phase 3 — Renderer

- Layout nodes, `data`/`chart`/`table`/`progress`, `when`, expressions.
- Live update without remount; no layout shift; no overflow.

**Evidence:** a value changing on the server updates in place; tab selection
survives a data tick; screenshots in both themes; zero console errors.

### Phase 4 — Plugin panels

- Plugin `panels.json` contribution, namespaced, scoped data channel.

**Evidence:** a plugin ships a panel; disabling removes it; a colliding id is
refused; the plugin cannot read another plugin's source.

### Phase 5 — Bug hunt

Systematic sweep of every panel, menu, slash command and keyboard path, with a
defect ledger: reproduction, root cause, fix, re-verification.

**Evidence:** the ledger in the README, every entry fixed or deferred with a
trigger.

---

## 7. Complexity governance

| Budget | Limit |
|---|---|
| Block kinds | 10 (6 existing + 4) |
| Layout node kinds | 3 |
| Expression operators | 17 |
| Nesting depth | 4 |
| Nodes + blocks per panel | 60 |
| Sources per panel | 8 |
| New dependencies | 0 |
| Motion primitives | 5 (see §12.4) |
| Shape primitives | 6 (see §12.4) |

Zero new dependencies is deliberate. Charts are SVG, tabs are state, expressions
are a small parser, layout is a tree walk, springs are a `linear()` generator.
Each is smaller than the library that would replace it, and each is testable in
isolation. §12.5 records the one case where a library would earn its place
(gesture drag) and why it is not needed yet.

Before any future block kind is added it must answer: *which existing kind plus
binding fails to express this?* If the answer is "none", it is not added.

---

## 8. Retirement

| Retired | Replacement | Phase |
|---|---|---|
| "A block is data" as the module invariant (`panels.rs:1`) | "a block is data or a view over data" | 1 |
| `sandbox` as the only way to show anything dynamic | `data`/`chart`/`table`/`progress`; `sandbox` stays for styled HTML | 3 |
| README's framing of the surface as complete | rewritten with the new surface and its limits | 5 |
| The claim that CSS-only tabs are the ceiling | real `tabs` node | 1 |
| `{{string}}` interpolation (never shipped) | `@path` binding objects | 1 |

---

## 9. Deferred ledger

| Deferred | Trigger to revisit |
|---|---|
| A2UI wire compatibility | a remote agent needs to drive VibeDesk panels |
| JS plugins | a sandboxed worker with a message boundary is designed and reviewed |
| Marketplace | plugin panels ship and are stable |
| Non-loopback sources | a user asks and accepts a per-host consent prompt |
| Drag-and-drop layout | the tree serialises reliably and users ask for it |
| Chart library | SVG primitives prove insufficient for a real case |
| `command` source (JSON stdout) | a user has data with no HTTP endpoint |
| `motion` library (LazyMotion) | gesture-driven drag is actually needed, or `linear()` springs prove insufficient |
| `spring()` CSS easing | it ships in WebView2 and `linear()` becomes the fallback instead of the primary |

---

## 10. Residual risk

- **Loopback is still a capability.** A panel can read any local service the
  user runs. The approval diff and the source list are visibility, not
  prevention.
- **Polling has a cost.** Eight sources at 1s is 8 req/s against a local
  service. The floor is a floor, not politeness.
- **The expression grammar is new surface.** Small, but it parses
  agent-authored input and needs its own tests.
- **Nested layout is new render paths.** Depth 4 is bounded, but the renderer
  must be proven not to remount subtrees on every data tick.
- **The 120s defect may have siblings.** Phase 0 audits every caller.
- **A2UI may move.** We adopt its shape, not its wire format, so a spec change
  costs us nothing.

---

## 11. What "done" means

1. A turn longer than 120s completes; a silent turn is reported as silent.
2. A panel displays a value that changes on a local server, without a reload.
3. Tabs nest to depth 4 and the validator proves the bound.
4. A binding typo fails at approval with a readable message.
5. A plugin ships a panel and disabling the plugin removes it.
6. Every cap in §3.5 has a test that trips it.
7. A v1 `panels.json` still loads.
8. The Phase 5 defect ledger is in the README with evidence per entry.
9. `cargo test --lib` and `tsc --noEmit` clean; installed build shows zero
   console errors.

---

## 12. Motion and shape foundation

### 12.1 The finding that explains the "not smooth" feeling

`globals.css:901` ends with a blanket kill switch:

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
  }
}
```

The WebView2 on this machine reports `prefers-reduced-motion: reduce` as
**true**. So every transition, every `vd-rise`, every `vd-pop`, every hover
colour change is currently running at 0.01ms — that is, not at all. The app is
not "missing" motion; it is being suppressed by a rule that treats reduced
motion as no motion.

That is both an accessibility mistake and the reason the interface feels abrupt.
The correct treatment is **tiered**, not binary:

| Tier | Reduced-motion behaviour |
|---|---|
| Colour, opacity, border | keep — they aid comprehension and do not move |
| Small transforms (≤ 8px, scale ≤ 1.02) | keep, shortened |
| Large movement, parallax, looping, auto-playing | drop |
| View transitions between panels | cross-fade only, no slide |

And because an OS preference is not a statement about this app, VibeDesk gets an
explicit **Motion: System / Full / Reduced** setting. Default `System`. The
user's OS says reduce; the user may still choose Full for this app, and that
choice must win.

### 12.2 What the runtime already supports

Probed against the installed build (WebView2, **Chrome 157**):

| Capability | Available | What it buys |
|---|---|---|
| `corner-shape: squircle` | yes | real squircles, no JS, no SVG |
| `linear()` easing | yes | **spring physics without a library** |
| `spring()` easing | no | not needed, `linear()` covers it |
| View Transitions (`startViewTransition`) | yes | shared-element transitions between panels and chats |
| `animation-timeline: scroll()` / `view()` | yes | reveal-on-scroll with no JS |
| individual `translate`/`rotate`/`scale` | yes | compositor-only animation |
| `interpolate-size: allow-keywords` | yes | animate to `height: auto` |
| `anchor-name` + `showPopover` | yes | tooltips and menus positioned without JS |
| `oklch` + `color-mix` | yes | perceptually even colour interpolation |
| `mask-composite`, `backdrop-filter` | yes | shape and gloss effects |
| `subgrid`, `:has()`, `if()` | yes | layout and conditional styling |

The conclusion is uncomfortable for the premise of the question: **a motion
library would mostly duplicate what the runtime already does.** `motion` is
~15 KB gzipped with `LazyMotion`, on top of a 223 KB main bundle, and it would
fight the existing token system rather than extend it.

### 12.3 What uiverse actually is

uiverse.io is a community gallery of ~7,400 copy-paste HTML/CSS/Tailwind/React
snippets. It is a **source of ideas, not a library**: there is no package, no
versioning, no shared token vocabulary, and every snippet carries its own
colours, radii and timings. Dropping them into a design system that already has
`--vd-dur-*`, `--ease-*` and `--radius-*` would produce exactly the
inconsistency the interface is trying to avoid.

Use it as a reference for *shapes and effects*, and re-express anything worth
keeping in VibeDesk tokens. Do not vendor it.

### 12.4 The foundation to build

A single `src/styles/motion.css` plus `src/lib/motion.ts`, both small, both
token-driven, both reusable by panels and plugins.

**Motion tokens** (extending what exists):

| Token | Purpose |
|---|---|
| `--vd-dur-1..4` | existing, kept |
| `--vd-ease-out`, `--vd-ease-snap`, `--vd-ease-spring` | existing, kept |
| `--vd-spring-soft`, `--vd-spring-snappy` | generated `linear()` curves |
| `--vd-stagger` | per-item delay step |
| `--vd-motion-scale` | 0 / 0.5 / 1, set by the Motion setting |

**Primitives:**

| Primitive | Built on | Size |
|---|---|---|
| `springToLinear(stiffness, damping, mass)` | pure function, build-time | ~40 lines |
| `stagger(index)` | `--vd-i` + `calc()` | CSS only |
| `useTween(value)` | rAF, for live data numbers | ~30 lines |
| `transitionTo(fn)` | `document.startViewTransition` wrapper | ~25 lines |
| `revealOnScroll` | `animation-timeline: view()` | CSS only |

**Shape primitives:**

| Shape | Technique | Fallback |
|---|---|---|
| Squircle | `corner-shape: squircle` | `border-radius` (already present) |
| Blob | 8-value `border-radius` | same, animatable |
| Mesh gradient | layered `radial-gradient` + `oklch` | flat gradient |
| Grain | `feTurbulence` data URI (already used at `globals.css:320`) | none needed |
| Gooey | `feGaussianBlur` + `feColorMatrix` | plain blur |
| Gloss | `backdrop-filter` + inset shadow + `color-mix` | flat surface |

Every one is native. None adds a dependency.

### 12.5 The dependency decision

**Zero new dependencies, still.** The one place a library would genuinely help
is gesture-driven drag, which this app does not need. If that changes, `motion`
with `LazyMotion` + `domAnimation` is the pick, and it should be a deliberate
decision with a measured bundle delta, not a default.

This is recorded in §7's budget and in the deferred ledger.

### 12.6 Phase 6 — Motion and shape

- Tiered reduced-motion, replacing the blanket kill switch.
- Motion setting (System / Full / Reduced) persisted in `settings.json`.
- `motion.css` + `motion.ts` primitives.
- Shape primitives applied to cards, panels, the composer and the rail.
- View transitions for panel open/close and chat switch.
- Value tweening for live panel data.

**Evidence:** with the OS set to reduce, colour and small transforms still
animate and large movement does not; with Motion set to Full, everything does;
a live panel number tweens instead of jumping; no layout shift; zero console
errors; screenshots in both themes.
