/**
 * The ABI fragments the app uses (docs/INTERFACES.md § 3; Morpho Blue's IMorpho / IIrm).
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

/** Public Registry events (docs/INTERFACES.md § 3.2). */
export const registryEventsAbi = [
  {
    type: "event",
    name: "VaultActivated",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "account", type: "address", indexed: true },
      { name: "amount", type: "uint64", indexed: false },
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
  {
    type: "event",
    name: "PayoutConfirmed",
    inputs: [
      { name: "vaultId", type: "bytes32", indexed: true },
      { name: "claimIds", type: "bytes32[]", indexed: false },
    ],
  },
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
] as const;

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
  ...registryEventsAbi,
  ...registryErrorsAbi,
] as const;

/** Registry `VaultStatus` enum. */
export const ONCHAIN_VAULT_STATUS = ["None", "Active", "Closed"] as const;

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
