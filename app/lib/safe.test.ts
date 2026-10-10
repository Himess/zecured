import { describe, expect, it } from "vitest";
import {
  createPublicClient,
  custom,
  decodeFunctionData,
  encodeAbiParameters,
  parseAbi,
  toFunctionSelector,
  type Address,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";
import { toSafeSmartAccount } from "permissionless/accounts";
import { predictAccountAddress, SAFE, SAFE_PROXY_CREATION_CODE, safeFactoryData, safeInitializer } from "./safe";

const KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"; // public test key
const owner = privateKeyToAccount(KEY);

/** A client whose only "RPC" answers proxyCreationCode(), eth_chainId and eth_getCode (undeployed). */
function offlineClient() {
  const selector = toFunctionSelector("proxyCreationCode()");
  const calls: string[] = [];
  const client = createPublicClient({
    chain: baseSepolia,
    transport: custom({
      async request({ method, params }: { method: string; params?: unknown }) {
        calls.push(method);
        if (method === "eth_chainId") return "0x14a34";
        if (method === "eth_getCode") return "0x";
        if (method === "eth_call") {
          const [tx] = params as [{ to: Address; data: string }];
          if (tx.to.toLowerCase() === SAFE.proxyFactory.toLowerCase() && tx.data.startsWith(selector)) {
            return encodeAbiParameters([{ type: "bytes" }], [SAFE_PROXY_CREATION_CODE]);
          }
        }
        throw new Error(`unexpected RPC ${method}`);
      },
    }),
  });
  return { client, calls };
}

async function permissionlessSafe(useMultiSendForSetup: boolean) {
  const { client } = offlineClient();
  return toSafeSmartAccount({
    client,
    owners: [owner],
    version: SAFE.version,
    entryPoint: { address: SAFE.entryPoint, version: SAFE.entryPointVersion },
    safe4337ModuleAddress: SAFE.safe4337Module,
    safeProxyFactoryAddress: SAFE.proxyFactory,
    safeSingletonAddress: SAFE.singleton,
    safeModuleSetupAddress: SAFE.moduleSetup,
    multiSendAddress: SAFE.multiSend,
    multiSendCallOnlyAddress: SAFE.multiSendCallOnly,
    saltNonce: SAFE.saltNonce,
    useMultiSendForSetup,
  });
}

describe("counterfactual Safe address", () => {
  it("matches permissionless.js for the pinned config (useMultiSendForSetup = false)", async () => {
    const account = await permissionlessSafe(false);
    expect(predictAccountAddress(owner.address)).toBe(await account.getAddress());
  });

  it("uses the same factory call permissionless puts in the first UserOp", async () => {
    const account = await permissionlessSafe(false);
    const { factory, factoryData } = await account.getFactoryArgs();
    expect(factory).toBe(SAFE.proxyFactory);
    expect(factoryData).toBe(safeFactoryData(owner.address));
  });

  it("differs from permissionless' default config (MultiSend setup), which is why it is pinned", async () => {
    const account = await permissionlessSafe(true);
    expect(predictAccountAddress(owner.address)).not.toBe(await account.getAddress());
  });

  it("is a pure function of the owner", () => {
    const a = predictAccountAddress(owner.address);
    expect(predictAccountAddress(owner.address)).toBe(a);
    expect(predictAccountAddress("0x0000000000000000000000000000000000000001")).not.toBe(a);
  });
});

describe("initCode accepted by the allowlist paymaster (SpikeAllowlistPaymaster._checkInitCode)", () => {
  it("setup(1 owner, threshold 1, SafeModuleSetup, enableModules([Safe4337Module]), fallback = Safe4337Module, payment 0)", () => {
    const setup = decodeFunctionData({
      abi: parseAbi([
        "function setup(address[] owners, uint256 threshold, address to, bytes data, address fallbackHandler, address paymentToken, uint256 payment, address paymentReceiver)",
      ]),
      data: safeInitializer(owner.address),
    });
    const [owners, threshold, to, data, fallbackHandler, , payment] = setup.args;
    expect(owners).toEqual([owner.address]);
    expect(threshold).toBe(1n);
    expect(to).toBe(SAFE.moduleSetup);
    expect(fallbackHandler).toBe(SAFE.safe4337Module);
    expect(payment).toBe(0n);
    const enable = decodeFunctionData({ abi: parseAbi(["function enableModules(address[] modules)"]), data });
    expect(enable.args[0]).toEqual([SAFE.safe4337Module]);
  });
});
