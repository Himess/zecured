/**
 * The smart-account layer, behind a small interface.
 *
 * Each loan has a fresh owner key (lib/keys.ts) controlling a 1-owner Safe (lib/safe.ts).
 * Every Base action is one ERC-4337 UserOp sponsored by our paymaster: the account never
 * holds ETH. The browser reaches the bundler and the paymaster only through our backend
 * (`/proxy/bundler`, `/proxy/paymaster`), never a provider directly.
 *
 * Whether the backend can sponsor at all comes from `GET /config` (`config.sponsorship`:
 * bundler, paymaster and allowlist configured); without it `send()` refuses before signing.
 * Verified end to end on a fork of Base Sepolia behind the backend proxy, with Pimlico's alto
 * bundler and mock paymaster (scripts/local-fork.sh): account deployment + authorizations +
 * borrow in the first UserOp, then a partial release, the account never holding ETH or zZEC.
 */
import { createClient, http, HttpRequestError, type Address, type Hex, type PublicClient, type Transport } from "viem";
import { createBundlerClient, createPaymasterClient } from "viem/account-abstraction";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";
import { toSafeSmartAccount } from "permissionless/accounts";
import { getUserOperationGasPrice } from "permissionless/actions/pimlico";
import { api as defaultApi, type Api } from "./api";
import { assertAllowlisted, type Call } from "./calls";
import { ContractsNotDeployedError, isDeployedAt, publicClient } from "./chain";
import { config as defaultConfig, type AppConfig } from "./config";
import { signCloseRequestAsSafe, type CloseRequestMessage } from "./eip712";
import { predictAccountAddress, SAFE } from "./safe";

export interface SentUserOp {
  userOpHash: Hex;
  /** Base transaction that included the UserOp. */
  transactionHash: Hex;
  success: boolean;
}

/** What the rest of the app may do with a loan's account. */
export interface LoanAccount {
  readonly owner: Address;
  /** Counterfactual Safe address (valid before deployment). */
  readonly address: Address;
  isDeployed(): Promise<boolean>;
  /**
   * ERC-1271 (Safe-wrapped) signature of a CloseRequest. Requires a deployed account:
   * the Adapter does not unwrap ERC-6492.
   */
  signCloseRequest(req: CloseRequestMessage): Promise<Hex>;
  /** Sends `calls` as one sponsored UserOp and waits for inclusion. */
  send(calls: readonly Call[]): Promise<SentUserOp>;
}

/** The backend's bundler / paymaster proxy cannot sponsor transactions right now. */
export class SponsorshipUnavailableError extends Error {
  constructor(reason: string) {
    super(`Sponsored transactions are not available right now (${reason}).`);
    this.name = "SponsorshipUnavailableError";
  }
}

/** Why a proxy request failed, in a few words (e.g. the backend's "bundler not configured"). */
function proxyFailure(e: unknown): string {
  if (e instanceof HttpRequestError) {
    const detail = typeof e.details === "string" ? e.details.replace(/^"|"$/g, "") : "";
    return detail && detail !== "undefined" ? detail : `HTTP ${e.status ?? "error"}`;
  }
  const short = (e as { shortMessage?: unknown }).shortMessage;
  return typeof short === "string" ? short : e instanceof Error ? e.message : "unreachable";
}

/** Fails fast, before anything is signed, when the bundler proxy is down or misconfigured. */
async function checkBundler(transport: Transport): Promise<void> {
  let entryPoints: readonly string[];
  try {
    entryPoints = (await createClient({ chain: baseSepolia, transport }).request({
      method: "eth_supportedEntryPoints",
    } as never)) as readonly string[];
  } catch (e) {
    throw new SponsorshipUnavailableError(`bundler: ${proxyFailure(e)}`);
  }
  if (!entryPoints.some((ep) => ep.toLowerCase() === SAFE.entryPoint.toLowerCase())) {
    throw new SponsorshipUnavailableError("the bundler does not support EntryPoint v0.7");
  }
}

export class AccountNotDeployedError extends Error {
  constructor() {
    super("The loan account is not deployed yet. It is deployed by its first transaction (your first borrow).");
    this.name = "AccountNotDeployedError";
  }
}

const USER_OP_TIMEOUT_MS = 120_000;

