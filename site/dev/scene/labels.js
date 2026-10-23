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
  ctx.font = `800 ${Math.round(h * 0.25)}px ${SANS}`;
  ctx.fillText(name, w * 0.11, h * 0.4);
  ctx.fillStyle = muted;
  ctx.font = `500 ${Math.round(h * 0.135)}px ${MONO}`;
  fields.slice(0, 2).forEach((line, i) => ctx.fillText(line, w * 0.11, h * (0.63 + i * 0.19)));
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

/** A card: a title and two lines. */
export function drawCard(ctx, w, h, title, lines, color = "#0f1b2a") {
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = color;
  ctx.font = `800 ${Math.round(h * 0.13)}px ${SANS}`;
  ctx.fillText(title, w * 0.1, h * 0.24);
  ctx.fillStyle = "#4c5d70";
  ctx.font = `500 ${Math.round(h * 0.075)}px ${MONO}`;
  lines.forEach((line, i) => ctx.fillText(line, w * 0.1, h * (0.42 + i * 0.12)));
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

/** A USDC chip's face: a ring and a dollar sign. */
export function drawChip(ctx, w, h) {
  ctx.fillStyle = "#2f6bff";
  ctx.fillRect(0, 0, w, h);
  ctx.strokeStyle = "rgba(255, 255, 255, 0.92)";
  ctx.lineWidth = w * 0.06;
  ctx.beginPath();
  ctx.arc(w / 2, h / 2, w * 0.34, 0, Math.PI * 2);
  ctx.stroke();
  ctx.fillStyle = "#ffffff";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.font = `800 ${Math.round(h * 0.42)}px ${SANS}`;
  ctx.fillText("$", w / 2, h * 0.52);
}

/** A lending market's surface: deep water in rings, its pair written around the rim. */
export function drawMarket(ctx, w, h) {
  const g = ctx.createRadialGradient(w / 2, h / 2, 0, w / 2, h / 2, w / 2);
  g.addColorStop(0, "#1f56e8");
  g.addColorStop(0.7, "#0b2c8c");
  g.addColorStop(1, "#061a5c");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  ctx.strokeStyle = "rgba(160, 196, 255, 0.22)";
  ctx.lineWidth = 2;
  for (let r = 0.12; r < 0.4; r += 0.07) {
    ctx.beginPath();
    ctx.arc(w / 2, h / 2, w * r, 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.fillStyle = "rgba(220, 234, 255, 0.85)";
  ctx.font = `700 ${Math.round(h * 0.052)}px ${MONO}`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  const text = "MORPHO BLUE · zZEC / USDC · ";
  const chars = [...(text + text)];
  chars.forEach((c, i) => {
    const a = (i / chars.length) * Math.PI * 2;
    ctx.save();
    ctx.translate(w / 2 + Math.cos(a) * w * 0.43, h / 2 + Math.sin(a) * h * 0.43);
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
