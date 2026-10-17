"use client";

import { useId, useState } from "react";
import type { Address, Hex } from "viem";
import { LoanTokenName } from "@/components/client";
import { Badge, Button, Card, KeyValue, Notice, Stat } from "@/components/ui";
import { publicClient } from "@/lib/chain";
import { config, marketParams } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useAction, useProtocol } from "@/lib/hooks";
import { priceSourceName } from "@/lib/labels";
import { collateralValue, formatBps, formatUsdc, formatUsdPrice, liquidationIncentiveFactor, parseUsdc } from "@/lib/morpho";
import { formatIncentive, formatWadPercent } from "@/lib/preliq";
import { bondNumbers, capNumbers, marketNumbers, readSupply, SIGNERS, supplyCalls, withdrawCall, type WalletCall } from "@/lib/public";
import { COMPENSATION_SENTENCE } from "@/lib/v3";
import { formatTaz } from "@/lib/zcash";
import { connectWallet, ensureChain, injectedWallet, sendFromWallet } from "@/lib/wallet";

/** Sends each call from the wallet in turn, waiting for its receipt through the proxy. */
async function sendAll(wallet: Address, calls: WalletCall[], onStage: (s: string) => void): Promise<Hex> {
  const w = injectedWallet();
  if (!w) throw new Error("No wallet found in this browser.");
  await ensureChain(w, config.chainId, DEPLOYMENT.chainName);
  let last: Hex = "0x";
  for (const [i, call] of calls.entries()) {
    onStage(calls.length > 1 ? `Confirm transaction ${i + 1} of ${calls.length} in your wallet…` : "Confirm the transaction in your wallet…");
    last = await sendFromWallet(w, { from: wallet, to: call.to, data: call.data });
    onStage("Waiting for Base to include it…");
    const receipt = await publicClient().waitForTransactionReceipt({ hash: last, timeout: 180_000, pollingInterval: 3_000 });
    if (receipt.status !== "success") throw new Error("The transaction reverted on Base; nothing was lent or withdrawn by it.");
  }
  return last;
}

/**
 * Optional lending from the user's own wallet (EIP-1193): supply the loan token to the Morpho
 * market, or withdraw it. Reads go through our proxy; only the transactions go through the
 * wallet. The wallet's address is public on Base, and nothing links it to any loan.
 */
