---
name: channeler
description: Load when you need to drive something you just built — a page, a native window, a CLI, a game — and find out whether it actually works. Covers the six channel_* tools, the loop they form, and the honest limits of each backend.
---

# The channeler

You build things and cannot check them. The channeler is the input channel that
lets you drive what you built and learn what broke, in the same breath as the
action that broke it.

Six tools, one loop:

```
channel_open  →  channel_look  →  channel_act  →  channel_expect  →  channel_close
                                        ↓
                                  channel_issues
```

## The one measurement that shapes everything

A CDP call costs milliseconds. An agent turn costs seconds. So `channel_act`
takes a **sequence**, not a single action. A hundred clicks as a hundred tool
calls is a hundred turns; as one sequence it is one.

If you find yourself calling `channel_act` once per click, stop and batch.

## Pick the backend before you open

| You built | Backend | Why |
| --- | --- | --- |
| A page, a widget, a canvas game in this app | `webview` | Driven over the DevTools protocol, in-process, no port |
| A native window, or a program that makes one | `window` | Read through UI Automation, so it needs no SDK and no cooperation from the target |
| A CLI, a server, a script | `process` | Its pipes, its exit code |
| A project with a test suite | `engine` | Runs the suite and turns its output into one outcome per test |

`engine` is the one that answers "does it still work" rather than "what is on
screen". It runs a command you declare and parses the result — `tap`, `cargo`,
`pytest`, or `exit` for a bare exit code. A format that does not match the output
reports **no tests**, never a silent pass, so a wrong `format` is visible.

A game engine is driven through `window` (its window) or `engine` (its headless
test runner). There is no Unity backend and no Godot backend, and there does not
need to be one: the depth is in the parsers, not in a list of engines.

Load `channeler-open` for the host and capture decisions, which are the ones
that decide whether the user's screen is taken.

## The loop, in order

1. **Open.** Read the `capabilities` in the reply before planning anything. It
   tells you `foreground` (does acting take the user's screen) and `input`
   (`hardware` or `synthetic`). Those two decide what you can ask for.
2. **Look.** You get a text digest, the refs you can act on, and the stage
   rectangle. Everything the guardrail allows is inside the stage.
3. **Act.** One sequence. It stops at the first failure, because the steps after
   a failed step were written assuming it worked.
4. **Expect.** The assertion. `channel_expect` fails on any new error-level
   issue. Use it as the last step rather than reading issues yourself.
5. **Close.** A process channel kills its child.

## Read `ok`, not `isError`

A failed assertion is a **successful tool call**. `channel_expect` returns
`{"ok": false, "issues": [...]}` with `isError: false`, because the tool worked
and the answer is "no". Parse the body.

The same is true of `channel_act`: `ok: false` with `stopped_at` naming the step
that failed is a normal reply, not a transport error.

## What is not there, and is not pretended to be

- **A picture of a protected or exclusive-fullscreen window.** `window` capture
  renders the window into a buffer this process owns, which works on a hidden
  desktop and never photographs the user's screen. A window with no redirection
  surface — exclusive fullscreen, protected content — has nothing to render, and
  the call says so instead of returning a black rectangle.
- **Cross-session control.** A channel runs on the user's desktop or on a desktop
  this process creates. It cannot reach another logon session.

## The other three skills

- `channeler-open` — backend, host, capture, and what each combination can do.
- `channeler-drive` — writing a sequence that does not waste turns.
- `channeler-verify` — turning a run into a verdict, and mapping an issue to a
  cause.
