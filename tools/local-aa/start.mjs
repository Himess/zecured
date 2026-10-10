// Local ERC-4337 infrastructure for an anvil fork of Base Sepolia: Pimlico's alto bundler and
// mock paymaster. It replaces the hosted bundler / paymaster in local tests, so the app's real
// UserOp path (Safe 4337 account, sponsored, through the backend proxy) runs without API keys.
//
//   node tools/local-aa/start.mjs            # bundler on :4337, paymaster on :4338
//
// Then start the backend with BUNDLER_URL=http://127.0.0.1:4337 PAYMASTER_URL=http://127.0.0.1:4338.
// The mock paymaster sponsors every UserOp it sees: the backend's paymaster allowlist is what
// limits sponsorship, exactly as with a hosted paymaster. Never point this at a public network.
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { paymaster } from "@pimlico/mock-paymaster";
import { createTestClient, http, parseEther } from "viem";
import { privateKeyToAccount } from "viem/accounts";

const ANVIL_RPC = process.env.ANVIL_RPC ?? "http://127.0.0.1:8545";
const BUNDLER_PORT = Number(process.env.BUNDLER_PORT ?? 4337);
const PAYMASTER_PORT = Number(process.env.PAYMASTER_PORT ?? 4338);
const ENTRY_POINT_V07 = "0x0000000071727De22E5E9d8BAf0edAc6f37da032";

// anvil's well-known development keys #8 and #9 (public test keys, local fork only).
const EXECUTOR_KEY = "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";
const UTILITY_KEY = "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6";

const chainId = await (async () => {
  const res = await fetch(ANVIL_RPC, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_chainId", params: [] }),
  });
  return Number((await res.json()).result);
})();
if (chainId !== 84532) throw new Error(`expected an anvil fork of Base Sepolia (84532), got chain ${chainId}`);

// Refuse anything that is not anvil: anvil_setBalance only exists on development nodes.
const testClient = createTestClient({ mode: "anvil", transport: http(ANVIL_RPC) });
for (const key of [EXECUTOR_KEY, UTILITY_KEY]) {
  await testClient.setBalance({ address: privateKeyToAccount(key).address, value: parseEther("100") });
}

// alto's package exports do not expose its CLI entry; it is the package's `bin`.
const altoCli = fileURLToPath(new URL("./node_modules/@pimlico/alto/esm/cli/alto.js", import.meta.url));
const alto = spawn(
  process.execPath,
  [
    altoCli,
    "--rpc-url", ANVIL_RPC,
    "--entrypoints", ENTRY_POINT_V07,
    "--executor-private-keys", EXECUTOR_KEY,
    "--utility-private-key", UTILITY_KEY,
    "--chain-type", "op-stack",
    "--safe-mode", "false",
    "--deploy-simulations-contract", "true",
    "--port", String(BUNDLER_PORT),
    "--log-level", process.env.ALTO_LOG_LEVEL ?? "warn",
  ],
  { stdio: "inherit" },
);
alto.on("exit", (code) => {
  console.error(`alto exited (${code})`);
  process.exit(code ?? 1);
});

// Wait for the bundler before the paymaster (it estimates gas through it).
const bundlerUrl = `http://127.0.0.1:${BUNDLER_PORT}`;
for (let i = 0; ; i++) {
  try {
    const res = await fetch(bundlerUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_supportedEntryPoints", params: [] }),
    });
    if (res.ok) break;
  } catch {}
  if (i > 120) throw new Error("bundler did not start");
  await new Promise((r) => setTimeout(r, 500));
}

const pm = paymaster({ anvilRpc: ANVIL_RPC, altoRpc: bundlerUrl, port: PAYMASTER_PORT, host: "127.0.0.1" });
await pm.start();
console.log(`local AA ready: bundler ${bundlerUrl}, paymaster http://127.0.0.1:${PAYMASTER_PORT}`);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, async () => {
    alto.kill();
    await pm.stop();
    process.exit(0);
  });
}
