import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
} from 'react';
import type {
  DiagramEdge,
  DiagramLayout,
  DiagramNode,
  DiagramRouting,
  DiagramTone,
} from '@/lib/customize';
import { display, evaluate, visible, type DataModel } from '@/lib/data';
import { useApp } from '@/store/app';
import { runAction } from '@/components/chrome/ActionStrip';

/**
 * A graph, drawn.
 *
 * A chart plots a series; a widget is a blank canvas that costs two hundred
 * lines of JavaScript. This is the third thing: twenty lines of JSON that say
 * "draw this graph", with every visible field bound to live data and every node
 * able to fire an action. The agent describes the graph; it does not write the
 * layout engine.
 *
 * Four layouts, each a real algorithm with a deterministic result:
 *
 * - `layered` ranks by longest path over the DAG left after back edges are
 *   removed, orders each rank by barycentre, and places columns left to right.
 * - `radial` finds a cycle and closes it into a ring, so a process that repeats
 *   reads as a ring rather than as a tangle.
 * - `tree` is a tidy tree: leaves advance a per-depth cursor, a parent centres
 *   over its children, and a parent that would collide with the previous
 *   subtree shifts its own subtree right.
 * - `sequence` is lifelines across and messages down, in declaration order.
 *
 * Geometry is computed in CSS pixels from a `ResizeObserver`, like `ChartBlock`,
 * so text stays crisp instead of being stretched by a `viewBox`. Node widths
 * come from real text measurement through an offscreen canvas, not from a
 * character count, because `WWWW` and `iiii` are not the same width and a box
 * sized for the average clips both.
 */

const NODE_PAD_X = 11;
const MIN_NODE_W = 58;
const MAX_NODE_W = 176;
const LABEL_ONLY_H = 30;
const WITH_DETAIL_H = 44;
const GAP_X = 44;
const GAP_Y = 24;
const MARGIN = 12;
const ARROW = 5.5;
const DEFAULT_HEIGHT = 240;
const SEQ_HEADER_H = 30;
const SEQ_STEP = 34;
/**
 * The narrowest and widest gap between two sequence participants.
 *
 * A sequence diagram is the one layout whose width is decided by the container
 * rather than by the content: participants are columns, and a column that runs
 * off the right edge is a participant the reader cannot see. So the columns are
 * sized to the panel and the gap absorbs the remainder — down to `SEQ_MIN_GAP`,
 * below which the labels are unreadable and horizontal scroll is the honest
 * answer, and up to `SEQ_MAX_GAP`, above which two participants sit at opposite
 * edges of a wide panel with a message arrow crossing the whole window.
 */
const SEQ_MIN_GAP = 10;
const SEQ_MAX_GAP = 160;
const LANE = 16;

/**
 * Tone to theme token.
 *
 * A closed set, because a diagram that accepted `#ff0000` would be a diagram
 * whose colours ignore the theme. `danger` here is the same red as a failed tool
 * call, in both themes, because both read the same token.
 */
const TONES: Record<DiagramTone, { fill: string; stroke: string; text: string }> = {
  neutral: { fill: 'var(--vd-surface)', stroke: 'var(--vd-line-strong)', text: 'var(--vd-text)' },
  accent: { fill: 'var(--vd-accent-wash)', stroke: 'var(--vd-accent)', text: 'var(--vd-text)' },
  ok: { fill: 'var(--vd-ok-wash)', stroke: 'var(--vd-ok)', text: 'var(--vd-text)' },
  warn: { fill: 'var(--vd-warn-wash)', stroke: 'var(--vd-warn)', text: 'var(--vd-text)' },
  danger: { fill: 'var(--vd-danger-wash)', stroke: 'var(--vd-danger)', text: 'var(--vd-text)' },
  muted: {
    fill: 'var(--vd-surface-sunken)',
    stroke: 'var(--vd-line)',
    text: 'var(--vd-text-tertiary)',
  },
};

const TONE_NAMES: DiagramTone[] = ['neutral', 'accent', 'ok', 'warn', 'danger', 'muted'];

/** Visually hidden, but still in the accessibility tree. */
const HIDDEN: CSSProperties = {
  position: 'absolute',
  width: 1,
  height: 1,
  margin: -1,
  padding: 0,
  overflow: 'hidden',
  clip: 'rect(0 0 0 0)',
  whiteSpace: 'nowrap',
  border: 0,
};

interface Props {
  label: string;
  layout: DiagramLayout;
  nodes: DiagramNode[];
  edges: DiagramEdge[];
  routing: DiagramRouting;
  height?: number | null;
  model: DataModel;
}

interface Point {
  x: number;
  y: number;
}

/** A node after its expressions have been resolved and its box measured. */
interface Box {
  id: string;
  label: string;
  detail: string | null;
  tone: DiagramTone;
  action: string | null;
  w: number;
  h: number;
  x: number;
  y: number;
  /**
   * The bottom of the band this box's level occupies, when the layout has
   * levels. A tree's horizontal edge run has to happen below the *tallest* box
   * on the level, not below this one, or it crosses a taller sibling.
   */
  band?: number;
}

interface Link {
  key: string;
  from: string;
  to: string;
  label: string | null;
}

