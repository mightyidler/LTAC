import assert from 'node:assert/strict';
import * as THREE from 'three';
import { RibbonCurve, indicatorShape, ringRadii, completionPose, updateRingGeometry } from '../ui/src/connection-motion.ts';

const camera = new THREE.PerspectiveCamera(indicatorShape.fov, indicatorShape.width / indicatorShape.height, 1, 1000);
camera.position.z = indicatorShape.cameraZ;
camera.updateMatrixWorld();
const geometry = new THREE.TubeGeometry(new RibbonCurve(), 200, 1.1, 2, true);
const point = new THREE.Vector3();
const group = new THREE.Group();
const ribbon = new THREE.Object3D();
group.add(ribbon);
const ring = new THREE.Mesh(new THREE.RingGeometry(ringRadii.inner, ringRadii.outer, 192));
ring.position.x = indicatorShape.ringOffset;
ring.rotation.y = Math.PI / 2;
group.add(ring);
let firstRadius = null, lastRadius = 0;
let maximumX = 0, maximumY = 0, finalRibbonRadius = 0;
for (let step = 0; step <= 240; step += 2) {
  const pose = completionPose(step / 240 * 4000, 4000, 4.2);
  group.rotation.y = -Math.PI / 2 * pose.progress;
  group.position.z = -indicatorShape.retreat * pose.progress;
  if (pose.cover > 0) {
    updateRingGeometry(ring.geometry, pose.ringRadius, pose.progress);
    group.updateMatrixWorld(true);
    const inner = new THREE.Vector3().fromBufferAttribute(ring.geometry.attributes.position, 48).applyMatrix4(ring.matrixWorld).project(camera);
    const outer = new THREE.Vector3().fromBufferAttribute(ring.geometry.attributes.position, 193 + 48).applyMatrix4(ring.matrixWorld).project(camera);
    assert.ok(Math.abs((outer.y - inner.y) * 24 - indicatorShape.stroke) < 1e-5, 'Circle growth must not scale stroke thickness');
    assert.ok(pose.ringRadius >= lastRadius, 'The circle should grow smoothly');
    firstRadius ??= pose.ringRadius;
    lastRadius = pose.ringRadius;
  }
  for (let rotation = 0; rotation < Math.PI * 2; rotation += .1) {
    ribbon.rotation.x = rotation;
    group.updateMatrixWorld(true);
    for (let i = 0; i < geometry.attributes.position.count; i++) {
      point.fromBufferAttribute(geometry.attributes.position, i).applyMatrix4(ribbon.matrixWorld).project(camera);
      maximumX = Math.max(maximumX, Math.abs(point.x) * 70);
      maximumY = Math.max(maximumY, Math.abs(point.y) * 24);
      if (pose.done) finalRibbonRadius = Math.max(finalRibbonRadius, Math.hypot(point.x * 70, point.y * 24));
    }
  }
}
assert.ok(maximumX < 70, `Horizontal clipping: ${maximumX}`);
assert.ok(maximumY < 24, `Vertical clipping: ${maximumY}`);
// The existing local X spin axis must face the viewer after the 90-degree turn.
const axis = new THREE.Vector3(1, 0, 0).applyQuaternion(group.quaternion);
assert.ok(axis.distanceTo(new THREE.Vector3(0, 0, 1)) < 1e-9);
const dt = .001;
assert.ok(Math.abs(completionPose(dt, 4000, 4.2).turn / (dt / 1000) - 4.2) < 1e-5);
const end = completionPose(4000, 4000, 4.2);
const after = completionPose(4500, 4000, 4.2);
assert.equal(end.progress, 1);
assert.equal(after.progress, 1);
assert.equal(after.turn, end.turn, 'Stop rotating once the end face is complete');
assert.equal(end.cover, 1, 'The front face must fully hide the rear turns');
assert.equal(completionPose(0, 4000, 4.2).cover, 0, 'Keep the cover out of the loading loop');
const faceDepth = indicatorShape.cameraZ + indicatorShape.retreat - indicatorShape.ringOffset;
const facePixelsPerUnit = 24 / (faceDepth * Math.tan(indicatorShape.fov * Math.PI / 360));
const outerRadius = ringRadii.outer * facePixelsPerUnit;
assert.ok(maximumY > outerRadius && maximumY - outerRadius < 1.25, 'Leave only slight rotating tips outside the thin circle before occlusion');
assert.ok(Math.abs(finalRibbonRadius - outerRadius) < 1e-5, 'The stopped ribbon tips and circle must meet at the same outer radius');
assert.ok(lastRadius > firstRadius);
assert.equal(lastRadius, indicatorShape.radius);
const finalSpeed = (end.turn - completionPose(3999, 4000, 4.2).turn) / .001;
assert.ok(finalSpeed < .001, 'The rotating tips must slow to a stop during the merge');
assert.deepEqual(geometry.morphAttributes, {}, 'Do not deform the original ribbon');
console.log(`Original ribbon: bounds ±${maximumX.toFixed(2)}×${maximumY.toFixed(2)}px; no clipping.`);
console.log(`O grows ${firstRadius.toFixed(2)}→${lastRadius.toFixed(2)}px; its stroke stays ${indicatorShape.stroke}px. Rotating tips settle to a stop.`);
