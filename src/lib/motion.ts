import { flushSync } from 'react-dom';
import { useEffect, useRef, useState, type CSSProperties } from 'react';

/**
 * The motion vocabulary, in code.
 *
 * Five primitives, no library. The runtime already has `linear()` easing, view
 * transitions, scroll-driven timelines and WAAPI, so a dependency would be a
 * dependency for what the platform does. See `docs/PLAN-STATUS.md`.
 *
 * Everything here is token-driven: the durations and curves live in
 * `src/styles/motion.css`, and the one thing that cannot be written in CSS —
 * a spring curve — is generated here and written back into the same token
 * namespace, so a stylesheet never has to know where a curve came from.
 */

/* ------------------------------------------------------------------ *
 * 1. springToLinear
 * ------------------------------------------------------------------ */

export interface SpringSpec {
  /** Spring constant. Higher is faster. */
  stiffness: number;
  /** Damping coefficient. Lower overshoots more. */
  damping: number;
  /** Mass at the end of the spring. */
  mass: number;
  /** How many points the curve is sampled at. */
  samples?: number;
  /** Envelope height at which the spring is considered settled. */
  epsilon?: number;
}

/**
 * Sample a damped harmonic oscillator into a CSS `linear()` easing.
 *
 * The curve is the position of a unit mass released one unit from its target,
 * normalised so it starts at 0 and ends at 1. An underdamped spring overshoots
 * on the way, which is what makes an arrival read as physical rather than as a
 * fade with a delay.
 *
 * `linear()` takes output progress at evenly spaced input times, so the sample
 * window is the settle time: the point where the envelope has decayed below
 * `epsilon`. Sampling past that would spend half the curve on a flat line.
 */
export function springToLinear({
  stiffness,
  damping,
  mass,
  samples = 40,
  epsilon = 0.005,
}: SpringSpec): string {
  const omega0 = Math.sqrt(stiffness / mass);
  const zeta = damping / (2 * Math.sqrt(stiffness * mass));

  // The envelope is `e^(-zeta * omega0 * t)`, so this is where it falls below
  // `epsilon`. A critically damped spring decays as `(1 + omega0 t) e^(-omega0 t)`,
  // which needs roughly twice as long for the same envelope.
  const settle =
    zeta < 1
      ? -Math.log(epsilon) / (zeta * omega0)
      : (-Math.log(epsilon) / omega0) * 2;

  const omegaD = zeta < 1 ? omega0 * Math.sqrt(1 - zeta * zeta) : 0;

  const at = (t: number): number => {
    if (zeta < 1) {
      const decay = Math.exp(-zeta * omega0 * t);
      return 1 - decay * (Math.cos(omegaD * t) + ((zeta * omega0) / omegaD) * Math.sin(omegaD * t));
    }
    // Critically damped and overdamped both arrive without overshoot; the
    // critically damped form is the one that reads as a spring.
    return 1 - Math.exp(-omega0 * t) * (1 + omega0 * t);
  };

  const points: string[] = [];
  for (let i = 0; i < samples; i += 1) {
    const t = (i / (samples - 1)) * settle;
    const value = i === 0 ? 0 : i === samples - 1 ? 1 : at(t);
    points.push(value.toFixed(4));
  }

  return `linear(${points.join(', ')})`;
}

/**
 * The two springs the interface uses.
 *
 * `soft` is for something that arrives and should settle — a panel, a card.
 * `snappy` is for something the user moved — a switch, a toggle — where the
 * overshoot is the acknowledgement.
 */
export const SPRINGS = {
  soft: { stiffness: 120, damping: 16, mass: 1 },
  snappy: { stiffness: 380, damping: 26, mass: 1 },
} as const;

/**
 * Write the generated curves into the token namespace.
 *
 * Called once at boot. The alternative — pasting the generated `linear()` into
 * the stylesheet — is a second source of truth that drifts the first time
 * somebody tunes a spring.
 */
export function installSprings(): void {
  const root = document.documentElement;
  root.style.setProperty('--vd-spring-soft', springToLinear(SPRINGS.soft));
  root.style.setProperty('--vd-spring-snappy', springToLinear(SPRINGS.snappy));
}

