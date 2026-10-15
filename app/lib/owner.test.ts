import { describe, expect, it } from "vitest";
import { concatBytes, keccak256, recoverTypedDataAddress, type Address, type Hex } from "viem";
import { generatePrivateKey, privateKeyToAccount } from "viem/accounts";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { createApi, parseRemoteConfig } from "./api";
import { buildConfig } from "./config";
import {
  destinationTypedData,
  openVaultTypedData,
  refundRequestTypedData,
  vaultOpenedTypedData,
  viewKeyRequestTypedData,
} from "./eip712";
import { createLoan, type StoredLoan } from "./keys";
import {
  checkVaultAddress,
  fetchViewingKey,
  openVault,
  OwnerSigningDisabledError,
  requestRefund,
  submitDestination,
} from "./owner";
import { tagKeyHash, textHash, vaultOpenedMessage, VaultProofError } from "./signers";
import { encodeUnifiedAddress, TYPECODE_ORCHARD, TYPECODE_SAPLING } from "./ua";

const REGISTRY: Address = "0x1111111111111111111111111111111111111111";
const VAULT: Hex = `0x${"ab".repeat(32)}`;
const NOW = 1_700_000_000_000;
const receiver = new Uint8Array(43).fill(0x42);
const vaultUa = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: new Uint8Array(43).fill(0x24) }]);
const destUa = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: receiver }]);

const UFVK = "uviewtest1vaultkey";
const EXPIRY = 4_486_288;
/** The five signer nodes' attestation keys; tests pin their addresses (`ctx.signers`). */
const SIGNER_KEYS = Array.from({ length: 5 }, () => generatePrivateKey());
const SIGNERS = SIGNER_KEYS.map((k) => privateKeyToAccount(k).address);

/** Every signer's VaultOpened signature for `loan`'s request and the vault `address`, by `keys`. */
async function vaultSignatures(loan: StoredLoan, address: string, keys: readonly Hex[] = SIGNER_KEYS, ufvk = UFVK) {
  const message = vaultOpenedMessage({
    vaultId: VAULT,
    owner: loan.owner,
    account: loan.account,
    amount: BigInt(loan.amount),
    expiryHeight: EXPIRY,
    address,
    tagKeyHash: tagKeyHash(ufvk),
  });
  const typedData = vaultOpenedTypedData(REGISTRY, message);
  return Promise.all(keys.map(async (k, i) => ({ index: i + 1, signature: await privateKeyToAccount(k).signTypedData(typedData) })));
}

