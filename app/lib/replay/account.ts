/**
 * The replay's loan account (DEMO), in place of the Safe + sponsored UserOps of lib/account.ts.
 * `send` decodes the calls the app built (lib/calls.ts) with the app's ABIs and plays them as the
 * recorded run's matching step: the first borrow (with its authorizations, and its protection when
 * the run set one) returns the run's own transaction. Anything the run did not do there is refused
 * with a sentence saying what it did. Nothing is signed for real: the replay checks no signature.
 */
import { decodeFunctionData, type Abi, type Address, type Hex } from "viem";
import { adapterAbi, bondAbi, disputesAbi, erc20Abi, intakeAbi, morphoAbi, registryAbi } from "../abi";
import { adapterV4Abi, bondV4Abi } from "../abi4";
import type { LoanAccount, SentUserOp } from "../account";
import type { Call } from "../calls";
import { formatUsdc } from "../morpho";
import { buildModel } from "./model";
import { OffScriptError, world, type ReplayWorld } from "./world";

/** What the replay hands back where a Safe's ERC-1271 signature would be (never checked). */
export const REPLAY_SIGNATURE: Hex = `0x${"00".repeat(64)}1b`;

const same = (a: unknown, b: string): boolean => typeof a === "string" && a.toLowerCase() === b.toLowerCase();

/** Why the replay refuses a call the recorded run did not make here. */
const NOT_IN_THE_RUN: Record<string, string> = {
  requestRelease: "The recorded run left this loan by closing from collateral, not by a release: choose “Close from collateral”.",
  requestPartialRelease: "Withdrawing part of the ZEC is not part of the recorded run.",
  repay: "The recorded run did not repay with USDC: it closed from collateral.",
  transfer: "Sending USDC from the loan account is not part of the recorded run.",
  invalidateNonce: "The recorded run did not cancel its close request.",
  setRefundDestination: "Refunds are not part of the recorded run.",
  refundPending: "Refunds are not part of the recorded run.",
  disputePayout: "Disputes are not part of the recorded run.",
  reopenDisputedClaim: "Disputes are not part of the recorded run.",
  slashLiveness: "Payments by the signers' bonds are not part of the recorded run.",
  armLiveness: "Payments by the signers' bonds are not part of the recorded run.",
  setPreLiquidation: "The recorded run set no protection on its own: this replay can only borrow as it did.",
  clearPreLiquidation: "The recorded run set no protection: there is none to clear.",
  setAuthorization: "The recorded run authorized Zecured on Morpho in its first borrow.",
};

function decodeCall(w: ReplayWorld, call: Call): { functionName: string; args: readonly unknown[] } {
  const r = w.rec;
  const c = r.config.contracts;
  const abis: [string | undefined, readonly unknown[]][] = [
    [r.config.market.morphoBlue, morphoAbi],
    [r.config.registry, [...registryAbi, ...disputesAbi]],
    [r.config.adapter, [...adapterAbi, ...adapterV4Abi]],
    [r.config.market.loanToken, erc20Abi],
    [r.config.intake, intakeAbi],
    [c.SignerBond, [...bondAbi, ...bondV4Abi]],
  ];
  const abi = abis.find(([a]) => a !== undefined && same(call.to, a))?.[1];
  if (!abi) throw new OffScriptError("The recorded run sent nothing to that contract.");
  const d = decodeFunctionData({ abi: abi as Abi, data: call.data }) as { functionName: string; args?: readonly unknown[] };
  return { functionName: d.functionName, args: d.args ?? [] };
}

/** `loanAccount()` in a replay build: the recorded loan's account. */
export function replayLoanAccount(_ownerKey: Hex, w: ReplayWorld = world()): LoanAccount {
  const r = w.rec;
  const account = r.loan.account;
  return {
    owner: r.loan.owner,
    address: account,
    isDeployed: async () => buildModel(w).accountDeployed,
    signCloseRequest: async () => REPLAY_SIGNATURE,
    signDeleverageRequest: async () => REPLAY_SIGNATURE,

    async send(calls: readonly Call[]): Promise<SentUserOp> {
      if (calls.length === 0) throw new Error("nothing to send");
      const decoded = calls.map((c) => decodeCall(w, c));
      const borrow = decoded.find((d) => d.functionName === "borrow");
      if (!borrow) {
        const first = decoded[0]!;
        throw new OffScriptError(NOT_IN_THE_RUN[first.functionName] ?? "The recorded run did not do this.");
      }
      w.expect("borrowed");
      const [, assets, , onBehalf, receiver] = borrow.args as [unknown, bigint, bigint, Address, Address];
      const recorded = BigInt(r.loan.borrow.assets);
      if (assets !== recorded) {
        throw new OffScriptError(
          `This replay borrows exactly ${formatUsdc(recorded)} USDC, what the recorded run borrowed: enter ${formatUsdc(recorded)}.`,
        );
      }
      if (!same(onBehalf, account)) throw new OffScriptError("The recorded run borrowed on behalf of its own account.");
      const protect = decoded.find((d) => d.functionName === "setPreLiquidation");
      if (protect && !r.loan.borrow.protected) {
        throw new OffScriptError("The recorded run borrowed without protection: uncheck “Protect my loan” to replay it.");
      }
      for (const d of decoded) {
        if (d.functionName !== "borrow" && d.functionName !== "setAuthorization" && d.functionName !== "setPreLiquidation") {
          throw new OffScriptError(NOT_IN_THE_RUN[d.functionName] ?? "The recorded run did not do this.");
        }
      }
      const terms = protect?.args[0] as Record<string, bigint> | undefined;
      w.update({
        ...(same(receiver, account) ? {} : { borrowReceiver: receiver }),
        ...(terms
          ? {
              terms: {
                preLltv: String(terms.preLltv),
                preLcf1: String(terms.preLcf1),
                preLcf2: String(terms.preLcf2),
                preLif1: String(terms.preLif1),
                preLif2: String(terms.preLif2),
                expiry: String(terms.expiry),
              },
            }
          : {}),
      });
      // The bundler's round trip: estimation, the paymaster's signature, inclusion.
      await w.latency(2_500);
      w.complete("borrowed");
      const tx = w.step("borrowed").tx;
      return { userOpHash: tx.userOp ?? tx.base!, transactionHash: tx.base!, success: true };
    },
  };
}
