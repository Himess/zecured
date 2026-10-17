/**
 * The user's own wallet, through EIP-1193 (`window.ethereum`), for what only the user's own address
 * can do: open a swap order (and approve its USDC), refund it, lend on the market page. The wallet
 * is asked for accounts, the chain, a chain switch and transactions only: every read, receipts
 * included, goes through our backend's proxy (lib/chain.ts), never the wallet's provider. Unlike
 * a loan account, this address pays its own gas and is the user's: it appears on Base.
 */
import { getAddress, numberToHex, type Address, type Hex } from "viem";

export interface Eip1193Provider {
  request(args: { method: string; params?: readonly unknown[] }): Promise<unknown>;
}

export class WalletError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WalletError";
  }
}

/** The injected wallet, if the browser has one. */
export function injectedWallet(): Eip1193Provider | null {
  if (typeof window === "undefined") return null;
  const w = (window as unknown as { ethereum?: Eip1193Provider }).ethereum;
  return w && typeof w.request === "function" ? w : null;
}

/** EIP-1193's "user rejected the request" (4001), in plain words. */
function walletFailure(e: unknown): WalletError {
  const code = (e as { code?: unknown })?.code;
  if (code === 4001) return new WalletError("You declined the request in your wallet.");
  const message = (e as { message?: unknown })?.message;
  return new WalletError(typeof message === "string" && message ? `Your wallet refused: ${message}` : "Your wallet refused the request.");
}

/** Asks the wallet for its account (the first one it exposes). */
export async function connectWallet(w: Eip1193Provider): Promise<Address> {
  let accounts: unknown;
  try {
    accounts = await w.request({ method: "eth_requestAccounts" });
  } catch (e) {
    throw walletFailure(e);
  }
  const first = Array.isArray(accounts) ? accounts[0] : undefined;
  if (typeof first !== "string") throw new WalletError("Your wallet exposed no account.");
  return getAddress(first);
}

/** Makes sure the wallet is on `chainId`, asking it to switch if not. */
export async function ensureChain(w: Eip1193Provider, chainId: number, chainName: string): Promise<void> {
  let current: unknown;
  try {
    current = await w.request({ method: "eth_chainId" });
  } catch (e) {
    throw walletFailure(e);
  }
  if (typeof current === "string" && BigInt(current) === BigInt(chainId)) return;
  try {
    await w.request({ method: "wallet_switchEthereumChain", params: [{ chainId: numberToHex(chainId) }] });
  } catch (e) {
    const code = (e as { code?: unknown })?.code;
    if (code === 4902) throw new WalletError(`Your wallet does not know ${chainName} yet: add it in the wallet, then try again.`);
    throw walletFailure(e);
  }
}

/** Sends one transaction from `from` (no ETH value) and returns its hash. */
export async function sendFromWallet(w: Eip1193Provider, tx: { from: Address; to: Address; data: Hex }): Promise<Hex> {
  let hash: unknown;
  try {
    hash = await w.request({ method: "eth_sendTransaction", params: [{ from: tx.from, to: tx.to, data: tx.data, value: "0x0" }] });
  } catch (e) {
    throw walletFailure(e);
  }
  if (typeof hash !== "string" || !/^0x[0-9a-fA-F]{64}$/.test(hash)) throw new WalletError("Your wallet returned no transaction hash.");
  return hash.toLowerCase() as Hex;
}
