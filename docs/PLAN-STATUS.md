# Plan status: dynamic customization, live data, nested layout

What is built against `docs/PLAN-dynamic-customization.md`, phase by phase, with
the evidence for each claim. Written 2026-11-16, last revised 2026-11-17.

This file is the record; the plan is the specification. Every phase below is
implemented and verified. Where a phase has a limit — a platform that has never
run, a capability that is closed on purpose — the limit is stated as a property
of what shipped, not as work left over.

## Where the tree stands

| Check                             | Result                                            |
| --------------------------------- | ------------------------------------------------- |
| `cargo test --lib`                | 667 passed, 0 failed                              |
| `npm run test:unit`               | 48 passed, 0 failed                               |
| `npx tsc --noEmit`                | clean                                             |
| `npx vite build`                  | clean                                             |
| `node scripts/live-check.mjs all` | 230/230 checks across 14 suites, 0 console errors |

`npm run verify` runs all five in order. The live step needs a built app, a
display and a free port, so it is a command rather than a CI job; everything
before it is a plain command that either passes or fails.

At the start of this work the same tree had 288 tests, 0 failures, and **9
warnings** — the plan claimed zero. Those nine were the consent ledger and two
unregistered commands, staged for the deferred non-loopback feature. They are
gone; see the README defect ledger, entry 1.

The consent ledger is no longer staged. `data_consent_list` and
`data_consent_set` are registered, the panel shows the origin it wants to read,
and the `data` suite drives the whole path against a real loopback server. That
change is what removed the last `#[allow(dead_code)]` in `consent.rs`.

## Phase summary

| Phase                           | Status                               | Tests added |
| ------------------------------- | ------------------------------------ | ----------- |
| 0 — Turn lifecycle              | Done before this work                | —           |
| 1 — Model v2                    | Done before this work, extended here | +6          |
| 2 — Data plane                  | Done before this work                | —           |
| 3 — Renderer                    | **Done here**                        | +6          |
| 4 — Plugin panels               | **Done here**                        | +14         |
| 5 — Bug hunt                    | **Done here**                        | —           |
| 6 — Motion and shape            | **Done here**                        | +6          |
| 7 — Widget boundary             | **Done here**                        | +17         |
| 8 — Rail as a directory         | **Done here**                        | —           |
| 9 — Closing the seven gaps      | **Done here**                        | +17         |
| 10 — Motion removed and rebuilt | **Done here**                        | −6, +2      |

The Rust suite is 667 tests. The frontend has 48 unit tests over the pure
modules (`data.ts`, `motion.ts`, `sanitize-svg.ts`) and 230 live checks over the
running app. The unit tests exist because the live suite cannot say what
`decimate` does to a spike or what shape a generated spring has; the live suite
exists because the unit tests cannot say whether a panel renders.

---

## Phase 0 — Turn lifecycle

**Done before this work.** Not touched. The idle watchdog, the three signals
that make silence legitimate, `TurnStalled`, `TurnInProgress` and the held-prompt
queue are all in `src-tauri/src/acp/bridge.rs` as the plan describes.

**Not re-verified here.** The plan's evidence for this phase is a 154s turn, a
257s turn with a silent shell command, and a stop mid-turn. Those need a running
agent and a real model; this environment has neither. The unit tests that pin
`await_turn` still pass.

## Phase 1 — Model v2

**Done before this work, extended here.** The expression language, the four
bound block kinds, the layout tree, the v1 migration and the published catalog
were already in place.

What this work added:

- **`when` on layout nodes.** Plan §3.3 says `when` is a field on every node and
  block; it was on blocks only. `LayoutNode::Tabs`, `::Split` and `::Stack` now
  each carry an optional `when`, serialised flat beside the `node` tag and
  omitted when absent, so existing JSON is byte-identical. `check_node` validates
  it once, before the per-variant match, with the same `check_condition` the
  block path uses.
- **The manifest says so.** `layout_node_fields` and `layout_rule` are published,
  and the example the agent is told to copy now wraps a chart and a table in a
  `stack` guarded by `{"@exists": "/ci/runs"}`. The example is still a test.
- **Six tests**: a node `when` round-trips; a `when` on a tabs node is accepted;
  a node `when` that is not a condition is refused with a message naming the
  node position; a node without `when` serialises without the key; a hidden
  node still contributes its blocks to the derived `blocks` view; the manifest
  says a layout node can be guarded.

**Decision worth keeping:** the flattening stays structural. A hidden node's
blocks still appear in `blocks`, because `blocks` is the structural view and
`when` is a visibility rule. Making the flattening conditional would mean the
derived view changed shape as data arrived.

## Phase 2 — Data plane

**Done before this work.** Not touched beyond one line: `CustomPanel` gained a
`plugin` field, which forced `plugin: None` into the `panel()` test helper in
`data.rs` and `battletests.rs`. No behaviour changed.

The split the plan calls the security boundary still holds: built-in sources
resolve in the frontend, external sources fetch in Rust, and the webview CSP is
still `connect-src 'self' ipc:`.

## Phase 3 — Renderer

**Done.** This was the headline gap: the Rust side validated and stored a
recursive layout tree and the frontend rendered `panel.blocks`, the flattened
view, so nested tabs and splits came out as a flat list.

What shipped:

| Piece              | Where                                                                                                                                                   |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Recursive renderer | `src/components/panels/LayoutView.tsx` (new, 232 lines)                                                                                                 |
| `stack`            | vertical flex, children in order                                                                                                                        |
| `split`            | horizontal flex, `flex: 0 1 <weight/total>%`, equal when weights absent, no drag handles                                                                |
| `tabs`             | `role="tablist"/"tab"/"tabpanel"`, `aria-selected`, `aria-controls`, `aria-labelledby`, roving tabindex, ArrowLeft/ArrowRight/Home/End with wrap-around |
| Guarded nodes      | `visible(node.when, model)` hides the whole subtree                                                                                                     |
| Unknown node tag   | names it and carries on, matching `BlockView`'s default case                                                                                            |
| Wiring             | `CustomPanels.tsx` renders `LayoutView` when `panel.layout` is present, flat fallback when null                                                         |

**The type bug this phase had to fix first.** `LayoutNode.children` was typed
`LayoutNode[]`, but Rust's children are `Vec<Node>` — a child may be a block.
`Tab.children` had the same error, and `CustomPanel.layout` was typed
`LayoutNode` when Rust's is a `Node`. A panel whose whole body is one block is a
legal file. All three are now `PanelNode = LayoutNode | Block`. This is the same
class of mismatch that blanked the window in Phase 1, caught by reading the Rust
type rather than by a test.

**Live updates do not remount.** Every key is an index path (`keyOf(path)`),
never a value. Tab selection is one `Record<string, number>` in `LayoutView`,
keyed by the tabs node's position, so a data tick cannot reset it and a node
hidden by its `when` and shown again keeps its tab.