function LendCard({ available, onChanged }: { available: bigint | null; onChanged: () => void }) {
  const amountId = useId();
  const [wallet, setWallet] = useState<Address | null>(null);
  const [supply, setSupply] = useState<Awaited<ReturnType<typeof readSupply>> | null>(null);
  const [amount, setAmount] = useState("");
  const [stage, setStage] = useState<string | null>(null);
  const refresh = async (a: Address) => setSupply(await readSupply(a));
  const connect = useAction(async () => {
    const w = injectedWallet();
    if (!w) throw new Error("No wallet found in this browser. Lending needs a wallet of your own, with the loan token and gas.");
    const a = await connectWallet(w);
    await ensureChain(w, config.chainId, DEPLOYMENT.chainName);
    setWallet(a);
    await refresh(a);
    return a;
  });
  const params = marketParams(config);
  const lend = useAction(async () => {
    if (!wallet || !supply) throw new Error("Connect your wallet first.");
    const assets = parseUsdc(amount);
    if (assets > supply.balance) throw new Error(`Your wallet holds ${formatUsdc(supply.balance)} only.`);
    const calls = supplyCalls({ morpho: config.morpho, usdc: config.usdc, params, wallet, assets, allowance: supply.allowance });
    try {
      return await sendAll(wallet, calls, setStage);
    } finally {
      setStage(null);
      await refresh(wallet).catch(() => undefined);
      onChanged();
    }
  });
  const withdraw = useAction(async (all: boolean) => {
    if (!wallet || !supply) throw new Error("Connect your wallet first.");
    const call = all
      ? withdrawCall({ morpho: config.morpho, params, wallet, shares: supply.shares })
      : withdrawCall({ morpho: config.morpho, params, wallet, assets: parseUsdc(amount) });
    try {
      return await sendAll(wallet, [call], setStage);
    } finally {
      setStage(null);
      await refresh(wallet).catch(() => undefined);
      onChanged();
    }
  });
  const busy = lend.busy || withdraw.busy;

  return (
    <Card title="Lend (optional)" action={<Badge tone="neutral">Your own wallet</Badge>}>
      <div className="space-y-4">
        <p className="text-sm text-muted">
          Lenders supply <LoanTokenName /> to the Morpho market that loans borrow from, and earn the supply rate. Lending is an
          ordinary Morpho position of your own wallet: its address is public on Base, and nothing links it to any loan. It pays
          its own gas.
        </p>
        {DEPLOYMENT.kind === "mainnet-beta" ? (
          <Notice tone="warn" title="Mainnet beta">
            <p>Unaudited, invite only, small caps. Lend only what you can afford to lose.</p>
          </Notice>
        ) : (
          <p className="text-xs text-muted">Testnet funds only: test USDC on Base Sepolia, which has no value.</p>
        )}
        {!wallet ? (
          <Button variant="secondary" onClick={() => void connect.run()} busy={connect.busy}>
            Connect a wallet
          </Button>
        ) : (
          <div className="space-y-4">
            <KeyValue
              rows={[
                ["Wallet", wallet],
                ["In your wallet", supply ? `${formatUsdc(supply.balance)} USDC` : "—"],
                ["You lend", supply ? `${formatUsdc(supply.assets)} USDC` : "—"],
                ["Withdrawable now", available === null ? "—" : `up to ${formatUsdc(available)} USDC (what borrowers do not use)`],
              ]}
            />
            <div className="space-y-2">
              <label htmlFor={amountId} className="block text-sm font-medium">
                Amount (USDC)
              </label>
              <input
                id={amountId}
                inputMode="decimal"
                autoComplete="off"
                value={amount}
                onChange={(e) => setAmount(e.target.value)}
                placeholder="25.00"
                className="w-full max-w-xs rounded-lg border border-border bg-surface-2 px-3 py-2 tabular-nums"
              />
            </div>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => void lend.run()} busy={lend.busy} disabled={busy || !amount.trim()}>
                Lend
              </Button>
              <Button variant="secondary" onClick={() => void withdraw.run(false)} busy={withdraw.busy} disabled={busy || !amount.trim()}>
                Withdraw this amount
              </Button>
              <Button variant="ghost" onClick={() => void withdraw.run(true)} disabled={busy || !supply || supply.shares === 0n}>
                Withdraw everything
              </Button>
            </div>
            {stage ? (
              <p className="text-sm text-muted" aria-live="polite">
                {stage}
              </p>
            ) : null}
          </div>
        )}
        {connect.error ? (
          <Notice tone="danger" title="Not connected" live>
            <p>{connect.error}</p>
          </Notice>
        ) : null}
        {lend.error ? (
          <Notice tone="danger" title="Nothing was lent" live>
            <p>{lend.error}</p>
          </Notice>
        ) : null}
        {withdraw.error ? (
          <Notice tone="danger" title="Nothing was withdrawn" live>
            <p>{withdraw.error}</p>
          </Notice>
        ) : null}
        {lend.result ? (
          <Notice tone="ok" title="Lent" live>
            <p>Your supply is on Base (transaction {lend.result}).</p>
          </Notice>
        ) : null}
        {withdraw.result ? (
          <Notice tone="ok" title="Withdrawn" live>
            <p>The USDC is back in your wallet (transaction {withdraw.result}).</p>
          </Notice>
        ) : null}
      </div>
    </Card>
  );
}

/**
 * P-4, the market page: the lending market's size, use and rates, the collateral behind it, and the
 * signers' bonds that compensate a theft (lenders before borrowers), from Base through the proxy.
 */
