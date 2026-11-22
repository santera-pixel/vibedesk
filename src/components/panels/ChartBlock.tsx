import { useEffect, useMemo, useRef, useState } from 'react';
import type {
  ChartReference,
  ChartScale,
  ChartSeries,
  ChartStack,
  ChartStyle,
  DiagramTone,
  Expr,
} from '@/lib/customize';
import {
  display,
  evaluate,
  series as toNumbers,
  seriesCount,
  type DataModel,
} from '@/lib/data';

/**
 * A chart with axes.
 *
 * The block this replaces drew a 38px strip: no scale, no ticks, no labels, one
 * series, no way to read a value off it. That is a sparkline, and calling it a
 * chart was the shallow surface `AGENTS.md` names. This one draws a real plot —
 * a fitted or explicit y-domain, whole-number ticks, gridlines, category or time
 * labels on x, up to four series with a legend, and a hover readout that names
 * the point under the cursor.
 *
 * On top of that it does the four things a plot needs before it can answer a
 * question rather than illustrate one:
 *
 * - **Stacking.** `stack: 'stacked'` accumulates the series so the top of the
 *   stack is the total; `'percent'` normalises each column to 100 so the
 *   question is "what share" rather than "how much". Grouped stays the default,
 *   because stacking two latencies says their sum, which nobody asked for.
 * - **A log scale.** `scale: 'log'` makes each decade the same height, which is
 *   the only way to read a series that spans orders of magnitude. A value at or
 *   below zero has no place on a log axis, so it is dropped and the line breaks
 *   at the gap rather than being drawn at minus infinity.
 * - **Reference lines.** Up to four horizontal thresholds, each an expression,
 *   so a budget drawn on the chart moves when the budget moves. They use the
 *   same tone vocabulary a diagram node does, because a red line means the same
 *   thing in both.
 * - **A legend that filters.** Clicking a series hides it and the axis rescales
 *   to what is left, which is how you read one series out of four.
 *
 * Geometry is computed in CSS pixels from a `ResizeObserver` rather than in a
 * scaled `viewBox`. A `viewBox` with `preserveAspectRatio="none"` stretches the
 * text with the plot, so axis labels come out at whatever aspect ratio the panel
 * happens to have. Measuring costs one observer and makes the labels crisp.
 *
 * `sparkline` is kept as an explicit style: no axes, no labels, no tooltip. It
 * is the honest name for the strip, and a panel that wants a strip should say so
 * rather than get one by accident.
 */

const PAD = { top: 8, right: 8, bottom: 18, left: 38 };
const SPARK_HEIGHT = 38;
const DEFAULT_HEIGHT = 150;

/**
 * Four series, four colours.
 *
 * The first is the theme accent so a single-series chart matches the app. The
 * rest are mid-tone on purpose: they have to read against both the ink and the
 * paper theme, and a palette tuned for one is invisible on the other.
 */
const PALETTE = ['var(--vd-accent)', '#8fb0d4', '#7fae8a', '#c9a05e'];

/** The stroke a reference line takes, from the same vocabulary a node uses. */
const TONE_STROKE: Record<DiagramTone, string> = {
  neutral: 'var(--vd-text-tertiary)',
  accent: 'var(--vd-accent)',
  ok: 'var(--vd-ok)',
  warn: 'var(--vd-warn)',
  danger: 'var(--vd-danger)',
  muted: 'var(--vd-line)',
};

const TONES: DiagramTone[] = ['neutral', 'accent', 'ok', 'warn', 'danger', 'muted'];

interface Props {
  label: string;
  style: ChartStyle;
  values?: Expr | null;
  series?: ChartSeries[];
  x?: Expr | null;
  yMin?: Expr | null;
  yMax?: Expr | null;
  height?: number | null;
  unit?: string | null;
  time?: boolean;
  stack?: ChartStack;
  scale?: ChartScale;
  reference?: ChartReference[];
  model: DataModel;
}

interface Line {
  name: string | null;
  points: number[];
  /** How many numbers the source held before decimation. */
  raw: number;
}

/** Where one series sits at one index: the bottom and the top of its band. */
interface Band {
  base: number[];
  top: number[];
}

