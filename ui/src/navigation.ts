import { t } from './i18n';
import { duration } from './motion';

// Shared 16px viewport, 1.6px stroke and 9⅓px glyph height in both states.
export function navigationPath(progress: number) {
  const phase = (value: number) => { const t = Math.max(0, Math.min(1, value)); return t * t * (3 - 2 * t); };
  // Turn the cross, then fold its two rear arms onto the front arms.
  // All four arms share one hinge throughout, rather than detached vertices
  // sliding across one another. Reverse playback unfolds the same geometry.
  const turn = Math.PI / 4 * phase(progress / .55);
  const fold = Math.PI / 2 * phase((progress - .2) / .8);
  const tip = (angle: number) => [Math.cos(angle), Math.sin(angle)];
  const points = [tip(-Math.PI / 2 + turn), [0, 0], tip(Math.PI + turn + fold), tip(turn), [0, 0], tip(Math.PI / 2 + turn - fold)];
  const xs = points.map(point => point[0]), ys = points.map(point => point[1]);
  const centerX = (Math.min(...xs) + Math.max(...xs)) / 2;
  const centerY = (Math.min(...ys) + Math.max(...ys)) / 2;
  const radius = (28 / 3) / (Math.max(...ys) - Math.min(...ys));
  return points.map(([x, y], i) => `${i % 3 === 0 ? 'M' : 'L'}${8 + (x - centerX) * radius} ${8 + (y - centerY) * radius}`).join(' ');
}

// transitions.dev icon timing: ease-in-out, so the fold remains visible both ways.
function easeInOut(time: number) {
  let low = 0, high = 1;
  for (let i = 0; i < 16; i++) {
    const t = (low + high) / 2;
    const x = 1.26 * (1 - t) ** 2 * t + 1.74 * (1 - t) * t ** 2 + t ** 3;
    if (x < time) low = t; else high = t;
  }
  const t = (low + high) / 2;
  return 3 * (1 - t) * t ** 2 + t ** 3;
}

export class Navigation {
  private progress = 0;
  private frame = 0;
  private mode: 'add' | 'back' | null = null;
  private path: SVGPathElement;

  constructor(private button: HTMLButtonElement) {
    button.innerHTML = `<svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true" focusable="false"><path d="${navigationPath(0)}" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/></svg>`;
    this.path = button.querySelector('path')!;
  }

  setMode(mode: 'add' | 'back' | null) {
    if (mode === this.mode) return;
    cancelAnimationFrame(this.frame);
    const wasVisible = this.mode !== null;
    this.mode = mode;
    this.button.hidden = mode === null;
    this.button.dataset.mode = mode ?? '';
    this.button.setAttribute('aria-label', mode === 'add' ? t("기기 추가") : t("뒤로"));
    if (!mode) return;
    const target = mode === 'back' ? 1 : 0;
    const from = this.progress;
    const ms = duration('--navigation-morph-dur', 350, this.button) * Math.abs(target - from);
    if (!wasVisible || !ms) {
      this.progress = target; this.path.setAttribute('d', navigationPath(target)); return;
    }
    const start = performance.now();
    const tick = (now: number) => {
      const time = Math.min(1, (now - start) / ms);
      this.progress = from + (target - from) * easeInOut(time);
      this.path.setAttribute('d', navigationPath(this.progress));
      if (time < 1) this.frame = requestAnimationFrame(tick);
      else { this.progress = target; this.path.setAttribute('d', navigationPath(target)); }
    };
    this.frame = requestAnimationFrame(tick);
  }
}
