/**
 * The ABI fragments the app uses (contracts/src/interfaces/v3: IZecuredRegistry, IZecuredDisputes,
 * IZecuredIntake, IZecuredSlasher, IZecuredCompensator, ISignerBond; Morpho Blue's IMorpho /
 * IIrm). lib/calls.test.ts checks each against the compiled ABIs in contracts/abi.
 */

const marketParamsTuple = {
  type: "tuple",
  name: "marketParams",
  components: [
    { name: "loanToken", type: "address" },
    { name: "collateralToken", type: "address" },
    { name: "oracle", type: "address" },
    { name: "irm", type: "address" },
    { name: "lltv", type: "uint256" },
  ],
} as const;

/**
 * v4 (R-1): the signers' confirmation of a payout names it (`payoutRef`), so a claimant can find the
 * transaction (IZecuredPayouts, served at the Registry's address).
 */
export const payoutConfirmedEvent = {
  type: "event",
  name: "PayoutConfirmed",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "payoutRef", type: "bytes32", indexed: true },
    { name: "claimIds", type: "bytes32[]", indexed: false },
  ],
} as const;

/**
 * v4 (R-3): a confirmation not disputed within its window, its vault not frozen meanwhile, became
 * final (`paidAt` 0, as after a proof of the payout); IZecuredPayouts.
 */
export const confirmationExpiredEvent = {
  type: "event",
  name: "ConfirmationExpired",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "claimId", type: "bytes32", indexed: true },
  ],
} as const;

/** v4 (section 3): a zero-knowledge proof that the claim's payout is in a Zcash block is on Base (IZecuredDisputes). */
export const payoutProvenEvent = {
  type: "event",
  name: "PayoutProven",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "claimId", type: "bytes32", indexed: true },
    { name: "payoutRef", type: "bytes32", indexed: false },
  ],
} as const;

/** v4 (R-11): a Refund claim without a destination set aside as dormant (IZecuredRefunds). */
export const refundDormantEvent = {
  type: "event",
  name: "RefundDormant",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "claimId", type: "bytes32", indexed: true },
  ],
} as const;

/** Public Registry events (docs/INTERFACES.md § 3.2), v4's included. */
export const registryEventsAbi = [
  {
    type: "event",
    name: "VaultActivated",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "account", type: "address", indexed: true },
      { name: "amount", type: "uint64", indexed: false },
      { name: "depositTag", type: "bytes32", indexed: false },
    ],
  },
  {
    type: "event",
    name: "ReleaseRequested",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "amount", type: "uint64", indexed: false },
      { name: "destCommitment", type: "bytes32", indexed: false },
    ],
  },
  {
    type: "event",
    name: "LiquidationRelease",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "amount", type: "uint64", indexed: false },
      { name: "recipientCommitment", type: "bytes32", indexed: false },
    ],
  },
  {
    type: "event",
    name: "CloseRelease",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "userClaimId", type: "bytes32", indexed: false },
      { name: "mmClaimId", type: "bytes32", indexed: false },
      { name: "userAmount", type: "uint64", indexed: false },
      { name: "mmAmount", type: "uint64", indexed: false },
      { name: "userCommitment", type: "bytes32", indexed: false },
      { name: "mmCommitment", type: "bytes32", indexed: false },
    ],
  },
  // IZecuredPayouts (v4, R-1: a confirmation names its payout), emitted at the Registry's address.
  payoutConfirmedEvent,
  confirmationExpiredEvent,
  {
    type: "event",
    name: "RefundOpened",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "account", type: "address", indexed: false },
      { name: "amount", type: "uint64", indexed: false },
      { name: "depositTag", type: "bytes32", indexed: false },
    ],
  },
  {
    type: "event",
    name: "RefundDestinationSet",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "commitment", type: "bytes32", indexed: false },
    ],
  },
  {
    type: "event",
    name: "FeeCharged",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "fee", type: "uint64", indexed: false },
    ],
  },
  // IZecuredDisputes, emitted at the Registry's address (its fallback runs the dispute code).
  {
    type: "event",
    name: "PayoutDisputed",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "dueAt", type: "uint64", indexed: false },
    ],
  },
  {
    type: "event",
    name: "DisputedClaimReopened",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimId", type: "bytes32", indexed: true },
    ],
  },
  payoutProvenEvent,
  // R-11: a Refund claim without a destination set aside (the spend logic emits it at the Registry's address).
  refundDormantEvent,
] as const;

