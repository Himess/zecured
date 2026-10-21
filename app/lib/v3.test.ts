import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { Address, Hex } from "viem";
import type { MintBlock, MintingStatus } from "./api";
import { formatUtc } from "./calls";
import type { LedgerClaim, PendingActivationView, VaultLedger } from "./chain";
import {
  abandonableAt,
  bondPayment,
  bondPaymentExplainer,
  claimView,
  compensatedClaimIds,
  COMPENSATION_SENTENCE,
  disputeState,
  EXITS_NEVER_STOP,
  feeClaim,
  feeNotice,
  formatPeriod,
  mintingNotices,
  needsRefundDestination,
  openGate,
  openingFee,
  PAUSE_SENTENCE,
  pendingState,
  REFUND_REASON_TEXT,
  refundClaims,
  refundsToClaim,
} from "./v3";

const NOW = 1_800_000_000;
const ACCOUNT: Address = "0x3333333333333333333333333333333333333333";
const OTHER: Address = "0x5555555555555555555555555555555555555555";
const ZERO: Address = "0x0000000000000000000000000000000000000000";
const NONE32: Hex = `0x${"00".repeat(32)}`;
const COMMITMENT: Hex = `0x${"cd".repeat(32)}`;
/** The judge deployment's fee (contracts/config/zecured-base-sepolia.json): 0.5 %, waived under 20,000 zatoshi. */
const FEE = { feeBps: 50, minFeeZat: 20_000n };
const DENOMINATIONS = [1_000_000n, 5_000_000n, 12_500_000n];

function claim(patch: Partial<LedgerClaim>): LedgerClaim {
  return {
    claimId: `0x${"c1".repeat(32)}`,
    sequence: 0,
    kind: "Release",
    status: "Pending",
    amount: 200_000n,
    commitment: COMMITMENT,
    createdAt: NOW - 3_600,
    bondPaid: false,
    claimant: ACCOUNT,
    paidAt: 0,
    disputeDueAt: 0,
    liveness: null,
    ...patch,
  };
}

function minting(patch: Partial<MintingStatus> = {}, blocks: [bigint, MintBlock | null][] = []): MintingStatus {
  return {
    paused: false,
    pausedUntil: 0,
    pausedSince: 0,
    halted: false,
    fullyBonded: true,
    capRoom: 100_000_000n,
    mintBlock: new Map(DENOMINATIONS.map((d) => [d, "None" as MintBlock | null])),
    readAt: NOW,
    ...patch,
    ...(blocks.length ? { mintBlock: new Map(blocks) } : {}),
  };
}

describe("the opening fee (T-06)", () => {
  it("is the Registry's: amount * feeBps / 10,000 rounded down, waived below minFeeZat", () => {
    const registry = readFileSync(fileURLToPath(new URL("../../contracts/src/ZecuredRegistry.sol", import.meta.url)), "utf8");
    expect(registry).toContain("uint64 fee = uint64(uint256(amount) * feeBps / BPS);");
    expect(registry).toContain("if (fee < minFeeZat) fee = 0;");
    expect(registry).toContain("uint256 private constant BPS = 10_000;");
    expect(openingFee(1_000_000n, FEE)).toBe(0n); // 5,000 < 20,000: waived
    expect(openingFee(5_000_000n, FEE)).toBe(25_000n);
    expect(openingFee(12_500_000n, FEE)).toBe(62_500n);
    expect(openingFee(4_000_001n, FEE)).toBe(20_000n); // 20,000.005 rounded down, not below the minimum
    expect(openingFee(3_999_999n, FEE)).toBe(0n);
    expect(openingFee(5_000_000n, { feeBps: 0, minFeeZat: 0n })).toBe(0n);
  });

  it("is said before the deposit only when the deployment charges one", () => {
    expect(feeNotice(5_000_000n, null)).toBeNull();
    expect(feeNotice(5_000_000n, { feeBps: 0, minFeeZat: 0n } as never)).toBeNull();
    expect(feeNotice(5_000_000n, FEE as never)).toBe(
      "Opening fee: 0.00025 TAZ (0.5 % of the deposit), owed to the signers' treasury and paid out of the vault with its first payout. The vault's collateral is 0.04975 TAZ.",
    );
    expect(feeNotice(1_000_000n, FEE as never)).toBe(
      "No opening fee for 0.01 TAZ: the 0.5 % fee would be under 0.0002 TAZ, so it is waived.",
    );
  });
});