/**
 * A drawn edge.
 *
 * `points` is a polyline for orthogonal routing and the four control points of a
 * cubic for curved routing; `curved` says which. Keeping one shape means the
 * arrowhead and the label position are computed once for both.
 */
interface Route {
  points: Point[];
  curved: boolean;
  label: Point | null;
}

/* ---------------------------------------------------------------- *\
 * Text measurement
\* ---------------------------------------------------------------- */

let measurer: CanvasRenderingContext2D | null | undefined;

/**
 * The width of `text` in `font`, in CSS pixels.
 *
 * A canvas context is the only way to ask the browser what a string will
 * actually measure. The fallback is a rough average, used only when the webview
 * refuses a 2D context — a box a few pixels off is better than a crash.
 */
function measure(text: string, font: string): number {
  if (measurer === undefined) {
    measurer = document.createElement('canvas').getContext('2d');
  }
  if (!measurer) return text.length * 6.2;
  measurer.font = font;
  return measurer.measureText(text).width;
}

/** `text`, trimmed with an ellipsis until it fits in `max` pixels. */
function fit(text: string, font: string, max: number): string {
  if (measure(text, font) <= max) return text;
  let low = 0;
  let high = text.length;
  while (low < high) {
    const mid = Math.ceil((low + high) / 2);
    if (measure(`${text.slice(0, mid)}…`, font) <= max) low = mid;
    else high = mid - 1;
  }
  return `${text.slice(0, low)}…`;
}

/* ---------------------------------------------------------------- *\
 * Geometry
\* ---------------------------------------------------------------- */

function centre(box: Box): Point {
  return { x: box.x + box.w / 2, y: box.y + box.h / 2 };
}

/** Where the ray from `box`'s centre toward `target` leaves the box. */
function clipToBox(box: Box, target: Point): Point {
  const from = centre(box);
  const dx = target.x - from.x;
  const dy = target.y - from.y;
  if (dx === 0 && dy === 0) return from;
  const scale = Math.min(
    dx === 0 ? Infinity : box.w / 2 / Math.abs(dx),
    dy === 0 ? Infinity : box.h / 2 / Math.abs(dy),
  );
  return { x: from.x + dx * scale, y: from.y + dy * scale };
}

function bezierAt(p0: Point, c1: Point, c2: Point, p1: Point, t: number): Point {
  const u = 1 - t;
  const a = u * u * u;
  const b = 3 * u * u * t;
  const c = 3 * u * t * t;
  const d = t * t * t;
  return {
    x: a * p0.x + b * c1.x + c * c2.x + d * p1.x,
    y: a * p0.y + b * c1.y + c * c2.y + d * p1.y,
  };
}

/** A filled triangle at `tip`, pointing along `dir`. */
function arrowHead(tip: Point, dir: Point): string {
  const length = Math.hypot(dir.x, dir.y) || 1;
  const ux = dir.x / length;
  const uy = dir.y / length;
  const back = { x: tip.x - ux * ARROW, y: tip.y - uy * ARROW };
  const px = -uy * ARROW * 0.5;
  const py = ux * ARROW * 0.5;
  return `${tip.x},${tip.y} ${back.x + px},${back.y + py} ${back.x - px},${back.y - py}`;
}

/* ---------------------------------------------------------------- *\
 * Layout — layered
\* ---------------------------------------------------------------- */

/**
 * Split the edges into forward edges and back edges.
 *
 * A back edge is one that closes a cycle. Ranking cannot handle a cycle, so the
 * back edges are set aside for ranking and drawn afterwards along a lane clear
 * of every box. The walk is iterative because a 40-node chain would otherwise
 * recurse 40 frames deep for no reason.
 */
function splitEdges(boxes: Box[], links: Link[]): { forward: Link[]; back: Set<string> } {
  const at = new Map(boxes.map((box, index) => [box.id, index]));
  const out = new Map<number, Link[]>();
  for (const link of links) {
    const from = at.get(link.from);
    if (from === undefined) continue;
    const list = out.get(from);
    if (list) list.push(link);
    else out.set(from, [link]);
  }

  const state = new Array<number>(boxes.length).fill(0);
  const back = new Set<string>();

  for (let start = 0; start < boxes.length; start++) {
    if (state[start] !== 0) continue;
    state[start] = 1;
    const stack: Array<{ node: number; next: number }> = [{ node: start, next: 0 }];
    while (stack.length > 0) {
      const frame = stack[stack.length - 1];
      if (!frame) break;
      const edges = out.get(frame.node) ?? [];
      if (frame.next >= edges.length) {
        state[frame.node] = 2;
        stack.pop();
        continue;
      }
      const link = edges[frame.next];
      frame.next += 1;
      if (!link) continue;
      const to = at.get(link.to);
      if (to === undefined) continue;
      if (state[to] === 1) back.add(link.key);
      else if (state[to] === 0) {
        state[to] = 1;
        stack.push({ node: to, next: 0 });
      }
    }
  }

  return { forward: links.filter((link) => !back.has(link.key)), back };
}

