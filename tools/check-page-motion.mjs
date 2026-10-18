import assert from 'node:assert/strict';
import { pageTransitionMode, transitionPages } from '../ui/src/motion.ts';

let reducedMotion = false;
globalThis.document = { documentElement: {} };
globalThis.matchMedia = () => ({ matches: reducedMotion });
const tokens = { '--page-navigation-dur': '350ms', '--page-navigation-ease': 'cubic-bezier(.32,.72,0,1)', '--page-replace-dur': '250ms', '--duration-medium': '350ms', '--duration-stagger': '40ms', '--ease-smooth-out': 'cubic-bezier(.22,1,.36,1)' };
globalThis.getComputedStyle = () => ({ getPropertyValue: key => tokens[key] ?? '' });
class Motion {
  constructor(frames, options) { this.frames = frames; this.options = options; }
  get finished() { return this.promise ??= new Promise((resolve, reject) => { this.resolve = resolve; this.reject = reject; }); }
  finish() { this.resolve?.(); }
  cancel() { this.cancelled = true; this.reject?.(new Error('cancelled')); }
}
class Page {
  constructor(root) { this.parentElement = root; this.style = {}; this.inert = false; this.animations = []; this.pieces = []; }
  animate(frames, options) { const motion = new Motion(frames, options); this.animations.push(motion); return motion; }
  querySelectorAll() { return this.pieces; }
}
const pages = () => { const root = {}; return [new Page(root), new Page(root)]; };
assert.equal(pageTransitionMode('home', 'choose', false), 'push');
assert.equal(pageTransitionMode('choose', 'manual', false), 'push');
assert.equal(pageTransitionMode('home', 'edit-tv-0', false), 'push');
assert.equal(pageTransitionMode('manual', 'connection', false), 'push');
assert.equal(pageTransitionMode('choose', 'connection', false), 'push');
assert.equal(pageTransitionMode('connection', 'home', false), 'complete');
assert.equal(pageTransitionMode('connection', 'home', true), 'pop');

for (const mode of ['push', 'pop']) {
  const [previous, next] = pages();
  transitionPages(previous, next, mode, false);
  assert.equal(previous.animations[0].frames[1].transform, mode === 'push' ? 'translateX(-30%)' : 'translateX(100%)');
  assert.equal(next.animations[0].frames[0].transform, mode === 'push' ? 'translateX(100%)' : 'translateX(-30%)');
  assert.equal(next.style.zIndex, mode === 'push' ? '2' : '1');
}
{
  const [previous, next] = pages();
  transitionPages(previous, next, 'replace', false);
  for (const node of [previous, next]) for (const frame of node.animations[0].frames) assert.equal(frame.transform, undefined);
}
{
  const [previous, home] = pages();
  home.pieces = [new Page(), new Page()];
  transitionPages(previous, home, 'complete', false);
  assert.equal(home.inert, true);
  assert.equal(home.pieces[0].animations.length, 0);
  previous.animations[0].finish();
  await Promise.resolve();
  assert.equal(home.inert, false);
  assert.equal(home.animations[0].cancelled, true);
  assert.equal(home.pieces[0].animations.length, 1);
  assert.equal(home.pieces[1].animations[0].options.delay, 40);
}
{
  const [previous, home] = pages();
  home.pieces = [new Page()];
  transitionPages(previous, home, 'complete', false);
  const detail = new Page(home.parentElement);
  home.inert = true;
  transitionPages(home, detail, 'push', false);
  await Promise.resolve();
  assert.equal(home.pieces[0].animations.length, 0);
  assert.equal(home.inert, true);
}
for (const initial of [true, false]) {
  reducedMotion = !initial;
  const [previous, next] = pages();
  transitionPages(previous, next, 'complete', initial);
  assert.equal(previous.animations.length + next.animations.length, 0);
}
console.log('Page routing, iOS-style push/pop, sequential completion→home, cancellation, and reduced motion passed.');
