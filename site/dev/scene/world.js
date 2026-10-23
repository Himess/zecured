// The world the story happens on: Zecured's mark made solid. A disc split by a seam, its left half Zcash's
// shielded pool (dark glass, ciphertext under the surface), its right half Base's public ledger (white, ruled), and
// at the centre of the seam a gold core, the one place where anything crosses: proofs.
import {
  AdditiveBlending,
  BackSide,
  BoxGeometry,
  CircleGeometry,
  Color,
  DirectionalLight,
  ExtrudeGeometry,
  Group,
  HemisphereLight,
  Mesh,
  MeshBasicMaterial,
  MeshPhysicalMaterial,
  PlaneGeometry,
  PMREMGenerator,
  PointLight,
  RepeatWrapping,
  Shape,
  ShaderMaterial,
  ShadowMaterial,
  SphereGeometry,
} from "three";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { canvasTexture, drawCipherField, drawLedgerField } from "./labels.js";

export const R = 5.2;
export const GAP = 0.44;
export const THICK = 0.46;

export const COLORS = {
  night: new Color(0x040907),
  obsidian: new Color(0x0a1712),
  porcelain: new Color(0xeef2f6),
  gold: new Color(0xf4b728),
  goldDeep: new Color(0x6b4a00),
  teal: new Color(0x38a684),
  ice: new Color(0xcfe0f2),
  ink: new Color(0x0f1b2a),
  red: new Color(0xff4f45),
  usdc: new Color(0x3d7dff),
};

/** One half of the disc: the part of a circle of radius R beyond the seam, with a bevelled edge. */
function halfGeometry(side) {
  const h = Math.sqrt(R * R - (GAP / 2) ** 2);
  const x0 = (side * GAP) / 2;
  const shape = new Shape();
  shape.moveTo(x0, -h);
  shape.lineTo(x0, h);
  const a1 = Math.atan2(h, x0);
  const a2 = Math.atan2(-h, x0);
  shape.absarc(0, 0, R, a1, a2, side > 0);
  const bevel = 0.09;
  const geometry = new ExtrudeGeometry(shape, {
    depth: THICK - bevel * 2,
    bevelEnabled: true,
    bevelThickness: bevel,
    bevelSize: bevel,
    bevelSegments: 6,
    curveSegments: 120,
  });
  // The shape lies in x/y: turn it so its top face looks up, at y = 0.
  geometry.rotateX(-Math.PI / 2);
  geometry.translate(0, -(THICK - bevel), 0);
  return geometry;
}

/** A top layer for a half, inside its bevel. */
function topLayer(side, map, opacity) {
  const geometry = new CircleGeometry(R - 0.5, 96, side < 0 ? Math.PI / 2 : -Math.PI / 2, Math.PI);
  geometry.rotateX(-Math.PI / 2);
  geometry.translate(side * (GAP / 2 + 0.16), 0.004, 0);
  const material = new MeshBasicMaterial({ map, transparent: true, opacity, depthWrite: false });
  return new Mesh(geometry, material);
}

