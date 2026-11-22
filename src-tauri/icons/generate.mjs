/**
 * Generates the VibeDesk app icons.
 *
 * The mark is drawn analytically (rounded square + chevron) and supersampled
 * 4x, so the output is crisp at every size without shipping a binary asset
 * pipeline. Run with `node src-tauri/icons/generate.mjs`.
 */
import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const OUT = dirname(fileURLToPath(import.meta.url));

const INK = [0x0b, 0x0c, 0x0b];
const ACCENT = [0x74, 0xc3, 0xa0];
const SS = 4;

function roundedRect(x, y, size, radius) {
  const r = radius;
  const cx = Math.min(Math.max(x, r), size - r);
  const cy = Math.min(Math.max(y, r), size - r);
  const dx = x - cx;
  const dy = y - cy;
  return dx * dx + dy * dy <= r * r;
}

function segment(x, y, ax, ay, bx, by, half) {
  const vx = bx - ax;
  const vy = by - ay;
  const wx = x - ax;
  const wy = y - ay;
  const len2 = vx * vx + vy * vy;
  const t = len2 === 0 ? 0 : Math.min(1, Math.max(0, (wx * vx + wy * vy) / len2));
  const px = ax + vx * t - x;
  const py = ay + vy * t - y;
  return px * px + py * py <= half * half;
}

function render(size) {
  const pixels = Buffer.alloc(size * size * 4);
  const radius = size * 0.225;
  const stroke = size * 0.052;
  const ax = size * 0.285;
  const ay = size * 0.315;
  const mx = size * 0.5;
  const my = size * 0.705;
  const bx = size * 0.715;
  const by = size * 0.315;

  for (let py = 0; py < size; py += 1) {
    for (let px = 0; px < size; px += 1) {
      let bgHits = 0;
      let markHits = 0;

      for (let sy = 0; sy < SS; sy += 1) {
        for (let sx = 0; sx < SS; sx += 1) {
          const x = px + (sx + 0.5) / SS;
          const y = py + (sy + 0.5) / SS;
          if (!roundedRect(x, y, size, radius)) continue;
          bgHits += 1;
          if (
            segment(x, y, ax, ay, mx, my, stroke) ||
            segment(x, y, mx, my, bx, by, stroke)
          ) {
            markHits += 1;
          }
        }
      }

      const total = SS * SS;
      const bg = bgHits / total;
      const mark = markHits / total;
      const offset = (py * size + px) * 4;

      if (bg === 0) {
        pixels[offset + 3] = 0;
        continue;
      }

      const mix = bg === 0 ? 0 : mark / bg;
      for (let channel = 0; channel < 3; channel += 1) {
        pixels[offset + channel] = Math.round(
          INK[channel] * (1 - mix) + ACCENT[channel] * mix,
        );
      }
      pixels[offset + 3] = Math.round(bg * 255);
    }
  }

  return pixels;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) {
    c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([length, body, crc]);
}

function png(size) {
  const pixels = render(size);
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y += 1) {
    raw[y * (size * 4 + 1)] = 0;
    pixels.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

function ico(sizes) {
  const images = sizes.map((size) => ({ size, data: png(size) }));
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);

  let offset = 6 + images.length * 16;
  const entries = [];
  for (const image of images) {
    const entry = Buffer.alloc(16);
    entry[0] = image.size >= 256 ? 0 : image.size;
    entry[1] = image.size >= 256 ? 0 : image.size;
    entry[2] = 0;
    entry[3] = 0;
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(image.data.length, 8);
    entry.writeUInt32LE(offset, 12);
    offset += image.data.length;
    entries.push(entry);
  }

  return Buffer.concat([header, ...entries, ...images.map((i) => i.data)]);
}

const targets = [
  ['32x32.png', png(32)],
  ['128x128.png', png(128)],
  ['128x128@2x.png', png(256)],
  ['icon.png', png(512)],
  ['icon.ico', ico([16, 32, 48, 64, 128, 256])],
];

for (const [name, data] of targets) {
  writeFileSync(join(OUT, name), data);
  process.stdout.write(`${name} ${data.length} bytes\n`);
}
