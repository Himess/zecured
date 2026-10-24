// The story, as a function of where the reader is. Each chapter of the page is one unit of story time
// (hero 0, deposit 1, prove 2 ...), so `s` = chapter + progress through it. Every object's state is computed from s
// alone, which makes the story scrub both ways: scroll back and the proof goes back into the vault.
import { Color, Vector3 } from "three";
import { COLORS } from "./world.js";

export const CHAPTERS = ["hero", "deposit", "prove", "borrow", "close", "swap", "privacy", "trust", "numbers", "demo", "end"];

const v = (x, y, z) => new Vector3(x, y, z);
const clamp01 = (x) => Math.min(1, Math.max(0, x));
/** 0 before a, 1 after b, eased between. */
export const seg = (s, a, b) => {
  const t = clamp01((s - a) / (b - a));
  return t * t * (3 - 2 * t);
};
/** 0 → 1 over [a, b], held, then 1 → 0 over [c, d]. */
const win = (s, a, b, c, d) => seg(s, a, b) * (1 - seg(s, c, d));
const mix = (a, b, t) => a + (b - a) * t;

// ------------------------------------------------------------------------------------------ the places

const VAULT = v(-2.65, 0, 0);
const NOTE_SPOTS = [v(-4.1, 0, -1.5), v(-4.1, 0, 0), v(-4.1, 0, 1.5), v(-2.65, 0, -3), v(-2.65, 0, 3), v(-1.2, 0, -3), v(-3.55, 0, -2.35), v(-1.2, 0, 3)];
const TO_YOU = v(-2.65, 0, 1.5);
const TO_MAKER = v(-1.2, 0, -1.5);
// The swap mirrors across the seam: ZEC from the maker to you on Zcash, USDC from you to the maker on Base.
const YOUR_ADDRESS = v(-1.2, 0, 1.5);
const EDGE = v(-4.95, 0, -1.6);
const CORE = v(0, 0.55, 0);
const LEDGER_X = 1.95;
const SLOTS = [2.0, 1.2, 0.4, -0.4, -1.2, -2.0, -2.8];
const MARKET = v(3.9, 0, -1.4);
const ACCOUNT = v(3.95, 0, 1.3);
const ESCROW = v(3.75, 0, -0.35);
const MAKER_DESK = v(3.75, 0, -2.0);
const BOND_SPOTS = [-2.4, -1.75, -1.1, -0.45, 0.2].map((z) => v(3.55, 0, z));
const WZEC = v(3.2, 0, 2.6);
const GAS = v(3.9, 0, -1.4);
const PLANK = v(0, 0.06, 3.0);

/** What Base records, in order; `until` takes back what only the bridge's hypothesis wrote. */
const EVENTS = [
  { t: 2.12, name: "HeaderAdded", fields: ["a later Zcash header,", "Equihash checked on Base"] },
  { t: 2.7, name: "VaultActivated", fields: ["vault 0x59d5e63e…", "amount 1,000,000"], tone: "gold" },
  { t: 3.5, name: "Borrow", fields: ["4 USDC", "to a fresh Safe"] },
  { t: 4.24, name: "SpendCommitted", fields: ["payoutRef 0xcdf694fd…", "before it is signed"] },
  { t: 4.88, name: "CloseRelease", fields: ["yours + the maker’s", "= the outstanding"], tone: "gold" },
  { t: 5.34, name: "OrderOpened", fields: ["USDC in escrow,", "keccak256(salt || receiver)"] },
  { t: 5.84, name: "OrderSettled", fields: ["on a payment proof:", "USDC to the maker"], tone: "gold" },
  { t: 6.38, name: "wZEC minted", fields: ["0.125 wZEC to", "0x7a3e…41c2"], tone: "leak", until: 6.6 },
  { t: 7.6, name: "ViolationProven", fields: ["a spend no commitment", "names: F1"], tone: "gold" },
  { t: 7.78, name: "Slashed", fields: ["all five bonds", ""] },
  { t: 7.94, name: "Compensated", fields: ["the victim’s debt repaid,", "3 → 12.65 USDC"] },
];

