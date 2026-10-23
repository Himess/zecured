// The landing page's scene (bundled into site/scene.js by site/dev/build.mjs): Zcash's shielded pool below, Base's
// ledger of light above, and gold proofs, the only thing that crosses between them. main.js drives it with the
// scroll position: each chapter of the page has a camera and a small story here, and the page's small tags
// ([data-tag]) follow the things they name.
import {
  AdditiveBlending,
  BackSide,
  BufferAttribute,
  BufferGeometry,
  DoubleSide,
  LineSegments,
  Mesh,
  PerspectiveCamera,
  PlaneGeometry,
  Points,
  Scene,
  ShaderMaterial,
  SphereGeometry,
  Vector3,
  WebGLRenderer,
} from "three";
import * as glsl from "./shaders.js";

const POOL_Y = -2.4;
const LEDGER_Y = 3.0;
const v3 = (x, y, z) => new Vector3(x, y, z);
const above = (p) => v3(p.x, LEDGER_Y, p.z);
const below = (p) => v3(p.x, POOL_Y, p.z);

// The places of the story.
const VAULT = v3(5, POOL_Y, -10);
const MARKET = v3(1, LEDGER_Y, -18);
const ACCOUNT = v3(9, LEDGER_Y, -9);
const ESCROW = v3(4.5, LEDGER_Y, -12);
const SWAP_IN = v3(11, LEDGER_Y, -3);
const SWAP_OUT = v3(13, LEDGER_Y, -21);
const MAKER_NOTE = v3(13, POOL_Y, -23);
const TO_YOU = v3(0.5, POOL_Y, -3.5);
const TO_MAKER = v3(-2, POOL_Y, -19);
const SWEPT = v3(-3, POOL_Y, -21);

const rgb = (hex) => v3(((hex >> 16) & 255) / 255, ((hex >> 8) & 255) / 255, (hex & 255) / 255);
const COLOR = {
  clear: 0x040907,
  deep: rgb(0x0c3326),
  crest: rgb(0x38a684),
  gold: rgb(0xf4b728),
  note: rgb(0xffe3a1),
  line: rgb(0xb9cde4),
  usdc: rgb(0x6aa6ff),
  red: rgb(0xff4f45),
  signer: rgb(0xe6f0ff),
  dim: rgb(0x33433c),
  skyTop: rgb(0x040807),
  skyHorizon: rgb(0x0a1c17),
  skyBottom: rgb(0x030605),
  skyGlow: rgb(0x14453b),
  skyWarm: rgb(0x3a2a08),
};

const clamp01 = (x) => Math.min(1, Math.max(0, x));
const smooth = (a, b, x) => {
  const t = clamp01((x - a) / (b - a));
  return t * t * (3 - 2 * t);
};
const mix = (a, b, t) => a + (b - a) * t;

/** A point on a comet's path: a straight line lifted into an arc. */
function pathPoint(from, to, lift, s, out) {
  out.lerpVectors(from, to, s);
  out.y += Math.sin(s * Math.PI) * lift;
  return out;
}

// ------------------------------------------------------------------------------------------- the chapters

/** Where the camera stands and looks, per chapter: the story on the side the chapter's text leaves free. */
const CAMERA = {
  hero: { pos: [0, 0.6, 10], at: [2, -2.6, -30] },
  deposit: { pos: [7.5, -0.2, 3], at: [1.2, -2.1, -10] },
  prove: { pos: [2.5, 0.5, 3], at: [9.5, 0.6, -10] },
  borrow: { pos: [1, 10.5, 6], at: [2.5, 3, -12] },
  close: { pos: [2.5, 1.2, 3.5], at: [9, -1.4, -10] },
  swap: { pos: [0, 8.5, 6], at: [3.5, 0.8, -12] },
  privacy: { pos: [0, 0.6, 17], at: [0, -1.4, -26] },
  trust: { pos: [3, 0.4, 3], at: [10, -1.2, -11] },
  numbers: { pos: [-2, 1.4, 13], at: [0, -1.2, -30] },
  demo: { pos: [2, 0.8, 12], at: [0, -1.6, -30] },
  end: { pos: [0, 0.6, 10], at: [2, -2.6, -30] },
};

