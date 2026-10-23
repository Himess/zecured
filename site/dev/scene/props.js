// The things of the story, all made of the same few solids: tiles with a face (notes, events, keys, proofs), coins
// (ZEC, zZEC, USDC) and a few fixtures (the market, the account, the escrow, the signers' bonds, a bridge).
import {
  BufferGeometry,
  CatmullRomCurve3,
  CircleGeometry,
  Color,
  CylinderGeometry,
  DynamicDrawUsage,
  EdgesGeometry,
  Float32BufferAttribute,
  Group,
  InstancedMesh,
  LatheGeometry,
  LineBasicMaterial,
  LineSegments,
  Mesh,
  MeshBasicMaterial,
  MeshPhysicalMaterial,
  Object3D,
  PlaneGeometry,
  TetrahedronGeometry,
  TorusGeometry,
  TubeGeometry,
  Vector2,
  Vector3,
} from "three";
import { RoundedBoxGeometry } from "three/addons/geometries/RoundedBoxGeometry.js";
import { COLORS } from "./world.js";
import { canvasTexture, drawCard, drawChip, drawCipher, drawCoin, drawEvent, drawGlyph, drawMarket } from "./labels.js";

const geometries = new Map();
function rounded(w, h, d, r) {
  const key = `${w}:${h}:${d}:${r}`;
  if (!geometries.has(key)) geometries.set(key, new RoundedBoxGeometry(w, h, d, 4, r));
  return geometries.get(key);
}

/** A coin: a lathe with bevelled edges, its face on top. */
function coinGeometry(radius, thickness) {
  const b = thickness * 0.35;
  const points = [
    new Vector2(0, -thickness / 2),
    new Vector2(radius - b, -thickness / 2),
    new Vector2(radius, -thickness / 2 + b),
    new Vector2(radius, thickness / 2 - b),
    new Vector2(radius - b, thickness / 2),
    new Vector2(radius * 0.86, thickness / 2),
    new Vector2(radius * 0.84, thickness / 2 - b * 0.6),
    new Vector2(0, thickness / 2 - b * 0.6),
  ];
  return new LatheGeometry(points, 72);
}

/** A face laid on a solid's top: a plane with a canvas texture. */
function face(width, depth, texture, y, opacity = 1) {
  const geometry = new PlaneGeometry(width, depth);
  geometry.rotateX(-Math.PI / 2);
  const mesh = new Mesh(geometry, new MeshBasicMaterial({ map: texture, transparent: true, opacity, depthWrite: false }));
  mesh.position.y = y;
  mesh.renderOrder = 2;
  return mesh;
}

function roundFace(radius, texture, y) {
  const geometry = new CircleGeometry(radius, 48);
  geometry.rotateX(-Math.PI / 2);
  const mesh = new Mesh(geometry, new MeshBasicMaterial({ map: texture, transparent: true, depthWrite: false }));
  mesh.position.y = y;
  mesh.renderOrder = 2;
  return mesh;
}

