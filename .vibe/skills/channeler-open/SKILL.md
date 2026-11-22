---
name: channeler-open
description: Load when choosing how to open a channel — which backend, which host, whether to capture — or when a channel_open reply's capabilities mean you cannot do what you planned. Covers the desktop versus hidden-desktop trade and the hardware versus synthetic input split.
---

# Opening a channel

`channel_open` returns `{id, capabilities, look}`. The `capabilities` are the
contract: read them before you plan a sequence, because two of the fields decide
what is possible.

```json
{ "backend": "window", "host": "hidden_desktop", "foreground": false,
  "input": "synthetic", "actions": [...], "capture": false, "issues": true }
```

## `foreground` — does acting take the user's screen

| Value | Meaning |
| --- | --- |
| `false` | The test runs while the user works. Nothing they are doing is disturbed. |
| `true` | Acting brings the target forward. The user's screen is taken for the duration. |

A `webview` channel is `foreground: false` even though it renders on the user's
desktop, because CDP is out of band: a click is a protocol message, not a
cursor. An `engine` channel is `foreground: false` because it runs a command and
reads its output — there is no window at all. A `window` channel on the desktop
is `foreground: true`.

## `input` — what actually reaches the target

This is the field that decides whether you can drive a game.

| Value | Mechanism | Reaches |
| --- | --- | --- |
| `hardware` | `SendInput` | Everything, including an app reading raw input or polling DirectInput |
| `synthetic` | Posted window messages, or CDP input events | Anything that listens for them; **not** an app reading raw input |

Measured, not assumed: `SendInput` does not reach a desktop that is not the
active input desktop. On a hidden desktop it either returns zero or reports
success and delivers nothing — the cursor never moves. So a hidden desktop is
always `synthetic`.

The consequence: **a game that polls raw input cannot be driven on a hidden
desktop.** A game that handles `WM_KEYDOWN` can. If you need hardware input, the
channel has to be on the desktop and the user's screen is taken.

## Choosing the host

| Host | Use when | Cost |
| --- | --- | --- |
| `desktop` (default) | The user is watching, or you need `hardware` input | Takes the foreground while acting |
| `hidden-desktop` | The user is working and the test should stay out of the way | `synthetic` input only |

`hidden-desktop` creates a desktop of its own and runs the target there. The
window is invisible to the user's desktop — not merely unfocused — and the
cursor does not move.

A `webview`, `process` or `engine` channel never needs a host: none of them
takes the foreground.

## Starting the program yourself

Pass `command` (and `args`) to `window` when no window is open yet. The channel
starts it **on its own host**, so a `hidden-desktop` channel launches the program
on the hidden desktop. A program you started yourself, on the user's desktop, is
not visible to a hidden-desktop channel — they are different desktops.

## Capture

`capture: true` on `channel_open` or `channel_look` asks for a picture of the
stage. The reply carries `capture` (base64) and `capture_type` (the media type),
so you never have to sniff which format you got.

| Backend | Capture | Format | How |
| --- | --- | --- | --- |
| `webview` | yes | `image/jpeg` | Chromium's own screenshot, over the protocol |
| `window` | yes | `image/png` | The window renders itself into a buffer this process owns |
| `process` | no | — | A pipe has no pixels |
| `engine` | no | — | There is nothing to photograph; `look` returns the parsed tests |

`window` capture works on a hidden desktop, which is the point: the request is a
message to the window, not a read of the screen, so it does not depend on which
desktop is in front and it never photographs what the user is looking at.

The limit is real and it is named: a window drawing through an exclusive-fullscreen
swap chain, or one whose content is protected, has no redirection surface to
render. The call fails with a message saying so rather than returning a black
rectangle that looks like a successful capture. A minimised window has no client
area and reports nothing to photograph — that is a state, not a failure.

## The `engine` backend — a test suite, not a UI

`engine` runs a command and turns its output into one outcome per test. It is
the backend for "does the game still work", not "what is on screen".

```json
{ "backend": "engine", "command": "cargo", "args": ["test"],
  "cwd": "D:/project", "format": "cargo", "timeout_ms": 120000 }
```

`format` decides the parser, and a format that does not match the output reports
**no tests** rather than a silent pass — so a wrong `format` is visible, not
invisible.

| `format` | Reads | Use for |
| --- | --- | --- |
| `exit` | nothing, only the exit code | Anything with no test protocol |
| `tap` | `ok` / `not ok` lines | `node --test`, `prove`, anything TAP |
| `cargo` | `test NAME ... ok` / `FAILED` | `cargo test` |
| `pytest` | `PASSED` / `FAILED` lines and the summary | `pytest -v` |

`open` runs the suite once. `look` is cheap and returns that run. `run` re-runs
it, optionally with a `filter` — which needs a `filter_args` template declared at
`open` (e.g. `["-k", "{filter}"]` for pytest). A `filter` with no template is
refused rather than run in full, because appending a bare argument does the wrong
thing for most runners.

A run that overruns `timeout_ms` is killed **with its descendants** and reported
as `timed_out` — a test runner is a process that starts processes, so killing
only the parent would leave the suite running.

## When the reply says you cannot

If `capabilities.actions` does not contain what you need, the backend does not
have it and no amount of retrying will add it. `process` has only `type`, `wait`
and `read` — it has no `click`, because a pipe has nothing to click. `engine` has
only `run`, `wait` and `read` — it has no `click` either, because a test suite
has no cursor.

If a desktop channel's window will not come forward, the action fails with a
message saying so rather than clicking into whatever is in front of it. That is
the guardrail working: close what is in front, or reopen on a hidden desktop.
