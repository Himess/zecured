// The camera, as a function of story time, like everything else. A track of keyframes, each an orbit around the
// thing being told (`at`; `az`imuth and `el`evation in degrees, `dist`ance) and the place on screen where that thing
// sits (`x`, `y`: shares of the width and height from the centre, right and up), so the chapter's text keeps its side.
import { MathUtils, Vector3 } from "three";

const key = (s, at, az, el, dist, x, y) => ({ s, at, az, el, dist, x, y });

// Wide screens: the text on the left, the story on the right.
const WIDE = [
  // The mark, whole: the logo made solid.
  key(0.0, [0, 0, 0], 24, 34, 21, 0.25, -0.02),
  key(0.62, [0, 0, 0], 24, 34, 21, 0.25, -0.02),
  // Deposit: the vault, its signers, the coin.
  key(1.12, [-2.65, 0.7, 0], 10, 36, 10.5, 0.2, 0),
  key(1.8, [-2.65, 0.6, 0], 4, 34, 11, 0.2, 0),
  // Prove: from the vault, through the core, onto the ledger.
  key(2.16, [-0.2, 0.5, 0.5], -4, 30, 15, 0.22, 0),
  key(2.86, [0.2, 0.5, 0.4], 2, 32, 15, 0.22, 0),
  // Borrow: the market and a fresh account, on Base.
  key(3.12, [3.3, 0.4, 0.1], 24, 36, 10.5, 0.2, 0),
  key(3.9, [3.2, 0.4, 0.2], 20, 36, 11, 0.2, 0),
  // Close: repaid on Base, then the vault opens, then a proof crosses back.
  key(4.24, [2.8, 0.4, 0.2], 14, 38, 12, 0.2, 0),
  key(4.4, [-2.2, 0.4, 0.1], 0, 36, 11, 0.2, 0),
  key(4.64, [-2.2, 0.4, 0.1], 0, 36, 11, 0.2, 0),
  key(4.74, [0.1, 0.4, 0.4], 0, 34, 13.5, 0.22, 0),
  key(4.92, [0.1, 0.4, 0.4], 0, 34, 13.5, 0.22, 0),
  // Swap: both halves at once, the seam in the middle.
  key(5.1, [1.6, 0.3, -0.3], 0, 46, 17, 0.22, 0),
  key(5.96, [1.6, 0.3, -0.3], 0, 46, 17, 0.22, 0),
  // Privacy: the front edge, where a bridge would stand.
  key(6.14, [1.2, 0.3, 1.2], 0, 32, 18.5, 0.25, 0),
  key(6.94, [0.8, 0.3, 1.0], 0, 34, 17.5, 0.25, 0),
  // Trust: the theft on Zcash, then the bonds on Base.
  key(7.1, [-3.0, 0.4, -0.5], -8, 36, 11.5, 0.2, 0),
  key(7.44, [-3.0, 0.4, -0.5], -8, 36, 11.5, 0.2, 0),
  key(7.56, [0.4, 0.3, -0.6], 0, 40, 16.5, 0.22, 0),
  key(7.66, [0.4, 0.3, -0.6], 0, 40, 16.5, 0.22, 0),
  key(7.76, [1.4, 0.3, -0.8], 8, 40, 14.5, 0.2, 0),
  key(7.98, [1.4, 0.3, -0.4], 6, 40, 15, 0.2, 0),
  // The rest of the page: the mark again, turning.
  key(8.3, [0, 0, 0], -16, 40, 33, 0.24, 0.24),
  key(9.2, [0, 0, 0], -16, 40, 33, 0.24, 0.24),
  key(9.5, [0, 0, 0], 20, 38, 28, 0.22, 0.02),
  key(10.2, [0, 0, 0], 20, 38, 28, 0.22, 0.02),
  key(10.5, [0, 0, 0], 24, 52, 30, 0, -0.24),
  key(11, [0, 0, 0], 24, 52, 30, 0, -0.24),
];

