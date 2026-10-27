import { describe, expect, it } from "vitest";
import {
  awaitingDeposit,
  capWait,
  depositConfirmed,
  openingsNotice,
  openRefusal,
  PROOF_MINTING_SENTENCE,
  refundProofRefusal,
  SWAP_PRIVACY_SENTENCE,
} from "./v4";

describe("R-11.1: a deposit waiting for room under the coverage cap", () => {
  const waiting = { vaultStatus: "awaiting_deposit", baseStatus: "None" as const, pending: false, mintBlock: "CapExceeded" as const };

  it("possible while the vault awaits its deposit, nothing is on Base and the cap is full for its size", () => {
    expect(capWait(waiting)).toBe("possible");
    expect(capWait({ ...waiting, baseStatus: undefined })).toBe("possible");
  });

  it("confirmed once the coordinator says a deposit was seen", () => {
    expect(capWait({ ...waiting, depositSeen: true })).toBe("confirmed");
    expect(capWait({ ...waiting, vaultStatus: "deposit_seen" })).toBe("confirmed");
  });

  it("none for a deposit already proven, activated or refunded, a closed vault, or anything else holding it", () => {
    expect(capWait({ ...waiting, pending: true })).toBe("none");
    // R-12: proven as a refund (a pending refund until its claim opens, then the claim).
    expect(capWait({ ...waiting, vaultStatus: "deposit_seen", refunding: true })).toBe("none");
    expect(capWait({ ...waiting, baseStatus: "Active" })).toBe("none");
    expect(capWait({ ...waiting, baseStatus: "Cancelled" })).toBe("none");
    expect(capWait({ ...waiting, vaultStatus: "expired" })).toBe("none");
    for (const mintBlock of ["None", "Paused", "NotBonded", "Halted", null, undefined] as const) {
      expect(capWait({ ...waiting, mintBlock })).toBe("none");
    }
  });
});

describe("R-11.1: the coordinator's refusals of a refund proof (track OFF's codes)", () => {
  it("says what each means, cap_full included; null for codes it does not know", () => {
    expect(refundProofRefusal("not_blocked")).toMatch(/room again/);
    expect(refundProofRefusal("no_deposit")).toMatch(/confirmations/);
    expect(refundProofRefusal("already_landed")).toMatch(/already on Base/);
    expect(refundProofRefusal("refund_unavailable")).toMatch(/cannot prove a deposit as a refund/);
    expect(refundProofRefusal("cap_full")).toMatch(/waits until there is room, or you ask for your deposit back/);
    expect(refundProofRefusal("not_owner")).toBeNull();
    expect(refundProofRefusal(null)).toBeNull();
  });
});

describe("P-8: the operator's switch on new vaults", () => {
  it("closed openings are a state of their own, invite-only ones say so, open ones say nothing", () => {
    expect(openingsNotice({ open: false, inviteOnly: true })?.title).toBe("New vault openings are temporarily closed");
    expect(openingsNotice({ open: false, inviteOnly: false })?.body).toMatch(/payouts go on/);
    expect(openingsNotice({ open: true, inviteOnly: true })?.title).toMatch(/invite code/);
    expect(openingsNotice({ open: true, inviteOnly: false })).toBeNull();
    expect(openingsNotice(null)).toBeNull();
  });
});

describe("refusals of a vault opening (the mainnet beta's invites, P-8)", () => {
  it("says what each fixed code means, and that closed openings never stop exits", () => {
    expect(openRefusal("invite_required")?.title).toBe("An invite code is needed");
    expect(openRefusal("invite_used")?.body).toMatch(/one vault/);
    const closed = openRefusal("openings_closed")!;
    expect(closed.openingsClosed).toBe(true);
    expect(closed.body).toMatch(/repayments, exits, liquidations and payouts go on/);
    expect(openRefusal("busy")?.openingsClosed).toBe(false);
    expect(openRefusal("not_owner")).toBeNull();
    expect(openRefusal(null)).toBeNull();
  });
});

describe("fixed sentences", () => {
  it("the swap's privacy line and proof-based minting", () => {
    expect(SWAP_PRIVACY_SENTENCE).toBe("On a swap your Base account is visible; where the ZEC goes is not.");
    expect(PROOF_MINTING_SENTENCE).toMatch(/zero-knowledge proof/);
    expect(PROOF_MINTING_SENTENCE).not.toMatch(/signers (see|saw)/);
  });
});

describe("a seen deposit has its confirmations (L-01: the live backend sends no count)", () => {
  it("counts a deposit the signers saw, with no count reported, as confirmed", () => {
    expect(depositConfirmed({ status: "awaiting_deposit", depositSeen: true }, 3)).toBe(true);
    expect(depositConfirmed({ status: "deposit_seen" }, 10)).toBe(true);
    expect(depositConfirmed({ status: "awaiting_deposit", depositSeen: true }, null)).toBe(true);
  });

  it("follows a reported count (the replay)", () => {
    expect(depositConfirmed({ status: "deposit_seen", depositSeen: true, confirmations: 1 }, 3)).toBe(false);
    expect(depositConfirmed({ status: "deposit_seen", depositSeen: true, confirmations: 3 }, 3)).toBe(true);
  });

  it("is false before the signers saw the deposit", () => {
    expect(depositConfirmed(null, 3)).toBe(false);
    expect(depositConfirmed({ status: "awaiting_deposit" }, 3)).toBe(false);
    expect(depositConfirmed({ status: "expired", confirmations: 5 }, 3)).toBe(false);
  });
});

describe("the deposit step stops asking for a payment once the deposit is seen", () => {
  const fresh = { vaultStatus: "awaiting_deposit", pending: false, refunding: false };

  it("asks while nothing was seen, also before the vault's status is read", () => {
    expect(awaitingDeposit(fresh)).toBe(true);
    expect(awaitingDeposit({ ...fresh, vaultStatus: undefined })).toBe(true);
    expect(awaitingDeposit({ ...fresh, depositSeen: false })).toBe(true);
  });

  it("stops once the signers saw the conforming deposit, before its proof is on Base (second e2e round)", () => {
    expect(awaitingDeposit({ ...fresh, depositSeen: true })).toBe(false);
  });

  it("stops once Base has the proof, the activation or a refund", () => {
    expect(awaitingDeposit({ ...fresh, vaultStatus: "deposit_seen" })).toBe(false);
    expect(awaitingDeposit({ ...fresh, pending: true })).toBe(false);
    expect(awaitingDeposit({ ...fresh, refunding: true })).toBe(false);
    expect(awaitingDeposit({ ...fresh, vaultStatus: "active" })).toBe(false);
  });
});
