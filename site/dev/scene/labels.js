// Text drawn onto the scene's things: the ciphertext of a shielded note, the readable events of Base's ledger, a
// coin's face. Each is a canvas the scene can redraw (ciphertext keeps changing), uploaded as a texture.
import { CanvasTexture, SRGBColorSpace } from "three";

export const SANS = "Archivo, 'Helvetica Neue', Arial, sans-serif";
export const MONO = "'Martian Mono', ui-monospace, Menlo, Consolas, monospace";
const HEX = "0123456789abcdef";

/** A canvas texture with its own draw function; `redraw(...args)` paints it again. */
export function canvasTexture(renderer, width, height, draw) {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  const texture = new CanvasTexture(canvas);
  texture.colorSpace = SRGBColorSpace;
  texture.anisotropy = Math.min(8, renderer.capabilities.getMaxAnisotropy());
  const redraw = (...args) => {
    ctx.clearRect(0, 0, width, height);
    draw(ctx, width, height, ...args);
    texture.needsUpdate = true;
  };
  redraw();
  return { texture, redraw };
}

/** Sets a font no larger than `size` at which `text` fits in `room` pixels. */
function fitFont(ctx, text, weight, size, family, room) {
  let px = size;
  ctx.font = `${weight} ${px}px ${family}`;
  while (px > 8 && ctx.measureText(text).width > room) {
    px -= 1;
    ctx.font = `${weight} ${px}px ${family}`;
  }
}

export function hex(rand, n) {
  let s = "";
  for (let i = 0; i < n; i += 1) s += HEX[Math.floor(rand() * 16)];
  return s;
}

/** A shielded note's face: rows of ciphertext, brighter where `glow` says. */
export function drawCipher(ctx, w, h, rand, color = "94, 211, 168", glow = 0) {
  const rows = 3;
  const size = Math.round(h * 0.17);
  ctx.font = `500 ${size}px ${MONO}`;
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";
  for (let r = 0; r < rows; r += 1) {
    const y = h * (0.26 + r * 0.24);
    const alpha = 0.55 + 0.35 * rand() + glow * 0.4;
    ctx.fillStyle = `rgba(${color}, ${Math.min(1, alpha)})`;
    ctx.fillText(`${hex(rand, 4)} ${hex(rand, 4)}`, w / 2, y);
  }
}

/** A ledger event: a mark of its kind, its name, then its fields, as a block explorer would print them. */
export function drawEvent(ctx, w, h, name, fields = [], tone = "ink") {
  const ink = tone === "leak" ? "#9a1f15" : tone === "gold" ? "#6b4700" : "#0f1b2a";
  const muted = tone === "leak" ? "#b4402f" : "#4c5d70";
  const mark = tone === "leak" ? "#ff4f45" : tone === "gold" ? "#f4b728" : "#3d7dff";
  ctx.fillStyle = mark;
  ctx.beginPath();
  ctx.roundRect(w * 0.045, h * 0.16, w * 0.022, h * 0.68, w * 0.011);
  ctx.fill();
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = ink;
  fitFont(ctx, name, 800, Math.round(h * 0.25), SANS, w * 0.84);
  ctx.fillText(name, w * 0.11, h * 0.4);
  ctx.fillStyle = muted;
  fields.slice(0, 2).forEach((line, i) => {
    fitFont(ctx, line, 500, Math.round(h * 0.135), MONO, w * 0.84);
    ctx.fillText(line, w * 0.11, h * (0.63 + i * 0.19));
  });
}

/** A coin's face, engraved. */
export function drawCoin(ctx, w, h, top, bottom, color = "rgba(74, 46, 0, 0.92)") {
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = color;
  ctx.font = `800 ${Math.round(h * 0.3)}px ${SANS}`;
  ctx.fillText(top, w / 2, h * 0.44);
  ctx.font = `700 ${Math.round(h * 0.15)}px ${SANS}`;
  ctx.fillText(bottom, w / 2, h * 0.7);
  ctx.strokeStyle = color;
  ctx.lineWidth = w * 0.018;
  ctx.beginPath();
  ctx.arc(w / 2, h / 2, w * 0.43, 0, Math.PI * 2);
  ctx.stroke();
}

