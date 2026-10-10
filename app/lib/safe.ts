/**
 * The pinned smart-account configuration and its counterfactual address.
 *
 * Each loan gets a fresh 1-owner Safe v1.4.1 with the Safe4337Module v0.3.0 (EntryPoint
 * v0.7), deployed by its first sponsored UserOp. `attestDeposit` supplies collateral to this
 * address *before* the account exists, so the app must compute it with exactly the options it
 * will deploy with (docs/spikes/SPIKE_C.md, "Pin the account config"):
 *
 *   setup([owner], 1, to = SafeModuleSetup, data = enableModules([Safe4337Module]),
 *         fallbackHandler = Safe4337Module, paymentToken = 0, payment = 0, paymentReceiver = 0)
 *   createProxyWithNonce(Safe 1.4.1 singleton, initializer, saltNonce = 0)
 *
 * This is permissionless.js' `toSafeSmartAccount({ version: "1.4.1", useMultiSendForSetup:
 * false, saltNonce: 0n })`, and the initCode the spike's allowlist paymaster accepts
 * (setup.to = SafeModuleSetup). The default `useMultiSendForSetup = true` would give a
 * different address. The prediction is pure (no RPC); a test checks it against permissionless.
 */
import {
  concatHex,
  encodeAbiParameters,
  encodeFunctionData,
  encodePacked,
  getContractAddress,
  keccak256,
  type Address,
  type Hex,
} from "viem";

/** Canonical deployments on Base Sepolia (safe-deployments, checked in docs/spikes/SPIKE_C.md). */
export const SAFE = {
  version: "1.4.1",
  proxyFactory: "0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67",
  singleton: "0x41675C099F32341bf84BFc5382aF534df5C7461a",
  moduleSetup: "0x2dd68b007B46fBe91B9A7c3EDa5A7a1063cB5b47",
  safe4337Module: "0x75cf11467937ce3F2f357CE24ffc3DBF8fD5c226",
  multiSend: "0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526",
  multiSendCallOnly: "0x9641d764fc13c8B624c04430C7356C1C7C8102e2",
  entryPoint: "0x0000000071727De22E5E9d8BAf0edAc6f37da032",
  entryPointVersion: "0.7",
  saltNonce: 0n,
  useMultiSendForSetup: false,
} as const satisfies Record<string, string | bigint | boolean>;

/**
 * `SafeProxyFactory.proxyCreationCode()` of Safe v1.4.1 (SafeProxy creation bytecode), read
 * from the factory on Base Sepolia. Hard-coded so the address can be computed without RPC.
 */
export const SAFE_PROXY_CREATION_CODE: Hex =
  "0x608060405234801561001057600080fd5b506040516101e63803806101e68339818101604052602081101561003357600080fd5b8101908080519060200190929190505050600073ffffffffffffffffffffffffffffffffffffffff168173ffffffffffffffffffffffffffffffffffffffff1614156100ca576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004018080602001828103825260228152602001806101c46022913960400191505060405180910390fd5b806000806101000a81548173ffffffffffffffffffffffffffffffffffffffff021916908373ffffffffffffffffffffffffffffffffffffffff1602179055505060ab806101196000396000f3fe608060405273ffffffffffffffffffffffffffffffffffffffff600054167fa619486e0000000000000000000000000000000000000000000000000000000060003514156050578060005260206000f35b3660008037600080366000845af43d6000803e60008114156070573d6000fd5b3d6000f3fea264697066735822122003d1488ee65e08fa41e58e888a9865554c535f2c77126a82cb4c0f917f31441364736f6c63430007060033496e76616c69642073696e676c65746f6e20616464726573732070726f7669646564";

const setupAbi = [
  {
    type: "function",
    name: "setup",
    stateMutability: "nonpayable",
    inputs: [
      { name: "_owners", type: "address[]" },
      { name: "_threshold", type: "uint256" },
      { name: "to", type: "address" },
      { name: "data", type: "bytes" },
      { name: "fallbackHandler", type: "address" },
      { name: "paymentToken", type: "address" },
      { name: "payment", type: "uint256" },
      { name: "paymentReceiver", type: "address" },
    ],
    outputs: [],
  },
] as const;

const enableModulesAbi = [
  {
    type: "function",
    name: "enableModules",
    stateMutability: "nonpayable",
    inputs: [{ name: "modules", type: "address[]" }],
    outputs: [],
  },
] as const;

const createProxyWithNonceAbi = [
  {
    type: "function",
    name: "createProxyWithNonce",
    stateMutability: "nonpayable",
    inputs: [
      { name: "_singleton", type: "address" },
      { name: "initializer", type: "bytes" },
      { name: "saltNonce", type: "uint256" },
    ],
    outputs: [{ name: "proxy", type: "address" }],
  },
] as const;

const ZERO: Address = "0x0000000000000000000000000000000000000000";

/** The Safe `setup` call data for a fresh 1-owner account. */
export function safeInitializer(owner: Address): Hex {
  return encodeFunctionData({
    abi: setupAbi,
    functionName: "setup",
    args: [
      [owner],
      1n,
      SAFE.moduleSetup,
      encodeFunctionData({ abi: enableModulesAbi, functionName: "enableModules", args: [[SAFE.safe4337Module]] }),
      SAFE.safe4337Module,
      ZERO,
      0n,
      ZERO,
    ],
  });
}

/** `factoryData` of the first UserOp (`createProxyWithNonce(singleton, initializer, 0)`). */
export function safeFactoryData(owner: Address): Hex {
  return encodeFunctionData({
    abi: createProxyWithNonceAbi,
    functionName: "createProxyWithNonce",
    args: [SAFE.singleton, safeInitializer(owner), SAFE.saltNonce],
  });
}

/** Counterfactual account address of `owner` (CREATE2 by the SafeProxyFactory). */
export function predictAccountAddress(owner: Address): Address {
  const salt = keccak256(encodePacked(["bytes32", "uint256"], [keccak256(safeInitializer(owner)), SAFE.saltNonce]));
  const bytecode = concatHex([SAFE_PROXY_CREATION_CODE, encodeAbiParameters([{ type: "address" }], [SAFE.singleton])]);
  return getContractAddress({ opcode: "CREATE2", from: SAFE.proxyFactory, salt, bytecode });
}