// ------------------------------------------------------------------------------------------ helpers

const tmp = v(0, 0, 0);

/** Along an arc from a to b, lifted by `lift` in its middle. */
function arc(a, b, t, lift, out) {
  out.lerpVectors(a, b, t);
  out.y += Math.sin(Math.PI * t) * lift;
  return out;
}

/** Shows, places and scales one object; scale 0 hides it. */
function put(object, position, scale = 1, visible = true) {
  object.visible = visible && scale > 0.002;
  if (!object.visible) return;
  object.position.copy(position);
  object.scale.setScalar(scale);
}

/** A proof's way: out of `from`, into the core, out of the core, onto `to`, over [a, b] of story time. */
function proofAt(s, a, b, from, to, out) {
  const t = clamp01((s - a) / (b - a));
  const start = v(from.x, 0.55, from.z);
  const end = v(to.x, to.y + 0.35, to.z);
  if (t < 0.12) {
    out.lerpVectors(v(from.x, 0.05, from.z), start, t / 0.12);
  } else if (t < 0.5) {
    arc(start, CORE, (t - 0.12) / 0.38, 0.9, out);
  } else {
    arc(CORE, end, (t - 0.5) / 0.5, 0.9, out);
  }
  // Smaller as it passes through the core.
  const scale = (1 - 0.6 * Math.exp(-((t - 0.5) ** 2) / 0.004)) * seg(s, a, a + 0.03) * (1 - seg(s, b - 0.02, b));
  return { scale, through: Math.exp(-((t - 0.5) ** 2) / 0.003) * (t > 0 && t < 1 ? 1 : 0) };
}

// --------------------------------------------------------------------------------------- the story