/** Longest-path ranking over the DAG, as rows of node indices. */
function rankRows(boxes: Box[], forward: Link[]): number[][] {
  const at = new Map(boxes.map((box, index) => [box.id, index]));
  const rank = new Array<number>(boxes.length).fill(0);
  const indegree = new Array<number>(boxes.length).fill(0);
  const out: number[][] = boxes.map(() => []);

  for (const link of forward) {
    const from = at.get(link.from);
    const to = at.get(link.to);
    if (from === undefined || to === undefined) continue;
    out[from]?.push(to);
    indegree[to] = (indegree[to] ?? 0) + 1;
  }

  const queue: number[] = [];
  for (let index = 0; index < boxes.length; index++) {
    if (indegree[index] === 0) queue.push(index);
  }

  let head = 0;
  while (head < queue.length) {
    const node = queue[head];
    head += 1;
    if (node === undefined) continue;
    for (const next of out[node] ?? []) {
      rank[next] = Math.max(rank[next] ?? 0, (rank[node] ?? 0) + 1);
      indegree[next] = (indegree[next] ?? 0) - 1;
      if (indegree[next] === 0) queue.push(next);
    }
  }

  const rows: number[][] = [];
  for (let index = 0; index < boxes.length; index++) {
    const row = rank[index] ?? 0;
    while (rows.length <= row) rows.push([]);
    rows[row]?.push(index);
  }
  return rows;
}

/**
 * Barycentre ordering.
 *
 * Four sweeps, alternating direction: left to right orders each rank by the mean
 * position of its predecessors, right to left by the mean of its successors. A
 * node with no neighbours on the relevant side keeps its current index, which is
 * what makes the sort stable and the result deterministic.
 */
function orderRows(rows: number[][], boxes: Box[], forward: Link[]): number[][] {
  const at = new Map(boxes.map((box, index) => [box.id, index]));
  const preds: number[][] = boxes.map(() => []);
  const succs: number[][] = boxes.map(() => []);
  for (const link of forward) {
    const from = at.get(link.from);
    const to = at.get(link.to);
    if (from === undefined || to === undefined) continue;
    succs[from]?.push(to);
    preds[to]?.push(from);
  }

  const order = rows.map((row) => row.slice());
  const position = new Map<number, number>();
  const sync = () => {
    position.clear();
    for (const row of order) row.forEach((node, index) => position.set(node, index));
  };
  sync();

  const sweep = (row: number[], neighbours: number[][]): number[] => {
    const keyed = row.map((node, index) => {
      const known = (neighbours[node] ?? []).filter((other) => position.has(other));
      const bary =
        known.length === 0
          ? index
          : known.reduce((sum, other) => sum + (position.get(other) ?? 0), 0) / known.length;
      return { node, bary, index };
    });
    keyed.sort((a, b) => a.bary - b.bary || a.index - b.index);
    return keyed.map((entry) => entry.node);
  };

  for (let pass = 0; pass < 4; pass++) {
    for (let row = 1; row < order.length; row++) {
      const current = order[row];
      if (!current) continue;
      order[row] = sweep(current, preds);
      sync();
    }
    for (let row = order.length - 2; row >= 0; row--) {
      const current = order[row];
      if (!current) continue;
      order[row] = sweep(current, succs);
      sync();
    }
  }

  return order;
}

function placeLayered(boxes: Box[], links: Link[]): { width: number; height: number } {
  const { forward } = splitEdges(boxes, links);
  const rows = orderRows(rankRows(boxes, forward), boxes, forward);

  const columnWidth = rows.map((row) =>
    row.reduce((widest, node) => Math.max(widest, boxes[node]?.w ?? 0), 0),
  );
  const columnHeight = rows.map((row) =>
    row.reduce(
      (tallest, node, index) => tallest + (boxes[node]?.h ?? 0) + (index === 0 ? 0 : GAP_Y),
      0,
    ),
  );
  const tallest = columnHeight.reduce((most, height) => Math.max(most, height), 0);

  let x = MARGIN;
  rows.forEach((row, column) => {
    const width = columnWidth[column] ?? 0;
    let y = MARGIN + (tallest - (columnHeight[column] ?? 0)) / 2;
    for (const node of row) {
      const box = boxes[node];
      if (!box) continue;
      box.x = x + (width - box.w) / 2;
      box.y = y;
      y += box.h + GAP_Y;
    }
    x += width + GAP_X;
  });

  return {
    width: x - GAP_X + MARGIN,
    height: tallest + MARGIN * 2,
  };
}

/* ---------------------------------------------------------------- *\
 * Layout — tree
\* ---------------------------------------------------------------- */

