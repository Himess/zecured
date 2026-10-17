import { describe, expect, it } from "vitest";
import { privateKeyToAccount } from "viem/accounts";
import { config } from "./config";
import {
  BACKUP_TYPE,
  backupFileName,
  createLoan,
  exportLoan,
  importLoan,
  LoanStore,
  parseLoan,
  short,
  STORAGE_KEY,
  verifyBackup,
  type KeyValueStorage,
} from "./keys";
import { predictAccountAddress } from "./safe";

class MemoryStorage implements KeyValueStorage {
  data = new Map<string, string>();
  getItem(k: string) {
    return this.data.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.data.set(k, v);
  }
  removeItem(k: string) {
    this.data.delete(k);
  }
}

/** Every call throws, like localStorage in some privacy modes or when the quota is exceeded. */
class BrokenStorage implements KeyValueStorage {
  getItem(): string | null {
    throw new Error("SecurityError: storage disabled");
  }
  setItem(): void {
    throw new Error("QuotaExceededError");
  }
  removeItem(): void {
    throw new Error("SecurityError: storage disabled");
  }
}

describe("createLoan", () => {
  it("generates a fresh owner key, its Safe address and a request id", () => {
    const a = createLoan(1_000_000n, 1234);
    const b = createLoan(1_000_000n);
    expect(a.ownerKey).toMatch(/^0x[0-9a-f]{64}$/);
    expect(a.owner).toBe(privateKeyToAccount(a.ownerKey).address);
    expect(a.account).toBe(predictAccountAddress(a.owner));
    expect(a.requestId).toMatch(/^0x[0-9a-f]{64}$/);
    expect(a.id).toBe(a.requestId);
    expect(a.amount).toBe("1000000");
    expect(a.createdAt).toBe(1234);
    expect(b.ownerKey).not.toBe(a.ownerKey);
    expect(b.account).not.toBe(a.account);
  });

  it("only accepts standard denominations", () => {
    expect(() => createLoan(2_000_000n)).toThrow(/denomination|amount/);
    expect(() => createLoan(5_000_000n)).not.toThrow();
  });

  it("follows the signers' denominations from GET /config; older backups stay importable", () => {
    const before = config.denominations;
    const old = createLoan(1_000_000n);
    try {
      config.denominations = [5_000_000n, 10_000_000n];
      expect(createLoan(10_000_000n).amount).toBe("10000000");
      expect(() => createLoan(1_000_000n)).toThrow(/amount/);
      expect(importLoan(exportLoan(old))).toEqual(old);
    } finally {
      config.denominations = before;
    }
  });
});

describe("export / import", () => {
  it("names backup files neutrally: nothing ties them to the account (L-11)", () => {
    const loan = createLoan(1_000_000n);
    const a = backupFileName();
    expect(a).toMatch(/^zecured-loan-backup-[0-9a-f]{8}\.json$/);
    expect(backupFileName()).not.toBe(a);
    expect(a.toLowerCase()).not.toContain(loan.account.slice(2, 8).toLowerCase());
  });

  it("round-trips a loan through a JSON backup", () => {
    const loan = {
      ...createLoan(5_000_000n),
      vaultId: `0x${"ab".repeat(32)}` as const,
      vaultAddress: "utest1example",
      expiryHeight: 4_500_000,
      backedUp: true,
    };
    const text = exportLoan(loan);
    expect(JSON.parse(text).type).toBe(BACKUP_TYPE);
    expect(text).toContain("private key");
    expect(importLoan(text)).toEqual(loan);
  });

  it("keeps the signers' vault signatures and tag key hash (C-4); malformed entries are dropped", () => {
    const signature = (i: number) => `0x${String(i).repeat(130)}` as const;
    const loan = {
      ...createLoan(1_000_000n),
      vaultId: `0x${"ab".repeat(32)}` as const,
      vaultAddress: "utest1example",
      expiryHeight: 4_500_000,
      tagKeyHash: `0x${"cd".repeat(32)}` as const,
      vaultSignatures: [1, 2, 3, 4, 5].map((index) => ({ index, signature: signature(index) })),
    };
    expect(importLoan(exportLoan(loan))).toEqual(loan);
    const messy = parseLoan({
      ...loan,
      tagKeyHash: "0x12",
      vaultSignatures: [{ index: 1, signature: signature(1) }, { index: "2", signature: signature(2) }, null, { index: 3 }],
    });
    expect(messy.tagKeyHash).toBeUndefined();
    // A VaultOpened v1 record's hash of the viewing key is not kept: such a vault is not vouched for.
    const { tagKeyHash: _k, ...v1 } = loan;
    expect(parseLoan({ ...v1, ufvkHash: `0x${"cd".repeat(32)}` })).not.toHaveProperty("ufvkHash");
    // What is left is checked against the pinned signer set before any use (lib/signers.ts).
    expect(messy.vaultSignatures).toEqual([{ index: 1, signature: signature(1) }]);
  });

  it("accepts a bare loan record", () => {
    const loan = createLoan(1_000_000n);
    expect(importLoan(JSON.stringify(loan))).toEqual(loan);
  });

  it("rejects backups whose owner or account does not match the key", () => {
    const loan = createLoan(1_000_000n);
    const other = createLoan(1_000_000n);
    expect(() => importLoan(JSON.stringify({ ...loan, owner: other.owner }))).toThrow(/owner/);
    expect(() => importLoan(JSON.stringify({ ...loan, account: other.account }))).toThrow(/account/);
  });

  it("rejects malformed input", () => {
    expect(() => importLoan("not json")).toThrow(/JSON/);
    expect(() => importLoan("{}")).toThrow(/owner key/);
    expect(() => importLoan(JSON.stringify({ type: BACKUP_TYPE, version: 2, loan: {} }))).toThrow(/version/);
    const loan = createLoan(1_000_000n);
    expect(() => importLoan(JSON.stringify({ ...loan, amount: "123" }))).toThrow(/amount/);
    expect(() => importLoan(JSON.stringify({ ...loan, ownerKey: "0x1234" }))).toThrow(/owner key/);
  });

  it("drops unknown fields and normalizes hex", () => {
    const loan = createLoan(1_000_000n);
    const parsed = parseLoan({ ...loan, ownerKey: loan.ownerKey.toUpperCase().replace("0X", "0x"), extra: "x" });
    expect(parsed.ownerKey).toBe(loan.ownerKey);
    expect("extra" in parsed).toBe(false);
  });
});

