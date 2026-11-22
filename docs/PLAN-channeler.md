# Plan: the channeler

Status: built through task 9. Every backend is proven, the `webview` path live and the rest by test.

## What is built

| Task | State | Where |
| --- | --- | --- |
| 1. The core | done, 25 tests | `channel/{backend,sequence,issues,guardrail}.rs` |
| 2. The `webview` backend | done, compiles | `channel/{cdp,webview}.rs` |
| 3. Session wiring | done | `config.rs`, `lib.rs` setup |
| 4. The `process` backend | done, compiles | `channel/process.rs` |
| 5. The `window` backend | tree, input, debug and capture done | `channel/window.rs`, `channel/capture.rs` |
| 6. The hosts | done, proven by test | `channel/host.rs` |
| 7. The `engine` backend | done, 22 tests | `channel/engine.rs` |
| 8. Registration and the panel | done | `capabilities/mod.rs`, `CapabilityPanels.tsx` |
| 9. One CDP client | done, proven neutral | `scripts/cdp-client.mjs` |

What is not built, and is not pretended to be:

- **A picture of a protected or exclusive-fullscreen window.** `window` capture
  renders the window into a buffer this process owns, which is what makes it work
  on a hidden desktop. A window with no redirection surface has nothing to
  render, and the call fails with a message saying so rather than returning a
  black rectangle.
- **Cross-session control.** A channel runs on the user's desktop or on a desktop
  this process creates. Driving a window in another logon session needs a service
  and a token, which is a different problem and is not claimed here.
- **`scripts/cdp.mjs`.** It is a standalone CLI with its own client and no
  importers. It was not deleted: there is no version control in this tree, so a
  deletion is unrecoverable, and the file is a working developer tool. It is the
  one remaining duplicate client and the decision to remove it is the owner's.

Every path has now been exercised against the real thing. `channel/window.rs`
creates a real window on a real hidden desktop, finds it by enumeration, clicks
it, and captures its pixels. `channel/engine.rs` spawns a real child process,
parses its real output, and kills a real overrunning run. `channel-probe` boots a
real Tauri app, loads a real page into a real WebView2, and drives it over the
same JSON-RPC the MCP server speaks.

## The input split, measured

Two things were assumed and turned out to be false. Both are now tests, so they
cannot quietly regress.

**`SendInput` does not reach a non-active desktop.** On a hidden desktop it
either returns zero — the system refusing the input — or reports success and
delivers nothing; the cursor does not move. Hardware input belongs to the active
input desktop and there is no flag that changes that.

**`SetForegroundWindow` is refused for a background process.** The call succeeds
and nothing moves, so a click lands on whatever is on top. Attaching to the
foreground thread's input queue with `AttachThreadInput` lifts the restriction
for the duration, which is the documented mechanism, and `focus` now reports
whether the window actually came forward.

So input is two mechanisms, and the capability table says which one a channel
has:

| Host | Input | Reaches |
| --- | --- | --- |
| `desktop` | `SendInput` | everything, including apps reading raw input |
| `hidden-desktop` | posted window messages | anything that handles window messages |

The trade is real and it is in `Capabilities.input`, not in a footnote. A game
polling DirectInput cannot be driven on a hidden desktop, because it never looks
at a window message. A game that handles `WM_KEYDOWN` can. The agent sees
`input: "hardware"` or `input: "synthetic"` before it plans a sequence, so it
finds this out from the tool rather than from a failed run.

A desktop channel whose window will not come forward now fails the action with a
message saying so, instead of clicking into whatever is in front of it.

## What this is

The agent builds things. It should be able to drive them.

Not just the panels and widgets inside VibeDesk — anything it produces: a native
app, a headless tool, a game in an engine. One capability, one interface, and
the target tells you what it can do.

Today the agent writes something and cannot check it. It reads its own source
and says "this should work", which is a guess in a confident voice. The loop
closes on you: you run it, find the bug, tell it, it guesses again.

After this, the agent builds the thing, drives it, sees the exception it just
caused, fixes it, and only then reports.

## Why it is shaped this way

| Hop | Cost |
| --- | --- |
| One CDP call | 1–5 ms |
| One screenshot, clipped, JPEG | 20–50 ms |
| **One MCP call = one agent turn** | **1–3 s** |

