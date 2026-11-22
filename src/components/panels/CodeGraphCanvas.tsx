/**
 * The code graph, drawn.
 *
 * The panel used to report counts. A count is not a picture: it says the index
 * has 266 nodes without saying that they are twenty files, a hundred functions
 * and a hundred variables wired together in a shape. This draws the shape.
 *
 * Three decisions worth stating, because each one is a place a graph view
 * usually goes wrong:
 *
 * - **The layout is a shelf stack, not a hairball.** Nodes are grouped by label
 *   and each label sits on its own plane at its own depth, so the structure
 *   reads as layers — files in front, the functions and variables they define
 *   behind them. A force layout is available for seeing clusters instead, and
 *   it starts from the shelf positions rather than from noise, so switching
 *   between the two does not throw the picture away.
 *
 * - **Depth is real, not a drop shadow.** Every point is projected through a
 *   perspective divide after a tilt about the horizontal axis, so a node's
 *   radius, its position and its neighbours' positions all agree about how far
 *   away it is. The tilt is a control, not a constant.
 *
 * - **Nothing animates when nothing is happening.** There is no render loop.
 *   The canvas is redrawn when the camera, the selection, the filters or the
 *   size change, and not otherwise. A graph that repaints sixty times a second
 *   to show a still image is a graph that costs a laptop's battery for nothing.
 *
 * Hit testing reads the same projected points the draw pass wrote, so what the
 * cursor finds is what the eye sees — a second, parallel geometry is how a
 * graph view comes to select the wrong node.
 */
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import { createPortal } from 'react-dom';
import { Maximize2, Minimize2, RotateCcw, Search, X } from 'lucide-react';
import type { CodeGraphEdge, CodeGraphGraph, CodeGraphNode } from '@/lib/capabilities';
import { cn } from '@/lib/utils';

/* ------------------------------------------------------------------ *
 * Geometry
 * ------------------------------------------------------------------ */

interface Point {
  x: number;
  y: number;
  z: number;
}

interface Projected {
  x: number;
  y: number;
  scale: number;
  depth: number;
}

interface Camera {
  x: number;
  y: number;
  zoom: number;
}

/** How far the eye sits from the origin, in world units. */
const FOCAL = 2600;

/** How far apart two shelves sit, in world units. */
const SHELF_GAP = 130;

/**
 * The cell a node occupies inside its shelf.
 *
 * Sized against the drawer, not against the expanded view: the drawer is 27rem
 * wide, and a layout that only becomes legible once expanded is a layout that
 * looks broken in the place it is first seen.
 */
const CELL_X = 38;
const CELL_Y = 32;

/**
 * The order shelves stack in.
 *
 * Structural labels first, so the files and folders a reader navigates by are
 * nearest the eye and the functions and variables they contain recede behind
 * them. A label the server adds later sorts after these, alphabetically.
 */
const LABEL_ORDER = [
  'Project',
  'Folder',
  'File',
  'Module',
  'Class',
  'Method',
  'Function',
  'Variable',
  'Section',
  'EnvVar',
  'Branch',
];

/**
 * A hue per label.
 *
 * Chosen to stay distinguishable at four pixels across and to survive both
 * themes: the renderer shifts lightness for the light theme rather than
 * swapping the palette, so a label keeps its identity when the theme changes.
 */
const LABEL_HUE: Record<string, number> = {
  Project: 26,
  Folder: 34,
  File: 205,
  Module: 172,
  Class: 268,
  Method: 320,
  Function: 142,
  Variable: 46,
  Section: 220,
  EnvVar: 0,
  Branch: 292,
};

function hueFor(label: string): number {
  const known = LABEL_HUE[label];
  if (known !== undefined) return known;
  let hash = 0;
  for (let index = 0; index < label.length; index += 1) {
    hash = (hash * 31 + label.charCodeAt(index)) % 360;
  }
  return hash;
}

function colorFor(label: string, dark: boolean, alpha = 1): string {
  const hue = hueFor(label);
  const saturation = dark ? 74 : 58;
  const lightness = dark ? 63 : 40;
  return `hsla(${hue}, ${saturation}%, ${lightness}%, ${alpha})`;
}

/** A deterministic 0..1 from a string, so a layout is the same every time. */
function hashUnit(text: string): number {
  let hash = 2166136261;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return ((hash >>> 0) % 1000) / 1000;
}

function project(point: Point, tilt: number): Projected {
  const cos = Math.cos(tilt);
  const sin = Math.sin(tilt);
  const y = point.y * cos - point.z * sin;
  const z = point.y * sin + point.z * cos;
  // Clamped so a node cannot be projected through the eye and come back
  // mirrored on the far side of the canvas.
  const scale = FOCAL / Math.max(FOCAL * 0.35, FOCAL + z);
  return { x: point.x * scale, y: y * scale, scale, depth: z };
}

/**
 * The world point that projects to a given screen point at a given depth.
 *
 * Dragging a node has to invert the projection, not approximate it: a node that
 * lags behind the cursor by a few pixels reads as a broken drag, and the error
 * grows with the tilt.
 */
