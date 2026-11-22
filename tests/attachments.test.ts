import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  baseName,
  dataUrlBytes,
  dataUrlMime,
  formatSize,
  isImageMime,
  rejectReason,
} from '../src/lib/attachments.ts';

test('isImageMime accepts the four formats the backend sniffs for', () => {
  for (const mime of ['image/png', 'image/jpeg', 'image/gif', 'image/webp']) {
    assert.equal(isImageMime(mime), true, mime);
  }
  assert.equal(isImageMime('IMAGE/PNG'), true);
  assert.equal(isImageMime('image/svg+xml'), false);
  assert.equal(isImageMime('text/plain'), false);
  assert.equal(isImageMime(''), false);
});

test('dataUrlMime reads the type out of the prefix', () => {
  assert.equal(dataUrlMime('data:image/png;base64,AAAA'), 'image/png');
  assert.equal(dataUrlMime('data:image/webp;base64,AAAA'), 'image/webp');
  assert.equal(dataUrlMime('not a data url'), '');
});

test('dataUrlBytes undoes the base64 expansion', () => {
  // "foo" -> "Zm9v" (4 chars, no padding) -> 3 bytes
  assert.equal(dataUrlBytes('data:image/png;base64,Zm9v'), 3);
  // "fo" -> "Zm8=" (1 pad) -> 2 bytes
  assert.equal(dataUrlBytes('data:image/png;base64,Zm8='), 2);
  // "f" -> "Zg==" (2 pads) -> 1 byte
  assert.equal(dataUrlBytes('data:image/png;base64,Zg=='), 1);
  assert.equal(dataUrlBytes('data:image/png;base64,'), 0);
  assert.equal(dataUrlBytes('no comma here'), 0);
});

test('formatSize switches units at the right boundaries', () => {
  assert.equal(formatSize(0), '0 B');
  assert.equal(formatSize(512), '512 B');
  assert.equal(formatSize(2048), '2.0 KB');
  assert.equal(formatSize(40 * 1024), '40 KB');
  assert.equal(formatSize(8 * 1024 * 1024), '8.0 MB');
});

test('rejectReason names the actual problem', () => {
  assert.equal(rejectReason({ type: 'image/png', size: 1024, name: 'a.png' }), null);
  assert.match(
    rejectReason({ type: 'image/svg+xml', size: 1024, name: 'logo.svg' }) ?? '',
    /logo\.svg is not a PNG, JPEG, GIF or WebP image/,
  );
  assert.match(
    rejectReason({ type: 'image/png', size: 9 * 1024 * 1024, name: 'huge.png' }) ?? '',
    /huge\.png is 9\.0 MB; the limit is 8 MB/,
  );
  assert.match(
    rejectReason({ type: 'image/png', size: 0, name: 'empty.png' }) ?? '',
    /empty\.png is empty/,
  );
});

test('baseName takes the last segment of either separator', () => {
  assert.equal(baseName('C:\\shots\\screen.png'), 'screen.png');
  assert.equal(baseName('/home/u/shot.png'), 'shot.png');
  assert.equal(baseName('shot.png'), 'shot.png');
  assert.equal(baseName(''), '');
});