/** On a tall screen the text sits low: the story moves to the middle and upper part of the frame. */
const CAMERA_TALL = {
  hero: { pos: [4, 1.0, 2], at: [6, -2.6, -24] },
  deposit: { pos: [5, 1.2, 3], at: [5, -4.4, -10] },
  prove: { pos: [5, 0.6, 6], at: [5, -1.6, -10] },
  borrow: { pos: [5, 13, 4], at: [5, 1.2, -13] },
  close: { pos: [3.5, 1.6, 4], at: [3.5, -3.8, -11] },
  swap: { pos: [5, 12, 5], at: [5, 0, -13] },
  privacy: { pos: [0, 1.0, 12], at: [0, -1.8, -26] },
  trust: { pos: [3, 1.4, 4], at: [2.5, -3.6, -12] },
  numbers: { pos: [0, 1.4, 12], at: [2, -2.4, -30] },
  demo: { pos: [1, 0.8, 11], at: [2, -2.6, -30] },
  end: { pos: [4, 1.0, 2], at: [6, -2.6, -24] },
};

/** Everything a chapter sets, at its own progress `p` (0 as it arrives, 1 as it leaves). */
function chapterState(name, p) {
  const s = {
    idle: 0,
    vaultGlow: 0,
    signers: 0,
    compromised: 0,
    slashed: 0,
    leaks: 0,
    usdc: 0,
    usdcFrom: MARKET,
    usdcTo: ACCOUNT,
    mark: ACCOUNT,
    markGlow: 0,
    alarm: 0,
    spot: VAULT,
    spotGlow: 0,
    cells: 0.06,
    focus: v3(4, LEDGER_Y, -24),
    focusR: 26,
    comets: [],
    tags: {},
  };
  const tag = (id, at, on = 1) => {
    s.tags[id] = { at, on };
  };
  switch (name) {
    case "deposit": {
      s.signers = 1;
      s.focus = above(VAULT);
      s.focusR = 10;
      const drop = smooth(0.08, 0.6, p);
      s.vaultGlow = smooth(0.55, 0.8, p);
      s.comets.push({ slot: 4, from: v3(VAULT.x + 0.4, 1.6, VAULT.z + 1.5), to: VAULT, lift: 0.4, progress: drop, size: 15, color: COLOR.note });
      tag("vault", v3(VAULT.x, POOL_Y + 1.25, VAULT.z));
      tag("payment", v3(VAULT.x + 0.4, 1.9, VAULT.z + 1.5), 1 - smooth(0.4, 0.6, p));
      break;
    }
    case "prove": {
      s.signers = 1;
      s.vaultGlow = 1;
      s.focus = above(VAULT);
      s.focusR = 12;
      const rise = smooth(0.06, 0.66, p);
      s.comets.push({ slot: 3, from: VAULT, to: above(VAULT), lift: 0.9, progress: rise, size: 24, color: COLOR.gold, impact: true, tag: "proof" });
      s.mark = above(VAULT);
      s.markGlow = smooth(0.66, 0.8, p);
      tag("minted", v3(VAULT.x, LEDGER_Y - 0.9, VAULT.z), smooth(0.68, 0.8, p));
      break;
    }
    case "borrow":
      s.focus = v3((MARKET.x + ACCOUNT.x) / 2, LEDGER_Y, (MARKET.z + ACCOUNT.z) / 2);
      s.focusR = 12;
      s.usdc = smooth(0.04, 0.28, p);
      s.mark = ACCOUNT;
      s.markGlow = smooth(0.04, 0.3, p);
      s.cells = 0.1;
      tag("market", v3(MARKET.x, LEDGER_Y + 0.9, MARKET.z), smooth(0.05, 0.25, p));
      tag("account", v3(ACCOUNT.x, LEDGER_Y + 1.5, ACCOUNT.z), smooth(0.15, 0.35, p));
      break;
    case "close": {
      s.signers = 1;
      s.focus = above(VAULT);
      s.focusR = 12;
      s.vaultGlow = 1 - smooth(0.4, 0.75, p);
      const commit = smooth(0.0, 0.28, p);
      s.comets.push({ slot: 3, from: VAULT, to: above(VAULT), lift: 0.6, progress: commit, size: 20, color: COLOR.gold, impact: true });
      tag("committed", v3(VAULT.x, LEDGER_Y - 0.9, VAULT.z), smooth(0.24, 0.32, p) * (1 - smooth(0.7, 0.8, p)));
      const out = smooth(0.34, 0.82, p);
      s.comets.push({ slot: 4, from: VAULT, to: TO_YOU, lift: 1.0, progress: out, size: 15, color: COLOR.note });
      s.comets.push({ slot: 5, from: VAULT, to: TO_MAKER, lift: 1.3, progress: out, size: 14, color: COLOR.note });
      tag("to-you", v3(TO_YOU.x, POOL_Y + 0.9, TO_YOU.z), smooth(0.78, 0.88, p));
      tag("to-maker", v3(TO_MAKER.x, POOL_Y + 1.1, TO_MAKER.z), smooth(0.78, 0.88, p));
      break;
    }
    case "swap": {
      s.focus = ESCROW;
      s.focusR = 13;
      s.mark = ESCROW;
      s.markGlow = smooth(0.0, 0.18, p) * (1 - smooth(0.88, 1.0, p) * 0.6);
      const locking = p < 0.6;
      s.usdc = locking ? smooth(0.0, 0.1, p) * (1 - smooth(0.28, 0.36, p)) : smooth(0.8, 0.88, p);
      s.usdcFrom = locking ? SWAP_IN : ESCROW;
      s.usdcTo = locking ? ESCROW : SWAP_OUT;
      tag("escrow", v3(ESCROW.x, LEDGER_Y + 1.3, ESCROW.z), smooth(0.04, 0.16, p));
      const payee = below(ESCROW);
      const paid = smooth(0.3, 0.55, p);
      s.comets.push({ slot: 4, from: MAKER_NOTE, to: payee, lift: 0.8, progress: paid, size: 15, color: COLOR.note });
      s.spot = payee;
      s.spotGlow = smooth(0.5, 0.6, p);
      tag("payee", v3(payee.x, POOL_Y + 0.9, payee.z), smooth(0.46, 0.54, p) * (1 - smooth(0.56, 0.62, p)));
      const proof = smooth(0.56, 0.8, p);
      s.comets.push({ slot: 3, from: payee, to: ESCROW, lift: 0.7, progress: proof, size: 24, color: COLOR.gold, impact: true, tag: "paymentProof" });
      tag("released", v3(SWAP_OUT.x, LEDGER_Y + 0.8, SWAP_OUT.z), smooth(0.84, 0.92, p));
      break;
    }
    case "privacy":
      s.focus = v3(0, LEDGER_Y, -22);
      s.focusR = 34;
      s.leaks = smooth(0.0, 0.16, p) * (1 - smooth(0.5, 0.64, p));
      s.idle = smooth(0.52, 0.7, p);
      s.cells = 0.03;
      break;
    case "trust": {
      s.signers = 1;
      s.focus = above(VAULT);
      s.focusR = 14;
      s.vaultGlow = 1 - smooth(0.3, 0.5, p);
      s.compromised = smooth(0.04, 0.22, p);
      tag("compromised", v3(VAULT.x, POOL_Y + 1.3, VAULT.z), smooth(0.08, 0.2, p) * (1 - smooth(0.7, 0.78, p)));
      const sweep = smooth(0.22, 0.46, p);
      s.comets.push({ slot: 5, from: VAULT, to: SWEPT, lift: 1.4, progress: sweep, size: 17, color: COLOR.red });
      const proof = smooth(0.48, 0.72, p);
      s.comets.push({ slot: 3, from: SWEPT, to: above(VAULT), lift: 1.2, progress: proof, size: 24, color: COLOR.gold, impact: true, tag: "violation" });
      s.mark = above(VAULT);
      s.markGlow = smooth(0.7, 0.78, p);
      s.alarm = 1;
      s.slashed = smooth(0.72, 0.86, p);
      tag("slashed", v3(VAULT.x, LEDGER_Y - 0.9, VAULT.z), smooth(0.74, 0.84, p));
      s.usdc = smooth(0.84, 0.94, p);
      s.usdcFrom = above(VAULT);
      s.usdcTo = ACCOUNT;
      break;
    }
    default:
      s.idle = 1;
      tag("pool", v3(17, POOL_Y + 0.3, -15));
      tag("ledger", v3(17, LEDGER_Y - 0.2, -16));
  }
  return s;
}