A hundred actions issued as a hundred tool calls costs 100–300 seconds, almost
all of it the model thinking between calls. As one call it costs about a second
of transport plus one turn.

That is the whole design. The tools take **sequences**, not single actions.

## Two axes, not one

The reason this kept collapsing into "web content only" is that I was treating
*what you drive* and *where you drive it* as the same question. They are not.

```
backend  = what you drive      webview | window | process | engine
host     = where you drive it  desktop | hidden-desktop | session
```

"Full control without stealing the machine" is a **host** property. "Can I see
inside it" is a **backend** property. Every combination is legal, and the pair
is what `channel_open` resolves.

## Backends

### `webview`

Chromium content: VibeDesk's own panels and widgets, any Electron or CEF app,
any page.

- **Observe:** screenshot plus the accessibility tree, through CDP.
- **Input:** the CDP `Input` domain — real renderer-level events.
- **Issues:** `Runtime.consoleAPICalled`, `Runtime.exceptionThrown`,
  `Log.entryAdded`, `Network.loadingFailed`, `Network.responseReceived` ≥400,
  `Page.javascriptDialogOpening`.
- **Background:** yes, by nature. Input is dispatched at the renderer, so the OS
  cursor does not move and the window need not be foreground.
- **Implementation:** in-process CDP. `Webview::with_webview` gives
  `PlatformWebview`, whose `.controller()` is `ICoreWebView2Controller`;
  `CallDevToolsProtocolMethod` is Microsoft's documented Win32 API for CDP and
  is a runtime API, not a debug feature. No port, no listener, works in release.
  `webview2-com 0.39.1` is already in `Cargo.lock` via `wry 0.57.2`.

### `window`

Any native window, by handle or by launching an executable.

- **Observe:** Windows Graphics Capture, which captures occluded, minimized and
  DirectX/OpenGL windows. `PrintWindow` returns black for game windows and DXGI
  duplication only sees visible ones, so WGC is the only method that works for
  the hard cases.
- **Structure:** the UI Automation tree — control names, roles, and patterns.
  Available for Win32, WPF, WinUI and Qt with accessibility enabled. A
  custom-drawn surface has no tree, and then observation is pixels only.
- **Input:** UIA patterns (`Invoke`, `Value`, `Toggle`) where the control
  exposes them, which needs no foreground. `SendInput` for everything else,
  which does.
- **Issues:** `OutputDebugString` captured from the shared pipe, the process's
  stderr, and its exit code.
- **Background:** observation always; input only when hosted.

### `process`

A headless binary: a CLI tool, a test runner, a headless engine.

- **Observe:** stdout and stderr.
- **Input:** stdin.
- **Issues:** stderr, non-zero exit, and a timeout.
- **Background:** yes, trivially — there is no window.

### `engine`

A project that provides its own test hook. This is the strongest backend where
it exists, because the input is simulated inside the engine's own loop rather
than injected from outside.

It is **generic on purpose**. There is no Unity, Godot or Unreal branch, because
a backend that only works for one engine is a backend most projects cannot use,
and the thing every project with a test hook has in common is a command that
runs the suite and prints a result. So the backend takes that command and a
declared output format, and the depth is in the parsers rather than in the
engine list.

- **Observe:** the suite's own result, parsed into one outcome per test.
- **Input:** the run itself. `run` re-runs the suite, optionally filtered to one
  test by name. There is no pointer and no key, so `click`, `hold`, `drag`,
  `scroll` and `type` are absent from its capabilities rather than silently
  doing nothing.
- **Issues:** every failing test, with its own message, plus a non-zero exit
  that no test claimed.
- **Background:** yes — headless, and it never touches the user's desktop.

Four formats, each a real parser with its own tests:

| Format | Reads | Used by |
| --- | --- | --- |
| `exit` | the exit code only | anything, as the universal fallback |
| `tap` | `ok 1 - name` / `not ok 2 - name` | `node --test`, `prove`, `bats`, most TAP emitters |
| `cargo` | `test name ... ok` / `... FAILED`, `test result:` | `cargo test`, and anything printing the same shape |
| `pytest` | `PASSED` / `FAILED` / `ERROR` lines, the summary | `pytest -v`, `pytest -q` |