/** One glyph, centred: π on a proof, a signer's number on its key. */
export function drawGlyph(ctx, w, h, glyph, color = "#0f1b2a", weight = 800, scale = 0.62) {
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = color;
  ctx.font = `${weight} ${Math.round(h * scale)}px ${glyph === "π" ? MONO : SANS}`;
  ctx.fillText(glyph, w / 2, h * 0.53);
}

/** A card: a title and its lines. */
export function drawCard(ctx, w, h, title, lines, color = "#0f1b2a", muted = "#4c5d70") {
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = color;
  fitFont(ctx, title, 800, Math.round(h * 0.13), SANS, w * 0.82);
  ctx.fillText(title, w * 0.1, h * 0.24);
  ctx.fillStyle = muted;
  lines.forEach((line, i) => {
    fitFont(ctx, line, 500, Math.round(h * 0.075), MONO, w * 0.82);
    ctx.fillText(line, w * 0.1, h * (0.42 + i * 0.12));
  });
}

/** The escrow's lid: a padlock, the order, and the salted hash of the buyer's address it is locked to. */
export function drawLock(ctx, w, h) {
  const ink = "#0f1b2a";
  ctx.strokeStyle = ink;
  ctx.fillStyle = ink;
  ctx.lineWidth = w * 0.024;
  ctx.beginPath();
  ctx.arc(w / 2, h * 0.21, w * 0.055, Math.PI, 0);
  ctx.stroke();
  ctx.beginPath();
  ctx.roundRect(w / 2 - w * 0.085, h * 0.2, w * 0.17, h * 0.13, w * 0.02);
  ctx.fill();
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = "#4c5d70";
  ctx.font = `600 ${Math.round(h * 0.07)}px ${MONO}`;
  ctx.fillText("ORDER 0x21660b10", w / 2, h * 0.47);
  ctx.fillStyle = ink;
  ctx.font = `700 ${Math.round(h * 0.074)}px ${MONO}`;
  ctx.fillText("keccak256(salt ||", w / 2, h * 0.63);
  ctx.fillText("your address)", w / 2, h * 0.75);
}

