import { describe, expect, it } from "vitest";
import { capBlocksActivation, openRefusal, PROOF_MINTING_SENTENCE, SWAP_PRIVACY_SENTENCE } from "./v4";

describe("R-11.1: a deposit waiting for room under the coverage cap", () => {
  const waiting = { vaultStatus: "deposit_seen", baseStatus: "None" as const, pending: false, mintBlock: "CapExceeded" as const };

  it("is a deposit the signers saw, nothing on Base yet, and the cap full for its size", () => {
    expect(capBlocksActivation(waiting)).toBe(true);
    expect(capBlocksActivation({ ...waiting, baseStatus: undefined })).toBe(true);
  });

  it("is not a deposit already proven, activated or refunded, nor one held by anything else", () => {
    expect(capBlocksActivation({ ...waiting, pending: true })).toBe(false);
    expect(capBlocksActivation({ ...waiting, baseStatus: "Active" })).toBe(false);
    expect(capBlocksActivation({ ...waiting, baseStatus: "Cancelled" })).toBe(false);
    expect(capBlocksActivation({ ...waiting, vaultStatus: "awaiting_deposit" })).toBe(false);
    for (const mintBlock of ["None", "Paused", "NotBonded", "Halted", null, undefined] as const) {
      expect(capBlocksActivation({ ...waiting, mintBlock })).toBe(false);
    }
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
