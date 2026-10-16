/**
 * Test fixture: addresses from `forge script DeployZecured` (v3, with ZecuredVersions) simulated
 * on a Base Sepolia fork at block 48,198,682 with a public test deployer key
 * (contracts/deployments/base-sepolia.dry-run.json; not committed). These contracts do not exist
 * on-chain. The loan token is Circle's testnet USDC; the market's oracle is the DemoShockOracle
 * (MOCK / DEMO) around a MockZecOracle (MOCK).
 */
export const DRY_RUN_ENV = {
  NEXT_PUBLIC_BACKEND_URL: "http://127.0.0.1:8080",
  NEXT_PUBLIC_REGISTRY: "0x2849a4FA18C4bD42687bD9B8eBDC2F4D89ECfB2F",
  NEXT_PUBLIC_ADAPTER: "0x3074AcAEa758A41C42f6EbCf5686f4a7f24ac957",
  NEXT_PUBLIC_INTAKE: "0x5197D7051ac19aD80dbD8829E46062B8d6d451aE",
  NEXT_PUBLIC_MORPHO: "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
  NEXT_PUBLIC_MARKET_ID: "0xabd581e8f7e88ea8e3e7abde60d9aea72986782f9b5d3fb3518b42968a89817e",
  NEXT_PUBLIC_USDC: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  NEXT_PUBLIC_ORACLE: "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
  NEXT_PUBLIC_ZZEC: "0xbB63C296eb88567318C36fa29a26be77D8695b84",
  NEXT_PUBLIC_SIGNERS: [
    "0x2D9E846e519c61b452ef3ad4Be34B28b22A6f6F3",
    "0x492C397388982CFdDe71cA67d3C30E2Bc9e6643B",
    "0x99b3a2b14875200f643830bc81a91B0701e1cD76",
    "0xc19dfB0971D572Ef39182497E81B9Bc12692c4b9",
    "0xc76BAEd8F702e9CB69E54b196b2628633A2E57Ba",
  ].join(","),
} as const;

/** The same deployment's other contracts (the app reads them through the Registry and the Intake). */
export const DRY_RUN_CONTRACTS = {
  BaseOracle: "0xC2f3515C272383D2b5e055D4CF39c9d719D232e1",
  CapPrice: "0x2860F0aB28D7ADa29CFB4D74B60bCD29C414BD20",
  DemoShockOracle: "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
  LoanToken: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  MarketOracle: "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
  MockZecOracle: "0xC2f3515C272383D2b5e055D4CF39c9d719D232e1",
  SettlementAdapter: "0x3074AcAEa758A41C42f6EbCf5686f4a7f24ac957",
  SignerBond: "0xf42821eF06F68a01bba08c91a16368A83C002586",
  ZZEC: "0xbB63C296eb88567318C36fa29a26be77D8695b84",
  ZecuredCompensator: "0xEe045dF461915D9E4C919DAFbB2Df2af7e62922b",
  ZecuredIntake: "0x5197D7051ac19aD80dbD8829E46062B8d6d451aE",
  ZecuredRegistry: "0x2849a4FA18C4bD42687bD9B8eBDC2F4D89ECfB2F",
  ZecuredSlasher: "0xCcF6091bB163431D3C51C3B48CC853549df003E2",
  ZecuredSpendLogic: "0xDB0768AE34e9926ddF58eF1aBC4C82547616a542",
  ZecuredVersions: "0x9BfbaDc8108F2e1959A2a58a647a6DAf07C99527",
} as const;

/** The deployment's EIP-712 domain separators, computed on the fork by the contracts themselves. */
export const DRY_RUN_DOMAIN_SEPARATORS = {
  registry: "0x6cdcc5b6f897215c97f90371f78aa8080c7ad48d35a3dae978e97eda6f1931b9",
  adapter: "0xc2b25f0ec7918d04c01a3c5b409abe0956fcf444a089a8aa134117e7d5d8cb8e",
} as const;

/**
 * `GET /config` as the backend serves it for the same deployment (wire format: the signer
 * network's Registry / Adapter in lowercase, the deployment file's checksummed addresses,
 * decimal strings, the v3 parameters as the deployment file writes them), with a bundler and a
 * paymaster configured.
 */
export const DRY_RUN_CONFIG = {
  adapter: "0x3074acaea758a41c42f6ebcf5686f4a7f24ac957",
  chainId: 84532,
  confirmations: 3,
  contracts: DRY_RUN_CONTRACTS,
  denominations: ["1000000", "5000000"],
  intake: "0x5197D7051ac19aD80dbD8829E46062B8d6d451aE",
  market: {
    collateralToken: "0xbB63C296eb88567318C36fa29a26be77D8695b84",
    irm: "0x46415998764C29aB2a25CbeA6254146D50D22687",
    lltv: "625000000000000000",
    loanToken: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    marketId: "0xabd581e8f7e88ea8e3e7abde60d9aea72986782f9b5d3fb3518b42968a89817e",
    morphoBlue: "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
    oracle: "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
  },
  openVaultStampBits: 20,
  registry: "0x2849a4fa18c4bd42687bd9b8ebdc2f4d89ecfb2f",
  sponsorship: true,
  v3: {
    feeBps: 50,
    minFeeZat: 20000,
    minRefundZat: 20000,
    refundAbandonPeriod: 2592000,
    pendingRefundDelay: 86400,
    maxPause: 2592000,
    livenessPeriod: 86400,
    commitGrace: 7200,
    livenessPremiumBps: 500,
    coverageBps: 10000,
    maxTotalZat: 150000000,
    incidentWindow: 259200,
  },
  vaultExpiryBlocks: 288,
} as const;
