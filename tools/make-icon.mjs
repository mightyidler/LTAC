import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import sharp from 'sharp';

// The supplied PNG is the canonical, byte-for-byte original. Never regenerate
// it from a smaller icon. Optional argument imports a replacement original.
const source = resolve('resources/icons/app/icon.png');
mkdirSync('resources/icons/app', { recursive: true });
if (process.argv[2] && resolve(process.argv[2]) !== source) copyFileSync(process.argv[2], source);
const original = readFileSync(source);
const metadata = await sharp(original).metadata();
if (metadata.format !== 'png' || metadata.width !== metadata.height || metadata.width < 256) {
  throw new Error('The original must be a square PNG of at least 256 pixels.');
}
const resize = size => sharp(original)
  .ensureAlpha()
  .resize(size, size, { kernel: sharp.kernel.lanczos3, fastShrinkOnLoad: false })
  .png({ compressionLevel: 9, palette: false })
  .toBuffer();

// Native entries cover small/large Windows icons at 100–400% display scaling.
// Every entry is sampled directly from the original and compressed losslessly.
const sizes = [16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 128, 256];
const frames = await Promise.all(sizes.map(resize));
const directory = Buffer.alloc(6 + sizes.length * 16);
directory.writeUInt16LE(1, 2);
directory.writeUInt16LE(sizes.length, 4);
let offset = directory.length;
frames.forEach((data, index) => {
  const entry = 6 + index * 16;
  directory[entry] = sizes[index] === 256 ? 0 : sizes[index];
  directory[entry + 1] = directory[entry];
  directory.writeUInt16LE(1, entry + 4);
  directory.writeUInt16LE(32, entry + 6);
  directory.writeUInt32LE(data.length, entry + 8);
  directory.writeUInt32LE(offset, entry + 12);
  offset += data.length;
});
const ico = Buffer.concat([directory, ...frames]);
writeFileSync('resources/icons/app/icon.ico', ico);
mkdirSync('resources/public/icons/app', { recursive: true });
writeFileSync('resources/public/icons/app/favicon.ico', ico);
writeFileSync('resources/public/icons/app/app-icon.png', await resize(512));
console.log(`Original ${metadata.width}×${metadata.height} preserved; ${sizes.length} RGBA PNG icon entries generated from the original.`);