// The ZEC, USDC and Morpho marks, exactly as their owners publish them: the Zcash brandmark from the Zcash brand
// folder ZecHub keeps (zechub.wiki/zcash-organizations/brand), the USDC token logo from Circle's press kit
// (circle.com/en/pressroom) and Morpho's symbol, dark-mode version, from brand.morpho.org. Each is drawn unaltered
// where the scene names the asset or the protocol; the marks are their owners' trademarks.
const MARKS = {
  zec: { box: [270, 270, 540, 540], rule: "evenodd", paths: [["m270,540c0-148.9,121.1-270,270-270s270,121.1,270,270-121.1,270-270,270-270-121.1-270-270Zm366.31-125.3v41.09l-114.28,155h114.28v54.5h-73.67v45.16h-45.28v-45.16h-73.67v-41.09l114.16-155h-114.16v-54.5h73.67v-45.28h45.28v45.28h73.67Z", null, 1]] },
  usdc: {
    box: [1, 1, 94, 94],
    rule: "nonzero",
    paths: [
      ["M48 95C73.9574 95 95 73.9574 95 48C95 22.0426 73.9574 1 48 1C22.0426 1 1 22.0426 1 48C1 73.9574 22.0426 95 48 95Z", "#0B53BF", 1],
      ["M56.4609 13.7778V19.8291C68.5341 23.4716 77.3759 34.6928 77.3759 47.9997C77.3759 61.3066 68.5341 72.5278 56.4609 76.1703V82.2216C71.8534 78.4616 83.2509 64.5672 83.2509 47.9997C83.2509 31.4322 71.8534 17.5378 56.4609 13.7778Z", "#FFFFFF", 1],
      ["M18.625 47.9997C18.625 34.6928 27.4669 23.4716 39.54 19.8291V13.7778C24.1475 17.5378 12.75 31.4322 12.75 47.9997C12.75 64.5672 24.1475 78.4616 39.54 82.2216V76.1703C27.4669 72.5572 18.625 61.3066 18.625 47.9997Z", "#FFFFFF", 1],
      ["M60.6319 54.5506C60.6319 42.5362 41.8025 47.4713 41.8025 40.8325C41.8025 38.4531 43.7119 36.9256 47.3544 36.9256C51.7019 36.9256 53.2 39.0406 53.67 41.89H59.6625C59.1279 36.5426 56.0588 33.1662 50.9382 32.1604V27.4375H45.0632V31.9918C39.4534 32.7062 35.9275 35.973 35.9275 40.8325C35.9275 52.9056 54.7863 48.3819 54.7863 54.9031C54.7863 57.3706 52.4069 59.0156 48.3825 59.0156C43.1244 59.0156 41.3913 56.695 40.745 53.4931H34.8994C35.2781 59.3502 38.8897 63.0159 45.0632 63.9307V68.5625H50.9382V63.9923C56.9633 63.2139 60.6319 59.7089 60.6319 54.5506Z", "#FFFFFF", 1],
    ],
  },
  morpho: {
    box: [0, 0, 74, 69],
    rule: "nonzero",
    paths: [
      ["M8.64066 46.6257V66.4908C8.64066 67.7137 9.67561 68.2213 9.99759 68.3366C10.3196 68.4751 11.4005 68.775 12.3664 67.8752L27.3944 53.4333C28.6742 52.2036 29.9093 50.9109 30.8375 49.3984C31.2744 48.6874 31.4555 48.2869 31.4555 48.2869C32.3755 46.418 32.3755 44.6184 31.4782 42.8187C30.1448 40.1424 26.7407 37.4198 21.6118 34.8357L12.8494 39.7271C10.2506 41.2037 8.64066 43.8109 8.64066 46.6257Z", "#ffffff", 0.8],
      ["M0 2.44212V23.2764C0 25.8835 1.74794 28.1907 4.23184 28.929C12.6954 31.3747 27.4375 36.6351 31.0024 44.3644C31.462 45.3795 31.7381 46.3717 31.8307 47.4099C34.1994 43.0954 35.2803 38.1348 34.7744 33.1051C34.0841 25.9758 30.3122 19.4925 24.4248 15.3626L3.42686 0.66555C3.05883 0.388682 2.62187 0.250244 2.1849 0.250244C1.81688 0.250244 1.4949 0.319463 1.14994 0.504042C0.459945 0.896269 0 1.61152 0 2.44212Z", "#ffffff", 1],
      ["M64.5352 46.6257V66.4908C64.5352 67.7137 63.5006 68.2213 63.1782 68.3366C62.8567 68.4751 61.775 68.775 60.8095 67.8752L45.4317 53.0975C44.3849 52.0913 43.3853 51.0273 42.5968 49.808C41.9723 48.843 41.7206 48.2869 41.7206 48.2869C40.8005 46.418 40.8005 44.6184 41.697 42.8187C43.0312 40.1424 46.4354 37.4198 51.5635 34.8357L60.3263 39.7271C62.9484 41.2037 64.5352 43.8109 64.5352 46.6257Z", "#ffffff", 0.8],
      ["M73.19 2.44065V23.2749C73.19 25.882 71.4416 28.1892 68.9576 28.9276C60.4944 31.3732 45.7524 36.6337 42.1875 44.3629C41.7271 45.3781 41.451 46.3702 41.3592 47.4085C38.9905 43.094 37.9096 38.1334 38.4155 33.1037C39.1049 25.9743 42.8769 19.4911 48.7651 15.3611L69.7631 0.664077C70.131 0.387209 70.5679 0.248779 71.0047 0.248779C71.3726 0.248779 71.695 0.317998 72.0393 0.502577C72.7295 0.894804 73.19 1.61005 73.19 2.44065Z", "#ffffff", 1],
    ],
  },
};
const built = new Map();

