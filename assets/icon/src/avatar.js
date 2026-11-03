// GitHub organization avatar: the app icon centred on a solid tile.
// Usage: node avatar.js <out-dir>
const fs = require('fs');
const path = require('path');
const { Resvg } = require('@resvg/resvg-js');

const icon = fs.readFileSync(path.join(__dirname, '..', 'vitrine.svg'), 'utf8');
const inner = icon.replace(/^[\s\S]*?<svg[^>]*>/, '').replace(/<\/svg>\s*$/, '');
const out = process.argv[2] || '.';
const SIZE = 1024;
const ICON = 0.66 * SIZE; // generous padding: GitHub crops avatars to a rounded square
const off = (SIZE - ICON) / 2;

for (const [name, bg] of [['dark', '#161616'], ['light', '#f4f4f3']]) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="${SIZE}" height="${SIZE}" viewBox="0 0 ${SIZE} ${SIZE}">
  <rect width="${SIZE}" height="${SIZE}" fill="${bg}"/>
  <svg x="${off}" y="${off}" width="${ICON}" height="${ICON}" viewBox="0 0 48 48">${inner}</svg>
</svg>`;
  const png = new Resvg(svg, { fitTo: { mode: 'width', value: SIZE } }).render().asPng();
  const file = path.join(out, `vitrine-widgets-avatar-${name}.png`);
  fs.writeFileSync(file, png);
  fs.writeFileSync(file.replace(/\.png$/, '.svg'), svg);
  console.log(file, png.length);
}