describe("the minting banner", () => {
  it("says nothing while nothing holds new deposits", () => {
    expect(mintingNotices(minting(), DENOMINATIONS, NOW)).toEqual([]);
  });

  it("a halt: minting stopped for good, and the bonds compensate the victims", () => {
    const n = mintingNotices(minting({ halted: true, paused: true, pausedUntil: NOW + 10, fullyBonded: false }), DENOMINATIONS, NOW);
    expect(n).toHaveLength(1);
    expect(n[0]!).toMatchObject({ tone: "danger", title: "New minting has stopped for good" });
    expect(n[0]!.body).toContain(COMPENSATION_SENTENCE);
    expect(n[0]!.body).toContain(EXITS_NEVER_STOP);
    expect(COMPENSATION_SENTENCE).toContain("the bonds compensate the victims: liquidators and lenders before the borrower's own equity.");
  });

  it("a pause by signatures until a time, or by an open theft case, with the pause sentence", () => {
    const bySignatures = mintingNotices(minting({ paused: true, pausedUntil: NOW + 86_400, pausedSince: NOW - 60 }), DENOMINATIONS, NOW);
    expect(bySignatures[0]!.title).toBe(`New minting is paused until ${formatUtc(NOW + 86_400)}`);
    expect(bySignatures[0]!.body).toContain(PAUSE_SENTENCE);
    expect(PAUSE_SENTENCE).toBe(
      "Three of five signers can pause new minting for up to 30 days at a time; withdrawals, repayments, liquidations and payouts never stop.",
    );
    const byCase = mintingNotices(minting({ paused: true, pausedUntil: 0 }), DENOMINATIONS, NOW);
    expect(byCase[0]!.title).toBe("New minting is paused while a theft case is open");
  });

  it("bonds short, the cap full (deposits land as refunds), room for the small sizes only, a stale cap price", () => {
    expect(mintingNotices(minting({ fullyBonded: false }), DENOMINATIONS, NOW)[0]!.title).toBe("Not every signer holds its full bond");
    const full = mintingNotices(minting({ capRoom: 0n }, DENOMINATIONS.map((d) => [d, "CapExceeded"])), DENOMINATIONS, NOW);
    expect(full[0]!.title).toBe("The coverage cap is full: new deposits land as refunds");
    expect(full[0]!.body).toContain(EXITS_NEVER_STOP);
    const some = mintingNotices(
      minting({ capRoom: 3_000_000n }, [
        [1_000_000n, "None"],
        [5_000_000n, "CapExceeded"],
        [12_500_000n, "CapExceeded"],
      ]),
      DENOMINATIONS,
      NOW,
    );
    expect(some[0]).toMatchObject({ tone: "info", title: "Room left under the coverage cap: 0.03 TAZ" });
    expect(some[0]!.body[0]).toBe("A deposit of 0.05 TAZ or 0.125 TAZ would land as a refund now; the smaller sizes still fit.");
    const stale = mintingNotices(minting({ capRoom: 0n }, DENOMINATIONS.map((d) => [d, null])), DENOMINATIONS, NOW);
    expect(stale.map((n) => n.title)).toEqual(["The coverage cap's price is stale"]);
  });
});

describe("opening a vault (no deposit that would only come back as a refund)", () => {
  const at = (block: MintBlock | null, patch: Partial<MintingStatus> = {}) => openGate(minting(patch, [[5_000_000n, block]]), 5_000_000n);

  it("refuses a halt and a full cap; warns of a pause, short bonds and a stale price; says nothing otherwise", () => {
    expect(at("Halted")).toMatchObject({ allowed: false });
    expect(at("CapExceeded", { capRoom: 1_000_000n })).toEqual({
      allowed: false,
      message:
        "The coverage cap has room for 0.01 TAZ only: a deposit of 0.05 TAZ proven now would only come back as a refund. Try a smaller size, or later.",
    });
    expect(at("Paused", { pausedUntil: NOW + 60 })).toEqual({
      allowed: true,
      message: `New minting is paused until ${formatUtc(NOW + 60)}: your deposit would wait as a pending activation until then.`,
    });
    expect(at("Paused").message).toMatch(/theft case is open/);
    // A pause by signatures that had ended when the backend read the gates: the case is what pauses.
    expect(at("Paused", { pausedUntil: NOW - 60 }).message).toMatch(/theft case is open/);
    expect(at("NotBonded")).toMatchObject({ allowed: true, message: expect.stringMatching(/full bond/) });
    expect(at(null)).toMatchObject({ allowed: true, message: expect.stringMatching(/stale/) });
    expect(at("None")).toEqual({ allowed: true, message: null });
    // No status (an older backend, or not loaded yet): nothing to say.
    expect(openGate(null, 5_000_000n)).toEqual({ allowed: true, message: null });
  });
});