/** A mark, `size` wide and centred on (x, y), in its own colours, or all of it in `color` (the ZEC brandmark's). */
export function drawMark(ctx, name, x, y, size, color = "#000000") {
  const mark = MARKS[name];
  if (!built.has(name)) built.set(name, mark.paths.map(([d]) => new Path2D(d)));
  const [bx, by, bw, bh] = mark.box;
  const k = size / bw;
  ctx.save();
  ctx.translate(x - (bx + bw / 2) * k, y - (by + bh / 2) * k);
  ctx.scale(k, k);
  built.get(name).forEach((path, i) => {
    const [, fill, alpha] = mark.paths[i];
    ctx.globalAlpha = alpha ?? 1;
    ctx.fillStyle = fill ?? color;
    ctx.fill(path, mark.rule);
  });
  ctx.restore();
}

/** A ZEC coin's face: the Zcash brandmark in black, so the coin's gold shows through its Z. */
export function drawZecCoin(ctx, w, h) {
  drawMark(ctx, "zec", w / 2, h / 2, w * 0.86, "#050505");
}

/** A USDC chip's face: the USDC token logo, edge to edge (a cylinder's cap shows the circle inscribed in it). */
export function drawChip(ctx, w, h) {
  ctx.fillStyle = "#0B53BF";
  ctx.fillRect(0, 0, w, h);
  drawMark(ctx, "usdc", w / 2, h / 2, w);
}

/** A lending market's surface: deep water in rings, Morpho's symbol at its centre, its pair around the rim. */
export function drawMarket(ctx, w, h) {
  const g = ctx.createRadialGradient(w / 2, h / 2, 0, w / 2, h / 2, w / 2);
  g.addColorStop(0, "#1f56e8");
  g.addColorStop(0.7, "#0b2c8c");
  g.addColorStop(1, "#061a5c");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  ctx.strokeStyle = "rgba(160, 196, 255, 0.18)";
  ctx.lineWidth = 2;
  for (let r = 0.3; r < 0.4; r += 0.05) {
    ctx.beginPath();
    ctx.arc(w / 2, h / 2, w * r, 0, Math.PI * 2);
    ctx.stroke();
  }
  drawMark(ctx, "morpho", w / 2, h * 0.5, w * 0.36, "#ffffff");
  ctx.fillStyle = "rgba(220, 234, 255, 0.85)";
  ctx.font = `700 ${Math.round(h * 0.05)}px ${MONO}`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  const text = "zZEC / USDC · MORPHO BLUE · ";
  const chars = [...(text + text)];
  chars.forEach((c, i) => {
    const a = (i / chars.length) * Math.PI * 2;
    ctx.save();
    ctx.translate(w / 2 + Math.cos(a) * w * 0.44, h / 2 + Math.sin(a) * h * 0.44);
    ctx.rotate(a + Math.PI / 2);
    ctx.fillText(c, 0, 0);
    ctx.restore();
  });
}

/** The pool's surface: ciphertext as far as one can see. */
export function drawCipherField(ctx, w, h, rand) {
  const size = Math.round(w / 78);
  ctx.font = `500 ${size}px ${MONO}`;
  ctx.textBaseline = "middle";
  for (let y = size; y < h; y += size * 1.9) {
    let x = (rand() * size * 3) | 0;
    while (x < w) {
      const word = hex(rand, 4);
      ctx.fillStyle = `rgba(94, 211, 168, ${0.035 + rand() * 0.09})`;
      ctx.fillText(word, x, y);
      x += size * 3.4;
    }
  }
}

/** The ledger's surface: ruled rows and columns, like a block explorer's table. */
export function drawLedgerField(ctx, w, h) {
  ctx.strokeStyle = "rgba(15, 27, 42, 0.12)";
  ctx.lineWidth = 2;
  for (let y = 0; y <= h; y += h / 24) {
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }
  ctx.strokeStyle = "rgba(15, 27, 42, 0.06)";
  for (let x = 0; x <= w; x += w / 12) {
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, h);
    ctx.stroke();
  }
}
