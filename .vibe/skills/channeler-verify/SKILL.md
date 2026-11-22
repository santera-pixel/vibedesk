---
name: channeler-verify
description: Load when turning a channel run into a verdict — asserting with channel_expect, reading the issue channel, or working out why a step failed. Covers what counts as passing and how to map an issue to its cause.
---

# Verifying a run

A run that did not throw is not a run that worked. The channeler's whole point is
that it tells you what broke, and it only tells you if you ask.

## The assertion

`channel_expect` fails on any **new error-level** issue since the last call. It
is the last step of a test, not an optional extra.

```json
{ "ok": false, "issues": [ { "level": "error", "source": "console", "text": "...", "count": 3 } ] }
```

**Read `ok` in the body.** A failed assertion is a successful tool call:
`isError` is `false`, because the tool worked and the answer is "no". An agent
that checks only `isError` reports every broken run as a pass.

`allow_issues: true` turns the assertion off. Use it when you are deliberately
provoking an error and want to assert on it — not to make a red run green.

## The issue channel

Issues are collected continuously, not only during `act`. Each has a `level`
(`info`, `warn`, `error`), a `source`, the `text`, and a `count` for repeats.

| Source | What it means |
| --- | --- |
| `console` | A console error or warning from the page |
| `exception` | An uncaught exception |
| `network` | A failed request |
| `dialog` | A dialog opened |
| `debug` | An `OutputDebugString` line from a native target |
| `stderr` | A child process wrote to stderr |
| `exit` | A child process exited non-zero |
| `test` | A test in an `engine` suite failed |
| `engine` | An `engine` run exited non-zero with no failing test, or was killed for overrunning |

`channel_issues` drains what is new. `channel_act` also returns the issues its
own steps produced, capped at ten with `more_issues` telling you the remainder —
so a sequence that fires a hundred errors does not bury the reply.

**That drain is a trap.** An issue raised during a sequence is consumed by that
sequence. A `channel_expect` afterwards will not see it, because it is no longer
new. So:

- To assert on something a step caused, read the `issues` in the `channel_act`
  reply.
- To let `expect` catch it, wait outside the sequence — a real pause, not a
  `wait` action — and let the issue arrive before you assert.

## Two kinds of refusal

| Field | Meaning |
| --- | --- |
| `blocked` | The destructive pre-check refused the whole sequence before any step ran. Names both steps and both labels. |
| `results[i]` with `stopped_at: i` | A single step was refused or failed — a point outside the stage, an unsupported action kind, a ref that is gone. |

`blocked` is never set for a scope refusal. If `blocked` is null and `ok` is
false, the reason is in `results`.

## What counts as passing

All three, not one:

1. `channel_act` returned `ok: true` with `stopped_at: null`.
2. `channel_expect` returned `ok: true`.
3. The thing you actually cared about is true — a `read` returned the text you
   expected, or the capture shows what you meant.

Step 3 is the one that gets skipped. A click that lands on the right pixel and
does nothing is `ok: true` and `expect: ok`. Assert on an observable effect:
read a label, read a value, compare a capture.

## Mapping an issue to a cause

Work backwards from the first error, not the last. The first error is usually the
cause and the rest are consequences.

| Symptom | Look at |
| --- | --- |
| `stopped_at` names a step, no issue | The step's own result `detail` — it says what it refused or could not find |
| A ref is gone | The page re-rendered or navigated. Look again; refs renumber per look |
| A point was refused | It fell outside the stage. Take the point from a target's rectangle |
| `blocked` is set | A destructive label followed by a confirm. It names both steps and both labels |
| `blocked` is null but `ok` is false | A single step failed. Read `results[stopped_at]` |
| `ok: true` but nothing happened | Wrong target, or the input did not reach it. Check `capabilities.input` |
| A click lands on the wrong thing | A desktop channel whose window would not come forward. The action should have failed; if it did not, the window moved |
| A game ignores every key | `input: synthetic` and the game reads raw input. It needs a desktop channel |
| `expect` passes after a run that clearly broke | The sequence already drained that issue. Read the `issues` in the `channel_act` reply |

## Do not report a pass you did not observe

If you did not run `channel_expect`, you do not know the run was clean. If you
did not read the value back, you do not know the click worked. Say which of the
three checks you actually performed.
