import { describe, expect, it } from "vitest";
import { concatBytes, keccak256, recoverTypedDataAddress, type Address, type Hex } from "viem";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { createApi, parseRemoteConfig } from "./api";
import { buildConfig } from "./config";
import { destinationTypedData, openVaultTypedData, refundRequestTypedData, viewKeyRequestTypedData } from "./eip712";
import { createLoan } from "./keys";
import {
  checkVaultAddress,
  fetchViewingKey,
  openVault,
  OwnerSigningDisabledError,
  requestRefund,
  submitDestination,
} from "./owner";
import { encodeUnifiedAddress, TYPECODE_ORCHARD, TYPECODE_SAPLING } from "./ua";

const REGISTRY: Address = "0x1111111111111111111111111111111111111111";
const VAULT: Hex = `0x${"ab".repeat(32)}`;
const NOW = 1_700_000_000_000;
const receiver = new Uint8Array(43).fill(0x42);
const vaultUa = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: new Uint8Array(43).fill(0x24) }]);
const destUa = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: receiver }]);

/** A fake backend that records request bodies and answers like the coordinator. */
function backend(handler: (path: string, body: Record<string, string>) => unknown) {
  const bodies: Record<string, string>[] = [];
  const api = createApi({
    baseUrl: "http://backend.test",
    fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input)).pathname;
      const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, string>;
      bodies.push(body);
      return new Response(JSON.stringify(handler(path, body)), { status: 200 });
    }) as typeof fetch,
  });
  return { api, bodies };
}

