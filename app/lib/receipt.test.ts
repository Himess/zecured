import { describe, expect, it } from "vitest";
import { hashTypedData, keccak256, toBytes, type Address, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import type { LivenessReceipt } from "./api";
import {
  armedNote,
  armingState,
  bondDomain,
  checkReceipt,
  DESTINATION_RECEIVED_TYPES,
  destinationReceivedTypedData,
  rememberArming,
  rememberedArmings,
  wasArmedHere,
  type ArmableClaim,
} from "./receipt";

/** Anvil's first two test keys: public, for test vectors only. */
const SIGNER_KEY: Hex = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const STRANGER_KEY: Hex = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const SIGNER = privateKeyToAccount(SIGNER_KEY).address;
const BOND: Address = "0xf42821eF06F68a01bba08c91a16368A83C002586";
const CLAIM: Hex = `0x${"c1".repeat(32)}`;
const COMMITMENT: Hex = `0x${"cd".repeat(32)}`;
const ACCOUNT: Address = "0x3333333333333333333333333333333333333333";
const NOW = 1_800_000_000;
const SIGNERS: Address[] = [SIGNER, "0x492C397388982CFdDe71cA67d3C30E2Bc9e6643B"];

async function receipt(patch: Partial<LivenessReceipt> = {}, key: Hex = SIGNER_KEY): Promise<LivenessReceipt> {
  const message = { claimId: patch.claimId ?? CLAIM, commitment: patch.commitment ?? COMMITMENT, receivedAt: patch.receivedAt ?? BigInt(NOW - 60) };
  const signature = await privateKeyToAccount(key).signTypedData(destinationReceivedTypedData(BOND, message, 84532));
  return { ...message, signer: privateKeyToAccount(key).address, signature, ...patch };
}

describe("R-5 receipts: DestinationReceived under SignerBondV2's domain (V4 freeze section 3)", () => {
  it("has the frozen type and domain", () => {
    expect(DESTINATION_RECEIVED_TYPES.DestinationReceived.map((f) => `${f.type} ${f.name}`).join(",")).toBe(
      "bytes32 claimId,bytes32 commitment,uint64 receivedAt",
    );
    expect(bondDomain(BOND, 84532)).toEqual({ name: "ZecuredSignerBond", version: "1", chainId: 84532, verifyingContract: BOND });
    // The typehash SignerBondV2 hard-codes: keccak256 of the type string.
    const typehash = keccak256(toBytes("DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)"));
    expect(typehash).toMatch(/^0x[0-9a-f]{64}$/);
    const digest = hashTypedData(destinationReceivedTypedData(BOND, { claimId: CLAIM, commitment: COMMITMENT, receivedAt: 1n }, 84532));
    expect(digest).not.toBe(hashTypedData(destinationReceivedTypedData(BOND, { claimId: CLAIM, commitment: COMMITMENT, receivedAt: 2n }, 84532)));
  });

  const ctx: { bond: Address; signers: readonly Address[] | null; chainTime: number; chainId: number } = {
    bond: BOND,
    signers: SIGNERS,
    chainTime: NOW,
    chainId: 84532,
  };
  const claim = { claimId: CLAIM, commitment: COMMITMENT };

  it("accepts a pinned signer's receipt for the claim's current destination", async () => {
    expect(await checkReceipt(await receipt(), claim, ctx)).toEqual({ ok: true });
    // Dated this very second is fine (receivedAt <= block.timestamp).
    expect(await checkReceipt(await receipt({ receivedAt: BigInt(NOW) }), claim, ctx)).toEqual({ ok: true });
  });

  it("refuses another claim, an earlier destination, a date in the future, a stranger, a forged signer", async () => {
    const refuse = async (r: LivenessReceipt, c = claim, x = ctx) => {
      const out = await checkReceipt(r, c, x);
      expect(out.ok).toBe(false);
      return out.ok ? "" : out.reason;
    };
    expect(await refuse(await receipt({ claimId: `0x${"c2".repeat(32)}` }))).toMatch(/another claim/);
    expect(await refuse(await receipt(), { claimId: CLAIM, commitment: `0x${"ee".repeat(32)}` })).toMatch(/earlier destination/);
    expect(await refuse(await receipt(), { claimId: CLAIM, commitment: `0x${"00".repeat(32)}` })).toMatch(/no destination/);
    expect(await refuse(await receipt({ receivedAt: BigInt(NOW + 1) }))).toMatch(/future/);
    expect(await refuse(await receipt({}, STRANGER_KEY))).toMatch(/not from one of the five signers/);
    // A stranger's signature presented as a signer's.
    const forged = { ...(await receipt({}, STRANGER_KEY)), signer: SIGNER };
    expect(await refuse(forged)).toMatch(/not its signer's/);
    // Signed for another bond (another domain).
    const otherBond = await privateKeyToAccount(SIGNER_KEY).signTypedData(
      destinationReceivedTypedData("0x000000000000000000000000000000000000bEEF", { claimId: CLAIM, commitment: COMMITMENT, receivedAt: BigInt(NOW - 60) }, 84532),
    );
    expect(await refuse({ ...(await receipt()), signature: otherBond })).toMatch(/not its signer's/);
    expect(await refuse(await receipt(), claim, { ...ctx, signers: null })).toMatch(/pins no signer set/);
    expect(await refuse({ ...(await receipt()), signature: "0x1234" })).toMatch(/unreadable/);
  });
});

describe("which of the account's claims to arm", () => {
  const base: ArmableClaim = {
    claimId: CLAIM,
    kind: "Release",
    status: "Pending",
    commitment: COMMITMENT,
    bondPaid: false,
    claimant: ACCOUNT,
    liveness: null,
    arming: { commitment: `0x${"00".repeat(32)}`, payableAt: 0 },
  };
  const ctx = { account: ACCOUNT, frozen: false, compromised: false };

  it("an unarmed own claim with a destination, on a v4 bond", () => {
    expect(armingState(base, ctx)).toEqual({ kind: "unarmed" });
    for (const kind of ["CloseUser", "Refund"]) expect(armingState({ ...base, kind }, ctx).kind).toBe("unarmed");
    // Armed for an earlier destination (a refund's new destination voids the arming).
    expect(armingState({ ...base, arming: { commitment: `0x${"ee".repeat(32)}`, payableAt: NOW } }, ctx)).toEqual({ kind: "unarmed" });
  });

  it("armed for the current destination: from when, and when the bonds pay", () => {
    const armed = { ...base, arming: { commitment: COMMITMENT, payableAt: NOW - 100 } };
    expect(armingState(armed, ctx)).toEqual({ kind: "armed", payableAt: NOW - 100, dueAt: null });
    expect(armingState({ ...armed, liveness: { dueAt: NOW + 86_400 } }, ctx)).toEqual({ kind: "armed", payableAt: NOW - 100, dueAt: NOW + 86_400 });
  });

  it("nothing to arm: not the account's, settled, without destination, bond-paid, frozen, compromised, a v3 bond", () => {
    const none = (c: Partial<ArmableClaim>, x = ctx) => expect(armingState({ ...base, ...c }, x)).toEqual({ kind: "none" });
    none({ claimant: "0x5555555555555555555555555555555555555555" });
    none({ kind: "Liquidation" });
    none({ kind: "Fee", claimant: "0x0000000000000000000000000000000000000000" }, { ...ctx, account: "0x0000000000000000000000000000000000000000" });
    none({ status: "Paid" });
    none({ commitment: `0x${"00".repeat(32)}` });
    none({ bondPaid: true });
    none({}, { ...ctx, frozen: true });
    none({}, { ...ctx, compromised: true });
    none({ arming: null });
  });
});

describe("what the page says about an armed claim", () => {
  const fmt = (s: number) => `t${s}`;

  it("when the bonds pay it if it is still unpaid then, or that they would pay nothing now (livenessDue (0, 0))", () => {
    expect(armedNote({ payableAt: 100, dueAt: 86_500 }, fmt)).toBe(
      "Bonds' guarantee armed (a signer confirmed the destination at t100): if it is still unpaid at t86500, the signers' bonds pay you instead.",
    );
    const stale = armedNote({ payableAt: 100, dueAt: null }, fmt);
    expect(stale).toMatch(/would pay nothing \(their price feed is stale, or the compensator owes this claim\); it stays payable on Zcash/);
    expect(stale).not.toMatch(/still unpaid at/);
  });
});

describe("what this browser already armed", () => {
  function memory(): Pick<Storage, "getItem" | "setItem"> & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v) };
  }

  it("remembers each claim and destination once, case-insensitively, and survives broken storage", () => {
    const s = memory();
    expect(wasArmedHere(rememberedArmings(s), CLAIM, COMMITMENT)).toBe(false);
    rememberArming(s, CLAIM, COMMITMENT);
    rememberArming(s, CLAIM, COMMITMENT);
    const seen = rememberedArmings(s);
    expect(seen.size).toBe(1);
    expect(wasArmedHere(seen, CLAIM.toUpperCase().replace("0X", "0x") as Hex, COMMITMENT)).toBe(true);
    expect(wasArmedHere(seen, CLAIM, `0x${"ee".repeat(32)}`)).toBe(false);
    s.data.set("zecured:armed:v1", "not json");
    expect(rememberedArmings(s).size).toBe(0);
    expect(rememberedArmings(null).size).toBe(0);
    expect(() => rememberArming(null, CLAIM, COMMITMENT)).not.toThrow();
  });
});