function unproject(projX: number, projY: number, z: number, tilt: number): Point {
  const cos = Math.cos(tilt);
  const sin = Math.sin(tilt);
  const denominator = cos - (projY * sin) / FOCAL;
  const y = (projY + z * (sin + (projY * cos) / FOCAL)) / (Math.abs(denominator) < 1e-4 ? 1e-4 : denominator);
  const scale = FOCAL / Math.max(FOCAL * 0.35, FOCAL + y * sin + z * cos);
  return { x: projX / scale, y, z };
}

/* ------------------------------------------------------------------ *
 * Layouts
 * ------------------------------------------------------------------ */

/**
 * Nodes grouped by label, each label on its own plane.
 *
 * Within a shelf the nodes sit on a wide grid rather than a square one: a shelf
 * that is as tall as it is wide reads as a block, and the point of a shelf is
 * that it is wide.
 */
function shelfLayout(nodes: CodeGraphNode[]): Map<string, Point> {
  const groups = new Map<string, CodeGraphNode[]>();
  for (const node of nodes) {
    const list = groups.get(node.label);
    if (list) list.push(node);
    else groups.set(node.label, [node]);
  }

  const order = [...groups.keys()].sort((a, b) => {
    const left = LABEL_ORDER.indexOf(a);
    const right = LABEL_ORDER.indexOf(b);
    if (left !== right) return (left === -1 ? 999 : left) - (right === -1 ? 999 : right);
    return a.localeCompare(b);
  });

  const positions = new Map<string, Point>();

  order.forEach((label, shelf) => {
    const members = groups.get(label) ?? [];
    const columns = Math.max(1, Math.ceil(Math.sqrt(members.length * 2.4)));
    const rows = Math.ceil(members.length / columns);
    const width = (columns - 1) * CELL_X;
    const height = (rows - 1) * CELL_Y;

    members.forEach((node, index) => {
      const column = index % columns;
      const row = Math.floor(index / columns);
      positions.set(node.id, {
        x: column * CELL_X - width / 2 + (hashUnit(node.id) - 0.5) * 7,
        y: row * CELL_Y - height / 2 + (hashUnit(`${node.id}:y`) - 0.5) * 7,
        z: shelf * SHELF_GAP,
      });
    });
  });

  return positions;
}

interface Body {
  x: number;
  y: number;
  z: number;
  dx: number;
  dy: number;
  dz: number;
}

/**
 * A force layout in three dimensions, seeded from the shelf positions.
 *
 * Repulsion runs through a uniform grid rather than over every pair: at two
 * hundred nodes the difference is invisible, at two thousand it is the
 * difference between a layout and a freeze. The seed matters as much as the
 * physics — starting from the shelves means the result keeps the label
 * grouping instead of scattering it.
 */
function forceLayout(
  nodes: CodeGraphNode[],
  edges: CodeGraphEdge[],
  seed: Map<string, Point>,
): Map<string, Point> {
  const index = new Map<string, number>();
  nodes.forEach((node, at) => index.set(node.id, at));

  const bodies: Body[] = nodes.map((node) => {
    const start = seed.get(node.id) ?? { x: 0, y: 0, z: 0 };
    return { x: start.x, y: start.y, z: start.z, dx: 0, dy: 0, dz: 0 };
  });

  const links: Array<[number, number]> = [];
  for (const edge of edges) {
    const from = index.get(edge.source);
    const to = index.get(edge.target);
    if (from === undefined || to === undefined || from === to) continue;
    links.push([from, to]);
  }

  const ideal = 48;
  const iterations = 200;
  const cell = ideal * 2.4;

  for (let step = 0; step < iterations; step += 1) {
    const cooling = 1 - step / iterations;

    for (const body of bodies) {
      body.dx = 0;
      body.dy = 0;
      body.dz = 0;
    }

    const buckets = new Map<string, number[]>();
    bodies.forEach((body, at) => {
      const key = `${Math.floor(body.x / cell)},${Math.floor(body.y / cell)},${Math.floor(body.z / cell)}`;
      const bucket = buckets.get(key);
      if (bucket) bucket.push(at);
      else buckets.set(key, [at]);
    });

    bodies.forEach((body, at) => {
      const cx = Math.floor(body.x / cell);
      const cy = Math.floor(body.y / cell);
      const cz = Math.floor(body.z / cell);
      for (let ox = -1; ox <= 1; ox += 1) {
        for (let oy = -1; oy <= 1; oy += 1) {
          for (let oz = -1; oz <= 1; oz += 1) {
            const bucket = buckets.get(`${cx + ox},${cy + oy},${cz + oz}`);
            if (!bucket) continue;
            for (const other of bucket) {
              if (other <= at) continue;
              const peer = bodies[other];
              if (!peer) continue;
              let vx = body.x - peer.x;
              let vy = body.y - peer.y;
              let vz = body.z - peer.z;
              let distance = Math.hypot(vx, vy, vz);
              if (distance < 0.01) {
                vx = 0.5;
                vy = 0.5;
                vz = 0.5;
                distance = 0.87;
              }
              const push = (ideal * ideal) / distance / distance;
              body.dx += vx * push;
              body.dy += vy * push;
              body.dz += vz * push;
              peer.dx -= vx * push;
              peer.dy -= vy * push;
              peer.dz -= vz * push;
            }
          }
        }
      }
    });

    for (const [from, to] of links) {
      const first = bodies[from];
      const second = bodies[to];
      if (!first || !second) continue;
      const vx = second.x - first.x;
      const vy = second.y - first.y;
      const vz = second.z - first.z;
      const distance = Math.hypot(vx, vy, vz) || 0.01;
      const pull = distance / ideal / 2;
      first.dx += vx * pull;
      first.dy += vy * pull;
      first.dz += vz * pull;
      second.dx -= vx * pull;
      second.dy -= vy * pull;
      second.dz -= vz * pull;
    }

    for (const body of bodies) {
      body.dx -= body.x * 0.01;
      body.dy -= body.y * 0.01;
      body.dz -= body.z * 0.01;
    }

    const limit = 14 * cooling + 0.4;
    for (const body of bodies) {
      const length = Math.hypot(body.dx, body.dy, body.dz) || 1;
      const scale = Math.min(length, limit) / length;
      body.x += body.dx * scale;
      body.y += body.dy * scale;
      body.z += body.dz * scale;
    }
  }

  const positions = new Map<string, Point>();
  nodes.forEach((node, at) => {
    const body = bodies[at];
    if (body) positions.set(node.id, { x: body.x, y: body.y, z: body.z });
  });
  return positions;
}