// Tall screens: the text at the bottom, the story above it; a way between the chains runs up the screen.
const TALL = [
  key(0.0, [0, 0, 0], 24, 30, 36, 0, 0.33),
  key(0.62, [0, 0, 0], 24, 30, 36, 0, 0.33),
  key(1.12, [-2.65, 0.7, 0], 10, 40, 13, 0, 0.24),
  key(1.8, [-2.65, 0.6, 0], 4, 38, 13.5, 0, 0.24),
  key(2.16, [-0.4, 0.5, 0.8], -80, 40, 16, 0, 0.24),
  key(2.86, [-0.2, 0.5, 0.8], -78, 40, 16, 0, 0.24),
  key(3.12, [3.0, 0.4, 0.5], 38, 42, 14, 0, 0.24),
  key(3.9, [3.0, 0.4, 0.5], 34, 42, 14.5, 0, 0.24),
  key(4.24, [3.0, 0.4, 0.4], 30, 42, 14.5, 0, 0.24),
  key(4.4, [-2.2, 0.4, 0.0], 0, 40, 13.5, 0, 0.24),
  key(4.64, [-2.2, 0.4, 0.0], 0, 40, 13.5, 0, 0.24),
  key(4.74, [-0.4, 0.4, 1.0], -80, 40, 16, 0, 0.24),
  key(4.92, [-0.4, 0.4, 1.0], -80, 40, 16, 0, 0.24),
  key(5.1, [1.4, 0.3, -0.2], -90, 42, 21, 0, 0.24),
  key(5.96, [1.4, 0.3, -0.2], -90, 42, 21, 0, 0.24),
  key(6.14, [0.6, 0.3, 1.4], -30, 40, 20, 0, 0.24),
  key(6.94, [0.4, 0.3, 1.0], -40, 40, 19, 0, 0.24),
  key(7.1, [-3.2, 0.4, -0.6], -8, 40, 14, 0, 0.24),
  key(7.44, [-3.2, 0.4, -0.6], -8, 40, 14, 0, 0.24),
  key(7.56, [-0.6, 0.3, 0.8], -80, 42, 16.5, 0, 0.24),
  key(7.66, [-0.6, 0.3, 0.8], -80, 42, 16.5, 0, 0.24),
  key(7.78, [1.4, 0.3, -0.6], -90, 44, 17, 0, 0.24),
  key(7.98, [1.4, 0.3, -0.4], -88, 44, 17.5, 0, 0.24),
  key(8.3, [0, 0, 0], -16, 40, 38, 0, 0.1),
  key(11, [0, 0, 0], 24, 46, 40, 0, 0.1),
];

export const TRACK = { wide: WIDE, tall: TALL };

const mix = (a, b, t) => a + (b - a) * t;
const ease = (t) => t * t * (3 - 2 * t);

/** A pose to fill in. */
export const pose = () => ({ at: new Vector3(), az: 0, el: 0, dist: 1, x: 0, y: 0 });

/** The pose at story time s, eased from one keyframe to the next. */
export function poseAt(track, s, out) {
  let i = 0;
  while (i < track.length - 2 && s > track[i + 1].s) i += 1;
  const a = track[i];
  const b = track[i + 1];
  const t = ease(Math.min(1, Math.max(0, (s - a.s) / (b.s - a.s))));
  out.at.set(mix(a.at[0], b.at[0], t), mix(a.at[1], b.at[1], t), mix(a.at[2], b.at[2], t));
  out.az = mix(a.az, b.az, t);
  out.el = mix(a.el, b.el, t);
  out.dist = mix(a.dist, b.dist, t);
  out.x = mix(a.x, b.x, t);
  out.y = mix(a.y, b.y, t);
  return out;
}

/** Moves `from` a share k of the way to `to`. */
export function follow(from, to, k) {
  from.at.lerp(to.at, k);
  for (const f of ["az", "el", "dist", "x", "y"]) from[f] += (to[f] - from[f]) * k;
}

/**
 * Stands the camera where a pose says, turned a little more by `turn` and `tilt` (degrees) and `zoom` times as far,
 * and shifts the frame so the thing it looks at sits at its place on screen.
 */
export function place(camera, p, width, height, turn = 0, tilt = 0, zoom = 1) {
  const az = MathUtils.degToRad(p.az + turn);
  const el = MathUtils.degToRad(p.el + tilt);
  const dist = p.dist * zoom;
  camera.position.set(
    p.at.x + dist * Math.cos(el) * Math.sin(az),
    p.at.y + dist * Math.sin(el),
    p.at.z + dist * Math.cos(el) * Math.cos(az),
  );
  camera.lookAt(p.at);
  camera.setViewOffset(width, height, -p.x * width, p.y * height, width, height);
}
