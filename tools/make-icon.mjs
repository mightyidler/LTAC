import { mkdirSync, writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

// Source geometry is intentionally simple: L + the restrained red brand dot.
const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="13" fill="#080808"/><path d="M17 14h8v32h18v8H17z" fill="#fff"/><circle cx="50" cy="50" r="6" fill="#f43d43"/></svg>`;
const crc = bytes => {
  let value = 0xffffffff;
  for (const byte of bytes) { value ^= byte; for (let b = 0; b < 8; b++) value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0); }
  return (value ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const body = Buffer.concat([Buffer.from(type), data]);
  const output = Buffer.alloc(data.length + 12);
  output.writeUInt32BE(data.length, 0); body.copy(output, 4); output.writeUInt32BE(crc(body), output.length - 4);
  return output;
};
function png(size) {
  const raw = Buffer.alloc(size * (1 + size * 4));
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const px = (x + .5) * 64 / size, py = (y + .5) * 64 / size;
    const dx = Math.max(13 - px, 0, px - 51), dy = Math.max(13 - py, 0, py - 51);
    let color = dx * dx + dy * dy > 169 ? [0, 0, 0, 0] : [8, 8, 8, 255];
    if ((px >= 17 && px <= 25 && py >= 14 && py <= 54) || (px >= 17 && px <= 43 && py >= 46 && py <= 54)) color = [255, 255, 255, 255];
    if ((px - 50) ** 2 + (py - 50) ** 2 <= 36) color = [244, 61, 67, 255];
    raw.set(color, y * (1 + size * 4) + 1 + x * 4);
  }
  const header = Buffer.alloc(13); header.writeUInt32BE(size); header.writeUInt32BE(size, 4); header[8] = 8; header[9] = 6;
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk('IHDR', header), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
const sizes = [16, 32, 48, 256];
const frames = sizes.map(png);
const directory = Buffer.alloc(6 + sizes.length * 16); directory.writeUInt16LE(1, 2); directory.writeUInt16LE(sizes.length, 4);
let offset = directory.length;
frames.forEach((data, index) => {
  const start = 6 + index * 16, size = sizes[index];
  directory[start] = size === 256 ? 0 : size; directory[start + 1] = directory[start];
  directory.writeUInt16LE(1, start + 4); directory.writeUInt16LE(32, start + 6);
  directory.writeUInt32LE(data.length, start + 8); directory.writeUInt32LE(offset, start + 12); offset += data.length;
});
mkdirSync('src-tauri/icons', { recursive: true });
writeFileSync('src-tauri/icons/icon.svg', svg);
writeFileSync('src-tauri/icons/icon.ico', Buffer.concat([directory, ...frames]));
writeFileSync('src-tauri/icons/icon.png', png(256));
