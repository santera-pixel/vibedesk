import { flushSync } from 'react-dom';
import { useApp } from '@/store/app';

type Theme = 'ink' | 'paper';

/** Where a reveal spreads from, in viewport pixels. */
export interface Origin {
  x: number;
  y: number;
}

/** The centre of the window, for a reveal with no pointer behind it. */
export function centreOrigin(): Origin {
  return { x: window.innerWidth / 2, y: window.innerHeight / 2 };
}

/**
 * Whether motion is suppressed, honouring the in-app preference first.
 *
 * The OS setting is machine-wide and blunt. Windows reports
 * `prefers-reduced-motion: reduce` whenever "Animation effects" is off, which is
 * a battery and accessibility switch, not a statement about this app. So the
 * layout carries a `motion` preference: `full` animates regardless of the OS,
 * `reduced` never animates, and `system` — the default — defers to the media
 * query. The attribute is written by `applyLayout`, so this reads the same value
 * the stylesheet does and the two can never disagree.
 */
function reduced(): boolean {
  const pref = document.documentElement.dataset.motion;
  if (pref === 'full') return false;
  if (pref === 'reduced') return true;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/**
 * The reveal curve, and why it is not `--vd-ease`.
 *
 * `--vd-ease` is `cubic-bezier(0.32, 0.72, 0, 1)`: 77% of the distance in the
 * first quarter of the duration, then a long crawl. That is right for a 140 ms
 * press, where the whole thing is over before the crawl is visible. Over 520 ms
 * across a whole window it reads as a pop followed by a stall, so the reveal
 * gets its own curve — a mild ease-out that spends its time evenly.
 */
const REVEAL_EASE = 'cubic-bezier(0.25, 0.46, 0.45, 0.94)';
const REVEAL_MS = 520;

/** The animation currently holding the incoming snapshot open. */
let running: Animation | null = null;

/**
 * Switch the theme, spreading the new one from the pointer like ink.
 *
 * A theme change is the only change that touches the whole window, so it is
 * the only place a view transition earns its snapshot. Both pseudo-elements
 * are frozen by `motion.css` and a single WAAPI animation grows a circular
 * mask on the incoming snapshot — one animation on one layer, no per-frame
 * JavaScript, no layout.
 *
 * Falls back to an instant switch when the API is missing or the user asked
 * for reduced motion. The mask is sized to nothing in CSS, so with no
 * animation running the incoming snapshot stays hidden and the transition
 * ends on the new theme. That is the fallback.
 */
export function revealTheme(next: Theme, origin: Origin = centreOrigin()) {
  const apply = () => {
    // Written here rather than left to the effect in App: the snapshot is
    // taken when this callback returns, so the attribute has to be on the
    // document before then.
    document.documentElement.dataset.theme = next;
    useApp.getState().setTheme(next);
  };

  if (!document.startViewTransition || reduced()) {
    apply();
    return;
  }

  // A second toggle while the first is still settling would stack a second
  // animation on the same pseudo-element. The first one is finished with.
  running?.cancel();
  running = null;

  const transition = document.startViewTransition(() => flushSync(apply));
  const { x, y } = origin;
  // Far enough to cover the furthest corner from the pointer.
  const radius = Math.hypot(
    Math.max(x, window.innerWidth - x),
    Math.max(y, window.innerHeight - y),
  );

  transition.ready
    .then(() => {
      const animation = document.documentElement.animate(
        [
          { maskSize: '0px 0px', maskPosition: `${x}px ${y}px` },
          {
            maskSize: `${radius * 2}px ${radius * 2}px`,
            maskPosition: `${x - radius}px ${y - radius}px`,
          },
        ],
        {
          duration: REVEAL_MS,
          easing: REVEAL_EASE,
          // Without this the mask snaps back to its CSS size the moment the
          // animation ends, which is a frame or two before the pseudo-element
          // tree is torn down.
          fill: 'forwards',
          pseudoElement: '::view-transition-new(root)',
        },
      );
      running = animation;
      // The tree goes away when the transition finishes; an animation still
      // filling a dead pseudo-element is a leak that outlives the reveal.
      transition.finished
        .finally(() => {
          animation.cancel();
          if (running === animation) running = null;
        })
        .catch(() => {
          /* a skipped transition rejects `finished`; nothing to clean up */
        });
    })
    .catch(() => {
      /* the transition was skipped — the theme is already applied */
    });
}
