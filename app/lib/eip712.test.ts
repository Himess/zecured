import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  concatHex,
  encodeAbiParameters,
  getTypesForEIP712Domain,
  hashDomain,
  hashStruct,
  hashTypedData,
  keccak256,
  recoverAddress,
  recoverTypedDataAddress,
  toBytes,
  type Address,
  type Hex,
  type TypedDataDefinition,
  type TypedDataDomain,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import {
  adapterDomain,
  closeRequestDigest,
  closeRequestTypedData,
  deadlineIn,
  destinationTypedData,
  openVaultTypedData,
  randomNonce128,
  randomNonce64,
  refundRequestTypedData,
  registryDomain,
  safeMessageHash,
  signCloseRequestAsSafe,
  signWithOwnerKey,
  typeString,
  types,
  viewKeyRequestTypedData,
  type CloseRequestMessage,
  type PrimaryType,
} from "./eip712";

// ------------------------------------------------------------------ the spec

const INTERFACES = readFileSync(fileURLToPath(new URL("../../docs/INTERFACES.md", import.meta.url)), "utf8");

/** Every `Name(type name,…)` line of docs/INTERFACES.md § 2. */
function specTypeStrings(): Map<string, string> {
  const start = INTERFACES.indexOf("## 2. EIP-712");
  const end = INTERFACES.indexOf("## 3.", start);
  const section = INTERFACES.slice(start, end);
  const out = new Map<string, string>();
  for (const m of section.matchAll(/^([A-Z][A-Za-z]+)\(([^)]*)\)$/gm)) out.set(m[1]!, m[0]);
  return out;
}

// ------------------------------------------------------------------ fixed vector inputs
// Same inputs as contracts/test/unit/ZecuredEIP712.t.sol (golden vectors).
const REGISTRY: Address = "0x1111111111111111111111111111111111111111";
const ADAPTER: Address = "0x2222222222222222222222222222222222222222";
const ACCOUNT: Address = "0x3333333333333333333333333333333333333333";
const EXECUTOR: Address = "0x4444444444444444444444444444444444444444";
const VAULT_ID: Hex = "0x5555555555555555555555555555555555555555555555555555555555555555";
const DEPOSIT_TAG: Hex = "0x6666666666666666666666666666666666666666666666666666666666666666";
const USER_COMMITMENT: Hex = "0x7777777777777777777777777777777777777777777777777777777777777777";
const DEADLINE = 1_800_000_000n;

const CLOSE_REQUEST: CloseRequestMessage = {
  vaultId: VAULT_ID,
  account: ACCOUNT,
  executor: EXECUTOR,
  marginBps: 100,
  minUserAmount: 500_000n,
  userCommitment: USER_COMMITMENT,
  nonce: 42n,
  deadline: DEADLINE,
};

/** Foundry / anvil test account #0 — a publicly known key, used for test vectors only. */
const TEST_KEY: Hex = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const TEST_OWNER: Address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/** A sample value per Solidity type, distinct per field. */
function sample(type: string, i: number): unknown {
  const byte = (i + 1).toString(16).padStart(2, "0");
  switch (type) {
    case "bytes32":
      return `0x${byte.repeat(32)}`;
    case "address":
      return `0x${byte.repeat(20)}`;
    default:
      return BigInt(1000 + i);
  }
}

describe("type strings (docs/INTERFACES.md § 2)", () => {
  const spec = specTypeStrings();

  it("the spec defines exactly the app's nine types", () => {
    expect([...spec.keys()].sort()).toEqual(Object.keys(types).sort());
  });

  it.each(Object.keys(types) as PrimaryType[])("%s matches the spec byte for byte", (name) => {
    expect(typeString(name)).toBe(spec.get(name));
  });

  it.each(Object.keys(types) as PrimaryType[])(
    "viem hashes %s with the spec's type string (typehash = keccak256(spec string))",
    (name) => {
      const fields = types[name];
      const data = Object.fromEntries(fields.map((f, i) => [f.name, sample(f.type, i)]));
      const viemHash = hashStruct({ data, primaryType: name, types: { [name]: fields } });
      const manual = keccak256(
        encodeAbiParameters(
          [{ type: "bytes32" }, ...fields.map((f) => ({ type: f.type }))],
          [keccak256(toBytes(spec.get(name)!)), ...fields.map((f, i) => sample(f.type, i))],
        ),
      );
      expect(viemHash).toBe(manual);
    },
  );
});

