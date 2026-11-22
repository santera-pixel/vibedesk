# AGENTS.md

## Aegis wraps the turn, mastery supplies the material — hard requirement

Every turn starts with Aegis routing, and the mastery bundles are loaded for the
work in front of you. This is the process, not a suggestion, and it is not
optional because a task looks small.

### 1. Aegis is the wrapper

`using-aegis` is the entry point for every turn. It decides whether the work
stays on the fast path or routes to a skill, and it owns the order of the turn:
classify before implementing, baseline before a medium or high change, a TDD
route before the first test, verification before any claim of done.

The rules in this file outrank Aegis. Aegis says so itself, and it is the right
way round: these are the constraints, Aegis is how the work is carried out
inside them. When the two disagree, this file wins and the disagreement gets
named rather than quietly resolved.

Do not skip the route because the task looks small. Skip it because the route
said fast path, and say that it did.

### 2. Mastery skills are loaded, not remembered

The mastery bundles are installed, but their skills are not in the skill
catalog. They arrive through `mcp_mastery.load { bundle, skill }`, which puts the
body in context for the rest of the turn. A skill you did not load is a skill
you did not use, whatever you remember of it.

Never zero. The relevant skill is always loaded; the irrelevant ones are not.
Loading all fourteen every turn is the same failure as loading none, because
neither is a decision about the work.

| Work | Load |
| --- | --- |
| Any code change | `antislop` — the core filter, which says to load it always |
| Writing or editing comments | `antislop-code` |
| Prose, copy, a user-facing string | `antislop-copywriting` |
| Any interface: colour, layout, components, motion | `antislop-ui`, plus `ui-ux-governance` from Aegis |
| Contrast, keyboard, focus, states | `antislop-human` |
| Anything that reflows across screen sizes | `antislop-layoutmobile` |
| Design tokens, palettes, typography, charts | `uiux` — `design-system`, `ui-styling`, `design` |
| Work where being wrong costs something | `assay` |
| A 3D scene or a spatial explainer | `3dviz` |

`mcp_mastery.find { query }` when you know the kind of work but not the skill.
`mcp_mastery.bundles` when you do not know what is available.

### 3. Name the route and the skills

At the first substantive stage of a turn, say which Aegis route ran and which
mastery skills were loaded. This is not ceremony. A turn that used the method and
a turn that did not produce the same-looking output, and this is the only place
the difference is visible before the work is wrong.

## No workarounds — hard requirement

A workaround is any change that makes the symptom disappear without making the
cause go away. It is the most expensive thing you can ship, because it converts a
visible failure into an invisible one, and the user only finds out after building
on it. This outranks speed, convenience, and the desire to report success.

### The rule

- If something does not work, say it does not work.
- If something is blocking you, say what is blocking you and what it would take
  to unblock it.
- If something is not working as intended, fix it or say it is not fixed. There
  is no third option, and "it works now" is not a fix.
- If an obstacle can be overcome, first check that the thing you are overcoming
  it with is not a workaround. If it is, say so instead of using it.
- Always attempt the real thing before the substitute.

### What counts as a workaround

| Workaround | The real thing |
| --- | --- |
| Catch the error and return a default | find out why it throws |
| Retry until it passes | find the race |
| `any`, `unwrap_or_default()`, a widened type to silence the compiler | make the state representable |
| Route around a capability with a shell command | use the escape hatch it names, or fix the capability |
| Skip, ignore or delete the failing test | fix the code, or delete the test and say why |
| Hardcode the value the environment failed to provide | make the environment provide it, or fail loudly |
| A fallback path that hides the primary path's failure | make the primary path work, or remove it |
| Lower a threshold until the test passes | find out why the real input does not reach it |
| Comment out the assertion | fix what it caught |
| "It works if you do X first" | make it work without X |
| A second code path that exists only because the first one is broken | one path that works |

### The test, before you take a detour