export function createStory(world, props) {
  const { notes, vaultNote, yourNote, makerNote, swapNote, theftNote, zec, makerStack, quote, coinYou, coinMaker, makerZec, zzec, wzec, gasWallet, keys, proof, events, morpho, chips, account, maker, address, box, bonds, shards, shardSeeds, ring, tethers, threads, plank, dummy } = props;
  const red = new Color(COLORS.red);
  const goldCore = new Color(COLORS.gold);
  const keyWhite = new Color(0xcfe0f2);
  const anchors = {};
  const p1 = v(0, 0, 0);
  const p2 = v(0, 0, 0);

  /** The ledger's rows at story time s: the newest at the front, older ones pushed back. */
  function ledger(s) {
    const list = EVENTS.filter((e) => e.t <= s && (e.until === undefined || s < e.until)).reverse();
    const newest = list[0];
    const arrive = newest ? seg(s, newest.t, newest.t + 0.09) : 1;
    events.forEach((tile, i) => {
      const e = list[i];
      if (!e) {
        tile.group.visible = false;
        return;
      }
      const key = `${e.name}|${e.fields.join("|")}`;
      if (tile.shown !== key) {
        tile.text.redraw(e.name, e.fields, e.tone ?? "ink");
        tile.shown = key;
      }
      const leaving = i === events.length - 1;
      tile.group.visible = !leaving || arrive < 1;
      const z = i === 0 ? SLOTS[0] : mix(SLOTS[i - 1], SLOTS[i], arrive);
      const rise = i === 0 ? arrive : 1;
      tile.group.position.set(LEDGER_X, mix(-0.4, 0.06, rise), z + (i === 0 ? (1 - arrive) * 0.4 : 0));
      tile.group.rotation.x = i === 0 ? (1 - arrive) * Math.PI * 0.5 : 0;
      tile.group.scale.setScalar(leaving ? 1 - arrive : 1);
      const fresh = i === 0 ? 1 - seg(s, newest.t + 0.06, newest.t + 0.3) : 0;
      const glow = e.tone === "leak" ? COLORS.red : e.tone === "gold" ? COLORS.gold : keyWhite;
      tile.material.emissive.copy(glow);
      tile.material.emissiveIntensity = fresh * (e.tone ? 0.55 : 0.25);
    });
  }

  /**
   * USDC chips on their way from `from` to `to` (`flow` 0 → 1, one after another), stacked at `startStack` before
   * they leave and at `endStack` once they arrive (each optional), all scaled by `size`.
   */
  function chipStream(first, count, from, to, flow, { startStack = null, endStack = null, size = 1, time = 0, stackIndex = 0 } = {}) {
    for (let i = 0; i < count; i += 1) {
      const t = clamp01(flow * 1.35 - (i / count) * 0.35);
      const stack = t <= 0 ? startStack : t >= 1 ? endStack : null;
      if (t <= 0 || t >= 1) {
        if (stack) {
          dummy.position.set(stack.x, stack.y + 0.04 + (stackIndex + i) * 0.068, stack.z);
          dummy.rotation.set(0, (stackIndex + i) * 0.5, 0);
          dummy.scale.setScalar(size);
        } else {
          dummy.position.set(0, -50, 0);
          dummy.scale.setScalar(0);
        }
      } else {
        arc(from, to, t, 1.1, dummy.position);
        dummy.position.y += 0.1;
        dummy.rotation.set(time * 3 + i, 0, time * 2 + i);
        dummy.scale.setScalar((Math.sin(Math.PI * t) * 0.5 + 0.5) * size);
      }
      dummy.updateMatrix();
      chips.setMatrixAt(first + i, dummy.matrix);
    }
  }

  function hideChips(from, to) {
    dummy.position.set(0, -50, 0);
    dummy.scale.setScalar(0);
    dummy.updateMatrix();
    for (let k = from; k < to; k += 1) chips.setMatrixAt(k, dummy.matrix);
  }

  return function update(s, time) {
    for (const key of Object.keys(anchors)) delete anchors[key];

    // The pool: other people's notes, breathing a little.
    notes.forEach((n, i) => {
      put(n.group, tmp.set(NOTE_SPOTS[i].x, 0.08 + Math.sin(time * 0.8 + i * 1.7) * 0.025, NOTE_SPOTS[i].z));
      n.group.rotation.y = Math.sin(time * 0.3 + i) * 0.06;
    });

    // Your ZEC: hovering above the pool, then paid into the vault, where it becomes ciphertext.
    const drop = seg(s, 1.3, 1.5);
    const sealVault = seg(s, 1.5, 1.64);
    const coinHover = v(VAULT.x, 1.55 + Math.sin(time * 1.1) * 0.08, VAULT.z);
    const coinPos = v(VAULT.x, mix(coinHover.y, 0.12, drop), VAULT.z);
    put(zec.group, coinPos, (1 - sealVault) * (1 - seg(s, 1.64, 1.7)), s < 1.7);
    zec.group.rotation.set(Math.sin(time * 0.6) * 0.25 * (1 - drop), time * 0.5 * (1 - drop), 0);
    if (s < 1.6) anchors.coin = { at: zec.group.position, on: win(s, 0.55, 0.75, 1.4, 1.5) };

    // The vault: a note of its own, ringed by its five signers.
    const vaultOpen = seg(s, 4.42, 4.52);
    const vaultScale = sealVault * (1 - vaultOpen);
    put(vaultNote.group, tmp.set(VAULT.x, 0.08, VAULT.z), vaultScale);
    vaultNote.material.emissiveIntensity = 1 + 6 * win(s, 1.52, 1.62, 1.68, 1.95);
    if (vaultScale > 0.5) anchors.vault = { at: v(VAULT.x, 0.6, VAULT.z), on: win(s, 1.6, 1.7, 2.1, 2.2) };

    // The signers: around the vault to create it and to sign its release, never for the proof; again for the theft.
    const keysUp = Math.max(win(s, 1.1, 1.26, 1.82, 1.96), win(s, 4.3, 4.42, 4.64, 4.74), win(s, 7.04, 7.12, 7.88, 7.98));
    const compromised = seg(s, 7.16, 7.26);
    const slashed = seg(s, 7.78, 7.88);
    keys.forEach((k, i) => {
      const angle = (i / 5) * Math.PI * 2 + time * 0.12;
      const y = mix(-0.4, 0.62 + Math.sin(time * 1.4 + i) * 0.05, keysUp) - slashed * 0.4;
      put(k.group, tmp.set(VAULT.x + Math.cos(angle) * 1.05, y, VAULT.z + Math.sin(angle) * 1.05), keysUp);
      k.group.rotation.y = -angle;
      const bad = i < 3 ? compromised : 0;
      k.material.emissive.copy(keyWhite).lerp(red, bad);
      k.material.emissiveIntensity = 0.1 + bad * 0.3 - slashed * 0.1;
      k.material.color.setRGB(mix(mix(0.96, 0.92, bad), 0.35, slashed), mix(mix(0.97, 0.3, bad), 0.38, slashed), mix(mix(0.98, 0.26, bad), 0.37, slashed));
    });
    const ringPos = ring.positions;
    for (let i = 0; i < 5; i += 1) {
      const a = keys[i].group.position;
      const b = keys[(i + 1) % 5].group.position;
      ringPos.setXYZ(i * 2, a.x, a.y, a.z);
      ringPos.setXYZ(i * 2 + 1, b.x, b.y, b.z);
    }
    ringPos.needsUpdate = true;
    ring.object.visible = keysUp > 0.05;
    ring.material.opacity = 0.45 * keysUp * (1 - slashed);
    ring.material.color.setHex(0xdbe8ff).lerp(red, compromised * 0.6);
    if (keysUp > 0.5 && s < 2) anchors.keys = { at: keys[1].group.position, on: win(s, 1.16, 1.24, 1.5, 1.58) };

    // The core: a heartbeat, and a flash each time a proof passes through it.
    let through = 0;
    let proofShown = false;
    const showProof = (a, b, from, to, tag) => {
      if (s < a || s > b) return;
      const { scale, through: t } = proofAt(s, a, b, from, to, proof.group.position);
      proof.group.visible = scale > 0.01;
      proof.group.scale.setScalar(scale);
      proof.group.rotation.set(0, time * 1.2, 0);
      through = Math.max(through, t);
      proofShown = true;
      if (tag) anchors[tag] = { at: proof.group.position, on: win(s, a + 0.02, a + 0.06, b - 0.08, b - 0.03) };
    };
    showProof(2.24, 2.7, VAULT, v(LEDGER_X, 0.06, SLOTS[0]), "proof");
    showProof(4.7, 4.88, TO_YOU, v(LEDGER_X, 0.06, SLOTS[0]), "inclusion");
    showProof(5.62, 5.78, YOUR_ADDRESS, v(ESCROW.x, 0.4, ESCROW.z), "payment");
    showProof(6.7, 6.92, TO_YOU, v(LEDGER_X, 0.06, SLOTS[0]), "only");
    showProof(7.5, 7.62, VAULT, v(LEDGER_X, 0.06, SLOTS[0]), "violation");
    if (!proofShown) proof.group.visible = false;
    const alarm = win(s, 7.46, 7.48, 7.52, 7.55);
    world.goldMaterial.emissive.copy(goldCore).lerp(red, alarm);
    world.goldMaterial.emissiveIntensity = 0.16 + 0.06 * Math.sin(time * 2.2) + through * 0.9 + alarm * 1.2;
    world.coreLight.color.copy(goldCore).lerp(red, alarm);
    world.seamMaterial.opacity = 0.35 + through * 0.65 + alarm * 0.65;
    world.seamMaterial.color.setRGB(2.2 + alarm * 0.6, 1.45 - alarm * 1.1, 0.35 - alarm * 0.1);
    world.coreLight.intensity = 3.5 + through * 10 + alarm * 14;
    if (s < 1) anchors.core = { at: world.core.position, on: 1 - seg(s, 0.55, 0.75) };

    // What Base records.
    ledger(s);

    // zZEC: minted on the proof, then supplied to the market.
    const zzecIn = seg(s, 2.7, 2.8);
    const supply = seg(s, 3.14, 3.28);
    const zzecFrom = v(LEDGER_X + 1.2, 0.1, 2.15);
    arc(zzecFrom, v(MARKET.x, 0.1, MARKET.z), supply, 0.9, p1);
    put(zzec.group, p1, zzecIn * (1 - seg(s, 3.24, 3.3)), s < 3.3);
    zzec.group.rotation.y = time * 0.7;
    if (zzecIn > 0.5) anchors.minted = { at: zzec.group.position, on: win(s, 2.74, 2.82, 3.06, 3.14) };

    // The market and the account.
    const marketUp = Math.max(win(s, 3.06, 3.16, 4.92, 5.02), win(s, 8.0, 8.12, 99, 100));
    put(morpho.group, tmp.set(MARKET.x, mix(-0.3, 0, marketUp), MARKET.z), marketUp);
    morpho.ringMaterial.emissiveIntensity = 0.6 + 0.25 * Math.sin(time * 2);
    morpho.pool.rotation.y = time * 0.15;
    anchors.market = { at: v(MARKET.x, 0.5, MARKET.z), on: win(s, 3.1, 3.18, 3.86, 3.94) };
    const accountUp = seg(s, 3.26, 3.36);
    put(account.group, tmp.set(ACCOUNT.x, mix(-0.3, 0.06, accountUp), ACCOUNT.z), accountUp);
    anchors.account = { at: v(ACCOUNT.x, 0.4, ACCOUNT.z), on: win(s, 3.32, 3.4, 3.86, 3.94) };

    // USDC: borrowed into the account, repaid by the close, put in escrow, and a victim's compensation.
    const accountTop = v(ACCOUNT.x + 0.35, 0.12, ACCOUNT.z);
    const inBox = v(ESCROW.x, 0.02, ESCROW.z);
    if (s < 4.0) {
      chipStream(0, 6, MARKET, accountTop, seg(s, 3.36, 3.72), { endStack: accountTop, time });
      hideChips(6, 40);
    } else if (s < 5.0) {
      chipStream(0, 6, accountTop, MARKET, seg(s, 4.06, 4.2), { startStack: accountTop, time });
      hideChips(6, 40);
      anchors.repaid = { at: v(MARKET.x, 0.5, MARKET.z), on: win(s, 4.08, 4.14, 4.22, 4.28) };
    } else if (s < 6.05) {
      // Your USDC into escrow and, on the proof, to the maker, onto the maker's own USDC: its liquidity on Base.
      const makerTop = v(MAKER_DESK.x + 0.42, 0.12, MAKER_DESK.z);
      const sink = 1 - seg(s, 5.98, 6.04);
      const outOfBox = seg(s, 5.86, 5.95);
      if (outOfBox <= 0) {
        chipStream(0, 6, accountTop, inBox, seg(s, 5.22, 5.34), { startStack: accountTop, endStack: inBox, size: seg(s, 5.14, 5.2), time });
      } else chipStream(0, 6, inBox, makerTop, outOfBox, { startStack: inBox, endStack: makerTop, size: sink, time, stackIndex: 6 });
      chipStream(6, 6, makerTop, makerTop, 0, { startStack: makerTop, size: seg(s, 5.04, 5.1) * sink, time });
      hideChips(12, 40);
      anchors.release = { at: v(MAKER_DESK.x, 0.9, MAKER_DESK.z), on: win(s, 5.88, 5.92, 5.98, 6.02) };
    } else if (s > 7.8 && s < 8.36) {
      const pay = seg(s, 7.84, 7.96);
      for (let b = 0; b < 5; b += 1) {
        chipStream(b * 2, 2, BOND_SPOTS[b], accountTop, pay, { endStack: accountTop, size: 1 - seg(s, 8.22, 8.34), time, stackIndex: b * 2 });
      }
      hideChips(10, 40);
      anchors.victim = { at: v(ACCOUNT.x, 0.4, ACCOUNT.z), on: win(s, 7.92, 7.98, 8.14, 8.22) };
    } else hideChips(0, 40);
    chips.instanceMatrix.needsUpdate = true;

    // The close: two payouts in one transaction, each a new note.
    const out = seg(s, 4.46, 4.62);
    const sealOut = seg(s, 4.62, 4.7);
    arc(VAULT, TO_YOU, out, 0.8, p1);
    p1.y += 0.12;
    put(coinYou.group, p1, (1 - sealOut) * seg(s, 4.44, 4.48), s > 4.42 && s < 4.72);
    coinYou.group.rotation.y = time;
    arc(VAULT, TO_MAKER, out, 0.8, p2);
    p2.y += 0.12;
    put(coinMaker.group, p2, (1 - sealOut) * seg(s, 4.44, 4.48), s > 4.42 && s < 4.72);
    coinMaker.group.rotation.y = -time;
    if (s > 4.44 && s < 4.72) {
      anchors["to-you"] = { at: coinYou.group.position, on: win(s, 4.48, 4.54, 4.64, 4.7) };
      anchors["to-maker"] = { at: coinMaker.group.position, on: win(s, 4.48, 4.54, 4.64, 4.7) };
    }
    const bridgeOut = win(s, 6.14, 6.2, 6.58, 6.66);
    put(yourNote.group, tmp.set(TO_YOU.x, 0.08 + Math.sin(time * 0.8 + 3) * 0.025, TO_YOU.z), sealOut * (1 - bridgeOut));
    put(makerNote.group, tmp.set(TO_MAKER.x, 0.08 + Math.sin(time * 0.8 + 5) * 0.025, TO_MAKER.z), sealOut);
    makerNote.material.emissiveIntensity = 1 + 3 * win(s, 5.04, 5.08, 5.14, 5.2) + 4 * win(s, 5.38, 5.42, 5.44, 5.5);

    // The swap, mirrored across the seam. Its liquidity is a market maker's own: USDC on Base, ZEC on Zcash. The
    // maker quotes you a price, signed for you alone; your USDC goes into escrow, locked to a salted hash of an
    // address of yours; the maker pays that address from its own ZEC; the payment's proof crosses, matches the hash,
    // and the escrow pays the maker. Neither coin leaves its chain.
    const swapUp = win(s, 5.02, 5.1, 6.0, 6.06);
    put(maker.group, tmp.set(MAKER_DESK.x, mix(-0.3, 0.06, swapUp), MAKER_DESK.z), swapUp);
    anchors.liquidity = { at: v(MAKER_DESK.x - 0.3, 0.5, MAKER_DESK.z), on: win(s, 5.06, 5.1, 5.15, 5.19) };
    const paid = seg(s, 5.42, 5.44);
    makerStack.forEach((c, i) => {
      put(c.group, tmp.set(TO_MAKER.x, 0.22 + i * 0.13, TO_MAKER.z), seg(s, 5.04, 5.1) * (1 - seg(s, 5.98, 6.04)), s > 5.03 && s < 6.05);
      c.group.rotation.y = 0.4 * i;
    });
    const quoteOut = seg(s, 5.1, 5.2);
    const quoteIn = seg(s, 5.26, 5.32);
    const hover = v(ACCOUNT.x - 0.25, 1.1 + Math.sin(time * 1.3) * 0.04, ACCOUNT.z);
    arc(v(MAKER_DESK.x, 0.3, MAKER_DESK.z), hover, quoteOut, 1.0, p2);
    if (quoteIn > 0) p2.lerpVectors(hover, v(ESCROW.x, 0.45, ESCROW.z), quoteIn);
    put(quote.group, p2, 0.78 * seg(s, 5.08, 5.12) * (1 - quoteIn), s > 5.07 && s < 5.33);
    quote.group.rotation.set(0.75 * (1 - quoteIn), 0.15 * (1 - quoteIn), 0);
    if (s > 5.08 && s < 5.33) anchors.quote = { at: quote.group.position, on: win(s, 5.14, 5.18, 5.24, 5.28) };
    put(box.group, ESCROW, win(s, 5.14, 5.22, 6.0, 6.05));
    // The proof matches the hash on the lid: the lid lights up and opens.
    const opened = seg(s, 5.8, 5.86);
    const match = win(s, 5.76, 5.79, 5.88, 5.94);
    box.lid.position.y = 0.66 + (1 - seg(s, 5.32, 5.38)) * 0.42 + opened * 0.55;
    box.lid.rotation.z = opened * 0.5;
    box.lidMaterial.emissive.copy(goldCore);
    box.lidMaterial.emissiveIntensity = 0.9 * match;
    box.lidFace.material.color.setRGB(1 + 1.3 * match, 1 + 0.55 * match, 1 - 0.65 * match);
    anchors.escrow = { at: v(ESCROW.x, 1.2, ESCROW.z), on: win(s, 5.24, 5.28, 5.32, 5.36) };
    anchors.hash = { at: v(ESCROW.x, 0.8, ESCROW.z), on: win(s, 5.36, 5.4, 5.46, 5.5) };
    const sealPay = seg(s, 5.56, 5.62);
    put(address.group, tmp.set(YOUR_ADDRESS.x, 0.08, YOUR_ADDRESS.z), win(s, 5.14, 5.22, 5.58, 5.62));
    address.material.opacity = 0.55 + 0.45 * Math.sin(time * 2.6) ** 2;
    address.fill.opacity = 0.08 + 0.1 * Math.sin(time * 2.6) ** 2;
    if (s > 5.18 && s < 5.42) anchors.address = { at: v(YOUR_ADDRESS.x, 0.3, YOUR_ADDRESS.z), on: win(s, 5.2, 5.24, 5.36, 5.4) };
    // The maker pays from the top of its own ZEC.
    const pay = seg(s, 5.42, 5.56);
    arc(v(TO_MAKER.x, 0.48, TO_MAKER.z), v(YOUR_ADDRESS.x, 0.14, YOUR_ADDRESS.z), pay, 1.0, p1);
    put(makerZec.group, p1, seg(s, 5.04, 5.1) * (1 - sealPay), s > 5.03 && s < 5.64);
    makerZec.group.rotation.y = 0.8 + time * paid;
    put(swapNote.group, tmp.set(YOUR_ADDRESS.x, 0.08 + Math.sin(time * 0.8 + 7) * 0.025, YOUR_ADDRESS.z), sealPay);
    swapNote.material.emissiveIntensity = 1 + 5 * win(s, 5.56, 5.6, 5.64, 5.76);
    if (s > 5.4 && s < 5.62) anchors.payee = { at: makerZec.group.position, on: win(s, 5.44, 5.48, 5.54, 5.58) };

    // A bridge, for comparison: the coin leaves the pool, crosses on a plank, becomes wZEC, and everything links.
    const plankUp = win(s, 6.1, 6.18, 6.56, 6.64);
    put(plank, tmp.set(PLANK.x, mix(-0.4, PLANK.y, plankUp), PLANK.z), plankUp);
    const cross = seg(s, 6.2, 6.36) * (1 - seg(s, 6.56, 6.64));
    const bridgePath = [v(TO_YOU.x, 0.14, TO_YOU.z), v(-1.0, 0.18, 3.0), v(1.0, 0.18, 3.0), v(WZEC.x, 0.14, WZEC.z)];
    const leg = Math.min(2.999, cross * 3);
    const legI = Math.floor(leg);
    p1.lerpVectors(bridgePath[legI], bridgePath[legI + 1], leg - legI);
    const minted = win(s, 6.34, 6.38, 6.54, 6.58);
    if (s > 6.1 && s < 6.7) {
      put(zec.group, p1, bridgeOut * (1 - minted), s > 6.12 && s < 6.66);
      zec.group.rotation.set(0, time, 0);
    }
    put(wzec.group, v(WZEC.x, 0.12, WZEC.z), minted);
    wzec.group.rotation.y = time * 0.6;
    put(gasWallet.group, v(GAS.x, 0.12, GAS.z), win(s, 6.32, 6.38, 6.54, 6.6));
    const linked = win(s, 6.38, 6.5, 6.54, 6.6);
    threads.forEach((t, i) => {
      const drawn = clamp01(linked * 1.6 - i * 0.25);
      t.mesh.visible = drawn > 0.01;
      t.mesh.geometry.setDrawRange(0, Math.floor((t.total * drawn) / 3) * 3);
    });
    anchors.bridge = { at: v(0, 0.6, PLANK.z), on: win(s, 6.16, 6.22, 6.32, 6.38) };
    anchors.links = { at: v(ACCOUNT.x, 0.5, ACCOUNT.z), on: win(s, 6.44, 6.48, 6.54, 6.58) };

    // The theft, on its own deployment: a vault, three signers turned, the note swept off, a proof, the bonds.
    const theftVault = win(s, 7.06, 7.12, 7.44, 7.52);
    const dragged = seg(s, 7.3, 7.44);
    const fall = seg(s, 7.42, 7.52);
    p1.lerpVectors(VAULT, EDGE, dragged);
    p1.y = 0.08 - fall * 2.8;
    put(theftNote.group, p1, Math.max(theftVault, dragged > 0 && fall < 1 ? 1 : 0), s > 7.04 && s < 7.54);
    theftNote.group.rotation.set(fall * 1.6, dragged * 0.8, fall * 0.9);
    theftNote.material.emissive.copy(new Color(0x2a1e00)).lerp(red, compromised * 0.4);
    const tetherOn = win(s, 7.24, 7.3, 7.44, 7.48);
    for (let i = 0; i < 3; i += 1) {
      const a = keys[i].group.position;
      tethers.positions.setXYZ(i * 2, a.x, a.y, a.z);
      tethers.positions.setXYZ(i * 2 + 1, p1.x, p1.y + 0.1, p1.z);
    }
    tethers.positions.needsUpdate = true;
    tethers.object.visible = tetherOn > 0.02;
    tethers.material.opacity = tetherOn;
    if (s > 7.04 && s < 7.52) {
      anchors.compromised = { at: keys[1].group.position, on: win(s, 7.18, 7.24, 7.3, 7.36) };
      anchors.swept = { at: theftNote.group.position, on: win(s, 7.34, 7.38, 7.46, 7.5) };
    }
    const bondsUp = win(s, 7.06, 7.14, 7.78, 7.8);
    bonds.forEach((b, i) => {
      put(b.group, tmp.set(BOND_SPOTS[i].x, mix(-0.3, 0.09, bondsUp), BOND_SPOTS[i].z), bondsUp);
      b.material.emissiveIntensity = 0.12 + 0.6 * win(s, 7.64, 7.7, 7.76, 7.8);
    });
    if (s > 7.6 && s < 7.95) {
      anchors.bonds = { at: v(BOND_SPOTS[2].x, 0.4, BOND_SPOTS[2].z), on: win(s, 7.64, 7.68, 7.74, 7.78) };
      anchors.slashed = { at: v(BOND_SPOTS[2].x, 0.5, BOND_SPOTS[2].z), on: win(s, 7.78, 7.82, 7.9, 7.94) };
    }
    const burst = seg(s, 7.78, 7.96);
    for (let i = 0; i < shardSeeds.length; i += 1) {
      const seed = shardSeeds[i];
      const origin = BOND_SPOTS[seed.bond];
      const t = burst * 0.9;
      dummy.position.set(origin.x + seed.vx * t, 0.15 + seed.vy * t - 4.9 * t * t, origin.z + seed.vz * t);
      dummy.rotation.set(seed.spin * t, seed.spin * t * 0.7, 0);
      dummy.scale.setScalar(burst > 0 && burst < 1 ? 1 - burst * 0.6 : 0);
      dummy.updateMatrix();
      shards.setMatrixAt(i, dummy.matrix);
    }
    shards.instanceMatrix.needsUpdate = true;

    // Names for the things on screen.
    if (s < 1) {
      anchors.pool = { at: v(-3.6, 0.2, 2.9), on: 1 - seg(s, 0.55, 0.75) };
      anchors.ledger = { at: v(3.6, 0.2, 2.9), on: 1 - seg(s, 0.55, 0.75) };
    }
    return anchors;
  };
}
