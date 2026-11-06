export const indicatorShape = {
  width: 140, height: 48, cameraZ: 48, fov: 28,
  retreat: 15, coverOffset: 31, ringOffset: 31.1,
  radius: 14.987204, stroke: 1.8,
} as const;

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
  let turn = 2.1 * seconds + entry + 60 * durationMs / 1000 * integral;
  const settleStart = 1 - Math.sqrt(.03 / 2);
  if (u > settleStart) {
    const startSeconds = settleStart * durationMs / 1000;
    const startQ = Math.min(1, startSeconds / blendSeconds);
    const startIntegral = -2 / 3 * settleStart ** 3 + 2 * settleStart ** 2 - settleStart + 1 / 6;
    const startEntry = (idleSpeed - 2.1) * blendSeconds * (startQ - startQ ** 3 + .5 * startQ ** 4);
    const startTurn = 2.1 * startSeconds + startEntry + 60 * durationMs / 1000 * startIntegral;
    const startSpeed = 2.1 + 60 * .97 + (idleSpeed - 2.1) * (1 - 3 * startQ ** 2 + 2 * startQ ** 3);
    const settle = (u - settleStart) / (1 - settleStart);
    turn = startTurn + startSpeed * (1 - settleStart) * durationMs / 1000 * (settle - settle ** 3 + .5 * settle ** 4);
  }
  const progress = Math.max(0, (acceleration - .35) / .65);
  const merge = smooth(.97, 1, acceleration);
  const ringRadius = indicatorShape.radius * (.9 + .1 * merge);
  const hole = smooth(.9, .98, acceleration);
  return {
    done: u === 1, turn, progress,
    cover: hole, ringRadius,
    // Grow an opaque depth cover from the center. Do not cross-fade the rear
    // ribbon through the finished outline: leave only its outer tips exposed.
    coverRadius: (ringRadius - indicatorShape.stroke / 2 + indicatorShape.stroke * merge + 1.25 * smooth(.99, 1, merge)) * hole,
    ringOpacity: smooth(.94, .97, acceleration),
    ringColor: merge,
  };
}