describe("a pending activation (K3, T-03)", () => {
  const DELAY = 86_400;
  const pending = (patch: Partial<PendingActivationView> = {}): PendingActivationView => ({
    amount: 1_000_000n,
    readyHeight: 4_500_006n,
    readyTime: NOW - 10,
    relayTip: 4_500_010n,
    anchorOnBestChain: true,
    openCases: 0n,
    paused: false,
    pausedUntil: 0,
    pausedSince: 0,
    halted: false,
    fullyBonded: true,
    ...patch,
  });

  it("ready once the relay reached the height and the time passed, with nothing else holding it", () => {
    expect(pendingState(pending(), DELAY, NOW)).toEqual({ ready: true, waits: [], refundable: false, refundableAt: null, dropped: false });
    expect(pendingState(pending({ relayTip: 4_500_005n }), DELAY, NOW).ready).toBe(false);
    expect(pendingState(pending({ readyTime: NOW + 1 }), DELAY, NOW).ready).toBe(false);
  });

  it("a pause by signatures: the account may refund once it lasted pendingRefundDelay, if it still runs", () => {
    const early = pendingState(pending({ paused: true, pausedSince: NOW - 3_600, pausedUntil: NOW + 7 * 86_400 }), DELAY, NOW);
    expect(early).toMatchObject({ refundable: false, refundableAt: NOW - 3_600 + DELAY });
    expect(early.waits).toEqual([`New minting is paused until ${formatUtc(NOW + 7 * 86_400)}.`]);
    const late = pendingState(pending({ paused: true, pausedSince: NOW - DELAY, pausedUntil: NOW + 60 }), DELAY, NOW);
    expect(late).toMatchObject({ refundable: true, refundableAt: null });
    // A pause that ends before the delay is up never makes it refundable.
    const short = pendingState(pending({ paused: true, pausedSince: NOW - 60, pausedUntil: NOW + 3_600 }), DELAY, NOW);
    expect(short).toMatchObject({ refundable: false, refundableAt: null });
    // A pause by an open theft case (autoPauseOnCase) is not one by signatures.
    const byCase = pendingState(pending({ paused: true }), DELAY, NOW);
    expect(byCase).toMatchObject({ refundable: false, waits: ["New minting is paused while a theft case is open."] });
  });

  it("a halt: never activated, refundable at once; an open case or short bonds hold it", () => {
    const halted = pendingState(pending({ halted: true }), DELAY, NOW);
    expect(halted).toMatchObject({ refundable: true, waits: ["Minting has stopped for good: this activation can only be refunded."] });
    expect(pendingState(pending({ openCases: 1n }), DELAY, NOW).waits[0]).toMatch(/violation case is open/);
    expect(pendingState(pending({ fullyBonded: false }), DELAY, NOW).waits).toEqual(["Not every signer holds its full bond."]);
  });

  it("an anchor that left the relay's best chain: dropped, never refundable", () => {
    expect(pendingState(pending({ anchorOnBestChain: false, halted: true }), DELAY, NOW)).toMatchObject({
      dropped: true,
      refundable: false,
    });
  });
});