A format that does not match the output is not a silent pass: the run reports
the exit code and says the format found no tests, so a wrong `format` reads as
"nothing parsed" rather than "everything is fine".

## Hosts

A desktop is a thread property on Windows. `SetThreadDesktop` binds the calling
thread, and from then on every window it creates, every `EnumWindows` it makes
and every message it posts belong to that desktop. The binding cannot be undone,
so the thread that takes it is spent — which is why a hidden desktop needs a
thread of its own rather than a flag on an existing one.

| Host | What it buys | What it costs |
| --- | --- | --- |
| `desktop` | Nothing to set up; real hardware input | The window has to come forward, so the user's screen is taken while it acts |
| `hidden-desktop` | `CreateDesktop` + `SetThreadDesktop`; the user's desktop, cursor and focus are untouched | Input is posted messages, so an app reading raw input cannot be driven |

The host thread pumps. Injected input is queued, not delivered — the window
procedure only runs when the owning thread pumps — so the loop pumps between
jobs. A desktop thread that never pumps is a thread whose windows never see a
click.

`session` is not a host. Driving a window in another logon session needs a
service and a token, which is a different problem; offering the name without the
mechanism would be a surface that implies a capability it does not have.

The host is chosen by the target's needs: a `webview` or `process` target never
needs one, and a `window` target needs `hidden-desktop` whenever the user is
meant to keep working.

## The six tools

### `channel_open`

```
{ target: { app } | { exe } | { dir } | { url } | { hwnd }, host? }
→ { id, backend, host, capabilities }
```

Resolves the backend and host, and returns what this target can actually do:

```
capabilities: {
  observe: "tree+image" | "image" | "text",
  input:   "full" | "patterns" | "stdin",
  background: true | false,
  issues:  ["console", "network", "debug-output", "stderr", "exit"]
}
```

That object is the point. The agent never guesses what a target supports — it
reads it.

### `channel_look`

```
{ id?, region?, image?: bool, frames?: number, issues?: "new" | "all" | "none" }
→ { digest, refs, issues, image?, frames?, viewport }
```

A text digest — interactive refs with roles, names and boxes — plus new issues.
`image: true` adds a JPEG clipped to the scope. `frames: n` adds a capture
window for watching motion.

Text by default because a tree read costs fewer tokens than a screenshot and
gives refs the agent can act on immediately. Images are for layout and
rendering, not for "what is here".

### `channel_act`

```
{ id?, actions: [ ... ], evidence?: "none" | "digest" | "image" }
→ { results: [{ ok, at?, error? }], issues, digest?, image? }
```

```
{ click:  { ref } | { x, y }, button?, count?, modifiers? }
{ hold:   { key } | { button, at }, frames? | ms? }
{ drag:   { from, to }, steps?, button? }
{ type:   { text }, target? }
{ scroll: { at, dx?, dy } }
{ tick:   { frames } }
{ wait:   { for, timeout_ms? } }
{ read:   { expression } }
```

Linear and declarative. No branching, no loops, no variables — if it grew those
it would be a shell script in JSON, which is the thing this replaces. An agent
that needs to branch makes another call.

Per-action results make failure attributable: the result names which action
failed and why, and the rest are marked skipped rather than silently attempted.

`hold` is the primitive that makes a game testable. `keyDown` with `autoRepeat`,
then `keyUp` after the duration — not a tap. Duration is expressed in frames as
well as milliseconds, because a game reads input inside its own loop and a
wall-clock hold races it.

### `channel_expect`

```
{ id?, that, equals?, timeout_ms?, allow_issues?: [pattern] }
→ { pass, observed, evidence: { image, at }, issues }
```

**Fails on new issues even when the assertion passes**, unless `allow_issues`
names them. A test that goes green while the console fills with exceptions is a
false pass, and this is the one place the capability refuses to produce one.

### `channel_issues`

```
{ id?, since?: "last" | "all", level?: "error" | "warn" | "all" }
→ { issues: [{ level, source, text, at, count }] }
```

Answers "what broke while I was not looking" without acting.

### `channel_close`

```
{ id }
→ { ok }
```

