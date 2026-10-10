/**
 * Minimal ZIP 316 unified address codec: Bech32m (no length limit) → F4Jumble⁻¹ → padding
 * check → TLV items. Enough for the app to extract the Orchard-protocol receiver of a UA
 * itself (D4: the "Ironwood receiver" is the UA's Orchard receiver, typecode 0x03) and to
 * check that a vault address is Ironwood-only.
 *
 * Validation follows librustzcash's `zcash_address::unified` (docs/ref/vendor): known HRPs
 * for Revision 0 (`u`, `utest`, `uregtest`) and Revision 2 (`zu…` shielded-only, `tu…`
 * transparent-including), ascending unique typecodes, no P2PKH + P2SH together, at least one
 * data item, a shielded item in Revision 0, no unknown MUST-understand metadata, receiver
 * lengths 20 / 43 bytes.
 */
import { bech32m } from "@scure/base";
import { f4jumble, f4jumbleInv, F4JUMBLE_MAX_LENGTH, F4JUMBLE_MIN_LENGTH } from "./f4jumble";

export type ZcashNetwork = "main" | "test" | "regtest";
export type UaRevision = 0 | 2;

export const TYPECODE_P2PKH = 0x00;
export const TYPECODE_P2SH = 0x01;
export const TYPECODE_SAPLING = 0x02;
export const TYPECODE_ORCHARD = 0x03;
const METADATA_MIN = 0xc0;
const METADATA_MAX = 0xfc;
const MUST_UNDERSTAND_MIN = 0xe0;
const METADATA_EXPIRY_HEIGHT = 0xe0;
const METADATA_EXPIRY_TIME = 0xe1;
const MAX_TYPECODE = 0x02000000;
const PADDING_LEN = 16;

interface HrpInfo {
  network: ZcashNetwork;
  revision: UaRevision;
  /** Revision 2 `tu…` HRPs may carry transparent receivers; `zu…` must not. */
  transparentIncluding: boolean;
}

const HRPS: Record<string, HrpInfo> = {
  u: { network: "main", revision: 0, transparentIncluding: true },
  utest: { network: "test", revision: 0, transparentIncluding: true },
  uregtest: { network: "regtest", revision: 0, transparentIncluding: true },
  zu: { network: "main", revision: 2, transparentIncluding: false },
  zutest: { network: "test", revision: 2, transparentIncluding: false },
  zuregtest: { network: "regtest", revision: 2, transparentIncluding: false },
  tu: { network: "main", revision: 2, transparentIncluding: true },
  tutest: { network: "test", revision: 2, transparentIncluding: true },
  turegtest: { network: "regtest", revision: 2, transparentIncluding: true },
};

/** HRP for encoding (Revision 0 only, which is what the signers produce). */
export const UA_HRP_R0: Record<ZcashNetwork, string> = { main: "u", test: "utest", regtest: "uregtest" };

export interface UaItem {
  typecode: number;
  data: Uint8Array;
}

export interface DecodedUa {
  hrp: string;
  network: ZcashNetwork;
  revision: UaRevision;
  /** All items in encoding order (receivers and metadata). */
  items: UaItem[];
  /** Raw 43-byte Orchard-protocol receiver, if present. */
  orchard?: Uint8Array;
  sapling?: Uint8Array;
  p2pkh?: Uint8Array;
  p2sh?: Uint8Array;
  /** Unknown receiver typecodes (ignored by Zecured, which pays only the Orchard receiver). */
  unknownReceivers: number[];
  /** Expiry metadata (Revision 2 only). */
  expiryHeight?: number;
  expiryTime?: bigint;
}

export class UnifiedAddressError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UnifiedAddressError";
  }
}

// ------------------------------------------------------------------ CompactSize

function readCompactSize(buf: Uint8Array, pos: number): { value: number; next: number } {
  const first = buf[pos];
  if (first === undefined) throw new UnifiedAddressError("truncated item");
  if (first < 0xfd) return { value: first, next: pos + 1 };
  const width = first === 0xfd ? 2 : first === 0xfe ? 4 : 8;
  if (pos + 1 + width > buf.length) throw new UnifiedAddressError("truncated item");
  let value = 0n;
  for (let k = width - 1; k >= 0; k--) value = (value << 8n) | BigInt(buf[pos + 1 + k] ?? 0);
  // Canonical encoding only, bounded by MAX_SIZE (0x02000000) like zcash_encoding.
  const min = width === 2 ? 0xfdn : width === 4 ? 0x10000n : 0x100000000n;
  if (value < min) throw new UnifiedAddressError("non-canonical CompactSize");
  if (value > 0x02000000n) throw new UnifiedAddressError("CompactSize too large");
  return { value: Number(value), next: pos + 1 + width };
}

function writeCompactSize(n: number): Uint8Array {
  if (n < 0xfd) return Uint8Array.of(n);
  if (n <= 0xffff) return Uint8Array.of(0xfd, n & 0xff, n >> 8);
  return Uint8Array.of(0xfe, n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >>> 24) & 0xff);
}

// ------------------------------------------------------------------ decode

function normalize(input: string): string {
  const s = input.trim();
  // Bech32 strings are all-lowercase or all-uppercase.
  if (s === s.toUpperCase()) return s.toLowerCase();
  return s;
}

