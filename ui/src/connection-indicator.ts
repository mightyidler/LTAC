import * as THREE from 'three';
import { palette } from './palette';
import { RibbonCurve, indicatorShape, ringRadii, completionPose } from './connection-motion';
import { cssTimeMs } from './motion';

const white = new THREE.Color(0xffffff);
const failureRed = new THREE.Color().setRGB(palette.red[0] / 255, palette.red[1] / 255, palette.red[2] / 255, THREE.SRGBColorSpace);

// Curve and completion choreography adapted from psyonline's Her / OS1 loader:
// https://codepen.io/psyonline/pen/yayYWg (CodePen, MIT).
// No pointer handlers: the connection result drives the original hold gesture.
export class ConnectionIndicator {
  private renderer: THREE.WebGLRenderer | null = null;
  private scene = new THREE.Scene();
  private camera = new THREE.PerspectiveCamera(indicatorShape.fov, indicatorShape.width / indicatorShape.height, 1, 1000);
  private group = new THREE.Group();
  private ribbon = new THREE.Mesh(new THREE.TubeGeometry(new RibbonCurve(), 200, 1.1, 2, true), new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true }));
  // Match the projected triangular ribbon's edge, instead of the thick original ring.
  private ring = new THREE.Mesh(new THREE.RingGeometry(ringRadii.inner, ringRadii.outer, 192), new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0, side: THREE.DoubleSide }));
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
  private ribbonCenterDepth = { value: 48 };
  private onRing: (() => void) | null = null;
  private onPreference = () => { if (this.active && !this.settled) { cancelAnimationFrame(this.frame); this.previous = 0; this.frame = requestAnimationFrame(this.animate); } };

  constructor(private host: HTMLElement) {
    this.host.innerHTML = `<span class="indicator-fallback" aria-hidden="true">∞</span><svg class="indicator-ring" viewBox="0 0 40 40" fill="none" aria-hidden="true"><circle cx="20" cy="20" r="15.84" stroke="currentColor" stroke-width="1.8"/></svg><span class="indicator-result t-success-check" data-state="out" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none"><path d="M5 12l4.5 4.5L19 7" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg></span>`;
    const path = this.host.querySelector('path')!;
    const length = Math.ceil(path.getTotalLength()) + 1;
    path.style.strokeDasharray = String(length);
    path.style.strokeDashoffset = String(length);
    this.camera.position.z = indicatorShape.cameraZ;
    this.group.add(this.ribbon);
    // White pigment with depth-dependent alpha, preserving the ribbon's volume.
    // The center follows the camera fit so completion keeps the same shading.
    this.ribbon.material.onBeforeCompile = shader => {
      shader.uniforms.ribbonCenterDepth = this.ribbonCenterDepth;
      shader.vertexShader = `uniform float ribbonCenterDepth;\nvarying float ribbonDepth;\n${shader.vertexShader}`
        .replace('#include <project_vertex>', '#include <project_vertex>\nribbonDepth = mvPosition.z + ribbonCenterDepth;');
      shader.fragmentShader = `varying float ribbonDepth;\n${shader.fragmentShader}`
        .replace('#include <alphamap_fragment>', '#include <alphamap_fragment>\ndiffuseColor.a *= 0.16 + 0.84 * smoothstep(-6.0, 6.0, ribbonDepth);');
    };
    this.ribbon.material.customProgramCacheKey = () => 'ltac-ribbon-depth-alpha';
    this.ring.position.x = indicatorShape.ringOffset;
    this.ring.rotation.y = Math.PI / 2;
    this.group.add(this.ring);
    this.scene.add(this.group);
    try {
      this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
      this.renderer.outputColorSpace = THREE.SRGBColorSpace;
      this.renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
      this.renderer.setSize(140, 48);
      this.renderer.setClearColor(0x000000, 0);
      host.prepend(this.renderer.domElement);
      host.classList.add('has-webgl');
    } catch { /* Keep a transparent, animated fallback on machines without WebGL. */ }
    this.reduced.addEventListener('change', this.onPreference);
  }

  start() {
    this.stop();
    this.active = true;
    this.outcome = 'loading';
    this.previous = 0;
    this.settled = false;
    this.group.rotation.set(0, 0, 0);
    this.group.position.set(0, 0, 0);
    this.ribbon.material.opacity = 1;
    this.ring.material.opacity = 0;
    this.ring.material.color.set(0xffffff);
    this.host.dataset.state = 'loading';
    this.idleSpeed = parseFloat(getComputedStyle(this.host).getPropertyValue('--indicator-idle-speed')) || 4.2;
    this.host.querySelector<HTMLElement>('.indicator-result')!.dataset.state = 'out';
    this.frame = requestAnimationFrame(this.animate);
  }

  finish(outcome: 'success' | 'failure', onRing?: () => void) {
    if (!this.active) this.start();
    this.outcome = outcome;
    this.onRing = onRing ?? null;
    this.completionStart = performance.now();
    this.completionRotation = this.ribbon.rotation.x;
    this.completionDuration = cssTimeMs(getComputedStyle(this.host).getPropertyValue('--indicator-complete-dur'), 4000);
    this.host.dataset.state = 'completing';
    if (this.reduced.matches) this.frame = requestAnimationFrame(this.animate);
    return new Promise<void>(resolve => { this.resolve = resolve; });
  }

  showResult(outcome: 'success' | 'failure') {
    this.start(); this.outcome = outcome; this.completionStart = performance.now() - this.completionDuration;
    this.completionRotation = this.ribbon.rotation.x;
    cancelAnimationFrame(this.frame); this.animate(performance.now());
  }

  private animate = (time: number) => {
    if (!this.active) return;
    const delta = this.previous ? Math.min((time - this.previous) / (1000 / 60), 3) : 1;
    this.previous = time;
    const completing = this.outcome !== 'loading';
    const pose = completionPose(completing ? this.reduced.matches ? this.completionDuration : time - this.completionStart : 0, this.completionDuration, this.idleSpeed);
    if (completing) this.ribbon.rotation.x = this.completionRotation + pose.turn;
    else if (!this.reduced.matches) this.ribbon.rotation.x += this.idleSpeed / 60 * delta;
    const progress = pose.progress;
    this.group.rotation.y = -Math.PI / 2 * progress;
    // Camera fit: keep the complete animation (including the final circle) in 140×48.
    this.group.position.z = -indicatorShape.retreat * progress;
    this.ribbonCenterDepth.value = indicatorShape.cameraZ + indicatorShape.retreat * progress;
    this.group.position.x = 0;
    this.ribbon.material.opacity = pose.ribbonOpacity;
    this.ring.material.opacity = pose.ringOpacity;
    if (this.outcome === 'failure') this.ring.material.color.copy(white).lerp(failureRed, pose.ringColor);
    this.renderer?.render(this.scene, this.camera);
    if (completing && pose.done && !this.settled) {
      this.settled = true;
      this.host.dataset.state = this.outcome;
      this.onRing?.(); this.onRing = null;
      const result = this.host.querySelector<HTMLElement>('.indicator-result')!;
      const paths = this.outcome === 'success' ? ['M5 12l4.5 4.5L19 7'] : ['M7 7l10 10', 'M17 7L7 17'];
      result.querySelector('svg')!.innerHTML = paths.map(d => `<path d="${d}" stroke="currentColor" stroke-width="${indicatorShape.stroke}" stroke-linecap="round" stroke-linejoin="round"/>`).join('');
      const style = getComputedStyle(this.host);
      const delay = cssTimeMs(style.getPropertyValue('--check-path-delay'), 0);
      const stagger = cssTimeMs(style.getPropertyValue('--indicator-cross-stagger'), 0);
      result.querySelectorAll('path').forEach((path, index) => {
        const length = Math.ceil(path.getTotalLength()) + 1;
        path.style.strokeDasharray = String(length);
        path.style.strokeDashoffset = String(length);
        path.style.setProperty('--check-path-delay', `${delay + index * stagger}ms`);
      });
      void result.offsetWidth;
      result.dataset.state = 'in';
      const duration = this.reduced.matches ? 0 : cssTimeMs(style.getPropertyValue('--check-path-dur'), 500) + delay + (paths.length - 1) * stagger;
      this.completionTimer = window.setTimeout(() => { this.resolve?.(); this.resolve = null; }, duration || 0);
      return;
    }
    if (!this.reduced.matches || this.outcome !== 'loading') this.frame = requestAnimationFrame(this.animate);
  };

  stop() { this.active = false; cancelAnimationFrame(this.frame); clearTimeout(this.completionTimer); this.onRing = null; this.resolve?.(); this.resolve = null; }
  destroy() {
    this.stop(); this.reduced.removeEventListener('change', this.onPreference);
    this.renderer?.dispose(); this.ribbon.geometry.dispose(); this.ribbon.material.dispose(); this.ring.geometry.dispose(); this.ring.material.dispose();
  }
}
