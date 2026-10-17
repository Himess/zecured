/**
 * The v4 ABI fragments the app uses, as the v4 interface freeze names them (SignerBondV2's
 * liveness receipts, the Settlement Adapter's pre-liquidation, the Registry's proven deliveries and
 * dormant refunds, ZecuredSwap), plus the v3 views the status and market pages read. Kept apart
 * from lib/abi.ts: lib/abi4.test.ts pins each against the frozen signature, and against the
 * compiled ABIs in contracts/abi once those contain it.
 */

import { confirmationExpiredEvent, payoutConfirmedEvent, payoutProvenEvent, refundDormantEvent } from "./abi";

const preLiquidationTermsTuple = {
  type: "tuple",
  name: "t",
  components: [
    { name: "preLltv", type: "uint256" },
    { name: "preLcf1", type: "uint256" },
    { name: "preLcf2", type: "uint256" },
    { name: "preLif1", type: "uint256" },
    { name: "preLif2", type: "uint256" },
    { name: "expiry", type: "uint64" },
  ],
} as const;

/** SignerBondV2, v4 (R-5): a signer's receipt arms a claim's liveness. */
export const bondV4Abi = [
  {
    type: "function",
    name: "armLiveness",
    stateMutability: "nonpayable",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "receivedAt", type: "uint64" },
      { name: "signature", type: "bytes" },
    ],
    outputs: [],
  },
  {
    type: "function",
    name: "hashDestinationReceived",
    stateMutability: "view",
    inputs: [
      { name: "claimId", type: "bytes32" },
      { name: "commitment", type: "bytes32" },
      { name: "receivedAt", type: "uint64" },
    ],
    outputs: [{ name: "", type: "bytes32" }],
  },
  {
    type: "function",
    name: "livenessArming",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [
      { name: "commitment", type: "bytes32" },
      { name: "payableAt", type: "uint64" },
    ],
  },
  {
    type: "event",
    name: "LivenessArmed",
    inputs: [
      { name: "claimId", type: "bytes32", indexed: true },
      { name: "commitment", type: "bytes32", indexed: false },
      { name: "payableAt", type: "uint64", indexed: false },
    ],
  },
  { type: "error", name: "ReceiptFromTheFuture", inputs: [{ name: "receivedAt", type: "uint64" }] },
  { type: "error", name: "NotASigner", inputs: [{ name: "caller", type: "address" }] },
  { type: "error", name: "PriceStale", inputs: [] },
] as const;

/** SignerBondV2's views the status and market pages read (v3, in contracts/abi). */
export const bondViewsAbi = [
  { type: "function", name: "effectiveBond", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
  { type: "function", name: "totalSlashable", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
  { type: "function", name: "minBond", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
  {
    type: "function",
    name: "bondOf",
    stateMutability: "view",
    inputs: [{ name: "signer", type: "uint8" }],
    outputs: [{ name: "", type: "uint256" }],
  },
  { type: "function", name: "halted", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "bool" }] },
  { type: "function", name: "fullyBonded", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "bool" }] },
] as const;

