/**
 * Per-loan owner keys.
 *
 * There is no browser wallet: each loan gets a fresh secp256k1 owner key generated in the
 * browser. It controls the loan's Safe (and so the Morpho position) and signs every owner
 * request. It is stored in this browser's localStorage only, never sent anywhere, and can be
 * exported as a JSON backup. Losing it means losing control of the loan, so the UI asks the
 * user to export it right after creation.
 *
 * Every storage access is wrapped in try/catch: private browsing, quota errors or disabled
 * storage must never crash the app (the UI then insists on an immediate export).
 */
import { getAddress, isAddress, isHex, type Address, type Hex } from "viem";
import { generatePrivateKey, privateKeyToAccount } from "viem/accounts";
import type { VaultSignature } from "./api";
import { config } from "./config";
import { randomBytes32 } from "./eip712";
import { predictAccountAddress } from "./safe";
import { DENOMINATIONS } from "./zcash";

export const STORAGE_KEY = "zecured:loans:v1";
export const BACKUP_TYPE = "zecured-loan-backup";

/** A pending close-from-collateral request, kept so the user can cancel it (invalidateNonce). */
export interface PendingClose {
  nonce: string;
  deadline: string;
  executor: Address;
  submittedAt: number;
  /** The market maker's closeWithCollateral transaction, if it reported one. */
  txHash?: Hex;
}

export interface StoredLoan {
  v: 1;
  /** Local id (= the OpenVault requestId). */
  id: string;
  /** SECRET: the owner private key. */
  ownerKey: Hex;
  owner: Address;
  /** Counterfactual Safe address (lib/safe.ts). */
  account: Address;
  /** Collateral in zatoshi, decimal string. */
  amount: string;
  requestId: Hex;
  createdAt: number;
  vaultId?: Hex;
  /** The vault's Ironwood-only UA (public; kept to show the QR again). */
  vaultAddress?: string;
  expiryHeight?: number;
  /**
   * keccak256 of the vault's tag key (derived from its UFVK), as all five signers signed it when
   * the vault opened (VaultOpened v2, security review C-4). Absent for loans opened before that
   * check, and for loans opened under VaultOpened v1 (whose `ufvkHash` is not kept).
   */
  tagKeyHash?: Hex;
  /** The signers' VaultOpened signatures, checked when the vault opened and again before use. */
  vaultSignatures?: VaultSignature[];
  /** The user confirmed they exported the key. */
  backedUp?: boolean;
  /**
   * Last destination commitment accepted by the signers (no salt, no address: those stay
   * with the signers). Lets an interrupted exit continue without a new destination.
   */
  destinationCommitment?: Hex;
  pendingClose?: PendingClose;
  /** P-1 (DEMO): the demo wallet paid this loan's deposit; its exits may go back to the demo wallet. */
  demo?: boolean;
  /** P-8: the user restored this loan's backup file in this browser, so the backup is known to work. */
  backupVerified?: boolean;
}

export interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** localStorage, or null when it is unavailable (server render, privacy mode, blocked). */
export function browserStorage(): KeyValueStorage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    const probe = "zecured:probe";
    window.localStorage.setItem(probe, "1");
    window.localStorage.removeItem(probe);
    return window.localStorage;
  } catch {
    return null;
  }
}

function isBytes32(v: unknown): v is Hex {
  return typeof v === "string" && isHex(v) && v.length === 66;
}