export function buildWorld(renderer, scene, { small }) {
  // Studio light for reflections, from three's room environment.
  const pmrem = new PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.4;
  pmrem.dispose();

  // The night around it.
  const sky = new Mesh(
    new SphereGeometry(120, 32, 16),
    new ShaderMaterial({
      side: BackSide,
      depthWrite: false,
      uniforms: {
        uTop: { value: new Color(0x020403) },
        uMid: { value: new Color(0x061410) },
        uLow: { value: new Color(0x010202) },
      },
      vertexShader: /* glsl */ `
        varying vec3 vDir;
        void main() {
          vDir = normalize(position);
          gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
        }`,
      fragmentShader: /* glsl */ `
        uniform vec3 uTop;
        uniform vec3 uMid;
        uniform vec3 uLow;
        varying vec3 vDir;
        void main() {
          float y = vDir.y;
          vec3 col = mix(uMid, uTop, smoothstep(0.0, 0.7, y));
          col = mix(col, uLow, smoothstep(0.0, -0.6, y));
          gl_FragColor = vec4(col, 1.0);
        }`,
    }),
  );
  sky.frustumCulled = false;
  scene.add(sky);

  // Lights: a key from above, a cool fill, a teal rim on the pool's side and a white one on the ledger's.
  const key = new DirectionalLight(0xfff4e2, 1.5);
  key.position.set(-6, 14, 9);
  key.castShadow = true;
  key.shadow.mapSize.set(small ? 1024 : 2048, small ? 1024 : 2048);
  key.shadow.camera.left = -9;
  key.shadow.camera.right = 9;
  key.shadow.camera.top = 9;
  key.shadow.camera.bottom = -9;
  key.shadow.camera.near = 1;
  key.shadow.camera.far = 40;
  key.shadow.bias = -0.0004;
  key.shadow.radius = 5;
  scene.add(key);
  scene.add(new HemisphereLight(0xbfd4ff, 0x0a1410, 0.3));
  const rimZ = new PointLight(0x38c99a, 16, 22, 2);
  rimZ.position.set(-8, 3, -6);
  scene.add(rimZ);
  const rimB = new PointLight(0xdbe8ff, 8, 22, 2);
  rimB.position.set(8, 4, -6);
  scene.add(rimB);

  // A shadow on nothing, so the disc floats.
  const ground = new Mesh(new PlaneGeometry(60, 60), new ShadowMaterial({ opacity: 0.42 }));
  ground.rotation.x = -Math.PI / 2;
  ground.position.y = -1.6;
  ground.receiveShadow = true;
  scene.add(ground);

  // A soft light pooled under the disc.
  const halo = new Mesh(
    new PlaneGeometry(26, 26),
    new ShaderMaterial({
      transparent: true,
      depthWrite: false,
      blending: AdditiveBlending,
      uniforms: { uColor: { value: new Color(0x0d3a2b) } },
      vertexShader: /* glsl */ `
        varying vec2 vUv;
        void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
      fragmentShader: /* glsl */ `
        uniform vec3 uColor;
        varying vec2 vUv;
        void main() {
          float d = length(vUv - 0.5) * 2.0;
          gl_FragColor = vec4(uColor * pow(max(0.0, 1.0 - d), 2.2), 1.0);
        }`,
    }),
  );
  halo.rotation.x = -Math.PI / 2;
  halo.position.y = -1.58;
  scene.add(halo);

  // The disc.
  const world = new Group();
  scene.add(world);
  const obsidian = new MeshPhysicalMaterial({
    color: COLORS.obsidian,
    metalness: 0.1,
    roughness: 0.22,
    clearcoat: 1,
    clearcoatRoughness: 0.06,
    envMapIntensity: 1.1,
  });
  const porcelain = new MeshPhysicalMaterial({
    color: new Color(0xd9dfe6),
    metalness: 0,
    roughness: 0.42,
    clearcoat: 0.7,
    clearcoatRoughness: 0.2,
  });
  const zcash = new Mesh(halfGeometry(-1), obsidian);
  const base = new Mesh(halfGeometry(1), porcelain);
  for (const half of [zcash, base]) {
    half.castShadow = true;
    half.receiveShadow = true;
    world.add(half);
  }

  // Under the pool's glass, ciphertext; on the ledger, its rules.
  let seed = 11;
  const rand = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
  const cipherField = canvasTexture(renderer, 1024, 1024, (ctx, w, h) => drawCipherField(ctx, w, h, rand));
  cipherField.texture.wrapS = RepeatWrapping;
  cipherField.texture.wrapT = RepeatWrapping;
  const ledgerField = canvasTexture(renderer, 1024, 1024, (ctx, w, h) => drawLedgerField(ctx, w, h));
  const poolTop = topLayer(-1, cipherField.texture, 1);
  const ledgerTop = topLayer(1, ledgerField.texture, 1);
  world.add(poolTop, ledgerTop);

  // The seam: a thread of gold light down the gap between the halves, brighter whenever a proof crosses.
  const seamMaterial = new MeshBasicMaterial({ color: new Color(2.2, 1.45, 0.35), transparent: true, opacity: 0.4, depthWrite: false });
  const seam = new Mesh(new BoxGeometry(0.035, 0.035, 2 * Math.sqrt(R * R - (GAP / 2) ** 2) - 0.4), seamMaterial);
  seam.position.y = -0.12;
  world.add(seam);

  // The core, where proofs cross.
  const goldMaterial = new MeshPhysicalMaterial({
    color: COLORS.gold,
    metalness: 1,
    roughness: 0.2,
    clearcoat: 0.5,
    clearcoatRoughness: 0.1,
    emissive: COLORS.gold,
    emissiveIntensity: 0.25,
    envMapIntensity: 1.4,
  });
  const core = new Mesh(new SphereGeometry(0.5, 64, 32), goldMaterial);
  core.position.set(0, 0.02, 0);
  core.castShadow = true;
  world.add(core);
  const coreLight = new PointLight(0xffc04d, 12, 9, 2);
  coreLight.position.set(0, 0.8, 0);
  world.add(coreLight);

  return { world, zcash, base, core, coreLight, goldMaterial, seamMaterial, cipherField, key, halo, rand };
}
