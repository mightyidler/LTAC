import * as THREE from 'three';
import { indicatorShape, type completionPose } from './connection-motion.ts';

const projection = indicatorShape.height / 2 / Math.tan(indicatorShape.fov * Math.PI / 360);
const finalDepth = indicatorShape.cameraZ + indicatorShape.retreat - indicatorShape.ringOffset;
export const ringRadii = {
  inner: (indicatorShape.radius - indicatorShape.stroke / 2) * finalDepth / projection,
  outer: (indicatorShape.radius + indicatorShape.stroke / 2) * finalDepth / projection,
};

// Move both edges by the same amount instead of scaling the entire band.
// This keeps its projected stroke at 1.8px while the circle grows.
export function updateRingGeometry(geometry: THREE.RingGeometry, radiusPixels: number, progress: number) {
  const depth = indicatorShape.cameraZ + indicatorShape.retreat * progress - indicatorShape.ringOffset * Math.sin(Math.PI / 2 * progress);
  const segments = geometry.parameters.thetaSegments;
  const positions = geometry.attributes.position;
  for (let edge = 0; edge < 2; edge++) {
    const radius = (radiusPixels + (edge === 0 ? -1 : 1) * indicatorShape.stroke / 2) * depth / projection;
    for (let i = 0; i <= segments; i++) {
      const angle = i / segments * Math.PI * 2;
      positions.setXYZ(edge * (segments + 1) + i, radius * Math.cos(angle), radius * Math.sin(angle), 0);
    }
  }
  positions.needsUpdate = true;
  geometry.boundingSphere = new THREE.Sphere(new THREE.Vector3(), (radiusPixels + indicatorShape.stroke / 2) * depth / projection);
}

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

export class ConnectionRenderer {
  private renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
  readonly canvas = this.renderer.domElement;
  private scene = new THREE.Scene();
  private camera = new THREE.PerspectiveCamera(indicatorShape.fov, indicatorShape.width / indicatorShape.height, 1, 1000);
  private group = new THREE.Group();
  private ribbon = new THREE.Mesh(new THREE.TubeGeometry(new RibbonCurve(), 200, 1.1, 2, true), new THREE.MeshBasicMaterial({ transparent: true }));
  // Write depth only to hide the rear turns while keeping the canvas transparent.
  private cover = new THREE.Mesh(new THREE.CircleGeometry(1, 192), new THREE.MeshBasicMaterial({ colorWrite: false, side: THREE.DoubleSide }));
  private ring = new THREE.Mesh(new THREE.RingGeometry(ringRadii.inner, ringRadii.outer, 192), new THREE.MeshBasicMaterial({ transparent: true, opacity: 0, depthWrite: false, side: THREE.DoubleSide }));
  private ribbonCenterDepth: { value: number } = { value: indicatorShape.cameraZ };
  private pigment = new THREE.Color();
  private failurePigment = new THREE.Color();

  constructor() {
    this.camera.position.z = indicatorShape.cameraZ;
    this.cover.position.x = indicatorShape.coverOffset;
    this.cover.rotation.y = Math.PI / 2;
    this.cover.renderOrder = -1;
    this.ring.position.x = indicatorShape.ringOffset;
    this.ring.rotation.y = Math.PI / 2;
    this.ring.renderOrder = -2;
    this.group.add(this.ribbon, this.cover, this.ring);
    // Keep the ribbon's depth shading through the final front view.
    this.ribbon.material.onBeforeCompile = shader => {
      shader.uniforms.ribbonCenterDepth = this.ribbonCenterDepth;
      shader.vertexShader = `uniform float ribbonCenterDepth;\nvarying float ribbonDepth;\n${shader.vertexShader}`
        .replace('#include <project_vertex>', '#include <project_vertex>\nribbonDepth = mvPosition.z + ribbonCenterDepth;');
      shader.fragmentShader = `varying float ribbonDepth;\n${shader.fragmentShader}`
        .replace('#include <alphamap_fragment>', '#include <alphamap_fragment>\ndiffuseColor.a *= 0.16 + 0.84 * smoothstep(-6.0, 6.0, ribbonDepth);');
    };
    this.ribbon.material.customProgramCacheKey = () => 'ltac-ribbon-depth-alpha';
    this.scene.add(this.group);
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    this.renderer.setSize(indicatorShape.width, indicatorShape.height);
    this.renderer.setClearAlpha(0);
  }

  setColors(pigment: string, failurePigment: string) {
    this.pigment.setStyle(pigment);
    this.failurePigment.setStyle(failurePigment);
    this.ribbon.material.color.copy(this.pigment);
    this.ring.material.color.copy(this.pigment);
  }

  draw(pose: ReturnType<typeof completionPose>, rotation: number, failed: boolean) {
    const progress = pose.progress;
    this.ribbon.rotation.x = rotation;
    this.group.rotation.y = -Math.PI / 2 * progress;
    this.group.position.z = -indicatorShape.retreat * progress;
    this.ribbonCenterDepth.value = indicatorShape.cameraZ + indicatorShape.retreat * progress;
    this.cover.visible = pose.cover > 0;
    const coverDepth = this.ribbonCenterDepth.value - indicatorShape.coverOffset * Math.sin(Math.PI / 2 * progress);
    const coverRadius = pose.coverRadius * coverDepth / projection;
    this.cover.scale.set(coverRadius, coverRadius, 1);
    this.ring.material.opacity = pose.ringOpacity;
    this.ring.visible = pose.ringOpacity > 0;
    if (this.ring.visible) updateRingGeometry(this.ring.geometry, pose.ringRadius, progress);
    if (failed) {
      this.ribbon.material.color.copy(this.pigment).lerp(this.failurePigment, pose.ringColor);
      this.ring.material.color.copy(this.pigment).lerp(this.failurePigment, pose.ringColor);
    }
    this.renderer.render(this.scene, this.camera);
  }

  dispose() {
    this.renderer.dispose();
    this.ribbon.geometry.dispose(); this.ribbon.material.dispose();
    this.cover.geometry.dispose(); this.cover.material.dispose();
    this.ring.geometry.dispose(); this.ring.material.dispose();
  }
}