**Live verification: done.** See [Live verification](#live-verification) for how.
Against a running app with a probe panel, all four claims held: nested tabs
rendered inside a tab, a `when` that evaluates false hid its block while its
sibling stayed, `weights: [2, 1]` produced 66.67% / 33.33%, and the selected tab
survived a data tick. ArrowRight moved the selection and focus with it, End
jumped to the last tab, and Escape closed the panel.

**Judged by a human:** whether a theme looks right. The harness can take a
screenshot (`shot`), but the judgement is not a check.

## Phase 4 — Plugin panels

**Done.** A plugin ships a panel; disabling the plugin removes it; a colliding
id is refused; a plugin cannot read another plugin's source.

What shipped:

| Piece                             | Where                                                                                                                                              |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Discovery, namespacing, caps      | `src-tauri/src/plugin_panels.rs` (new, 472 lines)                                                                                                  |
| `plugin` field, reserved-id rules | `panels.rs` — `validate` refuses a user panel that sets `plugin` or whose id starts with `plugin:`                                                 |
| Merge on read, filter on write    | `commands/customize.rs` — `panels_get` is now async and returns user panels then plugin panels; `panels_set` drops plugin panels before writing    |
| Watcher and boot                  | `lib.rs` — the three places that build a panel list merge, so a config edit or a launch does not drop plugin panels or leave their sources stopped |
| Rail marker                       | a 3px dot and a `— from the <plugin> plugin` title suffix                                                                                          |
| Editor                            | `CustomizePanel` excludes plugin panels from the editable list and names them read-only                                                            |

**The trap this phase had to avoid.** `CustomizePanel.tsx` saves the whole panel
list back on every edit. If `panels_get` returned merged panels and `panels_set`
wrote them, the first time a user added a panel every plugin panel would be
persisted into their file — and then refused by `validate` on the next load,
because a user panel may not carry `plugin`. The filter on write is load-bearing,
not tidiness, and there is a test that round-trips a save and reads the file back.

**Isolation.** Panel ids and source ids are both namespaced
`plugin:<plugin>/<id>`, applied by `scan` and never trusted from the file. Two
plugins that both declare a source called `ci` get distinct ids. A plugin name
containing `/` or `\` is refused outright, because the id is the isolation
boundary and an id that needs quoting is not one.

**One deviation worth recording.** A namespaced source id contains `/`, and a
binding path is read one segment at a time, so `usePanelModel` also exposes the
un-namespaced tail of a namespaced id. That is safe because `data::check_source`
restricts a source id to lowercase letters, digits and dashes — a source id can
never contain a slash, so the tail always equals the id the plugin declared —
and because the model only ever contains that panel's own sources.

**Live verification: done.** A real plugin folder in `~/.vibe/plugins` with a
`vibedesk/panels.json` contributed a panel marked `— from the probe-plugin
plugin`; disabling the plugin removed it from the rail. That second half failed
on the first attempt and is now defect 24 in the README ledger.

**Covered by unit tests rather than a live run:** a plugin that ships a
_source_ rather than a built-in, and the `panels_plugin_issues` surface for a
plugin whose file is malformed. Both are exercised over temp directories in Rust;
neither needs a running app to be true.

## Phase 5 — Bug hunt

**Done.** 23 defects fixed. The ledger is in the README under
`## Defect ledger`, one entry per defect with reproduction, root cause, fix and
re-verification. The last four were the deferred ones — the context menus, the
resize handle, the SVG sanitiser and the uncapped series — and they are fixed
rather than deferred now that the dependency budget allows a sanitiser written
from what is already there.

The ones worth knowing about:

- **Nine dead-code warnings** — the consent ledger is staged for the deferred
  non-loopback feature. Registering the commands would have put a consent switch
  in the UI for a capability that does not exist, so the module is
  `#![allow(dead_code)]` with the trigger named. `check_loopback_url` is
  untouched and still refuses every non-loopback host.
- **A circular import** between `CustomPanels` and `LayoutView` that worked only
  because `BlockView` was a hoisted function declaration. `BlockView` and its
  helpers moved to their own module; the import graph is acyclic.
- **Resuming a chat from the Sessions drawer** appended to the transcript and
  re-sent the preamble. It now goes through `openChat`.
- **The approval bar could render with no buttons at all**, leaving the agent
  stuck forever.
- **Escape did not close a panel.**
- **Dragging a layout slider wrote `layout.json` on every input event**, out of
  order. It now commits once on release, key-up or blur.

**How the frontend fixes are verified:** `tsc`, `vite build`, the live suite and
`tests/`. The unit runner arrived later — Node 24 strips the types, so it needed
no dependency — and it covers the pure modules the live suite cannot reach into.
The ledger says which of the four applies to each entry rather than implying
coverage that does not exist.

## Phase 6 — Motion and shape

**Done.** The defect the plan identified was real: `globals.css` ended with a
blanket rule forcing every animation and transition to 0.01ms under
`prefers-reduced-motion: reduce`, and the WebView2 on this machine reports
`reduce` as true. The app had no motion at all.

What shipped:

| Piece                         | Where                                                                                                                                                               |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Tiered reduced motion         | `src/styles/motion.css` (new) — colour/opacity/border kept, small transforms kept and shortened, loops and large movement dropped, view transitions cross-fade only |
| Scale token                   | `--vd-motion-scale` multiplies every `--vd-dur-*` in `globals.css`                                                                                                  |
| Motion setting                | `Settings.motion` (`system`/`full`/`reduced`, default system), `motion_get`/`motion_set`, bridge entry, control in `ConfigPanel`                                    |
| Applied at boot and on change | `main.tsx` resolves the OS preference before first paint; `App.tsx` applies the stored choice, which wins over the OS                                               |
| Primitives                    | `src/lib/motion.ts` — `springToLinear`, `stagger`, `useTween`, `transitionTo`, `applyMotion`, `motionScale`                                                         |
| Spring tokens                 | `--vd-spring-soft` and `--vd-spring-snappy` as generated `linear()` curves, pasted in so the stylesheet needs no build step                                         |
| Shape primitives              | squircle, blob, mesh gradient, gooey, gloss — each with a fallback, grain reused from the existing rule                                                             |
| View transitions              | panel open/close via `transitionTo` in `App.tsx`                                                                                                                    |
| Value tweening                | `useTween` wired into `DataBlock`, so a live number moves instead of jumping                                                                                        |

**Zero new dependencies**, as §7 and §12.5 require. The runtime already had
`linear()`, view transitions, scroll-driven timelines and compositor-only
transforms; the module is a thin layer over them.

**Live verification: done.** The Motion control in Configuration was driven
through the real UI: Full set `data-motion="full"` and `--vd-motion-scale: 1`
over an OS that reports reduce, Reduced set `0.5`, and System went back to the
OS answer. A duration token used in a real transition resolved to `0.075s` at
scale 0.5, so the tokens are live rather than decorative.

**Judged by a human:** whether the shape primitives look right in both themes.
The harness can take a screenshot; the judgement is not a check.

---

## Live verification

The gap this document kept repeating — "needs the installed app running" — is
now closed, and closing it found three defects that every unit test, type check
and build had passed over.

### How it works

A Tauri app on Windows is a WebView2 host, and WebView2 is Chromium, so it speaks
the Chrome DevTools Protocol. The only missing piece was opening the endpoint:

```bash
VIBEDESK_REMOTE_DEBUGGING=9222 npx tauri dev
```

`VIBEDESK_REMOTE_DEBUGGING` is honoured **only in a debug build**. A shipped app
that can be inspected is a shipped app that can be driven, so the release binary
ignores it. Port `0` is refused and an existing `--remote-debugging-port` is left
alone; four tests pin that.

`scripts/live-check.mjs` attaches over loopback and runs assertions against the
real DOM. No dependencies — Node 24 ships `fetch`, `WebSocket` and `node:http`,
and the plan forbids adding packages for this.

Fifteen suites, 230 checks. Each one installs what it needs and removes it again
in a `finally`, so a failing check still leaves the machine as it was:

| Suite          | Checks | What it proves                                                                                                                                                                                                                                        |
| -------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `smoke`        | 7      | The running bundle is the one in `dist`, the app mounts, a motion policy is on the document, the duration tokens resolve to a real time, the rail renders, nothing overflows, no console errors.                                                        |
| `panels`       | 14     | Nested tabs, one selected and one tabbable, `aria-controls` on every tab, a built-in source advancing without a reload, an integer staying whole, selection surviving a data tick, a guard hiding its block, split weights becoming proportions.        |
| `data`         | 7      | A declared `http` source asks before it fetches, nothing is fetched before consent, the badge says "needs permission", the fetch starts on Allow, it keeps polling, and the badge turns live.                                                          |
| `plugins`      | 3      | Enabling a plugin adds its panel to the rail, the panel is marked with its plugin name, and disabling removes it.                                                                                                                                     |
| `motion`       | 26     | The tiered duration tokens, the three motion preferences, a panel open running a named view transition with the root snapshots neutralised, the transcript containment rule, and the Customize drawer's motion control reaching `layout.json`.          |
| `shape`        | 20     | Six shape primitives defined, the gooey filter defined and absent at rest, the empty state composing mesh, blob and grain, springs generated as `linear()` and overshooting, live numbers tweening, no layout shift, both themes rendering.             |
| `chats`        | 5      | A chat the CLI holds a lock for but is not showing can be deleted, the chat just switched to cannot, the chat switched away from can, and the switch leaves exactly one chat open.                                                                     |
| `bulk`         | 16     | Multi-select on unopened chats, the open chat refusing selection, bulk archive and bulk delete, the counts, and the way back out.                                                                                                                     |
| `widget`       | 23     | The frame is a separate origin, the capability set is closed, a widget cannot reach the app except through a declared action, `resize` is coalesced, and the html may live in a sibling file.                                                          |
| `chart`        | 25     | Axes, ticks, labels, four series, the legend, the hover readout, the log scale, stacking, and the decimation note when a series is longer than the cap.                                                                                                |
| `diagram`      | 28     | Four layouts, the edge list as text, node titles and labels, the tone mapping, and the panel loader's handling of a diagram block.                                                                                                                    |
| `agentActions` | 7      | The manifest publishes the action surface, every run kind, the permission scopes and the elicitation answers.                                                                                                                                          |
| `settings`     | 16     | One rail entry, no panel as its own icon, every row labelled and described, no section listed twice, the groups, the subtitle count, a live value, every section opening a different panel, and the way back.                                          |
| `commands`     | 21     | All ten panel-opening slash commands, `/theme` both ways, `/clear`, the palette's roles and arrow keys, Escape leaving the draft alone, Shift+Enter, and a running command not eating the next draft.                                                  |
| `keyboard`     | 11     | Shift+F10 opening a menu, the menu taking focus, the arrow keys walking it, Escape closing it and returning focus to the row, and the rail separator resizing with the arrow keys and saving.                                                          |

```bash
node scripts/live-check.mjs all       # every suite, exit 1 on failure
node scripts/live-check.mjs panels    # one suite
node scripts/live-check.mjs eval "…"  # evaluate against the page
node scripts/live-check.mjs shot out.png
```

`--data-dir` points the suites at a different app data directory; `--json` emits
machine-readable results; `--keep` skips the cleanup when you want to look at
what a suite built. A suite that throws is reported as one failed check rather
than a stack trace, and a CDP call that never answers fails after 30s instead of
hanging the run, so a window that dies mid-run reads as a named failure and the
other suites still report.

For a full agent-driven session, Google's `chrome-devtools-mcp` attaches to the
same endpoint with console, network, performance and screenshot tools:

```bash
npx chrome-devtools-mcp@1.13.0 --browserUrl http://127.0.0.1:9222
```

`--browserUrl` is the flag that connects to a running instance rather than
launching its own Chrome. That was checked against the package's own `--help`,
not assumed.

The package documents Chrome as its only supported browser. **That caveat does
not apply to this path, and it was tested rather than assumed.** Speaking MCP to
the server over stdio against a running VibeDesk:

- `initialize` returns `chrome_devtools` 1.13.0.
- `tools/list` returns 33 tools.
- `list_pages` returns `1: VibeDesk (http://tauri.localhost/) [selected]`.
- `evaluate_script` with `pageId: 1` returns
  `{"title":"VibeDesk","motion":"reduced","rail":13}`.

So the MCP server drives the WebView2 window as if it were Chrome. The
zero-dependency harness still exists and is still the primary, because it is the
one that cannot break on a package upgrade — but the MCP is a working second
option, not a hopeful one.

### What it found

Five defects, all fixed, all now in the README ledger:

1. **The `clock` built-in never ticked.** `useClockTick` started a one-second
   interval and then threw the tick away (`const [, setTick]`), so the component
   re-rendered while `useMemo` handed back the model it had built a second
   earlier. A panel bound to `vibedesk.clock` showed a frozen epoch. The plan's
   Phase 2 evidence claims this source was seen advancing, so either that
   evidence was wrong or this regressed afterwards. Fixed by returning the tick
   and putting it in the dependency list.

2. **Disabling a plugin left its panel in the rail.** `plugins_set_enabled`
   updated the plugin list and nothing re-read `panels_get`, so the panel stayed
   until the next launch — directly against the plan's "disabling removes it".
   `reloadPlugins` had the same hole, so a plugin installed while the app was
   running contributed nothing. Fixed with a `refreshPanels()` called from both.

3. **A tweened integer rendered as a fraction.** Wiring `useTween` into
   `DataBlock` made a live epoch display as `1,794,842,690.9`. A tween passes
   through fractions, and a fraction of an epoch is noise. Fixed by rounding when
   the source value is an integer.

4. **The external source path was unreachable.** `data::split_by_consent` reads
   the consent ledger on every reconcile, and an origin with no decision is
   deliberately not fetched — but `data_consent_list` and `data_consent_set`
   were written, tested and **never registered**, and no component read the
   `awaiting-consent` state. The ledger could only ever be empty, so every
   `http` source waited forever and the panel reported it as `stopped`. The
   unit tests passed because they call `consent::decide` directly. Fixed by
   registering both commands, adding `origin` and `state` to the frontend's
   `SourceStatus`, and rendering a consent bar in the panel with Allow, Deny and
   Ask again.

5. **Every boot logged a 404.** No favicon was declared, so the webview
   requested `/favicon.ico` and the console carried one error from the first
   frame. Fixed with an inline `data:` SVG icon, which the existing
   `img-src data:` policy already permits.

6. **Every chat the user had visited became undeletable.** `chats::is_active`
   read `logs/session/active/<id>.lock` as "this chat is open", but the lock is
   per session, not per process: one CLI process keeps a lock for every session
   it has ever loaded and only drops them when it exits. After switching from A
   to B, A's lock was still there and the delete was refused with _That chat is
   open_. Fixed by comparing against the session the bridge actually has loaded
   and leaving the lock directory alone.

7. **The debugging-endpoint check was order-dependent.** It read the boot notice
   out of the DOM, and the notice is a chat item, so `clearSession()` wiped it on
   every chat switch. The check was green only on a freshly booted app. Fixed by
   giving the Help drawer's **Build** section a durable `debugging` row and
   reading that instead.

The first two are exactly the class of defect that only a running app shows: a
value that stops moving, and a list that does not refresh. Neither is reachable
from a unit test, and both passed `tsc`, `vite build` and 315 Rust tests. The
fourth is the opposite failure: a path that was tested in isolation and wired to
nothing, which no amount of unit testing would have caught. The sixth is a third
kind: a guard that asked a plausible question of the wrong file, and answered it
wrongly for every chat the user had ever opened.

### What it also confirmed

- Safe mode (`VIBEDESK_SAFE_MODE=1`) removes every custom and plugin panel.
- An invalid `panels.json` is refused, the last good config is kept, and the
  reason is shown verbatim: _"Config not applied — panels.json is not valid JSON:
  … `@gt` compares numbers or strings, but the left side is boolean"_.
- The app boots with zero console errors and zero warnings.

---

## The seven items this section used to carry, and where each one went

1. **The live suite is not in CI.** It is now `npm run verify`, which chains
   `tsc`, the unit tests, `vite build`, `cargo test --lib` and the live suite.
   The live step needs a built app, a display and a free port, which is why it is
   a command rather than a CI job; the other four are plain commands.

2. **No frontend test harness.** `tests/` holds 48 `node --test` cases over the
   pure modules. Node 24 strips the types, so there is no build step and no
   dependency: `data.ts` has one `import type`, which stripping removes.

3. **The four deferred defects.** All four are fixed, and the README ledger
   entries 20–23 now read as fixed rather than deferred:
   - Context menus take focus on open, walk with the arrow keys, Home and End,
     close on Escape and Tab, and hand focus back to the row they came from.
     Shift+F10 and the Menu key open them from the keyboard.
   - The rail resize handle is a focusable `separator` with `aria-valuenow`,
     `aria-valuemin` and `aria-valuemax`. Arrow keys move the edge, Shift moves
     it further, Home and End jump to the limits, and the width is persisted once
     the key repeat settles.
   - Agent-authored SVG is rebuilt from an allowlist before it is injected. The
     policy is a pure function over a plain tree, so it is unit-tested; the DOM
     is only the adapter that parses and serialises.
   - `series()` decimates to 512 points by taking each bucket's extremes, so a
     spike survives and the line does not stop halfway. The chart says
     "512 of 10,000 points" when it has done so.

4. **`stagger()` was exported and unused.** It is applied to the settings
   directory, which is the one list in the app that mounts once and never
   re-renders on a data tick. The index is global across groups, so the cascade
   reads as one list.

5. **Plan document drift.** The tab shape, the operator count and the status line
   are corrected in `docs/PLAN-dynamic-customization.md`.

6. **`SCALE` had no `0` entry.** The constant is gone; `motionScale()` reads
   `--vd-motion-scale` from the computed style and returns a number, so the
   stylesheet is the only place the scale is defined.

7. **The `plugin` field is a reserved word.** The README's panel section says so
   next to the field list, not only in the validator's error message.

## How to verify this yourself

```bash
export PATH="/c/Users/Santera/.cargo/bin:$PATH"
cd src-tauri && cargo test --lib      # 667 passed
cd src-tauri && cargo check --lib     # 0 warnings
cd .. && npx tsc --noEmit             # clean
npm run test:unit                     # 48 passed
npx vite build                        # clean
```

Then the live half, which is now a command rather than a manual session:

```bash
VIBEDESK_REMOTE_DEBUGGING=9222 npx tauri dev   # in one shell
node scripts/live-check.mjs all                # in another
```

`all` exits 2 when the app is not reachable, 1 when a check fails, 0 when
everything passes. `eval` runs an expression against the page and `shot` writes
a screenshot.

### The stale-bundle trap, and why it cannot bite

`tauri dev` serves the frontend from Vite, but a plain `cargo build` embeds
`dist/` at compile time. That made a silent failure possible: edit the frontend,
run `cargo build`, and the binary keeps the previous bundle — the change appears
not to work, and nothing reports an error. Worse, the live suite would then
"verify" code that is not running.

Three guards, so it cannot happen and cannot be missed:

1. **`dist/` is a build input.** `src-tauri/build.rs` emits
   `cargo:rerun-if-changed=../dist`, so a `vite build` forces the crate that
   embeds it to rebuild. The stale embed is no longer reachable by accident.
2. **A stale build warns.** The same script compares `dist/index.html` against
   the newest file under `src/` and emits a `cargo:warning` naming the file that
   is newer. Editing the frontend and building without `vite build` says so.
3. **The app and the suite both check.** `build_info` reports the embedded
   bundle, shown in Help under **Build**; and `smoke` compares the bundle the
   page is running against the one `dist/index.html` names, failing with
   _"running index-Dal-poBz.js, dist has index-STALE0000.js — run `npm run build`
   then `cargo build`"_. Verified by pointing `dist/index.html` at a fake bundle:
   the suite exits 1.

What the live run covers, and what is still a human call:

- **Covered:** nested tabs rendering, one tab selected and one tabbable,
  `aria-controls` on every tab, a `when` hiding its block, split weights, tab
  selection surviving a data tick, arrow-key navigation, Escape closing a panel,
  a built-in source advancing, an integer staying whole, a loopback `http` source
  asking for consent and then polling, the Motion setting at all three values, a
  plugin panel appearing on enable and disappearing on disable, and a debugging
  endpoint being announced when it is on.
- **Still a human call:** whether the shape primitives look right, and
  screenshots in both themes. `shot` captures them; judging them is not a check.
- **Not covered:** safe mode and an invalid `panels.json` are checked by hand
  rather than by a suite, because both need a restart with a different
  environment or a deliberately broken file. The unit tests cover the parsing;
  the live behaviour is a manual step.

---

# Phase 7 — Widgets: agent-authored scripts, no build

Added 2026-11-16, after Phase 6. This is the phase that answers the question the
whole customization surface was built toward:

> when I ask the agent to build graphs, diagrams or custom panels for plugins
> that use live data to update or control the agent behaviour, I want it to do it
> within the app without making a build.

Before this phase the answer was **no**, and the reason was a platform fact
rather than a missing feature. After this phase the answer is **yes**, with a
named boundary.

## Why `sandbox` could not be it, and what the old comment got wrong

`SandboxBlock.tsx` and `panels.rs` both carried the same claim: a `srcdoc`,
`blob:` or `data:` document inherits the _parent's_ CSP in Chromium, the app's
policy is `script-src 'self'`, therefore a frame cannot run scripts. That claim
is **correct for those three schemes** and it is still correct today.

It was incomplete in one way that mattered. A document served from a **different
origin** with its own `Content-Security-Policy` response header does not inherit
the parent's policy. Tauri serves a custom scheme on Windows as
`http://<scheme>.localhost`, which is a distinct origin from the app. So a real
execution boundary was reachable **without loosening the app's `script-src`** —
only `frame-src` had to grow by one origin.

That is the whole design. The app's policy is still `script-src 'self'`; a test
now fails if anyone changes that.

## What shipped

| Piece                    | Where                                                                                                                                                      |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `widget` block kind      | `src-tauri/src/panels.rs` — `PanelBlock::Widget { html, title, height, capabilities }`                                                                     |
| `Capability` closed enum | `panels.rs` — `Prompt`, `Action { id }`, serialised as `{"kind":"prompt"}` / `{"kind":"action","id":"…"}`                                                  |
| Validation               | `check_block` — size, height, capability count, duplicate capabilities, action-id grammar, and a forbidden-token list for loaders that would fail silently |
| Document server          | `src-tauri/src/widget.rs` (new) — `hash`, `document`, `Registry`, `serve`, `CSP`                                                                           |
| Protocol registration    | `src-tauri/src/lib.rs` — `register_uri_scheme_protocol("widget", …)`                                                                                       |
| Document registry        | `AppState.widgets`, rebuilt on boot and on every config change, before the emit                                                                            |
| CSP                      | `index.html` meta and `tauri.conf.json` — `frame-src` gains `http://widget.localhost` and `widget://localhost`                                             |
| Manifest                 | `manifest.rs` — `widgetCapabilities`, `widgetOrigin`, `widgetRule`, `widgetForbidden`, height and size limits                                              |
| Preamble                 | `src/lib/preamble.ts` — one paragraph telling the agent a widget exists and to read `widgetRule` first                                                     |
| Renderer                 | `src/components/panels/WidgetBlock.tsx` (new) — iframe, handshake, data push, theme push, capability enforcement, resize                                   |
| Types                    | `src/lib/customize.ts` — `WidgetCapability`, `capabilityName`, `WIDGET_ORIGIN`, `WIDGET_HEIGHT`, `MAX_WIDGET_PROMPT`, `widgetHash`, `widgetUrl`            |
| Dispatch                 | `src/components/panels/BlockView.tsx` — `case 'widget'`                                                                                                    |
| Live suite               | `scripts/live-check.mjs` — `widget`, 17 checks                                                                                                             |

## The boundary, in three independent layers

1. **The frame.** `sandbox="allow-scripts"` and nothing else. No
   `allow-same-origin`, so the document's origin is opaque: no cookies, no
   storage, no `parent` access, no navigation of the window it sits in.
2. **The document's own policy.** `default-src 'none'; script-src
'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:;
connect-src 'none'; form-action 'none'; base-uri 'none'`. Its own scripts run;
   the network does not exist.
3. **The bridge.** The only route back into the app, and it is a closed set. A
   widget may send a prompt or fire an action **its block declared**, and nothing
   else. An undeclared request is refused, the widget is told, and the user is
   told once.

The third layer is the one that is easy to get wrong. `event.origin` is `"null"`
for a sandboxed cross-origin frame and proves nothing, so the check is
`event.source === frame.contentWindow` plus a per-widget handshake. The live
suite proves the parent refuses an undeclared capability **even when the widget
bypasses the SDK and posts a raw invoke** — otherwise the SDK would be the only
boundary and the frame would not really be contained.

## What a widget gets

- Its own scripts, DOM and canvas. A graph, a diagram, a control surface.
- The panel's live data model, pushed on every tick — the same model the
  declarative blocks read, including the built-ins (`status`, `usage`, `session`,
  `chats`, `plugins`, `clock`).
- The live theme tokens, pushed on every theme change, applied as CSS custom
  properties so a widget does not have to know which theme is active.
- `vibedesk.ready`, `onData`, `onTheme`, `has`, `prompt`, `action`, `resize`,
  `data`, `theme`, `capabilities`.

Two details that remove footguns rather than documenting them:

- A subscriber registered **after** init is called once with the value already in
  hand. Without that, the natural pattern — markup first, then
  `vibedesk.onData(draw)` — misses the first tick and draws nothing until the
  next one.
- `prompt` and `action` wait for the handshake instead of throwing on the first
  line, so a widget may call them from its own first statement.

## What a `prompt` ack means

`sendPrompt` resolves when the **turn ends**, which is minutes, not milliseconds.
A widget that awaited it would sit on a spinner for the length of an agent turn
and then report a result it cannot use. So the bridge acks on **dispatch**: the
widget is told the text reached the agent, not that the agent answered. The
`widgetRule` in the manifest says this in the words the agent reads, and
`WidgetBlock` pre-flights `sessionId` and `cwd` so a widget in a workspace with
no open chat gets a refusal it can render instead of a promise that never
settles. A widget that wants the answer reads the panel's data model on the next
tick, which is the same route every other block uses.

## Addressing

Documents are content-addressed: `http://widget.localhost/w/<hash>`, FNV-1a over
the block's html, computed identically in Rust and TypeScript. Editing the html
changes the URL, so the frame reloads exactly when the content changes and a
stale URL 404s with a readable body instead of serving the previous widget. Both
sides pin the same test vectors; a drift is a 404 in the frame, which the live
suite would catch as a frame that never resizes.

## Two bugs found on the way, both fixed

1. **`tauri.conf.json` was not a build input.** `src-tauri/build.rs` declared
   `dist/`, `src/`, `index.html`, `package.json` and `vite.config.ts` as inputs
   and not the Tauri config, so a CSP edit did not re-run the build script and
   the binary kept serving the previous policy. This is the same class of trap
   the build script's own doc comment is about, one file over. Fixed by adding
   `tauri.conf.json` and `capabilities` as inputs.

2. **The CSP lived in two places and had drifted.** The policy that actually
   applies is a `<meta>` tag in `index.html`; `tauri.conf.json` carries a copy.
   The comment beside the meta says the two must be kept identical and that they
   had drifted once already. Changing only the config produced a frame that was
   blocked with a console error and no other symptom. Fixed by changing both, and
   by a test that reads `index.html`, extracts the meta policy and asserts it
   equals `app.security.csp` — so the next drift fails a test instead of a
   widget.

## What this phase left open, and what closed it

Phase 9 closed five of these and Phase 10 closed the sixth. They are recorded
here because the reasoning is what makes the current shape legible.

- **`chart` was shallow.** Phase 9 gave it axes, ticks, labels, four series, a
  legend, a hover readout, a log scale and stacking. It is a plot, not a strip.
- **`actions` was a closed enum.** Phase 9 widened it to seven run kinds —
  `prompt`, `command`, `open-url`, `reveal`, `mode`, `option`, `cancel` — plus
  `approve`, `reject` and `answer` for a paused agent. A widget can fire any of
  them. It still cannot invent a new effect, and that is the boundary: the
  alternative is a widget with IPC access.
- **A widget could not declare its own data source.** It can now: a widget block
  carries its own `sources`, so one edit is enough.
- **No widget-side file access.** A `file` capability now exists, read-only,
  scoped to the workspace.
- **macOS and Linux have never run.** The origin there is `widget://localhost`
  rather than `http://widget.localhost`; both are in `frame-src` and
  `widget::path_of` handles both shapes. This is a statement about coverage, not
  a defect: the code paths are the same and neither platform has been exercised.
- **No frontend unit tests.** `tests/` now covers `data.ts`, `motion.ts` and
  `sanitize-svg.ts`. `widgetHash` is still pinned by a Rust test with vectors
  computed from the TypeScript implementation, and the live suite proves the two
  agree end to end.

## The five decisions this section used to hold

1. **`sandbox` stays, as the markup-only variant.** It is dominated by `widget`
   in capability and not in privilege: content that does not need scripts should
   not get them, and a sandbox block cannot run one. `AGENTS.md` lists it as a
   known shallow surface for exactly this reason — it is markup and CSS with no
   execution boundary, and it is not trying to be a widget.
2. **`chart` was deepened, not removed.** See Phase 9.
3. **A widget reads the whole model, and the data plane stays the only poller.**
   A widget that polls on its own schedule would be a second source of truth for
   freshness, and the panel would have no way to show what it fetched.
4. **Widget html may live in a sibling file.** Phase 9 gap 7 added
   `widgets/<id>.html`, referenced by the block, so a 64 KB widget is a 64 KB
   file rather than a 64 KB escaped string.
5. **`resize` is coalesced.** Phase 9 gap 6 made the parent apply the last
   message per frame instead of one re-render per message.

## Proof, not assertion

The boundary was probed from outside the app rather than argued from the code.
A debug build with `VIBEDESK_REMOTE_DEBUGGING=9222` exposes the widget frame as
its **own CDP target** (`type: "iframe"`, url `http://widget.localhost/w/…`) —
which is itself the evidence that it is a separate origin, because a same-origin
frame is not a separate target. Evaluated inside that target:

| Probe                      | Result                      |
| -------------------------- | --------------------------- |
| `location.origin`          | `http://widget.localhost`   |
| live data arrived          | `of 256.0k tokens`          |
| `capabilities`             | `["prompt"]`                |
| theme tokens arrived       | `--vd-accent: #ff7a1a`      |
| both canvases painted      | ring 1477 px, spark 1332 px |
| `parent.document`          | `SecurityError`             |
| `localStorage`             | `SecurityError`             |
| `fetch` / `XMLHttpRequest` | rejected                    |
| `WebSocket` / `Image`      | blocked                     |

And the control path end to end: clicking the demo widget's button moved its
status line from `""` to `sent to the agent`, which is the dispatch ack
described above travelling frame → bridge → `sendPrompt`.

The demo panel (`live-widget`: a context ring, a 60-sample sparkline of
`usage.used`, and a "Summarise this session" button) was written to the debug
data directory and proposed to the installed app through the normal
`pending/panels.json` approval flow. It is additive — the three existing panels
are untouched — and it is a proposal, not an applied change.

# Phase 8 — One settings entry, and the end of `billion-context`

Added 2026-11-16, after Phase 7. Two changes that are not features in the
customization sense but are the difference between a surface that scales and one
that does not.

## The rail was a directory of features

It carried eight icons — Plugins, Configuration, Sessions, Context, Billing,
Skills, Customize, Help — all the same size, all the same weight, all the same
shape. Nothing in it read as more important than anything else, and every new
surface added a ninth. That is the failure mode of a rail that doubles as a menu:
it grows monotonically and stops being navigation.

All eight were settings surfaces. They are one entry now.

| Piece            | Where                                                                                                                       |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Section registry | `src/lib/settings.ts` — five groups, eleven sections, each with a label, a one-line hint and an icon                        |
| Directory        | `src/components/panels/SettingsPanel.tsx` — grouped rows, each carrying the live value that would make you open it          |
| Rail             | `src/components/chrome/Rail.tsx` — Conversation, the user's own panels, Settings, collapse                                  |
| Routing          | `src/store/app.ts` — `PanelId` is `SettingsSectionId \| 'settings' \| null`; a section id still opens that section directly |
| Back             | `src/components/chrome/Drawer.tsx` — a section shows a back arrow to the directory                                          |

Two decisions worth naming:

- **The section ids stayed in the union.** A dozen call sites open one directly —
  the status bar's model chip opens Configuration, an action opens Plugins, the
  title bar opens Status. Rewriting them all to `setPanel('settings')` plus a
  section would have been churn, and it would have lost the intent. They keep
  working, and the rail's Settings button stays lit while a section is open,
  because a section _is_ inside settings.
- **The directory is a readout, not a menu.** Each row carries the number that
  would make you open it: the model in force, how much of the window is full, how
  many chats exist, whether an update is waiting, how many proposals are pending
  approval. A row with nothing to report shows no badge rather than a zero.

The `settings` live suite is 16 checks: one entry in the rail, no panel as its
own icon, every row labelled and hinted, no section listed twice, the sections
grouped, at least one live value, every row opening a different panel, the rail
lit inside a section, a way back from every section, and a section opened from
the status bar still being a section.

## `billion-context` is gone

It was a third-party context-folding proxy that VibeDesk never depended on. It
had a panel in `panels.json` whose numbers were baked in, because a `sandbox`
block runs no scripts and cannot poll — the panel said so in its own text. That
panel is removed, along with every other trace on the machine: the proxy's
leftover scripts and probe files, a trashed session that quoted it, the approval
receipt that named it, and two lines in the DeepSeek harness's memory notes. The
repo's only remaining mention was a test fixture URL, which is now a neutral
path.

The lesson is the one `AGENTS.md` already carries: a panel that cannot poll is a
screenshot with a title bar, and a screenshot of a service you do not run is
worse than nothing.

---

## Phase 9 — Closing the seven gaps

Seven gaps were named after the widget boundary shipped. Each is closed, and
the table says what closing it meant.

| #   | Gap                                                                | Status                                                                                 |
| --- | ------------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| 1   | `chart` was a strip over a flat array — no axes, no scales         | **Closed.** A real plot; `sparkline` is now an explicit style                          |
| 2   | `actions` was a closed enum; "control the agent" meant "send text" | **Closed for the four controls the app has.** Still a closed set, by design            |
| 3   | A widget could not declare its own data source                     | **Closed.** `sources` on the block, run under the panel id                             |
| 4   | No widget-side file access                                         | **Closed.** A `file` capability, workspace-scoped, budgeted                            |
| 5   | macOS/Linux untested                                               | **Partly closed.** Both URI shapes are unit-tested; nothing has run on either platform |
| 6   | `resize` was clamped but unthrottled                               | **Closed.** Coalesced to one parent render per frame                                   |
| 7   | Widget html was an escaped JSON string                             | **Closed.** `src` points at `widgets/<name>.html`                                      |

### 1 — `chart` is a plot

`PanelBlock::Chart` is now `{ label, style, values?, series?, x?, yMin?, yMax?,
height?, unit?, time? }` with `ChartSeries { name?, values }`. `ChartStyle` gained
`area`, so the four styles are `bar`, `line`, `area`, `sparkline`.

`src/components/panels/ChartBlock.tsx` (new, ~495 lines) draws:

- a y-axis with whole-number ticks from `niceTicks` (1/2/5/10 × a power of ten)
  and a gridline per tick;
- an x-axis with category labels, thinned to roughly one per 46px, and epoch
  milliseconds read as `HH:MM` when `time` is set;
- up to four named series with a legend, over a four-colour palette whose first
  entry is the theme accent;
- a hover guide line and a readout naming the point and every series value with
  the block's `unit`;
- `sparkline` as the axis-free strip, kept as an explicit choice rather than the
  default.

Geometry is measured with a `ResizeObserver` and computed in CSS pixels rather
than in a scaled `viewBox`, because a stretched `viewBox` stretches the axis
labels with it.

**A bug found on the way.** `PanelBlock` carried
`#[serde(tag = "kind", rename_all = "kebab-case")]`, which renames _variants_,
not fields — so `yMin` and `yMax` were silently dropped on load and the scale
was always fitted. `rename_all_fields = "camelCase"` fixes it and is safe
because every other field name is a single word.

Five tests: a chart needs values or series; the single-series shorthand is
accepted; every series and both axis bounds are collected as paths; more series
than the palette holds is refused; an out-of-range height is clamped rather than
refused.

### 2 — `actions` has seven run kinds

`ActionRun` gained `Mode { mode }`, `Option { option, value }` and `Cancel`, and
`ActionRun::KINDS` publishes all seven. The two new argument kinds name ids the
agent advertises — a mode id from `availableModes`, an option id from
`configOptions` — so a widget or a button can switch the model, change a
configuration option, or stop a turn, not only send text.

`ActionStrip.tsx` runs them through `sessionApi.setMode`, `setConfigOption` and
`cancel`, mirroring the result into the store so the UI does not disagree with
the backend. `CustomizePanel.tsx` offers a `<select>` built from the live modes
and options, falling back to a text field when no session is open.

**Still a closed set, deliberately.** A widget cannot call an arbitrary IPC
command; that is the whole point of the frame boundary. What changed is that the
set now covers the controls the app actually has. A control the app does not
have is still not expressible, and pretending otherwise would be the shallow
answer.

### 3 — A widget may declare its own sources

`PanelBlock::Widget` gained `sources: Vec<Source>`. `validate` collects the
panel's sources **plus** every widget block's into one list, so the per-panel
budget and the id-uniqueness rule cover both, and a widget source may not shadow
a panel source. `data.rs::declared_sets` chains them, so they run under the
panel id and arrive in the same model.

The frontend half was the part that would have failed silently: `usePanelModel`
read `panel.sources` only, so a widget-declared feed would have run in Rust and
never reached the widget. `panelSources(panel)` in `customize.ts` is now the one
place that answers "what does this panel read", and both the model builder and
the panel header use it.

### 4 — A `file` capability

`Capability::File` is the third capability. A widget calls
`vibedesk.read(path)` and gets the file's text.

Three limits, each in the layer that can enforce it:

- **The path.** `workspace_read` resolves against the workspace root but does
  not refuse an absolute path or a `..` segment, so `WidgetBlock.tsx` checks
  first: relative, no drive letter, no leading separator, no `..`, no NUL, under
  400 characters. A widget is agent-authored HTML; treating its path as trusted
  would make the capability a read-anything primitive.
- **The size.** 2 MB, enforced in Rust by `workspace_read`.
- **The rate.** 20 reads per sliding 10-second window, enforced in the bridge.
  A widget polling in a loop is the case this exists for, and the refusal is
  reported back to the widget so its author sees it.

### 5 — Both URI shapes, and what is still untested

Tauri serves a custom scheme as `http://<scheme>.localhost` on Windows and
Android, and as `widget://localhost` on macOS and Linux. `path_of` already
reduced both to the same path; two tests now pin it —
`both_platform_uri_shapes_reach_the_same_widget` serves five URI shapes
including `https://` and a query string, and
`the_path_is_the_same_whatever_the_authority_is` pins the reduction directly.

**Coverage:** nothing has run on macOS or Linux. The tests prove the handler
does not care which authority it is given; they do not prove the frame loads
there. That needs a machine, and saying otherwise would be the kind of claim
this document exists to avoid.

### 6 — `resize` is coalesced

The bridge keeps the pending height in a ref and schedules one
`requestAnimationFrame`; every message in that frame collapses into one
`setBox`. A message that would not change the height is dropped before the frame
is even scheduled, and the frame is cancelled on unmount.

The live suite measures it rather than asserting it: a widget posts 300 resize
messages in one task, a `MutationObserver` counts how many times the parent
actually rewrote the frame's `style`, and the check requires the count to be
above zero and at most 40. Unthrottled, that number is 300.

### 7 — Widget html may live in a file

`PanelBlock::Widget` gained `src: Option<String>`, and `html` became optional.
`check_widget_src` requires a path under `widgets/` ending `.html`, on top of
`check_relative_path`. On load, `resolve_widget_srcs` reads the file into `html`
and errors with the path if it is missing or over 64 KB; on save,
`strip_derived_widget_html` removes `html` from any block that has a `src`, so
the file is the only copy and the two cannot drift.

**The bug this had to fix.** `save_panels_to` writes `layout` and drops
`blocks`, so on reload the widget exists only inside the layout tree — and
`resolve_widget_srcs` ran before `clamped()`, which is what rebuilds `blocks`
from `layout`. The `src` was therefore never resolved and a missing file
returned `Ok`. `resolve_widget_srcs` now walks both views, and the layout walker
is recursive over `Node`.

**And the gap that would have made it feel broken.** The config watcher stamped
four files and nothing else, so editing `widgets/<name>.html` sat on disk until
the next restart — the opposite of editing the harness live. `widgets_stamp()`
adds the directory's newest mtime and total length as a fifth stamp, and the
live suite proves the frame's content address moves after a file edit with
`panels.json` untouched.

### What the manifest and the preamble now say

`widget_capabilities` is `["prompt", "action", "file"]`. `widget_rule` names
`vibedesk.read`, the 2 MB and 20-per-10s limits, and widget-declared `sources`.
A new `widget_src_rule` says to prefer `src` over inline html and why. The
preamble carries the same three points, because an agent that only reads the
preamble should still reach for a file and a declared source.

### The edges of what this phase built

Stated as properties of the shipped code, not as work left over:

- **macOS and Linux have never run.** See gap 5.
- **The `file` capability reads only.** No write, no directory listing, no glob.
  A widget that needs to write proposes a change like anything else.
- **`chart` has no zoom, pan, grouped bars or second axis.** It has axes, ticks,
  labels, four series, a legend, a hover readout, a log scale and stacking, which
  is the difference between a plot and a strip. It is not a plotting library.
- **`actions` is a closed set of seven run kinds.** See gap 2. The set is closed
  in Rust and the frame has no route to the app that bypasses it.
- **A widget cannot declare its own capability.** Same boundary, same reason.

---

## Phase 10 — Motion removed and rebuilt

**Done.** This was the motion plan, phases A, B and D. That plan is complete and has been deleted; its record is this file.
Phase C — the `diagram` block — is not started and is deliberately its own plan.

The request was that the animation and motion go, because it was not wanted and
because it cost GPU. The plan opened by measuring, and the measurement said the
motion was **not** dropping frames on this machine. So the removal is justified
by the preference plus four costs that are real and unrelated to keyframes, and
the plan says so rather than inventing a performance story.

### What was deleted

| Piece                                                                                    | Was                                                                                                                                                                                                              | Now                                                     |
| ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `src/lib/motion.ts`                                                                      | 181 lines: `springToLinear`, `stagger`, `useTween`, `transitionTo`, `applyMotion`, `motionScale`, `tokenMs`                                                                                                      | gone                                                    |
| `src/styles/motion.css`                                                                  | 217 lines: three tiers, spring `linear()` curves, `vd-squircle`/`vd-blob`/`vd-mesh`/`vd-gooey`/`vd-gloss`, `vd-reveal`, `vd-stagger`                                                                             | replaced, ~60 lines of rules                            |
| The Motion setting                                                                       | `Settings.motion`, `MOTION_SYSTEM`/`MOTION_VALUES`/`validate_motion`/`motion()`, `motion_get`/`motion_set`, the bridge entry, the `ConfigPanel` section, the `App` state and effect, `applyMotion` in `main.tsx` | gone end to end                                         |
| `useTween`                                                                               | a rAF loop calling `setState` every frame, per live `data` block, ~340 ms per value change                                                                                                                       | a live number is shown as it is                         |
| `vd-rise`, `vd-pop`, `vd-shimmer`, `vd-pulse-dot`, `vd-blink`                            | five animations and their keyframes                                                                                                                                                                              | gone; `vd-rise`/`vd-pop` call sites became `vd-enter`   |
| `<filter id="vd-gooey">`                                                                 | an SVG filter definition shipped on every render for a class nothing used                                                                                                                                        | gone                                                    |
| `--ease-out-quint`, `--ease-snap`, `--ease-spring`, `--vd-motion-scale`, `--vd-dur-1..4` | three curves, four durations, one scale                                                                                                                                                                          | `--vd-ease`, `--vd-ease-land`, `--vd-fast`, `--vd-slow` |
| 43 `transition-colors` and 9 `duration-*` overrides                                      | colour transitions on hover, and per-element durations                                                                                                                                                           | gone; hover states are instant                          |

`vd-spin` stays. It is `transform: rotate`, it is compositor-only, and it is the
only signal that work is happening.

### What replaced it

| Piece                              | Where                   | What it does                                                                                                                                                                       |
| ---------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Two curves, two durations          | `src/styles/motion.css` | `--vd-ease` (reveal), `--vd-ease-land` (overshoot), `--vd-fast` 140 ms, `--vd-slow` 300 ms                                                                                         |
| One media query                    | same                    | `prefers-reduced-motion: reduce` sets both durations to 1 ms. Not a tier system: there is nothing to configure and nothing to get out of sync                                      |
| Tailwind defaults                  | `globals.css` `@theme`  | `--default-transition-duration` and `--default-transition-timing-function` point at the tokens, so a utility class cannot introduce a duration the language does not have          |
| `.vd-enter`                        | `motion.css`            | the one entrance: `opacity 0→1`, `translateY(4px)→0`, one shot, no stagger                                                                                                         |
| `.vd-expand`                       | `motion.css`            | `grid-template-rows: 0fr ↔ 1fr` plus opacity. No height read, no measuring, no JS                                                                                                 |
| `.vd-scroll-fade`                  | `motion.css`            | a scroll-driven timeline drives two registered custom properties that feed a mask. No listener, no re-render, no layout                                                            |
| Theme reveal                       | `src/lib/reveal.ts`     | a view transition with `animation: none` on both pseudo-elements and one WAAPI animation of `maskSize`/`maskPosition` on `::view-transition-new(root)`, spreading from the pointer |
| `vd-gloss` without a backdrop pass | `globals.css`           | a translucent fill plus a 1 px specular inset. No `backdrop-filter` anywhere in the app                                                                                            |
| `.vd-row`                          | `globals.css`           | `content-visibility: auto` + `contain-intrinsic-size: auto 80px` on every transcript row                                                                                           |

Three disclosures became real expand/collapse: the tool call body, the reasoning
block, and the approval bar's raw request. All three keep their content mounted,
which is what makes the animation possible; all three are height-capped, so the
layout pass is bounded.

Two things that were not motion were kept or changed deliberately:

- **The squircle corner stays.** `corner-shape: squircle` is a static shape with
  no runtime cost, and the user never complained about it. It moved from the
  deleted `motion.css` into `globals.css` behind `@supports`.
- **The progress bar animates `scaleX`, not `width`.** A width transition is a
  layout pass on every frame; a scale is a compositor transform of the same bar.

### What was measured

| Probe                                  | Before                                        | After                                                  |
| -------------------------------------- | --------------------------------------------- | ------------------------------------------------------ |
| Elements with `backdrop-filter`        | 0 at idle, 1 on the startup overlay           | 0                                                      |
| Elements with `filter: url(...)`       | 0 resolved, definition shipped                | 0, definition gone                                     |
| Elements with `will-change`            | 0                                             | 0                                                      |
| Animations at idle                     | 0                                             | 4, all scroll-driven (two fade pairs), none on a clock |
| Frame interval, idle                   | p50 6.9 ms, p95 7.1 ms, max 8.2 ms            | p50 6.90 ms, p95 7.00 ms, max 7.10 ms                  |
| Per-frame React work from live numbers | one rAF `setState` loop per live `data` block | none                                                   |

The frame timing is unchanged, which is what the plan predicted: the motion was
never the bottleneck. The wins are structural — no per-pixel backdrop pass, no
CPU SVG filter, no per-frame React loop, and off-screen transcript rows that are
no longer painted.

### Three defects the verification caught

Each of these was found by running the app, not by reading the diff:

1. **`.vd-enter` with `fill: both` never left `getAnimations()`.** A finished
   animation that keeps filling stays relevant forever, so it stays in the list
   and keeps its layer alive for the rest of the session. Changed to
   `backwards`: the from-state still applies on the first frame, and the
   animation is released when it ends. The end state of this animation is the
   element's own state, so there was nothing to hold.
2. **A margin on the direct child of a `0fr` grid row left the collapsed row
   8 px tall.** The margin sits outside the clipped box, so the row never
   reached zero. The gap moved to the `.vd-expand` wrapper. Measured after the
   fix: collapsed height is exactly 0 for both disclosures on screen.
3. **The live-check patched `document.startViewTransition` and never restored
   it.** The second run counted its own wrapper and reported three transitions
   where there was one. The suite now installs both counters once and removes
   them in a `finally`, and the motion suite was run twice in a row to prove it
   is idempotent.

### The five caveats this section carried, and what each turned into

All five were closed before Phase C started:

- **The row estimate was a fixed 80 px.** Now measured. `Transcript` reads every
  rendered `.vd-row` after a chat loads, takes the median, and writes it to
  `--vd-row-h` on the scroller; `.vd-row` uses
  `contain-intrinsic-size: auto var(--vd-row-h, 80px)`. On the debug data the
  median is 65 px, and the resolved value is `auto 65px`. The 80 px fallback
  survives only for the first frame of a chat with fewer than three rows.
- **The tool card body was always mounted.** Now lazy. `src/lib/disclosure.ts`
  exports `useDisclosure()`, which mounts a body on first open and arms
  `data-open` on a double `requestAnimationFrame` so the first open still
  transitions. `ToolCallCard`, `ReasoningBlock` and `ApprovalBar` all use it.
  Nothing below a closed disclosure is in the DOM.
- **The theme reveal was never seen.** Now seen. A throwaway CDP script forced
  `prefers-reduced-motion: no-preference`, clicked the theme button with real
  client coordinates, and screenshotted 180 ms into the 520 ms reveal: the new
  theme spreads from the button and the old one survives as a clean diagonal in
  the far corner. The look is correct. **The finding that matters:** this
  machine's OS reports `prefers-reduced-motion: reduce`, so the app has no
  motion at all here. The old Motion setting existed to override that and was
  removed on purpose. To see the reveal, turn on Windows Settings →
  Accessibility → Visual effects → Animation effects.
- **`vd-scroll-fade` was behind `@supports`.** Confirmed active rather than
  assumed: `mask-image` resolves to a `linear-gradient`, `--vd-fade-top: 0px`
  and `--vd-fade-bottom: 28px` at the top of a scrollable transcript. The guard
  stays as belt-and-braces for a WebView without scroll timelines.
- **macOS and Linux have still never run.** Unchanged, and not closable from
  this machine.

### The bug the readjustment found

`content-visibility: auto` broke pin-to-bottom. A row that has never rendered is
measured at its estimate, so `scrollTop = scrollHeight` in the layout effect
landed at the _estimated_ bottom; the rows that then rendered grew `scrollHeight`
and left the transcript short. Measured on a six-row chat: `scrollTop 0`,
`gap 258`. Setting `content-visibility: visible` on every row and re-pinning gave
`gap 0`, which named the estimate as the cause rather than the effect.

The fix is a bounded settle: the layout effect pins, then re-pins on the next
frame and once more after that. Two passes cover a transcript whose rows render
in batches; a third would be a loop. The pending frame is cancelled on cleanup so
a streaming chunk does not stack settles. Re-measured: `gap 0` on arrival and
`gap 0` after 1.2 s.

### Two more things the readjustment turned up

- **The reasoning and approval disclosures announced nothing.** `ToolCallCard`
  carried `aria-expanded`; `ReasoningBlock` and `ApprovalBar` did not, so a
  screen reader saw a button that changed the page with no state. Both now carry
  it. Verified live: `aria-expanded` reads `false`, `true`, `false` across a
  click, a second click and a close.
- **The arm frames were never cancelled.** `useDisclosure` armed on a double
  `requestAnimationFrame` with no cleanup, so a card that left the transcript
  between the click and the frame would set state on a dead component. Both
  frames are now tracked and cancelled on unmount.

### The reveal was painting a circle

Two reports, one cause. `::view-transition-new(root)` carried
`mask-image: radial-gradient(circle closest-side, ...)` and no `mask-size`, so
its default was `auto` — a circle inscribed in the box, centred in the window.
The WAAPI animation overrode that while it ran, but every frame outside it fell
back to the circle: the frame before `transition.ready` resolved, and every
frame after the animation ended but before the pseudo-element tree was torn
down. Sampled over one reveal, the mask read `3029.78px` at 540 ms and `auto` at
553 ms. That is the circle that appeared and vanished.

The fix is two lines of CSS and one option. `mask-size: 0px 0px` makes the
default paint nothing, so the animation is the only thing that can ever make the
incoming snapshot visible; `fill: 'forwards'` holds the last keyframe until the
tree goes away. Sampled again: `0px 0px` before the animation, a monotonic climb
through it, and `0px 0px` after — never `auto`.

The pacing was wrong too, and it is why the reveal read as a pop followed by a
stall. `--vd-ease` is `cubic-bezier(0.32, 0.72, 0, 1)`: 77% of the distance in
the first quarter of the duration. That is right for a 140 ms press and wrong
for a 520 ms sweep across a window — the last 46% of the duration covered 3.5%
of the distance. The reveal now has its own curve,
`cubic-bezier(0.25, 0.46, 0.45, 0.94)`, which spends its time evenly. Measured
diameter at 39/68/95/122 ms went from 292/754/1347/1988 px to 150/454/758/1050.

Two checks were added so neither can come back: the stylesheet must declare
`mask-size: 0px 0px` on the incoming snapshot, and the reveal animation must
fill forwards. 116/116.

## Motion plan, Phase C — the `diagram` block

Built. A graph described in JSON, laid out by an algorithm, with every visible
field bound to live data and every node able to fire an action.

### What is there

**Four layouts, four algorithms.** `layered` ranks by longest path over the DAG
left after back edges are removed, orders each rank by barycentre over four
alternating sweeps, and places columns left to right. `radial` finds a cycle by
depth-first search and closes it into a ring, then appends everything else
breadth-first from the ring so a node sits next to the node that feeds it.
`tree` is a tidy tree: leaves advance a per-depth cursor, a parent centres over
its children, and a parent that would collide with the previous subtree shifts
its own subtree right. `sequence` is lifelines across and messages down, in
declaration order.

**Every visible field is an expression.** `label`, `detail`, `tone`, a node's
`when`, and an edge's `label`. A node turns red when a source says the service is
down; an edge's label shows the live latency.

**Interaction that reaches the agent.** Hovering a node lights its incident edges
and dims everything else. Clicking a node with an `action` fires that entry from
`actions.json` — the same seven run kinds a widget can fire. An actionable node
is a focusable `role="button"` with Enter and Space handling, so the control
surface is not mouse-only.

**Edge routing that avoids nodes.** `orthogonal` on a layered or tree layout is
elbows through the gutter between ranks, which is empty by construction. A back
edge cannot use the gutter, so it leaves the flow entirely and runs along a lane
outside every box. On a radial layout there is no gutter — the nodes are on a
ring and the ring's interior is empty — so the chord is the one path that
provably crosses nothing, and `curved` bows it. Arrowheads are drawn as polygons
from the last real segment, so a zero-length segment cannot collapse one.

**Node widths from real measurement.** An offscreen canvas 2D context measures
the label and detail in the font the theme actually resolved, re-measured once
`document.fonts.ready` settles. A box sized by character count clips `WWWW` and
wastes space on `iiii`.

**Validated in Rust.** Node and edge counts capped at 40 and 80, ids slugged and
unique, every edge endpoint must exist, layout and routing from closed sets,
height clamped to 120..520. A bad diagram is a receipt the agent can read.

### The operator the plan needed and the language did not have

The plan's own example is
`"tone": { "@if": [{ "@gt": ["/api/errors", 0] }, "danger", "ok"] }`. There was
no `@if`. Without it `tone` could not be derived from a condition, and the
block's headline claim — a node turns red when a source says the service is down
— was not expressible. So `@if` was added to `expr.rs` and to `data.ts`: a
three-item array, the test must be a boolean, the result type is the join of the
branches, and the branch not taken is never evaluated. Four vectors were added to
`src/lib/expr-vectors.json` (42 → 46), and the operator count in the manifest
went from 17 to 18.

### The bug the block found in the panel loader

`PanelsConfig::clamped()` clamped `panel.blocks` — the derived view — while
`layout`, which is what renders and what `validate` walks, kept the original
value. A v1 panel with an out-of-range height would fail validation on a height
the clamp had already "fixed". `LayoutNode::for_each_block_mut` and
`Node::for_each_block_mut` now exist, `clamped()` clamps the tree and then
re-reconciles, and the latent chart-height bug went with it.

### Three bugs the screenshots found

None of these were caught by the first pass of checks, and all three were
visible the moment the four layouts were drawn side by side.

1. **The tree's subtree shift moved the wrong array.** `shift()` added the delta
   to `boxes[node].x`, but positions are assigned from the `x` array at the end
   of the walk, so the shift moved a zero and left the children behind. A parent
   landed one pixel off its single child and would have landed a whole box off in
   any wider tree. `shift()` now moves `x`.
2. **A tree's horizontal edge run used the source box's bottom, not its level's.**
   A short node on a level with a tall sibling put the run inside the level's
   band, where the tall sibling sits. `Box.band` now carries the level's bottom
   and the run happens in the gutter below it.
3. **Every edge from one source shared a single gutter line.** Three edges
   leaving one node drew three vertical runs at the same x, which reads as one
   bus with three labels. Each edge now gets its own offset within the gutter, in
   declaration order, clamped so it cannot leave the gutter and touch a box.

Two of the three are now assertions: no two node rects may overlap in any layout,
and a tree must centre a parent over its child. The third is pinned by the
`orthogonal routing draws elbows` and `curved routing draws a bezier` pair, which
at least proves the two routings are different code paths.

### The duplicate this block creates, and why it is the right one

The app already ships mermaid. `src/components/code/MermaidBlock.tsx` renders a
fenced ` ```mermaid ` block in the transcript, and the release bundle
carries mermaid's lazy chunks — `elk`, `cytoscape`, `dagre`, `katex` and one
chunk per diagram type. So there are now two graph renderers in one binary, and
that is a duplicate owner, which is the thing to be suspicious of.

They are not the same surface, and the difference is the reason the second one
exists:

|                         | mermaid in the transcript                                 | `diagram` in a panel                    |
| ----------------------- | --------------------------------------------------------- | --------------------------------------- |
| Input                   | a text DSL                                                | validated JSON                          |
| Bound to a source       | no — the text is fixed when the message arrives           | yes, every field is an expression       |
| Validated before render | `parse()` at render time, in the webview                  | in Rust, at write time, with a receipt  |
| Per-node interaction    | none; the output is an SVG string                         | hover highlights, click fires an action |
| Cost                    | ~1.5 MB of lazy chunks, paid when a mermaid block appears | ~700 lines, no dependency               |

The road not taken is mermaid plus post-processing: render the DSL, then walk the
resulting SVG to attach hover and click handlers and to substitute live values
into the text. That fails on the first requirement — a value substituted into a
DSL string is a string template, and the panel design rejects string templates
precisely because they cannot be validated or typed. It would also mean the
agent's diagram is only as safe as mermaid's parser, and the receipt would say
"rendered" rather than naming the node whose binding was wrong.

What is genuinely duplicated is the layout arithmetic: mermaid's `elk` and
`dagre` chunks already do layered layout, and this block does its own. That is
the cost of the bindings, and it is worth naming rather than hiding. If a future
pass wants one owner, the move is to make the transcript's mermaid block a
`diagram` block when the DSL is a flowchart — not to make the panel block depend
on mermaid.

### The four deliberate deviations from the plan

Each is a decision, and each is visible in the drawing rather than hidden:

- **The `<ul>` of connections is edges only.** The plan asked for a list of nodes
  and edges. Nodes already carry a `<title>` and an `aria-label`, so listing them
  again would announce every node twice; the edges are the one thing the drawing
  cannot say out loud, because a `<title>` on a non-focusable group is not
  announced. The list is the only place the connections exist as text.
- **`radial` with no cycle is a ring anyway.** A DAG has no cycle to close, so
  the order falls back to breadth-first from the first node. It is deterministic
  and it keeps neighbours adjacent, but it is not "the process that repeats" —
  there is nothing to repeat. A `radial` layout on a DAG is a choice the author
  made, not a reading of the graph.
- **A node with two parents in a `tree` gets one.** The first edge in declaration
  order wins, so the second parent's edge is drawn but the node is placed under
  the first. That is the honest behaviour for a layout that assumes a tree, and
  it is visible: the second edge is a long diagonal. A `layered` layout is the
  right answer for a DAG with shared children.
- **No zoom or pan.** Fit to width, scroll if it overflows, as the plan says. A
  40-node graph at 176 px per node is wider than any panel.

---

## Customization plan, Phases 3 and 4 — the four surfaces that were open

Four items were open when this document was first written. They are closed here.
Each one was a decision that had been deferred, not a task that had been
forgotten, so each entry says what the decision was.

These are not a phase of their own. `chart` and `actions` are block depth in
the renderer (Phase 3), `sequence` is a layout algorithm in the same phase, and
the platform seams are a build concern that belongs to no phase.

### 1. `chart` is deep, not shallow

`AGENTS.md` says a chart with no axes, scales, series or tooltips is a sparkline
wearing the word "chart". The previous revision had axes and a tooltip and was
still not deep, because it could only answer "what is the shape". It now answers
the four questions a plot is for:

| Field | What it adds | Why it is not decoration |
| --- | --- | --- |
| `stack: 'stacked'` | Series accumulate, so the top of the stack is the total | Grouped is the default because stacking two latencies says their sum, which nobody asked for. Stacking is opt-in for when the sum *is* the question. |
| `stack: 'percent'` | Each column normalises to 100 | The question is "what share", not "how much". The tooltip shows the raw value beside the share, so nothing is hidden. |
| `scale: 'log'` | Each decade is the same height | The only way to read a series spanning orders of magnitude. A non-positive point is dropped and the line **breaks** at the gap rather than jumping it. |
| `reference: [{value, label, tone}]` | Up to four horizontal thresholds | `value` and `tone` are expressions, so a budget drawn on the chart moves when the budget moves. The tone vocabulary is the diagram's, because a red line means the same thing in both. |
| Legend toggling | Clicking a series hides it | The axis rescales to what is left. Reading one series out of four is the point of a legend, and a hidden series that still set the scale would be a lie about the axis. |
| Crosshair dots | A dot on every visible series at the hovered index | A tooltip says a number; a dot says which point the number belongs to. |

Two validation rules came with it, both of which refuse rather than ignore:

- `stack` on a `line` or `sparkline` is an error. A line has no thickness to
  accumulate, so the field would be silently dropped.
- `scale: 'log'` with `yMin` is an error. A log axis cannot start at zero, and
  `yMin: 0` is the most likely thing to write.

`sparkline` stays as an explicit style. It is the honest name for the strip, and
a panel that wants a strip should say so rather than get one by accident.

### 2. `actions` reaches a paused agent

The gap was precise: `prompt` sends text to a **running** agent, and the agent
has two states where it is **stopped and waiting** — a permission request and an
elicitation — and neither had a button. Three run kinds close it:

| Kind | What it does |
| --- | --- |
| `approve { scope }` | Answers the pending permission with an allow |
| `reject { scope }` | Answers it with a reject |
| `answer { action, content }` | Answers the pending elicitation: `accept` submits `content`, `decline` and `cancel` carry none |

The option is chosen by **kind**, not by id. The ids belong to the agent and
differ between agents; `allow_once` is the one thing every ACP agent agrees on.
`scope: 'always'` prefers the always variant and falls back to the one-shot one,
because an agent that offers only `allow_once` should still be answerable. A
request that offers no option of the requested kind is answered with a plain
reject rather than left hanging — an agent paused forever is worse than an agent
that was told no.

`content` is a literal object, not an expression: the agent validates it against
the form's schema, and a value that changed shape between the click and the
answer would be rejected. It is bounded at 4 KB, and a `content` beside a
`decline` is an error rather than a field that would be ignored.

**What the harness does not cover.** The agent round trip needs a live model to
decide to call a tool, and a check that only sometimes runs is a check nobody
trusts. The `agentActions` suite pins the vocabulary, the editor and the guard
(clicking with nothing pending says so and does not throw). `approve` calls the
same `respond_permission` the approval bar calls, so the path is the one already
in daily use.

### 3. `sequence` fits its panel

A sequence diagram is the one layout whose width is decided by the container
rather than by the content: participants are columns, and a column past the
right edge is a participant the reader cannot see. Five long labels in a 460 px
panel used to need about 1080 px and scroll.

The columns are now sized to the panel before any box is measured, the labels
are truncated to the resulting width, and the gap absorbs the remainder between
`SEQ_MIN_GAP` (10 px, below which the labels are unreadable and scroll is the
honest answer) and `SEQ_MAX_GAP` (160 px, above which two participants sit at
opposite edges of a wide panel with a message arrow crossing the whole window).

The bug this found: the `useMemo` that lays the graph out did not depend on the
container width, so the column was computed once at `MAX_NODE_W` and never
recomputed. The width is now folded into a `columnWidth` that is zero for every
other layout, so only a sequence diagram re-lays-out on resize.

### 4. The platform seams are correct, and still untested

Nothing has been run on macOS or Linux. What changed is that the code no longer
*assumes* Windows:

- **The widget origin is derived, not hardcoded.** Tauri serves a custom scheme
  as `http://<scheme>.localhost` on Windows and Android and as
  `<scheme>://localhost` elsewhere. The app's own origin is decided by the same
  rule, so it is the one signal that cannot disagree with the platform: the
  frontend reads `location.origin` and picks the matching widget origin, and
  `widget::ORIGIN` in Rust is a `cfg!` over the same two cases. A user-agent
  sniff would answer a question about the browser rather than about the scheme.
- **`icons/icon.icns` exists.** macOS bundling needs it and it was missing, so
  `tauri build` on macOS would have failed. Generated with `tauri icon` and added
  to the bundle list.
- **`bundle.targets` is `"all"`**, so `tauri build` produces the platform's own
  bundles rather than an NSIS installer everywhere.
- **`paths.rs` and `install.rs` were already guarded** — `cfg!(windows)` around
  the `APPDATA`/`LOCALAPPDATA` lookups and `CREATE_NO_WINDOW`. Audited, not
  changed.

The remaining gap is honest and unchanged: **nobody has run this on either
platform.** The seams are correct by construction; that is not the same as
tested, and this file will not claim otherwise.

---

## Motion plan, Phase B — the theme reveal, and why it looked broken

The report was "I keep clicking the theme change and it doesn't have any
animation, the change is immediate". The installed build was not stale. The
cause was one line of platform state:

```
SystemParametersInfo(SPI_GETCLIENTAREAANIMATION) → ClientAreaAnimation=0
```

Windows "Animation effects" is **off** on this machine, so Chromium reports
`prefers-reduced-motion: reduce`, so `revealTheme` took its `apply()` early
return and the theme flipped instantly. A reinstall would not have changed that,
and neither would any amount of work on the reveal itself.

The fix is an in-app preference rather than a change to the OS setting, because
the OS setting is machine-wide and blunt: a user who turned animation effects
off for battery reasons still wants a theme reveal, and a user who wants no
motion should not have to change a system preference to get it.

| Layer | Change |
| --- | --- |
| `layout.rs` | `Motion { System, Full, Reduced }`, default `System`, a closed enum so an unknown value is a rejected file |
| `customize.ts` | `LayoutConfig.motion`, written to `data-motion` on the document element by `applyLayout` |
| `reveal.ts` | `reduced()` reads `data-motion` first and only falls back to the media query for `system` |
| `motion.css` | `@media (prefers-reduced-motion: reduce) { :root:not([data-motion='full']) { … } }` plus `:root[data-motion='reduced']`, so the tokens and the reveal read the same attribute and cannot disagree |
| Customize drawer | A Motion segmented control with a sentence explaining what each value does |

A second, independent bug was found on the way: `src/lib/actions.ts`'s
`toggle-theme` builtin command called `state.setTheme` directly, with no reveal
at all. That is the path the `/theme` slash command and any `actions.json` entry
with `run: { kind: 'command', command: 'toggle-theme' }` take, so the same bug
had a second doorway. It now routes through `revealTheme` with a centre origin.

Four live checks pin it, including the exact case that was broken: with the OS
reporting `reduce` and the preference set to `full`, a theme toggle must produce
one view transition and one mask animation. And the reverse: with the OS
reporting `no-preference` and the preference `reduced`, it must produce none —
while still switching the theme, because a preference that silently swallows the
toggle would be worse than no preference.

### Two things this section settled

- **The default is `system`, which is correct.** The installed app's
  `layout.json` is set to `motion: "full"` so the reveal works out of the box on
  this machine, whose OS reports `prefers-reduced-motion: reduce`. A fresh
  install on a machine with animation effects on gets the reveal with no
  configuration.
- **`openSection` in the live check is idempotent.** Clicking the Settings rail
  button while the directory was already showing used to close it, and the wait
  that followed timed out. The click is now conditional, so every caller can
  reach a section from any state.

## Install location — the folder Tauri put on `C:`

The question was asked twice: the app is installed on `D:`, so why does a folder
appear on `C:`? The first answer was `paths::app_data_dir()`, which already
prefers `<exe dir>/data` over `%APPDATA%`. That answer was correct and
incomplete. It governs the files VibeDesk writes. It does not govern the files
Tauri writes.

### The cause

`tauri::manager::webview` forces a data directory on Windows and Linux, and
creates it before the webview starts:

```rust
if pending.webview_attributes.data_directory.is_none() {
  match manager.path().app_local_data_dir() {
    Ok(user_data_dir) => pending.webview_attributes.data_directory = Some(user_data_dir),
    ...
  }
}
if let Some(user_data_dir) = &pending.webview_attributes.data_directory {
  if !user_data_dir.exists() { create_dir_all(user_data_dir)?; }
}
```

`app_local_data_dir()` is `%LOCALAPPDATA%\<identifier>`, so every launch created
`C:\Users\<user>\AppData\Local\ai.mistral.vibedesk` no matter where the
installer had put the executable. It stayed empty — `pin_webview_data_dir` sets
`WEBVIEW2_USER_DATA_FOLDER` to `<exe dir>/data/webview2` and the profile lives
there, 84 MB of it, 75 files touched in ten minutes — but an empty folder on the
wrong drive is still a folder on the wrong drive, and it came back on every
launch.

### The fix

`pin_app_directories` sets `appDirectoriesOverride` to `paths::app_data_dir()`
before the builder runs. That is the supported way to move all five app
directories at once — config, data, local data, cache and log — and it is what
makes `app_local_data_dir()` resolve to `<exe dir>/data`, a directory that
already exists, so Tauri creates nothing new.

Two properties the override depends on, both now tested:

- **The root must be absolute.** Tauri reads a relative override as relative to
  the executable, so a relative answer here would silently relocate every app
  directory. `the_data_dir_is_absolute`.
- **Every file the app owns must sit under it.** `settings_live_under_the_data_dir`.

The override is set at runtime rather than in `tauri.conf.json` because a static
relative path only works for per-user installers. A per-machine install in
`Program Files` is not writable next to the executable, and the runtime value
already falls back to the per-user directory in exactly that case.

### What was verified

- `cargo test --lib` — 664 passed, 0 failed.
- The stale `%APPDATA%\VibeDesk` (a `settings.json` last written on 14 November,
  from before the portable data dir existed) and the empty
  `%LOCALAPPDATA%\ai.mistral.vibedesk` were deleted, the app was launched, and
  neither came back.
- The webview profile stayed at `<exe dir>/data/webview2` and kept growing, so
  the override did not move it and did not orphan the existing profile.

### How the webview profile is pinned

`pin_webview_data_dir` sets `WEBVIEW2_USER_DATA_FOLDER` before the window is
created, and WebView2 prefers it over the `data_directory` Tauri hands to
`CreateCoreWebView2EnvironmentWithOptions`. Verified: the profile is at
`<exe dir>/data/webview2`, it kept growing, and the `C:` folder stayed empty.

The precedence is WebView2's behaviour rather than something this code states,
which is why the environment variable is set as early as it is — before any
window exists, so there is no window whose `dataDirectory` could disagree with
it.

---

# Phase 10 — Six upstream projects become capabilities

Added 2026-11-17. Six repositories were asked for as core capabilities, with the
CLI's own compaction replaced by theirs, panels only where a panel is the right
surface, and the whole thing additive enough to merge with concurrent work on the
same tree. A seventh was linked by mistake and is not part of this layer.

The contract is `docs/PLAN-capabilities.md`. This section records what changed
here and the one thing that looks like a contradiction.

## `billion-context` is back, and Phase 8 was still right

Phase 8 removed a `billion-context` panel from `panels.json` because its numbers
were baked in: a `sandbox` block runs no scripts, so the panel was a screenshot of
a service VibeDesk did not run. That removal was correct and stays correct.

What exists now is a different thing with the same name. `ranxianglei/billion-context`
is a real compression kernel — `kernel/`, pure TypeScript, MIT — and it is ported
into `src-tauri/src/capabilities/compact/` and attached to a boundary VibeDesk
owns: the model request, reached by being the endpoint it is sent to. The panel
that renders it polls the proxy's live session table. Nothing about it is baked
in.

The distinction is the one `AGENTS.md` draws: a capability that is described but
not reachable is worse than a missing one. Phase 8 removed the description. This
phase added the capability.

## What attaches where

| Capability | Boundary | Owner of the boundary |
| --- | --- | --- |
| terminal compaction (tokenjuice) | the model request, on `bash` results | `capabilities/proxy/tools.rs` |
| sparse reading (SparseReading) | the model request, on `read_file` and `cat` results | `capabilities/proxy/tools.rs` |
| context compaction (billion-context) | the model request | `capabilities/proxy/` |
| reflective memory (dream-memory) | transcript + proxy system prompt | `capabilities/memory/` |
| code graph (codebase-memory-mcp) | MCP registration | `capabilities/external.rs` |
| web search and read (donsetch) | MCP registration | `capabilities/external.rs` |

The proxy is the only honest way to see the request: the agent builds it
internally, so the only supported way to read it is to be the endpoint.

### Why reading and terminal moved to the proxy

They were built against `fs/read_text_file` and `terminal/output`, on the
assumption that an agent offered client file and shell tools will use them.
`client_capabilities()` advertises both, and its doc comment said Vibe routes its
tools through the client when the client claims them. It does not. A traced turn
that read a file and ran a command produced zero `acp: agent → client:` lines:
the agent used its own `read_file` and `bash` and never sent an `fs/*` or
`terminal/*` request.

Both capabilities therefore reported zero calls, and the zero was honest — they
were wired to a boundary the agent never crosses. They now rewrite tool results
inside the model request, keyed by the tool name the agent actually sends. The
ACP handlers stay as a fallback. `proxy::needed()` starts the proxy when any of
compaction, reading or terminal is on, rather than only for compaction.

Three things this exposed, all fixed:

- The card named `fs/read_text_file`, a tool the agent does not have. It now
  names the agent's own tool with the agent's own numbering — zero-based
  `offset` for `read_file`, one-based `line` for ACP — via `sparse::ReadTool`.
- The card offered only "read the whole file", so the agent either paid the full
  cost or went around it with `sed -n`. It now lists each section with its line
  range and a call per section.
- `cat README.md` was a one-command way around the gate. A whole-file dump
  through the shell is now carded by the same gate. The check strips the
  `cd <dir> &&` prefix the agent's shell tool adds to every command, because a
  guard that refuses anything containing `&&` never fires in practice.

## Why every capability has a panel

Six capabilities, six drawers, plus the `/capabilities` ledger that shows all six
together. No two share a drawer, and the registry enforces that in a test rather
than leaving it to convention: a capability with no surface would be a row the
user can enable and never inspect, which is the failure Phase 8 was about.

Each drawer shows something no other surface can: the proxy's live session table
and tier structure, the rule families that fired, a gate probe on an arbitrary
path, the memory store and its gate, and the install and registration state of the
two native servers.

## What is deliberately not built

Recorded in full in `docs/PLAN-capabilities.md`. The two that matter:

- **SparseReading's `sro_read` tool.** ACP gives the client no way to add tools.
  The gate, the card and the `line`/`limit` paging all work over the existing
  protocol; the tool surface needs an MCP server and is named as the next step.
- **PDF, XLSX and YAML previews.** Detected and reported as
  `Structure::Unsupported` with the reason. Not faked.

## The tier ladder is built, not deferred

An earlier draft of this phase shipped T1 only and recorded T2/T3 as a gap. That
was the wrong call: a port that stops at T1 does not replace the CLI's
summariser, it adds a second and shallower one, and the gap was visible to the
user as a session that kept accumulating summary lines.

All three tiers are now built. T1 folds raw messages into a summary block. T2
distils several blocks into one and T3 condenses distilled blocks again; a new
block's tier is `max(children) + 1` capped at 3. The nudge escalates: below
`min_compress_range` it asks for a distillation over a block range (`b3–b7`)
rather than a compression over a message range, which is the cheaper ask. Both
gates are settings, and the compaction drawer shows the resulting structure per
conversation (`T1 ×2`, `T2 ×1`) because a single block count cannot tell a flat
transcript from a distilled one.

The children of a distillation are deactivated, not deleted, so `decompress`
restores them, and the raw messages behind them stay reachable through the CCR
store.

## Verification

- `cargo test --lib` — 667 passed, 0 failed. 383 were the baseline; 284 cover this
  layer, including all 136 tokenjuice fixtures replayed against the port.
- `npx tsc --noEmit` — clean.
- `npx vite build` — succeeds.
- `npm run test:unit` — 48 passed.

Verified end to end, against a live window. The proxy carried real requests from
the real UI through the real kernel to a fake OpenAI-compatible upstream, and the
panel was read back from screenshots of the running app. That run found four
defects no unit test had caught. They are the strongest evidence in this phase,
because each one was invisible to the suite and fatal in use:

1. **The transient system prompt invalidated every ref.** `with_system` appended
   the nudge to the first system message *after* `process_turn` assigned refs.
   `base_id` hashes message text, so the system message's identity changed and
   every ref the nudge named stopped resolving — every compression was rejected
   with "ref m00001 does not exist in this session". The prompt is now applied to
   a clone at encode time and never touches the kernel's array. Regression test:
   `the_transient_prompt_must_not_touch_the_kernels_message_array`.
2. **The usage bar reported the compressed total as the live size.** `statuses()`
   passed `stats.tokens_compressed` as the token count, so a session rendered
   `usage: 2.2` — 220% of the window. `SessionInner` now carries `last_tokens`,
   set from `turn.tokens_after`.
3. **Tiers 2 and 3 were unreachable.** After escalating to a distillation,
   `decide` still gated `pending_tokens` on `min_compress_range`. Summaries are
   small by construction, so a distillation could never clear it and the nudge
   held forever with "nothing large enough left to fold". `decide` now takes the
   floor belonging to the thing being asked for. Regression test:
   `a_distillation_is_measured_against_its_own_gate`.
4. **Panel settings did not reach an open conversation.** `Proxy::session`
   returned the existing kernel with the config it was born with, so `compress
   at` and `distil at` were decoration until the app restarted — a control that
   implied a capability it did not have. The kernel's config is now refreshed on
   every request. Regression test: `a_live_session_picks_up_a_changed_gate`.

# The transcript folds tool calls into one bar

The transcript used to render one card per tool call, each on its own line. A
turn that read a file, edited it, read it back and rebuilt produced six cards
between two sentences, and the conversation stopped reading as a conversation
and started reading as a log. The calls are evidence, not dialogue.

They are now one dim bar per run of consecutive calls: a wrench, a chevron, the
words `on it`, and either a pulsing dot while a call is unsettled or a count
once they have all settled. Clicking it opens the same disclosure the reasoning
block uses, with the cards indented under a left rule. Collapsed, the cards are
not mounted at all, so a long turn costs one row instead of six.

## The bug the bar shipped with

The first version decided liveness with `(call.status ?? 'pending') === 'pending'`.
A call that is actually running reports `in_progress`, not `pending`, so the
predicate was false for every real call and the bar was permanently settled: the
pulsing dot never appeared, and a turn in progress looked finished.

Nothing in the type system caught it — `ToolStatus` is
`'pending' | 'in_progress' | 'completed' | 'failed'` and both unsettled values
are legal. The browser harness caught it only because it happened to sample the
transcript while a call was open, which is luck, not coverage. The predicate now
lives in `src/lib/tool-run.ts` as `toolRunState`, with eight cases in
`tests/tool-run.test.ts` that pin down every status, the missing-status case, and
a run that mixes a failure with a call still running.

## A failure is the one thing the bar must not hide

Folding calls behind a dim line is only honest if the line still says when
something went wrong. A failed call now adds `· N failed` to the bar in the
danger colour, so a collapsed run reports the failure without being opened. The
count is not dimmed with the rest of the bar.

`failed` is a status the bridge already handles — `track_tool_activity` treats it
as settled, and `a_failed_call_is_finished` covers it — so the branch is
reachable. It is not exercised by a screenshot: no turn in testing produced a
failed call. A missing file came back `completed` with the error in its output,
and a deliberately invalid tool argument was corrected by the agent before it was
sent. The count is covered by `tests/tool-run.test.ts`; the danger styling is
not, and this is the one claim in this section that rests on reading the code
rather than on watching it run.

## What the harness had to learn

Three checks failed on the first full run after the change, and none of them was
a product defect. Each was the harness assuming something about the window that
a person would have fixed by looking:

1. **The bar check clicked the first bar in the document.** The transcript
   virtualises off-screen rows, so the first bar sat 4000px above the viewport
   and never became clickable. It now picks the bar nearest the middle of the
   window, scrolls it there, and addresses it by index so the click and the card
   count are about the same bar.
2. **The drag checks assumed the rail was open.** The rail can be collapsed and
   the collapse is persisted to `layout.json`, so on a machine where it had been
   closed the resize handle measured 0×0 inside a `display: none` aside and read
   as "no resize handle". `ensureRailVisible` puts it back first.
3. **Two drawer checks clicked fields below the fold.** The compaction panel
   gained rows in this phase, which pushed `compress at` past the bottom of the
   window. The click landed on nothing, the field never took focus, and the check
   reported that the setting did not round-trip. `scrollTo` brings the target
   into view first, the way a person would.

The lesson is the same one the phase keeps teaching: a harness that fails for a
reason the user would never hit is a harness that hides the failures the user
does hit.

## Verification

- `cargo test --lib` — 679 passed, 0 failed.
- `npx tsc --noEmit` — clean.
- `npx vite build` — succeeds.
- `npm run test:unit` — 56 passed (48 before, 8 for `toolRunState`).
- `node scripts/ui-probe.mjs all` — 70/70 checks, 0 console errors.
- The live state was confirmed against a real turn: `on it` with no count, a 4px
  accent dot, `pulse` animation, collapsed, exactly one live bar, settling to
  zero. The follow-up check in the `game` suite now fails if no live bar is ever
  observed, so the state cannot regress silently.

## No workarounds, and the two bugs the rule found on its first day

`AGENTS.md` now opens with a hard requirement: no workarounds. If something does
not work, say it; if something blocks you, name it; if something is not working
as intended, fix it or say it is not fixed. Before taking a detour, check that
the detour is not itself a workaround.

The rule was written because the harness kept routing around its own failures.
Applying it to the last hour of work found two real bugs immediately.

### 1. Enter did nothing in the slash palette

Typing `/status` and pressing Enter left the text in the composer and ran
nothing. The palette's Enter handler called `accept()`, which completes the
highlighted command into the draft — and for a command with no argument that
rewrites the draft to the text already there, closes the palette and swallows
the keypress. A second Enter then worked, which is why it survived: the first
press looked like a no-op rather than a bug.

The fix is `slashEnterAction` in `src/lib/commands.ts`: a command with no
`inputHint` is already complete, so Enter runs it; a command with one still
completes, because the next thing the user types is that argument. Tab always
completes and never runs. `tests/commands.test.ts` pins the decision down,
including a case over the whole catalogue so that adding an `inputHint` to a
command with no argument fails there rather than in someone's hands.

Verified in the running app: `/status` + Enter clears the composer and opens
Agent status; `/mcp` + Enter leaves `/mcp ` and waits; `/sta` + Tab completes to
`/status` without running; a plain message still sends and the agent answers.

### 2. The terminal budget was never applied to the compacted text

The Terminal panel says "Output longer than this is reduced to its head and
tail." It was not. `max_inline_chars` was only ever compared against the *raw*
output, so a rule that compacted a 40 KB log into 8 KB returned all 8 KB against
a 1200-character budget. `clamp_text_middle` existed for exactly this and nothing
called it — the function was tested, the wiring was not, which is how the gap
survived a green suite.

`select_inline_text` now clamps the compacted text to the budget before returning
it. The new test fails without the fix with the real number: *"the agent read 597
characters against a 200-character budget"*.

Verified against a real turn: a 400-line `find` over `System32` went
**14962 → 855 characters** through the proxy.

### 3. The plain-text tool result was the one result the budget could not reach

Vibe CLI offloads a very large tool result to a file before the proxy sees it and
hands the model a ~9 KB notice instead. That notice is plain text, not the exec
JSON shape the terminal rules match, so `rewrite_command` parsed it, failed, and
returned `None` — the largest result in the request was the one result the
boundary could not shrink.

`rewrite_plain` closes it. A shell result that is not the exec shape and is over
`max_inline_chars` is clamped from the middle, because both ends carry meaning:
the head names the tool and the path to read, the tail carries the end of the
preview. A zero budget still means no clamp rather than clamp-to-nothing.

Verified against a real turn: `seq 1 20000` produced a 129 KB result, the CLI
offloaded it, and the notice went **9155 → 851 characters** (`plain=1
saved=8304` in the trace). The agent then read the saved file back and reported
the correct last line, which is the part that matters — a clamp that cut the path
would have looked identical in the byte count.

`clamp_text` is gone, along with its two tests. `clamp_text_middle` is the only
clamp, and it is wired in both places that need it.

## The method is stated to the agent, not assumed

Aegis and the mastery bundles are installed on this machine and neither is
discoverable from inside a session: Aegis is a skill the agent has to route
through, and the mastery skills are not in the skill catalog at all. An agent
that does not know they exist writes the same code it would have written without
them.

Two channels carry the requirement, because one of them can be erased:

- `src/lib/preamble.ts` — the `METHOD` block, sent with the first user turn of
  every session whatever capabilities are switched on.
- `rules::METHOD_REMINDER` in the proxy's transient system prompt — rebuilt on
  every request and never entering the conversation. Compaction is the one thing
  that can fold the first user turn away, so compaction is also the thing that
  restates it. The coupling is the point.

The trace prints `method reminder present|MISSING` per request, so a session that
lost it is distinguishable from a session that ignored it.

Verified against a real turn, with a prompt that named neither: *"Add a score
readout to the top-left corner of the game window."* The agent called
`skill {name: "using-aegis"}`, then `mcp_mastery.find`, then
`mcp_mastery.load({bundle: "uiux", skill: "ui-styling"})`, and closed with
*"Route: using-aegis → ui-ux-governance ... with mastery skills uiux/ui-styling
and antislop/antislop-ui loaded."* A second turn on a read-only command reported
`Route: fast-path` with its reason, which is the other half of the requirement.

## The harness has to describe itself, or the agent fights it

Stating the method was half the job. The other half is that six capabilities
rewrite the agent's own traffic and the agent was never told. A whole-file read
came back as a card and the agent read it as a broken tool: *"the README came
back as a sparse preview, so I'll read the Controls section directly"* — then it
went around the gate with `sed`/`awk`, which is the exact cost the card exists to
remove.

`CAPABILITIES` in `src/lib/preamble.ts` names each rewrite in terms of what the
agent will observe: a whole-file read may arrive as a card and is not a failure,
paging with `offset`/`limit` is never carded, command output keeps its head and
tail around an `... omitted ...` marker, an oversized result is saved to a file
and the notice names the path, history is folded into summaries, and the graph
and web tools are MCP rather than shell.

Verified against a real turn: *"Read README.md in full and tell me exactly what
it says about the controls."* The agent's own narration was *"The whole-file read
came back as a card. Paging the Controls section for exact text"*, it paged
`README.md` at `offset: 36, limit: 8`, used no shell read at all, and closed with
`Route: fast-path ... Skills loaded: using-aegis. No mastery bundle applies to a
documentation lookup.` It also caught a real discrepancy the README has: `Ctrl-C`
quits at `skate.cpp:742` and the controls table omits it.

## The proxy was breaking the agent's tools

The proxy sits in front of the whole provider base URL, not just the chat route,
and its passthrough was `self.client.post(...)` — every request became a POST.
The CLI's connector fetch is a GET, so it came back **405**, and the app showed
*"Couldn't load your connectors: Failed to load connectors (HTTP 405). Their
tools are unavailable in this session."* That is how a proxy bug surfaced as the
agent losing the mastery tools it was being told to use.

Two fixes, both in `proxy/mod.rs`:

- `post` became `forward(method, target, …)` and keeps the caller's method. The
  target is `path_and_query`, not `path`, so a query string survives the hop.
- `accept-encoding` is dropped on the way out. The proxy reads and rewrites
  bodies, so it has to negotiate identity; forwarding the caller's
  `accept-encoding` asked upstream for gzip that this client cannot decode, and
  the bytes reached the CLI with no `content-encoding` to explain them. That
  surfaced as `Failed to load connectors: UnicodeDecodeError` once the 405 was
  gone.

Verified in the running app: the connector banner is gone and the session has its
tools.

## Two smaller ones found while testing

- **The composer footer overflowed.** The model chip is the longest thing in the
  row and a flex item will not shrink below its content, so `Stop` and `Queue`
  were pushed past the composer's right edge and clipped — present, clickable,
  invisible. The chips now carry `min-w-0` with a truncating inner span and the
  action group is `shrink-0`. Measured with the drawer open and a turn running:
  `scrollWidth === clientWidth`, `Queue` right edge 599 inside a card ending at
  610.

  That was only half of it. The `/ commands @ files` hint was gated on the
  *viewport* (`sm:flex`, 640px) while the composer is narrowed by the *drawer*,
  so a wide window with a panel open kept a hint the box could not hold. The
  shell is now a container (`@container`) and the hint is `@min-[24rem]:flex`.

  And the chips bottom out at their padding, so there is a width below which
  shrinking cannot save the row: at the window's own 940px minimum with the
  drawer open the shell is **158px**, and `Send` was clipped by 26px. `flex-wrap`
  fixes the clip but is the wrong tool — wrapping and shrinking are mutually
  exclusive on one axis, so the row broke onto a second line at 590px where
  shrinking alone fits. The row keeps `nowrap` and the labels give way below
  `17rem`: the status chip drops to its dot and `Stop`/`Queue`/`Send` drop to
  their icons, each keeping an `aria-label` so the accessible name survives the
  hidden text. The threshold is the *busy* row, not the idle one, because busy is
  the widest this row ever gets — three chips at 84px, `Stop` and `Queue` at
  146px, gaps at 18px, so a labelled row needs 248px and a compact one needs
  142px. Measuring against the idle row instead left `Stop` and `Queue` clipped
  at 218px.

  Verified across the whole reachable range — 940, 1000, 1100, 1320, 1600, each
  idle and busy, drawer open and closed, 20 cases: `overflow: 0` and
  `clipped: false` in all 20, and the composer stays one row (80px) everywhere
  except the two widths where compact mode is the point.

  **Still open, and not the composer's to fix:** at 940px with the drawer open
  the transcript column is 158px. The drawer is a fixed `w-[27rem]` (432px) and
  the rail is ~303px, so 735 of 940 is chrome. The composer now survives that
  width, but the transcript is unreadable there. The lever is the drawer's sizing
  policy — let it yield to a `min-w-` on the transcript column — and that is a
  design decision, not a bug fix, so it is recorded here rather than changed.
- **The fold check was measuring the wrong thing.** `ui-probe.mjs` counted
  `[data-vd-tool]` nodes inside a closed `.vd-expand`, but that element animates
  `grid-template-rows` from `0fr` to `1fr` rather than unmounting, so a bar
  opened once keeps its cards in the DOM at zero height. The check reported a
  leak that was not there. It now counts cards with a box and closes the bar
  first if an earlier check left it open.
- **`waitClickable` could never reach a scrolled-out target.** It polled the hit
  test for eight seconds and gave up, which is correct for an element that is
  covered or still animating and wrong for one inside a scroll container: the
  rail grows past the window as chats accumulate, and the workstation row ends up
  above the fold. `the workspace is selectable from the rail` failed on exactly
  that — `offscreen at 327,-8`. It now calls `scrollIntoView({ block: 'nearest',
  inline: 'nearest' })` once when the target is offscreen, which is what a user
  does and a no-op for every target that was already reachable.

## Verification

- `cargo test --lib` — 700 passed, 0 failed.
- `npx tsc --noEmit` — clean.
- `npx vite build` — succeeds.
- `npm run test:unit` — 72 passed (65 before, 7 for the preamble).
- `node scripts/ui-probe.mjs all` — 70/70 checks, 0 console errors.
- Composer width matrix — 20/20 (5 widths × idle/busy × drawer open/closed).
