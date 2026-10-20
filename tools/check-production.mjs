import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';

const directory = new URL('../web-dist/assets/', import.meta.url);
const scripts = readdirSync(directory).filter(name => name.endsWith('.js'));
assert(scripts.length > 0, 'Build the app before inspecting production output');
const forbidden = ['OLED42C4ENA', 'A5:D3:31:F3:35:A8', 'preview-tools', 'LTAC · WEB DEV UI'];
for (const script of scripts) {
  const source = readFileSync(new URL(script, directory), 'utf8');
  for (const marker of forbidden) assert(!source.includes(marker), `Development data found in ${script}`);
}
console.log('Production output contains no preview controls or demo device data.');