describe("openVault", () => {
  it("signs OpenVault with the loan key and validates the response", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({
      vaultId: VAULT,
      address: vaultUa,
      zip321: `zcash:${vaultUa}?amount=0.01`,
      amount: "1000000",
      expiryHeight: 100,
    }));
    const res = await openVault(loan, { api, registry: REGISTRY, now: NOW });
    expect(res.vaultId).toBe(VAULT);
    const b = bodies[0]!;
    expect(b).toMatchObject({ owner: loan.owner, account: loan.account, amount: "1000000", requestId: loan.requestId, deadline: "1700000600" });
    const signer = await recoverTypedDataAddress({
      ...openVaultTypedData(REGISTRY, {
        owner: loan.owner,
        account: loan.account,
        amount: 1_000_000n,
        requestId: loan.requestId,
        deadline: 1_700_000_600n,
      }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });

  it("refuses a vault opened for another amount", async () => {
    const loan = createLoan(1_000_000n);
    const { api } = backend(() => ({ vaultId: VAULT, address: vaultUa, zip321: "x", amount: "5000000", expiryHeight: 1 }));
    await expect(openVault(loan, { api, registry: REGISTRY })).rejects.toThrow(/different amount/);
  });
});

describe("checkVaultAddress", () => {
  it("accepts an Ironwood-only testnet UA and builds the ZIP 321 URI locally", () => {
    const r = checkVaultAddress(vaultUa, 1_000_000n, `zcash:${vaultUa}?amount=0.01`);
    expect(r).toEqual({ ok: true, problems: [], uri: `zcash:${vaultUa}?amount=0.01` });
  });

  it("flags a differing backend URI but keeps the local one", () => {
    const r = checkVaultAddress(vaultUa, 1_000_000n, `zcash:${vaultUa}?amount=0.05`);
    expect(r.ok).toBe(true);
    expect(r.uri).toBe(`zcash:${vaultUa}?amount=0.01`);
    expect(r.problems).toHaveLength(1);
  });

  it("refuses addresses that are not Ironwood-only", () => {
    const multi = encodeUnifiedAddress("test", [
      { typecode: TYPECODE_SAPLING, data: new Uint8Array(43).fill(1) },
      { typecode: TYPECODE_ORCHARD, data: new Uint8Array(43).fill(2) },
    ]);
    expect(checkVaultAddress(multi, 1_000_000n)).toMatchObject({ ok: false, uri: null });
    expect(checkVaultAddress("utest1garbage", 1_000_000n).ok).toBe(false);
    const mainnet = encodeUnifiedAddress("main", [{ typecode: TYPECODE_ORCHARD, data: receiver }]);
    expect(checkVaultAddress(mainnet, 1_000_000n).ok).toBe(false);
  });
});

describe("submitDestination", () => {
  it("commits to the Orchard receiver with a fresh salt, signs Destination, checks the echo", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend((_path, body) => ({
      // The coordinator returns keccak256(salt || receiver) of what it decoded.
      commitment: keccak256(concatBytes([Uint8Array.from(Buffer.from(body.salt!.slice(2), "hex")), receiver])),
    }));
    const { commitment } = await submitDestination(loan, VAULT, destUa, { api, registry: REGISTRY, now: NOW });
    const b = bodies[0]!;
    expect(b.receiver).toBe(destUa);
    expect(b.salt).toMatch(/^0x[0-9a-f]{64}$/);
    expect(commitment).toBe(keccak256(concatBytes([Uint8Array.from(Buffer.from(b.salt!.slice(2), "hex")), receiver])));
    const signer = await recoverTypedDataAddress({
      ...destinationTypedData(REGISTRY, { vaultId: VAULT, commitment, nonce: BigInt(b.nonce!), deadline: BigInt(b.deadline!) }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });

  it("stops if the backend acknowledges another commitment", async () => {
    const loan = createLoan(1_000_000n);
    const { api } = backend(() => ({ commitment: `0x${"00".repeat(32)}` }));
    await expect(submitDestination(loan, VAULT, destUa, { api, registry: REGISTRY })).rejects.toThrow(/different commitment/);
  });

  it("rejects invalid destinations before contacting the backend", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({}));
    await expect(submitDestination(loan, VAULT, "tmNotShielded", { api, registry: REGISTRY })).rejects.toThrow();
    expect(bodies).toHaveLength(0);
  });
});

describe("refund and viewing key", () => {
  it("signs a RefundRequest bound to the note and the salted destination", async () => {
    const loan = createLoan(1_000_000n);
    const noteId: Hex = `0x${"cc".repeat(32)}`;
    const { api, bodies } = backend(() => ({ accepted: true }));
    expect(await requestRefund(loan, VAULT, noteId, destUa, { api, registry: REGISTRY })).toBe(true);
    const b = bodies[0]!;
    expect(b.noteId).toBe(noteId);
    const commitment = keccak256(concatBytes([Uint8Array.from(Buffer.from(b.salt!.slice(2), "hex")), receiver]));
    const signer = await recoverTypedDataAddress({
      ...refundRequestTypedData(REGISTRY, { vaultId: VAULT, noteId, commitment, nonce: BigInt(b.nonce!), deadline: BigInt(b.deadline!) }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });

  it("signs a ViewKeyRequest and returns the UFVK", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({ ufvk: "uviewtest1example" }));
    expect(await fetchViewingKey(loan, VAULT, { api, registry: REGISTRY })).toBe("uviewtest1example");
    const b = bodies[0]!;
    const signer = await recoverTypedDataAddress({
      ...viewKeyRequestTypedData(REGISTRY, { vaultId: VAULT, nonce: BigInt(b.nonce!), deadline: BigInt(b.deadline!) }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });
});

describe("owner signatures follow the pinned configuration (H-6)", () => {
  it("signs under the pinned Registry, never one the backend chose", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({ ufvk: "uviewtest1example" }));
    const pinned = buildConfig(DRY_RUN_ENV, parseRemoteConfig(DRY_RUN_CONFIG));
    await fetchViewingKey(loan, VAULT, { api, config: pinned });
    const b = bodies[0]!;
    const signer = await recoverTypedDataAddress({
      ...viewKeyRequestTypedData(DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY, { vaultId: VAULT, nonce: BigInt(b.nonce!), deadline: BigInt(b.deadline!) }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });

  it("refuses every owner request while the pins and the backend disagree, before contacting it", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({}));
    const config = buildConfig(DRY_RUN_ENV, parseRemoteConfig({ ...DRY_RUN_CONFIG, registry: "0x5555555555555555555555555555555555555555" }));
    expect(config.ownerSigning).toBe(false);
    await expect(openVault(loan, { api, config })).rejects.toBeInstanceOf(OwnerSigningDisabledError);
    await expect(submitDestination(loan, VAULT, destUa, { api, config })).rejects.toBeInstanceOf(OwnerSigningDisabledError);
    await expect(requestRefund(loan, VAULT, VAULT, destUa, { api, config })).rejects.toBeInstanceOf(OwnerSigningDisabledError);
    await expect(fetchViewingKey(loan, VAULT, { api, config })).rejects.toThrow(/Owner requests are disabled/);
    expect(bodies).toHaveLength(0);
  });
});