1. Name the obstacle in one sentence.
2. Name what you are about to do instead.
3. Ask: after this, is the obstacle gone, or only hidden?
4. If it is only hidden, stop. Fix the cause, or say you cannot and stop.

### Reporting an obstacle

The response says what failed, what you tried, what the cause is (or that you do
not know), and what you are doing about it. "It works now" without a cause is a
workaround report.

Never describe a workaround as a fix, and never describe a fallback as a feature.
If you added one, name it as one in the same sentence.

### The one legitimate exception

A deliberate, documented degradation is not a workaround when all three hold:

- the primary path is still attempted first,
- the degradation is visible to the user — a banner, a log line, a status — not
  silent,
- the reason is a real external limit you cannot remove: a platform API that does
  not exist, a missing permission, a capability the host does not expose.

If any of the three is missing, it is a workaround. Say so.

## No shallow features

Never ship a shallow feature. A feature is either feature-rich depth or it does
not exist. This is a standing instruction for this repository and it outranks
convenience, phase plans, and any "we can deepen it later".

A workaround is the shallow version of a fix. The section above governs it: a
detour that hides a failure is the same lie as a surface that implies a
capability it does not have.

### What counts as shallow

- A surface that implies a capability it does not have. A chart block with no
  axes, scales, series or tooltips is a sparkline wearing the word "chart".
- A fixed list standing in for an open one. A closed enum of fourteen commands
  is not "control the agent".
- A feature that completes the demo case and fails the real one.
- A stub, placeholder, or deferred item that is visible to the user.
- Depth faked by widening: more block kinds, more enum variants, more config
  keys, when the missing thing is execution, composition or interactivity.

### The test, before building anything

1. Name the concrete task a user must be able to complete end to end.
2. Name what the deep version does that the shallow one cannot.
3. If the deep version needs a foundation that does not exist, say so and stop.
   Do not ship the shallow version as a placeholder for it.

### What to do instead of shipping shallow

Say no. Explain the ceiling and what closing it would take. Propose the deep
version, or nothing. A missing feature is honest; a hollow one is a lie the user
only discovers after building on it.

### Known shallow surfaces in this repo

None. Every surface that was named here has been deepened or resolved, and the
list is kept empty on purpose: an entry here is a promise that the surface is
either about to be fixed or about to be removed, and a list that stays populated
stops being read.

If you add a surface that fails the test above, add it here in the same commit
that adds the surface, with the decision it needs. Do not add it and leave this
section alone.

### Resolved shallow surfaces

- `sandbox` block — was markup and CSS only, with no way to become interactive.
  Resolved by building the execution boundary it needed rather than by widening
  the block: `widget` serves agent-authored HTML **and scripts** from its own
  origin (`http://widget.localhost`) with its own CSP, so the app's
  `script-src 'self'` never had to be loosened. `sandbox` is kept as the
  markup-only variant for content that does not need scripts — least privilege,
  not a placeholder. See `docs/PLAN-STATUS.md`, Phase 7.
- `chart` block — was bar, line and sparkline over a flat array of numbers, with
  no axes, scales, series or tooltips: a sparkline wearing the word "chart".
  Deepened rather than renamed or removed. It now has axes with ticks and
  labels, up to four series, a legend that hides a series from the axis as well
  as the plot, a hover readout, a log scale, stacking, and a decimation note when
  the source is longer than the 512-point cap. `sparkline` survives as an
  explicit style, which is what it always was.
- `actions` block — was a fixed button list over a closed `run.kind` enum whose
  only agent control was `prompt`. Widened to seven run kinds — `prompt`,
  `command`, `open-url`, `reveal`, `mode`, `option`, `cancel` — plus `approve`,
  `reject` and `answer` for a paused agent, so "control the agent" now means
  answering its questions and changing its mode, not only sending it text. The
  set is still closed, and that is the boundary rather than a gap: the
  alternative is a widget with IPC access. ACP has no steer method, so a running
  turn cannot be redirected mid-flight by anything, including this block.