## The issue channel

One buffer, fed by whatever the backend can see, in one shape:
`{ level, source, text, at, count }`.

| Backend | Sources |
| --- | --- |
| `webview` | console, exceptions, browser log, failed requests, dialogs |
| `window` | `OutputDebugString`, stderr, exit code |
| `process` | stderr, exit code, timeout |
| `engine` | the engine's log, assertion failures |

Deduplicated by source and text with a count, capped as a ring. Every tool
result carries only what is **new since the last call**, capped at ten with a
remainder count. The agent cannot take an action and fail to learn that it threw.

The webview pattern is already proven here — `live-check.mjs:170-200` has
`watchConsole` doing exactly this. It is not new; it is unowned and unreachable
from the agent.

## The guardrail

The agent already runs with your permissions and can run shell commands. It can
already do anything. So this is not a security boundary and pretending otherwise
would be theatre. It is a guardrail against **accidents**: a test that clicks
Approve corrupts its own run, and a test that clicks Delete destroys state the
next test needs.

Two checks, both deterministic, both before dispatch:

1. **Scope.** The target's own surface. For `webview`, the content rect derived
   from the layout config the frontend already carries (`railWidth`,
   `sidebarWidth`); title bar, rail, approval bar, elicitation bar and settings
   are outside it. For `window`, the window rect. For `process` and `engine`,
   the process. Every input event is checked: coordinates as points, key and
   text events by the bounding box of the focused element, because a key event
   goes to whatever holds focus. Outside is refused and reported, never clamped
   — clamping would silently click the nearest legal pixel, a different action
   than the one asked for.

2. **Destructive patterns.** A sequence that activates a control labelled
   delete / remove / approve / install / pay and then confirms is refused. Labels
   come from the accessibility tree where there is one, and from the window's
   UIA tree otherwise. Label matching is crude on purpose: a model judge would
   be slower, non-deterministic and talkable-out-of. For a guardrail, predictable
   and crude beats clever.

It fails closed. An action it cannot evaluate is blocked, and the error names
the rule. Every decision — allowed or blocked — is appended to `channel.jsonl`
beside the swarm's `permissions.jsonl`, so the user can audit what was driven on
their behalf.

`read` is not restricted. It evaluates in the target's context, so no scope check
can confine it — and the agent already has more power than `read` gives it.

## Where it lives in the capability layer

The channeler is a native VibeDesk capability, not an external one. The layer's
own definition is that a capability *"attaches at a boundary that already belongs
to this app, so the capability is not a description of something the agent might
do — it is code on the path the data actually travels."* The app owns the
webview, so it sees the input before the renderer does.

| | |
| --- | --- |
| Boundary | `Boundary::McpServer`, session-scoped |
| Registration | `mcp_servers_for_session`, beside `swarm-mcp` |
| Registry | `CapabilityId::Channeler`, `record()` per call, `bump()` for counters |
| Panel | its own — the registry enforces one per capability in a test |

Two things distinguish it from the six capabilities already in the ledger, and
both are worth stating rather than discovering later.

**It is not a port.** The six are upstream projects VibeDesk adopted —
tokenjuice, SparseReading, billion-context, dream-memory, codebase-memory-mcp,
donsetch. The channeler is new code the app owns outright. The ledger's framing
of "six upstream projects become capabilities" becomes seven capabilities, six
of them ports.

**It spends tokens rather than saving them.** The ledger is shaped around
compaction savings — `bytes_in`, `bytes_out`, `tokens_in`, `tokens_out` — because
every existing capability exists to make the context smaller. The channeler
exists to make the agent *correct*, and it costs context to do it. Its panel
therefore shows actions run, issues caught and tests passed, not bytes saved.
That is a real difference in kind, not a formatting one, and the panel should say
so rather than display a saving of zero.

## How it relates to the swarm

The swarm was upgraded while this plan was being written. Three of its new
modules bear on the channeler. None changes the design; two confirm it and one
constrains where it runs.

**`prove.rs` is the same principle pointed at a command.** It runs a test twice —
once in the base checkout, once in the change — so `failing_first` is an
observation rather than a phrase the verifier used. The channeler is that idea
pointed at a surface: don't read the claim, look at the thing. The natural
extension is a `failing_first` for UI — capture before and after a change and
assert the difference — and that is a next step, not part of this plan.

