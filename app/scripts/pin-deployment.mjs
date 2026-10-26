// Writes the app's build pins (security review H-6) from a deployment file the deploy scripts wrote, so a live build
// signs for and reads exactly that deployment:
//   node scripts/pin-deployment.mjs base-sepolia http://127.0.0.1:8080 > .env.local
// It reads ../contracts/deployments/<name>.json and, when present, <name>.swap.json (the swap contract). Every value
// is public. The demo path (P-1) is switched on for a testnet deployment only.
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const [name, backend] = process.argv.slice(2);
if (!name || !backend) {
  console.error("usage: node scripts/pin-deployment.mjs <deployment name> <backend URL>");
  process.exit(1);
}
const dir = fileURLToPath(new URL("../../contracts/deployments/", import.meta.url));
const d = JSON.parse(readFileSync(`${dir}${name}.json`, "utf8"));
const swapFile = `${dir}${name}.swap.json`;
const swap = existsSync(swapFile) ? JSON.parse(readFileSync(swapFile, "utf8")) : null;
const testnet = d.chainId === 84532;

const pins = {
  NEXT_PUBLIC_BACKEND_URL: backend,
  NEXT_PUBLIC_ZECURED_DEPLOYMENT: testnet ? "testnet" : "mainnet-beta",
  NEXT_PUBLIC_REGISTRY: d.contracts.ZecuredRegistry,
  NEXT_PUBLIC_ADAPTER: d.contracts.SettlementAdapter,
  NEXT_PUBLIC_INTAKE: d.contracts.ZecuredIntake,
  NEXT_PUBLIC_MORPHO: d.market.morphoBlue,
  NEXT_PUBLIC_MARKET_ID: d.market.marketId,
  NEXT_PUBLIC_USDC: d.market.loanToken,
  NEXT_PUBLIC_ORACLE: d.market.oracle,
  NEXT_PUBLIC_ZZEC: d.market.collateralToken,
  NEXT_PUBLIC_IRM: d.market.irm,
  NEXT_PUBLIC_SIGNERS: d.signers.join(","),
  NEXT_PUBLIC_SWAP: swap?.ZecuredSwap ?? "",
  NEXT_PUBLIC_DEMO_PATH: testnet ? "1" : "",
};
for (const [k, v] of Object.entries(pins)) {
  if (v === undefined) {
    console.error(`${name}.json has no value for ${k}`);
    process.exit(1);
  }
}
console.log(`# Pins of contracts/deployments/${name}.json${swap ? ` and ${name}.swap.json` : ""} (scripts/pin-deployment.mjs).`);
for (const [k, v] of Object.entries(pins)) console.log(`${k}=${v}`);