describe("refunds (T-02)", () => {
  it("a Refund claim needs the account's destination while it has none, or once abandoned", () => {
    expect(needsRefundDestination(claim({ kind: "Refund", commitment: NONE32 }))).toBe(true);
    expect(needsRefundDestination(claim({ kind: "Refund", status: "WrittenOff", commitment: NONE32 }))).toBe(true);
    expect(needsRefundDestination(claim({ kind: "Refund" }))).toBe(false);
    expect(needsRefundDestination(claim({ kind: "Refund", commitment: NONE32, bondPaid: true }))).toBe(false);
    expect(needsRefundDestination(claim({ kind: "Release", commitment: NONE32 }))).toBe(false);
    expect(needsRefundDestination(claim({ kind: "Refund", status: "Paid", commitment: NONE32 }))).toBe(false);
  });

  it("can be written off refundAbandonPeriod after it was created, while it has no destination", () => {
    expect(abandonableAt(claim({ kind: "Refund", commitment: NONE32, createdAt: NOW }), 2_592_000)).toBe(NOW + 2_592_000);
    expect(abandonableAt(claim({ kind: "Refund" }), 2_592_000)).toBeNull();
    expect(abandonableAt(claim({ kind: "Refund", commitment: NONE32, status: "WrittenOff" }), 2_592_000)).toBeNull();
  });

  it("says why every deposit lands as a refund", () => {
    expect(Object.keys(REFUND_REASON_TEXT)).toEqual([
      "ModeRefund",
      "VaultNotNew",
      "PendingExists",
      "AccountHasVault",
      "Halted",
      "CapExceeded",
      "PendingConverted",
    ]);
    expect(REFUND_REASON_TEXT.CapExceeded).toMatch(/coverage cap was full/);
  });
});