### The rule this repo learned the hard way

A capability that is described but not reachable is worse than a missing one,
because the user builds on it before finding out. When the honest answer is "the
platform does not allow this", say that and stop — do not ship a narrower thing
under the same name. `sandbox` sat in this file for weeks as a block that looked
like a widget and could not be one. The fix was not a better `sandbox`; it was
finding the one scheme that does not inherit the parent CSP.

## Capabilities before tools — hard requirement

This is not a preference. It is the order of operations for every task in this
repository, and it outranks convenience, speed, and habit.

### 1. Install first

A capability that is not installed is not a capability. Before any work that
depends on one, confirm the binary or server is present, on `PATH`, and
registered in `VIBE_HOME/config.toml`. `capabilities_external_status` must
report `registered: true` and `blocked: null` with a real version string.

Never proceed to step 2 while a capability is uninstalled or unregistered. An
uninstalled capability makes every test that follows meaningless, because the
test is measuring the absence of the thing it claims to test.

### 2. The agent uses the capability, not its own tool

When a capability covers the task, the agent must call the capability. Its own
built-in tool is the fallback, not the default.

| Task | Use this | Not this |
| --- | --- | --- |
| Search the web, fetch a page | `mcp_donsetch.web_search` / `web_fetch` | guessing, or `curl` |
| Understand a codebase's structure | `mcp_codebase_memory.*` | `grep` over the tree |
| Recall what a previous session learned | the memory capability | re-deriving it |
| Read a long document | the reading capability's card, then its `offset`/`limit` | reading the whole file |
| Run a command whose output is long | the terminal capability's compaction | dumping raw output |

If the capability is unavailable, say so and name what is missing. Do not
silently substitute a built-in tool and report success: that is the failure mode
this section exists to prevent, and it is invisible in the transcript.

### 2a. Do not walk around a capability with a shell command

A capability that intercepts one tool is only as good as the agent's willingness
to use that tool. The failure looks like this:

```
read_file README.md   →  [sparse-read] card, 5.5 KB → 1.5 KB
bash sed -n '28,35p' README.md   →  the raw bytes, no gate
```

The card saved 4 KB and the next command spent it back. When a capability hands
you a card, a reference, or a summary, the way to get the detail is the escape
hatch it names — the read tool's own `offset`/`limit`, the block id, the ref —
not a different tool that happens to reach the same bytes.

Concretely, in this repository:

- A `[sparse-read]` card names the exact `read_file { path, offset, limit }`
  calls for each section. Use one of those. Do not `cat`, `sed`, `head` or `tail`
  the file to get around the card.
- A whole-file dump through the shell (`cat FILE`, `type FILE`,
  `Get-Content FILE`) is carded by the same gate, so the detour costs a round
  trip and gains nothing.
- A paged read is never carded. Asking for lines 28-35 is the intended
  behaviour, not a workaround.

If the escape hatch is missing or wrong, that is a bug in the capability. Fix the
capability; do not route around it.

### 3. Test the real thing, or do not claim a test

A test must exercise the capability the way a user does, end to end, and be
verified by evidence the capability itself produced — a call count above zero, a
byte delta, a visible effect in the transcript.

These are not tests and must never be reported as verification:

- A unit test of a helper that the real path never reaches.
- A "smoke test" that starts a process and checks it is alive.
- A test that passes because the capability is disabled and the code
  short-circuits. If a test asserts "nothing changed", confirm the capability was
  enabled first, or the assertion is vacuous.
- A screenshot of a panel showing zeros.

If the honest answer is "I have not tested this against a real request", say
that. An untested claim is recoverable; a false one is not.

### The order, restated

```
install → register → the agent calls it → a real request proves it → only then claim it works
```

Skipping a step does not save time. It moves the discovery to the user.