/** Overridable dependencies (tests); the app uses the defaults. */
export interface AccountDeps {
  config?: AppConfig;
  api?: Api;
  client?: PublicClient;
  /** Receipt polling interval (ms). */
  pollingInterval?: number;
}

export function loanAccount(ownerKey: Hex, deps: AccountDeps = {}): LoanAccount {
  const config = deps.config ?? defaultConfig;
  const api = deps.api ?? defaultApi;
  const owner = privateKeyToAccount(ownerKey);
  const address = predictAccountAddress(owner.address);
  const chain = () => ({ client: deps.client ?? publicClient(), config });

  return {
    owner: owner.address,
    address,

    isDeployed: () => isDeployedAt(address, chain()),

    async signCloseRequest(req) {
      // The Adapter's domain must be pinned and agree with the backend (H-6).
      if (!config.contractsDeployed) throw new ContractsNotDeployedError();
      if (!(await isDeployedAt(address, chain()))) throw new AccountNotDeployedError();
      return signCloseRequestAsSafe(ownerKey, config.adapter, address, req);
    },

    async send(calls) {
      if (!config.contractsDeployed) throw new ContractsNotDeployedError();
      // GET /config says whether the backend can sponsor at all: refuse before signing anything.
      if (!config.sponsorship) {
        throw new SponsorshipUnavailableError(
          config.remote === "loaded"
            ? "the backend has no bundler or paymaster configured"
            : "the backend's configuration could not be loaded",
        );
      }
      // Never ask the paymaster for anything outside the allowlist (INTERFACES § 4.1).
      assertAllowlisted(calls, address, config);

      const client = chain().client; // reads through /proxy/rpc
      const account = await toSafeSmartAccount({
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
        // Pinned: the default (true) yields a different account address (SPIKE_C.md).
        useMultiSendForSetup: SAFE.useMultiSendForSetup,
        // Precomputed; a mismatch with the initCode would make the EntryPoint reject (AA14).
        address,
      });

      // Wiring as implemented by backend/src/proxy.rs: the paymaster proxy speaks ERC-7677
      // (`pm_getPaymasterStubData` / `pm_getPaymasterData`; any sponsorship-policy id is the
      // proxy's business, so `paymasterContext` stays empty); the bundler proxy allows
      // `pimlico_getUserOperationGasPrice`; preVerificationGas (Base L1 data fee included)
      // comes from `eth_estimateUserOperationGas`, never hard-coded. permissionless encodes
      // calls as Safe4337Module `executeUserOpWithErrorString` (batches: DELEGATECALL to
      // MultiSendCallOnly), which the proxy's allowlist decoder accepts.
      const transportOptions = { fetchOptions: { credentials: "omit", referrerPolicy: "no-referrer" } } as const;
      const bundlerTransport = http(api.proxyUrl("bundler"), transportOptions);
      await checkBundler(http(api.proxyUrl("bundler"), { ...transportOptions, retryCount: 0 }));
      const paymaster = createPaymasterClient({ transport: http(api.proxyUrl("paymaster"), transportOptions) });
      const bundler = createBundlerClient({
        account,
        client,
        paymaster,
        transport: bundlerTransport,
        pollingInterval: deps.pollingInterval,
        userOperation: {
          // Pimlico's recommended UserOp fees; a bundler without that method falls back to the
          // RPC's fees. A failing proxy is reported as such, not hidden behind the fallback.
          async estimateFeesPerGas() {
            try {
              return (await getUserOperationGasPrice(createClient({ chain: baseSepolia, transport: bundlerTransport }))).fast;
            } catch (e) {
              if (e instanceof HttpRequestError) throw new SponsorshipUnavailableError(`bundler: ${proxyFailure(e)}`);
              return client.estimateFeesPerGas();
            }
          },
        },
      });

      const userOpHash = await bundler.sendUserOperation({
        calls: calls.map((c) => ({ to: c.to, data: c.data, value: 0n })),
      });
      const receipt = await bundler.waitForUserOperationReceipt({ hash: userOpHash, timeout: USER_OP_TIMEOUT_MS });
      return { userOpHash, transactionHash: receipt.receipt.transactionHash, success: receipt.success };
    },
  };
}
