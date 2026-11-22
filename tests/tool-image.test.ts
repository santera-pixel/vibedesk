/**
 * Unit tests for images that arrive inside a tool result.
 *
 * The chat showed a `read_file` on a PNG as a block of JSON. The bytes were
 * never in the result — the reader hands them to the model and the client only
 * a notice naming the path — so the card had nothing to render and printed what
 * it had. These cases pin down which results are images, because the failure
 * mode is silent: a notice that is not recognised looks exactly like a tool
 * that returned text.
 *
 * Run with `npm run test:unit`.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  imageFromBlock,
  imageFromNotice,
  imageFromText,
  toolImages,
} from '../src/lib/tool-image.ts';

const NOTICE = {
  path: 'C:\\shots\\frame.png',
  content: 'Image file (image/png, 186091 bytes).',
  file_size_bytes: 186091,
  returned_bytes: 0,
  was_truncated: false,
};

test('a read_file notice on an image is recognised', () => {
  const image = imageFromNotice(NOTICE);
  assert.ok(image);
  assert.equal(image.path, 'C:\\shots\\frame.png');
  assert.equal(image.mime, 'image/png');
  assert.equal(image.bytes, 186091);
});

test('a notice without a size still names the image', () => {
  const image = imageFromNotice({ path: '/tmp/a.webp', content: 'Image file (image/webp).' });
  assert.ok(image);
  assert.equal(image.mime, 'image/webp');
  assert.equal(image.bytes, undefined);
});

test('a notice that is not an image is left alone', () => {
  assert.equal(
    imageFromNotice({ path: '/tmp/a.bin', content: 'Image file (application/octet-stream).' }),
    null,
  );
});

test('a text result is not mistaken for an image', () => {
  assert.equal(imageFromNotice({ path: '/tmp/a.txt', content: 'hello' }), null);
  assert.equal(imageFromNotice({ content: 'Image file (image/png).' }), null, 'no path');
  assert.equal(imageFromNotice(null), null);
  assert.equal(imageFromNotice('Image file (image/png).'), null);
});

test('a stringified notice is recognised', () => {
  const image = imageFromText(JSON.stringify(NOTICE));
  assert.ok(image);
  assert.equal(image.mime, 'image/png');
});

test('a text block that is not JSON is not swallowed', () => {
  assert.equal(imageFromText('the build failed'), null);
  assert.equal(imageFromText('{ not json'), null);
  assert.equal(imageFromText('{"path":"/a","content":"plain text"}'), null);
});

test('an image content block becomes a data URL', () => {
  const image = imageFromBlock({ type: 'image', data: 'AAAA', mimeType: 'image/jpeg' });
  assert.ok(image);
  assert.equal(image.dataUrl, 'data:image/jpeg;base64,AAAA');
  assert.equal(image.mime, 'image/jpeg');
});

test('a data URL that is already whole is not wrapped twice', () => {
  const image = imageFromBlock({
    type: 'image',
    data: 'data:image/png;base64,AAAA',
    mimeType: 'image/png',
  });
  assert.ok(image);
  assert.equal(image.dataUrl, 'data:image/png;base64,AAAA');
});

test('a text block is not an image block', () => {
  assert.equal(imageFromBlock({ type: 'text', text: 'hi' }), null);
  assert.equal(imageFromBlock({ type: 'image', data: '' }), null);
});

test('a call reports its images once, in order', () => {
  const images = toolImages({
    content: [
      { type: 'content', content: { type: 'image', data: 'AAAA', mimeType: 'image/png' } },
      { type: 'content', content: { type: 'text', text: JSON.stringify(NOTICE) } },
    ],
    rawOutput: NOTICE,
  });
  assert.equal(images.length, 2, 'the same file twice is one image');
  assert.equal(images[0]?.dataUrl, 'data:image/png;base64,AAAA');
  assert.equal(images[1]?.path, 'C:\\shots\\frame.png');
});

test('a call with no images reports none', () => {
  assert.deepEqual(toolImages({ content: [], rawOutput: { ok: true } }), []);
  assert.deepEqual(toolImages({}), []);
});