describe("LoanStore", () => {
  it("saves, lists (newest first), finds by vault, updates and removes", () => {
    const store = new LoanStore(new MemoryStorage());
    const a = createLoan(1_000_000n, 1);
    const b = createLoan(5_000_000n, 2);
    expect(store.save(a)).toBe(true);
    expect(store.save(b)).toBe(true);
    expect(store.list().map((l) => l.id)).toEqual([b.id, a.id]);

    const vaultId = `0x${"cd".repeat(32)}` as const;
    const updated = store.update(a.id, { vaultId, expiryHeight: 42 });
    expect(updated?.vaultId).toBe(vaultId);
    expect(store.findByVault(vaultId.toUpperCase().replace("0X", "0x"))?.id).toBe(a.id);
    expect(store.get(a.id)?.expiryHeight).toBe(42);

    // `undefined` in a patch clears the field.
    store.update(a.id, { expiryHeight: undefined });
    expect(store.get(a.id)?.expiryHeight).toBeUndefined();

    expect(store.remove(a.id)).toBe(true);
    expect(store.get(a.id)).toBeNull();
    expect(store.list()).toHaveLength(1);
  });

  it("wipes everything", () => {
    const mem = new MemoryStorage();
    const store = new LoanStore(mem);
    store.save(createLoan(1_000_000n));
    expect(store.wipe()).toBe(true);
    expect(store.list()).toEqual([]);
    expect(mem.data.has(STORAGE_KEY)).toBe(false);
  });

  it("skips corrupted records instead of failing", () => {
    const mem = new MemoryStorage();
    const good = createLoan(1_000_000n);
    mem.setItem(STORAGE_KEY, JSON.stringify({ [good.id]: good, bad: { ownerKey: "nope" } }));
    const store = new LoanStore(mem);
    expect(store.list().map((l) => l.id)).toEqual([good.id]);
    mem.setItem(STORAGE_KEY, "{not json");
    expect(store.list()).toEqual([]);
    expect(store.lastError).toBeTruthy();
  });

  it("never throws when storage is broken or missing", () => {
    for (const store of [new LoanStore(new BrokenStorage()), new LoanStore(null)]) {
      const loan = createLoan(1_000_000n);
      expect(store.list()).toEqual([]);
      expect(store.get("x")).toBeNull();
      expect(store.save(loan)).toBe(false);
      expect(store.update(loan.id, { backedUp: true })).toBeNull();
      expect(store.remove(loan.id)).toBe(false);
      expect(store.wipe()).toBe(false);
      expect(store.lastError).toBeTruthy();
    }
  });
});

describe("short", () => {
  it("shortens long hex only", () => {
    expect(short("0x1234567890abcdef1234")).toBe("0x1234…1234");
    expect(short("0x1234")).toBe("0x1234");
  });
});

describe("P-8: the backup's restore test", () => {
  const VAULT = `0x${"ab".repeat(32)}` as const;

  it("passes only for a file that restores this very loan, vault included", () => {
    const loan = { ...createLoan(config.denominations[0]!), vaultId: VAULT };
    expect(verifyBackup(exportLoan(loan), loan)).toEqual({ ok: true });
    // A bare record (pasted) restores too.
    expect(verifyBackup(JSON.stringify(loan), loan)).toEqual({ ok: true });
    const other = createLoan(config.denominations[0]!);
    expect(verifyBackup(exportLoan({ ...other, vaultId: VAULT }), loan)).toMatchObject({ ok: false, reason: expect.stringMatching(/another loan/) });
    const early = { ...loan, vaultId: undefined };
    expect(verifyBackup(exportLoan(early), loan)).toMatchObject({ ok: false, reason: expect.stringMatching(/before the vault opened/) });
    expect(verifyBackup("not json", loan)).toMatchObject({ ok: false, reason: expect.stringMatching(/does not restore a loan/) });
  });

  it("keeps the demo and verified flags of a loan record", () => {
    const loan = { ...createLoan(config.denominations[0]!), demo: true, backupVerified: true };
    expect(parseLoan(JSON.parse(JSON.stringify(loan)))).toMatchObject({ demo: true, backupVerified: true });
    expect(parseLoan({ ...loan, demo: "yes" }).demo).toBeUndefined();
  });
});
