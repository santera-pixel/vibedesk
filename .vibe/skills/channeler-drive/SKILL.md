---
name: channeler-drive
description: Load when writing a channel_act sequence — batching actions, using refs versus points, holding keys for a game, waiting for something to settle, or when a sequence was refused by the guardrail. Covers the mistakes that waste turns.
---

# Driving a channel

## Batch, always

`channel_act` takes an array. One call, many steps. The cost of a tool call is
seconds; the cost of a step is milliseconds. A sequence of twenty steps is one
turn; twenty calls is twenty turns.

The sequence **stops at the first failure**, because the steps after a failed
step were written assuming it worked. So order matters: put the step you are
least sure about early, and read `stopped_at`.

## Refs are renumbered on every look

This is the trap. `channel_look` clears the ref table and starts again at `e1`.
So:

- A ref is valid **only until the next look**. Look, then act. Do not look,
  look again, then act on a ref from the first look.
- Between the look and the act, a ref survives a layout shift: `act` re-resolves
  the element's rectangle fresh. That is the point of refs over coordinates.
- The guardrail reads the label from the **last look**, so a ref whose label you
  never saw cannot be judged.

If a ref is gone, the error says so. Look again rather than guessing a point.

## Refs versus points

| Use | When |
| --- | --- |
| A ref (`"e3"`) | The thing has a name and you want the guardrail to read it |
| A point (`{x, y}`) | A canvas, a game, anything with no accessible name |

A point is unchecked beyond "is it inside the stage". A ref carries its label, so
the guardrail can see that it says Delete.

## The action kinds

| Kind | Fields | Notes |
| --- | --- | --- |
| `click` | `target`, `button`, `count` | `button` is left, right or middle |
| `hold` | `key`, `frames` or `ms` | How a game is driven |
| `drag` | `from`, `to` | Twelve interpolated moves, because a drag that teleports is a drag no handler sees |
| `type` | `text`, optional `target` | With a target it clicks first, so focus lands where you meant |
| `scroll` | optional `target`, `dx`, `dy` | No target scrolls the stage centre |
| `tick` | `frames` | Advance the simulation. **`webview` only** |
| `wait` | `ms` | Wall clock |
| `read` | `target` | The target's text, for an assertion |
| `run` | optional `filter` | Re-run the suite. **`engine` only** |

`tick` is refused on a `window` channel: a native window has no frame clock this
backend can observe, so a tick would be a sleep wearing the word "tick". Use
`wait` there. Check `capabilities.actions` if you are unsure — a kind that is not
in the list is refused before dispatch, with the backend named.

`run` is the `engine` backend's verb: it re-runs the declared command and
replaces the issue buffer with the new result, so a test that failed and now
passes stops being an issue. `filter` narrows the run and needs a `filter_args`
template declared at `open`; without one it is refused rather than run in full.

## Holding a key for a game

A tap is one frame and does nothing. A hold is what moves a character.

Prefer `frames` over `ms`: frames are what the simulation counts, and a hold
measured in milliseconds drifts with the frame rate.

```json
{ "kind": "hold", "key": "ArrowRight", "frames": 40 }
```

On a `webview` channel `tick` advances real animation frames. On a `window`
channel it is refused, because there is no frame clock to observe.

## Waiting

`wait` is wall clock and always available. Use it after an action that starts
something asynchronous — a navigation, a save, a spawn — before the step that
depends on it.

Do not sprinkle waits. A wait that is not needed is a second of every run.

**A sequence drains the issues its own steps produced.** They come back in the
reply's `issues`, capped at ten with `more_issues` for the rest. So an issue that
appears *during* a sequence is consumed by that sequence, and a later
`channel_expect` will not see it. If you need to assert on something a step
caused, read the `issues` in the `channel_act` reply, or wait outside the
sequence and let `expect` catch it.

## The guardrail

Before a sequence runs, the guardrail looks for a **destructive label followed by
a confirm label** in the same sequence. If it finds one, the whole sequence is
refused before any step runs.

Destructive: delete, remove, discard, uninstall, approve, install, pay, purchase,
reset, clear, erase, destroy, revoke.
Confirm: confirm, yes, ok, okay, continue, proceed, and the destructive words
again.

Matching is whole-word, so "Undelete" does not read as "delete" and "Cancel" does
not read as "clear".

**When it blocks, do not just add `allow_destructive: true`.** The block means
your test is about to change state the next test depends on. Ask whether the
sequence needs to be destructive at all. If it does, pass the flag deliberately
and say why.

A single destructive action with no confirm after it is not a pair and is not
blocked. The pattern is the pair, because the pair is what actually destroys
something.

## Points must be inside the stage

Every point is checked against the stage rectangle from the last look. A point
outside it is refused before dispatch. If you are computing a point, take it
from a target's rectangle rather than inventing coordinates.

A scope refusal is **not** in `blocked`. `blocked` is the destructive pre-check
only, which refuses the whole sequence before any step runs. A scope refusal is a
failed step: it appears in `results` with `stopped_at` naming its index.
