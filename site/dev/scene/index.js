// The landing page's scene (bundled into site/scene.js by site/dev/build.mjs). Zecured's mark made solid: a disc
// split by a seam, Zcash's shielded pool on one half, Base's ledger on the other, a gold core where proofs cross.
// The page's scroll position is story time: main.js tells the scene which chapter is on screen and how far into it
// the reader is, and every object takes the state the story gives it at that moment (story.js), the camera too
// (camera.js).
import { NeutralToneMapping, PCFShadowMap, PerspectiveCamera, Scene, Vector2, Vector3, WebGLRenderer } from "three";
import { EffectComposer } from "three/addons/postprocessing/EffectComposer.js";
import { OutputPass } from "three/addons/postprocessing/OutputPass.js";
import { RenderPass } from "three/addons/postprocessing/RenderPass.js";
import { UnrealBloomPass } from "three/addons/postprocessing/UnrealBloomPass.js";
import { TRACK, boxAt, follow, place, pose, poseAt } from "./camera.js";
import { buildProps } from "./props.js";
import { CHAPTERS, createStory, seg } from "./story.js";
import { buildWorld } from "./world.js";

export function startScene(canvas, { tags: tagLayer = null, onReady, onLost } = {}) {
  let renderer;
  try {
    // No antialiasing on the canvas: the scene is drawn into the composer's own target and only copied onto the
    // canvas, where multisampling would smooth nothing. Its edges are smoothed by drawing it a little larger than
    // the screen instead (below), which costs far less memory than a multisampled float target.
    renderer = new WebGLRenderer({ canvas, antialias: false, powerPreference: "default" });
  } catch {
    return null;
  }
  if (!renderer.getContext()) return null;
  // Shader logs are for development: on Windows, Direct3D's compiler warns about constants in three's own shaders.
  renderer.debug.checkShaderErrors = false;
  renderer.toneMapping = NeutralToneMapping;
  renderer.toneMappingExposure = 1.05;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = PCFShadowMap;

  const small = Math.min(window.innerWidth, window.innerHeight) < 700;
  // A desktop screen gets at least 1.25 pixels a pixel (that is the antialiasing) and at most 1.5, a phone at most
  // 1.5; the frame-rate keeper below steps it down if frames run slow.
  let pixelRatio = small ? Math.min(window.devicePixelRatio || 1, 1.5) : Math.min(Math.max(window.devicePixelRatio || 1, 1.25), 1.5);
  renderer.setPixelRatio(pixelRatio);

  const scene = new Scene();
  const camera = new PerspectiveCamera(32, 1, 0.1, 300);
  const parts = buildWorld(renderer, scene, { small });
  const props = buildProps(renderer, parts.world, parts.rand);
  const story = createStory(parts, props);

  const composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  // Only what is brighter than white glows: a proof, the seam while one crosses, the core's flash.
  const bloom = new UnrealBloomPass(new Vector2(256, 256), 0.38, 0.55, 1.15);
  composer.addPass(bloom);
  composer.addPass(new OutputPass());

  // The page's tags that follow things in the scene.
  const tagEls = new Map();
  tagLayer?.querySelectorAll("[data-tag]").forEach((el) =>
    tagEls.set(el.dataset.tag, { el, label: el.firstElementChild, width: 0, on: 0, shown: false, below: false }),
  );

  // ----------------------------------------------------------------------------------------- the state

  let width = 1;
  let height = 1;
  let tall = false;
  let target = 0;
  let storyTime = 0;
  let edge = 0;
  let edgeShown = 0;
  const goal = pose();
  const view = pose();
  const pointer = { x: 0, y: 0, sx: 0, sy: 0 };
  const scratch = new Vector3();
  const probe = new PerspectiveCamera(32, 1, 0.1, 300);
  const box = [0, 0, 0, 0, 0];

  /**
   * Sets the page's place: the chapter on screen, how far into it the reader is, and where its text ends (a share of
   * the screen: the right edge on a wide screen, the top on a tall one; 0 if the text runs across).
   */
  function setScroll(name, p, textEdge = 0) {
    const index = Math.max(0, CHAPTERS.indexOf(name));
    target = index + Math.min(0.999, Math.max(0, p));
    edge = textEdge;
  }

  /**
   * The track is drawn for a 1440 by 900 screen. Elsewhere the story stands in the middle of the space the text
   * leaves, and the camera steps back as far as that space is narrower.
   */
  function fit(p) {
    if (edgeShown <= 0.02) return;
    if (tall) {
      // Above the text, unless the track sinks the mark out of the way (the demo): then it goes down with it.
      const sink = Math.min(1, Math.max(0, (0.1 - p.y) / 1.05));
      p.y = Math.min(0.32, Math.max(0.1, 0.5 - (0.07 + edgeShown) / 2)) * (1 - sink) + p.y * sink;
    } else {
      p.x = edgeShown / 2;
      p.dist *= Math.min(1.4, Math.max(0.85, 0.933 / ((1 - edgeShown) * (width / height))));
    }
    keepInView(p);
  }

  /**
   * Then the whole of what the story is telling must be on screen with room to spare, clear of the header and of
   * the text: the camera centres it in the free space and steps back until it fits.
   */
  function keepInView(p) {
    if (!boxAt(storyTime, box)) return;
    const left = tall ? 0.04 : edgeShown + 0.03;
    const right = 0.97;
    const top = tall ? 0.1 : 0.13;
    const bottom = tall ? edgeShown - 0.02 : 0.94;
    // Where the text covers nearly all the screen, the veil does the work.
    if (bottom - top < 0.22 || right - left < 0.3) return;
    probe.fov = camera.fov;
    probe.aspect = camera.aspect;
    const matrix = parts.world.matrixWorld;
    for (let i = 0; i < 8; i += 1) {
      place(probe, p, width, height);
      probe.updateMatrixWorld();
      let x0 = 1;
      let x1 = 0;
      let y0 = 1;
      let y1 = 0;
      for (let c = 0; c < 8; c += 1) {
        scratch.set(c & 1 ? box[2] : box[0], c & 2 ? box[4] : 0, c & 4 ? box[3] : box[1]).applyMatrix4(matrix).project(probe);
        const sx = (scratch.x + 1) / 2;
        const sy = (1 - scratch.y) / 2;
        x0 = Math.min(x0, sx);
        x1 = Math.max(x1, sx);
        y0 = Math.min(y0, sy);
        y1 = Math.max(y1, sy);
      }
      p.x += (left + right) / 2 - (x0 + x1) / 2;
      p.y -= (top + bottom) / 2 - (y0 + y1) / 2;
      const over = Math.max((x1 - x0) / (right - left), (y1 - y0) / (bottom - top));
      // The first screen's mark may also come closer, until it fills most of the space the headline leaves.
      const fill = storyTime < 0.8 && over < 0.84;
      if (over <= 1 && !fill) break;
      p.dist *= fill ? Math.max(0.88, over / 0.88) : over * 1.02;
    }
  }

  function resize() {
    width = Math.max(1, window.innerWidth);
    height = Math.max(1, window.innerHeight);
    tall = width / height < 0.9;
    renderer.setSize(width, height, false);
    composer.setPixelRatio(pixelRatio);
    composer.setSize(width, height);
    camera.aspect = width / height;
    camera.fov = tall ? 42 : 32;
    for (const entry of tagEls.values()) entry.width = 0;
  }

  function setPixelRatio(value) {
    pixelRatio = value;
    renderer.setPixelRatio(value);
    resize();
  }

  let lastScramble = 0;

  function update(time, dt, snap = false) {
    // Story time eases toward the scroll; a long jump (a link) skips most of the way first.
    if (snap) storyTime = target;
    else {
      if (Math.abs(target - storyTime) > 1.2) storyTime = target - Math.sign(target - storyTime) * 0.3;
      storyTime += (target - storyTime) * (1 - Math.exp(-dt * 4.5));
    }
    // The mark turns gently while nothing is being told.
    const idle = 1 - seg(storyTime, 0.4, 0.95) + seg(storyTime, 8.05, 8.6);
    parts.world.rotation.y = Math.sin(time * 0.11) * 0.32 * idle;
    parts.world.updateMatrixWorld();

    // The camera glides after the story's, and leans a little toward the pointer. Once, as the page opens, it swings
    // in a little toward the mark while the core lights up (gently: the still image it replaces shows the end pose).
    edgeShown = snap ? edge : edgeShown + (edge - edgeShown) * (1 - Math.exp(-dt * 3));
    poseAt(tall ? TRACK.tall : TRACK.wide, storyTime, goal);
    fit(goal);
    follow(view, goal, snap ? 1 : 1 - Math.exp(-dt * 2.4));
    pointer.sx += (pointer.x - pointer.sx) * (1 - Math.exp(-dt * 3));
    pointer.sy += (pointer.y - pointer.sy) * (1 - Math.exp(-dt * 3));
    const opening = (1 - Math.min(1, time / 2.8)) ** 3;
    place(camera, view, width, height, pointer.sx * 2.5 - opening * 10, -pointer.sy * 1.5 + opening * 5, 1 + opening * 0.12);
    camera.updateMatrixWorld();

    // Ciphertext never sits still.
    parts.cipherField.texture.offset.y = (time * 0.006) % 1;
    if (time - lastScramble > 0.14) {
      lastScramble = time;
      const pool = [...props.notes, props.vaultNote, props.yourNote, props.makerNote, props.swapNote, props.theftNote];
      const note = pool[Math.floor(Math.random() * pool.length)];
      if (note.group.visible) note.cipher.redraw(0.2);
    }

    const anchors = story(storyTime, time);
    parts.coreLight.intensity *= 1 - opening;
    parts.goldMaterial.emissiveIntensity *= 1 - opening * 0.8;

    // Tags: placed on the things they name, faded with the story. Where the text fills nearly all of a tall screen,
    // there is nothing left for a tag to point at.
    const crowded = tall && edgeShown > 0.02 && edgeShown < 0.24;
    for (const [name, entry] of tagEls) {
      const spec = crowded ? null : anchors[name];
      const goalOn = spec ? spec.on : 0;
      entry.on = snap ? goalOn : entry.on + (goalOn - entry.on) * (1 - Math.exp(-dt * 8));
      if (spec) {
        scratch.copy(spec.at).applyMatrix4(parts.world.matrixWorld).project(camera);
        const x = (scratch.x * 0.5 + 0.5) * width;
        const y = (-scratch.y * 0.5 + 0.5) * height;
        // A name never lies over the chapter's text: past its edge it waits until its thing is clear of it.
        const covered = edgeShown > 0.02 && (tall ? y > edgeShown * height - 8 : x < edgeShown * width + 8);
        const visible = scratch.z < 1 && Math.abs(scratch.x) < 1.05 && Math.abs(scratch.y) < 1.05 && !covered;
        if (visible) {
          entry.el.style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0)`;
          // Near the top of the screen a name hangs below its thing, clear of the page's header.
          const below = y < 130;
          if (below !== entry.below) {
            entry.below = below;
            entry.el.classList.toggle("below", below);
          }
          if (entry.label) {
            if (!entry.width) entry.width = entry.label.offsetWidth;
            const half = entry.width / 2;
            // A name stays clear of the chapter's text column, as its thing does.
            const left = !tall && edgeShown > 0.02 ? edgeShown * width + 12 : 12;
            const fix = x - half < left ? left - (x - half) : x + half > width - 12 ? width - 12 - (x + half) : 0;
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
    const dt = Math.min(0.1, last ? (ms - last) / 1000 : 0.016);
    last = ms;
    update((ms - start) / 1000, dt);
    // A window with no size (one being prerendered, or hidden) has nothing to draw into.
    if (width > 16 && height > 16) composer.render(dt);
    // Keep the frame rate: while frames run slow, drop the pixel ratio, then the glow, then the shadows.
    frames.push(dt);
    if (frames.length >= 45) {
      frames.sort((a, b) => a - b);
      const median = frames[22];
      frames.length = 0;
      if (median > 0.026) {
        if (pixelRatio > 1) setPixelRatio(Math.max(1, pixelRatio - 0.25));
        else if (bloom.enabled) bloom.enabled = false;
        else parts.key.castShadow = false;
      }
    }
    requestAnimationFrame(frame);
  }

  function play() {
    if (running || lost) return;
    running = true;
    last = 0;
    requestAnimationFrame(frame);
  }

  function pause() {
    running = false;
  }

  /** One frame of the story at a chapter and a progress through it, the same every time (the page's still image is one). */
  function still(at = 4, name = "hero", p = 0, textEdge = edge) {
    pause();
    start = performance.now() - at * 1000;
    setScroll(name, p, textEdge);
    update(at, 0.016, true);
    if (width > 16 && height > 16) composer.render(0.016);
  }

  window.addEventListener("resize", resize);
  window.addEventListener(
    "pointermove",
    (e) => {
      pointer.x = (e.clientX / window.innerWidth) * 2 - 1;
      pointer.y = (e.clientY / window.innerHeight) * 2 - 1;
    },
    { passive: true },
  );
  document.addEventListener("visibilitychange", () => (document.hidden ? pause() : play()));
  // If the GPU takes the context away (a driver reset, too many tabs), the scene stops and the page shows its still.
  let lost = false;
  canvas.addEventListener("webglcontextlost", (e) => {
    e.preventDefault();
    lost = true;
    pause();
    onLost?.();
  });

  resize();
  update(0, 0.016, true);
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