/** Registry errors an account's own calls can hit (IZecuredRegistry), decoded to explain reverts. */
export const registryErrorsAbi = [
  { type: "error", name: "VaultNotActive", inputs: [{ name: "vaultId", type: "bytes32" }] },
  {
    type: "error",
    name: "NotVaultAccount",
    inputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "caller", type: "address" },
    ],
  },
  { type: "error", name: "ZeroCommitment", inputs: [] },
  { type: "error", name: "DebtOutstanding", inputs: [{ name: "vaultId", type: "bytes32" }] },
  {
    type: "error",
    name: "ReleaseAmountTooSmall",
    inputs: [
      { name: "amount", type: "uint64" },
      { name: "minimum", type: "uint64" },
    ],
  },
  {
    type: "error",
    name: "ReleaseAmountNotPartial",
    inputs: [
      { name: "amount", type: "uint64" },
      { name: "outstanding", type: "uint64" },
    ],
  },
  {
    type: "error",
    name: "ReleaseRemainderTooSmall",
    inputs: [
      { name: "remaining", type: "uint64" },
      { name: "minimum", type: "uint64" },
    ],
  },
  {
    type: "error",
    name: "ReleaseLtvTooHigh",
    inputs: [
      { name: "debt", type: "uint256" },
      { name: "collateralValue", type: "uint256" },
    ],
  },
  {
    type: "error",
    name: "CollateralMismatch",
    inputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "morphoCollateral", type: "uint256" },
      { name: "outstanding", type: "uint64" },
    ],
  },
  // v3: refunds (T-02) and the freeze of a vault under a theft case (T-04).
  { type: "error", name: "NotARefund", inputs: [{ name: "claimId", type: "bytes32" }] },
  { type: "error", name: "RefundBound", inputs: [{ name: "claimId", type: "bytes32" }] },
  { type: "error", name: "VaultFrozen", inputs: [{ name: "vaultId", type: "bytes32" }] },
  { type: "error", name: "VaultCompromised", inputs: [{ name: "vaultId", type: "bytes32" }] },
] as const;

const claimDetailsTuple = {
  type: "tuple",
  name: "d",
  components: [
    { name: "vaultId", type: "bytes32" },
    { name: "kind", type: "uint8" },
    { name: "status", type: "uint8" },
    { name: "amount", type: "uint64" },
    { name: "commitment", type: "bytes32" },
    { name: "createdAt", type: "uint40" },
    { name: "bondPaid", type: "bool" },
    { name: "claimant", type: "address" },
  ],
} as const;

const vaultDetailsTuple = {
  type: "tuple",
  name: "",
  components: [
    { name: "account", type: "address" },
    { name: "outstanding", type: "uint64" },
    { name: "status", type: "uint8" },
    { name: "claimCount", type: "uint64" },
    { name: "unpaidClaims", type: "uint64" },
  ],
} as const;

export const registryAbi = [
  {
    type: "function",
    name: "requestRelease",
    stateMutability: "nonpayable",
    inputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "destCommitment", type: "bytes32" },
    ],
    outputs: [{ name: "claimId", type: "bytes32" }],
  },
  {
    type: "function",
    name: "requestPartialRelease",
    stateMutability: "nonpayable",
    inputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "amount", type: "uint64" },
      { name: "destCommitment", type: "bytes32" },
    ],
    outputs: [{ name: "claimId", type: "bytes32" }],
  },
  {
    type: "function",
    name: "vaultOf",
    stateMutability: "view",
    inputs: [{ name: "account", type: "address" }],
    outputs: [{ name: "vaultId", type: "bytes32" }],
  },
  {
    type: "function",
    name: "vault",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [
      { name: "account", type: "address" },
      { name: "outstanding", type: "uint64" },
      { name: "status", type: "uint8" },
    ],
  },
  {
    type: "function",
    name: "claim",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "kind", type: "uint8" },
      { name: "amount", type: "uint64" },
      { name: "commitment", type: "bytes32" },
      { name: "status", type: "uint8" },
    ],
  },
  // v3
  {
    type: "function",
    name: "setRefundDestination",
    stateMutability: "nonpayable",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "destCommitment", type: "bytes32" },
    ],
    outputs: [],
  },
  {
    type: "function",
    name: "vaultDetails",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [vaultDetailsTuple],
  },
  {
    type: "function",
    name: "claimDetails",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [claimDetailsTuple],
  },
  { type: "function", name: "intake", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  { type: "function", name: "slasher", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  { type: "function", name: "compensator", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  { type: "function", name: "signerBond", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  {
    type: "function",
    name: "refundAbandonPeriod",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "uint64" }],
  },
  ...registryEventsAbi,
  ...registryErrorsAbi,
] as const;

/** IZecuredDisputes' errors (the reverts of a dispute or a reopening). */
export const disputesErrorsAbi = [
  { type: "error", name: "NotDisputable", inputs: [{ name: "claimId", type: "bytes32" }] },
  {
    type: "error",
    name: "NotClaimant",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "caller", type: "address" },
    ],
  },
  {
    type: "error",
    name: "DisputeWindowClosed",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "closedAt", type: "uint64" },
    ],
  },
  { type: "error", name: "DisputeVaultCompromised", inputs: [{ name: "vaultId", type: "bytes32" }] },
  { type: "error", name: "NotDisputed", inputs: [{ name: "claimId", type: "bytes32" }] },
  {
    type: "error",
    name: "InclusionWindowOpen",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "dueAt", type: "uint64" },
    ],
  },
] as const;

