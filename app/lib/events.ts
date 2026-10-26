/**
 * Events the vault's trail (lib/journey.ts) reads that the other ABIs do not carry, exactly as the frozen
 * contracts and the vendored Morpho Blue declare them: the Intake's `ActivationPending` (IZecuredIntake), the
 * spend logic's `SpendCommitted` (IZecuredSpendCommits, emitted at the Registry's address) and Morpho Blue's
 * `Borrow` and `Repay` (EventsLib).
 */
export const activationPendingEvent = {
  type: "event",
  name: "ActivationPending",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "account", type: "address", indexed: true },
    { name: "amount", type: "uint64", indexed: false },
    { name: "depositTag", type: "bytes32", indexed: false },
    { name: "readyHeight", type: "uint256", indexed: false },
    { name: "readyTime", type: "uint256", indexed: false },
  ],
} as const;

export const spendCommittedEvent = {
  type: "event",
  name: "SpendCommitted",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "payoutRef", type: "bytes32", indexed: true },
    { name: "inputTag", type: "bytes32", indexed: false },
    { name: "claimIds", type: "bytes32[]", indexed: false },
    { name: "claimRefs", type: "bytes32[]", indexed: false },
    { name: "changeTag", type: "bytes32", indexed: false },
    { name: "changeValue", type: "uint64", indexed: false },
    { name: "signerBitmap", type: "uint8", indexed: false },
  ],
} as const;

export const morphoBorrowEvent = {
  type: "event",
  name: "Borrow",
  inputs: [
    { name: "id", type: "bytes32", indexed: true },
    { name: "caller", type: "address", indexed: false },
    { name: "onBehalf", type: "address", indexed: true },
    { name: "receiver", type: "address", indexed: true },
    { name: "assets", type: "uint256", indexed: false },
    { name: "shares", type: "uint256", indexed: false },
  ],
} as const;

export const morphoRepayEvent = {
  type: "event",
  name: "Repay",
  inputs: [
    { name: "id", type: "bytes32", indexed: true },
    { name: "caller", type: "address", indexed: true },
    { name: "onBehalf", type: "address", indexed: true },
    { name: "assets", type: "uint256", indexed: false },
    { name: "shares", type: "uint256", indexed: false },
  ],
} as const;