export function buildProps(renderer, world, rand) {
  const glass = () =>
    new MeshPhysicalMaterial({
      color: new Color(0x0f2119),
      metalness: 0.05,
      roughness: 0.2,
      clearcoat: 1,
      clearcoatRoughness: 0.08,
      emissive: new Color(0x062018),
      emissiveIntensity: 1,
    });
  const white = () =>
    new MeshPhysicalMaterial({
      color: new Color(0xf6f8fb),
      roughness: 0.32,
      clearcoat: 0.8,
      clearcoatRoughness: 0.15,
      emissive: new Color(0xffffff),
      emissiveIntensity: 0,
    });
  const gold = () =>
    new MeshPhysicalMaterial({
      color: COLORS.gold,
      metalness: 1,
      roughness: 0.22,
      clearcoat: 0.6,
      clearcoatRoughness: 0.1,
      emissive: COLORS.gold,
      emissiveIntensity: 0.12,
      envMapIntensity: 1.5,
    });

  // ------------------------------------------------------------------------------------- shielded notes

  /** A note: a tile of dark glass whose face is ciphertext. */
  function note() {
    const group = new Group();
    const material = glass();
    const body = new Mesh(rounded(0.92, 0.16, 0.62, 0.06), material);
    body.castShadow = true;
    body.receiveShadow = true;
    const cipher = canvasTexture(renderer, 256, 160, (ctx, w, h, glow = 0) => drawCipher(ctx, w, h, rand, "94, 211, 168", glow));
    const top = face(0.84, 0.54, cipher.texture, 0.083);
    group.add(body, top);
    world.add(group);
    return { group, body, material, top, cipher };
  }

  // ---------------------------------------------------------------------------------------------- coins

  function coin(material, radius, thickness, top, bottom, ink) {
    const group = new Group();
    const body = new Mesh(coinGeometry(radius, thickness), material);
    body.castShadow = true;
    const label = canvasTexture(renderer, 256, 256, (ctx, w, h, a = top, b = bottom) => drawCoin(ctx, w, h, a, b, ink));
    const faceMesh = roundFace(radius * 0.84, label.texture, thickness / 2 - thickness * 0.35 * 0.6 + 0.002);
    group.add(body, faceMesh);
    world.add(group);
    return { group, body, label, face: faceMesh, material };
  }

  // ---------------------------------------------------------------------------------------- the ledger

  /** A ledger row: a white tile with an event written on it. */
  function eventTile() {
    const group = new Group();
    const material = white();
    const body = new Mesh(rounded(1.38, 0.12, 0.62, 0.05), material);
    body.castShadow = true;
    body.receiveShadow = true;
    const text = canvasTexture(renderer, 512, 230, (ctx, w, h, name = "", fields = [], tone = "ink") =>
      drawEvent(ctx, w, h, name, fields, tone),
    );
    const top = face(1.3, 0.56, text.texture, 0.063);
    group.add(body, top);
    world.add(group);
    return { group, body, material, top, text, shown: "" };
  }

  // ------------------------------------------------------------------------------------------ signers

  function signerKey(n) {
    const group = new Group();
    const material = white();
    material.emissive = new Color(0xcfe0f2);
    material.emissiveIntensity = 0.12;
    const body = new Mesh(rounded(0.34, 0.34, 0.34, 0.07), material);
    body.castShadow = true;
    const label = canvasTexture(renderer, 128, 128, (ctx, w, h) => drawGlyph(ctx, w, h, String(n), "#0f1b2a", 800, 0.6));
    const top = face(0.28, 0.28, label.texture, 0.172);
    group.add(body, top);
    world.add(group);
    return { group, material };
  }

  // ------------------------------------------------------------------------------------------- the proof

  function proofToken() {
    const group = new Group();
    const material = gold();
    material.emissiveIntensity = 0.55;
    const body = new Mesh(rounded(0.56, 0.16, 0.56, 0.08), material);
    body.castShadow = true;
    const label = canvasTexture(renderer, 128, 128, (ctx, w, h) => drawGlyph(ctx, w, h, "π", "#3d2800", 700, 0.7));
    const top = face(0.46, 0.46, label.texture, 0.083);
    group.add(body, top);
    world.add(group);
    return { group, material };
  }

  // --------------------------------------------------------------------------------- the market and USDC

  function market() {
    const group = new Group();
    const ringMaterial = new MeshPhysicalMaterial({
      color: new Color(0x1d4fd8),
      metalness: 0.6,
      roughness: 0.25,
      clearcoat: 1,
      emissive: COLORS.usdc,
      emissiveIntensity: 0.6,
    });
    const ring = new Mesh(new TorusGeometry(0.78, 0.09, 24, 96), ringMaterial);
    ring.rotation.x = Math.PI / 2;
    ring.position.y = 0.09;
    ring.castShadow = true;
    const surface = canvasTexture(renderer, 512, 512, drawMarket);
    const pool = new Mesh(
      new CircleGeometry(0.72, 64).rotateX(-Math.PI / 2),
      new MeshPhysicalMaterial({ map: surface.texture, roughness: 0.08, clearcoat: 1, emissive: COLORS.usdc, emissiveIntensity: 0.12 }),
    );
    pool.position.y = 0.03;
    group.add(ring, pool);
    world.add(group);
    return { group, ringMaterial, pool };
  }

  // USDC: blue chips, a dollar sign on each face (a cylinder's caps take the face, its side the rim).
  const chipRim = new MeshPhysicalMaterial({
    color: new Color(0x2f6bff),
    metalness: 0.35,
    roughness: 0.25,
    clearcoat: 1,
    emissive: COLORS.usdc,
    emissiveIntensity: 0.3,
  });
  const chipFace = new MeshPhysicalMaterial({
    map: canvasTexture(renderer, 128, 128, drawChip).texture,
    roughness: 0.3,
    clearcoat: 1,
    emissive: COLORS.usdc,
    emissiveIntensity: 0.15,
  });
  const CHIPS = 40;
  const chips = new InstancedMesh(new CylinderGeometry(0.17, 0.17, 0.055, 40), [chipRim, chipFace, chipFace], CHIPS);
  chips.instanceMatrix.setUsage(DynamicDrawUsage);
  chips.castShadow = true;
  chips.frustumCulled = false;
  world.add(chips);

  function card() {
    const group = new Group();
    const material = white();
    const body = new Mesh(rounded(1.22, 0.12, 0.9, 0.06), material);
    body.castShadow = true;
    body.receiveShadow = true;
    const label = canvasTexture(renderer, 512, 384, (ctx, w, h) =>
      drawCard(ctx, w, h, "A fresh Safe", ["deployed by its first", "UserOp, gas sponsored", "ETH ever received: 0"]),
    );
    const top = face(1.12, 0.82, label.texture, 0.063);
    group.add(body, top);
    world.add(group);
    return { group, material };
  }

  function escrow() {
    const group = new Group();
    const material = new MeshPhysicalMaterial({
      color: new Color(0xdbe8ff),
      roughness: 0.05,
      metalness: 0,
      clearcoat: 1,
      transparent: true,
      opacity: 0.18,
      depthWrite: false,
    });
    const boxGeometry = rounded(0.96, 0.62, 0.96, 0.06);
    const box = new Mesh(boxGeometry, material);
    box.position.y = 0.31;
    const edges = new LineSegments(new EdgesGeometry(rounded(0.96, 0.62, 0.96, 0.06), 30), new LineBasicMaterial({ color: 0xe8f0ff, transparent: true, opacity: 0.85 }));
    edges.position.y = 0.31;
    const lid = new Mesh(rounded(1.02, 0.08, 1.02, 0.03), white());
    lid.position.y = 0.66;
    lid.castShadow = true;
    group.add(box, edges, lid);
    world.add(group);
    return { group, material, edges, lid };
  }

  // ------------------------------------------------------------------------------ the bonds and a theft

  function bond() {
    const group = new Group();
    const material = gold();
    material.color = new Color(0xc99512);
    const body = new Mesh(rounded(0.5, 0.18, 0.5, 0.05), material);
    body.castShadow = true;
    group.add(body);
    world.add(group);
    return { group, material };
  }

  const SHARDS = 80;
  const shardMaterial = gold();
  const shards = new InstancedMesh(new TetrahedronGeometry(0.12), shardMaterial, SHARDS);
  shards.instanceMatrix.setUsage(DynamicDrawUsage);
  shards.frustumCulled = false;
  world.add(shards);
  const shardSeeds = Array.from({ length: SHARDS }, () => ({
    vx: (rand() - 0.5) * 2.4,
    vy: 1.5 + rand() * 2.2,
    vz: (rand() - 0.5) * 2.4,
    spin: (rand() - 0.5) * 12,
    bond: Math.floor(rand() * 5),
  }));

  // ------------------------------------------------------------------------------------- lines and links

  /** A line between moving points: the signers' ring, the tethers of a theft. */
  function lines(segments, color, opacity) {
    const geometry = new BufferGeometry();
    geometry.setAttribute("position", new Float32BufferAttribute(new Float32Array(segments * 6), 3));
    geometry.attributes.position.setUsage(DynamicDrawUsage);
    const material = new LineBasicMaterial({ color, transparent: true, opacity, depthWrite: false });
    const object = new LineSegments(geometry, material);
    object.frustumCulled = false;
    world.add(object);
    return { object, material, positions: geometry.attributes.position };
  }

  /** A thread a bridge leaves: a red tube between two places, drawn on as it appears. */
  function thread(points) {
    const curve = new CatmullRomCurve3(points.map((p) => new Vector3(...p)));
    const geometry = new TubeGeometry(curve, 64, 0.028, 8, false);
    const material = new MeshPhysicalMaterial({ color: COLORS.red, emissive: COLORS.red, emissiveIntensity: 1.2, roughness: 0.4 });
    const mesh = new Mesh(geometry, material);
    mesh.geometry.setDrawRange(0, 0);
    world.add(mesh);
    return { mesh, total: geometry.index.count };
  }

  const plankMaterial = new MeshPhysicalMaterial({ color: new Color(0x7a2a24), roughness: 0.5, metalness: 0.2, emissive: COLORS.red, emissiveIntensity: 0.15 });
  const plank = new Mesh(rounded(2.1, 0.1, 0.8, 0.04), plankMaterial);
  plank.castShadow = true;
  world.add(plank);

  // ----------------------------------------------------------------------------------------- assemble

  const notes = Array.from({ length: 8 }, () => note());
  const vaultNote = note();
  const yourNote = note();
  const makerNote = note();
  const swapNote = note();
  const theftNote = note();
  for (const n of [vaultNote, yourNote, makerNote, swapNote, theftNote]) n.material.emissive = new Color(0x2a1e00);

  const zec = coin(gold(), 0.46, 0.14, "0.01", "ZEC", "rgba(74, 46, 0, 0.92)");
  const coinYou = coin(gold(), 0.36, 0.12, "you", "ZEC", "rgba(74, 46, 0, 0.92)");
  const coinMaker = coin(gold(), 0.36, 0.12, "maker", "ZEC", "rgba(74, 46, 0, 0.92)");
  const makerZec = coin(gold(), 0.4, 0.13, "0.01", "ZEC", "rgba(74, 46, 0, 0.92)");
  const zzecMaterial = white();
  const zzec = coin(zzecMaterial, 0.42, 0.13, "zZEC", "receipt", "rgba(122, 82, 0, 0.95)");
  const wzecMaterial = new MeshPhysicalMaterial({ color: new Color(0xffd1cc), roughness: 0.3, clearcoat: 1, emissive: COLORS.red, emissiveIntensity: 0.25 });
  const wzec = coin(wzecMaterial, 0.42, 0.13, "wZEC", "0x7a3e…41c2", "rgba(154, 31, 21, 0.95)");

  const keys = [1, 2, 3, 4, 5].map((n) => signerKey(n));
  const proof = proofToken();
  const events = Array.from({ length: 7 }, () => eventTile());
  const morpho = market();
  const account = card();
  const box = escrow();
  const bonds = Array.from({ length: 5 }, () => bond());
  const ring = lines(5, 0xdbe8ff, 0.5);
  const tethers = lines(3, 0xff4f45, 0.9);
  const threads = [
    thread([[-1.2, 0.1, 3.0], [0.0, 1.3, 3.2], [1.6, 1.0, 3.1], [3.2, 0.12, 2.6]]),
    thread([[3.2, 0.12, 2.6], [3.6, 0.9, 2.2], [3.95, 0.12, 1.3]]),
    thread([[3.9, 0.12, -1.4], [4.5, 1.1, 0.0], [3.95, 0.12, 1.3]]),
  ];
  const gasWallet = coin(wzecMaterial, 0.34, 0.11, "ETH", "0x51c0…9e1a", "rgba(154, 31, 21, 0.95)");

  return {
    notes,
    vaultNote,
    yourNote,
    makerNote,
    swapNote,
    theftNote,
    zec,
    coinYou,
    coinMaker,
    makerZec,
    zzec,
    wzec,
    gasWallet,
    keys,
    proof,
    events,
    morpho,
    chips,
    account,
    box,
    bonds,
    shards,
    shardSeeds,
    ring,
    tethers,
    threads,
    plank,
    dummy: new Object3D(),
  };
}
