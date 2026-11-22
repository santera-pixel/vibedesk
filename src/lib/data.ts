/**
 * The data plane, frontend half.
 *
 * Two jobs:
 *
 * 1. **Evaluate bindings.** A block's value is an expression; this turns it into
 *    a value against the panel's data model. The grammar mirrors
 *    `src-tauri/src/expr.rs` exactly — same operators, same formats, same
 *    semantics — because Rust validates at approval time and this runs at render
 *    time. `src/lib/expr-vectors.json` is the shared table both sides are tested
 *    against, so the two cannot drift silently.
 *
 * 2. **Resolve built-in sources.** VibeDesk's own state, read from the store
 *    that already holds it. No IPC, no network. External sources arrive as
 *    `panel://data` events from Rust, which is where the network lives.
 */

import type { Expr } from '@/lib/customize';

/* ---------------------------------------------------------------- *
 * Types
 * ---------------------------------------------------------------- */

/** One source's latest answer. `value` is null when the fetch failed. */
export interface DataPoint {
  value: unknown;
  at: string;
  error: string | null;
}

/** `panelId -> sourceId -> point`. */
export type PanelData = Record<string, Record<string, DataPoint>>;

/** The data model a binding reads: `sourceId -> value`. */
export type DataModel = Record<string, unknown>;

export interface DataEvent {
  panelId: string;
  sourceId: string;
  value: unknown | null;
  at: string;
  error: string | null;
}

export interface SourceStatus {
  panelId: string;
  sourceId: string;
  kind: string;
  target: string;
  /** The origin the user is asked about, e.g. `http://127.0.0.1:8787`. */
  origin: string;
  mode: string | null;
  intervalMs: number | null;
  running: boolean;
  /**
   * `running`, `stopped`, `awaiting-consent` or `denied`.
   *
   * A source that is declared but not permitted is not broken, and showing it
   * as `stopped` would send the user looking for a config error that is not
   * there. The distinction is the whole point of the consent ledger.
   */
  state: string;
}

/** One origin the config wants, and what the user has said about it. */
export interface ConsentEntry {
  origin: string;
  /** `allow`, `deny`, or `null` when the user has not been asked. */
  decision: 'allow' | 'deny' | null;
  /** Panel ids that read this origin. */
  panels: string[];
  /** Source ids that read this origin. */
  sources: string[];
  /** True when at least one source on this origin is actually running. */
  running: boolean;
}

export interface DataPlaneStatus {
  paused: boolean;
  safeMode: boolean;
  sources: SourceStatus[];
  limits: {
    maxSourcesPerPanel: number;
    maxBodyBytes: number;
    maxJsonDepth: number;
    maxJsonNodes: number;
    maxConcurrent: number;
    intervalMin: number;
    intervalMax: number;
    timeoutMin: number;
    timeoutMax: number;
  };
}

/* ---------------------------------------------------------------- *
 * Evaluation
 * ---------------------------------------------------------------- */

/** A path that resolved to nothing. Distinct from a value that is `null`. */
export const MISSING = Symbol('missing');

type Resolved = unknown | typeof MISSING;

function isMissing(value: Resolved): value is typeof MISSING {
  return value === MISSING;
}

/**
 * Walk a `/`-separated path into the data model.
 *
 * A path is not a JSON Pointer: no `~0`/`~1` escaping, no array index syntax.
 * A panel that needs to index into an array uses a `table` or `chart` block,
 * which take the whole array. Keeping the grammar this small is what makes this
 * a lookup rather than a parser.
 */
export function readPath(data: DataModel, path: string): Resolved {
  if (!path.startsWith('/')) return MISSING;
  const segments = path.slice(1).split('/');
  let cursor: unknown = data;
  for (const segment of segments) {
    if (segment === '') return MISSING;
    if (cursor === null || typeof cursor !== 'object') return MISSING;
    if (Array.isArray(cursor)) {
      const index = Number(segment);
      if (!Number.isInteger(index) || index < 0 || index >= cursor.length) return MISSING;
      cursor = cursor[index];
      continue;
    }
    if (!Object.prototype.hasOwnProperty.call(cursor, segment)) return MISSING;
    cursor = (cursor as Record<string, unknown>)[segment];
  }
  return cursor;
}

function isExprObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asNumber(value: Resolved): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function compare(op: string, left: Resolved, right: Resolved): boolean {
  const a = isMissing(left) ? undefined : left;
  const b = isMissing(right) ? undefined : right;
  switch (op) {
    case '@eq':
      return a === b;
    case '@ne':
      return a !== b;
    default:
      break;
  }
  // Ordering: numbers compare numerically, strings lexicographically. Anything
  // else is not ordered, so the comparison is false rather than a coercion.
  const na = asNumber(a);
  const nb = asNumber(b);
  if (na !== null && nb !== null) {
    if (op === '@gt') return na > nb;
    if (op === '@gte') return na >= nb;
    if (op === '@lt') return na < nb;
    return na <= nb;
  }
  if (typeof a === 'string' && typeof b === 'string') {
    if (op === '@gt') return a > b;
    if (op === '@gte') return a >= b;
    if (op === '@lt') return a < b;
    return a <= b;
  }
  return false;
}

function arithmetic(op: string, left: Resolved, right: Resolved): number | null {
  const a = asNumber(left);
  const b = asNumber(right);
  if (a === null || b === null) return null;
  switch (op) {
    case '@add':
      return a + b;
    case '@sub':
      return a - b;
    case '@mul':
      return a * b;
    case '@div':
      return b === 0 ? null : a / b;
    default:
      return null;
  }
}

const NUMBER = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const CURRENCY = new Intl.NumberFormat(undefined, {
  style: 'currency',
  currency: 'USD',
  maximumFractionDigits: 2,
});
const PERCENT = new Intl.NumberFormat(undefined, {
  style: 'percent',
  maximumFractionDigits: 1,
});

function format(kind: string, value: Resolved): string {
  if (isMissing(value) || value === null || value === undefined) return '—';
  switch (kind) {
    case 'number': {
      const n = asNumber(value);
      return n === null ? String(value) : NUMBER.format(n);
    }
    case 'currency': {
      const n = asNumber(value);
      return n === null ? String(value) : CURRENCY.format(n);
    }
    case 'percent': {
      const n = asNumber(value);
      // A percent binding is a fraction: 0.42 is 42%.
      return n === null ? String(value) : PERCENT.format(n);
    }
    case 'duration': {
      const n = asNumber(value);
      if (n === null) return String(value);
      const total = Math.max(0, Math.round(n));
      const hours = Math.floor(total / 3600);
      const minutes = Math.floor((total % 3600) / 60);
      const seconds = total % 60;
      if (hours > 0) return `${hours}h ${minutes}m`;
      if (minutes > 0) return `${minutes}m ${seconds}s`;
      return `${seconds}s`;
    }
    case 'date': {
      const date = new Date(String(value));
      if (Number.isNaN(date.getTime())) return String(value);
      return date.toLocaleString();
    }
    case 'truncate': {
      const text = String(value);
      return text.length > 40 ? `${text.slice(0, 39)}…` : text;
    }
    default:
      return String(value);
  }
}

/**
 * Evaluate a binding against a data model.
 *
 * Total: every input produces a value or `MISSING`, never a throw. A renderer
 * that throws on a bad binding takes the window down; one that returns `MISSING`
 * shows a dash.
 */