/**
 * IZecuredDisputes (T-05 (a)): served at the Registry's address, whose fallback runs the dispute
 * code, so calls and reads go to the Registry.
 */
export const disputesAbi = [
  {
    type: "function",
    name: "disputePayout",
    stateMutability: "nonpayable",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "function",
    name: "reopenDisputedClaim",
    stateMutability: "nonpayable",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "function",
    name: "payoutDispute",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [
      { name: "paidAt", type: "uint64" },
      { name: "dueAt", type: "uint64" },
    ],
  },
  { type: "function", name: "disputeWindow", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  { type: "function", name: "inclusionWindow", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  ...disputesErrorsAbi,
] as const;

/** The Intake's errors a pending activation's refund can hit. */
export const intakeErrorsAbi = [
  { type: "error", name: "NoPendingActivation", inputs: [{ name: "vaultId", type: "bytes32" }] },
  { type: "error", name: "NotRefundableYet", inputs: [{ name: "vaultId", type: "bytes32" }] },
  {
    type: "error",
    name: "NotPendingAccount",
    inputs: [
      { name: "vaultId", type: "bytes32" },
      { name: "caller", type: "address" },
    ],
  },
] as const;

/** The Intake's `DepositRefunded(vaultId, claimId, reason)`: why a deposit landed as a refund (T-02). */
export const depositRefundedEvent = {
  type: "event",
  name: "DepositRefunded",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "claimId", type: "bytes32", indexed: true },
    { name: "reason", type: "uint8", indexed: false },
  ],
} as const;

/** ZecuredIntake: pending activations, their refund (T-03) and the minting gates (T-01). */
export const intakeAbi = [
  {
    type: "function",
    name: "pendingActivation",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [
      {
        type: "tuple",
        name: "",
        components: [
          { name: "account", type: "address" },
          { name: "amount", type: "uint64" },
          { name: "depositTag", type: "bytes32" },
          { name: "anchor", type: "bytes32" },
          { name: "readyHeight", type: "uint256" },
          { name: "readyTime", type: "uint256" },
          // R-14 (v4): the deposit version whose seal proved it.
          { name: "versionId", type: "bytes32" },
        ],
      },
    ],
  },
  {
    type: "function",
    name: "refundPending",
    stateMutability: "nonpayable",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [{ name: "claimId", type: "bytes32" }],
  },
  { type: "function", name: "mintingPaused", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "bool" }] },
  { type: "function", name: "pausedUntil", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  { type: "function", name: "pausedSince", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  {
    type: "function",
    name: "pendingRefundDelay",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "uint64" }],
  },
  { type: "function", name: "relay", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  { type: "function", name: "signerBond", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "address" }] },
  depositRefundedEvent,
  ...intakeErrorsAbi,
] as const;

