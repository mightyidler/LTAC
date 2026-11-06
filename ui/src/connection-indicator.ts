import { indicatorShape, completionPose } from './connection-motion';
import type { ConnectionRenderer } from './connection-renderer';
import { cssTimeMs } from './motion';
import { icons } from './icons';

// Curve and completion choreography adapted from psyonline's Her / OS1 loader:
// https://codepen.io/psyonline/pen/yayYWg (CodePen, MIT).
// No pointer handlers: the connection result drives the original hold gesture.
export class ConnectionIndicator {
  private renderer: ConnectionRenderer | null = null;
  private destroyed = false;
  private frame = 0;
  private previous = 0;
  private outcome: 'loading' | 'success' | 'failure' = 'loading';
  private active = false;
  private settled = false;
  private reduced = matchMedia('(prefers-reduced-motion: reduce)');
  private resolve: (() => void) | null = null;
  private completionTimer = 0;
  private completionStart = 0;
  private completionRotation = 0;
  private completionDuration = 4000;
  private idleSpeed = 4.2;
  private rotation = 0;
  private pose = completionPose(0, this.completionDuration, this.idleSpeed);
  private onRing: (() => void) | null = null;
  private onTheme = () => {
    if (!this.renderer) return;
    const style = getComputedStyle(this.host);
    this.renderer.setColors(style.getPropertyValue('--indicator-color').trim(), style.getPropertyValue('--indicator-error').trim());
    this.renderer.draw(this.pose, this.rotation, this.outcome === 'failure');
  };
  private onPreference = () => { if (this.active) { cancelAnimationFrame(this.frame); this.previous = 0; this.frame = requestAnimationFrame(this.animate); } };

  constructor(private host: HTMLElement) {
    this.host.innerHTML = `<span class="indicator-fallback" aria-hidden="true">∞</span><svg class="indicator-ring" viewBox="0 0 40 40" fill="none" aria-hidden="true"><circle cx="20" cy="20" r="${indicatorShape.radius}" stroke="currentColor" stroke-width="${indicatorShape.stroke}"/></svg><span class="indicator-result t-success-check" data-state="out" aria-hidden="true">${icons.check}</span>`;
    const path = this.host.querySelector('path')!;
    const length = Math.ceil(path.getTotalLength()) + 1;
    path.style.strokeDasharray = String(length);
    path.style.strokeDashoffset = String(-length);
    this.reduced.addEventListener('change', this.onPreference);
    window.addEventListener('ltac-theme-change', this.onTheme);
    void this.loadRenderer();
  }

  private async loadRenderer() {
    try {
      const { ConnectionRenderer } = await import('./connection-renderer');
      if (this.destroyed) return;
      this.renderer = new ConnectionRenderer();
      this.onTheme();
      this.host.prepend(this.renderer.canvas);
      this.host.classList.add('has-webgl');
    } catch {
      // The SVG and CSS indicator handles the full flow while WebGL is unavailable.
      this.renderer?.dispose();
      this.renderer = null;
    }
  }

  start() {
    this.stop();
    this.active = true;
    this.outcome = 'loading';
    this.previous = 0;
    this.settled = false;
    this.rotation = 0;
    this.idleSpeed = parseFloat(getComputedStyle(this.host).getPropertyValue('--indicator-idle-speed')) || 4.2;
    this.pose = completionPose(0, this.completionDuration, this.idleSpeed);
    this.onTheme();
    this.host.dataset.state = 'loading';
    this.host.querySelector<HTMLElement>('.indicator-result')!.dataset.state = 'out';
    this.frame = requestAnimationFrame(this.animate);
  }

  finish(outcome: 'success' | 'failure', onRing?: () => void) {
    if (!this.active) this.start();
    this.outcome = outcome;
    this.onRing = onRing ?? null;
    this.completionStart = performance.now();
    this.completionRotation = this.rotation;
    this.completionDuration = cssTimeMs(getComputedStyle(this.host).getPropertyValue('--indicator-complete-dur'), 4000);
    this.host.dataset.state = 'completing';
    if (this.reduced.matches) this.frame = requestAnimationFrame(this.animate);
    return new Promise<void>(resolve => { this.resolve = resolve; });
  }

  showResult(outcome: 'success' | 'failure') {
    this.start(); this.outcome = outcome; this.completionStart = performance.now() - this.completionDuration;
    this.completionRotation = this.rotation;
    cancelAnimationFrame(this.frame); this.animate(performance.now());
  }

  private animate = (time: number) => {
    if (!this.active) return;
    const delta = this.previous ? Math.min((time - this.previous) / (1000 / 60), 3) : 1;
    this.previous = time;
    const completing = this.outcome !== 'loading';
    const pose = completionPose(completing ? this.reduced.matches ? this.completionDuration : time - this.completionStart : 0, this.completionDuration, this.idleSpeed);
    this.pose = pose;
    if (completing) this.rotation = this.completionRotation + pose.turn;
    else if (!this.reduced.matches) this.rotation += this.idleSpeed / 60 * delta;
    this.renderer?.draw(pose, this.rotation, this.outcome === 'failure');
    if (completing && pose.done && !this.settled) {
      this.settled = true;
      this.host.dataset.state = this.outcome;
      this.onRing?.(); this.onRing = null;
      const result = this.host.querySelector<HTMLElement>('.indicator-result')!;
      result.innerHTML = this.outcome === 'success' ? icons.check : `<svg width="24" height="24" viewBox="0 0 24 24" fill="none">${['M7 7l10 10', 'M17 7L7 17'].map(d => `<path d="${d}" stroke="currentColor" stroke-width="${indicatorShape.stroke}" stroke-linecap="round" stroke-linejoin="round"/>`).join('')}</svg>`;
      const style = getComputedStyle(this.host);
      const delay = cssTimeMs(style.getPropertyValue('--check-path-delay'), 0);
      const stagger = cssTimeMs(style.getPropertyValue('--indicator-cross-stagger'), 0);
      result.querySelectorAll('path').forEach((path, index) => {
        const length = Math.ceil(path.getTotalLength()) + 1;
        path.style.strokeDasharray = String(length);
        path.style.strokeDashoffset = String(this.outcome === 'success' ? -length : length);
        path.style.setProperty('--check-path-delay', `${delay + index * stagger}ms`);
      });
      void result.offsetWidth;
      result.dataset.state = 'in';
      const duration = this.reduced.matches ? 0 : cssTimeMs(style.getPropertyValue('--check-path-dur'), 500) + delay + (result.querySelectorAll('path').length - 1) * stagger;
      this.completionTimer = window.setTimeout(() => { this.resolve?.(); this.resolve = null; }, duration || 0);
      return;
    }
    if (!this.reduced.matches || (completing && !this.settled)) this.frame = requestAnimationFrame(this.animate);
  };

  stop() { this.active = false; cancelAnimationFrame(this.frame); clearTimeout(this.completionTimer); this.onRing = null; this.resolve?.(); this.resolve = null; }
  destroy() {
    this.destroyed = true;
    window.removeEventListener('ltac-theme-change', this.onTheme);
    this.stop(); this.reduced.removeEventListener('change', this.onPreference);
    this.renderer?.dispose();
    this.renderer = null;
  }
}