/** The Settlement Adapter, v4: pre-liquidation terms (V4 freeze section 5). */
export const adapterV4Abi = [
  {
    type: "function",
    name: "setPreLiquidation",
    stateMutability: "nonpayable",
    inputs: [preLiquidationTermsTuple],
    outputs: [],
  },
  { type: "function", name: "clearPreLiquidation", stateMutability: "nonpayable", inputs: [], outputs: [] },
  {
    type: "function",
    name: "preLiquidationTerms",
    stateMutability: "view",
    inputs: [{ name: "account", type: "address" }],
    outputs: [{ ...preLiquidationTermsTuple, name: "" }],
  },
  {
    type: "event",
    name: "PreLiquidationSet",
    inputs: [
      { name: "account", type: "address", indexed: true },
      { ...preLiquidationTermsTuple, name: "terms", indexed: false },
    ],
  },
  {
    type: "event",
    name: "PreLiquidationCleared",
    inputs: [{ name: "account", type: "address", indexed: true }],
  },
  {
    type: "event",
    name: "PreLiquidated",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "account", type: "address", indexed: true },
      { name: "liquidator", type: "address", indexed: true },
      { name: "repaidAssets", type: "uint256", indexed: false },
      { name: "seized", type: "uint64", indexed: false },
    ],
  },
  { type: "error", name: "NotAVaultAccount", inputs: [{ name: "borrower", type: "address" }] },
  {
    type: "error",
    name: "InvalidPreLltv",
    inputs: [
      { name: "preLltv", type: "uint256" },
      { name: "lltv", type: "uint256" },
    ],
  },
  {
    type: "error",
    name: "InvalidPreLcf",
    inputs: [
      { name: "preLcf1", type: "uint256" },
      { name: "preLcf2", type: "uint256" },
    ],
  },
  {
    type: "error",
    name: "InvalidPreLif",
    inputs: [
      { name: "preLif1", type: "uint256" },
      { name: "preLif2", type: "uint256" },
      { name: "maxLif", type: "uint256" },
    ],
  },
  { type: "error", name: "InvalidExpiry", inputs: [{ name: "expiry", type: "uint64" }] },
] as const;

