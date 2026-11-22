/**
 * A sanitiser for agent-authored SVG.
 *
 * A fenced ```` ```svg ```` block is markup the model wrote, and it is injected
 * into the transcript. The CSP stops script and external loads, but it does not
 * stop navigation: an `<a href="https://…">` inside the drawing navigates the
 * whole webview away from the app, and there is no way back except restarting
 * it. So the markup is rebuilt from an allowlist rather than trusted.
 *
 * The allowlist is deliberately a drawing allowlist. Anything that can reach the
 * network, run code, or lay itself over the window is dropped:
 *
 * - no `<script>`, `<foreignObject>`, `<style>`, `<image>`, `<a>`
 * - no `on*` handlers, no `style` attribute
 * - `href` and `xlink:href` survive only as same-document `#id` references, so
 *   `<use>` still works and a link cannot
 *
 * Unknown elements are unwrapped — their children are kept, the element is not —
 * so a `<foo><path/></foo>` still draws its path instead of vanishing.
 *
 * The policy is a pure function over a plain tree (`sanitizeTree`), and the DOM
 * is only the adapter that parses and serialises. That split is what lets the
 * policy be unit-tested without a browser, which matters because the policy is
 * the part that has to be right.
 */

/** The shape the policy works on. Deliberately not a DOM node. */
export interface SvgNode {
  name: string;
  attributes: { name: string; value: string }[];
  children: SvgNode[];
  /** Text content, for the elements that carry any. */
  text?: string;
}

const ELEMENTS = new Set([
  'svg',
  'g',
  'defs',
  'symbol',
  'use',
  'path',
  'rect',
  'circle',
  'ellipse',
  'line',
  'polyline',
  'polygon',
  'text',
  'tspan',
  'title',
  'desc',
  'clipPath',
  'mask',
  'pattern',
  'marker',
  'linearGradient',
  'radialGradient',
  'stop',
  'filter',
  'feBlend',
  'feColorMatrix',
  'feComponentTransfer',
  'feComposite',
  'feDisplacementMap',
  'feDropShadow',
  'feFlood',
  'feFuncA',
  'feFuncB',
  'feFuncG',
  'feFuncR',
  'feGaussianBlur',
  'feMerge',
  'feMergeNode',
  'feMorphology',
  'feOffset',
  'feTurbulence',
]);

const ATTRIBUTES = new Set([
  'd',
  'x',
  'y',
  'x1',
  'y1',
  'x2',
  'y2',
  'cx',
  'cy',
  'r',
  'rx',
  'ry',
  'width',
  'height',
  'viewBox',
  'preserveAspectRatio',
  'points',
  'transform',
  'fill',
  'fill-opacity',
  'fill-rule',
  'stroke',
  'stroke-width',
  'stroke-linecap',
  'stroke-linejoin',
  'stroke-dasharray',
  'stroke-dashoffset',
  'stroke-opacity',
  'stroke-miterlimit',
  'opacity',
  'offset',
  'stop-color',
  'stop-opacity',
  'gradientUnits',
  'gradientTransform',
  'spreadMethod',
  'clip-path',
  'clip-rule',
  'clipPathUnits',
  'mask',
  'maskUnits',
  'maskContentUnits',
  'patternUnits',
  'patternContentUnits',
  'patternTransform',
  'markerWidth',
  'markerHeight',
  'markerUnits',
  'refX',
  'refY',
  'orient',
  'filter',
  'filterUnits',
  'primitiveUnits',
  'stdDeviation',
  'in',
  'in2',
  'result',
  'mode',
  'values',
  'type',
  'operator',
  'k1',
  'k2',
  'k3',
  'k4',
  'baseFrequency',
  'numOctaves',
  'seed',
  'scale',
  'dx',
  'dy',
  'rotate',
  'text-anchor',
  'dominant-baseline',
  'font-family',
  'font-size',
  'font-weight',
  'font-style',
  'letter-spacing',
  'textLength',
  'lengthAdjust',
  'id',
  'class',
  'role',
  'aria-label',
  'aria-hidden',
  'focusable',
  'xmlns',
  'xmlns:xlink',
  'version',
]);

/** Attributes whose value is a URL, and the only shape of URL allowed. */
const URL_ATTRIBUTES = new Set(['href', 'xlink:href']);

function safeUrl(value: string): boolean {
  return value.trim().startsWith('#');
}

function keepAttribute(attribute: { name: string; value: string }): boolean {
  if (ATTRIBUTES.has(attribute.name)) return true;
  if (URL_ATTRIBUTES.has(attribute.name)) return safeUrl(attribute.value);
  return false;
}

/**
 * The policy. Returns the node with only allowed elements and attributes, or
 * `null` when the root is not an `<svg>`.
 *
 * A disallowed element is unwrapped, not deleted: its children are sanitised and
 * hoisted into its parent. That is what keeps a drawing inside an unknown
 * wrapper visible while still removing the wrapper.
 */
export function sanitizeTree(node: SvgNode): SvgNode | null {
  if (node.name !== 'svg') return null;

  const clean = (source: SvgNode): SvgNode[] => {
    const out: SvgNode[] = [];
    for (const child of source.children) {
      if (!ELEMENTS.has(child.name)) {
        out.push(...clean(child));
        continue;
      }
      out.push({
        name: child.name,
        attributes: child.attributes.filter(keepAttribute),
        children: clean(child),
        ...(child.text === undefined ? {} : { text: child.text }),
      });
    }
    return out;
  };

  return {
    name: 'svg',
    attributes: node.attributes.filter(keepAttribute),
    children: clean(node),
    ...(node.text === undefined ? {} : { text: node.text }),
  };
}

/* ---------------------------------------------------------------- *
 * DOM adapter
 * ---------------------------------------------------------------- */

function toNode(element: Element): SvgNode {
  const children: SvgNode[] = [];
  for (const child of element.children) children.push(toNode(child));
  const text = element.children.length === 0 ? (element.textContent ?? '') : undefined;
  return {
    name: element.localName,
    attributes: [...element.attributes].map((attribute) => ({
      name: attribute.name,
      value: attribute.value,
    })),
    children,
    ...(text === undefined || text === '' ? {} : { text }),
  };
}

function escapeText(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function escapeAttribute(value: string): string {
  return escapeText(value).replace(/"/g, '&quot;');
}

function serialise(node: SvgNode): string {
  const attributes = node.attributes
    .map((attribute) => ` ${attribute.name}="${escapeAttribute(attribute.value)}"`)
    .join('');
  const inner = node.children.map(serialise).join('');
  const text = node.text === undefined ? '' : escapeText(node.text);
  if (inner === '' && text === '') return `<${node.name}${attributes}/>`;
  return `<${node.name}${attributes}>${text}${inner}</${node.name}>`;
}

/**
 * Returns markup that is safe to inject, or `null` when the source is not SVG at
 * all. A `null` is rendered as text, not as nothing, so a broken drawing is
 * visible rather than silently missing.
 */
export function sanitizeSvg(source: string): string | null {
  try {
    const parsed = new DOMParser().parseFromString(source, 'image/svg+xml');
    if (parsed.querySelector('parsererror')) return null;
    const root = parsed.documentElement;
    if (!root) return null;
    const clean = sanitizeTree(toNode(root));
    return clean === null ? null : serialise(clean);
  } catch {
    return null;
  }
}
