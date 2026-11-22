/**
 * Unit tests for the SVG sanitiser's policy.
 *
 * The policy is the part that has to be right: it decides whether a drawing the
 * model wrote can navigate the webview away from the app. It is a pure function
 * over a plain tree, so it is tested here without a browser. The DOM adapter
 * around it only parses and serialises.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { sanitizeTree, type SvgNode } from '../src/lib/sanitize-svg.ts';

function node(
  name: string,
  attributes: Record<string, string> = {},
  children: SvgNode[] = [],
  text?: string,
): SvgNode {
  return {
    name,
    attributes: Object.entries(attributes).map(([key, value]) => ({ name: key, value })),
    children,
    ...(text === undefined ? {} : { text }),
  };
}

const names = (tree: SvgNode | null): string[] =>
  tree === null ? [] : tree.children.map((child) => child.name);

const attributeNames = (tree: SvgNode | null): string[] =>
  tree === null ? [] : tree.attributes.map((attribute) => attribute.name);

test('a root that is not svg is refused', () => {
  assert.equal(sanitizeTree(node('html', {}, [node('path')])), null);
  assert.equal(sanitizeTree(node('div')), null);
});

test('a drawing keeps its shapes', () => {
  const clean = sanitizeTree(
    node('svg', { viewBox: '0 0 10 10' }, [node('path', { d: 'M0 0L10 10' }), node('circle', { r: '4' })]),
  );
  assert.deepEqual(names(clean), ['path', 'circle']);
  assert.deepEqual(attributeNames(clean), ['viewBox']);
});

test('a link is removed and its text is kept', () => {
  // The defect this exists for: clicking the link navigated the whole webview
  // to the URL, and there was no way back except restarting the app.
  const clean = sanitizeTree(
    node('svg', {}, [node('a', { href: 'https://example.com' }, [node('text', {}, [], 'click me')])]),
  );
  assert.deepEqual(names(clean), ['text']);
  assert.equal(clean?.children[0]?.text, 'click me');
});

test('a script is removed', () => {
  const clean = sanitizeTree(node('svg', {}, [node('script', {}, [], 'alert(1)')]));
  assert.deepEqual(names(clean), []);
});

test('a foreignObject is removed and its children are sanitised too', () => {
  // Unwrapping must not smuggle the child past the walk.
  const clean = sanitizeTree(
    node('svg', {}, [
      node('foreignObject', {}, [node('a', { href: 'https://example.com' }, [node('text', {}, [], 'x')])]),
    ]),
  );
  assert.deepEqual(names(clean), ['text']);
});

test('an event handler attribute is removed', () => {
  const clean = sanitizeTree(node('svg', {}, [node('rect', { onclick: 'alert(1)', width: '4' })]));
  assert.deepEqual(clean?.children[0]?.attributes.map((attribute) => attribute.name), ['width']);
});

test('a style attribute is removed', () => {
  const clean = sanitizeTree(
    node('svg', {}, [node('rect', { style: 'position:fixed;inset:0', fill: 'red' })]),
  );
  assert.deepEqual(clean?.children[0]?.attributes.map((attribute) => attribute.name), ['fill']);
});

test('an external href is removed but a fragment survives', () => {
  const external = sanitizeTree(node('svg', {}, [node('use', { href: 'https://example.com/x.svg#a' })]));
  assert.deepEqual(external?.children[0]?.attributes, []);

  const internal = sanitizeTree(node('svg', {}, [node('use', { href: '#arrow' })]));
  assert.deepEqual(internal?.children[0]?.attributes, [{ name: 'href', value: '#arrow' }]);
});

test('an image element is removed', () => {
  const clean = sanitizeTree(node('svg', {}, [node('image', { href: 'https://example.com/a.png' })]));
  assert.deepEqual(names(clean), []);
});

test('a style element is removed', () => {
  const clean = sanitizeTree(node('svg', {}, [node('style', {}, [], '@import url(https://x)')]));
  assert.deepEqual(names(clean), []);
});

test('nested groups keep their structure', () => {
  const clean = sanitizeTree(
    node('svg', {}, [node('g', { transform: 'translate(1,2)' }, [node('g', {}, [node('path', { d: 'M0 0' })])])]),
  );
  assert.deepEqual(names(clean), ['g']);
  assert.deepEqual(names(clean?.children[0] ?? null), ['g']);
  assert.deepEqual(names(clean?.children[0]?.children[0] ?? null), ['path']);
});

test('a gradient and its stops survive', () => {
  const clean = sanitizeTree(
    node('svg', {}, [
      node('defs', {}, [
        node('linearGradient', { id: 'g1' }, [node('stop', { offset: '0', 'stop-color': '#fff' })]),
      ]),
    ]),
  );
  assert.deepEqual(names(clean), ['defs']);
  assert.deepEqual(names(clean?.children[0] ?? null), ['linearGradient']);
  assert.deepEqual(names(clean?.children[0]?.children[0] ?? null), ['stop']);
});

test('text content is preserved', () => {
  const clean = sanitizeTree(node('svg', {}, [node('text', { x: '1' }, [], 'hello')]));
  assert.equal(clean?.children[0]?.text, 'hello');
});