export function ChartBlock({
  label,
  style,
  values,
  series,
  x,
  yMin,
  yMax,
  height,
  unit,
  time,
  stack = 'none',
  scale = 'linear',
  reference,
  model,
}: Props) {
  const spark = style === 'sparkline';
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [hover, setHover] = useState<number | null>(null);
  // Indices, not names: two series may share a name, and a name is optional.
  const [hidden, setHidden] = useState<ReadonlySet<number>>(() => new Set());

  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const observer = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width ?? 0;
      setWidth(Math.max(0, Math.round(next)));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const lines = useMemo<Line[]>(() => {
    const declared = series ?? [];
    if (declared.length > 0) {
      return declared.map((one) => {
        const resolved = evaluate(one.values, model);
        return {
          name: one.name ?? null,
          points: toNumbers(resolved),
          raw: seriesCount(resolved),
        };
      });
    }
    if (values != null) {
      const resolved = evaluate(values, model);
      return [{ name: null, points: toNumbers(resolved), raw: seriesCount(resolved) }];
    }
    return [];
  }, [series, values, model]);

  const labels = useMemo<string[]>(() => {
    if (x == null) return [];
    const resolved = evaluate(x, model);
    if (!Array.isArray(resolved)) return [];
    return resolved.map((item) => (time ? clock(item) : display(item)));
  }, [x, model, time]);

  const marks = useMemo<Array<{ value: number; label: string | null; tone: DiagramTone }>>(() => {
    const declared = reference ?? [];
    return declared.flatMap((one) => {
      const value = number(evaluate(one.value, model));
      if (value === null) return [];
      const tone = toneOf(one.tone == null ? null : evaluate(one.tone, model));
      return [{ value, label: one.label ?? null, tone }];
    });
  }, [reference, model]);

  const count = lines.reduce((most, line) => Math.max(most, line.points.length), 0);
  // A decimated series is a summary, and a summary that does not say so is a
  // chart that lies about how much data it drew.
  const raw = lines.reduce((most, line) => Math.max(most, line.raw), 0);
  const note = raw > count ? `${count} of ${raw} points` : null;
  const total = height ?? (spark ? SPARK_HEIGHT : DEFAULT_HEIGHT);

  if (count === 0) {
    return (
      <div className="flex flex-col gap-1.5">
        <Header label={label} value={null} unit={unit} />
        <span className="text-[11px] text-[var(--vd-text-faint)]">—</span>
      </div>
    );
  }

  if (spark) {
    return (
      <div className="flex flex-col gap-1.5">
        <Header label={label} value={last(lines[0])} unit={unit} note={note} />
        <Sparkline points={lines[0]?.points ?? []} height={total} />
      </div>
    );
  }

  // A hidden series leaves the axis as well as the plot: reading one series out
  // of four is the point of the legend, and a series that is not drawn must not
  // still be setting the scale.
  const visible = lines
    .map((line, index) => ({ line, index }))
    .filter(({ index }) => !hidden.has(index));

  const stacked = stack !== 'none' && visible.length > 0;
  const log = scale === 'log';
  const bands = stacked ? stackBands(visible.map((one) => one.line), stack) : null;

  const domain = stacked
    ? stackedDomain(bands ?? [], stack, log)
    : fit(
        visible.map((one) => one.line),
        yMin,
        yMax,
        model,
        style === 'bar',
        log,
      );

  const ticks = log ? logTicks(domain.min, domain.max) : niceTicks(domain.min, domain.max, 4);
  const plotWidth = Math.max(0, width - PAD.left - PAD.right);
  const plotHeight = Math.max(0, total - PAD.top - PAD.bottom);
  const step = count > 1 ? plotWidth / (count - 1) : plotWidth;
  const band = count > 0 ? plotWidth / count : plotWidth;

  // One projection for both the axis and the marks, so a reference line cannot
  // land somewhere the data does not.
  const project = (value: number) => (log ? Math.log10(Math.max(value, Number.MIN_VALUE)) : value);
  const low = project(domain.min);
  const high = project(domain.max);
  const yOf = (value: number) => PAD.top + plotHeight - ((project(value) - low) / (high - low || 1)) * plotHeight;
  const xOf = (index: number) => PAD.left + (style === 'bar' ? band * (index + 0.5) : index * step);

  const xTicks = pickLabels(count, labels, plotWidth);
  const summary = describe(style, stack, scale, visible.length, marks.length);

  return (
    <div className="flex flex-col gap-1.5">
      <Header label={label} value={last(lines[0])} unit={unit} />
      <div ref={box} className="relative w-full" style={{ height: total }}>
        {width > 0 && (
          <svg
            width={width}
            height={total}
            role="img"
            data-vd-chart={style}
            data-vd-chart-stack={stack}
            data-vd-chart-scale={scale}
            aria-label={`${label}: ${summary}. ${visible
              .map((one) => one.line.name ?? 'series')
              .join(', ')}`}
            onPointerMove={(event) => {
              const rect = event.currentTarget.getBoundingClientRect();
              const local = event.clientX - rect.left - PAD.left;
              if (local < 0 || local > plotWidth) {
                setHover(null);
                return;
              }
              const index =
                style === 'bar'
                  ? Math.floor(local / (band || 1))
                  : Math.round(local / (step || 1));
              setHover(index >= 0 && index < count ? index : null);
            }}
            onPointerLeave={() => setHover(null)}
          >
            {ticks.map((tick) => (
              <g key={tick}>
                <line
                  x1={PAD.left}
                  x2={PAD.left + plotWidth}
                  y1={yOf(tick)}
                  y2={yOf(tick)}
                  stroke="var(--vd-line-soft)"
                  strokeWidth={1}
                />
                <text
                  x={PAD.left - 6}
                  y={yOf(tick)}
                  textAnchor="end"
                  dominantBaseline="middle"
                  className="fill-[var(--vd-text-tertiary)] text-[9px]"
                >
                  {compact(tick)}
                </text>
              </g>
            ))}

            {xTicks.map(({ index, text }) => (
              <text
                key={index}
                x={xOf(index)}
                y={total - 5}
                textAnchor={index === 0 ? 'start' : index === count - 1 ? 'end' : 'middle'}
                className="fill-[var(--vd-text-tertiary)] text-[9px]"
              >
                {text}
              </text>
            ))}

            {style === 'bar'
              ? visible.map(({ line, index: lineIndex }, slotIndex) => {
                  const slot = band / Math.max(1, stacked ? 1 : visible.length);
                  const offset = stacked ? 0 : slot * slotIndex;
                  return line.points.map((point, index) => {
                    const base = bands ? (bands[slotIndex]?.base[index] ?? 0) : 0;
                    const top = bands ? (bands[slotIndex]?.top[index] ?? 0) : point;
                    const x = PAD.left + band * index + offset;
                    const yTop = yOf(Math.max(top, domain.min));
                    const yBase = yOf(Math.max(base, domain.min));
                    return (
                      <rect
                        key={`${lineIndex}-${index}`}
                        data-vd-chart-bar={`${lineIndex}-${index}`}
                        x={x}
                        y={Math.min(yTop, yBase)}
                        width={Math.max(1, slot - 1)}
                        height={Math.max(1, Math.abs(yBase - yTop))}
                        fill={PALETTE[lineIndex % PALETTE.length]}
                        opacity={hover === null || hover === index ? 0.9 : 0.45}
                      />
                    );
                  });
                })
              : visible.map(({ line, index: lineIndex }, slotIndex) => {
                  const colour = PALETTE[lineIndex % PALETTE.length];
                  // A log axis has no place for a non-positive point, so the
                  // line breaks there instead of jumping across the gap.
                  const runs = segments(line.points, log);
                  return (
                    <g key={lineIndex}>
                      {style === 'area' &&
                        runs.map((run, runIndex) => {
                          const path = run
                            .map(
                              (index) =>
                                `${xOf(index).toFixed(2)},${yOf(line.points[index] ?? 0).toFixed(2)}`,
                            )
                            .join(' ');
                          const floor = bands
                            ? run
                                .map(
                                  (index) =>
                                    `${xOf(index).toFixed(2)},${yOf(
                                      Math.max(bands[slotIndex]?.base[index] ?? 0, domain.min),
                                    ).toFixed(2)}`,
                                )
                                .reverse()
                                .join(' ')
                            : `${xOf(run[run.length - 1] ?? 0).toFixed(2)},${yOf(domain.min).toFixed(
                                2,
                              )} ${xOf(run[0] ?? 0).toFixed(2)},${yOf(domain.min).toFixed(2)}`;
                          return (
                            <polygon
                              key={runIndex}
                              points={`${path} ${floor}`}
                              fill={colour}
                              opacity={0.14}
                            />
                          );
                        })}
                      {runs.map((run, runIndex) => (
                        <polyline
                          key={runIndex}
                          points={run
                            .map(
                              (index) =>
                                `${xOf(index).toFixed(2)},${yOf(line.points[index] ?? 0).toFixed(2)}`,
                            )
                            .join(' ')}
                          fill="none"
                          stroke={colour}
                          strokeWidth={1.5}
                          strokeLinejoin="round"
                          strokeLinecap="round"
                        />
                      ))}
                    </g>
                  );
                })}

            {marks.map((mark, index) => {
              const y = yOf(mark.value);
              if (y < PAD.top - 1 || y > PAD.top + plotHeight + 1) return null;
              return (
                <g key={`ref-${index}`} data-vd-chart-reference={index}>
                  <line
                    x1={PAD.left}
                    x2={PAD.left + plotWidth}
                    y1={y}
                    y2={y}
                    stroke={TONE_STROKE[mark.tone]}
                    strokeWidth={1}
                    strokeDasharray="4 3"
                  />
                  {mark.label && (
                    <text
                      x={PAD.left + plotWidth - 2}
                      y={y - 3}
                      textAnchor="end"
                      className="text-[9px]"
                      fill={TONE_STROKE[mark.tone]}
                    >
                      {mark.label}
                    </text>
                  )}
                </g>
              );
            })}

            {hover !== null && (
              <line
                x1={xOf(hover)}
                x2={xOf(hover)}
                y1={PAD.top}
                y2={PAD.top + plotHeight}
                stroke="var(--vd-line-strong)"
                strokeWidth={1}
                strokeDasharray="2 2"
              />
            )}

            {/* The crosshair is the difference between a tooltip and a readout:
                a dot on the series says which point the number belongs to. */}
            {hover !== null &&
              visible.map(({ line, index: lineIndex }, slotIndex) => {
                const value = line.points[hover] ?? 0;
                if (!Number.isFinite(value) || (log && value <= 0)) return null;
                const y = bands ? yOf(bands[slotIndex]?.top[hover] ?? 0) : yOf(value);
                return (
                  <circle
                    key={`dot-${lineIndex}`}
                    data-vd-chart-dot={lineIndex}
                    cx={xOf(hover)}
                    cy={y}
                    r={2.5}
                    fill={PALETTE[lineIndex % PALETTE.length]}
                    stroke="var(--vd-canvas-raised)"
                    strokeWidth={1}
                  />
                );
              })}
          </svg>
        )}

        {hover !== null && width > 0 && (
          <div
            data-vd-chart-tooltip
            className="pointer-events-none absolute top-0 z-10 min-w-[86px] rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-canvas-raised)] px-1.5 py-1 shadow-sm"
            style={{
              left: Math.min(Math.max(xOf(hover) + 8, 0), Math.max(0, width - 96)),
            }}
          >
            {labels[hover] && (
              <div className="mb-0.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
                {labels[hover]}
              </div>
            )}
            {visible.map(({ line, index: lineIndex }, slotIndex) => (
              <div key={lineIndex} className="flex items-center gap-1.5 text-[10px]">
                <span
                  className="h-[6px] w-[6px] shrink-0 rounded-[1px]"
                  style={{ background: PALETTE[lineIndex % PALETTE.length] }}
                />
                {line.name && (
                  <span className="text-[var(--vd-text-tertiary)]">{line.name}</span>
                )}
                <span className="ml-auto font-mono text-[var(--vd-text)]">
                  {compact(line.points[hover] ?? 0)}
                  {unit ?? ''}
                </span>
                {stack === 'percent' && (
                  <span className="font-mono text-[var(--vd-text-tertiary)]">
                    {share(bands, slotIndex, hover)}
                  </span>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {lines.length > 1 && (
        <div className="flex flex-wrap gap-x-3 gap-y-1" role="group" aria-label={`${label} series`}>
          {lines.map((line, index) => {
            const off = hidden.has(index);
            return (
              <button
                key={index}
                type="button"
                data-vd-chart-legend={index}
                aria-pressed={!off}
                title={off ? `Show ${line.name ?? `series ${index + 1}`}` : `Hide ${line.name ?? `series ${index + 1}`}`}
                onClick={() =>
                  setHidden((current) => {
                    const next = new Set(current);
                    if (next.has(index)) next.delete(index);
                    else next.add(index);
                    return next;
                  })
                }
                className="flex items-center gap-1 text-[10px] text-[var(--vd-text-tertiary)] duration-[var(--vd-fast)] hover:text-[var(--vd-text-secondary)]"
                style={off ? { opacity: 0.45 } : undefined}
              >
                <span
                  className="h-[6px] w-[6px] rounded-[1px]"
                  style={{
                    background: off ? 'transparent' : PALETTE[index % PALETTE.length],
                    boxShadow: off ? `inset 0 0 0 1px ${PALETTE[index % PALETTE.length]}` : undefined,
                  }}
                />
                <span className={off ? 'line-through' : undefined}>
                  {line.name ?? `series ${index + 1}`}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function Header({
  label,
  value,
  unit,
  note,
}: {
  label: string;
  value: number | null;
  unit?: string | null;
  note?: string | null;
}) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <h3 className="vd-label m-0">{label}</h3>
      <span className="flex items-baseline gap-2">
        {note && (
          <span className="vd-tabular text-[10px] text-[var(--vd-text-faint)]">{note}</span>
        )}
        {value !== null && (
          <span className="vd-tabular text-[10.5px] text-[var(--vd-text-tertiary)]">
            {compact(value)}
            {unit ?? ''}
          </span>
        )}
      </span>
    </div>
  );
}

function Sparkline({ points, height }: { points: number[]; height: number }) {
  if (points.length === 0) return null;
  const top = Math.max(...points);
  const bottom = Math.min(...points);
  const span = top - bottom || 1;
  const step = points.length > 1 ? 100 / (points.length - 1) : 100;
  const coordinates = points
    .map((point, index) => `${(index * step).toFixed(2)},${(32 - ((point - bottom) / span) * 32).toFixed(2)}`)
    .join(' ');

  return (
    <svg
      viewBox="0 0 100 32"
      preserveAspectRatio="none"
      style={{ height }}
      className="w-full"
      data-vd-chart="sparkline"
      aria-hidden
    >
      <polyline
        points={coordinates}
        fill="none"
        stroke="var(--vd-accent)"
        strokeWidth={1.25}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

function last(line: Line | undefined): number | null {
  if (!line || line.points.length === 0) return null;
  return line.points[line.points.length - 1] ?? null;
}

/**
 * The bands a stacked chart draws.
 *
 * `percent` normalises each column to 100. A negative value has no share of a
 * whole, so it contributes nothing rather than dragging the column below zero —
 * the tooltip still shows the raw number, so the value is not hidden, only
 * excluded from the share.
 */
function stackBands(lines: Line[], mode: ChartStack): Band[] {
  const count = lines.reduce((most, line) => Math.max(most, line.points.length), 0);
  const bands: Band[] = lines.map(() => ({
    base: new Array<number>(count).fill(0),
    top: new Array<number>(count).fill(0),
  }));

  for (let index = 0; index < count; index += 1) {
    const raw = lines.map((line) => line.points[index] ?? 0);
    const values = mode === 'percent' ? raw.map((value) => Math.max(0, value)) : raw;
    const sum = values.reduce((total, value) => total + value, 0);
    const factor = mode === 'percent' && sum > 0 ? 100 / sum : 1;
    let running = 0;
    values.forEach((value, lineIndex) => {
      const band = bands[lineIndex];
      if (!band) return;
      band.base[index] = running;
      running += value * factor;
      band.top[index] = running;
    });
  }

  return bands;
}

/** The domain of a stacked chart: always from zero, because a stack is a total. */
function stackedDomain(bands: Band[], mode: ChartStack, log: boolean): { min: number; max: number } {
  if (mode === 'percent') return { min: log ? 1 : 0, max: 100 };
  const tops = bands.flatMap((band) => band.top).filter((value) => Number.isFinite(value));
  const max = tops.length > 0 ? Math.max(...tops) : 1;
  if (log) {
    const positive = tops.filter((value) => value > 0);
    const low = positive.length > 0 ? Math.min(...positive) : 1;
    return { min: low, max: Math.max(max, low * 10) };
  }
  return { min: 0, max: max === 0 ? 1 : max };
}

/**
 * The y-domain.
 *
 * A bar chart starts at zero, because a bar that does not is a lie about
 * magnitude. A line chart fits its data with a little air, because a flat series
 * pinned to the top and bottom of the box reads as noise. An explicit `yMin` or
 * `yMax` wins over both.
 *
 * A log domain never includes zero: the axis is built from the positive values
 * only, and a series with no positive value falls back to a decade so the plot
 * still has a frame.
 */
function fit(
  lines: Line[],
  yMin: Expr | null | undefined,
  yMax: Expr | null | undefined,
  model: DataModel,
  fromZero: boolean,
  log: boolean,
): { min: number; max: number } {
  const all = lines
    .flatMap((line) => line.points)
    .filter((value) => Number.isFinite(value) && (!log || value > 0));
  const explicitMin = yMin == null ? null : number(evaluate(yMin, model));
  const explicitMax = yMax == null ? null : number(evaluate(yMax, model));

  let min = explicitMin ?? (all.length > 0 ? Math.min(...all) : log ? 1 : 0);
  let max = explicitMax ?? (all.length > 0 ? Math.max(...all) : log ? 10 : 1);

  if (log) {
    if (min <= 0) min = max > 0 ? max / 10 : 1;
    if (max <= min) max = min * 10;
    return { min, max };
  }

  if (fromZero && explicitMin === null) min = Math.min(0, min);
  if (fromZero && explicitMax === null) max = Math.max(0, max);

  if (min === max) {
    // A single value, or a perfectly flat series. Give the axis something to
    // span so the line does not sit on the frame.
    const pad = Math.abs(min) > 0 ? Math.abs(min) * 0.1 : 1;
    if (explicitMin === null) min -= pad;
    if (explicitMax === null) max += pad;
  } else if (!fromZero && explicitMin === null && explicitMax === null) {
    const pad = (max - min) * 0.05;
    min -= pad;
    max += pad;
  }

  return { min, max };
}

/**
 * Whole-number ticks across the domain.
 *
 * The step is 1, 2, 5 or 10 times a power of ten, which is the only set that
 * produces labels a person reads without dividing. `count` is a target, not a
 * promise: a domain of 0..3 with a target of 4 yields 0,1,2,3.
 */
function niceTicks(min: number, max: number, count: number): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max) || max <= min) return [];
  const raw = (max - min) / count;
  const magnitude = Math.pow(10, Math.floor(Math.log10(raw)));
  const normalised = raw / magnitude;
  const step = (normalised <= 1 ? 1 : normalised <= 2 ? 2 : normalised <= 5 ? 5 : 10) * magnitude;
  const ticks: number[] = [];
  const start = Math.ceil(min / step) * step;
  for (let value = start; value <= max + step * 1e-9 && ticks.length < 12; value += step) {
    ticks.push(Number(value.toPrecision(12)));
  }
  return ticks;
}

/**
 * Ticks on a log axis: one per decade, plus 2 and 5 when the span is narrow.
 *
 * A decade-only axis over 1..1000 draws three lines, which is a frame with two
 * gridlines in it. When the span is under two decades the 2 and 5 multiples are
 * added so the reader has something to interpolate between.
 */
function logTicks(min: number, max: number): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max) || max <= min || min <= 0) return [];
  const low = Math.floor(Math.log10(min));
  const high = Math.ceil(Math.log10(max));
  const decades = high - low;
  const ticks: number[] = [];
  for (let power = low; power <= high; power += 1) {
    const base = Math.pow(10, power);
    const multiples = decades <= 2 ? [1, 2, 5] : [1];
    for (const multiple of multiples) {
      const value = base * multiple;
      if (value >= min * 0.999 && value <= max * 1.001) ticks.push(value);
    }
  }
  return ticks.slice(0, 12);
}

/**
 * The contiguous runs of drawable points.
 *
 * A log axis cannot place a value at or below zero, and a line that jumps the
 * gap is a line that claims a value it does not have. Splitting into runs lets
 * the renderer draw a break instead.
 */
function segments(points: number[], log: boolean): number[][] {
  const runs: number[][] = [];
  let run: number[] = [];
  points.forEach((value, index) => {
    const drawable = Number.isFinite(value) && (!log || value > 0);
    if (drawable) {
      run.push(index);
    } else if (run.length > 0) {
      runs.push(run);
      run = [];
    }
  });
  if (run.length > 0) runs.push(run);
  return runs;
}

/** One series' share of its column, for the percent tooltip. */
function share(bands: Band[] | null, slot: number, index: number): string {
  if (!bands || !bands[slot]) return '';
  const top = bands[slot].top[index] ?? 0;
  const base = bands[slot].base[index] ?? 0;
  return `${Math.round(top - base)}%`;
}

/**
 * Which x labels to draw.
 *
 * Every label on a 60-point series is a smear. The first and last always draw,
 * because they are the range; the rest are thinned to whatever fits at roughly
 * 46px per label.
 */
function pickLabels(
  count: number,
  labels: string[],
  plotWidth: number,
): Array<{ index: number; text: string }> {
  if (labels.length === 0 || count === 0) return [];
  const room = Math.max(2, Math.floor(plotWidth / 46));
  if (count <= room) {
    return labels.slice(0, count).map((text, index) => ({ index, text }));
  }
  const stride = Math.ceil(count / room);
  const picked: Array<{ index: number; text: string }> = [];
  for (let index = 0; index < count; index += stride) {
    const text = labels[index];
    if (text) picked.push({ index, text });
  }
  const lastIndex = count - 1;
  const lastText = labels[lastIndex];
  if (lastText && picked[picked.length - 1]?.index !== lastIndex) {
    picked.push({ index: lastIndex, text: lastText });
  }
  return picked;
}

/** What the chart is, in words, for the accessible name. */
function describe(
  style: ChartStyle,
  stack: ChartStack,
  scale: ChartScale,
  series: number,
  marks: number,
): string {
  const parts: string[] = [style];
  if (stack !== 'none') parts.push(`${stack} stack`);
  if (scale === 'log') parts.push('log scale');
  parts.push(`${series} ${series === 1 ? 'series' : 'series'}`);
  if (marks > 0) parts.push(`${marks} reference ${marks === 1 ? 'line' : 'lines'}`);
  return parts.join(', ');
}

/** A number, compacted: 1.2k, 3.4M, 0.42. */
function compact(value: number): string {
  if (!Number.isFinite(value)) return '—';
  const abs = Math.abs(value);
  if (abs >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (abs >= 1_000) return `${(value / 1_000).toFixed(abs < 10_000 ? 1 : 0)}k`;
  if (abs >= 10) return value.toFixed(0);
  // A whole number reads as `1`, not `1.0`. A log axis is mostly whole
  // numbers, and `1.0` on a decade label looks like a rounding error.
  if (abs >= 1) return Number.isInteger(value) ? String(value) : value.toFixed(1);
  if (abs === 0) return '0';
  return value.toFixed(2);
}

/** An epoch-millisecond x value as a clock time, or a date when the span is long. */
function clock(value: unknown): string {
  const ms = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(ms)) return display(value);
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return display(value);
  const hh = String(date.getHours()).padStart(2, '0');
  const mm = String(date.getMinutes()).padStart(2, '0');
  return `${hh}:${mm}`;
}

function number(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

/** A tone expression's value, or `neutral` for anything outside the set. */
function toneOf(value: unknown): DiagramTone {
  const text = typeof value === 'string' ? value.trim().toLowerCase() : '';
  return (TONES as string[]).includes(text) ? (text as DiagramTone) : 'neutral';
}