/** A camera that puts every projected point inside the canvas. */
function fitCamera(points: Projected[], width: number, height: number): Camera {
  if (points.length === 0) return { x: 0, y: 0, zoom: 1 };

  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const point of points) {
    minX = Math.min(minX, point.x);
    maxX = Math.max(maxX, point.x);
    minY = Math.min(minY, point.y);
    maxY = Math.max(maxY, point.y);
  }

  const spanX = Math.max(1, maxX - minX);
  const spanY = Math.max(1, maxY - minY);
  const zoom = Math.min((width - 56) / spanX, (height - 56) / spanY, 2.4);

  return {
    x: (minX + maxX) / 2,
    y: (minY + maxY) / 2,
    zoom: Math.max(0.05, zoom),
  };
}

/* ------------------------------------------------------------------ *
 * Theme
 * ------------------------------------------------------------------ */

interface Theme {
  dark: boolean;
  accent: string;
  text: string;
  textStrong: string;
}

function parseHex(value: string): [number, number, number] | null {
  const match = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(value.trim());
  if (!match) return null;
  const hex = match[1] ?? '';
  if (hex.length === 3) {
    const expand = (index: number) => parseInt(`${hex[index]}${hex[index]}`, 16);
    return [expand(0), expand(1), expand(2)];
  }
  return [
    parseInt(hex.slice(0, 2), 16),
    parseInt(hex.slice(2, 4), 16),
    parseInt(hex.slice(4, 6), 16),
  ];
}

function luminance([red, green, blue]: [number, number, number]): number {
  const channel = (value: number) => {
    const scaled = value / 255;
    return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue);
}

/**
 * The colours the canvas draws with, read from the live theme.
 *
 * A canvas cannot inherit a CSS variable, so the tokens are read once per
 * redraw. Reading them rather than hard-coding them is what keeps the graph
 * legible when the theme changes — a dark palette on the paper theme is a graph
 * nobody can see.
 */
function readTheme(): Theme {
  const styles = getComputedStyle(document.documentElement);
  const read = (name: string, fallback: string) => styles.getPropertyValue(name).trim() || fallback;

  const canvas = read('--vd-canvas', '#0c0a09');
  const parsed = parseHex(canvas);

  return {
    dark: parsed ? luminance(parsed) < 0.4 : true,
    accent: read('--vd-accent', '#ff7a1a'),
    text: read('--vd-text-tertiary', '#6f655e'),
    textStrong: read('--vd-text-secondary', '#a89e96'),
  };
}

/* ------------------------------------------------------------------ *
 * The component
 * ------------------------------------------------------------------ */

type Layout = 'shelves' | 'force';

interface ScreenPoint {
  node: CodeGraphNode;
  x: number;
  y: number;
  r: number;
}