export function evaluate(expr: Expr, data: DataModel): Resolved {
  if (!isExprObject(expr)) return expr;

  // `@format` is the one operator that takes a second key.
  if ('@format' in expr) {
    const kind = String(expr['@format']);
    return format(kind, evaluate(expr['@of'] ?? null, data));
  }

  const keys = Object.keys(expr);
  if (keys.length !== 1) return MISSING;
  const token = keys[0] as string;
  const argument = expr[token];

  switch (token) {
    case '@path':
      return typeof argument === 'string' ? readPath(data, argument) : MISSING;

    case '@exists': {
      if (typeof argument !== 'string') return false;
      return !isMissing(readPath(data, argument));
    }

    case '@missing': {
      if (typeof argument !== 'string') return false;
      return isMissing(readPath(data, argument));
    }

    case '@not':
      return !truthy(evaluate(argument, data));

    // `[test, then, otherwise]`. The branch that is not taken is never
    // evaluated, which is what lets a diagram pick a tone from a condition
    // without reading the path the other branch would have read.
    case '@if': {
      if (!Array.isArray(argument) || argument.length !== 3) return MISSING;
      return truthy(evaluate(argument[0], data))
        ? evaluate(argument[1], data)
        : evaluate(argument[2], data);
    }

    case '@and': {
      if (!Array.isArray(argument)) return false;
      return argument.every((item) => truthy(evaluate(item, data)));
    }

    case '@or': {
      if (!Array.isArray(argument)) return false;
      return argument.some((item) => truthy(evaluate(item, data)));
    }

    case '@eq':
    case '@ne':
    case '@gt':
    case '@gte':
    case '@lt':
    case '@lte': {
      if (!Array.isArray(argument) || argument.length !== 2) return false;
      return compare(token, evaluate(argument[0], data), evaluate(argument[1], data));
    }

    case '@add':
    case '@sub':
    case '@mul':
    case '@div': {
      if (!Array.isArray(argument) || argument.length !== 2) return MISSING;
      const result = arithmetic(token, evaluate(argument[0], data), evaluate(argument[1], data));
      return result === null ? MISSING : result;
    }

    default:
      return MISSING;
  }
}

/** A condition is true only for a real `true`. `MISSING` is not true. */
export function truthy(value: Resolved): boolean {
  return value === true;
}

/**
 * Evaluate a `when` condition. A block with no condition is always shown; a
 * block whose condition cannot be evaluated is hidden, because showing a block
 * whose guard is unknown is the failure mode that lies.
 */
export function visible(when: Expr | null | undefined, data: DataModel): boolean {
  if (when === null || when === undefined) return true;
  return truthy(evaluate(when, data));
}

/** Render a resolved value as text. `MISSING` and `null` both become a dash. */
export function display(value: Resolved): string {
  if (isMissing(value) || value === null || value === undefined) return '—';
  if (typeof value === 'string') return value;
  if (typeof value === 'number') return NUMBER.format(value);
  if (typeof value === 'boolean') return value ? 'yes' : 'no';
  if (Array.isArray(value)) return `${value.length} items`;
  return JSON.stringify(value);
}

/** A resolved value as a number, or null. Used by `progress` and `chart`. */
export function numeric(value: Resolved): number | null {
  return isMissing(value) ? null : asNumber(value);
}

/** The most points a chart draws. Beyond this the series is decimated. */
export const MAX_SERIES = 512;

/**
 * Reduce a series to at most `limit` points without changing its shape.
 *
 * Truncating would be a lie: the line would stop halfway through the data and
 * the axis would describe a range the data does not have. So the array is cut
 * into buckets and each bucket contributes its lowest and highest sample, in the
 * order they occur. Spikes survive, the endpoints survive, and the result is at
 * most `limit` points — which is what keeps a 10,000-element array from becoming
 * a 10,000-point polyline.
 */