/** Validates and normalizes an untrusted loan record (from storage or an import). */
export function parseLoan(raw: unknown): StoredLoan {
  if (!raw || typeof raw !== "object") throw new Error("not a loan record");
  const r = raw as Record<string, unknown>;
  if (!isBytes32(r.ownerKey)) throw new Error("missing or malformed owner key");
  const ownerKey = r.ownerKey.toLowerCase() as Hex;
  const owner = privateKeyToAccount(ownerKey).address;
  if (typeof r.owner === "string" && isAddress(r.owner, { strict: false }) && getAddress(r.owner) !== owner) {
    throw new Error("owner address does not match the owner key");
  }
  const account = predictAccountAddress(owner);
  if (typeof r.account === "string" && isAddress(r.account, { strict: false }) && getAddress(r.account) !== account) {
    throw new Error("account address does not match the owner key (different Safe configuration?)");
  }
  const amount = typeof r.amount === "string" && /^\d+$/.test(r.amount) ? r.amount : undefined;
  // A size the signers accept now (GET /config) or a D7 testnet size, so older backups stay importable.
  if (!amount || !(config.denominations.includes(BigInt(amount)) || DENOMINATIONS.includes(BigInt(amount)))) {
    throw new Error("unsupported collateral amount");
  }
  if (!isBytes32(r.requestId)) throw new Error("missing request id");
  const loan: StoredLoan = {
    v: 1,
    id: typeof r.id === "string" && r.id ? r.id : r.requestId,
    ownerKey,
    owner,
    account,
    amount,
    requestId: r.requestId.toLowerCase() as Hex,
    createdAt: typeof r.createdAt === "number" ? r.createdAt : Date.now(),
  };
  if (isBytes32(r.vaultId)) loan.vaultId = r.vaultId.toLowerCase() as Hex;
  if (typeof r.vaultAddress === "string") loan.vaultAddress = r.vaultAddress;
  if (typeof r.expiryHeight === "number" && Number.isInteger(r.expiryHeight)) loan.expiryHeight = r.expiryHeight;
  if (isBytes32(r.tagKeyHash)) loan.tagKeyHash = r.tagKeyHash.toLowerCase() as Hex;
  // Kept as stored; whether they vouch for the vault is checked before every use (lib/signers.ts).
  if (Array.isArray(r.vaultSignatures)) {
    const signatures = r.vaultSignatures.filter(
      (s): s is VaultSignature =>
        !!s &&
        typeof s === "object" &&
        Number.isInteger((s as VaultSignature).index) &&
        isHex((s as VaultSignature).signature),
    );
    loan.vaultSignatures = signatures.map((s) => ({ index: s.index, signature: s.signature.toLowerCase() as Hex }));
  }
  if (r.backedUp === true) loan.backedUp = true;
  if (r.demo === true) loan.demo = true;
  if (r.backupVerified === true) loan.backupVerified = true;
  if (isBytes32(r.destinationCommitment)) loan.destinationCommitment = r.destinationCommitment;
  const pc = r.pendingClose as Record<string, unknown> | undefined;
  if (
    pc &&
    typeof pc.nonce === "string" &&
    typeof pc.deadline === "string" &&
    typeof pc.executor === "string" &&
    isAddress(pc.executor, { strict: false })
  ) {
    loan.pendingClose = {
      nonce: pc.nonce,
      deadline: pc.deadline,
      executor: getAddress(pc.executor),
      submittedAt: typeof pc.submittedAt === "number" ? pc.submittedAt : 0,
    };
    if (isBytes32(pc.txHash)) loan.pendingClose.txHash = pc.txHash;
  }
  return loan;
}

/** A new loan: fresh owner key, its Safe address, a random OpenVault request id (any size the signers accept). */
export function createLoan(amount: bigint, now: number = Date.now()): StoredLoan {
  if (!config.denominations.includes(amount)) throw new Error("unsupported collateral amount");
  const ownerKey = generatePrivateKey();
  const owner = privateKeyToAccount(ownerKey).address;
  const requestId = randomBytes32();
  return {
    v: 1,
    id: requestId,
    ownerKey,
    owner,
    account: predictAccountAddress(owner),
    amount: amount.toString(),
    requestId,
    createdAt: now,
  };
}

/**
 * A neutral name for a backup file: nothing in it ties the file to the loan's account, which
 * is public on Base (security review L-11).
 */
export function backupFileName(): string {
  const b = new Uint8Array(4);
  globalThis.crypto.getRandomValues(b);
  return `zecured-loan-backup-${Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("")}.json`;
}

/** JSON backup of one loan (contains the private key). */
export function exportLoan(loan: StoredLoan): string {
  return JSON.stringify(
    {
      type: BACKUP_TYPE,
      version: 1,
      warning:
        "This file contains the private key that controls a Zecured loan. Anyone with it can act on the loan. Keep it offline.",
      loan,
    },
    null,
    2,
  );
}

/** Parses a backup produced by `exportLoan` (or a bare loan record). */
export function importLoan(text: string): StoredLoan {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("not a JSON backup");
  }
  const obj = parsed as Record<string, unknown>;
  if (obj && obj.type === BACKUP_TYPE) {
    if (obj.version !== 1) throw new Error("unsupported backup version");
    return parseLoan(obj.loan);
  }
  return parseLoan(parsed);
}

/**
 * P-8: the restore test of a backup. `text` (the file the user saved, read back) must parse as a
 * backup and restore this very loan: its owner key, hence its account, and its vault once opened.
 */