export function MarketView() {
  const p = useProtocol(60_000);
  const s = p.data;
  const n = s ? marketNumbers(s.market, s.borrowRate) : null;
  const cap = s ? capNumbers(s) : null;
  const bonds = s?.bonds ? bondNumbers(s.bonds, s.lockedZat, s.price) : null;
  const labels = config.marketLabels;

  return (
    <div className="space-y-6">
      {p.error ? (
        <Notice tone="warn" title="Base could not be read" live>
          <p>{p.error}</p>
        </Notice>
      ) : null}
      {p.loading && !s ? <p className="text-sm text-muted">Reading the market on Base…</p> : null}
      {!config.contractsDeployed ? (
        <Notice tone="info" title="Nothing read from Base">
          <p>This build does not pin the deployment&apos;s contract addresses, so it reads nothing from Base.</p>
        </Notice>
      ) : null}

      <Card
        title="The lending market"
        action={labels?.loanTokenIsMock ? <Badge tone="warn">MOCK loan token</Badge> : <Badge tone="neutral">Morpho Blue</Badge>}
      >
        {n ? (
          <dl className="grid gap-3 sm:grid-cols-3">
            <Stat label="Supplied" value={`${formatUsdc(n.suppliedUsdc)} USDC`} hint="by lenders" />
            <Stat label="Borrowed" value={`${formatUsdc(n.borrowedUsdc)} USDC`} hint="against shielded ZEC" />
            <Stat label="Utilization" value={formatWadPercent(n.utilizationWad)} hint={`${formatUsdc(n.availableUsdc)} USDC available`} />
            <Stat label="Borrow rate" value={formatWadPercent(n.borrowApyWad)} hint="a year, compounded" />
            <Stat label="Supply rate" value={formatWadPercent(n.supplyApyWad)} hint="what lenders earn" />
            <Stat label="Liquidation LTV" value={formatWadPercent(config.lltv)} hint={`${formatIncentive(liquidationIncentiveFactor(config.lltv))} incentive`} />
          </dl>
        ) : (
          <p className="text-sm text-muted">—</p>
        )}
      </Card>

      <Card title="The collateral" action={labels?.oracleIsDemo ? <Badge tone="warn">DEMO price</Badge> : null}>
        {s && cap ? (
          <KeyValue
            rows={[
              ["Shielded ZEC locked", `${formatTaz(cap.lockedZat)} in ${s.openVaults.toString()} open vault(s)`],
              ["Its value", `${formatUsdc(collateralValue(cap.lockedZat, s.price))} USDC at ${formatUsdPrice(s.price)} per ZEC`],
              ["Price source", priceSourceName()],
              ["Coverage cap used", cap.fillBps === null ? "—" : `${formatBps(cap.fillBps)} (pending deposits count too)`],
            ]}
          />
        ) : (
          <p className="text-sm text-muted">—</p>
        )}
        <p className="mt-3 text-xs text-muted">
          The ZEC stays in shielded vaults on Zcash, controlled by five signers (three must sign each payout). Morpho holds only
          zZEC, a token that exists while the ZEC is locked.
        </p>
      </Card>

      <Card title="If a vault's ZEC is ever taken">
        <div className="space-y-4">
          <p className="text-sm">{COMPENSATION_SENTENCE}</p>
          {bonds ? (
            <dl className="grid gap-3 sm:grid-cols-3">
              <Stat label="Signers' bonds (ΣB)" value={`${formatUsdc(bonds.sum)} USDC`} hint={`${bonds.bonded} of ${SIGNERS} signers at the minimum`} />
              <Stat label="Counted for coverage (B_eff)" value={`${formatUsdc(bonds.effective)} USDC`} hint="min(ΣB, 5/3 × the three smallest)" />
              <Stat
                label="Bonds per dollar locked"
                value={bonds.perCollateralWad === null ? "—" : formatWadPercent(bonds.perCollateralWad)}
                hint={s ? `the cap keeps it at ${formatBps(BigInt(s.coverageBps))} or more` : undefined}
              />
            </dl>
          ) : (
            <p className="text-sm text-muted">
              {!config.contractsDeployed ? "—" : p.loading ? "Reading the bonds…" : "No signer bond on this deployment."}
            </p>
          )}
          <p className="text-xs text-muted">
            For lenders: in a compensation, the market&apos;s loss is paid before any borrower&apos;s own equity, which comes last.
          </p>
        </div>
      </Card>

      {config.contractsDeployed ? <LendCard available={n?.availableUsdc ?? null} onChanged={() => void p.refresh()} /> : null}
    </div>
  );
}
