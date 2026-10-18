import assert from 'node:assert/strict';
import { AppUpdate } from '../ui/src/update.ts';

let calls = 0, progress, finish;
const phases = [];
const app = new AppUpdate('0.1.0', false, callback => {
  calls++; progress = callback;
  return new Promise(resolve => { finish = resolve; });
}, () => phases.push(app.phase));
const pending = app.check();
await app.check();
assert.equal(calls, 1, 'Repeated clicks cannot launch a second update');
assert.equal(app.busy, true);
progress();
assert.equal(app.phase, 'installing');
finish({ status: 'installed', version: '0.1.1' });
await pending;
assert.deepEqual(phases, ['checking', 'installing', 'installed']);
assert.equal(app.version, '0.1.1');
assert.equal(app.busy, false);

const latest = new AppUpdate('0.1.0', false, async () => ({ status: 'latest', version: '0.1.0' }), () => {});
await latest.check();
assert.equal(latest.label, '최신 버전');

let failed = true;
const retry = new AppUpdate('0.1.0', false, async callback => {
  callback();
  if (failed) throw new Error('Install failed');
  return { status: 'installed', version: '0.1.1' };
}, () => {});
await retry.check();
assert.equal(retry.phase, 'error');
assert.equal(retry.version, '0.1.0', 'Failed installs must not change the displayed version');
assert.equal(retry.busy, false);
failed = false; await retry.check();
assert.equal(retry.phase, 'installed');
assert.equal(retry.error, '');
assert.equal(new AppUpdate('0.1.1', true, async () => {}, () => {}).label, '업데이트 완료');
console.log('App update: progress, duplicate click guard, latest, failed installation, retry, and post-restart completion passed.');
