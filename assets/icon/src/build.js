// Rasterise the Vitrine icon SVGs and pack vitrine.ico.
//
//   frames 16/20/24/32  <- svg/vitrine-<size>.svg (pixel-fitted drawings)
//   frames 40 and up    <- vitrine.svg (master, 48-unit grid)
//
// Output (in assets/icon): png/vitrine-<size>.png, vitrine-1024.png, vitrine.ico
// PNGs are re-encoded with zlib level 9 and per-row adaptive filters (smaller than resvg's default).
'use strict';
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const { Resvg } = require('@resvg/resvg-js');

const ICON = path.resolve(__dirname, '..');
const SMALL = [16, 20, 24, 32];
const LARGE = [40, 48, 64, 96, 128, 256];
const ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256];

// ---------------------------------------------------------------- PNG decode / encode (8-bit RGBA)
function decodePng(buf) {
  let pos = 8, w = 0, h = 0;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos), type = buf.toString('ascii', pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      w = data.readUInt32BE(0); h = data.readUInt32BE(4);
      if (data[8] !== 8 || data[9] !== 6 || data[12] !== 0) throw new Error('expected 8-bit RGBA, non-interlaced');
    } else if (type === 'IDAT') idat.push(data);
    pos += 12 + len;
  }
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = w * 4, out = Buffer.alloc(stride * h);
  for (let y = 0; y < h; y++) {
    const ft = raw[y * (stride + 1)], src = y * (stride + 1) + 1, dst = y * stride;
    for (let x = 0; x < stride; x++) {
      const a = x >= 4 ? out[dst + x - 4] : 0, b = y ? out[dst - stride + x] : 0;
      const c = x >= 4 && y ? out[dst - stride + x - 4] : 0;
      let v = raw[src + x];
      if (ft === 1) v += a; else if (ft === 2) v += b; else if (ft === 3) v += (a + b) >> 1;
      else if (ft === 4) v += paeth(a, b, c);
      out[dst + x] = v & 255;
    }
  }
  return { w, h, px: out };
}

function paeth(a, b, c) {
  const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

const CRC = new Int32Array(256).map((_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320 ^ (c >>> 1) : c >>> 1; return c; });
function crc32(buf) { let c = -1; for (const b of buf) c = CRC[(c ^ b) & 255] ^ (c >>> 8); return (c ^ -1) >>> 0; }
function chunk(type, data) {
  const t = Buffer.from(type, 'ascii'), len = Buffer.alloc(4), crc = Buffer.alloc(4);
  len.writeUInt32BE(data.length); crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return Buffer.concat([len, t, data, crc]);
}

function encodePng({ w, h, px }) {
  const stride = w * 4, raw = Buffer.alloc((stride + 1) * h), row = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    let best = null, bestSum = Infinity, bestType = 0;
    for (let ft = 0; ft <= 4; ft++) {
      let sum = 0;
      for (let x = 0; x < stride; x++) {
        const i = y * stride + x, v = px[i];
        const a = x >= 4 ? px[i - 4] : 0, b = y ? px[i - stride] : 0, c = x >= 4 && y ? px[i - stride - 4] : 0;
        const p = ft === 0 ? 0 : ft === 1 ? a : ft === 2 ? b : ft === 3 ? (a + b) >> 1 : paeth(a, b, c);
        const d = (v - p) & 255;
        row[x] = d; sum += d < 128 ? d : 256 - d;
      }
      if (sum < bestSum) { bestSum = sum; bestType = ft; best = Buffer.from(row); }
    }
    raw[y * (stride + 1)] = bestType; best.copy(raw, y * (stride + 1) + 1);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9, memLevel: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

// ---------------------------------------------------------------- ICO (BMP frames, PNG for 256)
function bmpEntry({ w, h, px }) {
  const hdr = Buffer.alloc(40), maskStride = Math.ceil(w / 32) * 4;
  hdr.writeUInt32LE(40, 0); hdr.writeInt32LE(w, 4); hdr.writeInt32LE(h * 2, 8);
  hdr.writeUInt16LE(1, 12); hdr.writeUInt16LE(32, 14);
  hdr.writeUInt32LE(w * h * 4 + maskStride * h, 20);
  const xor = Buffer.alloc(w * h * 4), and = Buffer.alloc(maskStride * h);
  for (let y = 0; y < h; y++) {
    const sy = h - 1 - y;                                   // bottom-up rows
    for (let x = 0; x < w; x++) {
      const s = (sy * w + x) * 4, d = (y * w + x) * 4;
      xor[d] = px[s + 2]; xor[d + 1] = px[s + 1]; xor[d + 2] = px[s]; xor[d + 3] = px[s + 3];
      if (px[s + 3] === 0) and[y * maskStride + (x >> 3)] |= 0x80 >> (x & 7);
    }
  }
  return Buffer.concat([hdr, xor, and]);
}

function ico(frames) {
  const dir = Buffer.alloc(6 + 16 * frames.length);
  dir.writeUInt16LE(1, 2); dir.writeUInt16LE(frames.length, 4);
  let offset = dir.length;
  const blobs = frames.map((fr, i) => {
    const data = fr.w >= 256 ? fr.png : bmpEntry(fr);
    const e = 6 + 16 * i;
    dir[e] = fr.w >= 256 ? 0 : fr.w; dir[e + 1] = fr.h >= 256 ? 0 : fr.h;
    dir.writeUInt16LE(1, e + 4); dir.writeUInt16LE(32, e + 6);
    dir.writeUInt32LE(data.length, e + 8); dir.writeUInt32LE(offset, e + 12);
    offset += data.length;
    return data;
  });
  return Buffer.concat([dir, ...blobs]);
}

// ---------------------------------------------------------------- build
function render(file, size) {
  const svg = fs.readFileSync(file, 'utf8');
  const png = new Resvg(svg, { fitTo: { mode: 'width', value: size }, shapeRendering: 2 }).render().asPng();
  const img = decodePng(png);
  const opt = encodePng(img);
  return { ...img, png: opt.length < png.length ? opt : png };
}

const pngDir = path.join(ICON, 'png');
fs.mkdirSync(pngDir, { recursive: true });
const frames = {};
for (const s of SMALL) frames[s] = render(path.join(ICON, 'svg', `vitrine-${s}.svg`), s);
for (const s of LARGE) frames[s] = render(path.join(ICON, 'vitrine.svg'), s);
for (const [s, fr] of Object.entries(frames)) fs.writeFileSync(path.join(pngDir, `vitrine-${s}.png`), fr.png);
fs.writeFileSync(path.join(ICON, 'vitrine-1024.png'), render(path.join(ICON, 'vitrine.svg'), 1024).png);
fs.writeFileSync(path.join(ICON, 'vitrine.ico'), ico(ICO_SIZES.map(s => frames[s])));
console.log('wrote', Object.keys(frames).length, 'PNG frames, vitrine-1024.png and vitrine.ico', ICO_SIZES.join('/'));