export function CodeGraphCanvas({ graph }: { graph: CodeGraphGraph }) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const screenRef = useRef<ScreenPoint[]>([]);

  const [size, setSize] = useState({ width: 0, height: 0 });
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [layout, setLayout] = useState<Layout>('shelves');
  const [tilt, setTilt] = useState(0.26);
  const [camera, setCamera] = useState<Camera>({ x: 0, y: 0, zoom: 1 });
  const [hovered, setHovered] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [pinned, setPinned] = useState<Map<string, Point>>(() => new Map());
  const [hiddenLabels, setHiddenLabels] = useState<Set<string>>(() => new Set());
  const [hiddenKinds, setHiddenKinds] = useState<Set<string>>(() => new Set());
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState(false);
  const [showKinds, setShowKinds] = useState(false);

  const dragRef = useRef<{
    mode: 'pan' | 'node';
    id: string | null;
    startX: number;
    startY: number;
    camera: Camera;
    moved: boolean;
  } | null>(null);

  /* -------------------------------------------------------------- *
   * Layout and projection
   * -------------------------------------------------------------- */

  const shelves = useMemo(() => shelfLayout(graph.nodes), [graph.nodes]);

  const positions = useMemo(
    () => (layout === 'shelves' ? shelves : forceLayout(graph.nodes, graph.edges, shelves)),
    [layout, shelves, graph.nodes, graph.edges],
  );

  const projected = useMemo(() => {
    const map = new Map<string, Projected>();
    for (const node of graph.nodes) {
      const base = positions.get(node.id);
      if (!base) continue;
      map.set(node.id, project(pinned.get(node.id) ?? base, tilt));
    }
    return map;
  }, [graph.nodes, positions, pinned, tilt]);

  /* -------------------------------------------------------------- *
   * Environment
   * -------------------------------------------------------------- */

  // Re-attached when the view expands, because expanding replaces the element
  // the observer was watching and a stale observer reports a size that no
  // longer exists.
  useEffect(() => {
    const element = containerRef.current;
    if (!element) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setSize({ width: Math.round(width), height: Math.round(height) });
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [expanded]);

  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(readTheme()));
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'class', 'style'],
    });
    return () => observer.disconnect();
  }, []);

  // A different project is a different graph: keeping the pins and the
  // selection would leave the camera framing nodes that no longer exist.
  useEffect(() => {
    setPinned(new Map());
    setSelected(new Set());
    setHovered(null);
  }, [graph.project]);

  const fittedFor = useRef('');
  useEffect(() => {
    if (size.width === 0 || size.height === 0) return;
    const key = `${layout}:${graph.project}:${graph.nodes.length}:${expanded}`;
    if (fittedFor.current === key) return;
    fittedFor.current = key;
    setCamera(fitCamera([...projected.values()], size.width, size.height));
  }, [projected, size, layout, graph.project, graph.nodes.length, expanded]);

  /* -------------------------------------------------------------- *
   * Focus
   * -------------------------------------------------------------- */

  const active = useMemo(() => {
    const set = new Set(selected);
    if (hovered) set.add(hovered);
    return set;
  }, [selected, hovered]);

  const neighbours = useMemo(() => {
    const set = new Set<string>();
    if (active.size === 0) return set;
    for (const edge of graph.edges) {
      if (active.has(edge.source)) set.add(edge.target);
      if (active.has(edge.target)) set.add(edge.source);
    }
    return set;
  }, [active, graph.edges]);

  const needle = query.trim().toLowerCase();

  const isDimmed = useCallback(
    (node: CodeGraphNode) => {
      if (needle) {
        const hit =
          node.name.toLowerCase().includes(needle) ||
          node.file.toLowerCase().includes(needle) ||
          node.label.toLowerCase().includes(needle);
        if (!hit) return true;
      }
      if (active.size > 0 && !active.has(node.id) && !neighbours.has(node.id)) return true;
      return false;
    },
    [needle, active, neighbours],
  );

  /* -------------------------------------------------------------- *
   * Draw
   * -------------------------------------------------------------- */

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || size.width === 0 || size.height === 0) return;
    const context = canvas.getContext('2d');
    if (!context) return;

    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(size.width * ratio);
    canvas.height = Math.round(size.height * ratio);
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, size.width, size.height);

    const centreX = size.width / 2;
    const centreY = size.height / 2;
    const radiusScale = Math.sqrt(Math.min(camera.zoom, 3));

    const points: ScreenPoint[] = [];
    for (const node of graph.nodes) {
      if (hiddenLabels.has(node.label)) continue;
      const point = projected.get(node.id);
      if (!point) continue;
      points.push({
        node,
        x: centreX + (point.x - camera.x) * camera.zoom,
        y: centreY + (point.y - camera.y) * camera.zoom,
        r: Math.max(1.3, (2.6 + Math.sqrt(node.degree) * 1.15) * point.scale * radiusScale),
      });
    }
    screenRef.current = points;

    const byId = new Map(points.map((point) => [point.node.id, point]));

    /* Edges, batched into three passes so the canvas changes stroke style
       three times instead of seven hundred. */
    const dim = new Path2D();
    const lit = new Path2D();
    const hot = new Path2D();

    for (const edge of graph.edges) {
      if (hiddenKinds.has(edge.kind)) continue;
      const from = byId.get(edge.source);
      const to = byId.get(edge.target);
      if (!from || !to) continue;

      const both = active.has(edge.source) && active.has(edge.target);
      const either = active.has(edge.source) || active.has(edge.target);
      const path = both ? hot : either ? lit : dim;

      const dx = to.x - from.x;
      const dy = to.y - from.y;
      const length = Math.hypot(dx, dy) || 1;
      // A slight bow keeps two edges between the same pair from drawing as one.
      const bow = Math.min(14, length * 0.12);
      path.moveTo(from.x, from.y);
      path.quadraticCurveTo(
        (from.x + to.x) / 2 - (dy / length) * bow,
        (from.y + to.y) / 2 + (dx / length) * bow,
        to.x,
        to.y,
      );
    }

    context.lineWidth = 1;
    context.strokeStyle = theme.dark ? 'rgba(255, 255, 255, 0.055)' : 'rgba(20, 20, 18, 0.07)';
    context.stroke(dim);
    context.strokeStyle = theme.dark ? 'rgba(255, 255, 255, 0.17)' : 'rgba(20, 20, 18, 0.2)';
    context.stroke(lit);
    context.lineWidth = 1.4;
    context.strokeStyle = theme.accent;
    context.stroke(hot);

    /* Shelf edges, so the layers read as layers rather than as a scatter that
       happens to be sorted. */
    if (layout === 'shelves') {
      const shelvesByLabel = new Map<string, ScreenPoint[]>();
      for (const point of points) {
        const list = shelvesByLabel.get(point.node.label);
        if (list) list.push(point);
        else shelvesByLabel.set(point.node.label, [point]);
      }

      context.font = '9px ui-sans-serif, system-ui, sans-serif';
      context.textBaseline = 'middle';
      context.textAlign = 'right';

      for (const [label, members] of shelvesByLabel) {
        let minX = Infinity;
        let maxX = -Infinity;
        let maxY = -Infinity;
        for (const member of members) {
          minX = Math.min(minX, member.x);
          maxX = Math.max(maxX, member.x);
          maxY = Math.max(maxY, member.y);
        }
        const lineY = maxY + 12;

        context.beginPath();
        context.moveTo(minX - 14, lineY);
        context.lineTo(maxX + 14, lineY);
        context.strokeStyle = colorFor(label, theme.dark, 0.16);
        context.lineWidth = 1;
        context.stroke();

        context.fillStyle = colorFor(label, theme.dark, 0.62);
        // Clamped away from the edge: a shelf label that runs off the canvas
        // reads as a rendering fault rather than as a label.
        context.fillText(label, Math.max(64, minX - 20), lineY);
      }

      context.textAlign = 'left';
    }

    /* Auras, under the nodes so the core stays crisp. */
    const aura = (point: ScreenPoint, strength: number) => {
      const radius = Math.max(10, point.r * 7);
      const gradient = context.createRadialGradient(point.x, point.y, 0, point.x, point.y, radius);
      gradient.addColorStop(0, colorFor(point.node.label, theme.dark, 0.5 * strength));
      gradient.addColorStop(0.35, colorFor(point.node.label, theme.dark, 0.16 * strength));
      gradient.addColorStop(1, colorFor(point.node.label, theme.dark, 0));
      context.fillStyle = gradient;
      context.beginPath();
      context.arc(point.x, point.y, radius, 0, Math.PI * 2);
      context.fill();
    };

    context.globalCompositeOperation = theme.dark ? 'lighter' : 'source-over';
    for (const point of points) {
      if (selected.has(point.node.id)) aura(point, 1);
      else if (hovered === point.node.id) aura(point, 0.85);
    }
    context.globalCompositeOperation = 'source-over';

    /* Nodes, grouped by label so the fill style changes once per label. */
    // A selection is a deliberate focus, so it dims hard. A hover is a glance,
    // so it dims gently: a graph that goes dark every time the cursor crosses
    // it is a graph that is hard to read.
    const dimAlpha = selected.size > 0 ? 0.14 : hovered ? 0.34 : 0.16;
    const groups = new Map<string, { lit: ScreenPoint[]; dim: ScreenPoint[] }>();
    for (const point of points) {
      const group = groups.get(point.node.label) ?? { lit: [], dim: [] };
      if (isDimmed(point.node)) group.dim.push(point);
      else group.lit.push(point);
      groups.set(point.node.label, group);
    }

    for (const [label, group] of groups) {
      if (group.dim.length > 0) {
        const path = new Path2D();
        for (const point of group.dim) {
          path.moveTo(point.x + point.r, point.y);
          path.arc(point.x, point.y, point.r, 0, Math.PI * 2);
        }
        context.globalAlpha = dimAlpha;
        context.fillStyle = colorFor(label, theme.dark);
        context.fill(path);
      }
      if (group.lit.length > 0) {
        const path = new Path2D();
        for (const point of group.lit) {
          path.moveTo(point.x + point.r, point.y);
          path.arc(point.x, point.y, point.r, 0, Math.PI * 2);
        }
        context.globalAlpha = 1;
        context.fillStyle = colorFor(label, theme.dark);
        context.fill(path);
      }
    }
    context.globalAlpha = 1;

    /* A ring on every selected node, so a multi-selection is countable. */
    if (selected.size > 0) {
      const ring = new Path2D();
      for (const point of points) {
        if (!selected.has(point.node.id)) continue;
        ring.moveTo(point.x + point.r + 3, point.y);
        ring.arc(point.x, point.y, point.r + 3, 0, Math.PI * 2);
      }
      context.strokeStyle = theme.accent;
      context.lineWidth = 1.5;
      context.stroke(ring);
    }

    /* Labels: the focused nodes always, then the most connected ones. The
       degree floor matters more than the radius floor — at the drawer's zoom
       every node is a few pixels across, so a radius test alone would leave the
       overview unlabelled and unreadable. */
    const labelled = points
      .filter((point) => {
        if (selected.has(point.node.id) || hovered === point.node.id) return true;
        if (isDimmed(point.node)) return false;
        return point.r > 3.2 || point.node.degree >= 12;
      })
      .sort((a, b) => b.node.degree - a.node.degree)
      .slice(0, 80);

    context.font = '9.5px ui-sans-serif, system-ui, sans-serif';
    context.textBaseline = 'middle';
    context.textAlign = 'left';
    for (const point of labelled) {
      const name = point.node.name;
      const text = name.length > 24 ? `${name.slice(0, 23)}…` : name;
      const x = point.x + point.r + 4;
      const y = point.y;
      context.strokeStyle = theme.dark ? 'rgba(12, 10, 9, 0.82)' : 'rgba(245, 243, 238, 0.9)';
      context.lineWidth = 2.5;
      context.strokeText(text, x, y);
      context.fillStyle =
        selected.has(point.node.id) || hovered === point.node.id ? theme.textStrong : theme.text;
      context.fillText(text, x, y);
    }
  }, [
    size,
    projected,
    camera,
    theme,
    layout,
    graph.nodes,
    graph.edges,
    hiddenLabels,
    hiddenKinds,
    active,
    selected,
    hovered,
    isDimmed,
  ]);

  /* -------------------------------------------------------------- *
   * Interaction
   * -------------------------------------------------------------- */

  const hitTest = useCallback((x: number, y: number): string | null => {
    let best: string | null = null;
    let bestDistance = Infinity;
    for (const point of screenRef.current) {
      const distance = Math.hypot(point.x - x, point.y - y);
      if (distance <= point.r + 6 && distance < bestDistance) {
        bestDistance = distance;
        best = point.node.id;
      }
    }
    return best;
  }, []);

  const localPoint = (event: { clientX: number; clientY: number }) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const { x, y } = localPoint(event);
    const hit = hitTest(x, y);
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      mode: hit ? 'node' : 'pan',
      id: hit,
      startX: x,
      startY: y,
      camera,
      moved: false,
    };
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const { x, y } = localPoint(event);
    const drag = dragRef.current;

    if (!drag) {
      const hit = hitTest(x, y);
      if (hit !== hovered) setHovered(hit);
      return;
    }

    const dx = x - drag.startX;
    const dy = y - drag.startY;
    if (Math.hypot(dx, dy) > 3) drag.moved = true;

    if (drag.mode === 'pan') {
      setCamera({
        zoom: drag.camera.zoom,
        x: drag.camera.x - dx / drag.camera.zoom,
        y: drag.camera.y - dy / drag.camera.zoom,
      });
      return;
    }

    if (!drag.id) return;
    const depth = positions.get(drag.id)?.z ?? 0;
    const world = unproject(
      (x - size.width / 2) / camera.zoom + camera.x,
      (y - size.height / 2) / camera.zoom + camera.y,
      depth,
      tilt,
    );
    setPinned((current) => new Map(current).set(drag.id as string, world));
  };

  const onPointerUp = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (!drag || drag.moved) return;

    const { x, y } = localPoint(event);
    const hit = hitTest(x, y);
    const additive = event.shiftKey || event.metaKey || event.ctrlKey;

    if (!hit) {
      if (!additive) setSelected(new Set());
      return;
    }

    setSelected((current) => {
      if (!additive) {
        // A plain click on the only selected node clears it; a plain click on
        // anything else replaces the selection with that node. Without the
        // first branch a selected node could never be deselected by clicking
        // it, because the replacement set starts empty and then adds it back.
        if (current.size === 1 && current.has(hit)) return new Set();
        return new Set([hit]);
      }
      const next = new Set(current);
      if (next.has(hit)) next.delete(hit);
      else next.add(hit);
      return next;
    });
  };

  const onPointerLeave = () => {
    if (!dragRef.current) setHovered(null);
  };

  // A native listener, because React's wheel handler is passive and a passive
  // handler cannot stop the drawer from scrolling under the zoom.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const rect = canvas.getBoundingClientRect();
      const x = event.clientX - rect.left;
      const y = event.clientY - rect.top;
      const factor = Math.exp(-event.deltaY * 0.0015);

      setCamera((current) => {
        const zoom = Math.min(8, Math.max(0.04, current.zoom * factor));
        const worldX = (x - size.width / 2) / current.zoom + current.x;
        const worldY = (y - size.height / 2) / current.zoom + current.y;
        return {
          zoom,
          x: worldX - (x - size.width / 2) / zoom,
          y: worldY - (y - size.height / 2) / zoom,
        };
      });
    };

    canvas.addEventListener('wheel', onWheel, { passive: false });
    return () => canvas.removeEventListener('wheel', onWheel);
  }, [size, expanded]);

  const fit = useCallback(() => {
    setPinned(new Map());
    if (size.width === 0) return;
    setCamera(fitCamera([...projected.values()], size.width, size.height));
  }, [projected, size]);

  const toggleLabel = (label: string) => {
    setHiddenLabels((current) => {
      const next = new Set(current);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });
  };

  const toggleKind = (kind: string) => {
    setHiddenKinds((current) => {
      const next = new Set(current);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  };

  /* -------------------------------------------------------------- *
   * Render
   * -------------------------------------------------------------- */

  const hoveredNode = hovered ? graph.nodes.find((node) => node.id === hovered) : undefined;
  const hoveredPoint = hovered ? projected.get(hovered) : undefined;
  const selectedNodes = graph.nodes.filter((node) => selected.has(node.id));
  const single = selectedNodes.length === 1 ? selectedNodes[0] : undefined;

  const tooltip = hoveredNode && hoveredPoint
    ? {
        left: Math.min(
          Math.max(8, size.width / 2 + (hoveredPoint.x - camera.x) * camera.zoom + 14),
          Math.max(8, size.width - 190),
        ),
        top: Math.min(
          Math.max(8, size.height / 2 + (hoveredPoint.y - camera.y) * camera.zoom + 14),
          Math.max(8, size.height - 74),
        ),
      }
    : null;

  const body = (
    <div className={cn('flex min-h-0 flex-col gap-2', expanded && 'h-full')}>
      <div className="flex flex-wrap items-center gap-1.5">
        <div className="flex overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line)]">
          {(['shelves', 'force'] as const).map((mode) => (
            <button
              key={mode}
              type="button"
              onClick={() => setLayout(mode)}
              className={cn(
                'px-2 py-1 text-[10.5px] capitalize transition-colors',
                layout === mode
                  ? 'bg-[var(--vd-accent-wash)] text-[var(--vd-accent)]'
                  : 'text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]',
              )}
            >
              {mode}
            </button>
          ))}
        </div>

        <label className="flex items-center gap-1 text-[10px] text-[var(--vd-text-tertiary)]">
          tilt
          <input
            type="range"
            min={0}
            max={70}
            value={Math.round((tilt * 180) / Math.PI)}
            onChange={(event) => setTilt((Number(event.target.value) * Math.PI) / 180)}
            className="h-1 w-14 accent-[var(--vd-accent)]"
            aria-label="Tilt"
          />
        </label>

        <button type="button" onClick={fit} className="vd-icon-btn" title="Fit to view">
          <RotateCcw size={12} strokeWidth={2} />
        </button>
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="vd-icon-btn"
          title={expanded ? 'Collapse' : 'Expand'}
        >
          {expanded ? <Minimize2 size={12} strokeWidth={2} /> : <Maximize2 size={12} strokeWidth={2} />}
        </button>
      </div>

      <div className="relative">
        <Search
          size={11}
          strokeWidth={2}
          className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-[var(--vd-text-tertiary)]"
        />
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Filter by name, file or label"
          className="w-full rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] py-1 pl-6 pr-6 text-[11px] text-[var(--vd-text)] outline-none placeholder:text-[var(--vd-text-tertiary)] focus:border-[var(--vd-accent)]"
        />
        {query && (
          <button
            type="button"
            onClick={() => setQuery('')}
            className="absolute right-1.5 top-1/2 -translate-y-1/2 text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text)]"
            title="Clear filter"
          >
            <X size={11} strokeWidth={2} />
          </button>
        )}
      </div>

      <div
        ref={containerRef}
        className={cn(
          'relative overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)]',
          expanded ? 'min-h-0 flex-1' : 'h-[340px]',
        )}
      >
        <canvas
          ref={canvasRef}
          aria-label={`Code graph of ${graph.project}: ${graph.nodes.length} nodes, ${graph.edges.length} edges`}
          className={cn(
            'block h-full w-full touch-none',
            hovered ? 'cursor-pointer' : 'cursor-grab active:cursor-grabbing',
          )}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerLeave={onPointerLeave}
        />

        {tooltip && hoveredNode && (
          <div
            className="pointer-events-none absolute z-10 max-w-[180px] rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-overlay)] px-2 py-1.5 shadow-[var(--vd-shadow-pop)]"
            style={{ left: tooltip.left, top: tooltip.top }}
          >
            <div className="truncate font-mono text-[10.5px] text-[var(--vd-text)]">
              {hoveredNode.name}
            </div>
            <div className="mt-0.5 flex items-center gap-1.5 text-[9.5px] text-[var(--vd-text-tertiary)]">
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ background: colorFor(hoveredNode.label, theme.dark) }}
              />
              {hoveredNode.label}
              <span className="vd-numeric ml-auto">{hoveredNode.degree} edges</span>
            </div>
            {hoveredNode.file && (
              <div className="mt-0.5 truncate font-mono text-[9.5px] text-[var(--vd-text-tertiary)]">
                {hoveredNode.file}
                {hoveredNode.line !== null ? `:${hoveredNode.line}` : ''}
              </div>
            )}
          </div>
        )}

        {graph.nodes.length === 0 && (
          <div className="absolute inset-0 flex items-center justify-center px-6 text-center text-[11px] text-[var(--vd-text-tertiary)]">
            The index reports no nodes for this project.
          </div>
        )}
      </div>

      <div className="flex flex-wrap gap-1">
        {graph.labels.map((entry) => {
          const off = hiddenLabels.has(entry.label);
          return (
            <button
              key={entry.label}
              type="button"
              onClick={() => toggleLabel(entry.label)}
              title={off ? `Show ${entry.label}` : `Hide ${entry.label}`}
              className={cn(
                'flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-[10px] transition-opacity',
                off
                  ? 'border-[var(--vd-line-soft)] text-[var(--vd-text-tertiary)] opacity-45'
                  : 'border-[var(--vd-line)] text-[var(--vd-text-secondary)]',
              )}
            >
              <span
                className="h-1.5 w-1.5 rounded-full"
                style={{ background: colorFor(entry.label, theme.dark) }}
              />
              {entry.label}
              <span className="vd-numeric text-[var(--vd-text-tertiary)]">{entry.count}</span>
            </button>
          );
        })}
      </div>

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => setShowKinds((value) => !value)}
          className="text-[10px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
        >
          {showKinds ? 'Hide' : 'Show'} {graph.kinds.length} edge types
        </button>
        {selected.size > 0 && (
          <button
            type="button"
            onClick={() => setSelected(new Set())}
            className="ml-auto text-[10px] text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]"
          >
            Clear {selected.size} selected
          </button>
        )}
      </div>

      {showKinds && (
        <div className="flex flex-wrap gap-1">
          {graph.kinds.map((entry) => {
            const kind = entry.label;
            const off = hiddenKinds.has(kind);
            return (
              <button
                key={kind}
                type="button"
                onClick={() => toggleKind(kind)}
                className={cn(
                  'rounded-full border px-1.5 py-0.5 font-mono text-[9.5px] transition-opacity',
                  off
                    ? 'border-[var(--vd-line-soft)] text-[var(--vd-text-tertiary)] opacity-45'
                    : 'border-[var(--vd-line)] text-[var(--vd-text-secondary)]',
                )}
              >
                {kind}
                <span className="vd-numeric ml-1 text-[var(--vd-text-tertiary)]">{entry.count}</span>
              </button>
            );
          })}
        </div>
      )}

      {single && (
        <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5">
          <div className="flex items-baseline gap-2">
            <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-[var(--vd-text)]">
              {single.name}
            </span>
            <span
              className="shrink-0 rounded-full px-1.5 text-[9.5px]"
              style={{
                background: colorFor(single.label, theme.dark, 0.16),
                color: colorFor(single.label, theme.dark),
              }}
            >
              {single.label}
            </span>
          </div>
          <div className="mt-1 grid grid-cols-2 gap-x-3 gap-y-0.5 text-[10px] text-[var(--vd-text-tertiary)]">
            <span className="truncate font-mono">
              {single.file || '—'}
              {single.line !== null ? `:${single.line}` : ''}
            </span>
            <span className="vd-numeric text-right">{single.degree} edges</span>
            {single.complexity !== null && (
              <span className="vd-numeric">complexity {single.complexity.toFixed(1)}</span>
            )}
            {single.lines !== null && <span className="vd-numeric text-right">{single.lines} lines</span>}
            {single.entry && <span className="text-[var(--vd-accent)]">entry point</span>}
            {single.test && <span className="text-right">test</span>}
          </div>
        </div>
      )}

      {selectedNodes.length > 1 && (
        <div className="rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)] px-2 py-1.5 text-[10px] text-[var(--vd-text-tertiary)]">
          {selectedNodes.length} nodes selected ·{' '}
          {selectedNodes.reduce((total, node) => total + node.degree, 0)} edges between them and the
          rest
        </div>
      )}
    </div>
  );

  if (expanded) {
    return (
      <>
        <p className="m-0 text-[11px] text-[var(--vd-text-tertiary)]">
          The graph is open in the expanded view.
        </p>
        {createPortal(
          <div className="fixed inset-0 z-50 flex flex-col bg-[var(--vd-canvas)] p-4">
            <div className="mb-2 flex items-center gap-2">
              <h2 className="m-0 text-[12.5px] font-semibold text-[var(--vd-text)]">
                {graph.project}
              </h2>
              <span className="text-[10.5px] text-[var(--vd-text-tertiary)]">
                {graph.nodes.length} nodes · {graph.edges.length} edges
              </span>
              <button
                type="button"
                onClick={() => setExpanded(false)}
                className="vd-icon-btn ml-auto"
                title="Close"
              >
                <X size={14} strokeWidth={2} />
              </button>
            </div>
            {body}
          </div>,
          document.body,
        )}
      </>
    );
  }

  return body;
}
