# VibeDesk — design direction

This file is the direction the interface is judged against. It exists because a
filter cannot invent taste: rules can remove slop, but only a stated direction
produces a look. Every value below is a decision, not a default.

Authored from the owner's brief: *"smoothness and consistent motion as a base;
a bit glossy, with the orange taint of Mistral as default."*

## Identity

A polished dark instrument panel for driving an agent. Warm graphite ground,
one hot orange accent, specular edges. It should read as a machined object —
something with a light source above it — not as a web page with a dark theme.

The product is a developer tool, so dark is a decision with a reason: the
transcript is read for long stretches, and code blocks need a ground that does
not glare. A light theme exists and is maintained, not bolted on.

## Dials

| Dial | Value | What it means here |
|---|---|---|
| ENERGY | 3 | The accent is hot and present. It is dosed, not spread. |
| RHYTHM | 2 | Sections vary by content, but this is an app shell, not a landing page. The transcript, the rail and the composer are deliberately different weights. |
| MOTION | 2 | Hover, press, enter and state transitions. No ambient loops except where a loop *is* the state. |

## Palette

Two themes, one geometry. Colour is capped at a neutral base, one accent, and
three semantic hues that appear only when they carry meaning.

### ink (default)

| Token | Value | Role |
|---|---|---|
| `--vd-canvas` | `#0c0a09` | Ground |
| `--vd-canvas-raised` | `#131010` | Rail, sidebar |
| `--vd-surface` | `#191514` | Cards, bubbles |
| `--vd-surface-hover` | `#201b19` | Hover ground |
| `--vd-surface-sunken` | `#100d0c` | Wells, code |
| `--vd-line` | `#2b2422` | Hairline |
| `--vd-line-strong` | `#3d332f` | Emphasis hairline |
| `--vd-text` | `#f2ede8` | Primary |
| `--vd-text-secondary` | `#a89e96` | Secondary |
| `--vd-text-tertiary` | `#6f655e` | Meta |
| `--vd-accent` | `#ff7a1a` | Mistral orange |
| `--vd-accent-strong` | `#ff9445` | Hover |
| `--vd-on-accent` | `#1a0c00` | Text on accent |

`#ff7a1a` is Mistral's `#ff7000` lifted a few points so it holds up as small
text on a near-black ground. Contrast against `--vd-canvas` is 6.4:1.

### paper

Warm off-white, same accent family darkened to `#c2410c` so it clears 5.9:1 on
`#f5f3ee`. Not a grey theme with an orange button — the ground is warm.

## Gloss

"Glossy" here means **a light source above the object**, not frosted glass.
Three devices, each dose-capped:

| Device | Where | Cap |
|---|---|---|
| Specular top edge (`inset 0 1px 0`) | Raised surfaces, cards, composer | Every elevated surface — it is a material property, not an accent |
| Warm sheen (one gradient, top of shell) | Title bar only | 1 element |
| Accent glow | Primary action on hover | 1 element |

No `backdrop-filter` anywhere. Glass is at zero: blurring every surface flattens
hierarchy, and this interface has real hierarchy to protect.

## Motion contract

One grammar, in `src/styles/motion.css`. Every transition in the app draws from
this scale — a raw millisecond value in a component is a bug.

| Token | Value | Use |
|---|---|---|
| `--vd-fast` | 140ms | State changes: press, toggle, hover lift, expand |
| `--vd-slow` | 300ms | Arrivals: a row, a panel, a menu |
| `--vd-dur-spin` | 900ms | The one loop, and only while work is happening |

| Easing | Curve | Use |
|---|---|---|
| `--vd-ease` | `cubic-bezier(0.32, 0.72, 0, 1)` | Default: quick out of the gate, long settle |
| `--vd-ease-land` | `cubic-bezier(0.34, 1.56, 0.64, 1)` | Something that arrives and should land with weight |

Rules:

- Animate `transform` and `opacity`. Nothing else. Both are compositor
  properties, so a transition costs a layer update and never a paint or a layout.
- **A colour transition is a paint, and a paint on hover is the most common way
  an interface feels heavy.** Hover states are instant. This is why there is no
  `transition-colors` anywhere in the app.
- Never `width`, `height`, `top`, `left`. A meter animates `scaleX`, not `width`.
  The one exception is the rail resize, which writes a custom property directly
  during a drag.
- Press is `translateY(0.5px) scale(0.995)` at `--vd-fast`. Every button, the
  same. Consistency is the whole point.
- One entrance: `.vd-enter`, `opacity 0→1` with `translateY(4px)→0`, one shot.
  No stagger — a stagger is N animations where one would do, and it delays the
  last item for no information.
- Expand and collapse use `grid-template-rows: 0fr ↔ 1fr` (`.vd-expand`), which
  is bounded to the subtree and needs no measuring.
- Scroll edges fade from a scroll-driven mask (`.vd-scroll-fade`), not a
  listener: no scroll event, no re-render, no layout.
- One loop is allowed, and only where the loop *is* the state: `vd-spin`, while a
  turn is running. Nothing else repeats.
- One whole-window effect is allowed, and only because the whole window is what
  changed: the theme reveal, a view transition with both snapshots frozen and a
  single mask animation spreading from the pointer.
- `prefers-reduced-motion` sets both durations to 1ms through one media query.
  Not a tier system, and not optional.

## What this interface refuses

Recorded so the refusals are decisions rather than accidents:

- No blue-purple gradient, no purple-and-black, no neon or pastel palette.
- No blurred radial orbs, no background grid or blueprint texture.
- No glassmorphism, no glow on more than one element.
- No pill radius on everything; the radius scale is a hierarchy tool.
- No decorative status dots — every dot marks a real state.
- No uniform card grid where the content has different weights.
- No emoji in headings, bullets or buttons.
- No invented metrics, no fake placeholder data.

## Where the direction is enforced

- `src/styles/globals.css` — tokens, surfaces, gloss, containment.
- `src/styles/motion.css` — the whole motion vocabulary, and nothing else.
- `src-tauri/src/customize.rs` — a user theme may override tokens and add rules
  for `.vd-*` classes, but can never reach the approval bar, the elicitation
  form, the status bar or the composer.
- `src-tauri/src/layout.rs` — geometry as integers and closed enums only.