export function decimate(points: number[], limit = MAX_SERIES): number[] {
  if (points.length <= limit) return points;
  // One slot is reserved for the final sample, so the result cannot exceed
  // the limit by the point that closes the line.
  const buckets = Math.max(1, Math.floor((limit - 1) / 2));
  const size = points.length / buckets;
  const out: number[] = [];
  for (let bucket = 0; bucket < buckets; bucket += 1) {
    const start = Math.floor(bucket * size);
    const end = Math.min(points.length, Math.floor((bucket + 1) * size));
    if (end <= start) continue;
    let low = start;
    let high = start;
    let lowValue = points[start] ?? 0;
    let highValue = lowValue;
    for (let index = start + 1; index < end; index += 1) {
      const value = points[index];
      if (value === undefined) continue;
      if (value < lowValue) {
        low = index;
        lowValue = value;
      }
      if (value > highValue) {
        high = index;
        highValue = value;
      }
    }
    if (low === high) out.push(lowValue);
    else if (low < high) out.push(lowValue, highValue);
    else out.push(highValue, lowValue);
  }
  const last = points[points.length - 1];
  if (last !== undefined && out[out.length - 1] !== last) out.push(last);
  return out;
}

/** How many numbers a resolved value holds, before decimation. */
export function seriesCount(value: Resolved): number {
  if (isMissing(value) || !Array.isArray(value)) return 0;
  return value.reduce<number>((total, item) => (asNumber(item) === null ? total : total + 1), 0);
}

/** A resolved value as an array of numbers, or an empty array. */
export function series(value: Resolved): number[] {
  if (isMissing(value) || !Array.isArray(value)) return [];
  const points = value.map((item) => asNumber(item)).filter((item): item is number => item !== null);
  return decimate(points);
}

/** A resolved value as rows of cells. Objects are read by `columns`. */
export function rows(value: Resolved, columns: string[]): string[][] {
  if (isMissing(value) || !Array.isArray(value)) return [];
  return value.slice(0, 200).map((row) => {
    if (Array.isArray(row)) {
      return columns.map((_, index) => display(row[index]));
    }
    if (isExprObject(row)) {
      return columns.map((column) => display(row[column]));
    }
    return columns.map(() => '—');
  });
}

/* ---------------------------------------------------------------- *
 * Built-in sources
 * ---------------------------------------------------------------- */

/**
 * The six feeds, resolved from state the app already holds.
 *
 * These are the ones that make a panel live with zero risk, and they are what an
 * agent should reach for first. Each returns a plain object whose keys are the
 * paths a binding reads: `status` is `/status/state`, `/status/pid`, and so on.
 */
export interface BuiltinInputs {
  status: {
    state: string;
    pid: number | null;
    version: string | null;
    cwd: string | null;
    sessionId: string | null;
    model: string | null;
    agent: string | null;
    restarts: number;
    startedAt: string | null;
    error: string | null;
  } | null;
  usage: {
    used: number | null;
    size: number | null;
    cost: number | null;
    promptTokens: number | null;
    completionTokens: number | null;
    cachedTokens: number | null;
    hitRate: number | null;
  } | null;
  session: {
    id: string | null;
    title: string | null;
    cwd: string | null;
    busy: boolean;
    mode: string | null;
  } | null;
  chats: { total: number; visible: number; archived: number } | null;
  plugins: { total: number; enabled: number; names: string[] } | null;
}

export function resolveBuiltin(name: string, inputs: BuiltinInputs): unknown {
  switch (name) {
    case 'status':
      return inputs.status ?? {};
    case 'usage':
      return inputs.usage ?? {};
    case 'session':
      return inputs.session ?? {};
    case 'chats':
      return inputs.chats ?? {};
    case 'plugins':
      return inputs.plugins ?? {};
    case 'clock': {
      const now = new Date();
      return {
        iso: now.toISOString(),
        epoch: Math.floor(now.getTime() / 1000),
        hour: now.getHours(),
        minute: now.getMinutes(),
      };
    }
    default:
      return {};
  }
}

/** The six names, for the UI. Mirrors `BuiltinName::ALL` in Rust. */
export const BUILTIN_NAMES = ['status', 'usage', 'session', 'chats', 'plugins', 'clock'] as const;