/** The Zcash header relay's best chain (IZcashHeaderRelay). */
export const relayAbi = [
  {
    type: "function",
    name: "bestChainTip",
    stateMutability: "view",
    inputs: [],
    outputs: [
      { name: "blockHash", type: "bytes32" },
      { name: "height", type: "uint256" },
      { name: "chainWork", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "isInBestChain",
    stateMutability: "view",
    inputs: [{ name: "blockHash", type: "bytes32" }],
    outputs: [{ name: "", type: "bool" }],
  },
] as const;

/** ZecuredSlasher: a vault's freeze and compromise (T-04). */
export const slasherAbi = [
  {
    type: "function",
    name: "vaultFrozen",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "isCompromised",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "openCasesOf",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [{ name: "", type: "uint256" }],
  },
] as const;

/** SignerBondV2's errors a payment by the bonds can hit (T-05). */
export const bondErrorsAbi = [
  {
    type: "error",
    name: "NotTheClaimant",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "caller", type: "address" },
    ],
  },
  { type: "error", name: "NotLivenessEligible", inputs: [{ name: "claimId", type: "bytes32" }] },
  {
    type: "error",
    name: "LivenessNotDue",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "dueAt", type: "uint64" },
    ],
  },
  {
    type: "error",
    name: "InsufficientBond",
    inputs: [
      { name: "due", type: "uint256" },
      { name: "slashable", type: "uint256" },
    ],
  },
] as const;

/** SignerBondV2: the gates the Intake reads (T-01), and the bonds paying an overdue claim (T-05). */
export const bondAbi = [
  { type: "function", name: "halted", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "bool" }] },
  { type: "function", name: "fullyBonded", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "bool" }] },
  {
    type: "function",
    name: "livenessDue",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [
      { name: "dueAt", type: "uint64" },
      { name: "amount", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "slashLiveness",
    stateMutability: "nonpayable",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [{ name: "paid", type: "uint256" }],
  },
  { type: "function", name: "totalSlashable", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
  ...bondErrorsAbi,
] as const;

/** ZecuredCompensator: what a compromised vault's victims get (T-04), read-only. */
export const compensatorAbi = [
  {
    type: "function",
    name: "victim",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [
      {
        type: "tuple",
        name: "",
        components: [
          { name: "incident", type: "uint256" },
          { name: "tallied", type: "bool" },
          { name: "settled", type: "bool" },
          { name: "authorized", type: "bool" },
          { name: "pendingClaims", type: "uint64" },
          { name: "talliedClaims", type: "uint64" },
          { name: "outstanding", type: "uint64" },
          { name: "debt", type: "uint256" },
        ],
      },
    ],
  },
  {
    type: "function",
    name: "entitlement",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [
      {
        type: "tuple",
        name: "",
        components: [
          { name: "incident", type: "uint256" },
          { name: "layer", type: "uint8" },
          { name: "zat", type: "uint64" },
          { name: "to", type: "address" },
          { name: "paid", type: "bool" },
        ],
      },
    ],
  },
  {
    type: "function",
    name: "equityOf",
    stateMutability: "view",
    inputs: [{ name: "vaultId", type: "bytes32" }],
    outputs: [{ name: "", type: "uint256" }],
  },
  {
    type: "function",
    name: "incident",
    stateMutability: "view",
    inputs: [{ name: "id", type: "uint256" }],
    outputs: [
      {
        type: "tuple",
        name: "",
        components: [
          { name: "openedAt", type: "uint64" },
          { name: "closesAt", type: "uint64" },
          { name: "finalized", type: "bool" },
          { name: "victims", type: "uint32" },
          { name: "talliedVictims", type: "uint32" },
          { name: "price", type: "uint256" },
          { name: "layer2", type: "uint256" },
          { name: "layer3", type: "uint256" },
          { name: "layer4", type: "uint256" },
          { name: "r2", type: "uint256" },
          { name: "r3", type: "uint256" },
          { name: "r4", type: "uint256" },
        ],
      },
    ],
  },
] as const;

/** Every revert the app explains (lib/calls.ts `describeRevert`). */
export const revertErrorsAbi = [...registryErrorsAbi, ...disputesErrorsAbi, ...intakeErrorsAbi, ...bondErrorsAbi] as const;

/** Registry v3 `VaultStatus` (append-only: off-chain code reads it as a number). */
export const ONCHAIN_VAULT_STATUS = ["None", "Active", "Closed", "Cancelled", "Pending"] as const;
/** Registry v3 `ClaimKind` (append-only). */
export const CLAIM_KIND = ["Release", "Liquidation", "CloseUser", "CloseMarketMaker", "Refund", "Fee", "Retired"] as const;
/** Registry v3 `ClaimStatus` (append-only; `Disputed` came with IZecuredDisputes). */
export const CLAIM_STATUS = ["None", "Pending", "Paid", "WrittenOff", "Compensated", "Disputed"] as const;
/** Intake `MintBlock`. */
export const MINT_BLOCK = ["None", "Halted", "NotBonded", "Paused", "CapExceeded"] as const;
/** Intake `RefundReason`. */
export const REFUND_REASON = [
  "ModeRefund",
  "VaultNotNew",
  "PendingExists",
  "AccountHasVault",
  "Halted",
  "CapExceeded",
  "PendingConverted",
] as const;

export const adapterAbi = [
  {
    type: "function",
    name: "invalidateNonce",
    stateMutability: "nonpayable",
    inputs: [{ name: "nonce", type: "uint256" }],
    outputs: [],
  },
  {
    type: "function",
    name: "nonceUsed",
    stateMutability: "view",
    inputs: [
      { name: "account", type: "address" },
      { name: "nonce", type: "uint256" },
    ],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "MAX_MARGIN_BPS",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "uint16" }],
  },
] as const;