function placeTree(boxes: Box[], links: Link[]): { width: number; height: number } {
  const { forward } = splitEdges(boxes, links);
  const at = new Map(boxes.map((box, index) => [box.id, index]));
  const children: number[][] = boxes.map(() => []);
  const hasParent = new Array<boolean>(boxes.length).fill(false);

  for (const link of forward) {
    const from = at.get(link.from);
    const to = at.get(link.to);
    if (from === undefined || to === undefined) continue;
    // One parent per node: the first edge in declaration order wins, which is
    // what makes the result deterministic when the graph is not a tree.
    if (hasParent[to]) continue;
    hasParent[to] = true;
    children[from]?.push(to);
  }

  const roots: number[] = [];
  for (let index = 0; index < boxes.length; index++) {
    if (!hasParent[index]) roots.push(index);
  }
  if (roots.length === 0 && boxes.length > 0) roots.push(0);

  const depth = new Array<number>(boxes.length).fill(0);
  const x = new Array<number>(boxes.length).fill(0);
  const cursor: number[] = [];
  const placed = new Set<number>();

  // Positions are assigned from `x` at the end, so a shift has to move `x`.
  // Moving `boxes[node].x` here would move a zero and leave the subtree behind,
  // which is how a parent ends up overlapping the previous subtree.
  const shift = (node: number, delta: number) => {
    x[node] = (x[node] ?? 0) + delta;
    for (const kid of children[node] ?? []) shift(kid, delta);
  };

  const walk = (node: number, level: number) => {
    if (placed.has(node)) return;
    placed.add(node);
    depth[node] = level;
    const box = boxes[node];
    if (!box) return;

    const kids = (children[node] ?? []).filter((kid) => !placed.has(kid));
    if (kids.length === 0) {
      const left = cursor[level] ?? MARGIN;
      x[node] = left + box.w / 2;
      cursor[level] = left + box.w + GAP_X;
      return;
    }

    for (const kid of kids) walk(kid, level + 1);

    const first = x[kids[0] ?? 0] ?? 0;
    const last = x[kids[kids.length - 1] ?? 0] ?? 0;
    let middle = (first + last) / 2;
    const floor = cursor[level] ?? MARGIN;
    const left = middle - box.w / 2;
    if (left < floor) {
      const delta = floor - left;
      for (const kid of kids) shift(kid, delta);
      middle += delta;
    }
    x[node] = middle;
    cursor[level] = middle + box.w / 2 + GAP_X;
  };

  for (const root of roots) walk(root, 0);
  // A node unreachable from any root is its own tree.
  for (let index = 0; index < boxes.length; index++) {
    if (!placed.has(index)) walk(index, 0);
  }

  const levelHeight: number[] = [];
  for (let index = 0; index < boxes.length; index++) {
    const level = depth[index] ?? 0;
    levelHeight[level] = Math.max(levelHeight[level] ?? 0, boxes[index]?.h ?? 0);
  }
  const levelY: number[] = [];
  let top = MARGIN;
  for (let level = 0; level < levelHeight.length; level++) {
    levelY[level] = top;
    top += (levelHeight[level] ?? 0) + GAP_Y;
  }

  let widest = 0;
  for (let index = 0; index < boxes.length; index++) {
    const box = boxes[index];
    if (!box) continue;
    const level = depth[index] ?? 0;
    box.x = (x[index] ?? 0) - box.w / 2 + MARGIN;
    box.y = levelY[level] ?? MARGIN;
    box.band = (levelY[level] ?? MARGIN) + (levelHeight[level] ?? box.h);
    widest = Math.max(widest, box.x + box.w);
  }

  return { width: widest + MARGIN, height: top - GAP_Y + MARGIN };
}

/* ---------------------------------------------------------------- *\
 * Layout — radial
\* ---------------------------------------------------------------- */

/**
 * The order nodes take around the ring.
 *
 * A cycle is found first, because a ring is the honest picture of a process that
 * repeats: the loop becomes the ring itself. Everything else is appended
 * breadth-first from the ring, so a node sits next to the node that feeds it.
 * With no cycle at all the walk still produces a breadth-first order from the
 * first node, which is the best a ring can do for a DAG.
 */
function radialOrder(boxes: Box[], links: Link[]): number[] {
  const at = new Map(boxes.map((box, index) => [box.id, index]));
  const out: number[][] = boxes.map(() => []);
  for (const link of links) {
    const from = at.get(link.from);
    const to = at.get(link.to);
    if (from === undefined || to === undefined) continue;
    out[from]?.push(to);
  }

  const state = new Array<number>(boxes.length).fill(0);
  const path: number[] = [];
  let cycle: number[] = [];

  const walk = (node: number): boolean => {
    state[node] = 1;
    path.push(node);
    for (const next of out[node] ?? []) {
      if (state[next] === 1) {
        const start = path.indexOf(next);
        cycle = path.slice(start < 0 ? 0 : start);
        return true;
      }
      if (state[next] === 0 && walk(next)) return true;
    }
    path.pop();
    state[node] = 2;
    return false;
  };

  for (let index = 0; index < boxes.length && cycle.length === 0; index++) {
    if (state[index] === 0) walk(index);
  }

  const order: number[] = [];
  const seen = new Set<number>();
  for (const node of cycle) {
    if (seen.has(node)) continue;
    seen.add(node);
    order.push(node);
  }

  let head = 0;
  while (head < order.length) {
    const node = order[head];
    head += 1;
    if (node === undefined) continue;
    for (const next of out[node] ?? []) {
      if (seen.has(next)) continue;
      seen.add(next);
      order.push(next);
    }
  }
  for (let index = 0; index < boxes.length; index++) {
    if (seen.has(index)) continue;
    seen.add(index);
    order.push(index);
  }

  return order;
}

