import assert from 'node:assert/strict';
import * as THREE from 'three';
import { RibbonCurve, indicatorShape, ringRadii, completionPose } from '../ui/src/connection-motion.ts';

// Numeric geometry check; does not launch a browser or interact with a TV.
const camera = new THREE.PerspectiveCamera(indicatorShape.fov, indicatorShape.width / indicatorShape.height, 1, 1000);
camera.position.z = indicatorShape.cameraZ; camera.updateMatrixWorld();
const geometry = new THREE.TubeGeometry(new RibbonCurve(), 200, 1.1, 2, true);
const point = new THREE.Vector3();
const group = new THREE.Group();
const ribbon = new THREE.Object3D(); group.add(ribbon);
const ring = new THREE.Mesh(new THREE.RingGeometry(ringRadii.inner, ringRadii.outer, 192));
ring.position.x = indicatorShape.ringOffset; ring.rotation.y = Math.PI / 2; group.add(ring);
let maximumX = 0, maximumY = 0;
function measure(positions, object) {
  for (let i = 0; i < positions.count; i++) {
    point.fromBufferAttribute(positions, i).applyMatrix4(object.matrixWorld).project(camera);
    maximumX = Math.max(maximumX, Math.abs(point.x) * 70);
    maximumY = Math.max(maximumY, Math.abs(point.y) * 24);
  }
}
for (let step = 0; step <= 240; step += 2) {
  const pose = completionPose(step / 240 * 4000, 4000, 4.2);
  group.rotation.y = -Math.PI / 2 * pose.progress; group.position.z = -indicatorShape.retreat * pose.progress;
  for (let rotation = 0; rotation < Math.PI * 2; rotation += .1) {
    ribbon.rotation.x = rotation; group.updateMatrixWorld(true);
    if (pose.ribbonOpacity > 0) measure(geometry.attributes.position, ribbon);
    if (pose.ringOpacity > 0) measure(ring.geometry.attributes.position, ring);
  }
}
assert.ok(maximumX < 70, `Horizontal clipping: ${maximumX}`);
assert.ok(maximumY < 24, `Vertical clipping: ${maximumY}`);
console.log(`140×48 indicator: projected bounds ±${maximumX.toFixed(2)}×${maximumY.toFixed(2)}px; no geometry clipping.`);
const projectRadius = radius => new THREE.Vector3(0, radius, 0).applyMatrix4(ring.matrixWorld).project(camera).y * indicatorShape.height / 2;
assert.ok(Math.abs((projectRadius(ringRadii.inner) + projectRadius(ringRadii.outer)) / 2 - indicatorShape.radius) < 1e-6);
assert.ok(Math.abs(projectRadius(ringRadii.outer) - projectRadius(ringRadii.inner) - indicatorShape.stroke) < 1e-6);
const center = new THREE.Vector3().applyMatrix4(ring.matrixWorld).project(camera);
assert.ok(Math.abs(center.x) + Math.abs(center.y) < 1e-9);
const dt = .001;
assert.ok(Math.abs(completionPose(dt, 4000, 4.2).turn / (dt / 1000) - 4.2) < 1e-5);
assert.equal(completionPose(4000, 4000, 4.2).ringOpacity, 1);
assert.equal(completionPose(4000, 4000, 4.2).ribbonOpacity, 0);
let lastTurn = 0;
for (let elapsed = 0; elapsed <= 4000; elapsed++) {
  const pose = completionPose(elapsed, 4000, 4.2);
  assert.ok(pose.turn >= lastTurn);
  assert.ok(pose.ribbonOpacity >= 0 && pose.ribbonOpacity <= 1);
  assert.ok(pose.ringOpacity >= 0 && pose.ringOpacity <= 1);
  assert.ok(pose.ribbonOpacity + pose.ringOpacity >= 1 - 1e-9, 'Gap in ribbon/ring coverage');
  lastTurn = pose.turn;
}
console.log('Final circle: centered, radius 15.84px, stroke 1.8px; idle→completion angular velocity continuous.');