/* ------------------------------------------------------------------ *
 * 2. stagger
 * ------------------------------------------------------------------ */

/**
 * The per-item delay index for a staggered list.
 *
 * CSS does the arithmetic: `.vd-stagger > *` multiplies `--vd-i` by
 * `--vd-stagger` and by the motion scale, so a reduced-motion user gets no
 * delay at all without this function knowing about it.
 *
 * Only for a list that mounts once. A stagger on a list that re-renders on a
 * data tick is N animations per tick for no information.
 */
export function stagger(index: number): CSSProperties {
  // A custom property is not in `CSSProperties`, so the object is asserted
  // rather than widened: the alternative is a `Record<string, string>` that
  // accepts any typo.
  return { '--vd-i': String(index) } as CSSProperties;
}

/* ------------------------------------------------------------------ *
 * 3. motionScale
 * ------------------------------------------------------------------ */

/**
 * The current motion scale: 1, 0.5 or 0.
 *
 * Read from the token rather than recomputed, so the number a tween uses is the
 * same number the stylesheet uses. `applyLayout` writes `data-motion` and the
 * stylesheet derives the scale from it, which keeps the OS media query live
 * without a listener in JavaScript.
 */
export function motionScale(): number {
  const raw = getComputedStyle(document.documentElement).getPropertyValue('--vd-motion-scale');
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) ? value : 1;
}

/* ------------------------------------------------------------------ *
 * 4. useTween
 * ------------------------------------------------------------------ */

/**
 * Animate a number towards its new value instead of jumping to it.
 *
 * A live panel number that jumps is unreadable: the eye cannot tell 292 from
 * 295 when the digits are replaced in one frame. Tweening the value makes the
 * change legible and makes the direction of travel visible.
 *
 * The first value is not animated — a panel that opens with a tween from zero
 * is a panel that lies about its data for 300 ms. Only a change tweens.
 */
export function useTween(value: number | null, duration = 320): number | null {
  const [shown, setShown] = useState<number | null>(value);
  const from = useRef<number | null>(value);
  const frame = useRef<number | null>(null);

  useEffect(() => {
    if (value === null) {
      from.current = null;
      setShown(null);
      return;
    }

    const start = from.current;
    from.current = value;

    // Nothing to animate from, or motion is off: land on the value.
    if (start === null || start === value || motionScale() === 0) {
      setShown(value);
      return;
    }

    const span = duration * motionScale();
    const began = performance.now();

    const step = (now: number) => {
      const t = Math.min(1, (now - began) / span);
      // The same curve the stylesheet uses for an arrival, so a number and the
      // card it sits in move together.
      const eased = 1 - Math.pow(1 - t, 3);
      setShown(start + (value - start) * eased);
      if (t < 1) {
        frame.current = requestAnimationFrame(step);
      } else {
        frame.current = null;
      }
    };

    frame.current = requestAnimationFrame(step);
    return () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      frame.current = null;
    };
  }, [value, duration]);

  return shown;
}

/* ------------------------------------------------------------------ *
 * 5. transitionTo
 * ------------------------------------------------------------------ */

/** Which element a view transition is allowed to snapshot. */
export type TransitionKind = 'panel' | 'chat';

/**
 * Run a state change inside a view transition, cross-fading one element.
 *
 * The snapshot is bounded to a single named element — the panel surface or the
 * transcript — rather than the window. A whole-window snapshot on every panel
 * open is a full-surface capture for a change that touches one column, and the
 * theme reveal already owns the one whole-window transition the app can afford.
 *
 * The name is applied through a `data-vt` attribute for the duration of the
 * transition only. A static `view-transition-name` would also capture these
 * elements during the theme reveal, which would punch a hole in its mask.
 */
export function transitionTo(kind: TransitionKind, change: () => void): void {
  if (!document.startViewTransition || motionScale() === 0) {
    change();
    return;
  }

  const root = document.documentElement;
  root.dataset.vt = kind;

  const transition = document.startViewTransition(() => flushSync(change));
  const done = () => {
    delete root.dataset.vt;
  };
  transition.finished.then(done, done);
}