describe("claims in plain words", () => {
  const ctx = { refundAbandonPeriod: 2_592_000, nowSeconds: NOW };
  const view = (patch: Partial<LedgerClaim>) => claimView(claim(patch), ctx);

  it("each kind's label", () => {
    expect(view({ kind: "Fee", claimant: ZERO }).label).toBe("Opening fee (to the signers' treasury)");
    expect(view({ kind: "Refund" }).label).toBe("Refund of a deposit");
    expect(view({ kind: "CloseMarketMaker" }).label).toBe("Close: the market maker's share");
  });

  it("each status, with what happens next", () => {
    expect(view({}).badge).toEqual({ tone: "warn", text: "Pending" });
    expect(view({ kind: "Refund", commitment: NONE32, createdAt: NOW }).badge.text).toBe("Needs your destination");
    expect(view({ kind: "Refund", commitment: NONE32, createdAt: NOW }).detail).toContain(formatUtc(NOW + 2_592_000));
    expect(view({ kind: "Fee", claimant: ZERO }).detail).toMatch(/next payout, to the signers' treasury/);
    expect(view({ status: "Paid" }).badge).toEqual({ tone: "ok", text: "Paid" });
    expect(view({ kind: "Refund", status: "WrittenOff", commitment: NONE32 }).badge.text).toBe("Abandoned");
    expect(view({ status: "WrittenOff", amount: 9_000n }).detail).toMatch(/dust/);
    expect(view({ status: "Compensated" }).detail).toBe("This vault was compromised: the compensator pays this claim in USDC instead.");
    expect(view({ kind: "Retired", status: "Compensated", claimant: ZERO }).detail).toMatch(/never paid on Zcash/);
    expect(view({ status: "Disputed", disputeDueAt: NOW + 60 }).detail).toContain(formatUtc(NOW + 60));
    expect(view({ status: "Disputed", disputeDueAt: NOW - 60 }).detail).toMatch(/can be reopened/);
    expect(view({ bondPaid: true, commitment: NONE32 }).badge.text).toBe("Paid from the bonds");
  });
});

describe("disputing a payout confirmation (T-05 (a))", () => {
  const ctx = { account: ACCOUNT, disputeWindow: 86_400, compromised: false, nowSeconds: NOW };

  it("the claimant may dispute a confirmed claim within disputeWindow of its confirmation", () => {
    expect(disputeState(claim({ status: "Paid", paidAt: NOW - 3_600 }), ctx)).toEqual({
      canDispute: true,
      until: NOW - 3_600 + 86_400,
      canReopen: false,
      dueAt: null,
    });
    expect(disputeState(claim({ status: "Paid", paidAt: NOW - 86_401 }), ctx)).toMatchObject({ canDispute: false });
  });

  it("not another's claim, not a Fee (no claimant), not an upheld or bond-paid one, not on a compromised vault", () => {
    const paid = { status: "Paid" as const, paidAt: NOW - 60 };
    expect(disputeState(claim({ ...paid, claimant: OTHER }), ctx).canDispute).toBe(false);
    expect(disputeState(claim({ ...paid, kind: "Fee", claimant: ZERO }), { ...ctx, account: ZERO }).canDispute).toBe(false);
    expect(disputeState(claim({ status: "Paid", paidAt: 0 }), ctx).canDispute).toBe(false);
    expect(disputeState(claim({ ...paid, bondPaid: true }), ctx).canDispute).toBe(false);
    expect(disputeState(claim(paid), { ...ctx, compromised: true }).canDispute).toBe(false);
  });

  it("a disputed claim can be reopened once its inclusion window passed, and shows when", () => {
    expect(disputeState(claim({ status: "Disputed", disputeDueAt: NOW + 60 }), ctx)).toEqual({
      canDispute: false,
      until: null,
      canReopen: false,
      dueAt: NOW + 60,
    });
    expect(disputeState(claim({ status: "Disputed", disputeDueAt: NOW - 1 }), ctx).canReopen).toBe(true);
  });
});

describe("the bonds paying an overdue claim (T-05)", () => {
  const ctx = { nowSeconds: NOW, slashable: 1_000_000n };
  const overdue = { liveness: { dueAt: NOW - 60, usdc: 82_000n } };

  it("overdue: what the bonds pay now, and whether they hold that much", () => {
    expect(bondPayment(claim(overdue), ctx)).toEqual({ kind: "now", usdc: 82_000n, covered: true });
    expect(bondPayment(claim({ liveness: { dueAt: NOW, usdc: 82_000n } }), ctx).kind).toBe("now");
    expect(bondPayment(claim(overdue), { ...ctx, slashable: 81_999n })).toEqual({ kind: "now", usdc: 82_000n, covered: false });
    // Unknown holdings never hide the action: the bond itself refuses beyond them.
    expect(bondPayment(claim(overdue), { ...ctx, slashable: null })).toMatchObject({ covered: true });
  });

  it("not yet: when the bonds pay it if the signers have not", () => {
    expect(bondPayment(claim({ liveness: { dueAt: NOW + 60, usdc: 82_000n } }), ctx)).toEqual({ kind: "later", dueAt: NOW + 60 });
  });

  it("never: no reading (not the account's claim, no destination…), settled, already paid by the bonds, nothing to pay", () => {
    expect(bondPayment(claim({}), ctx)).toEqual({ kind: "none" });
    expect(bondPayment(claim({ ...overdue, status: "Paid" }), ctx)).toEqual({ kind: "none" });
    expect(bondPayment(claim({ ...overdue, bondPaid: true }), ctx)).toEqual({ kind: "none" });
    expect(bondPayment(claim({ liveness: { dueAt: NOW - 60, usdc: 0n } }), ctx)).toEqual({ kind: "none" });
  });

  it("says in plain words that the claim's ZEC then goes to the bonds instead", () => {
    const text = bondPaymentExplainer(500);
    expect(text).toContain("plus a 5 % premium");
    expect(text).toMatch(/the claim's ZEC no longer comes to you: when the signers pay it out, it goes to the bonds instead/);
    expect(bondPaymentExplainer(null)).toContain("plus a premium");
    expect(bondPaymentExplainer(0)).not.toMatch(/premium/);
  });
});

describe("the ledger's views", () => {
  const ledger = {
    claims: [
      claim({ sequence: 0, kind: "Fee", claimant: ZERO, claimId: `0x${"01".repeat(32)}` }),
      claim({ sequence: 1, kind: "Refund", commitment: NONE32, claimId: `0x${"02".repeat(32)}` }),
      claim({ sequence: 2, kind: "Refund", claimId: `0x${"03".repeat(32)}` }),
      claim({ sequence: 3, status: "Compensated", claimId: `0x${"04".repeat(32)}` }),
      claim({ sequence: 4, kind: "Retired", status: "Compensated", claimant: ZERO, claimId: `0x${"05".repeat(32)}` }),
    ],
  } as VaultLedger;

  it("refunds (newest first), those to claim, the fee, and the claims the compensator owes", () => {
    expect(refundClaims(ledger).map((c) => c.sequence)).toEqual([2, 1]);
    expect(refundsToClaim(ledger).map((c) => c.sequence)).toEqual([1]);
    expect(feeClaim(ledger)?.sequence).toBe(0);
    expect(compensatedClaimIds(ledger)).toEqual([`0x${"04".repeat(32)}`]);
  });

  it("periods in words", () => {
    expect([formatPeriod(300), formatPeriod(21_600), formatPeriod(86_400), formatPeriod(2_592_000)]).toEqual([
      "5 min",
      "6 h",
      "24 h",
      "30 days",
    ]);
  });
});
