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
} as const;
