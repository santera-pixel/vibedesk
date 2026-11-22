# VibeDesk

A desktop cockpit for the Mistral Vibe agent runtime. VibeDesk wraps the
official `vibe-acp` binary in a Tauri v2 shell and speaks the
[Agent Client Protocol](https://agentclientprotocol.com) to it over stdio, so
the UI is a real client of the same agent the CLI runs — not a reimplementation
of it.

```
┌──────────────────────────────┐        JSON-RPC 2.0        ┌──────────────┐
│  React 19 + Tailwind v4      │  ◀── newline-delimited ──▶ │  vibe-acp    │
│  zustand transcript store    │        over stdio          │  (Python)    │
└──────────────┬───────────────┘                            └──────┬───────┘
               │ Tauri IPC (invoke + events)                       │
┌──────────────▼───────────────┐                                   │
│  Rust bridge (src-tauri)     │ ──── fs/*, terminal/*, ───────────┘
│  process · correlation ·     │      session/request_permission
│  fs · terminals · approvals  │      elicitation/create
└──────────────────────────────┘
```

## Requirements

|          |                                                                 |
| -------- | --------------------------------------------------------------- |
| Node     | 20+                                                             |
| Rust     | 1.77+ (stable)                                                  |
| Vibe CLI | `uv tool install mistral-vibe` (or `pipx install mistral-vibe`) |
| WebView2 | preinstalled on Windows 11                                      |

`vibe-acp` is resolved in this order: `$VIBEDESK_ACP_BIN`, `PATH`, then the
`uv`/`pipx` install directories. A desktop launch inherits a minimal `PATH`, so
the install-directory fallback is what usually wins.

### First run

VibeDesk does not bundle the CLI — it wraps whatever `vibe-acp` is installed,
so the agent always matches the user's `~/.vibe` config, plugins and keys. That
makes "not installed" a state the app handles rather than a crash:

1. On boot, `vibe_install_status` probes for the binary and for `uv`, `pipx`
   and `python`.
2. If the binary is missing, the chat is replaced by a setup screen showing
   what was found and the exact command that would run.
3. **Install Vibe** runs it (`uv tool install mistral-vibe`, falling back to
   `pipx`, then `python -m pip install --user`). **Not now** shows the command
   to copy plus a _Check again_ button.
4. On success the agent starts without restarting the app.

The install runs with a 10-minute timeout and its combined stdout/stderr is
shown on failure. `VIBEDESK_ACP_BIN` overrides detection entirely, for installs
in unusual locations.

## Run

```bash
npm install
npm run tauri:dev      # dev server on :1420 + native window
npm run tauri:build    # bundled installer
```

`npm run build` alone runs `tsc --noEmit && vite build` — useful for checking
the frontend without a Rust toolchain.

### Launching the built app

`npm run tauri:build` produces three things:

| Artifact       | Path                                                                |
| -------------- | ------------------------------------------------------------------- |
| Standalone exe | `src-tauri/target/release/vibedesk.exe`                             |
| NSIS installer | `src-tauri/target/release/bundle/nsis/VibeDesk_0.1.0_x64-setup.exe` |
| MSI installer  | `src-tauri/target/release/bundle/msi/VibeDesk_0.1.0_x64_en-US.msi`  |

The exe is self-contained — the frontend is compiled into it, so it runs
without Node or the `dist/` folder. A copy lives at
`%LOCALAPPDATA%\Programs\VibeDesk\VibeDesk.exe`, with shortcuts on the Desktop
and in the Start Menu. Re-run the copy step after a rebuild:

```bash
cp src-tauri/target/release/vibedesk.exe "$LOCALAPPDATA/Programs/VibeDesk/VibeDesk.exe"
```

The installers are the proper path for a real install: they register the app,
add Start Menu entries and an uninstaller.

**Pass `/S` through Git Bash with `MSYS2_ARG_CONV_EXCL='*'`.** MSYS rewrites any
argument that starts with `/` into a Windows path, so `setup.exe /S` reaches NSIS
as `setup.exe C:/Program Files/Git/S` and is ignored — the installer then opens
its welcome page and waits, which looks exactly like a hang. The same applies to
`uninstall.exe /S`, which shows a _VibeDesk Uninstall_ window instead of running
silently. Both work with the variable set:

```bash
MSYS2_ARG_CONV_EXCL='*' ./VibeDesk_0.1.0_x64-setup.exe /S '/D=D:\VibeDesk'
MSYS2_ARG_CONV_EXCL='*' ./uninstall.exe /S
```

The uninstaller returns before it finishes and leaves `data\` in place, so give
it ~15 s before installing. With a previous version registered the installer
shows a maintenance page (_Add/Reinstall components_ / _Uninstall VibeDesk_)
that `/S` cannot answer, so uninstall first. The MSI has no such page and is the
better target for scripted installs.

The installer honours the directory chosen on its own directory page, so an
install pointed at `D:\VibeDesk` keeps `data\` there and the uninstall registry
entry records `InstallLocation = D:\VibeDesk`. Copying the exe by hand into
`%LOCALAPPDATA%\Programs\VibeDesk` does **not** update that entry — the two
locations then diverge, and the next installer run reinstalls to `D:` while the
hand-copied exe stays stale. Pick one location and stay with it.

## Architecture

### The bridge (`src-tauri/src/acp/`)

`bridge.rs` owns the child process and is the only place that touches the wire.

- **Transport.** One JSON object per line on stdin/stdout. Requests are
  correlated by a monotonic id; responses resolve a `oneshot` channel. Agent
  notifications are re-emitted to the webview as `acp://notification`.
- **Handshake.** `initialize` with `clientCapabilities` declaring
  `fs.readTextFile`, `fs.writeTextFile`, `terminal`, `elicitation.form` and
  `plan`. A failed handshake means the binary is not an ACP agent, and the
  status flips to `failed` with the stderr tail as the reason.
- **Agent-initiated requests.** The agent asks the _client_ to read and write
  files, run terminals, and request permission. Those are handled in Rust:
  `fs/*` maps to `tokio::fs`, `terminal/*` to a `TerminalRegistry` that pumps
  output and polls for exit, and `session/request_permission` /
  `elicitation/create` are forwarded to the UI as `acp://request` and answered
  when the user clicks.
- **Crash handling.** If stdout closes without a shutdown in progress, pending
  requests are failed, the process is reaped, and the status becomes `failed`
  with the last stderr line. `kill_on_drop` guarantees no orphan on exit.

### Sessions

`session/new` returns a `sessionId`, mode state and config options. VibeDesk
then asks `_trust/status` so the UI can say whether the folder is trusted
before the agent starts writing. `session/prompt` blocks for the whole turn
while `session/update` notifications stream in; the store applies each one to a
flat ordered transcript keyed by `messageId` / `toolCallId`.

### Reload semantics

Two different things, deliberately kept apart:

| Action             | What it does                                                                                                                                                                              |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Reload Session** | Restarts the `vibe-acp` process and opens a fresh session. Clears transient context.                                                                                                      |
| **Reload Plugins** | Re-scans `~/.vibe/plugins` and `./.vibe/plugins`, diffs against the pinned snapshot, and — if anything changed — opens a new session so the agent re-reads skills, MCP servers and hooks. |

The agent resolves plugins once, at session creation. A "hot reload" that only
re-read the directory would report success while the running turn still used
the old snapshot, so `plugins_reload` re-pins by creating a session and reports
`repinned: true` when it did.

### Plugins

`plugins.rs` discovers Agent Plugins 1.0 manifests (`plugin.json`) plus the
adapted formats Vibe also reads (`.claude-plugin/`, `.codex-plugin/`,
`.kimi.plugin.json`, `.opencode/`). Components are enumerated from the
filesystem: `skills/*/SKILL.md`, `mcp.json`, and — for native plugins —
`ai.mistral.vibe/{hooks.toml,agents,knowledge}`, `connectors.json`,
`libraries.json`. Each record carries a truncated SHA-256 of its manifest, which
is what the reload diff compares.

Toggling a plugin writes `disabled_plugins` in `~/.vibe/config.toml`. That file
belongs to the agent, so a `config.toml.bak` copy is written first and only the
one key is touched.

### Updates

`update.rs` reads the installed version from the package's own
`vibe/__init__.py`, then queries `https://pypi.org/pypi/mistral-vibe/json`.
Checks are gated to once per 24 hours (`settings.json`), with a manual refresh
that bypasses the gate. The install method is inferred from the binary path and
turned into the matching upgrade command (`uv tool upgrade mistral-vibe`,
`pipx upgrade mistral-vibe`, or `pip install --upgrade mistral-vibe`).

Applying an update stops the agent first: on Windows the running
`vibe-acp.exe` holds a lock on the file the installer needs to replace.

## Design system

`DESIGN.md` is the direction artifact: identity, dials, palette tables, dose
caps and the motion contract. This section is the summary; when the two
disagree, `DESIGN.md` wins.

The interface is built from tokens, not utility soup.

- **Type.** Instrument Sans for UI, Instrument Serif for display, JetBrains
  Mono for code — all bundled as latin woff2 subsets in
  `src/assets/fonts/`, so there is no network dependency and no Inter.
- **Colour.** Two themes (`ink` default, `paper`) driven by `data-theme` on
  `<html>`. One accent, and it is Mistral orange: `#ff7a1a` in ink (the brand
  `#ff7000` lifted for legibility on a dark ground, 6.4:1 on canvas),
  `#c2410c` in paper (5.9:1 on `#f5f3ee`). Semantic warn/danger/info appear
  only when they mean something.
- **Surfaces.** Depth comes from a three-step surface ramp (`sunken` →
  `surface` → `raised`) plus hairline borders at low alpha. No blurred orbs,
  no background grids, no blue-purple gradients.
- **Gloss.** Gloss is modelled as a light source above the object, not as
  frosted glass — `backdrop-filter` is at zero. Three devices, dose-capped:
  a specular top edge on raised surfaces (a material property, so unlimited),
  one warm sheen on the title bar, one glow on the primary action on hover.
- **Motion.** One grammar, in `src/styles/motion.css`, and five primitives in
  `src/lib/motion.ts`. Four duration tiers chosen by *what moves*, not by where
  it is used: `--vd-dur-1` 90ms for colour and opacity, `--vd-dur-2` 140ms for
  small transforms, `--vd-dur-3` 300ms for arrivals, `--vd-dur-4` 520ms for the
  whole-window reveal. `--vd-fast` and `--vd-slow` are derived from tiers 2 and
  3 through `--vd-motion-scale`, so one number scales the interface. Only
  `opacity` and `transform` ever transition — a colour transition is a paint, so
  hover states are instant. A raw millisecond value in a component is a bug.
- **Springs.** `springToLinear()` samples a damped harmonic oscillator into a CSS
  `linear()` easing, and `installSprings()` writes `--vd-spring-soft` and
  `--vd-spring-snappy` onto `:root` at boot. The alternative — pasting a
  generated curve into the stylesheet — is a second source of truth that drifts
  the first time somebody tunes a spring. The stylesheet carries a flattened
  `cubic-bezier` fallback so a frame that renders before the script still has a
  usable easing.
- **Reduced motion is a scale, not a switch.** `--vd-motion-scale` is 1, 0.5 or
  0. The three preferences are mutually exclusive by construction, so specificity
  never decides the outcome: `system` (the default) consults the OS and halves
  small transforms while keeping colour, `full` restores everything, `reduced`
  drops movement and keeps colour. Halving rather than zeroing is the point — the
  old rule collapsed every duration to 1ms, which is not reduced motion, it is no
  motion, and it is why the interface read as abrupt rather than as calm.
- **View transitions.** A panel opening and a chat switching are named view
  transitions (`vd-panel`, `vd-transcript`), not window snapshots. The root
  snapshots are neutralised through inherited custom properties while a bounded
  transition runs, because an opaque old root freezes the window on a stale
  picture.
- **Shape.** Six primitives, dose-capped: `squircle` on cards, panels, the
  composer and rail buttons; `gloss` on the composer shell; `mesh`, `blob` and
  `grain` composed on the empty-state backdrop; `gooey` on the thinking dot
  cluster, which only exists while a turn runs so the filter costs nothing at
  rest. A filter that runs at rest is a filter that runs forever.
- **Stagger.** `.vd-stagger > *` multiplies `--vd-i` by `--vd-stagger` and by the
  motion scale, so a reduced-motion user gets no delay at all. It is applied to
  the settings directory — the one list in the app that mounts once and never
  re-renders on a data tick. A stagger on a list that re-renders is N animations
  per tick for no information.
- **Components.** `.vd-card`, `.vd-btn`, `.vd-chip`, `.vd-prose`, `.vd-diff-*`
  are defined once in `src/styles/globals.css` with real hover, active, focus
  and disabled states, so a button cannot drift between panels.

## Workstations and chats

The right rail is two lists: pinned project folders on top, that folder's chats
below. A chat _is_ an ACP session, so the agent owns the storage and VibeDesk
only renders it.

| Action             | Protocol                                                                           |
| ------------------ | ---------------------------------------------------------------------------------- |
| Add a workstation  | `workstation_add` → canonicalised, then stored plain in `settings.json`            |
| Switch workstation | `agent_set_workspace` moves the bridge's cwd, then `session/list({ cwd })`         |
| New chat           | `session/new({ cwd })`                                                             |
| Open a chat        | `session/load({ cwd, sessionId })` — the agent replays history as `session/update` |

Switching a workstation does **not** restart the agent: ACP sessions carry
their own `cwd`, and the agent's tools resolve paths from the session, not the
process. Only the bridge's view of the folder moves, so the file index and
`session/fork` follow along.

Both lists have a right-click menu: a workstation offers _Open_, _New chat
here_, _Reveal in file manager_, _Copy path_ and _Remove from workstations_; a
chat offers _Open chat_, _Copy session id_, _Archive chat_ and _Delete chat_.
Right-clicking anything else does nothing, because the webview's own menu —
Back, Refresh, Save as, Print — is suppressed. See
[Context menus](#context-menus).

### A chat opens at the bottom

`session/load` has no "last N" option. `replay_session_updates` in the agent
walks the whole history and emits one `session/update` per entry, so a long chat
arrives as a burst of hundreds of Tauri events. Applying them one at a time
meant one store write and one React render per chunk, and the transcript grew
without limit — which is why opening a long chat crawled.

Three things bound it now:

- **One reduction per frame.** `enqueueUpdate` buffers notifications and
  `applyUpdates` folds the whole batch in a single `set()`. A replay costs a
  handful of renders instead of one per chunk. The buffer drains early past
  1024 updates so a hidden window cannot grow it without bound.
- **A row cap.** `MAX_TRANSCRIPT_ITEMS` is 400. `trimItems` drops from the
  front, because a chat is read from the bottom, and `trimmed` counts what went
  so the transcript can say _N earlier entries not shown_.
- **Memoised rows.** `TranscriptRow` is `memo`ised on the item object, and the
  reducer returns the same array when nothing changed, so only the row an update
  touched re-renders. The replay also opts out of the `vd-enter` mount animation
  through `.vd-replay`, since animating hundreds of rows at once costs more than
  it communicates.
- **Contained rows.** Every row is wrapped in `.vd-row`, which is
  `content-visibility: auto` with `contain-intrinsic-size: auto 80px`. A row
  scrolled out of view stops being painted and its layer is discarded; the
  remembered size means the 80px estimate is used once per row.

Replayed chunks are marked `streaming: false` because `busy` is false: history
arrives through the same `agent_message_chunk` as a live turn, and without that
every restored message kept a streaming caret.

Verified by screenshot against the installed build: opening a chat lands at the
bottom with no caret on the last message, and with the cap temporarily set to 1
the transcript showed only the newest row under a _1 earlier entry not shown_
marker.

### Archiving and deleting

Archive is a view filter; delete moves the session out of the agent's store.
Both are recorded in `<data>/config/chats.json`, which holds two id lists:

```json
{ "archived": ["…"], "deleted": ["…"] }
```

An archived chat stays on disk and stays loadable — it is hidden from the list
until the eye toggle in the Chats header reveals it, where it renders dimmed
with an _archived_ label and its menu offers _Restore from archive_. The toggle
only appears when something is archived, and its label counts what it hides
(_Show 1 archived chat_).

Delete is the destructive one, so it is built to be recoverable:

| Step    | What happens                                                                                                             |
| ------- | ------------------------------------------------------------------------------------------------------------------------ |
| Confirm | A native dialog from `@tauri-apps/plugin-dialog`, not a webview `confirm`                                                |
| Refuse  | The chat VibeDesk has open is never deleted — `chats_delete` compares against the session the agent has loaded           |
| Move    | The session directory is renamed into `<data>/trash/sessions/<id>`; a cross-volume rename falls back to copy-then-remove |
| Record  | The id is appended to `deleted`, so the row stays gone even though the agent's index may still list it                   |

The refusal is about the one session the agent is writing to, and nothing else.
It used to read the CLI's lock directory instead, which looked like the same
question and was not: a single CLI process keeps a lock in
`logs/session/active/` for _every_ session it has ever loaded, and only drops
them when it exits. One `python.exe` holding six locks is normal. Reading a lock
as "this chat is open" made every chat the user had visited undeletable, which
is what the _That chat is open_ error was reporting after a switch. The lock
directory is now left alone; see defect 30 in the ledger.

Nothing is ever `rm -rf`'d. The trash keeps `chunks/`, `generations/` and
`checkpoints/` intact, so a deleted chat can be restored by hand by moving the
directory back into `~/.vibe/logs/session/unified/` and removing its id from
`deleted`. Ids are validated against a path-traversal guard before they are used
to build a path, so a crafted id cannot escape the trash directory.

Verified against the installed build: archiving dropped the list 19 → 18 with a
_Chat archived_ toast and wrote `chats.json`; the toggle brought the row back
dimmed; restoring returned it to 19 and emptied `archived`; deleting a chat
moved 105K of session data into `trash/sessions/` and left a tombstone.

### Paths are stored plain

`std::fs::canonicalize` returns `\\?\D:\project` on Windows. That form is right
for the filesystem and wrong for everything else: it is the key a workstation is
compared by, it is what the agent is told the cwd is, and it is what the title
bar shows. The agent keys its session history on the plain form, so a verbatim
cwd never matched its own chats and the rail read _No chats in this folder yet_
with ten sessions on disk. `paths::strip_verbatim` drops the prefix on the way
in, `settings::load` repairs a file written by an older build, and the frontend
`samePath` strips it too so either form matches.

## Billing and usage

The Billing panel (rail, or `/billing`) answers two questions that look alike
and are not:

| Ledger           | Source                             | Scope                                   |
| ---------------- | ---------------------------------- | --------------------------------------- |
| **Subscription** | `_account/read` → `plan`, `budget` | The whole account, shared by every chat |
| **API usage**    | `usage_update._meta`               | The open chat only                      |

The allowance percentage is the server's own figure. VibeDesk never derives it,
because only the server knows how big the pool is — a client-side guess would be
a confident lie. `billing.rs` parses the payload defensively: `_account/read` is
an ACP extension, not published protocol, so every field is optional and a
missing one renders as _not reported_ rather than as zero. Zero would read as
"nothing spent", which is a different claim from "the agent did not say".

The panel shows the plan title and tier, the allowance bar with a countdown to
its reset, whether pay-as-you-go is on, the masked key with its scope and access
level, the auth state, and the signed-in identity. The key is only ever the
masked preview the agent reports — the full value never crosses the bridge and
VibeDesk does not store it.

The token half is live from the store, so it updates as a turn streams without
another round trip: prompt, completion, cached and total tokens, the cache hit
rate, cost, steps, and the last turn's size.

### Cache hits

`usage_update` carries a `_meta` block that the context meter used to discard:

```json
{
  "steps": 2,
  "promptTokens": 9101,
  "completionTokens": 3,
  "cachedTokens": 3456,
  "totalTokens": 9104
}
```

`cachedTokens / promptTokens` is the hit rate, and it is the number that explains
a bill: cached prompt tokens are billed at a fraction of the full input rate. The
Context panel's **Cache** section shows the rate as a bar, then cached, fresh,
prompt and completion counts, and says in words how many tokens were actually
billed at full rate.

Two details that matter:

- **`_meta` is absent before the first model call.** A bare `usage_update` would
  otherwise wipe the ledger, so the reducer keeps the previous `_meta` when the
  new one is missing. The same applies to `PromptResponse.usage`, which carries
  no `_meta` at all — `setUsage` there preserves what the notifications already
  delivered.
- **The reset date is in the future.** `relativeTime` measures backwards, so it
  rendered a reset as _just now_. The panel counts forwards instead: _resets in
  15 days_.

Verified against the installed build with a real turn: the panel read _Pro ·
INDIVIDUAL · chat · ready_, 8.7% of the allowance used, _resets in 15 days_,
`****U4Ut` scoped to `vibe`, and 10k prompt / 3 completion / 3.5k cached tokens
at a 35% hit rate for $0.0020. The Context panel showed the same 35% with 6.6k
fresh tokens. No console errors, no overflow.

## Context menus

WebView2 shows Chromium's context menu on any right-click no handler claims.
That menu is about a browser, not about this app, so `installContextMenuGuard`
suppresses it in the capture phase and only calls `preventDefault` — a
component's own `onContextMenu` still fires afterwards and opens a real menu.

Two exceptions are deliberate:

- **Editable fields keep it.** Native Copy / Paste / Select all in the composer
  is worth more than the consistency of removing it.
- **Shift + right-click keeps it anywhere**, which is the escape hatch for
  Inspect Element in a release build.

Menus are rendered into `document.body` through `ContextMenu`, so an
`overflow: hidden` ancestor cannot clip them, and they close on the next click,
on Escape and on resize.

## Runtime customization

The rule is **data, never code**. A theme is a map of CSS custom properties plus
declaration blocks for the app's own classes; nothing in it can execute, import
or fetch. That is what makes it safe to accept a theme from a plugin or from the
agent itself.

`<data>/config/theme.json`:

```json
{
  "tokens": { "--vd-accent": "#ff6b35", "--radius-md": "10px" },
  "rules": [{ "selector": ".vd-btn-primary", "declarations": { "letter-spacing": "0.02em" } }]
}
```

Save the file and the window updates — no restart. `customize.rs` polls
(mtime, length) every 400 ms rather than using a filesystem watcher, because
editors save by writing a temp file and renaming it, which fires several events
for one logical change.

### What validation rejects

| Rejected                                                  | Why                                      |
| --------------------------------------------------------- | ---------------------------------------- |
| `url()`, `@import`                                        | Turns a stylesheet into a network client |
| `expression()`, `javascript:`, `behavior:`                | Script execution                         |
| `position`, `z-index`, `pointer-events`, `content`        | Can cover or redraw the interface        |
| Selectors outside `.vd-*`, `:root`, `[data-theme`, `html` | Keeps a theme inside the app's namespace |
| **Protected surfaces**                                    | See below                                |

### Protected surfaces

The approval bar, the elicitation form, the status bar and the composer carry
`data-vd-protected` and can never be targeted by any config, from any source.
A theme that could restyle them could make the UI lie about what the agent is
asking — hide an approval, or make a destructive button look like a cancel.

This matters most for plugin- and agent-authored themes: an LLM that can restyle
the interface can be talked into hiding the controls the user needs to judge it.

### Failure behaviour

- Invalid JSON or a rejected value: the last good theme stays applied and the
  reason appears as a notice. A typo never wipes the UI.
- `VIBEDESK_SAFE_MODE=1` ignores every user, plugin and agent customization.
  The escape hatch when a config makes the window unusable.
- Writes go through a temp file and a rename, so a crash mid-write cannot leave
  a half-written theme for the watcher to apply.

### Layout

`<data>/config/layout.json` moves the geometry. It is narrower than a theme on
purpose: **integers and closed enums only**, no CSS strings at all, so there is
nothing in it that could become a declaration.

```json
{
  "railWidth": 48,
  "sidebarWidth": 252,
  "sidebarSide": "right",
  "sidebarCollapsed": false,
  "transcriptMax": 720,
  "composerMax": 720,
  "density": "comfortable"
}
```

| Field              | Range                                 | Notes                                                 |
| ------------------ | ------------------------------------- | ----------------------------------------------------- |
| `railWidth`        | 40–72                                 | Icon rail                                             |
| `sidebarWidth`     | 200–520                               | Workstation rail; also set by dragging its inner edge |
| `sidebarSide`      | `left` \| `right`                     | Which edge the rail sits on                           |
| `sidebarCollapsed` | boolean                               | Toggled by the button at the bottom of the icon rail  |
| `transcriptMax`    | 480–1100                              | Widest the transcript column grows                    |
| `composerMax`      | 480–1100                              | Widest the composer grows                             |
| `density`          | `compact` \| `comfortable` \| `roomy` | Multiplies vertical rhythm (0.82 / 1 / 1.22)          |

Numbers are **clamped, not rejected** — a layout can look wrong but can never
make the window unusable. Clamping is silent because the UI reads the effective
values back: a slider that asked for 900 px shows 520 px. An unknown enum value
is a parse error, so the last good layout stays applied.

The mechanism is one step: `applyLayout` writes the values as inline custom
properties on `<html>`, which outrank the `:root` defaults in `globals.css`.
Nothing re-renders. A drag writes the property directly on every pointer move and
only persists on release, so the rail tracks the pointer at frame rate.

### Actions

`<data>/config/actions.json` adds buttons. Each one is a label plus exactly one
of four things it can do.

```json
{
  "actions": [
    {
      "id": "review",
      "label": "Review diff",
      "icon": "git-branch",
      "slot": "composer",
      "hint": "Ask the agent to review the working tree",
      "run": { "kind": "prompt", "text": "Review the current working tree diff." }
    },
    {
      "id": "reload-plugins",
      "label": "Reload plugins",
      "icon": "refresh",
      "slot": "composer",
      "run": { "kind": "command", "command": "reload-plugins" }
    }
  ]
}
```

`run.kind` is a **closed set** — a tagged enum, not a string:

| `kind`     | Payload   | What happens                                                           |
| ---------- | --------- | ---------------------------------------------------------------------- |
| `prompt`   | `text`    | Sent to the agent exactly as if typed                                  |
| `command`  | `command` | Runs one of VibeDesk's own commands                                    |
| `open-url` | `url`     | Opens in the browser; must be `http(s)`                                |
| `reveal`   | `path`    | Reveals in the file manager; must be relative and inside the workspace |

`command` is the field that could have gone wrong. A button that ran an arbitrary
shell line would turn `actions.json` into a script host, and the whole point of
validating a theme is lost if the file next to it can execute. So `command` names
one of fourteen built-ins (`reload-session`, `reload-plugins`, `new-chat`,
`toggle-theme`, `toggle-sidebar`, `focus-composer`, `check-updates`,
`open-plugins`, `open-config`, `open-sessions`, `open-context`, `open-skills`,
`open-customize`, `open-help`). A user who wants a shell command has the agent
for that, with the approval prompt in between — which is the boundary this
module refuses to move.

`slot` is `composer` (the strip above the input) or `rail` (below the built-in
panel icons). `icon` is one of twenty names; a file cannot smuggle in a component
path. Limits: 24 actions, 16 KB, ids unique and `[a-z0-9-]`.

### Panels

`<data>/config/panels.json` adds a right-hand panel. A panel is **sources + a
data model + a layout tree**, and a block is a leaf of that tree: data, or a view
over data.

That is the v2 model, and it replaced "a panel is a list of blocks" for three
reasons. A block that contains blocks is a layout node, not a block — so nesting
is a tree, not a kind. Data fetched per block duplicates requests and makes a
panel's state incoherent — so one model per panel, fed by declared sources.
And a string template cannot be validated or typed — so bindings are path
objects.

```json
{
  "panels": [
    {
      "id": "ci",
      "title": "CI",
      "icon": "git-branch",
      "width": 340,
      "sources": [
        { "id": "build", "type": "builtin", "name": "status" },
        { "id": "queue", "type": "http", "url": "http://127.0.0.1:8787/ci", "intervalMs": 5000 }
      ],
      "layout": {
        "node": "tabs",
        "tabs": [
          {
            "label": "Now",
            "children": [
              { "kind": "data", "label": "Queue depth", "value": { "@path": "/queue/depth" } },
              {
                "kind": "progress",
                "label": "Build",
                "value": { "@path": "/build/percent" },
                "max": 100
              }
            ]
          },
          {
            "label": "History",
            "children": [
              {
                "kind": "chart",
                "label": "Depth",
                "style": "sparkline",
                "values": { "@path": "/queue/history" }
              }
            ]
          }
        ]
      }
    }
  ]
}
```

**Twelve block kinds.** Six are literal — `text`, `file`, `list`, `link`,
`actions`, `sandbox`. Five are bound — `data`, `chart`, `diagram`, `table`,
`progress` — and carry an expression rather than a value. One is a document —
`widget` — and runs in a frame with a declared set of capabilities. `PanelBlock::KINDS`
is the list, and the manifest publishes it, so the agent never has to guess.

**A `diagram` is a graph, not a picture of one.** Four layouts, each a real
algorithm: `layered` ranks by longest path and orders each rank by barycentre,
`radial` finds the cycle and closes it into a ring, `tree` is a tidy tree with
children centred under their parent, `sequence` is lifelines and messages. Every
visible field is an expression, so a node turns red when a source says the
service is down and an edge's label shows the live latency. Hovering a node
lights its edges; clicking one fires an action from `actions.json`, which makes
the diagram a control surface rather than a screenshot. A `sequence` diagram
sizes its columns to the panel and truncates the labels to fit, because a
participant past the right edge is a participant the reader cannot see.

**A `chart` answers a question, not just a shape.** Beyond axes, ticks,
gridlines and a hover readout: `stack: "stacked"` accumulates the series so the
top is the total, `stack: "percent"` normalises each column to 100, `scale:
"log"` makes each decade the same height and breaks the line at a non-positive
point rather than drawing it at minus infinity, and `reference` draws up to four
horizontal thresholds whose value and tone are expressions — a budget that moves
when the budget moves. Clicking a legend entry hides that series and rescales the
axis to what is left. `stack` on a line is refused, because a line has no
thickness to accumulate.

**An action can unblock a paused agent.** `prompt` sends text to a *running*
agent; `approve`, `reject` and `answer` answer the two states where it is
*stopped and waiting* — a permission request and an elicitation. The permission
option is chosen by kind rather than by id, because the ids belong to the agent
and `allow_once` is the one thing every ACP agent agrees on. A request that
offers no option of the requested kind is answered with a plain reject rather
than left hanging.

```json
{
  "kind": "diagram",
  "label": "Request path",
  "layout": "layered",
  "nodes": [
    { "id": "client", "label": "Client", "detail": "browser", "tone": "accent" },
    { "id": "edge", "label": "Edge", "detail": { "@path": "/cdn/region" } },
    {
      "id": "api",
      "label": "API",
      "tone": { "@if": [{ "@gt": [{ "@path": "/api/errors" }, 0] }, "danger", "ok"] },
      "action": "restart-api"
    }
  ],
  "edges": [
    { "id": "e1", "from": "client", "to": "edge", "label": "GET /" },
    {
      "id": "e2",
      "from": "edge",
      "to": "api",
      "label": { "@format": "duration", "@of": { "@path": "/cdn/latency" } }
    }
  ],
  "height": 260
}
```

`tone` is a closed set — `neutral`, `accent`, `ok`, `warn`, `danger`, `muted` —
mapped to the app's own tokens, so a `danger` node is the same red as a failed
tool call in both themes. `routing` is `orthogonal` (elbows through the gutter
between ranks, which is empty by construction) or `curved`. A node's `when`
removes it and every edge touching it. Limits: 40 nodes, 80 edges, height
120–520.

**Three layout node kinds.** `stack` is a column, `split` is a row with optional
`weights`, `tabs` is one child visible at a time. A node's `children` hold nodes
_or_ blocks at any level, so a stack may contain a block directly. Nesting is
capped at 4 and the whole tree at 60 nodes and blocks together, because a panel
is a sidebar and not a window manager.

**`when` is on every node and every block.** A guarded node hides its whole
subtree, which is what lets a panel guard a group instead of repeating the same
condition on each leaf. A condition is true only for a real `true`: a binding
that cannot be evaluated hides its block, because showing a block whose guard is
unknown is the failure mode that lies.

**A `file` block carries a path and nothing else.** No content, no cache, no
eager read. The frontend renders it as a stub reading `right-click to load`, and
the file is read only after an explicit right-click, from a menu offering _Load
file_, _Reveal in file manager_ and _Copy path_. There is deliberately **no**
`expanded` or `autoLoad` field: a config cannot opt itself into reading the
workspace on open, because that would defeat the point of the panel being
confined until needed. Unknown fields are dropped by serde, so adding one to the
file does nothing — there is a test for exactly that.

Paths must be relative and stay inside the workspace; `..`, absolute paths and
drive letters are rejected. Widths are clamped to 240–520 px so a panel cannot
push the transcript off the screen. Limits: 12 panels, 40 blocks and 60 nodes per
panel, 8 sources per panel, 50 list items, 32 KB.

Both files hot-reload on the same 400 ms poll as the theme. A rejected file
leaves the last good config applied and shows the reason as a notice.

#### Sources and bindings

A source is a declared input. `builtin` reads VibeDesk's own state — `status`,
`usage`, `session`, `chats`, `plugins`, `clock` — with no network, no approval
friction and no risk. `http` is fetched in Rust and may only address loopback;
see [Sources: plugins and the agent](#sources-plugins-and-the-agent) and
`data.rs` for why that rule is the load-bearing one.

A binding is a path object, not a string template:

```json
{ "@path": "/queue/depth" }
{ "@gt": [{ "@path": "/queue/depth" }, 10] }
{ "@format": "duration", "@of": { "@path": "/build/seconds" } }
```

The grammar is closed and non-Turing-complete: 16 operators, no loops, no
function calls. `src-tauri/src/expr.rs` validates it at approval time and
`src/lib/data.ts` evaluates it at render time, and both are tested against the
same table in `src/lib/expr-vectors.json` so the two cannot drift silently.
Evaluation is total — every input produces a value or `MISSING`, never a throw,
because a renderer that throws on a bad binding takes the window down.

A source that has not answered yet is absent from the model, so `@exists` is
false and a `data` block shows a dash rather than a zero. That distinction is the
whole reason `MISSING` exists.

#### The `sandbox` block

The one block kind whose content is markup rather than data, so it is the only
one that needs a boundary:

```json
{
  "kind": "sandbox",
  "title": "Build card",
  "height": 150,
  "html": "<div style=\"padding:12px\"><div style=\"font-size:26px;color:var(--vd-accent)\">3.71 MiB</div></div>"
}
```

It renders in an iframe with `sandbox=""` — the strictest value: no scripts, no
forms, no popups, no top navigation, no same-origin. The document gets an opaque
origin, so it cannot reach `parent`, cannot read the app's cookies or storage,
and cannot navigate the window it sits in. A CSP is injected into the document as
well (`default-src 'none'`, inline style only, `data:` images only), and Rust
refuses the common mistakes at write time so the agent gets a message in its
receipt instead of a blank box:

| Refused                           | Why                            |
| --------------------------------- | ------------------------------ |
| `http://`, `https://`             | no network, from any attribute |
| `<script src`, `<link`, `@import` | no external code or styles     |
| `<iframe`, `<object`, `<embed`    | no nested frames               |
| `<form`, `javascript:`            | no submission, no script URLs  |

The palette is read from the live theme and injected into the frame, so a widget
follows ink/paper without knowing which is active. Height is clamped to 80–480 px
and the markup to 16 KiB.

**There are no scripts, and that is a platform fact rather than a choice.** A
`srcdoc`, `blob:` and `data:` document all inherit the _parent's_ CSP in
Chromium — all three were tested against this build — and the app's policy is
`script-src 'self'`. A frame cannot be given a looser policy than the app without
loosening the app, and `'unsafe-inline'` on the whole app to enable a widget is
not a trade worth making. What is left — styled cards, tables, SVG, CSS animation
— is what a panel needs. A block that has to _do_ something uses the `widget`
kind instead, which is served from its own origin with its own CSP and reaches
the app only through the capabilities the panel declared in a file the user can
read.

### Sources: plugins and the agent

The four files above are the user's own. Two other parties can propose a change,
and neither is allowed to apply one.

**Plugins.** A plugin may ship a `vibedesk/` folder next to its manifest holding
any of `theme.json`, `layout.json`, `actions.json`, `panels.json`. Installing the
plugin _lists_ those configs in the Customize drawer's Sources section; it never
applies them. A plugin that could restyle the app by being installed would be a
supply-chain hole, and the user would have no way to tell which plugin did it.
A config that would be rejected is still listed, with the reason and a disabled
Apply button, so a broken plugin is visible rather than silently absent.

**The agent.** ACP has no client-registered tools, so the agent cannot call into
VibeDesk. Its only channel is the filesystem: it writes a proposal into
`<configDir>/pending/`, using the same four file names. VibeDesk notices it on
the next watcher tick, validates it, and shows a bar above the composer with what
it would do and Apply / Discard buttons.

The two gates are deliberately separate. The agent's file-write permission is
about touching the filesystem; this approval is about changing the app's own
chrome. Both must pass, which is why a proposal can sit in the bar while the
agent was perfectly free to have written the file.

Three properties make this safe rather than decorative:

- **What lands is the normalised body, not the raw bytes.** Parse, validate,
  re-serialise, write. Anything serde ignored never reaches disk, so a proposal
  cannot smuggle in a field the schema does not have — including the `expanded`
  flag a file block deliberately lacks.
- **The digest is a TOCTOU guard.** The bar shows a proposal and the user clicks
  a moment later. In between, the agent — a process that writes files and does
  not have to wait — can rewrite it. `apply` takes the sha256 the UI displayed
  and refuses a mismatch, so the user cannot approve one thing and get another.
- **The frontend never supplies a path.** For a plugin it names a plugin and a
  kind; the backend re-discovers. The webview cannot hand in an arbitrary file to
  be read and promoted into the live config.

Neither source gets a shortcut through validation. `normalise` runs the same
`validate` the user's own files go through, so protected surfaces, value escapes,
closed enums and the no-eager-read rule apply identically. There is no "trusted"
flag and no source that skips the check. `VIBEDESK_SAFE_MODE=1` disables both.

The approval bar carries `data-vd-protected="approval"` for the same reason the
permission prompt does: a theme that could hide the bar could approve itself.

### Third-party CSS

There is no CSS dependency and there will not be one. Component galleries such as
[uiverse.io](https://uiverse.io) — MIT-licensed, mirrored as
[`uiverse-io/galaxy`](https://github.com/uiverse-io/galaxy) — are copy-paste
snippet collections, not packages, so they cannot be installed. Their signature
style (gradients, glows, neumorphism) is also the opposite of what this interface
is for.

The rule: a pattern from a gallery may be **ported by hand into a `.vd-*` class**
in `globals.css`, where it inherits both themes and every layout token. It may
never be injected as raw HTML or CSS, from any source — that is exactly the
escape the validation in `customize.rs` exists to close.

## Where VibeDesk stores things

`paths::app_data_dir()` resolves in this order:

1. `VIBEDESK_DATA_DIR`, for tests.
2. `<folder containing VibeDesk.exe>/data`, when that folder is writable.
3. The per-user platform directory, for read-only installs such as
   `C:\Program Files`.

So an install on `D:` keeps `settings.json` and the WebView2 cache on `D:`
(`data/settings.json`, `data/webview2/`). The agent's own `~/.vibe` is left
alone on purpose: it is shared with the CLI, and redirecting it would hide the
user's existing config and API keys. The Status panel shows both paths.

Everything VibeDesk writes lives under that one directory:

```
data/
├── settings.json          workstations, theme, layout, creator mode
├── config/chats.json      archived and deleted chat ids
├── trash/sessions/<id>/   deleted sessions, kept whole
└── webview2/              the webview's own cache
```

## Telling the agent it is inside VibeDesk

ACP has no field for a client-supplied system prompt. `clientInfo` reaches the
agent, but `vibe/acp/agent.py` only feeds it to telemetry — it never reaches
`SessionOptions`. So the first prompt of each session carries a one-paragraph
preamble (`src/lib/preamble.ts`) describing the chat UI, the absence of a
terminal, and the preference for fenced code and Mermaid blocks.

It is appended _after_ the user's text, because the agent derives a session
title from the start of the first user message — a leading preamble would title
every chat with the same paragraph. Replayed history is stripped back out so a
resumed chat shows only what the user typed.

The preamble also carries the customization contract: the absolute path of the
manifest and of the pending directory, the four file names, and the rule that
writing a proposal is not applying one. Without it the agent has no way to know
the channel exists, and would either claim it changed the theme when it only
wrote a file, or not try at all. The paths are passed in rather than hardcoded
because they live under the app's data directory, which moves with the install.

## The manifest and the receipt

Two files close the loop between the agent and the customization surface. Both
live in `<data>/config/`.

The preamble used to recite every schema in prose — 2 847 characters on the first
turn of every session — and still left the agent guessing, because it could not
see the current values or the valid names. It is 1 640 characters now and says
"read the manifest" instead, which is both shorter and strictly more
informative.

### `vibedesk.manifest.json` — what the agent may change

Written on boot and after every config change, from the same loaders the UI
uses. It carries the current state of all four surfaces plus the vocabulary each
one accepts: the 20 icon names, the 14 built-in commands, the 5 panel block
kinds, the layout clamps, the protected selectors, and the 36 theme tokens with
their built-in values.

The property that matters is that **every list is derived from the thing that
validates it**, never restated:

| List                     | Derived from                               |
| ------------------------ | ------------------------------------------ |
| icons                    | `ActionIcon::ALL`                          |
| commands                 | `BuiltinCommand::ALL`                      |
| slots                    | `ActionSlot::ALL`                          |
| block kinds              | `PanelBlock::KINDS`                        |
| layout fields and bounds | the `layout::*` clamp tuples               |
| theme tokens             | `include_str!` of `src/styles/globals.css` |
| protected selectors      | `customize::PROTECTED_SELECTORS`           |

So adding an icon to the enum makes it appear in the manifest with no second
edit, and a token added to the stylesheet appears the same way. Tests fail if a
variant is added without being listed in its `ALL`, and if the token vocabulary
stops containing `--vd-accent`.

The token list is the **vocabulary**, not the user's overrides — `current` holds
those. This distinction is load-bearing: the first version published the
overrides, so on a fresh install the list was empty, and the agent correctly
refused to propose anything because it had been told nothing was customizable.
The five tokens owned by `layout.json` are excluded, because `applyLayout` writes
the same names inline on `<html>` and would silently outrank a theme value.

### `<kind>.result.json` — what happened to a proposal

Written next to the proposal after a decision, and read by the agent on its next
turn:

```json
{ "kind": "theme", "applied": true, "at": "…", "digest": "…", "summary": "1 token", "error": null }
```

Three write sites: apply (`applied: true`), discard (`applied: false`, error
`discarded by the user`), and — the one that used to be invisible — a file that
fails validation, which carries the validator's own message:

```json
{ "applied": false, "error": "theme.json: --vd-accent: `url(` is not allowed in a value" }
```

Receipts are collected _after_ the proposal loop, not before. A file that fails
validation writes its receipt during that loop, and the pending directory is only
re-read when a proposal file's stamp moves — so reading receipts first would mean
the UI never sees the receipt for the very failure it just reported.

The digest makes the write idempotent. The directory is polled every 400 ms and
an invalid file stays invalid, so without it the same failure receipt would be
rewritten on every tick and the agent would see a fresh timestamp for a decision
the user made once. Receipts are also excluded from the stray-name check, or the
approval bar would show four permanent issues.

The approval bar prints the outcome as _The agent has been told: theme refused_,
so the user sees exactly what the agent is about to read.

### The diff and the preview

A proposal used to arrive as a count — _Theme · 7 tokens_. That is not a decision
anyone can make. The bar now shows the fields:

```
Theme   7 tokens                                    theme.json
  --vd-accent    #ff7a1a → #ff7414
  --vd-line      — → #2a2622
  --vd-radius    #0.5rem → 0.375rem
  show 4 more
```

`Change { field, from, to }` is computed in Rust (`sources::diff`) and travels
with the proposal, so the UI never re-derives it. Four rows show by default; the
rest fold behind _show N more_, because a bar that pushes the transcript off
screen is a bar people dismiss without reading.

The `from` column has two sources. When the live file exists, it is read from
disk. When it does not, the app is running the built-in defaults — and for
`layout`, `actions` and `panels` those defaults are synthesised so the column is
still true. **Theme is the exception**: a `theme.json` merges over the
stylesheet, so an absent file overrides nothing and every token is genuinely an
addition. Synthesising the full built-in vocabulary there would report 23 phantom
removals for a one-token proposal. The UI fills that blank cell from the live
computed value instead, snapshotted once when the proposal appears so a preview
cannot rewrite the column it is being compared against.

**Preview** wears a change without writing it. Only `theme` and `layout` offer it,
because those are the two surfaces that are pure CSS variables — an `actions` or
`panels` change has no honest preview short of building the thing. The previewed
config is reconstructed by merging the diff into the live config, so a _removed_
token correctly falls back to the built-in value rather than to nothing.

The header switches to _previewing — nothing is written_, and Escape leaves the
preview. Escape is scoped to the bar and only fires while a preview is active: it
is deliberately not a global shortcut, and deliberately not Escape-to-discard,
which would throw away the agent's work on a stray keypress. Every exit path —
toggle, apply, discard, unmount — restores the live config, because a preview
that outlives its proposal is the app showing a config that does not exist.

## Creator mode

The contract above says _you may propose, you cannot apply_. That is enough to
stop an agent from lying about a change, but not enough to make it useful: asked
to "make the rail narrower", an agent reading only that contract answers with
instructions for the user to follow by hand. Creator mode adds the missing half —
that VibeDesk itself is the subject of the work.

It is one boolean, and it moves three things at once:

| Surface         | Effect                                                                                 |
| --------------- | -------------------------------------------------------------------------------------- |
| `settings.json` | `creatorMode` — persisted, so the mode survives a relaunch                             |
| The preamble    | a builder paragraph appended to the contract on the next first turn                    |
| The chrome      | the `creator` chip lights up, the Customize panel pins open, a standing banner appears |

The banner is deliberately not dismissible. A mode you can hide is a mode you
forget you are in, and the agent behaves differently in it — so the exit is a real
button, not a second click on the chip.

Toggling mid-session does not spend a turn announcing itself. The contract rides
on the first user turn, so a session that already sent one is running on the old
text; the toggle clears `preambleSent` and the _next_ prompt carries the new
contract. That is cheaper and quieter than a message whose only content is "you
are now in creator mode".

### Apply and tell

The receipt is the agent's only feedback channel, but it has to _read_ it, which
normally costs a turn the user has to ask for. **Apply + tell** closes the loop in
one click: it applies the proposal, re-reads the pending directory, and sends the
receipt back as a follow-up prompt.

```
apply → read receipt → sendPrompt("I applied the layout change. Receipt: {…}. Read it and continue.")
```

The turn is not awaited. `sendPrompt` owns its own errors and its own busy flag,
and holding the button disabled for the length of a model turn would make the bar
look stuck.

A live run of the whole loop, in creator mode, on a fresh session:

1. _"make the left rail a little narrower (44px instead of 48) and warm the accent
   up slightly"_
2. Agent read the manifest, then: _"Two changes, two surfaces — I'll propose the
   rail width first (layout), then the accent (theme) once this one is decided."_
3. `pending/layout.json` written; _"it's waiting for your approval, not applied."_
4. **Apply + tell** → `layout.json` on disk with `railWidth: 44`, receipt
   `{"applied":true,"summary":"rail 44px, sidebar 252px right, comfortable"}`.
5. Agent: _"Layout receipt confirms applied: rail 44px… Now the accent — proposing
   the theme surface."_ → `pending/theme.json` with six accent tokens.
6. **Apply** → `--vd-accent` repaints `#ff7a1a` → `#ff7414`.

## Slash commands and mentions

Typing `/` opens the command palette. Commands are split by transport:

- **agent** — sent as prompt text and handled by the agent itself (`/reload`,
  `/compact`, `/mcp`, `/proxy-setup`, `/retry`, `/log`, `/data-retention`,
  `/leanstall`, `/unleanstall`, plus installed skills).
- **local** — handled by VibeDesk (`/config`, `/plugins`, `/reload-plugins`,
  `/model`, `/thinking`, `/theme`, `/clear`, `/resume`, `/branch`, `/rename`,
  `/context`, `/status`, `/billing`, `/skills`, `/release-notes`, `/help`,
  `/whoami`, and the hidden `/exit`). `/plugins` and `/reload-plugins` are
  TUI-only in the CLI, so the desktop app implements them against the same
  directories.

`SLASH_COMMANDS` in `src/lib/commands.ts` is the list; this paragraph is a
summary of it, not a second source of truth. Aliases (`/new`, `/continue`,
`/fork`, `/usage`, `/cost`, `/whatsnew`) resolve through `findCommand`.

`@` opens a file palette backed by a bounded workspace walk in Rust
(`workspace_files`), skipping `node_modules`, `.git`, `target` and friends.

### Palette behaviour

The agent reports its own command list, and it overlaps the built-ins almost
entirely. `filterCommands` keeps the curated entry and appends only names the
agent knows that VibeDesk does not, so a command appears once. Rows are keyed by
`transport:name` — keying by name alone let React reconcile two rows onto one
node and the list drifted out of sync with the query.

Matching is name-first, then description: `/th` puts `/theme` and `/thinking` at
the top and follows them with commands whose description contains "th". Escape
closes the palette whether or not there are matches, and blurring the composer
closes it too. The popup calls `preventDefault` on `mousedown` so clicking a row
does not blur the textarea before the click lands.

## Debugging the running app

The release build is a WebView2 host, so the DOM can be driven directly instead
of guessing at pixel coordinates:

```bash
powershell.exe -NoProfile -Command "\$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9222'; Start-Process -FilePath 'D:\VibeDesk\vibedesk.exe'"
curl -s http://127.0.0.1:9222/json/list
```

A debug build has a supported switch for the same thing, so the arguments do not
have to be assembled by hand:

```bash
VIBEDESK_REMOTE_DEBUGGING=9222 npx tauri dev
```

`VIBEDESK_REMOTE_DEBUGGING` names the port and is honoured **only in a debug
build**. A shipped app that can be inspected is a shipped app that can be driven,
so the release binary ignores it entirely. Port `0` is refused, and an existing
`--remote-debugging-port` in `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is left
alone.

### Live verification

`scripts/live-check.mjs` attaches to that endpoint and runs assertions against
the real DOM. It has no dependencies — Node 24 ships `fetch`, `WebSocket` and
`node:http`.

```bash
node scripts/live-check.mjs all              # every suite, exit 1 on failure
node scripts/live-check.mjs panels           # one suite
node scripts/live-check.mjs eval "document.title"
node scripts/live-check.mjs shot out.png
node scripts/live-check.mjs probe            # the CDP version and target list
```

Six suites, 41 checks:

| Suite     | Checks | What it proves                                                                                                                                                                                                                                                             |
| --------- | ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `smoke`   | 8      | The running bundle is the one in `dist`, the app mounts, a motion policy and a resolvable duration token are on the document, the rail renders, nothing overflows, a debugging endpoint is announced when it is on, and no console error or uncaught exception was logged. |
| `panels`  | 14     | Nested tabs, one selected and one tabbable, `aria-controls` on every tab, a built-in source advancing without a reload, an integer staying whole, selection surviving a data tick, a `when` hiding its block, split weights, ArrowRight/End/Escape.                        |
| `data`    | 7      | A declared `http` source asks before it fetches, nothing is fetched before consent, the badge says "needs permission", the fetch starts on Allow, it keeps polling, and the badge turns live.                                                                              |
| `plugins` | 3      | Enabling a plugin adds its panel to the rail, the panel is marked with its plugin name, and disabling removes it.                                                                                                                                                          |
| `motion`  | 4      | Full sets the attribute and restores the scale, Reduced halves it, System follows the OS.                                                                                                                                                                                  |
| `chats`   | 5      | A chat the CLI holds a lock for but is not showing can be deleted, the chat just switched to cannot, the chat switched away from can, and the switch leaves exactly one chat open.                                                                                         |

Each suite installs what it needs — a `panels.json`, a plugin folder, a loopback
server — and removes it again in a `finally`, so a failing check still leaves the
machine as it was. A suite that throws is reported as one failed check rather
than a stack trace, and a CDP call that never answers fails after 30s instead of
hanging the run, so a window that dies mid-run reads as a named failure and the
other suites still report. `--data-dir` points them at a different app data
directory, `--json` emits machine-readable results, and `--keep` skips the
cleanup when you want to look at what a suite built.

This is the check that catches the class of defect a unit test cannot: a
renderer that throws on unexpected data, a value that stops updating, a setting
that does not reach the document, a command that is written and never wired.

For a full agent-driven session, Google's `chrome-devtools-mcp` attaches to the
same endpoint and brings console, network, performance and screenshot tools:

```bash
npx chrome-devtools-mcp@1.13.0 --browserUrl http://127.0.0.1:9222
```

`--browserUrl` is the flag that connects to a running instance instead of
launching its own Chrome. The package documents Chrome as its only supported
browser, but that caveat does not apply here — it was tested against a running
VibeDesk: `list_pages` returns `VibeDesk (http://tauri.localhost/)`, and
`evaluate_script` with `pageId: 1` returns real DOM state. WebView2 is Chromium,
so the protocol is the same one Chrome speaks.

### The frontend is embedded, so a stale build is possible

`tauri dev` serves the frontend from Vite, but a plain `cargo build` embeds
`dist/` at compile time. Edit the frontend, run `cargo build`, and the binary
keeps the previous bundle: the change appears not to work and nothing reports an
error. Three guards, so it cannot happen and cannot be missed.

- `src-tauri/build.rs` declares `dist/` as a build input, so a `vite build`
  forces the crate that embeds it to rebuild.
- The same script compares `dist/index.html` against the newest file under
  `src/` and emits a `cargo:warning` naming the file that is newer, for the case
  where you build without running `vite build` at all.
- `build_info` reports the embedded bundle, shown in **Help → Build**, and
  `smoke` compares the bundle the page is running against the one
  `dist/index.html` names. A mismatch fails the suite with
  _"running index-Dal-poBz.js, dist has index-STALE0000.js — run `npm run build`
  then `cargo build`"_.

The last one matters most: without it the live suite would happily verify code
that is not running.

Connect a WebSocket to the page target's `webSocketDebuggerUrl` and use
`Runtime.evaluate`. Node 24 has a global `WebSocket`, so no client library is
needed. To set the composer in a way React notices, go through the native value
setter and dispatch `input`:

```js
const s = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
s.call(el, '/th');
el.dispatchEvent(new Event('input', { bubbles: true }));
```

Useful selectors: `.vd-input` (composer), `.vd-enter` (anything that just
arrived), `.vd-row` (transcript rows), `.vd-expand` (a disclosure),
`[role="menu"]` (context menus), `[role="status"]` (toast). The rail is `aside.vd-sidebar`; an open panel drawer is the second
`aside`.

## Verification status

The current state, in one command:

```bash
npm run verify
```

That chains `tsc --noEmit`, `npm run test:unit` (48 tests over the pure
frontend modules), `vite build`, `cargo test --lib` (664 tests) and
`node scripts/live-check.mjs all` (230 checks across 15 suites against the
running app). The live step needs a built app, a display and a free port, which
is why it is a command rather than a CI job; the other four are plain commands
that either pass or fail.

What was actually run, in order, over the course of the work:

- `npx tsc --noEmit` — clean.
- `npx vite build` — clean.
- `cargo check` — clean, zero warnings.
- **Protocol smoke test** against the real `vibe-acp` 3.1.2 binary, driving it
  the same way the Rust bridge does: `initialize` → `session/new` →
  `_trust/status` → `_auth/status` → `session/prompt` → `session/list`. The
  prompt streamed `agent_message_chunk` and returned
  `stopReason: "end_turn"` with a `usage` object.
- **`npm run tauri:dev`** — the window launched, the bridge spawned
  `vibe-acp.exe` as a child, created a session (confirmed in
  `~/.vibe/logs/session/active/`), and the update check wrote
  `%APPDATA%\VibeDesk\settings.json` with `latestVersion: "3.1.2"`. Closing the
  app left no orphaned agent process.
- **Elicitation round trip** against `vibe-acp` 3.1.2, advertising
  `elicitation: { form: {} }` the way `protocol.rs` does. The agent sent
  `elicitation/create` with `mode: "form"` and a `oneOf` schema; answering it
  ended the turn in 3s with `stopReason: "end_turn"`, and the **next** prompt
  was accepted — the `-32603 A turn is already running` failure does not
  reproduce once the request is answered.
- **Preamble and chat list** against the same binary: with the preamble
  appended to the first prompt the agent answered _"I'm running inside
  VibeDesk, a desktop client that drives the Mistral Vibe agent over ACP…"_.
  `session/list({ cwd })` returned the new session, and `session/load` replayed
  its history as `session/update` notifications.
- **Theme validation**, 7 Rust unit tests in `customize.rs`: token names,
  network/script values, protected selectors, layout-hijacking properties, and a
  file round trip through `load_theme_from`.
- **CSP**, verified by launching the built exe with the policy enabled: the
  window booted, the bundle loaded and `vibe-acp` spawned, so script-src and
  connect-src allow everything the app actually uses.
- **Never-bricks**, verified against the running app: writing a valid
  `theme.json` then an invalid one (`url()` plus a protected selector) left the
  process and the agent child alive, same PID.
- **Portable storage**, verified by launching the installed exe: it created
  `%LOCALAPPDATA%\Programs\VibeDesk\data\settings.json` and
  `data\webview2\`, and left the older `%APPDATA%\VibeDesk\settings.json`
  untouched (mtime unchanged).
- **Archive and delete**, driven through the real UI over CDP against the
  installed build: right-clicking a chat row offered _Open chat_, _Copy session
  id_, _Archive chat_, _Delete chat_; archiving dropped the list 18 → 17, wrote
  `config/chats.json` and toasted _Chat archived_; the header toggle read _Show
  1 archived chat_ and revealed the row dimmed; its menu offered _Restore from
  archive_, which returned the count to 18 and emptied `archived`. Deleting a
  chat moved its 105K session directory into `trash/sessions/` with `chunks/`,
  `generations/` and `checkpoints/` intact, and left a tombstone in `deleted`.
  The open chat's _Delete chat_ item renders disabled.
- **`-31002` is explained, not dumped.** The CLI refusing a session another
  process holds is correct behaviour, so `AppError::explain` rewrites it to
  _"This chat is open in another Vibe session. Close it there, then open it
  here."_ and keeps the original detail in parentheses. Two unit tests cover the
  rewrite and the pass-through of every other code.
- **Billing and cache hits**, verified against the installed build with a real
  turn: the Billing panel read _Pro · INDIVIDUAL · chat · ready_, 8.7% of the
  allowance used, _resets in 15 days_, `****U4Ut` scoped to `vibe`, and 10k
  prompt / 3 completion / 3.5k cached tokens at a 35% hit rate for $0.0020. The
  Context panel's Cache section showed the same 35% with 6.6k fresh tokens.
  `/billing` opened the panel and cleared the composer. Eight `billing.rs` unit
  tests cover the live payload, clamping, missing and wrongly-typed fields, and
  the camelCase serialisation.
- **A local command no longer leaves its text in the composer.** `sendPrompt`
  returned before `setDraft('')` for every `transport: 'local'` command, so
  `/billing` stayed in the box and a second Enter re-ran it instead of sending.
  Found while verifying the slash command; fixed for all of them.

Two things the smoke test corrected, both fixed:

- `PromptResponse.usage` is the ACP `Usage` shape
  (`totalTokens`/`inputTokens`/`outputTokens`), **not** the `usage_update`
  notification shape (`used`/`size`/`cost`). The context meter now reads only
  from `usage_update`; the prompt response no longer overwrites it with
  `undefined`.
- `StopReason` also carries `refusal` and `max_turn_requests`, which now
  surface as notices instead of being silently ignored.

- **Theme repaint**, verified by screenshot against the running app: writing
  `{"tokens":{"--vd-accent":"#ff6b35","--vd-canvas":"#12100e"}}` to
  `data\config\theme.json` repainted the wordmark, the `ready` pill and the Send
  button orange and the canvas warm-black, with no restart.
- **Layout**, verified by screenshot against the running app, all without a
  restart and with the same PID throughout: `sidebarWidth: 380` +
  `sidebarSide: "left"` + `density: "roomy"` moved the rail to the left edge and
  widened it; `sidebarWidth: 9999` clamped to 520 px and flipped it back to the
  right; `sidebarCollapsed: true` removed the rail and re-centred the transcript;
  deleting the file restored every default. 6 Rust unit tests cover the clamps,
  idempotence, unknown enum values and partial files.
- **Design direction**, verified by screenshot against the installed build in
  both themes: the ink theme carries the orange accent on the wordmark, the
  `ready` pill, the Send button and the active workstation marker; the paper
  theme carries `#c2410c` on the same surfaces. The syntax palette was checked
  against a real agent-authored Rust block — salmon keywords, blue function
  name, sand types and numbers, olive strings — and the first pass was wrong:
  `hljs-type` was grouped with keywords, so `i32`/`usize` rendered salmon and
  the block read as one colour. Split out to the sand tone.
- **Help panel layout**, verified by screenshot: four `<li>` rows in the
  keyboard table had bare text nodes as siblings of inline `<span>`/icon
  elements, so each description split into columns. Wrapped in `<span>`; the
  text now flows as one sentence.
- **Motion and gloss**, verified by screenshot, by grep and by the `motion`
  live-check suite: no raw millisecond value remains in `src/` outside the
  `--vd-fast`/`--vd-slow` definitions and the `prefers-reduced-motion` reset,
  every meter animates `scaleX` rather than `width`, no element resolves a
  `backdrop-filter` or an SVG filter, nothing is animating at idle, and every
  transcript row resolves `content-visibility: auto`.

- **Actions**, verified by screenshot against the installed build: a six-action
  `actions.json` rendered three buttons in the strip above the composer and three
  in the icon rail. Clicking the `command` action produced the toast
  _Plugins reloaded: no changes_; clicking a `prompt` action sent its text as a
  user turn and the agent began working on it. Editing the file on disk
  re-rendered the strip without a restart.
- **Panels**, verified by screenshot: a two-panel `panels.json` added two rail
  buttons; opening one showed the text block, three file stubs reading
  `right-click to load`, and an actions block borrowing ids from `actions.json`.
  Right-clicking a stub opened a portal menu (_Load file_, _Reveal in file
  manager_, _Copy path_); _Load file_ on a missing path surfaced
  `could not open \\?\D:\VibeDesk\README.md: The system cannot find the file
specified. (os error 2)` inline, and on an existing path rendered the contents
  under a `LOADED` header with a collapse control. Nothing was read before the
  right-click. Editing `panels.json` on disk swapped the block list live.
- **Customize panel**, verified by screenshot: the drawer opened from the rail
  with the subtitle _6 actions · 2 panels_, six appearance token rows with
  colour swatches and hex inputs, four layout sliders showing their effective
  pixel values, the sidebar-side and density segmented controls, the action list
  with per-kind badges (`prompt` / `command` / `open-url`) and remove buttons,
  the add-action form, the panel list with block counts, and the add-panel form.
- **Battletests**, 5 batteries in `battletests.rs` (~30 tests) over security,
  shortcomings, efficiency, safety and resilience. Two real holes were found and
  fixed: `vbscript:msgbox(1)` was accepted as a theme value because
  `FORBIDDEN_VALUE` listed `javascript:` but not `vbscript:` (nor `data:`), and
  `html body` was accepted as a selector because the check was only
  `starts_with("html")`, so a descendant combinator reached the whole document.
  The selector check now splits on commas and restricts what may follow a root
  prefix to `:`, `[` or `.`. Both are covered by tests.
- **Sources battery**, 12 tests in `battletests.rs::sources` over the proposal
  and plugin-source paths: a proposal over `max_bytes` is refused, a symlink is
  refused, a directory named `theme.json` is refused, a digest mismatch is
  refused, a rejected proposal is deleted rather than applied, a stray file name
  is reported and not applied, a differently-cased name is treated as the same
  file, and a plugin source that fails validation is listed with its error and
  cannot be applied. `cargo test --lib` is 92 passed, 0 failed, zero warnings.
- **Proposal round trip**, verified end to end against the installed build:
  dropping `theme.json` and `panels.json` into `data\config\pending\` made the
  proposal bar appear within ~3 s reading _The agent proposed a change / nothing
  is applied until you say so / THEME · 1 token · Apply · Discard_. Clicking
  Apply moved the file into the live config, cleared the pending list and
  repainted the accent to `#22cc88` with no restart; what landed was the
  normalised body (`rules: []` added by serde, pretty-printed), not the raw
  bytes. A second proposal written with no clicks **stayed pending**, so the
  gate does not auto-apply.
- **Plugin sources**, verified against the installed build:
  `~/.vibe/plugins/acme-demo/vibedesk/actions.json` was listed as
  `acme-demo USER actions · 1 button: Deploy` with Apply enabled, and
  `bad-demo` was listed as `refused` with
  `--vd-accent: 'url(' is not allowed in a value` and Apply disabled. Clicking
  Apply on `acme-demo` wrote the normalised `actions.json` (`"hint": null`
  added) and the action strip grew a Deploy button live; the drawer header
  recounted to _1 actions · 1 panels_. Rescan cleared the list once the test
  plugins were deleted. Restoring the original `actions.json` (6) and
  `panels.json` (2) and deleting `theme.json` hot-reloaded back to the orange
  default.
- **Case-insensitive filesystem**, found by the sources battery: a file named
  `Theme.JSON` was both offered as `theme.json` _and_ reported as a stray name,
  because `read_dir` returns the real name while `dir.join("theme.json")` matches
  it on Windows. The stray check now compares with `eq_ignore_ascii_case`
  against `ConfigKind::ALL`. Two tests cover it.
- **Install**, verified: `D:\VibeDesk\uninstall.exe /S` exited 0, removed the
  install directory and the `HKCU\...\Uninstall\VibeDesk` key, and preserved
  `data\`. A stale unregistered copy under `%LOCALAPPDATA%\Programs\VibeDesk`
  was deleted. The NSIS installer then ran with `/S '/D=D:\VibeDesk'` and the
  fresh build booted with the workstation, 6 actions and 2 panels restored from
  the preserved data directory. Both need `MSYS2_ARG_CONV_EXCL='*'` under Git
  Bash, or `/S` is rewritten into a path and the installer waits on its welcome
  page — see [Run](#run).

- **Context menus**, verified by screenshot against the installed build:
  right-clicking a workstation row opened VibeDesk's own menu (_Open_ greyed
  because it is the active folder, _New chat here_, _Reveal in file manager_,
  _Copy path_, _Remove from workstations_); right-clicking a chat row opened
  _Open chat_ / _Copy session id_; right-clicking empty transcript produced no
  menu at all, where the build before this one showed Chromium's _Back /
  Refresh / Save as / Print_; and right-clicking the composer still produced the
  native editing menu with _Paste_ and _Select all_. _New chat here_ created a
  session.
- **Verbatim paths**, found while verifying the above: the rail read _No chats
  in this folder yet_ while ten sessions existed, because `canonicalize` had
  stored `\\?\D:\MistralVibe` and the agent keys history on `D:\MistralVibe`.
  After the fix the same install lists all ten chats with their titles and
  relative times, and the title bar reads `D:\MistralVibe`. Four tests cover
  `strip_verbatim`, including that a `\?` deeper in a path is left alone.
- **Transcript cap**, verified by screenshot against the installed build:
  opening a chat lands at the bottom with no streaming caret on the last
  message, and with `MAX_TRANSCRIPT_ITEMS` temporarily set to 1 the transcript
  showed only the newest row under a _1 earlier entry not shown_ marker. The
  batching itself was not measured — the claim that a replay costs a handful of
  renders rather than one per chunk follows from the code, not from a profile.

- **Bug hunt**, driven through the WebView2 debugger against the installed
  build rather than by screenshot. Six defects were found and fixed:
  1. _Slash palette corruption._ `Composer.tsx` keyed palette rows by
     `command.name`, but the agent's reported command list duplicates the
     built-ins. React reconciled two rows onto one node, stale rows accumulated
     and the list stopped matching the query: `/` rendered 59 rows instead of
     39, `/zzz` rendered 20 instead of 0, `/th` rendered 47 instead of 2, and
     `data-index` came back non-contiguous. `filterCommands` now dedupes by
     name and rows are keyed `transport:name`. Measured after the fix: `/` → 29
     rows with no duplicates and contiguous indices, `/zzz` → 0 rows plus _No
     command matches that._, `/th` → `/theme` and `/thinking` first.
  2. _Escape did not close the palette with zero matches._ The guard was
     `if (palette && results.length > 0)`, so Escape fell through to the busy
     branch. It now closes the palette first, unconditionally.
  3. _The palette never closed on blur._ No `onBlur` on the textarea and no
     outside-click handler, so the popup floated over the transcript
     indefinitely. Added `onBlur`, plus `preventDefault` on the popup's
     `mousedown` so clicking a row does not blur the textarea before the click
     lands. Verified: blur closes it, and clicking a row still inserts
     `/theme` and closes.
  4. _Transcript row keys._ `key={item.id}` collided because the agent reuses
     `messageId` across item kinds. Now `${item.type}:${item.id}`.
  5. _Chat titles leaked the preamble._ The agent titles a session from its
     first user message, and the first message carries the VibeDesk preamble,
     so the rail showed _"Reply with exactly: PONG [VibeDesk] You are running
     inside VibeDesk, a desktop client…"_. `chatTitle()` trims at the marker for
     display only; the stored title is untouched. Applied to the rail, the
     title bar and the Sessions panel. Verified: no rail title contains
     `[VibeDesk]`.
  6. _The composer said "working" while the agent was blocked on a question._
     An unanswered elicitation or approval pauses the turn, so the placeholder
     now reads _Waiting on your answer above — the turn is paused_.

  Re-verified on the rebuilt install: `npx tsc --noEmit` clean, `cargo test
--lib` 96 passed, NSIS bundle 3.69 MiB, reinstalled to `D:\VibeDesk` with
  `data\` preserved. Live checks: a prompt round trip (user row → thought row →
  `OK`, no notices), the busy state (placeholder switches, Stop replaces Send,
  both revert), a Mermaid block (8 themed SVGs, no `<pre>` fallback, no error),
  all ten rail panels (real content, zero console errors), context-menu
  suppression (`defaultPrevented` on the rail and transcript, not on the
  composer), both VibeDesk context menus, and the toast
  _Plugins reloaded · no changes_. Layout holds at 720 px with no horizontal
  overflow and no clipped buttons; the window's own `minWidth` is 940.

  Ruled out as probe errors, not bugs: the toast has no `.vd-toast` class (it is
  `[role="status"]`, 4200 ms), and the rail panel buttons are not inside
  `aside` (they are the 32 px icon strip at `x=8`).

- **Manifest and receipt**, verified end to end against the installed build with
  a real agent turn, not a mock. The agent was asked to read the manifest and
  report three facts; it answered _"20 icons bolt 14 commands"_, which is
  correct, and confirms the agent's read tool accepts an absolute path outside
  the workspace — the one assumption the design rests on. It was then asked to
  propose a theme change: it read the manifest, confirmed `--vd-accent` was in
  the vocabulary at `#ff7a1a`, wrote `pending/theme.json`, and said _"It is
  waiting for your approval — nothing has been applied, and I did not touch the
  live config."_ Applying it wrote
  `{"applied": true, "summary": "1 token", "error": null}`, repainted the accent
  to `#ff6b35`, and the agent then read the receipt back as _"Applied: applied:
  true, summary 1 token, no error."_ The recovery half was verified by writing an
  invalid proposal directly: the receipt carried
  `theme.json: --vd-accent: `url(` is not allowed in a value`, and the agent read
  it, noticed the refused file was not its own, and replaced it with a valid one.

  Two defects were found by that live run and fixed. The first was mine: the
  manifest published the _user's_ theme tokens rather than the app's vocabulary,
  so on a fresh install the list was empty and the agent refused to propose
  anything — the blindness the manifest exists to remove, reintroduced. The
  vocabulary now comes from `include_str!` of `globals.css`. The second was an
  ordering bug: receipts were read before the proposal loop, so the first poll
  wrote a receipt the UI never saw, because the pending directory is only
  re-read when a proposal file's stamp moves.

  `cargo test --lib` is 113 passed, zero warnings; `npx tsc --noEmit` clean;
  bundle 3.70 MiB. The manifest is 11 673 bytes on this install.

- **Creator mode**, verified against the installed build over CDP. The `creator`
  chip toggles, gains `vd-chip-accent`, pins the Customize panel (aside count
  1 → 2) and raises the standing banner at 720×34 px above the composer; the
  banner does not overflow at 560, 720 or 1100 px. `settings.json` gained
  `"creatorMode": true`, and a full relaunch came back with the chip still lit —
  the mode is persisted, not per-session. The banner's exit button turns it off
  again.

  The contract itself was verified with a real agent turn on a fresh session:
  asked to narrow the rail and warm the accent, the agent read the manifest and
  answered _"Two changes, two surfaces — I'll propose the rail width first
  (layout), then the accent (theme) once this one is decided"_, then wrote
  `pending/layout.json` and said _"it's waiting for your approval, not applied"_.
  That is the builder framing working: the base contract alone produces a list of
  steps for the user, not a proposal.

  **Apply + tell** was then clicked on that proposal: `layout.json` landed with
  `railWidth: 44`, the receipt read
  `{"applied":true,"summary":"rail 44px, sidebar 252px right, comfortable"}`, and
  the agent replied _"Layout receipt confirms applied: rail 44px… Now the accent
  — proposing the theme surface"_ before writing `pending/theme.json` with six
  accent tokens. Plain **Apply** on that one repainted `--vd-accent` from
  `#ff7a1a` to `#ff7414`. Both surfaces were then reset to defaults and the
  receipts cleared.

  `cargo test --lib` is 116 passed (3 new in `commands/prefs.rs`), zero warnings;
  `npx tsc --noEmit` clean; bundle 3.70 MiB.

- **The sandbox block**, verified against the installed build over CDP. Two
  blocks rendered at 150 and 130 px from `data:text/html` URLs with
  `sandbox=""`; both painted their content, and evaluating inside each frame
  returned `origin: "null"`, `parentDoc: "blocked"`, `storage: "blocked"`,
  `cookie: "blocked"`. The theme token reached them: `--vd-accent` read
  `#ff7a1a` in ink and `#c2410c` after switching to paper, so a widget follows
  the theme without knowing which one is active. No console errors.

  The validator was exercised by writing the demo panel by hand: a widget whose
  script called `fetch('https://example.com')` was refused with
  ``sandbox html contains `https://`; a sandbox block has no network and cannot
embed frames or forms`` — the agent's own probe, caught at write time. Ten
  forbidden forms are covered by a test, including the uppercase
  `<SCRIPT SRC>` variant.

  **Two defects were found by this phase and fixed.** The first is the reason
  the block has no scripts: `srcdoc`, `blob:` and `data:` documents all inherit
  the parent's CSP in Chromium, so `script-src 'self'` blocked every inline
  script in the frame. The block was built with a per-block _run_ grant first,
  and the live test showed the grant could never work without adding
  `'unsafe-inline'` to the whole app — so the grant was removed rather than the
  policy weakened. The second was a real security gap that predates this phase:
  `index.html` carries a hardcoded meta CSP which is the policy that actually
  applies, and it had drifted from `tauri.conf.json` — the meta was missing
  `object-src 'none'`, `base-uri 'none'` and `form-action 'none'`, and allowed
  the unused `asset:` scheme. Both now carry the identical string, and the meta
  says so.

  `cargo test --lib` is 123 passed (7 new in `panels.rs`), zero warnings;
  `npx tsc --noEmit` clean; bundle 3.71 MiB.

- **The diff and the preview**, verified against the installed build over CDP
  with real agent turns. Asked for a deeper accent, the bar rendered
  `--vd-accent  #ff7a1a → #ff7414` — a field, not a count. A six-field layout
  proposal folded at four rows behind _show 2 more_ and expanded to all six.

  **Preview** repainted `--vd-accent` to `#ff7414` while the header read
  _previewing — nothing is written_, and `data\config\theme.json` did not exist
  on disk until **Apply** was clicked. Escape on the bar put `#ff7a1a` back and
  restored the header text. The layout preview moved `--vd-rail-w` 51 → 44 px,
  `data-sidebar-side` right → left and `--vd-density` 1 → 1.22, and Escape
  reverted all three. The diff row kept showing the true `from` throughout, so a
  preview cannot rewrite the column it is being compared against.

  **Apply + tell** on a theme proposal wrote `theme.json` with exactly one
  token, wrote `theme.result.json`, and the agent's follow-up turn answered
  _"Confirmed applied. Receipt: applied: true, summary: \"1 token\"…"_.

  `cargo test --lib` is 130 passed (1 new in `sources.rs`), zero warnings;
  `npx tsc --noEmit` clean; bundle 3.71 MiB.

  **Three defects were found by this phase and fixed.**

  The first was in the diff itself: with no `theme.json` on disk every token
  rendered as an addition (`— → #ff7414`), which hides the thing being decided.
  The fix is split — `layout`, `actions` and `panels` get a synthesised baseline
  in Rust because their defaults are real configs, while theme does not, because
  a `theme.json` _merges_ over the stylesheet and an absent file overrides
  nothing. The UI fills that one blank cell from the live computed value,
  memoised on the proposal list so a preview cannot move it.

  The second was a layout bug the phase exposed: `.vd-btn` sets
  `white-space: nowrap` but not `flex-shrink: 0`, so in a tight row the buttons
  shrank and their labels spilled outside the button box. The bar's button row
  now wraps and its buttons hold their width. The same probe found the title
  bar's button cluster overflowing a 560 px window by 35 px — the labels now
  drop below 880 px and the icons carry the meaning.

  The third was a duplicate user message. `addUserMessage` puts the prompt on
  screen immediately with a local id, and the backend then echoes it back as a
  `user_message_chunk` with its own id; the reducer could not match the two, so
  every echoed prompt appeared twice. The reducer now adopts the backend's id
  onto the optimistic row and keeps the text it already has, which also makes
  the row stable across a replay — when the echo is the only copy there is.

The Sources list is a snapshot taken at boot and on Rescan; a plugin deleted
from disk stays listed until then, and Apply on it fails safely with
`ships no …` rather than writing anything.

Mermaid is now observed: a `mermaid` fenced block from the agent rendered as a
themed SVG under the CSP, with no `<pre>` fallback and no console error. KaTeX
is still wired and typechecked but not observed. The plugin toggle path
(`plugins_set_enabled` writes `~/.vibe/config.toml` — it has a `.bak` guard but
was not exercised against a real plugin). The workstation rail's folder picker
and the elicitation form were exercised at the protocol level and typechecked,
but not clicked through in the running window.

The first-run flow was verified in both directions by launching the built exe:

- `VIBEDESK_ACP_BIN=C:\nope\vibe-acp.exe` → app starts, **no** `vibe-acp`
  child is spawned, so the setup branch ran instead of the agent boot.
- Normal launch → `vibe-acp` child spawned as before, so no regression.

The install runner itself (`vibe_install`) was **not** executed end-to-end —
that would mean uninstalling Vibe first. The detection, the plan selection and
the UI branch are verified; the `uv tool install` spawn is not.

## Defect ledger

Phase 5 of `docs/PLAN-dynamic-customization.md` is a sweep of every panel, menu,
slash command and keyboard path. This is what it found. Each entry says what the
defect was, how to reproduce it, why it happened, what changed, and how the fix
was re-verified.

A frontend fix is re-verified by `npx tsc --noEmit`, `npx vite build`, the live
suite and — for the pure modules — `npm run test:unit`. A Rust fix is re-verified
by `cargo test --lib`. Where a defect could be pinned by a test, it was.

### Fixed

**1. Nine dead-code warnings on every `cargo check`.** Reproduce: `cargo check
--lib` in `src-tauri/`. Root cause: `consent.rs` and the two consent commands in
`commands/data.rs` are staged for the deferred non-loopback source feature, and
nothing calls them, so the compiler was right. Registering the commands would
have been the wrong fix: it would put a consent switch in the UI for a capability
that does not exist, and a user who "allowed" an origin would see nothing happen.
Fix: `#![allow(dead_code)]` on `consent.rs` and `#[allow(dead_code)]` on
`ConsentEntry`, `data_consent_list` and `data_consent_set`, each with a comment
naming the trigger. `data::check_loopback_url` is untouched and still refuses
every non-loopback host; nothing consults the ledger to permit a fetch.
Re-verified: `cargo check --lib` is silent, `cargo test --lib` still 314 passing.

**Superseded by entry 27.** The ledger _is_ consulted — `data::split_by_consent`
reads it on every reconcile — so the commands were registered and the allows
removed. The reasoning above was right about the UI and wrong about the backend:
a consent switch was needed, because without one no `http` source could ever
run.

**2. `CustomPanels` and `LayoutView` imported each other.** Reproduce: change
`export function BlockView` to `export const BlockView = (…) => …` in
`CustomPanels.tsx` and open any v2 panel — the window dies with a temporal dead
zone error. Root cause: `LayoutView` renders the leaves of a layout tree and
needed `BlockView`, which lived in the module that renders `LayoutView`. It
worked only because a hoisted function declaration is defined before either
module body runs. Fix: `BlockView` and its helpers moved to
`src/components/panels/BlockView.tsx`; both callers import from there and the
cycle is gone. Re-verified: `tsc --noEmit` and `vite build` clean, and the import
graph is now acyclic.

**3. `panels_plugin_issues` was registered but unreachable.** Reproduce: put a
malformed `panels.json` in a plugin's `vibedesk/` folder and open the rail — the
panel is simply absent, with nothing anywhere saying which plugin or why. Root
cause: the command existed and was tested, but `src/lib/bridge.ts` had no entry
for it, so no component could call it. Fix: `customize.pluginIssues()` in the
bridge, read on mount and on "Rescan plugins", and rendered in the Customize
drawer's Panels section as _Plugin panels not loaded_. Re-verified: `tsc` and
`vite build` clean; the command's own Rust tests are unchanged.

**4. Resuming a chat from the Sessions drawer appended to the transcript and
re-sent the preamble.** Reproduce: open a chat, run `/resume`, pick a different
saved session — the old messages stay on screen above the replayed ones, and the
next prompt carries the VibeDesk contract a second time. Root cause:
`SessionsPanel` called `session/load` itself instead of going through
`openChat`, so it skipped `clearSession()` and `markPreambleSent(true)`. Fix:
`resume` now calls `openChat`, which is the same operation done correctly, and
the duplicate load is gone. Re-verified: `tsc` and `vite build` clean; the
transcript-clearing and preamble paths are the ones the rail already used.

**5. A bar chart faded to invisible past 40 points.** Reproduce: a `chart` block
with `style: "bar"` bound to a series of more than 40 numbers — every bar renders
at roughly the same near-zero opacity. Root cause: the bars drawn were
`points.slice(-40)` but the fade was divided by `points.length`, so a 200-point
series topped out at 0.35 + 0.65 × 40/200. Fix: the fade is computed over the
bars actually drawn. Re-verified: `tsc` and `vite build` clean.

**6. The Config panel's reload button could produce an unhandled rejection.**
Reproduce: open `/config` and click reload while the agent is unreachable. Root
cause: `reloadConfig` was an async click handler with `try/finally` and no
`catch`, and React does not catch a rejected handler. Fix: a `catch` that reports
through `addNotice`, matching every other handler in the file. Re-verified: `tsc`
and `vite build` clean.

**7. The elicitation bar's "Open the page" could produce an unhandled
rejection.** Reproduce: an `elicitation/create` with `mode: "url"`, then click
_Open the page_ when the opener refuses the URL. Root cause: `void openUrl(url)`
discards the promise, so a refusal was both unhandled and invisible. Fix: the
rejection is caught and shown in the bar's own error line. Re-verified: `tsc` and
`vite build` clean.

**8. The approval bar could render with no buttons at all.** Reproduce: an agent
that reports permission options whose `kind` is not `allow*` or `reject*`. Root
cause: the bar filtered `options` into allow and deny and rendered only those, so
an unrecognised kind produced an empty button row — and the agent is paused until
it gets an answer, so the session was stuck with no way out. Fix: when neither
list has an entry, a single _Reject_ button answers with a null option, which the
bridge already maps to `{"outcome": {"outcome": "cancelled"}}`. Re-verified:
`tsc` and `vite build` clean; the null-option path is the one the existing reject
button already used.

**9. Escape did not close a panel.** Reproduce: open `/config` or a custom panel
and press Escape. Root cause: the drawer and the custom panel are `<aside>`
elements with a close button and no key handler, so the one surface that only
answered the mouse was the one the keyboard could not dismiss. Fix:
`useEscapeToClose` in `Drawer.tsx`, used by both. It yields to a field inside the
panel, to a handler that already claimed the key (the composer's Escape cancels a
turn), and to an open context menu, so one keypress is never two dismissals.
Re-verified: `tsc` and `vite build` clean; the Help panel's shortcut list now
says so.

**10. The rail's Conversation button read as active while a custom panel was
open.** Reproduce: open a plugin panel from the rail — the Conversation icon is
highlighted and the plugin's icon is too. Root cause: the button tested
`panel === null`, and a custom panel lives in `customPanel`, so both were true.
Fix: the test is `panel === null && customPanel === null`, for the class and for
`aria-pressed`. Re-verified: `tsc` and `vite build` clean.

**11. The chat context menu could open past the bottom of the window.**
Reproduce: right-click a chat row near the bottom of the rail. Root cause:
`anchorAt(event, 2)` told the clamp the menu was two rows tall; it is five. Fix:
the real row count. Re-verified: `tsc` and `vite build` clean.

**12. The workstation remove button was invisible to the keyboard.** Reproduce:
Tab to the X on a workstation row — it is `opacity-0` and only `group-hover`
brings it back, so a keyboard user focuses an invisible control. Fix:
`focus-visible:opacity-100` alongside the hover rule. Re-verified: `tsc` and
`vite build` clean.

**13. A v2 panel showed "0 blocks" in the Customize editor.** Reproduce: a panel
whose body is a `layout` tree — the Panels section reads `0 blocks` over a panel
full of tabs. Root cause: the row counted `blocks`, which is the flattening of
the tree, and printed "blocks" regardless of which model the file used. Fix: the
count is still the flattening (the honest number of things drawn) and the unit
says `layout` when the file holds a tree. Re-verified: `tsc` and `vite build`
clean.

**14. The composer's Send tooltip named a shortcut that does not exist.**
Reproduce: hover Send — it read `Send (Ctrl+Enter)`; Enter alone sends. Root
cause: the tooltip used `modKey()` from a time when the binding was different.
Fix: the tooltip says `Send (Enter)`, and `modKey`/`isMac` went with it. See
entry 16. Re-verified: `tsc` and `vite build` clean.

**15. Dragging a layout slider wrote `layout.json` on every input event, out of
order.** Reproduce: drag _Sidebar width_ in the Customize drawer from one end to
the other. Root cause: `NumberRow` committed on every `input` event, so one drag
fired dozens of `layout_set` calls; each is an IPC round trip, a file write and a
`config://changed` broadcast, and two writes in flight can land in either order —
leaving the file at a width the user had already dragged past. Fix: the slider
tracks the pointer in local state and commits once, on release, on key-up or on
blur. Re-verified: `tsc` and `vite build` clean.

**16. Dead code in `src/lib`.** Reproduce: search for `diffFromTexts`, `dirname`,
`modKey`, `isMac` — no callers. Root cause: `DiffView` carried its own copy of
`diffFromTexts` and never imported the one in `diff.ts`, so the shared version
was dead and the two could drift; `dirname` and the platform helpers outlived
their last use. Fix: `DiffView` imports `diffFromTexts` and its duplicate is
deleted; `dirname`, `isMac` and `modKey` are deleted. Re-verified: `tsc` and
`vite build` clean.

**17. The README described the v1 panel model.** Reproduce: read _Panels_ — "a
panel is a list of blocks, and a block is data". Root cause: the section was
written before the layout tree, the sources and the four bound block kinds
existed, and nothing in the build fails when prose goes stale. Fix: the section
now describes sources + data model + layout tree, names all twelve block kinds and
all three node kinds, documents `when` on nodes, and shows a v2 example. See
[Panels](#panels). Re-verified: every limit and kind in the new text is read from
`panels.rs` and `data.rs` rather than restated from memory.

**18. The README's slash-command lists named a command that does not exist.**
Reproduce: read _Slash commands and mentions_ — it lists `/sessions` as local
(there is no such command; it is `/resume`) and `/help` as an agent command (it
is local, and opens the Help drawer). Root cause: the list was hand-written
beside `SLASH_COMMANDS` and drifted. Fix: both lists corrected, the aliases
named, and the paragraph now says `commands.ts` is the source of truth. See
[Slash commands and mentions](#slash-commands-and-mentions). Re-verified: every
name checked against `SLASH_COMMANDS` and its `transport`.

**19. A plugin name containing `/` made `plugin:<plugin>/<id>` ambiguous.**
Reproduce: a plugin whose manifest `name` is `acme/ci` contributes a panel with
id `plugin:acme/ci/build`, which cannot be split back into plugin and panel.
Root cause: `plugins::read_plugin` takes `name` from the manifest verbatim, and
`plugin_panels::scan` concatenated it into the id with no check. Nothing parses
the id today — it is an opaque key in the store, the rail and the data plane — so
this was a trap for the next person rather than a live bug. Fix: `scan` refuses a
plugin whose name contains `/` or `\`, records an issue naming the plugin, and
leaves its panels out; the id is the isolation boundary, and an id that needs
quoting is not one. Re-verified:
`a_plugin_name_with_a_separator_is_refused` asserts both separators are refused
and that the same folder under a plain name still loads.

**20. Context menus had no keyboard path.** Reproduce: with the keyboard only,
try to open the menu on a chat row. Root cause: every menu was opened from an
`onContextMenu` handler, and `ContextMenu` rendered `role="menu"` with no focus
move and no arrow-key handling. Fixed in both halves at once, because half a fix
is worse than none: `keyboardAnchor` turns Shift+F10 and the Menu key into an
anchor computed from the focused row's rect, and `ContextMenu` takes focus on
open, walks with ArrowUp, ArrowDown, Home and End, closes on Escape and Tab, and
hands focus back to whatever had it. The file card in a panel is a `div`, so it
gained a tab stop and a `role="group"` to be reachable at all. Re-verified by
the `keyboard` suite: eleven checks covering the open, the focus move, the walk,
the close and the focus restore.

**21. The rail resize handle was pointer-only.** Reproduce: Tab to the separator
between the transcript and the workstation rail — it was not focusable, and there
was no key to change the width. Root cause: `ResizeHandle` was a
`role="separator"` with pointer handlers and no `tabIndex`. Fixed: it is a
focusable separator with `aria-valuenow`, `aria-valuemin` and `aria-valuemax`;
ArrowLeft and ArrowRight move the edge in the direction that grows or shrinks the
rail for its current side, Shift moves it 48 px instead of 16, Home and End jump
to the limits, and the width is persisted once the key repeat settles rather than
once per press. Re-verified by the `keyboard` suite, including that the saved
width reaches `layout_get`.

**22. Agent-authored SVG in a fenced block was injected as HTML.** Reproduce: a
reply containing a ` ```svg ` block whose markup includes an `<a href>` —
clicking it navigates the whole webview away from the app. Root cause:
`Markdown`'s `SvgBlock` used `dangerouslySetInnerHTML` on the fence body. The CSP
(`script-src 'self'`, `img-src 'self' data: blob:`, `object-src 'none'`) stops
script and external loads, but navigation is not a CSP directive, so a link
survived. Fixed without a dependency: `src/lib/sanitize-svg.ts` rebuilds the
drawing from an allowlist. The policy is a pure function over a plain tree
(`sanitizeTree`), so it is unit-tested without a browser; the DOM is only the
adapter that parses and serialises. Unknown elements are unwrapped rather than
deleted, so a drawing inside an unknown wrapper stays visible. `href` survives
only as a same-document `#id`, which keeps `<use>` working and a link impossible.
Re-verified by thirteen unit tests, one per attack the policy has to refuse.

**23. `series()` was uncapped while `rows()` capped at 200.** Reproduce: a
`chart` bound to a 10,000-element array — the polyline carried all 10,000 points.
Root cause: `rows()` sliced to 200 for the table, and `series()` had no
equivalent limit. Fixed by decimating rather than truncating: `decimate()` cuts
the array into buckets and takes each bucket's lowest and highest sample in the
order they occur, so a spike survives, the endpoints survive, and the result is
at most 512 points. Truncating would have been a lie — the line would stop
halfway and the axis would describe a range the data does not have. The chart
says `512 of 10,000 points` when it has decimated, because a summary that does
not say so is a chart that lies about how much data it drew. Re-verified by six
unit tests including the spike and the trough, and by the `chart` suite.

### Fixed after live verification

These eight were found by driving the running app over CDP, not by reading it.
All eight passed `tsc`, `vite build` and 319 Rust tests. See
[Live verification](docs/PLAN-STATUS.md#live-verification) for the harness.

**24. The `clock` built-in never ticked.** Reproduce: a panel with a `builtin`
source named `clock` and a `data` block bound to `/clock/epoch` — the number is
correct when the panel opens and never changes again. Root cause:
`useClockTick` started a one-second interval and discarded the value
(`const [, setTick]`), so the component re-rendered while the `useMemo` in
`usePanelModel` returned the model it had built a second earlier; the tick was
never a dependency. Fix: the hook returns the tick and the memo depends on it.
Re-verified live: the epoch advanced across a 3.3s window and the minute
progress bar moved with it.

**25. Disabling a plugin left its panel in the rail.** Reproduce: install a
plugin with a `vibedesk/panels.json`, open the Plugins drawer, disable it — the
panel stays until the next launch. Root cause: `plugins_set_enabled` returns the
new plugin list and nothing re-reads `panels_get`, so the derived panel list was
stale. `reloadPlugins` had the same hole, which also meant a plugin installed
while the app was running contributed nothing. Fix: `refreshPanels()` in
`actions.ts`, called from both paths. Re-verified live: enable added the panel,
disable removed it, no reload.

**26. A tweened integer rendered as a fraction.** Reproduce: a `data` block
bound to a large integer that changes — it displays `1,794,842,690.9`. Root
cause: `useTween` interpolates through fractions and `display()` formats
whatever it is given. Fix: round when the source value is an integer, so a count
or an epoch stays whole while a genuine decimal still moves smoothly.
Re-verified live: the epoch stayed integral across ticks.

**27. The external source path was unreachable.** Reproduce: a panel with an
`http` source — it never fetches, and the header says `stopped` as if the config
were broken. Root cause: `data::split_by_consent` reads the consent ledger on
every reconcile and refuses an origin with no decision, but `data_consent_list`
and `data_consent_set` were written, tested and never registered in `lib.rs`, and
no component read the `awaiting-consent` state. The ledger could only ever be
empty, so every `http` source waited forever. The unit tests passed because they
call `consent::decide` directly. Fix: both commands registered, `origin` and
`state` added to the frontend's `SourceStatus`, and a consent bar in the panel
with Allow, Deny and Ask again. Re-verified live: the panel asks, nothing is
fetched before the answer, and the value starts polling on Allow.

**28. Every boot logged a 404.** Reproduce: open the app and read the console —
`Failed to load resource: 404` on the first frame. Root cause: no favicon was
declared, so the webview requested `/favicon.ico`, which does not exist. Fix: an
inline `data:` SVG icon in `index.html`, which the existing `img-src data:`
policy already permits. Re-verified live: zero console errors on boot.

**29. A `cargo build` could embed a stale frontend, silently.** Reproduce: edit
anything under `src/`, run `cargo build`, launch — the app serves the previous
bundle and the change appears not to work. Root cause: `tauri::generate_context!`
embeds `dist/` at compile time, but nothing declared `dist/` as a build input, so
Cargo reused the cached crate. The dangerous part is not the confusion: the live
suite would then verify code that is not running and report 36/36. Fix:
`src-tauri/build.rs` declares `dist/` and the frontend sources as inputs, warns
when `dist/` is older than the newest source, and exports the embedded bundle
name; `build_info` shows it in Help under **Build**; and `smoke` fails when the
bundle the page is running is not the one `dist/index.html` names. Re-verified:
pointing `dist/index.html` at a fake bundle makes the suite exit 1 with the two
names side by side.

**30. Every chat the user had visited became undeletable.** Reproduce: open chat
A, switch to chat B, right-click A and choose _Delete chat_ — _That chat is open.
Switch to another chat before deleting it._ Root cause: `chats::is_active` treated
the presence of `~/.vibe/logs/session/active/<id>.lock` as "this chat is open".
The lock is per session, not per process: one CLI process keeps a lock for every
session it has ever loaded and only drops them when it exits, so a single
`python.exe` was holding six. After a switch the old chat's lock was still there
and the guard refused forever. The frontend guard was already correct, which is
why the message appeared as an error rather than a warning. Fix: `chats_delete`
compares against the session the bridge actually has loaded
(`state.bridge.status().session_id`), and the lock directory is no longer
consulted. Re-verified live: a lock file written for a chat that is not open no
longer blocks the delete, the chat just switched to is still refused, and the
chat switched away from is deletable.

**31. The debugging-endpoint check passed or failed on how long the app had been
up.** Reproduce: start the app, switch chats, then run `npm run live` — `smoke`
fails _a debugging endpoint is announced when it is on_ even though the port is
open and was announced. Root cause: the check read the boot notice out of
`document.body.innerText`, and the notice is a chat item, so `clearSession()`
wipes it on every chat switch. The check was green only when the suite ran before
the first switch, which is why it read 36/36 and then failed on a warm app. Fix:
the Help drawer's **Build** section now carries a durable `debugging` row and the
same warning, and `smoke` reads that instead. Re-verified: two consecutive `all`
runs on an app that had already switched chats both report 41/41.

## Layout

```
src/
  assets/fonts/          bundled woff2 + fonts.css
  components/
    chat/                Transcript, Markdown, ToolCallCard, ApprovalBar,
                         ElicitationBar, ProposalBar, CreatorBanner
    code/                CodeBlock, MermaidBlock, DiffView
    composer/            Composer with / and @ palettes
    chrome/              TitleBar, StatusBar, Rail, WorkstationRail, Drawer,
                         ActionStrip
    panels/              Plugins, Config, Sessions, Info, Customize, CustomPanels,
                         LayoutView, BlockView, SandboxBlock
    ui/                  Toast, ContextMenu
  lib/                   bridge, actions, commands, contextmenu, customize, data,
                         diff, highlight, motion, preamble, utils
  store/app.ts           single zustand store
  styles/globals.css     tokens + component classes
  types/acp.ts           wire + view types
src-tauri/
  src/acp/               bridge, protocol, terminal registry
  src/commands/          IPC surface (agent, session, vibe, workspace, workstations,
                         customize, prefs)
  src/actions.rs         custom action buttons (closed run.kind enum)
  src/panels.rs          declarative panels: blocks, layout tree, validation
  src/expr.rs            the binding grammar, shared with src/lib/data.ts
  src/data.rs            the data plane: loopback-only fetch, caps, kill switch
  src/consent.rs         per-origin consent ledger, consulted on every reconcile
  src/plugin_panels.rs   panels a plugin ships, namespaced under its own name
  src/customize.rs       theme validation + config watcher
  src/sources.rs         pending proposals, receipts + plugin-authored config sources
  src/manifest.rs        the agent's map: vocabulary derived from the validators
  src/layout.rs          geometry, integers and closed enums only
  src/battletests.rs     5 batteries: security, shortcomings, efficiency,
                         safety, resilience
  src/config.rs          config.toml / hooks.toml
  src/paths.rs           binary resolution + portable data dir
  src/plugins.rs         plugin discovery + reload diff
  src/skills.rs          skill discovery
  src/update.rs          PyPI check + upgrade
  icons/generate.mjs     regenerates the app icons
```
