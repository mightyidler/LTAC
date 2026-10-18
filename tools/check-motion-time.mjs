import assert from 'node:assert/strict';
import { cssTimeMs, duration } from '../ui/src/motion.ts';

for (const [css, expected] of [['4000ms', 4000], ['4s', 4000], ['.5s', 500], ['0.08s', 80], ['900ms', 900], ['0ms', 0], ['0s', 0], ['0', 0], ['  .35s  ', 350]]) {
  assert.equal(cssTimeMs(css, 123), expected, css);
}
for (const invalid of ['', 'auto', '-4s', 'NaNms', '4s trailing', '4']) assert.equal(cssTimeMs(invalid, 123), 123);
globalThis.document = { documentElement: {} };
let reduced = false;
globalThis.matchMedia = () => ({matches: reduced});
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '.25s' });
assert.equal(duration('--page-replace-dur', 999), 250);
reduced = true;
assert.equal(duration('--page-replace-dur', 999), 0);
console.log('CSS motion times preserve ms/s units and explicit zero after production minification.');