export function verifyBackup(text: string, loan: StoredLoan): { ok: true } | { ok: false; reason: string } {
  let restored: StoredLoan;
  try {
    restored = importLoan(text);
  } catch (e) {
    return { ok: false, reason: `this file does not restore a loan (${e instanceof Error ? e.message : "unreadable"})` };
  }
  if (restored.ownerKey !== loan.ownerKey || restored.account !== loan.account) {
    return { ok: false, reason: "this file restores another loan's key" };
  }
  if (loan.vaultId && restored.vaultId?.toLowerCase() !== loan.vaultId.toLowerCase()) {
    return { ok: false, reason: "this file was saved before the vault opened: save a new backup now" };
  }
  return { ok: true };
}

/** Loan storage on top of a key/value store; every method is failure-tolerant. */
export class LoanStore {
  /** Last storage error (null if the last operation succeeded). */
  lastError: string | null = null;

  constructor(private readonly storage: KeyValueStorage | null) {}

  get available(): boolean {
    return this.storage !== null;
  }

  private readAll(): Record<string, StoredLoan> {
    if (!this.storage) return {};
    try {
      const raw = this.storage.getItem(STORAGE_KEY);
      this.lastError = null;
      if (!raw) return {};
      const obj = JSON.parse(raw) as Record<string, unknown>;
      const out: Record<string, StoredLoan> = {};
      for (const [id, value] of Object.entries(obj ?? {})) {
        try {
          const loan = parseLoan(value);
          out[id] = { ...loan, id };
        } catch {
          // A corrupted record is skipped, not deleted: the user may still recover it.
        }
      }
      return out;
    } catch (e) {
      this.lastError = e instanceof Error ? e.message : "storage read failed";
      return {};
    }
  }

  private writeAll(all: Record<string, StoredLoan>): boolean {
    if (!this.storage) {
      this.lastError = "local storage is unavailable";
      return false;
    }
    try {
      this.storage.setItem(STORAGE_KEY, JSON.stringify(all));
      this.lastError = null;
      return true;
    } catch (e) {
      this.lastError = e instanceof Error ? e.message : "storage write failed";
      return false;
    }
  }

  list(): StoredLoan[] {
    return Object.values(this.readAll()).sort((a, b) => b.createdAt - a.createdAt);
  }

  get(id: string): StoredLoan | null {
    return this.readAll()[id] ?? null;
  }

  findByVault(vaultId: string): StoredLoan | null {
    const v = vaultId.toLowerCase();
    return this.list().find((l) => l.vaultId?.toLowerCase() === v) ?? null;
  }

  /** Saves (inserts or replaces) a loan. Returns false if it could not be persisted. */
  save(loan: StoredLoan): boolean {
    const all = this.readAll();
    all[loan.id] = loan;
    return this.writeAll(all);
  }

  update(id: string, patch: Partial<Omit<StoredLoan, "v" | "id" | "ownerKey" | "owner" | "account">>): StoredLoan | null {
    const all = this.readAll();
    const current = all[id];
    if (!current) return null;
    const next: StoredLoan = { ...current, ...patch };
    // `undefined` in a patch removes the field.
    for (const [k, value] of Object.entries(patch)) {
      if (value === undefined) delete (next as unknown as Record<string, unknown>)[k];
    }
    all[id] = next;
    return this.writeAll(all) ? next : null;
  }

  /** Forgets one loan's key on this device. */
  remove(id: string): boolean {
    const all = this.readAll();
    delete all[id];
    return this.writeAll(all);
  }

  /** Forgets every loan key on this device. */
  wipe(): boolean {
    if (!this.storage) return false;
    try {
      this.storage.removeItem(STORAGE_KEY);
      this.lastError = null;
      return true;
    } catch (e) {
      this.lastError = e instanceof Error ? e.message : "storage wipe failed";
      return false;
    }
  }
}

let defaultStore: LoanStore | null = null;

/** The browser's loan store (created lazily; on the server it has no storage). */
export function loanStore(): LoanStore {
  if (typeof window === "undefined") return new LoanStore(null);
  defaultStore ??= new LoanStore(browserStorage());
  return defaultStore;
}

/** Short display form of an address or hash: 0x1234…abcd. */
export function short(hex: string, head = 6, tail = 4): string {
  return hex.length <= head + tail + 1 ? hex : `${hex.slice(0, head)}…${hex.slice(-tail)}`;
}