/** The Registry's views the claim badges and the status page read (v3, in contracts/abi). */
export const registryViewsAbi = [
  {
    type: "function",
    name: "claimInput",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [{ name: "", type: "bytes32" }],
  },
  { type: "function", name: "lockedZatoshi", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
  { type: "function", name: "openVaultCount", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint256" }] },
] as const;

/** R-11 at the Registry's address (the spend logic, through its fallback): dormant refunds. */
export const refundsV4Abi = [
  { type: "function", name: "refundDormancy", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  {
    type: "function",
    name: "isRefundDormant",
    stateMutability: "view",
    inputs: [{ name: "claimId", type: "bytes32" }],
    outputs: [{ name: "", type: "bool" }],
  },
] as const;

/**
 * The Registry's v4 events (served at its address; the spend logic emits them through its fallback):
 * the same fragments as lib/abi.ts's `registryEventsAbi` carries.
 */
export const registryV4EventsAbi = [payoutProvenEvent, payoutConfirmedEvent, confirmationExpiredEvent, refundDormantEvent] as const;

/** R-12: a proof landed as a refund; its claim opens at `finalizeRefund` (`reason` is a `RefundReason`). */
export const refundPendingEvent = {
  type: "event",
  name: "RefundPending",
  inputs: [
    { name: "vaultId", type: "bytes32", indexed: true },
    { name: "depositTag", type: "bytes32", indexed: true },
    { name: "reason", type: "uint8", indexed: false },
    { name: "readyHeight", type: "uint256", indexed: false },
    { name: "readyTime", type: "uint256", indexed: false },
  ],
} as const;

/**
 * The Intake, v4 (R-12, V4 freeze section 4): a deposit proven as a refund waits, like an activation
 * (K3), before its Refund claim opens (`finalizeRefund`); pending refunds are keyed by deposit tag.
 */
export const intakeV4Abi = [
  refundPendingEvent,
  {
    type: "function",
    name: "pendingRefund",
    stateMutability: "view",
    inputs: [{ name: "depositTag", type: "bytes32" }],
    outputs: [
      {
        type: "tuple",
        name: "",
        components: [
          { name: "vaultId", type: "bytes32" },
          { name: "account", type: "address" },
          { name: "amount", type: "uint64" },
          { name: "reason", type: "uint8" },
          { name: "anchor", type: "bytes32" },
          { name: "readyHeight", type: "uint256" },
          { name: "readyTime", type: "uint256" },
          { name: "versionId", type: "bytes32" },
        ],
      },
    ],
  },
  { type: "function", name: "pendingRefundTotal", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
] as const;

/** The Intake's views the status page reads (v3, in contracts/abi). */
export const intakeViewsAbi = [
  { type: "function", name: "capRoom", stateMutability: "view", inputs: [], outputs: [{ name: "zatoshi", type: "uint256" }] },
  { type: "function", name: "pendingTotal", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  { type: "function", name: "maxTotalZat", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint64" }] },
  { type: "function", name: "coverageBps", stateMutability: "view", inputs: [], outputs: [{ name: "", type: "uint16" }] },
] as const;

const swapQuoteTuple = {
  type: "tuple",
  name: "q",
  components: [
    { name: "maker", type: "address" },
    { name: "taker", type: "address" },
    { name: "side", type: "uint8" },
    { name: "zatoshi", type: "uint64" },
    { name: "usdc", type: "uint256" },
    { name: "makerReceiverCommitment", type: "bytes32" },
    { name: "payWindow", type: "uint32" },
    { name: "quoteDeadline", type: "uint64" },
    { name: "nonce", type: "uint256" },
  ],
} as const;

/** ZecuredSwap (the v4 design's section 5.4), as the freeze names it. */
export const swapAbi = [
  {
    type: "function",
    name: "open",
    stateMutability: "nonpayable",
    inputs: [swapQuoteTuple, { name: "makerSig", type: "bytes" }, { name: "takerReceiverCommitment", type: "bytes32" }],
    outputs: [{ name: "orderId", type: "bytes32" }],
  },
  {
    type: "function",
    name: "refund",
    stateMutability: "nonpayable",
    inputs: [{ name: "orderId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "event",
    name: "OrderOpened",
    inputs: [
      { name: "orderId", type: "bytes32", indexed: true },
      { name: "maker", type: "address", indexed: true },
      { name: "taker", type: "address", indexed: true },
      { name: "side", type: "uint8", indexed: false },
      { name: "zatoshi", type: "uint64", indexed: false },
      { name: "usdc", type: "uint256", indexed: false },
      { name: "receiverCommitment", type: "bytes32", indexed: false },
      { name: "payDeadline", type: "uint64", indexed: false },
    ],
  },
  {
    type: "event",
    name: "SettlementPending",
    inputs: [
      { name: "orderId", type: "bytes32", indexed: true },
      { name: "anchor", type: "bytes32", indexed: false },
      { name: "readyHeight", type: "uint256", indexed: false },
      { name: "readyTime", type: "uint256", indexed: false },
    ],
  },
  { type: "event", name: "OrderSettled", inputs: [{ name: "orderId", type: "bytes32", indexed: true }] },
  { type: "event", name: "SettlementDropped", inputs: [{ name: "orderId", type: "bytes32", indexed: true }] },
  { type: "event", name: "OrderRefunded", inputs: [{ name: "orderId", type: "bytes32", indexed: true }] },
] as const;

/** ERC-20 reads and approval for the user's own wallet (the swap and the lending form). */
export const erc20AllowanceAbi = [
  {
    type: "function",
    name: "allowance",
    stateMutability: "view",
    inputs: [
      { name: "owner", type: "address" },
      { name: "spender", type: "address" },
    ],
    outputs: [{ name: "", type: "uint256" }],
  },
] as const;

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

/** Morpho Blue's supply side, for the market page's optional lending form (the user's own wallet). */
export const morphoSupplyAbi = [
  {
    type: "function",
    name: "supply",
    stateMutability: "nonpayable",
    inputs: [
      marketParamsTuple,
      { name: "assets", type: "uint256" },
      { name: "shares", type: "uint256" },
      { name: "onBehalf", type: "address" },
      { name: "data", type: "bytes" },
    ],
    outputs: [
      { name: "assetsSupplied", type: "uint256" },
      { name: "sharesSupplied", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "withdraw",
    stateMutability: "nonpayable",
    inputs: [
      marketParamsTuple,
      { name: "assets", type: "uint256" },
      { name: "shares", type: "uint256" },
      { name: "onBehalf", type: "address" },
      { name: "receiver", type: "address" },
    ],
    outputs: [
      { name: "assetsWithdrawn", type: "uint256" },
      { name: "sharesWithdrawn", type: "uint256" },
    ],
  },
] as const;

/** The v4 reverts the app explains: the bond's arming and the Adapter's pre-liquidation terms. */
export const v4ErrorsAbi = [...bondV4Abi, ...adapterV4Abi].filter((i) => i.type === "error");