function placeRadial(boxes: Box[], links: Link[]): { width: number; height: number } {
  const order = radialOrder(boxes, links);
  const count = Math.max(1, order.length);
  const widest = boxes.reduce((most, box) => Math.max(most, box.w), MIN_NODE_W);
  const tallest = boxes.reduce((most, box) => Math.max(most, box.h), LABEL_ONLY_H);

  // Enough circumference that two neighbouring boxes cannot touch.
  const radius = Math.max(
    84,
    ((widest + GAP_X) * count) / (2 * Math.PI),
    ((tallest + GAP_Y) * count) / (2 * Math.PI),
  );
  const cx = radius + widest / 2 + MARGIN;
  const cy = radius + tallest / 2 + MARGIN;

  order.forEach((node, index) => {
    const box = boxes[node];
    if (!box) return;
    const angle = -Math.PI / 2 + (index * 2 * Math.PI) / count;
    box.x = cx + radius * Math.cos(angle) - box.w / 2;
    box.y = cy + radius * Math.sin(angle) - box.h / 2;
  });

  return {
    width: (radius + widest / 2 + MARGIN) * 2,
    height: (radius + tallest / 2 + MARGIN) * 2,
  };
}

/* ---------------------------------------------------------------- *\
 * Layout — sequence
\* ---------------------------------------------------------------- */

function placeSequence(
  boxes: Box[],
  links: Link[],
  available: number,
): { width: number; height: number; headerBottom: number } {
  const total = boxes.reduce((sum, box) => sum + box.w, 0);
  // The gap absorbs whatever the columns did not use, so the lifelines span the
  // panel instead of bunching at the left. Clamped at both ends: below the
  // minimum the labels are unreadable, above the maximum a two-participant
  // diagram puts its columns at opposite edges of the window.
  const gap =
    boxes.length > 1 && available > 0
      ? Math.max(
          SEQ_MIN_GAP,
          Math.min(SEQ_MAX_GAP, (available - MARGIN * 2 - total) / (boxes.length - 1)),
        )
      : Math.max(GAP_X, 30);

  let x = MARGIN;
  for (const box of boxes) {
    box.x = x;
    box.y = MARGIN;
    x += box.w + gap;
  }
  const width = Math.max(MARGIN * 2, x - gap + MARGIN);
  const headerBottom = MARGIN + SEQ_HEADER_H;
  return { width, height: headerBottom + SEQ_STEP * (links.length + 1), headerBottom };
}

/* ---------------------------------------------------------------- *\
 * Routing
\* ---------------------------------------------------------------- */

/**
 * The path an edge takes.
 *
 * `orthogonal` on a layered or tree layout is elbows through the gutter between
 * ranks, which is empty by construction — that is what makes it avoid nodes
 * rather than merely miss them. A back edge cannot use the gutter, so it leaves
 * the flow entirely and runs along a lane outside every box.
 *
 * On a radial layout there is no gutter: the nodes are on a ring and the ring's
 * interior is empty, so the chord between two boxes is the one path that
 * provably crosses nothing. `orthogonal` there is a straight segment and
 * `curved` is a bow, and both are honest about it.
 */
function routeEdge(
  from: Box,
  to: Box,
  layout: DiagramLayout,
  routing: DiagramRouting,
  lane: number,
  spread: number,
): Route {
  const curved = routing === 'curved';

  if (layout === 'radial') {
    const start = clipToBox(from, centre(to));
    const end = clipToBox(to, centre(from));
    if (!curved) return { points: [start, end], curved: false, label: midpoint(start, end) };
    const dx = end.x - start.x;
    const dy = end.y - start.y;
    const bow = 0.16;
    const c1 = { x: start.x + dx * 0.3 - dy * bow, y: start.y + dy * 0.3 + dx * bow };
    const c2 = { x: start.x + dx * 0.7 - dy * bow, y: start.y + dy * 0.7 + dx * bow };
    return { points: [start, c1, c2, end], curved: true, label: bezierAt(start, c1, c2, end, 0.5) };
  }

  const vertical = layout === 'tree';
  const forward = vertical ? to.y >= from.y + from.h : to.x >= from.x + from.w;

  if (forward) {
    if (vertical) {
      const start = { x: from.x + from.w / 2, y: from.y + from.h };
      const end = { x: to.x + to.w / 2, y: to.y };
      const middle = ((from.band ?? start.y) + end.y) / 2 + spread;
      const c1 = { x: start.x, y: middle };
      const c2 = { x: end.x, y: middle };
      if (!curved) {
        return {
          points: [start, c1, c2, end],
          curved: false,
          label: { x: (start.x + end.x) / 2, y: middle - 4 },
        };
      }
      return {
        points: [start, c1, c2, end],
        curved: true,
        label: bezierAt(start, c1, c2, end, 0.5),
      };
    }
    const start = { x: from.x + from.w, y: from.y + from.h / 2 };
    const end = { x: to.x, y: to.y + to.h / 2 };
    const middle = (start.x + end.x) / 2 + spread;
    const c1 = { x: middle, y: start.y };
    const c2 = { x: middle, y: end.y };
    if (!curved) {
      return {
        points: [start, c1, c2, end],
        curved: false,
        label: { x: middle, y: (start.y + end.y) / 2 - 4 },
      };
    }
    return { points: [start, c1, c2, end], curved: true, label: bezierAt(start, c1, c2, end, 0.5) };
  }

  // A back edge. An elbow along a lane outside every box is the only route that
  // provably clears them, so `curved` does not apply here.
  if (vertical) {
    const start = { x: from.x + from.w, y: from.y + from.h / 2 };
    const end = { x: to.x + to.w, y: to.y + to.h / 2 };
    return {
      points: [start, { x: lane, y: start.y }, { x: lane, y: end.y }, end],
      curved: false,
      label: { x: lane + 4, y: (start.y + end.y) / 2 },
    };
  }
  const start = { x: from.x + from.w / 2, y: from.y + from.h };
  const end = { x: to.x + to.w / 2, y: to.y + to.h };
  return {
    points: [start, { x: start.x, y: lane }, { x: end.x, y: lane }, end],
    curved: false,
    label: { x: (start.x + end.x) / 2, y: lane - 4 },
  };
}