describe("domains and digests (Solidity golden vectors)", () => {
  const separator = (domain: TypedDataDomain): Hex =>
    hashDomain({ domain, types: { EIP712Domain: getTypesForEIP712Domain({ domain }) } } as Parameters<typeof hashDomain>[0]);

  it("domain separators", () => {
    expect(separator(registryDomain(REGISTRY))).toBe("0x489271068185e1811817d2dfae78e2be898ee5fc9768bf01d870981f667ef9f8");
    expect(separator(adapterDomain(ADAPTER))).toBe("0xb62551e236af33b3b84da5fe38134bcea7c80b912f8b172bb4020080457a9260");
    // contracts/deployments/base-sepolia.dry-run.json (computed on a fork by the contracts).
    expect(separator(registryDomain("0xbdB942d882882bF795aA9c0Cd20ADe2115c73AAc"))).toBe(
      "0x69d16596f5fe8b56a5a842fcca4461f0bef9d0a76fc625604317d9581a6b74c7",
    );
    expect(separator(adapterDomain("0xC0a2A554dcFB5a1bd7e5E769A2Ad0c33BA0500fc"))).toBe(
      "0x59e17d33c50be097bde376a1c88c46c405743fb47b43fea0eb1fac767095e5d4",
    );
  });

  it("DepositAttestation digest", () => {
    const digest = hashTypedData({
      domain: registryDomain(REGISTRY),
      types: { DepositAttestation: types.DepositAttestation },
      primaryType: "DepositAttestation",
      message: { vaultId: VAULT_ID, account: ACCOUNT, amount: 1_000_000n, depositTag: DEPOSIT_TAG },
    });
    expect(digest).toBe("0xbe0251d9c909749194da8ebf137e42a33a60d6fba8672a022c4923558fecb90f");
  });

  it("PayoutConfirmation digest (claimIdsHash = keccak256 of packed claim ids)", () => {
    const claimId = (seq: bigint) => keccak256(encodeAbiParameters([{ type: "bytes32" }, { type: "uint64" }], [VAULT_ID, seq]));
    const claimIdsHash = keccak256(concatHex([claimId(0n), claimId(1n)]));
    const digest = hashTypedData({
      domain: registryDomain(REGISTRY),
      types: { PayoutConfirmation: types.PayoutConfirmation },
      primaryType: "PayoutConfirmation",
      message: { vaultId: VAULT_ID, claimIdsHash },
    });
    expect(digest).toBe("0xf166a0191609bbccd2b7aa9e7527b1553a418f7626902cc10099034b838e4973");
  });

  it("CloseRequest digest", () => {
    expect(closeRequestDigest(ADAPTER, CLOSE_REQUEST)).toBe(
      "0x2207e7a8cd1b3966d87015d4b554f9271a760f2854c1c9d37ae0391b885ae8f7",
    );
    expect(hashTypedData(closeRequestTypedData(ADAPTER, CLOSE_REQUEST))).toBe(closeRequestDigest(ADAPTER, CLOSE_REQUEST));
  });

  it("changing any CloseRequest field changes the digest", () => {
    const base = closeRequestDigest(ADAPTER, CLOSE_REQUEST);
    const variants: Partial<CloseRequestMessage>[] = [
      { vaultId: USER_COMMITMENT },
      { account: EXECUTOR },
      { executor: ACCOUNT },
      { marginBps: 101 },
      { minUserAmount: 500_001n },
      { userCommitment: VAULT_ID },
      { nonce: 43n },
      { deadline: DEADLINE + 1n },
    ];
    for (const v of variants) expect(closeRequestDigest(ADAPTER, { ...CLOSE_REQUEST, ...v })).not.toBe(base);
  });
});

