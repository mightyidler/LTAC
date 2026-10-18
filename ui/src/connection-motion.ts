import * as THREE from 'three';

export const indicatorShape = {
  width: 140, height: 48, cameraZ: 48, fov: 28,
  retreat: 15, ringOffset: 31.1, radius: 15.84, stroke: 1.8,
} as const;
const finalDepth = indicatorShape.cameraZ + indicatorShape.retreat - indicatorShape.ringOffset;
const pixelsPerUnit = indicatorShape.height / 2 / (finalDepth * Math.tan(indicatorShape.fov * Math.PI / 360));
export const ringRadii = {
  inner: (indicatorShape.radius - indicatorShape.stroke / 2) / pixelsPerUnit,
  outer: (indicatorShape.radius + indicatorShape.stroke / 2) / pixelsPerUnit,
};

export class RibbonCurve extends THREE.Curve<THREE.Vector3> {
  constructor() { super(); }
  getPoint(percent: number, target = new THREE.Vector3()) {
    const tau = Math.PI * 2;
    let t = (percent % .25) / .25;
    t = percent % .25 - (2 * (1 - t) * t * -.0185 + t * t * .25);
    if ([0, 2].includes(Math.floor(percent / .25))) t *= -1;
    return target.set(30 * Math.sin(tau * percent), 5.6 * Math.cos(tau * 3 * percent), 5.6 * Math.sin(tau * 2 * (percent - t)));
  }
}

const smooth = (low: number, high: number, value: number) => {
  const t = Math.max(0, Math.min(1, (value - low) / (high - low)));
  return t * t * (3 - 2 * t);
};

// The original four-second acceleration, integrated over elapsed time rather
// than accumulated frames. Its initial angular velocity matches the idle loop.
export function completionPose(elapsedMs: number, durationMs: number, idleSpeed: number) {
  const u = Math.max(0, Math.min(1, elapsedMs / durationMs));
  const acceleration = u < .5 ? 2 * u * u : -2 * u * u + 4 * u - 1;
  const integral = u < .5 ? 2 / 3 * u ** 3 : -2 / 3 * u ** 3 + 2 * u * u - u + 1 / 6;
  const seconds = u * durationMs / 1000;
  const blendSeconds = Math.min(.3, durationMs / 1000);
  const q = Math.min(1, seconds / blendSeconds);
  const entry = (idleSpeed - 2.1) * blendSeconds * (q - q ** 3 + .5 * q ** 4);
  const turn = 2.1 * seconds + entry + 60 * durationMs / 1000 * integral;
  const progress = Math.max(0, (acceleration - .35) / .65);
  const merge = Math.max(0, Math.min(1, (acceleration - .97) / .03));
  return {
    done: u === 1, turn, progress,
    // A small overlap keeps the outline from dimming between two alpha layers.
    ribbonOpacity: 1 - smooth(.15, 1, merge),
    ringOpacity: smooth(0, .85, merge),
    ringColor: smooth(0, 1, merge),
  };
}