export const morphoAbi = [
  {
    type: "function",
    name: "setAuthorization",
    stateMutability: "nonpayable",
    inputs: [
      { name: "authorized", type: "address" },
      { name: "newIsAuthorized", type: "bool" },
    ],
    outputs: [],
  },
  {
    type: "function",
    name: "isAuthorized",
    stateMutability: "view",
    inputs: [
      { name: "authorizer", type: "address" },
      { name: "authorized", type: "address" },
    ],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "borrow",
    stateMutability: "nonpayable",
    inputs: [
      marketParamsTuple,
      { name: "assets", type: "uint256" },
      { name: "shares", type: "uint256" },
      { name: "onBehalf", type: "address" },
      { name: "receiver", type: "address" },
    ],
    outputs: [
      { name: "assetsBorrowed", type: "uint256" },
      { name: "sharesBorrowed", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "repay",
    stateMutability: "nonpayable",
    inputs: [
      marketParamsTuple,
      { name: "assets", type: "uint256" },
      { name: "shares", type: "uint256" },
      { name: "onBehalf", type: "address" },
      { name: "data", type: "bytes" },
    ],
    outputs: [
      { name: "assetsRepaid", type: "uint256" },
      { name: "sharesRepaid", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "position",
    stateMutability: "view",
    inputs: [
      { name: "id", type: "bytes32" },
      { name: "user", type: "address" },
    ],
    outputs: [
      { name: "supplyShares", type: "uint256" },
      { name: "borrowShares", type: "uint128" },
      { name: "collateral", type: "uint128" },
    ],
  },
  {
    type: "function",
    name: "market",
    stateMutability: "view",
    inputs: [{ name: "id", type: "bytes32" }],
    outputs: [
      { name: "totalSupplyAssets", type: "uint128" },
      { name: "totalSupplyShares", type: "uint128" },
      { name: "totalBorrowAssets", type: "uint128" },
      { name: "totalBorrowShares", type: "uint128" },
      { name: "lastUpdate", type: "uint128" },
      { name: "fee", type: "uint128" },
    ],
  },
] as const;

export const irmAbi = [
  {
    type: "function",
    name: "borrowRateView",
    stateMutability: "view",
    inputs: [
      marketParamsTuple,
      {
        type: "tuple",
        name: "market",
        components: [
          { name: "totalSupplyAssets", type: "uint128" },
          { name: "totalSupplyShares", type: "uint128" },
          { name: "totalBorrowAssets", type: "uint128" },
          { name: "totalBorrowShares", type: "uint128" },
          { name: "lastUpdate", type: "uint128" },
          { name: "fee", type: "uint128" },
        ],
      },
    ],
    outputs: [{ name: "", type: "uint256" }],
  },
] as const;

export const oracleAbi = [
  { type: "function", name: "price", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
] as const;

export const erc20Abi = [
  {
    type: "function",
    name: "balanceOf",
    stateMutability: "view",
    inputs: [{ name: "account", type: "address" }],
    outputs: [{ name: "", type: "uint256" }],
  },
  {
    type: "function",
    name: "approve",
    stateMutability: "nonpayable",
    inputs: [
      { name: "spender", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "transfer",
    stateMutability: "nonpayable",
    inputs: [
      { name: "to", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    outputs: [{ name: "", type: "bool" }],
  },
] as const;
