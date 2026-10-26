/**
 * The replay's browser wallet (DEMO), an EIP-1193 provider in place of `window.ethereum`: it is the
 * wallet the recorded run bought with, on Base Sepolia, and its two transactions are the recorded
 * swap's (the USDC approval, then `open`), matched by their decoded calldata. Lending on the market
 * page, or any other transaction, is refused: the run did not make it.
 */
import { decodeFunctionData, getAddress, type Abi, type Hex } from "viem";
import { erc20Abi } from "../abi";
import { swapAbi } from "../abi4";
import type { Eip1193Provider } from "../wallet";
import { quantity } from "./model";
import { OffScriptError, world, type ReplayWorld } from "./world";

/** What an EIP-1193 wallet throws (lib/wallet.ts reads `code` and `message`). */
class WalletRefusal extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
    this.name = "WalletRefusal";
  }
}

const same = (a: unknown, b: string): boolean => typeof a === "string" && a.toLowerCase() === b.toLowerCase();

export function replayWallet(w: ReplayWorld = world()): Eip1193Provider {
  const r = w.rec;
  const s = r.swap;
  const send = async (tx: { from?: string; to?: string; data?: Hex }): Promise<Hex> => {
    if (!same(tx.from, s.taker)) throw new WalletRefusal(4100, "This wallet only signs for its own account.");
    const step = w.step("swap-opened");
    try {
      if (same(tx.to, r.config.market.loanToken) && tx.data) {
        const d = decodeFunctionData({ abi: erc20Abi as Abi, data: tx.data }) as { functionName: string; args?: readonly unknown[] };
        const [spender, amount] = (d.args ?? []) as [string, bigint];
        if (d.functionName !== "approve" || !same(spender, s.swap) || amount !== BigInt(s.usdc)) {
          throw new OffScriptError("The recorded run approved exactly the order's USDC to ZecuredSwap, nothing else.");
        }
        w.expect("swap-opened");
        await w.latency(1_800);
        w.update({ approvedAt: w.now() });
        return step.tx.approve!;
      }
      if (same(tx.to, s.swap) && tx.data) {
        const d = decodeFunctionData({ abi: swapAbi as Abi, data: tx.data }) as { functionName: string; args?: readonly unknown[] };
        if (d.functionName !== "open") throw new OffScriptError("The recorded run did not refund an order.");
        const [quote] = (d.args ?? []) as [{ maker: string; usdc: bigint; zatoshi: bigint }];
        if (!same(quote.maker, s.maker) || quote.usdc !== BigInt(s.usdc) || quote.zatoshi !== BigInt(s.zatoshi)) {
          throw new OffScriptError("This is not the recorded order: ask for a new quote.");
        }
        if (!w.inputs.approvedAt) throw new WalletRefusal(-32000, "execution reverted: the USDC is not approved");
        await w.latency(1_800);
        w.complete("swap-opened");
        return step.tx.base!;
      }
      throw new OffScriptError("The recorded run's wallet sent nothing else: lending and other transactions are not part of the replay.");
    } catch (e) {
      if (e instanceof OffScriptError) throw new WalletRefusal(4200, `not part of the replay. ${e.message}`);
      throw e;
    }
  };
  return {
    async request({ method, params }) {
      switch (method) {
        case "eth_requestAccounts":
        case "eth_accounts":
          return [getAddress(s.taker)];
        case "eth_chainId":
          return quantity(r.network.chainId);
        case "wallet_switchEthereumChain":
        case "wallet_addEthereumChain":
          return null;
        case "eth_sendTransaction":
          return send((params?.[0] ?? {}) as { from?: string; to?: string; data?: Hex });
        default:
          throw new WalletRefusal(4200, `the replay's wallet does not support ${method}.`);
      }
    },
  };
}