**`permissions.rs` already establishes the audit pattern.** It answers a
member's permission request from the role and the declared effects, and records
the decision in the run's `permissions.jsonl` so the user can audit what was
allowed on their behalf. The channeler's guardrail does the same job for UI
actions and records to `channel.jsonl` beside it.

**Members get capabilities, not the control surface.** `engine.rs:1647` is
explicit: a member session gets `mcp_servers_for_acp`, not
`mcp_servers_for_session`, because *"a member that can start a swarm is a member
that can recurse. Capabilities yes, control surface no."*

The channeler is a capability — the guardrail confines it to content, so it
cannot start a swarm or approve anything. But it is also a **shared resource**:
one webview, one driver. A member works in an isolated worktree, so the app's UI
is not its work. The split is therefore by backend, not by capability:

| Backend | Main session | Swarm member |
| --- | --- | --- |
| `webview` | yes | no — one webview, one driver |
| `process` | yes | yes — isolated per worktree |
| `engine` | yes | yes — isolated per worktree |
| `window` | yes | no — shared desktop |

That is not a restriction on members. It is the correct answer to "whose UI is
this": a member's work is in its worktree, and the app's UI belongs to the
session the user is looking at.

## Build order

Each step ends with something that works, and the core is backend-agnostic so
the later backends do not reopen it.

1. **Core.** The backend trait, the sequence engine, the issue buffer, the
   guardrail, the MCP server. No backend yet.
2. **`webview`.** In-process CDP. Proves the core and ships the capability
   inside VibeDesk.
3. **`process`.** The cheapest backend, and it covers every CLI tool and
   headless runner the agent produces.
4. **`window` + `desktop`.** WGC, UIA, `SendInput`. Native apps, foreground.
5. **`hidden-desktop`.** The same backend, hosted so the user's desktop is
   untouched.
6. **`session`.** Full background control for targets that need real input.
7. **`engine`.** Unity and Wicked hooks.

## Tasks

### 1. The core

`src-tauri/src/channel/` — `backend.rs` (the trait: `look`, `act`, `issues`,
`capabilities`), `sequence.rs` (eight action kinds, per-action results,
skip-on-failure), `issues.rs` (buffer, dedup, ring, new-since-last),
`guardrail.rs` (scope, destructive patterns, fail-closed), `mcp.rs` (JSON-RPC
over HTTP, bearer auth, per-session token).

**Proof:** a fake backend drives every tool end to end; `tools/list` returns
six; an unauthenticated call is refused; a failing third action marks the fourth
skipped.

### 2. The `webview` backend

`src-tauri/src/channel/webview.rs`. Async wrapper over
`CallDevToolsProtocolMethod`: callback → `oneshot`, serialized, timeout, error
mapping. The six event subscriptions. Digest, refs, clipped JPEG, capture
window, `tick`, `wait`.

**Proof:** a live call returns the page title; the digest lists refs;
`image: true` returns a clipped JPEG; `frames: 3` returns three increasing
timestamps; `tick` advances a counter by exactly `n`; a deliberate
`console.error` appears in the buffer.

### 3. Session wiring

`config.rs`: inject the channeler into `mcp_servers_for_session` with its port
and token. `tauri = "2"` becomes `tauri = "2.12"`, because `with_webview`
exposes platform types that can move in a minor release.

**Proof:** a session's server list contains it; the agent calls `channel_open`
and gets capabilities back.

### 4. The `process` backend

`src-tauri/src/channel/process.rs`. Spawn, stdin, stdout, stderr, exit code,
timeout.

**Proof:** a CLI tool that echoes stdin and exits non-zero produces the right
stdout, the right exit code, and an issue.

### 5. The `window` backend

`src-tauri/src/channel/window.rs`. WGC capture, UIA tree and patterns,
`SendInput`, `OutputDebugString` capture.

**Proof:** a native app is captured while occluded; a button is invoked through
UIA without focus; a `SendInput` click lands when foreground.

### 6. The hosts

