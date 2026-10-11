/**
 * Test fixture: addresses from `forge script DeployZecured` simulated on a Base Sepolia fork
 * (contracts/deployments/base-sepolia.dry-run.json). These contracts do not exist on-chain.
 */
export const DRY_RUN_ENV = {
  NEXT_PUBLIC_BACKEND_URL: "http://127.0.0.1:8080",
  NEXT_PUBLIC_REGISTRY: "0xbdB942d882882bF795aA9c0Cd20ADe2115c73AAc",
  NEXT_PUBLIC_ADAPTER: "0xC0a2A554dcFB5a1bd7e5E769A2Ad0c33BA0500fc",
  NEXT_PUBLIC_MORPHO: "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
  NEXT_PUBLIC_MARKET_ID: "0x2b619287a851dac5813f6829422d6733296c54b01bba0ec5b4856893fa2362ac",
  NEXT_PUBLIC_USDC: "0xab07317f7B9390394271D9c03FE2E912C0E58FA5",
  NEXT_PUBLIC_ORACLE: "0xcFf71125555a66A7f9D593A73961A8ec026d97D4",
  NEXT_PUBLIC_ZZEC: "0xa7d7b3f6bC459fdE532a1B3077aa187A4927dEcA",
  NEXT_PUBLIC_SIGNERS: [
    "0x2D9E846e519c61b452ef3ad4Be34B28b22A6f6F3",
    "0x492C397388982CFdDe71cA67d3C30E2Bc9e6643B",
    "0x99b3a2b14875200f643830bc81a91B0701e1cD76",
    "0xc19dfB0971D572Ef39182497E81B9Bc12692c4b9",
    "0xc76BAEd8F702e9CB69E54b196b2628633A2E57Ba",
  ].join(","),
} as const;

/**
 * `GET /config` as the backend serves it for the same deployment (wire format: the signer
 * network's Registry / Adapter in lowercase, the deployment file's checksummed addresses,
 * decimal strings), with a bundler and a paymaster configured.
 */
export const DRY_RUN_CONFIG = {
  adapter: "0xc0a2a554dcfb5a1bd7e5e769a2ad0c33ba0500fc",
  chainId: 84532,
  confirmations: 3,
  contracts: {
    MockUSDC: "0xab07317f7B9390394271D9c03FE2E912C0E58FA5",
    MockZecOracle: "0xcFf71125555a66A7f9D593A73961A8ec026d97D4",
    SettlementAdapter: "0xC0a2A554dcFB5a1bd7e5E769A2Ad0c33BA0500fc",
    ZZEC: "0xa7d7b3f6bC459fdE532a1B3077aa187A4927dEcA",
    ZecuredRegistry: "0xbdB942d882882bF795aA9c0Cd20ADe2115c73AAc",
  },
  denominations: ["1000000", "5000000"],
  market: {
    collateralToken: "0xa7d7b3f6bC459fdE532a1B3077aa187A4927dEcA",
    irm: "0x46415998764C29aB2a25CbeA6254146D50D22687",
    lltv: "625000000000000000",
    loanToken: "0xab07317f7B9390394271D9c03FE2E912C0E58FA5",
    marketId: "0x2b619287a851dac5813f6829422d6733296c54b01bba0ec5b4856893fa2362ac",
    morphoBlue: "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
    oracle: "0xcFf71125555a66A7f9D593A73961A8ec026d97D4",
  },
  registry: "0xbdb942d882882bf795aa9c0cd20ade2115c73aac",
  sponsorship: true,
  vaultExpiryBlocks: 288,
} as const;