/** Decodes and validates a unified address. Throws `UnifiedAddressError`. */
export function decodeUnifiedAddress(input: string): DecodedUa {
  const s = normalize(input);
  let prefix: string;
  let bytes: Uint8Array;
  try {
    const decoded = bech32m.decode(s, false);
    prefix = decoded.prefix;
    bytes = bech32m.fromWords(decoded.words);
  } catch {
    throw new UnifiedAddressError("not a Bech32m-encoded unified address (check for typos)");
  }
  const info = HRPS[prefix];
  if (!info) throw new UnifiedAddressError(`unknown address prefix "${prefix}"`);
  if (bytes.length < F4JUMBLE_MIN_LENGTH || bytes.length > F4JUMBLE_MAX_LENGTH) {
    throw new UnifiedAddressError("invalid unified address length");
  }

  const raw = f4jumbleInv(bytes);
  const expectedPadding = new Uint8Array(PADDING_LEN);
  expectedPadding.set(new TextEncoder().encode(prefix));
  const body = raw.subarray(0, raw.length - PADDING_LEN);
  const padding = raw.subarray(raw.length - PADDING_LEN);
  if (!padding.every((b, i) => b === expectedPadding[i])) {
    throw new UnifiedAddressError("invalid padding (corrupted address or wrong network prefix)");
  }

  const items: UaItem[] = [];
  let pos = 0;
  while (pos < body.length) {
    const tc = readCompactSize(body, pos);
    const len = readCompactSize(body, tc.next);
    const end = len.next + len.value;
    if (end > body.length) throw new UnifiedAddressError("truncated item");
    items.push({ typecode: tc.value, data: body.slice(len.next, end) });
    pos = end;
  }
  return validate(prefix, info, items);
}

function validate(hrp: string, info: HrpInfo, items: UaItem[]): DecodedUa {
  const out: DecodedUa = {
    hrp,
    network: info.network,
    revision: info.revision,
    items,
    unknownReceivers: [],
  };
  let prev = -1;
  let hasData = false;
  let onlyTransparent = true;
  for (const { typecode, data } of items) {
    if (typecode > MAX_TYPECODE) throw new UnifiedAddressError("typecode out of range");
    if (typecode < prev) throw new UnifiedAddressError("items are out of typecode order");
    if (typecode === prev) throw new UnifiedAddressError("duplicate typecode");
    const isMetadata = typecode >= METADATA_MIN && typecode <= METADATA_MAX;
    if (isMetadata) {
      if (typecode >= MUST_UNDERSTAND_MIN) {
        if (info.revision === 0) throw new UnifiedAddressError("metadata item not allowed in a Revision 0 address");
        if (typecode === METADATA_EXPIRY_HEIGHT) {
          if (data.length !== 4) throw new UnifiedAddressError("invalid expiry height length");
          out.expiryHeight = new DataView(data.buffer, data.byteOffset, 4).getUint32(0, true);
        } else if (typecode === METADATA_EXPIRY_TIME) {
          if (data.length !== 8) throw new UnifiedAddressError("invalid expiry time length");
          out.expiryTime = new DataView(data.buffer, data.byteOffset, 8).getBigUint64(0, true);
        } else {
          throw new UnifiedAddressError(`unknown MUST-understand metadata typecode 0x${typecode.toString(16)}`);
        }
      }
    } else {
      hasData = true;
      switch (typecode) {
        case TYPECODE_P2PKH:
          if (data.length !== 20) throw new UnifiedAddressError("invalid P2PKH receiver length");
          out.p2pkh = data;
          break;
        case TYPECODE_P2SH:
          if (data.length !== 20) throw new UnifiedAddressError("invalid P2SH receiver length");
          if (prev === TYPECODE_P2PKH) throw new UnifiedAddressError("address has both P2PKH and P2SH receivers");
          out.p2sh = data;
          break;
        case TYPECODE_SAPLING:
          if (data.length !== 43) throw new UnifiedAddressError("invalid Sapling receiver length");
          out.sapling = data;
          onlyTransparent = false;
          break;
        case TYPECODE_ORCHARD:
          if (data.length !== 43) throw new UnifiedAddressError("invalid Orchard receiver length");
          out.orchard = data;
          onlyTransparent = false;
          break;
        default:
          out.unknownReceivers.push(typecode);
          onlyTransparent = false;
      }
    }
    prev = typecode;
  }
  if (!hasData) throw new UnifiedAddressError("address has no receivers");
  if (info.revision === 0 && onlyTransparent) throw new UnifiedAddressError("address has only transparent receivers");
  if (info.revision === 2 && !info.transparentIncluding && (out.p2pkh || out.p2sh)) {
    throw new UnifiedAddressError("transparent receiver in a shielded-only (zu) address");
  }
  return out;
}

// ------------------------------------------------------------------ encode (Revision 0)

/** Encodes items (sorted by typecode) as a Revision 0 unified address. */
export function encodeUnifiedAddress(network: ZcashNetwork, items: UaItem[]): string {
  const hrp = UA_HRP_R0[network];
  const sorted = [...items].sort((a, b) => a.typecode - b.typecode);
  const parts: Uint8Array[] = [];
  for (const { typecode, data } of sorted) {
    parts.push(writeCompactSize(typecode), writeCompactSize(data.length), data);
  }
  const padding = new Uint8Array(PADDING_LEN);
  padding.set(new TextEncoder().encode(hrp));
  parts.push(padding);
  const total = parts.reduce((n, p) => n + p.length, 0);
  const raw = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    raw.set(p, off);
    off += p.length;
  }
  return bech32m.encode(hrp, bech32m.toWords(f4jumble(raw)), false);
}

/** True if the address is an "Ironwood-only" UA (D4): exactly one receiver, the Orchard one. */
export function isIronwoodOnly(ua: DecodedUa): boolean {
  const receivers = ua.items.filter((i) => i.typecode < METADATA_MIN || i.typecode > METADATA_MAX);
  return receivers.length === 1 && receivers[0]?.typecode === TYPECODE_ORCHARD;
}