`src-tauri/src/channel/host.rs`. `CreateDesktop` + `SetThreadDesktop` on a
dedicated thread, which then pumps between jobs. `Host::run` dispatches a job to
that thread and waits; on the user's desktop it is a plain call.

**Proof, and it runs:** `channel/window.rs` creates a real window on a real
hidden desktop, finds it by enumeration, posts a click, and asserts the window
procedure ran. Three more tests hold the edges: the window is invisible to the
user's desktop, hardware input does not reach it, and a desktop window comes to
the front when asked.

### 7. The `engine` backend

`src-tauri/src/channel/engine.rs`. A declared test command and a declared output
format, parsed into one outcome per test. Generic by design: no engine-specific
branch, because the common denominator of every project with a test hook is a
command that runs the suite and prints a result.

**Proof:** a real child process runs a real suite, the parser reports the
passing and failing tests by name, a failing test becomes an error-level issue,
and `run` re-runs it. The parsers are unit-tested against captured output from
each of the four formats, including the shapes that are easy to get wrong: a
test name containing `...`, a `not ok` with a `# TODO` directive, a cargo
`ignored` line, and a pytest summary with no tests collected.

### 8. Capability registration and the panel

`CapabilityId::Channeler`, a `panel()` mapping, `record()` per tool call and
`bump()` for the counters. The panel renders the issue buffer and the run
counters.

This is not a trailing nicety. The registry enforces *"every capability has a
panel, and no two share one"* in a test, so a capability without a surface fails
the build. The channeler is a capability, so it gets a panel.

**Proof:** the registry test passes; the panel shows a run's actions and issues;
a deliberate error appears and clears.

### 9. Retire the duplicate clients

Three independent `class Cdp` implementations — `cdp.mjs:44`,
`live-check.mjs:47`, `ui-probe.mjs:61` — none importing the others. 6,477 lines
of scripts holding three copies of the same client.

**What was done.** `live-check.mjs` and `ui-probe.mjs` carried the *same* client,
and the copies had drifted: one guarded a call made after the socket closed and
the other did not, so the same failure hung one harness and failed fast in the
other. Both now import `scripts/cdp-client.mjs`, which is the merged client with
the guard. 242 lines of duplication removed.

**What the plan got wrong.** It said to migrate the two consumers onto the
channeler's HTTP API. They cannot be: the bearer token is derived from the app's
process state and never written anywhere, so a Node script has no way to obtain
it. That is deliberate — the token exists to stop a different local client
reaching the server by accident — and making it discoverable is a security
decision, not a refactor. The harnesses keep the debug-port route.

**Proof:** the migration is neutral. `npm run live` scores 47/60 with the shared
client and 47/60 with the original inline client restored, failing the same
thirteen checks. Those failures are not the scripts: the app's view crashes with
React error #185 (a render loop) and recovers on reload, and the ACP agent is not
installed in this environment. Both are recorded below as separate findings.

## The skills

Four skills in `.vibe/skills/`, one per decision phase, so the agent loads the
one it needs rather than a wall of text:

| Skill | Owns |
| --- | --- |
| `channeler` | The loop, the six tools, and the honest limits. The entry point. |
| `channeler-open` | Backend, host and capture. The `foreground` and `input` fields and what they forbid. |
| `channeler-drive` | Writing a sequence: batching, refs, holds, waits, the guardrail. |
| `channeler-verify` | Turning a run into a verdict, and mapping an issue to a cause. |

## What battle-testing the skills found

The skills make factual claims, so each one was asserted against the real tool
surface rather than trusted. Four claims were wrong, and three of them were
wrong about the code, not about the prose.

**`window` implemented a `tick` it did not advertise.** `capabilities.actions`
omitted `Tick`, so `sequence::run` refused it before dispatch — the arm in
`dispatch` was unreachable. It slept for sixteen milliseconds a frame and called
that advancing the simulation. Removed: a window has no frame clock to observe,
and a `tick` that sleeps is a sleep wearing the word "tick". `wait` is the
honest verb, and the refusal now says so.

**`blocked` is not where a scope refusal lands.** `blocked` is the destructive
pre-check, which refuses the whole sequence before any step runs. A point outside
the stage is a failed *step*: it appears in `results` with `stopped_at` naming
its index. The skill said to read `blocked`; it now says which of the two to
read, and the test asserts `blocked` is null in the scope case.