// --------------------------------------------------------------------------------------------- the scene

export function startScene(canvas, { tags: tagLayer = null, onReady } = {}) {
  let renderer;
  try {
    renderer = new WebGLRenderer({ canvas, antialias: true, powerPreference: "high-performance" });
  } catch {
    return null;
  }
  if (!renderer.getContext()) return null;
  renderer.setClearColor(COLOR.clear, 1);

  const small = Math.min(window.innerWidth, window.innerHeight) < 700;
  let pixelRatio = Math.min(window.devicePixelRatio || 1, small ? 1.5 : 1.75);
  renderer.setPixelRatio(pixelRatio);

  const scene = new Scene();
  const camera = new PerspectiveCamera(small ? 62 : 50, 1, 0.1, 220);
  const additive = { transparent: true, depthWrite: false, blending: AdditiveBlending };

  let seed = 7;
  const rand = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };

  // The sky, drawn first and always behind.
  const sky = new Mesh(
    new SphereGeometry(150, 32, 16),
    new ShaderMaterial({
      vertexShader: glsl.skyVertex,
      fragmentShader: glsl.skyFragment,
      side: BackSide,
      depthTest: false,
      depthWrite: false,
      uniforms: {
        uTop: { value: COLOR.skyTop },
        uHorizon: { value: COLOR.skyHorizon },
        uBottom: { value: COLOR.skyBottom },
        uGlow: { value: COLOR.skyGlow },
        uWarm: { value: COLOR.skyWarm },
      },
    }),
  );
  sky.renderOrder = -10;
  sky.frustumCulled = false;
  scene.add(sky);

  // The pool: rows of notes, breathing.
  const cols = small ? 140 : 240;
  const rows = small ? 130 : 220;
  const poolPositions = new Float32Array(cols * rows * 3);
  const poolSeeds = new Float32Array(cols * rows);
  for (let r = 0, k = 0; r < rows; r += 1) {
    for (let c = 0; c < cols; c += 1, k += 1) {
      poolPositions[k * 3] = -60 + (c + rand() * 0.25) * (120 / cols);
      poolPositions[k * 3 + 1] = POOL_Y;
      poolPositions[k * 3 + 2] = 16 - (r + rand() * 0.25) * (110 / rows);
      poolSeeds[k] = rand();
    }
  }
  const poolGeometry = new BufferGeometry();
  poolGeometry.setAttribute("position", new BufferAttribute(poolPositions, 3));
  poolGeometry.setAttribute("aSeed", new BufferAttribute(poolSeeds, 1));
  const poolMaterial = new ShaderMaterial({
    ...additive,
    vertexShader: glsl.poolVertex,
    fragmentShader: glsl.poolFragment,
    uniforms: {
      uTime: { value: 0 },
      uPixelRatio: { value: pixelRatio },
      uSize: { value: small ? 2.6 : 2.2 },
      uDeep: { value: COLOR.deep },
      uCrest: { value: COLOR.crest },
      uGold: { value: COLOR.gold },
      uVault: { value: VAULT.clone() },
      uVaultGlow: { value: 0 },
      uSpot: { value: VAULT.clone() },
      uSpotGlow: { value: 0 },
    },
  });
  const pool = new Points(poolGeometry, poolMaterial);
  pool.frustumCulled = false;
  scene.add(pool);

  // The ledger.
  const impacts = Array.from({ length: 4 }, () => new Vector3());
  const impactAges = [-1, -1, -1, -1];
  const ledgerMaterial = new ShaderMaterial({
    ...additive,
    vertexShader: glsl.ledgerVertex,
    fragmentShader: glsl.ledgerFragment,
    side: DoubleSide,
    uniforms: {
      uTime: { value: 0 },
      uLine: { value: COLOR.line },
      uGold: { value: COLOR.gold },
      uRed: { value: COLOR.red },
      uOpacity: { value: 1 },
      uCells: { value: 0.06 },
      uImpact: { value: impacts },
      uImpactAge: { value: impactAges },
      uMark: { value: ACCOUNT.clone() },
      uMarkGlow: { value: 0 },
      uAlarm: { value: 0 },
      uFocus: { value: v3(4, LEDGER_Y, -24) },
      uFocusR: { value: 26 },
    },
  });
  const ledger = new Mesh(new PlaneGeometry(260, 260), ledgerMaterial);
  ledger.rotation.x = -Math.PI / 2;
  ledger.position.set(0, LEDGER_Y, -60);
  ledger.frustumCulled = false;
  scene.add(ledger);

  // Comets: eight slots of a head and its tail.
  const TRAIL = 60;
  const SLOTS = 8;
  const cometIndex = new Float32Array(SLOTS * TRAIL);
  const cometTrail = new Float32Array(SLOTS * TRAIL);
  for (let i = 0; i < SLOTS; i += 1) {
    for (let j = 0; j < TRAIL; j += 1) {
      cometIndex[i * TRAIL + j] = i;
      cometTrail[i * TRAIL + j] = j / TRAIL;
    }
  }
  const cometGeometry = new BufferGeometry();
  cometGeometry.setAttribute("position", new BufferAttribute(new Float32Array(SLOTS * TRAIL * 3), 3));
  cometGeometry.setAttribute("aComet", new BufferAttribute(cometIndex, 1));
  cometGeometry.setAttribute("aTrail", new BufferAttribute(cometTrail, 1));
  const comets = Array.from({ length: SLOTS }, () => ({
    from: new Vector3(),
    to: new Vector3(),
    lift: 0,
    progress: 0,
    size: 10,
    color: COLOR.gold.clone(),
    on: 0,
    head: new Vector3(),
  }));
  const cometMaterial = new ShaderMaterial({
    ...additive,
    vertexShader: glsl.cometVertex,
    fragmentShader: glsl.cometFragment,
    uniforms: {
      uPixelRatio: { value: pixelRatio },
      uFrom: { value: comets.map((c) => c.from) },
      uTo: { value: comets.map((c) => c.to) },
      uLift: { value: comets.map(() => 0) },
      uProgress: { value: comets.map(() => 0) },
      uSize: { value: comets.map(() => 10) },
      uColor: { value: comets.map((c) => c.color) },
      uOn: { value: comets.map(() => 0) },
    },
  });
  const cometPoints = new Points(cometGeometry, cometMaterial);
  cometPoints.frustumCulled = false;
  scene.add(cometPoints);

  // Small lights: USDC on the ledger, the signers, the market and the account, and what a bridge leaks.
  const nodeUniforms = { uPixelRatio: { value: pixelRatio }, uTime: { value: 0 } };
  function lights(count) {
    const geometry = new BufferGeometry();
    const positions = new Float32Array(count * 3);
    const colors = new Float32Array(count * 3);
    const sizes = new Float32Array(count);
    geometry.setAttribute("position", new BufferAttribute(positions, 3));
    geometry.setAttribute("aColor", new BufferAttribute(colors, 3));
    geometry.setAttribute("aSize", new BufferAttribute(sizes, 1));
    const points = new Points(geometry, new ShaderMaterial({ ...additive, vertexShader: glsl.nodesVertex, fragmentShader: glsl.nodesFragment, uniforms: nodeUniforms }));
    points.frustumCulled = false;
    scene.add(points);
    return {
      positions,
      colors,
      sizes,
      commit() {
        geometry.attributes.position.needsUpdate = true;
        geometry.attributes.aColor.needsUpdate = true;
        geometry.attributes.aSize.needsUpdate = true;
      },
    };
  }

  const FLOW = small ? 120 : 220;
  const flow = lights(FLOW);
  const flowOffsets = Float32Array.from({ length: FLOW }, () => rand());

  const nodes = lights(7);

  const THREADS = small ? 18 : 28;
  const PER_THREAD = 9;
  const leak = lights(THREADS * PER_THREAD);
  const threads = Array.from({ length: THREADS }, () => ({
    a: v3(-30 + rand() * 60, POOL_Y + 0.2, -4 - rand() * 40),
    b: v3(-30 + rand() * 60, LEDGER_Y, -4 - rand() * 40),
    phase: rand(),
    speed: 0.12 + rand() * 0.1,
  }));

  // Lines: the vault's ring of signers, and the threads a bridge leaves between the two chains.
  const SEGMENTS = 5 + THREADS;
  const lineGeometry = new BufferGeometry();
  const linePositions = new Float32Array(SEGMENTS * 6);
  const lineColors = new Float32Array(SEGMENTS * 6);
  const lineAlphas = new Float32Array(SEGMENTS * 2);
  lineGeometry.setAttribute("position", new BufferAttribute(linePositions, 3));
  lineGeometry.setAttribute("aColor", new BufferAttribute(lineColors, 3));
  lineGeometry.setAttribute("aAlpha", new BufferAttribute(lineAlphas, 1));
  const lines = new LineSegments(
    lineGeometry,
    new ShaderMaterial({ ...additive, vertexShader: glsl.linesVertex, fragmentShader: glsl.linesFragment, uniforms: { uOpacity: { value: 1 } } }),
  );
  lines.frustumCulled = false;
  scene.add(lines);

  // The page's tags that follow things in the scene.
  const tagEls = new Map();
  tagLayer?.querySelectorAll("[data-tag]").forEach((el) =>
    tagEls.set(el.dataset.tag, { el, label: el.firstElementChild, width: 0, on: 0, shown: false }),
  );

  // ----------------------------------------------------------------------------------------- the state

  const view = { pos: v3(...CAMERA.hero.pos), at: v3(...CAMERA.hero.at) };
  const goal = { pos: view.pos.clone(), at: view.at.clone() };
  const keys = ["idle", "vaultGlow", "signers", "compromised", "slashed", "leaks", "usdc", "markGlow", "alarm", "spotGlow", "cells", "focusR"];
  let target = chapterState("hero", 0);
  const now = Object.fromEntries(keys.map((k) => [k, target[k]]));
  const focus = target.focus.clone();
  const pointer = { x: 0, y: 0, sx: 0, sy: 0 };
  const scratch = new Vector3();
  let width = 1;
  let height = 1;

  /** Sets the page's place: the chapter on screen, its progress, and the next chapter it blends into. */
  function setScroll(name, p, next) {
    const table = camera.aspect < 0.9 ? CAMERA_TALL : CAMERA;
    const a = table[name] ?? table.hero;
    const b = table[next] ?? a;
    const t = smooth(0.6, 1.0, p);
    goal.pos.set(mix(a.pos[0], b.pos[0], t), mix(a.pos[1], b.pos[1], t), mix(a.pos[2], b.pos[2], t));
    goal.at.set(mix(a.at[0], b.at[0], t), mix(a.at[1], b.at[1], t), mix(a.at[2], b.at[2], t));
    target = chapterState(name, p);
  }

  function resize() {
    width = window.innerWidth;
    height = window.innerHeight;
    renderer.setSize(width, height, false);
    camera.aspect = width / height;
    camera.updateProjectionMatrix();
  }

  function setPixelRatio(value) {
    pixelRatio = value;
    renderer.setPixelRatio(value);
    poolMaterial.uniforms.uPixelRatio.value = value;
    cometMaterial.uniforms.uPixelRatio.value = value;
    nodeUniforms.uPixelRatio.value = value;
    resize();
  }

  // Idle proofs: three rising one after another, on the side the hero's text leaves free.
  const IDLE = [
    { from: v3(6, POOL_Y, -17), period: 7.5, offset: 0.0 },
    { from: v3(14, POOL_Y, -27), period: 7.5, offset: 0.36 },
    { from: v3(-2, POOL_Y, -36), period: 7.5, offset: 0.7 },
  ];
  const lastProgress = new Array(SLOTS).fill(0);
  let impactNext = 0;

  function impact(at) {
    impacts[impactNext].copy(at);
    impactAges[impactNext] = 0;
    impactNext = (impactNext + 1) % impacts.length;
  }

  function update(time, dt, snap = false) {
    // Ease every quantity toward its target: the camera slowly, the story a little faster.
    const kCam = 1 - Math.exp(-dt * 2.4);
    const kState = 1 - Math.exp(-dt * 5);
    view.pos.lerp(goal.pos, kCam);
    view.at.lerp(goal.at, kCam);
    for (const k of keys) now[k] += (target[k] - now[k]) * kState;
    pointer.sx += (pointer.x - pointer.sx) * (1 - Math.exp(-dt * 3));
    pointer.sy += (pointer.y - pointer.sy) * (1 - Math.exp(-dt * 3));

    const drift = now.idle;
    camera.position.set(
      view.pos.x + pointer.sx * 0.7 + Math.sin(time * 0.07) * 1.6 * drift,
      view.pos.y - pointer.sy * 0.35 + Math.sin(time * 0.05) * 0.18 * drift,
      view.pos.z,
    );
    camera.lookAt(view.at);
    camera.updateMatrixWorld();
    sky.position.copy(camera.position);
    focus.lerp(target.focus, kCam);

    poolMaterial.uniforms.uTime.value = time;
    poolMaterial.uniforms.uVaultGlow.value = now.vaultGlow;
    poolMaterial.uniforms.uSpot.value.copy(target.spot);
    poolMaterial.uniforms.uSpotGlow.value = now.spotGlow;
    const lu = ledgerMaterial.uniforms;
    lu.uTime.value = time;
    lu.uCells.value = now.cells;
    lu.uMark.value.copy(target.mark);
    lu.uMarkGlow.value = now.markGlow;
    lu.uAlarm.value = now.alarm;
    lu.uFocus.value.copy(focus);
    lu.uFocusR.value = now.focusR;
    nodeUniforms.uTime.value = time;

    // Comets: the hero's idle proofs in slots 0 to 2, the chapter's own in 3 to 7.
    for (const c of comets) c.on = 0;
    let heroProof = 0;
    IDLE.forEach((idle, i) => {
      const phase = (time / idle.period + idle.offset) % 1;
      const c = comets[i];
      c.from.copy(idle.from);
      c.to.set(idle.from.x + 1.5, LEDGER_Y, idle.from.z - 2);
      c.lift = 0.6;
      c.size = 24;
      c.color.copy(COLOR.gold);
      c.progress = smooth(0.0, 0.62, phase) + (phase > 0.62 ? (phase - 0.62) * 0.6 : 0);
      c.on = now.idle;
      if (i === 0) heroProof = smooth(0.05, 0.15, phase) * (1 - smooth(0.5, 0.6, phase)) * now.idle;
    });
    const tagged = {};
    for (const spec of target.comets) {
      const c = comets[spec.slot];
      c.from.copy(spec.from);
      c.to.copy(spec.to);
      c.lift = spec.lift;
      c.size = spec.size;
      c.color.copy(spec.color);
      c.progress += (spec.progress - c.progress) * (1 - Math.exp(-dt * 8));
      if (spec.progress <= 0.001 && c.progress < 0.01) c.progress = 0;
      c.on = c.progress > 0.002 ? 1 : 0;
      if (spec.tag) tagged[spec.tag] = { slot: spec.slot, on: c.on * smooth(0.06, 0.18, c.progress) * (1 - smooth(0.9, 0.99, c.progress)) };
    }
    comets.forEach((c, i) => {
      const lands = i < 3 ? c.on > 0.05 : target.comets.some((s) => s.slot === i && s.impact);
      if (lands && lastProgress[i] < 0.985 && c.progress >= 0.985) impact(c.to);
      lastProgress[i] = c.progress;
      pathPoint(c.from, c.to, c.lift, Math.min(c.progress, 1), c.head);
      const u = cometMaterial.uniforms;
      u.uLift.value[i] = c.lift;
      u.uProgress.value[i] = Math.min(c.progress, 1.25);
      u.uSize.value[i] = c.size;
      u.uOn.value[i] = c.on;
    });
    for (let i = 0; i < impactAges.length; i += 1) {
      if (impactAges[i] >= 0) impactAges[i] = impactAges[i] + dt / 2.4 < 1 ? impactAges[i] + dt / 2.4 : -1;
    }

    // USDC streaming over the ledger.
    const { usdcFrom: from, usdcTo: to } = target;
    for (let i = 0; i < FLOW; i += 1) {
      const s = (time * 0.18 + flowOffsets[i]) % 1;
      const wobble = Math.sin(flowOffsets[i] * 40 + time) * 0.25;
      flow.positions[i * 3] = mix(from.x, to.x, s) + wobble * 0.3;
      flow.positions[i * 3 + 1] = LEDGER_Y + 0.05 + Math.sin(s * Math.PI) * 0.6 + wobble * 0.15;
      flow.positions[i * 3 + 2] = mix(from.z, to.z, s) + wobble;
      const fade = Math.sin(s * Math.PI) * now.usdc;
      flow.colors[i * 3] = COLOR.usdc.x * fade;
      flow.colors[i * 3 + 1] = COLOR.usdc.y * fade;
      flow.colors[i * 3 + 2] = COLOR.usdc.z * fade;
      flow.sizes[i] = 7 + ((flowOffsets[i] * 7) % 1) * 5;
    }
    flow.commit();

    // Signers around the vault: red once compromised, dim once slashed. Then the market and the account.
    for (let i = 0; i < 5; i += 1) {
      const angle = (i / 5) * Math.PI * 2 + time * 0.15;
      nodes.positions[i * 3] = VAULT.x + Math.cos(angle) * 1.9;
      nodes.positions[i * 3 + 1] = POOL_Y + 0.55 + Math.sin(time * 1.3 + i) * 0.08;
      nodes.positions[i * 3 + 2] = VAULT.z + Math.sin(angle) * 1.9;
      const bad = i < 3 ? now.compromised : 0;
      const c = COLOR.signer.clone().lerp(COLOR.red, bad).lerp(COLOR.dim, now.slashed * 0.85);
      nodes.colors.set([c.x * now.signers, c.y * now.signers, c.z * now.signers], i * 3);
      nodes.sizes[i] = 10;
    }
    nodes.positions.set([MARKET.x, LEDGER_Y + 0.15, MARKET.z, ACCOUNT.x, LEDGER_Y + 0.15, ACCOUNT.z], 15);
    const m = now.usdc;
    nodes.colors.set([COLOR.usdc.x * m, COLOR.usdc.y * m, COLOR.usdc.z * m, COLOR.signer.x * m, COLOR.signer.y * m, COLOR.signer.z * m], 15);
    nodes.sizes[5] = 16;
    nodes.sizes[6] = 12;
    nodes.commit();

    // What a bridge leaks: red sparks running up threads from the pool to the ledger.
    threads.forEach((t, k) => {
      for (let j = 0; j < PER_THREAD; j += 1) {
        const i = k * PER_THREAD + j;
        const s = (time * t.speed + t.phase + j / PER_THREAD) % 1;
        leak.positions[i * 3] = mix(t.a.x, t.b.x, s);
        leak.positions[i * 3 + 1] = mix(t.a.y, t.b.y, s);
        leak.positions[i * 3 + 2] = mix(t.a.z, t.b.z, s);
        const a = now.leaks * Math.sin(s * Math.PI) * 1.2;
        leak.colors.set([COLOR.red.x * a, COLOR.red.y * a, COLOR.red.z * a], i * 3);
        leak.sizes[i] = 11;
      }
    });
    leak.commit();

    // The vault's ring, then the bridge's threads.
    for (let i = 0; i < 5; i += 1) {
      const j = (i + 1) % 5;
      linePositions.set(nodes.positions.subarray(i * 3, i * 3 + 3), i * 6);
      linePositions.set(nodes.positions.subarray(j * 3, j * 3 + 3), i * 6 + 3);
      const c = COLOR.signer.clone().lerp(COLOR.red, (i < 3 && j < 3 ? 1 : 0.5) * now.compromised);
      lineColors.set([c.x, c.y, c.z, c.x, c.y, c.z], i * 6);
      const a = now.signers * 0.35 * (1 - now.slashed * 0.7);
      lineAlphas.set([a, a], i * 2);
    }
    threads.forEach((t, k) => {
      const i = 5 + k;
      linePositions.set([t.a.x, t.a.y, t.a.z, t.b.x, t.b.y, t.b.z], i * 6);
      lineColors.set([COLOR.red.x, COLOR.red.y, COLOR.red.z, COLOR.red.x, COLOR.red.y, COLOR.red.z], i * 6);
      lineAlphas.set([now.leaks * 0.15, now.leaks * 0.55], i * 2);
    });
    lineGeometry.attributes.position.needsUpdate = true;
    lineGeometry.attributes.aColor.needsUpdate = true;
    lineGeometry.attributes.aAlpha.needsUpdate = true;

    // Tags: placed on the things they name, faded with the story.
    const anchors = { ...target.tags };
    if (heroProof > 0) anchors.proofIdle = { at: comets[0].head, on: heroProof };
    for (const [name, t] of Object.entries(tagged)) anchors[name] = { at: comets[t.slot].head, on: t.on };
    for (const [name, entry] of tagEls) {
      const spec = anchors[name];
      const goalOn = spec ? spec.on : 0;
      entry.on = snap ? goalOn : entry.on + (goalOn - entry.on) * (1 - Math.exp(-dt * 7));
      if (spec) {
        scratch.copy(spec.at).project(camera);
        const visible = scratch.z < 1 && Math.abs(scratch.x) < 1.05 && Math.abs(scratch.y) < 1.05;
        if (visible) {
          const x = (scratch.x * 0.5 + 0.5) * width;
          const y = (-scratch.y * 0.5 + 0.5) * height;
          entry.el.style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0)`;
          // Keep the name on screen, even when the thing it names is near an edge.
          if (entry.label) {
            if (!entry.width) entry.width = entry.label.offsetWidth;
            const half = entry.width / 2;
            const fix = x - half < 12 ? 12 - (x - half) : x + half > width - 12 ? width - 12 - (x + half) : 0;
            entry.label.style.transform = `translateX(${(fix - half).toFixed(1)}px)`;
          }
        } else entry.on = 0;
      }
      const o = entry.on < 0.02 ? 0 : entry.on;
      if (o === 0 && !entry.shown) continue;
      entry.el.style.opacity = o.toFixed(3);
      entry.shown = o > 0;
    }
  }

  // ------------------------------------------------------------------------------------------ the loop

  let running = false;
  let last = 0;
  let start = performance.now();
  const frames = [];

  function frame(ms) {
    if (!running) return;
    const dt = Math.min(0.05, last ? (ms - last) / 1000 : 0.016);
    last = ms;
    update((ms - start) / 1000, dt);
    renderer.render(scene, camera);
    // Keep the frame rate: drop the pixel ratio a step while frames run slow.
    frames.push(dt);
    if (frames.length >= 90) {
      frames.sort((a, b) => a - b);
      const median = frames[45];
      frames.length = 0;
      if (median > 0.026 && pixelRatio > 1) setPixelRatio(Math.max(1, pixelRatio - 0.25));
    }
    requestAnimationFrame(frame);
  }

  function play() {
    if (running) return;
    running = true;
    last = 0;
    requestAnimationFrame(frame);
  }

  function pause() {
    running = false;
  }

  /** One frame of the hero at a moment when proofs are in flight (the page's still image is one). */
  function still(at = 3.1) {
    pause();
    start = performance.now() - at * 1000;
    target = chapterState("hero", 0);
    goal.pos.set(...CAMERA.hero.pos);
    goal.at.set(...CAMERA.hero.at);
    view.pos.copy(goal.pos);
    view.at.copy(goal.at);
    for (const k of keys) now[k] = target[k];
    focus.copy(target.focus);
    update(at, 0.016, true);
    renderer.render(scene, camera);
  }

  window.addEventListener("resize", () => {
    resize();
    for (const entry of tagEls.values()) entry.width = 0;
  });
  window.addEventListener(
    "pointermove",
    (e) => {
      pointer.x = (e.clientX / window.innerWidth) * 2 - 1;
      pointer.y = (e.clientY / window.innerHeight) * 2 - 1;
    },
    { passive: true },
  );
  document.addEventListener("visibilitychange", () => (document.hidden ? pause() : play()));

  resize();
  play();
  onReady?.();

  return {
    setScroll,
    still,
    play,
    pause,
    get running() {
      return running;
    },
  };
}