describe("owner request signatures (cross-checked with Foundry `cast wallet sign --data`)", () => {
  const msgs = {
    OpenVault: openVaultTypedData(REGISTRY, {
      owner: TEST_OWNER,
      account: ACCOUNT,
      amount: 1_000_000n,
      requestId: "0x8888888888888888888888888888888888888888888888888888888888888888",
      deadline: DEADLINE,
    }),
    Destination: destinationTypedData(REGISTRY, { vaultId: VAULT_ID, commitment: USER_COMMITMENT, nonce: 7n, deadline: DEADLINE }),
    RefundRequest: refundRequestTypedData(REGISTRY, {
      vaultId: VAULT_ID,
      noteId: "0x9999999999999999999999999999999999999999999999999999999999999999",
      commitment: USER_COMMITMENT,
      nonce: 8n,
      deadline: DEADLINE,
    }),
    ViewKeyRequest: viewKeyRequestTypedData(REGISTRY, { vaultId: VAULT_ID, nonce: 9n, deadline: DEADLINE }),
  };
  // Produced by alloy (cast 1.4.2) from the equivalent typed-data JSON and TEST_KEY.
  const castSignatures = {
    OpenVault:
      "0xe72475a384072a8d65fcd4f4a7a90d82ea83548acef5ecd3174aa6ed2690c17c35592f2bfe2be0d2c391db7e1a511a8abc46bd9213b5406a41b44a2ee2eaa35e1b",
    Destination:
      "0x0afa9061a6e7284fbd1d1bc3befa8df60cc8cd3e9a604b2e04e5e20b6d91e4c94ff4e34ea82be94c17d0d276f3a6802ea4c7fbb7e9c825077abb22123b3fd68b1c",
    RefundRequest:
      "0x7fff48e93c3137aba204e91bb7e0460057f8283e508d7bb36899640837c344900e2041acac826753fdac7919c03d159ab880cf5cbedc73629794217bcbec2df71c",
    ViewKeyRequest:
      "0x055866306057077a8068bb9da720ef5ee8ab98b52bfef6963e51f6c43c42ad8139a9d2873ebc395615a102f6e14e1503ce53741c372e61a590ebc58e97bdf2d31c",
  } as const;

  it.each(Object.keys(msgs) as (keyof typeof msgs)[])("%s: same 65-byte signature as alloy, recovers to the owner", async (k) => {
    const td = msgs[k] as unknown as TypedDataDefinition;
    const sig = await signWithOwnerKey(TEST_KEY, td);
    expect(sig).toBe(castSignatures[k]);
    expect(sig.length).toBe(2 + 65 * 2);
    expect(["1b", "1c"]).toContain(sig.slice(-2)); // v = 27/28, as the signers' recover_typed expects
    expect(await recoverTypedDataAddress({ ...td, signature: sig })).toBe(TEST_OWNER);
  });

  it("a signature for one request kind does not verify as another", async () => {
    const sig = await signWithOwnerKey(TEST_KEY, msgs.Destination);
    const asRefund = await recoverTypedDataAddress({ ...(msgs.ViewKeyRequest as unknown as TypedDataDefinition), signature: sig });
    expect(asRefund).not.toBe(TEST_OWNER);
  });
});

describe("CloseRequest through the Safe (ERC-1271, docs/spikes/SPIKE_C.md)", () => {
  // safeHash and signature computed independently with cast (keccak / abi-encode, then
  // `cast wallet sign --no-hash`) from the golden CloseRequest digest.
  const SAFE_HASH = "0x2eeda3540aabc3968e4b2423b1aa1eff7941fbaed0af7f323d75010398345521";
  const SAFE_SIG =
    "0xd7fe7fc389a3b9f8fee113bc5757f67c5c2ddd958ce97b39a23a93660df8d0dd648d5a4d0f9798a4c53cbd3677891e01840a4d49bfd3951a11c75a8f1ae9ea6e1c";

  it("wraps the Adapter digest in SafeMessage under the Safe's domain", () => {
    expect(safeMessageHash(ACCOUNT, closeRequestDigest(ADAPTER, CLOSE_REQUEST))).toBe(SAFE_HASH);
  });

  it("signs safeHash (not the raw digest) with v = 27/28", async () => {
    const sig = await signCloseRequestAsSafe(TEST_KEY, ADAPTER, ACCOUNT, CLOSE_REQUEST);
    expect(sig).toBe(SAFE_SIG);
    expect(await recoverAddress({ hash: SAFE_HASH, signature: sig })).toBe(TEST_OWNER);
    // The likeliest integration bug (rejected by the Safe): signing the raw digest.
    const raw = await privateKeyToAccount(TEST_KEY).sign({ hash: closeRequestDigest(ADAPTER, CLOSE_REQUEST) });
    expect(raw).not.toBe(sig);
  });

  it("refuses a request whose account is not the signing Safe", async () => {
    await expect(signCloseRequestAsSafe(TEST_KEY, ADAPTER, EXECUTOR, CLOSE_REQUEST)).rejects.toThrow(/account/);
  });
});

describe("request parameters", () => {
  it("nonces are random and in range", () => {
    const a = randomNonce64();
    expect(a > 0n && a <= 2n ** 48n).toBe(true);
    expect(randomNonce64()).not.toBe(a);
    const b = randomNonce128();
    expect(b < 2n ** 128n).toBe(true);
  });

  it("deadlines are Unix seconds", () => {
    expect(deadlineIn(600n, 1_700_000_000_123)).toBe(1_700_000_600n);
  });
});