/** POST /vaults as the coordinator answers it (signatures for `signedAddress`, address `address`). */
async function opened(loan: StoredLoan, opts: { address?: string; signedAddress?: string; keys?: readonly Hex[] } = {}) {
  const address = opts.address ?? vaultUa;
  return {
    vaultId: VAULT,
    address,
    zip321: `zcash:${address}?amount=0.01`,
    amount: loan.amount,
    expiryHeight: EXPIRY,
    tagKeyHash: tagKeyHash(UFVK),
    signatures: await vaultSignatures(loan, opts.signedAddress ?? address, opts.keys),
  };
}

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
    const answer = await opened(loan);
    const { api, bodies } = backend(() => answer);
    const res = await openVault(loan, { api, registry: REGISTRY, signers: SIGNERS, now: NOW });
    expect(res).toMatchObject({ vaultId: VAULT, address: vaultUa, expiryHeight: EXPIRY, tagKeyHash: tagKeyHash(UFVK), check: "pinned" });
    expect(res.signatures).toHaveLength(5);
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
    const answer = { ...(await opened(loan)), amount: "5000000" };
    const { api } = backend(() => answer);
    await expect(openVault(loan, { api, registry: REGISTRY, signers: SIGNERS })).rejects.toThrow(/different amount/);
  });

  /** Security review C-4: a compromised backend hands out an address of its own. */
  it("refuses a vault that not every pinned signer vouches for", async () => {
    const loan = createLoan(1_000_000n);
    const attacker = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: new Uint8Array(43).fill(0x66) }]);
    const ctx = { registry: REGISTRY, signers: SIGNERS };
    const answers = [
      // The signers' signatures, relayed with the backend's own (well-formed, Ironwood-only) address.
      await opened(loan, { address: attacker, signedAddress: vaultUa }),
      // The backend's address, "signed" by keys of its own.
      await opened(loan, { address: attacker, keys: Array.from({ length: 5 }, () => generatePrivateKey()) }),
      // Four of the five signers.
      { ...(await opened(loan)), signatures: (await opened(loan)).signatures.slice(1) },
      // Another viewing key's tag key than the one they signed.
      { ...(await opened(loan)), tagKeyHash: tagKeyHash("uviewtest1attacker") },
      // VaultOpened v1's hash of the viewing key itself.
      { ...(await opened(loan)), tagKeyHash: textHash(UFVK) },
    ];
    for (const answer of answers) {
      const { api } = backend(() => answer);
      await expect(openVault(loan, { api, ...ctx })).rejects.toBeInstanceOf(VaultProofError);
    }
    // An answer without signatures is not even parsed.
    const { signatures: _dropped, ...unsigned } = await opened(loan);
    const { api } = backend(() => unsigned);
    await expect(openVault(loan, { api, ...ctx })).rejects.toThrow(/signatures/);
  });

  it("a production build without a pinned signer set opens no vault", async () => {
    const loan = createLoan(1_000_000n);
    const { api, bodies } = backend(() => ({}));
    const config = buildConfig({ ...DRY_RUN_ENV, NEXT_PUBLIC_SIGNERS: undefined });
    expect(config.ownerSigning).toBe(true);
    await expect(openVault(loan, { api, config })).rejects.toBeInstanceOf(VaultProofError);
    expect(bodies).toHaveLength(0);
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
    // A loan without the signers' vault signatures (opened before them): returned, unchecked.
    expect(await fetchViewingKey(loan, VAULT, { api, registry: REGISTRY })).toEqual({ ufvk: "uviewtest1example", checked: false });
    const b = bodies[0]!;
    const signer = await recoverTypedDataAddress({
      ...viewKeyRequestTypedData(REGISTRY, { vaultId: VAULT, nonce: BigInt(b.nonce!), deadline: BigInt(b.deadline!) }),
      signature: b.signature as Hex,
    });
    expect(signer).toBe(loan.owner);
  });
});

describe("the viewing key is the one the signers signed (C-4)", () => {
  async function signedLoan() {
    const loan = createLoan(1_000_000n);
    return {
      ...loan,
      vaultId: VAULT,
      vaultAddress: vaultUa,
      expiryHeight: EXPIRY,
      tagKeyHash: tagKeyHash(UFVK),
      vaultSignatures: await vaultSignatures(loan, vaultUa),
    };
  }
  const ctx = { registry: REGISTRY, signers: SIGNERS };

  it("shows a key whose tag key hashes to the signed tagKeyHash", async () => {
    const loan = await signedLoan();
    const { api } = backend(() => ({ ufvk: UFVK }));
    expect(await fetchViewingKey(loan, VAULT, { api, ...ctx })).toEqual({ ufvk: UFVK, checked: true });
  });

  it("refuses the backend's own key (one that would match an address of its own)", async () => {
    const loan = await signedLoan();
    const { api } = backend(() => ({ ufvk: "uviewtest1attacker" }));
    await expect(fetchViewingKey(loan, VAULT, { api, ...ctx })).rejects.toBeInstanceOf(VaultProofError);
  });

  it("trusts the stored hash only while the signatures over it verify", async () => {
    const loan = await signedLoan();
    const { api, bodies } = backend(() => ({ ufvk: "uviewtest1attacker" }));
    // A record whose hash was swapped (say, in an imported backup) is refused before any request.
    const tampered = { ...loan, tagKeyHash: tagKeyHash("uviewtest1attacker") };
    await expect(fetchViewingKey(tampered, VAULT, { api, ...ctx })).rejects.toBeInstanceOf(VaultProofError);
    await expect(fetchViewingKey(loan, `0x${"cd".repeat(32)}`, { api, ...ctx })).rejects.toThrow(/another vault/);
    expect(bodies).toHaveLength(0);
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