function midpoint(a: Point, b: Point): Point {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

function pathOf(route: Route): string {
  const [p0, p1, p2, p3] = route.points;
  if (!p0) return '';
  if (route.curved && p1 && p2 && p3) {
    return `M ${p0.x},${p0.y} C ${p1.x},${p1.y} ${p2.x},${p2.y} ${p3.x},${p3.y}`;
  }
  // An elbow whose two ends share a coordinate produces a zero-length segment.
  // Drawing it is harmless; leaving it in makes the path unreadable when
  // someone is looking at why an edge is in the wrong place.
  const points: Point[] = [];
  for (const point of route.points) {
    const last = points[points.length - 1];
    if (last && Math.abs(last.x - point.x) < 0.01 && Math.abs(last.y - point.y) < 0.01) continue;
    points.push(point);
  }
  return points.map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x},${point.y}`).join(' ');
}

function arrowOf(route: Route): string {
  const points = route.points;
  const tip = points[points.length - 1];
  if (!tip) return '';
  // For a cubic the last control point is the direction the curve arrives from;
  // for a polyline it is the previous vertex. Walk back past any point that
  // coincides with the tip, or the direction is zero and the head collapses.
  let before: Point | undefined;
  for (let index = points.length - 2; index >= 0; index--) {
    const candidate = points[index];
    if (!candidate) continue;
    if (Math.abs(candidate.x - tip.x) > 0.01 || Math.abs(candidate.y - tip.y) > 0.01) {
      before = candidate;
      break;
    }
  }
  if (!before) return '';
  return arrowHead(tip, { x: tip.x - before.x, y: tip.y - before.y });
}

/* ---------------------------------------------------------------- *\
 * The block
\* ---------------------------------------------------------------- */

export function DiagramBlock({ label, layout, nodes, edges, routing, height, model }: Props) {
  const actionsConfig = useApp((s) => s.actionsConfig);
  const frame = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [family, setFamily] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);

  useEffect(() => {
    const element = frame.current;
    if (!element) return;
    const observer = new ResizeObserver((entries) => {
      setWidth(Math.max(0, Math.round(entries[0]?.contentRect.width ?? 0)));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Measure with the font the theme actually resolved, and re-measure once the
  // webfont has loaded — a box measured against the fallback is a box that
  // clips when Instrument Sans arrives.
  useEffect(() => {
    const element = frame.current;
    if (!element) return;
    let alive = true;
    const read = () => {
      if (alive) setFamily(getComputedStyle(element).fontFamily);
    };
    read();
    void document.fonts?.ready.then(read).catch(() => undefined);
    return () => {
      alive = false;
    };
  }, []);

  // Only a sequence diagram reads the container width, so only a sequence
  // diagram re-lays-out when it changes. Folding the width into a value that is
  // zero for every other layout keeps the memo from invalidating on every
  // resize of a panel that does not care.
  const columnWidth = layout === 'sequence' ? width : 0;

  const graph = useMemo(() => {
    const font = family ?? 'system-ui, sans-serif';
    const labelFont = `600 11px ${font}`;
    const detailFont = `400 10px ${font}`;

    // A sequence diagram's columns are sized by the panel, not by the text, so
    // the width has to be decided before any box is measured. Every other
    // layout lets the content decide and scrolls if it has to.
    const shown = nodes.filter((node) => visible(node.when ?? null, model)).length;
    const column =
      columnWidth > 0 && shown > 0
        ? Math.max(
            MIN_NODE_W,
            Math.min(MAX_NODE_W, (columnWidth - MARGIN * 2 - SEQ_MIN_GAP * (shown - 1)) / shown),
          )
        : MAX_NODE_W;
    const room = column - NODE_PAD_X * 2;

    const boxes: Box[] = [];
    for (const node of nodes) {
      if (!visible(node.when ?? null, model)) continue;
      const text = display(evaluate(node.label, model));
      const raw = node.detail == null ? null : display(evaluate(node.detail, model));
      const detail = raw === null || raw === '—' ? null : raw;
      const tone = toneOf(evaluate(node.tone ?? null, model));
      const needed = Math.max(
        measure(text, labelFont),
        detail === null ? 0 : measure(detail, detailFont),
      );
      boxes.push({
        id: node.id,
        label: fit(text, labelFont, room),
        detail: detail === null ? null : fit(detail, detailFont, room),
        tone,
        action: node.action ?? null,
        w: Math.min(column, Math.max(MIN_NODE_W, Math.ceil(needed) + NODE_PAD_X * 2)),
        h: detail === null ? LABEL_ONLY_H : WITH_DETAIL_H,
        x: 0,
        y: 0,
      });
    }

    const present = new Set(boxes.map((box) => box.id));
    const links: Link[] = [];
    edges.forEach((edge, index) => {
      if (!present.has(edge.from) || !present.has(edge.to)) return;
      const raw = edge.label == null ? null : display(evaluate(edge.label, model));
      links.push({
        key: edge.id ?? `${edge.from}->${edge.to}#${index}`,
        from: edge.from,
        to: edge.to,
        label: raw === null || raw === '—' ? null : raw,
      });
    });

    if (boxes.length === 0) {
      return {
        boxes,
        links,
        routes: [] as Array<{ link: Link; route: Route }>,
        width: 0,
        height: 0,
      };
    }

    let size: { width: number; height: number };
    let headerBottom = 0;
    if (layout === 'radial') size = placeRadial(boxes, links);
    else if (layout === 'tree') size = placeTree(boxes, links);
    else if (layout === 'sequence') {
      const placed = placeSequence(boxes, links, columnWidth);
      size = placed;
      headerBottom = placed.headerBottom;
    } else size = placeLayered(boxes, links);

    const byId = new Map(boxes.map((box) => [box.id, box]));
    const routes: Array<{ link: Link; route: Route }> = [];

    if (layout === 'sequence') {
      const lane = new Map(boxes.map((box) => [box.id, box.x + box.w / 2]));
      links.forEach((link, index) => {
        const y = headerBottom + SEQ_STEP * (index + 1);
        const sx = lane.get(link.from) ?? 0;
        const tx = lane.get(link.to) ?? 0;
        if (sx === tx) {
          // A message to itself: a loop to the right of the lifeline.
          const points = [
            { x: sx, y: y - 6 },
            { x: sx + 22, y: y - 6 },
            { x: sx + 22, y: y + 6 },
            { x: sx, y: y + 6 },
          ];
          routes.push({ link, route: { points, curved: false, label: { x: sx + 26, y } } });
          return;
        }
        const start = { x: sx, y };
        const end = { x: tx, y };
        if (routing === 'curved') {
          const c1 = { x: sx + (tx - sx) * 0.4, y: y - 9 };
          const c2 = { x: tx - (tx - sx) * 0.4, y: y - 9 };
          routes.push({
            link,
            route: {
              points: [start, c1, c2, end],
              curved: true,
              label: bezierAt(start, c1, c2, end, 0.5),
            },
          });
          return;
        }
        routes.push({
          link,
          route: { points: [start, end], curved: false, label: { x: (sx + tx) / 2, y: y - 5 } },
        });
      });
    } else {
      const lane =
        layout === 'tree'
          ? boxes.reduce((most, box) => Math.max(most, box.x + box.w), 0) + LANE
          : boxes.reduce((most, box) => Math.max(most, box.y + box.h), 0) + LANE;

      // Three edges leaving one node would otherwise share a single gutter line
      // and read as one bus. Each gets its own offset within the gutter, in
      // declaration order, clamped so it cannot leave the gutter and touch a
      // box.
      const spread = new Map<string, number>();
      const bySource = new Map<string, Link[]>();
      for (const link of links) {
        const list = bySource.get(link.from);
        if (list) list.push(link);
        else bySource.set(link.from, [link]);
      }
      const room = GAP_X / 2 - 6;
      for (const list of bySource.values()) {
        list.forEach((link, index) => {
          const raw = (index - (list.length - 1) / 2) * 7;
          spread.set(link.key, Math.max(-room, Math.min(room, raw)));
        });
      }

      for (const link of links) {
        const from = byId.get(link.from);
        const to = byId.get(link.to);
        if (!from || !to) continue;
        routes.push({
          link,
          route: routeEdge(from, to, layout, routing, lane, spread.get(link.key) ?? 0),
        });
      }
    }

    return { boxes, links, routes, width: size.width, height: size.height };
  }, [nodes, edges, layout, routing, model, family, columnWidth]);

  const total = height ?? DEFAULT_HEIGHT;
  const offset = Math.max(0, (width - graph.width) / 2);
  const incident = (id: string) =>
    graph.links.some(
      (link) => (link.from === hover && link.to === id) || (link.to === hover && link.from === id),
    );

  function fire(id: string) {
    const action = actionsConfig?.actions.find((entry) => entry.id === id);
    if (!action) {
      useApp.getState().addNotice(`No action named \`${id}\`.`, 'error');
      return;
    }
    void runAction(action);
  }

  function activate(event: KeyboardEvent<SVGGElement>, id: string) {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    fire(id);
  }

  const summary = `${graph.boxes.length} nodes, ${graph.links.length} edges, ${layout} layout`;

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="vd-label m-0">{label}</h3>
        <span className="vd-tabular text-[10.5px] text-[var(--vd-text-tertiary)]">
          {graph.boxes.length} · {graph.links.length}
        </span>
      </div>

      <div
        ref={frame}
        className="vd-code-scroll w-full overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)]"
        style={{ height: total }}
      >
        {graph.boxes.length === 0 ? (
          <p className="m-0 p-2 text-[11px] text-[var(--vd-text-faint)]">—</p>
        ) : (
          width > 0 && (
            <svg
              width={Math.max(width, graph.width)}
              height={graph.height}
              role="group"
              aria-label={`${label}. ${summary}.`}
              data-vd-diagram={layout}
              data-vd-diagram-routing={routing}
            >
              <g transform={`translate(${offset},0)`}>
                {layout === 'sequence' &&
                  graph.boxes.map((box) => (
                    <line
                      key={`life-${box.id}`}
                      x1={box.x + box.w / 2}
                      x2={box.x + box.w / 2}
                      y1={box.y + box.h}
                      y2={graph.height - MARGIN / 2}
                      stroke="var(--vd-line)"
                      strokeWidth={1}
                      strokeDasharray="3 3"
                    />
                  ))}

                {graph.routes.map(({ link, route }) => {
                  const lit = hover !== null && (link.from === hover || link.to === hover);
                  const dim = hover !== null && !lit;
                  const colour = lit ? 'var(--vd-accent)' : 'var(--vd-line-strong)';
                  const text = link.label;
                  return (
                    <g key={link.key} data-vd-edge={link.key} style={{ opacity: dim ? 0.18 : 1 }}>
                      <title>
                        {`${nameOf(graph.boxes, link.from)} → ${nameOf(graph.boxes, link.to)}${
                          text ? `: ${text}` : ''
                        }`}
                      </title>
                      <path
                        d={pathOf(route)}
                        fill="none"
                        stroke={colour}
                        strokeWidth={lit ? 1.6 : 1.2}
                        strokeLinejoin="round"
                        strokeLinecap="round"
                      />
                      <polygon points={arrowOf(route)} fill={colour} />
                      {text && route.label && (
                        <text
                          x={route.label.x}
                          y={route.label.y}
                          textAnchor="middle"
                          dominantBaseline="middle"
                          fill="var(--vd-text-tertiary)"
                          stroke="var(--vd-surface-sunken)"
                          strokeWidth={3}
                          paintOrder="stroke"
                          className="text-[9.5px]"
                        >
                          {text}
                        </text>
                      )}
                    </g>
                  );
                })}

                {graph.boxes.map((box) => {
                  const tone = TONES[box.tone];
                  const dim = hover !== null && hover !== box.id && !incident(box.id);
                  const description = describe(box);
                  return (
                    <g
                      key={box.id}
                      data-vd-node={box.id}
                      data-vd-tone={box.tone}
                      role={box.action ? 'button' : 'img'}
                      tabIndex={box.action ? 0 : undefined}
                      aria-label={description}
                      onPointerEnter={() => setHover(box.id)}
                      onPointerLeave={() => setHover(null)}
                      onClick={box.action ? () => fire(box.action as string) : undefined}
                      onKeyDown={
                        box.action ? (event) => activate(event, box.action as string) : undefined
                      }
                      style={{
                        opacity: dim ? 0.4 : 1,
                        cursor: box.action ? 'pointer' : 'default',
                      }}
                    >
                      <title>{description}</title>
                      <rect
                        x={box.x}
                        y={box.y}
                        width={box.w}
                        height={box.h}
                        rx={6}
                        fill={tone.fill}
                        stroke={tone.stroke}
                        strokeWidth={hover === box.id ? 1.7 : 1}
                      />
                      <text
                        x={box.x + box.w / 2}
                        y={box.detail ? box.y + 17 : box.y + box.h / 2}
                        textAnchor="middle"
                        dominantBaseline="middle"
                        fill={tone.text}
                        className="text-[11px] font-semibold"
                      >
                        {box.label}
                      </text>
                      {box.detail && (
                        <text
                          x={box.x + box.w / 2}
                          y={box.y + 31}
                          textAnchor="middle"
                          dominantBaseline="middle"
                          fill="var(--vd-text-tertiary)"
                          className="text-[10px]"
                        >
                          {box.detail}
                        </text>
                      )}
                    </g>
                  );
                })}
              </g>
            </svg>
          )
        )}
      </div>

      {/*
        The edges are the one thing the drawing cannot say out loud: a `<title>`
        on a non-focusable group is not announced. This list is the linear
        reading of the graph, and it is the only place the connections exist as
        text.
      */}
      <ul style={HIDDEN} aria-label={`${label} connections`}>
        {graph.routes.map(({ link }) => (
          <li key={link.key}>
            {`${nameOf(graph.boxes, link.from)} to ${nameOf(graph.boxes, link.to)}${
              link.label ? `: ${link.label}` : ''
            }`}
          </li>
        ))}
      </ul>
    </div>
  );
}

function nameOf(boxes: Box[], id: string): string {
  return boxes.find((box) => box.id === id)?.label ?? id;
}

function describe(box: Box): string {
  const parts = [box.label];
  if (box.detail) parts.push(box.detail);
  if (box.tone !== 'neutral') parts.push(box.tone);
  if (box.action) parts.push('activates an action');
  return parts.join(', ');
}

/** A resolved tone, or `neutral` for anything outside the closed set. */
function toneOf(value: unknown): DiagramTone {
  return typeof value === 'string' && (TONE_NAMES as string[]).includes(value)
    ? (value as DiagramTone)
    : 'neutral';
}