**A sequence drains the issues its own steps produced.** They come back in the
reply, so a `channel_expect` afterwards sees nothing — the issue is no longer
new. A test that waited through `channel_act` and then asserted passed a run
that had exited non-zero. The skill now says to read the `issues` in the
`channel_act` reply, or to wait outside the sequence.

**Hardware input and UI Automation are global resources.** Two tests clicking at
screen coordinates fight over whatever is on top, and UIA returns `E_FAIL` under
the contention. The desktop tests now take a mutex, which mirrors what the app
already does: the channel registry serialises every call through one lock.

The capability shape the skills show is pinned by a test, so a renamed field
cannot make them lie silently.

## How we know it works

What runs today, in `cargo test --lib`, against real Windows objects rather than
mocks:

| Test | What it proves |
| --- | --- |
| `a_window_on_a_hidden_desktop_is_found_and_clicked` | A real window on a real hidden desktop is created, found by enumeration, clicked, and its window procedure runs |
| `a_window_on_a_hidden_desktop_is_not_visible_to_the_users_desktop` | The same search from the user's desktop finds nothing — the test is out of the way, not merely unfocused |
| `hardware_input_does_not_reach_a_hidden_desktop` | The measured limit, held so nobody simplifies the message path back into `SendInput` |
| `a_window_on_the_desktop_comes_to_the_front_when_asked` | `AttachThreadInput` lifts the foreground restriction, and `focus` reports the truth |
| `the_tool_surface_drives_a_window_end_to_end` | `channel_open` → `channel_act` → `channel_expect` → `channel_close` over the same JSON-RPC the MCP server speaks, with the click landing |
| `a_hidden_desktop_can_enumerate_its_own_windows` | A fresh desktop starts empty, so the thread really is somewhere else |
| `a_real_child_process_is_run_and_parsed` | `engine` spawns a real command and turns its real output into outcomes |
| `a_run_that_overruns_is_killed_and_reported` | A run past `timeout_ms` is killed with its descendants and reported as `timed_out` |
| `the_tool_surface_runs_a_real_suite_end_to_end` | `node --test` driven through open, look, act, expect, close |
| `a_window_channel_captures_the_colour_the_window_paints` | A real window's own pixels come back as a PNG, checked against the colour it paints |
| `a_window_on_a_hidden_desktop_is_captured_with_its_own_colour` | The same, on a desktop nobody is looking at |

1461 tests pass, with no warnings.

Proven outside the test binary, because it needs a real WebView2 and an event
loop on the main thread:

| Probe | What it proves |
| --- | --- |
| `cargo run --bin channel-probe` | A real Tauri app, a real page in a real WebView2: background and synthetic input, the button in the tree, a console error failing the assertion, a label refused with the form it wanted, a click by ref landing, a JPEG capture, and a clean close |

Still to prove, and not claimed:

- **`engine` against a real project's suite.** The parsers are proven against
  real `cargo test` and `pytest` output and the process control against a real
  child, but no project's own suite has been driven through the tool surface end
  to end.

The acceptance case that matters most is still ahead: a canvas game in a widget,
an arrow key held for 40 frames, asserted on pixels, with a console error
correctly failing the run. That is the `webview` path, and it is the one that
needs the app running.

## Found while proving this, and not fixed here

Two things surfaced while running the live harness. Neither is the channeler, and
neither was introduced by this work — no frontend file was touched in the session
that found them, and the built bundle predates the changes.

**The view crashes with React error #185.** "Maximum update depth exceeded" — a
render loop. It happens on a cold start and again mid-run under the live harness,
and the error boundary recovers it: the page shows "This view stopped rendering"
with a Reload button, and after the reload the whole UI is present and correct.
It is intermittent, which is why it has not been reported before. Fixing it needs
a non-minified stack, which needs a sourcemap build; guessing at the component
would be a workaround, so it is recorded rather than patched.

**The ACP agent is not installed in this environment.** The app reports "The
agent could not start — agent request failed (-32602): Invalid params. Install the
CLI with `uv tool install mistral-vibe`". Every live check that needs a running
agent fails for that reason, which is most of the thirteen.
