"use client";

import { useId, useState } from "react";
import type { Address, Hex } from "viem";
import { LoanTokenName } from "@/components/client";
import { IconShield } from "@/components/icons";
import { TokenAmount, usdc } from "@/components/loan";
import { MorphoMark, PairMarks, UsdcMark, ZecMark, ZzecMark } from "@/components/marks";
import { Badge, Button, Card, KeyValue, Meter, Notice, Skeleton, Stat } from "@/components/ui";
import { publicClient } from "@/lib/chain";
import { config, marketParams } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useAction, useProtocol } from "@/lib/hooks";
import { priceSourceName } from "@/lib/labels";
import { collateralValue, formatBps, formatUsdc, formatUsdPrice, liquidationIncentiveFactor, parseUsdc } from "@/lib/morpho";
import { formatIncentive, formatWadPercent } from "@/lib/preliq";
import { bondNumbers, capNumbers, marketNumbers, readSupply, SIGNERS, supplyCalls, withdrawCall, type WalletCall } from "@/lib/public";
import { COMPENSATION_SENTENCE } from "@/lib/v3";
import { formatZec } from "@/lib/zcash";
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
  const utilization = n ? Number((n.utilizationWad * 1000n) / 10n ** 18n) / 10 : 0;

  return (
    <div className="space-y-6">
      {p.error ? (
        <Notice tone="warn" title="Base could not be read" live>
          <p>{p.error}</p>
        </Notice>
      ) : null}
      {!config.contractsDeployed ? (
        <Notice tone="info" title="Nothing read from Base">
          <p>This build does not pin the deployment&apos;s contract addresses, so it reads nothing from Base.</p>
        </Notice>
      ) : null}

      <section aria-labelledby="market-h" className="panel panel-lead overflow-hidden">
        <div className="grid gap-6 p-[clamp(20px,2.6vw,32px)] lg:grid-cols-[minmax(0,1fr)_auto] lg:items-center">
          <div className="flex items-center gap-4">
            <PairMarks a={<ZzecMark size={46} />} b={<UsdcMark size={46} />} size={46} />
            <div>
              <h2 id="market-h" className="text-[26px] leading-tight font-extrabold font-stretch-118%">
                zZEC / <LoanTokenName />
              </h2>
              <p className="flex flex-wrap items-center gap-2 text-[13.5px] text-muted">
                <MorphoMark size={16} /> Morpho Blue · liquidation at {formatWadPercent(config.lltv)} LTV ·{" "}
                {formatIncentive(liquidationIncentiveFactor(config.lltv))} liquidation incentive
              </p>
            </div>
          </div>
          <div className="flex flex-wrap gap-2">
            {labels?.loanTokenIsMock ? <Badge tone="warn">MOCK loan token</Badge> : <Badge tone="neutral">Isolated market</Badge>}
            {labels?.oracleIsDemo ? <Badge tone="warn">DEMO price</Badge> : null}
          </div>
        </div>
        <dl className="grid border-t border-hairline sm:grid-cols-2 lg:grid-cols-4 [&>div]:border-hairline [&>div]:p-5 sm:[&>div]:border-r lg:[&>div:last-child]:border-r-0">
          <div>
            <dt className="text-[13px] font-semibold text-muted">Total supplied</dt>
            <dd className="mt-1">{n ? <TokenAmount token="USDC" value={usdc(n.suppliedUsdc)} unit={null} size="lg" /> : <Skeleton className="h-7 w-28" />}</dd>
          </div>
          <div>
            <dt className="text-[13px] font-semibold text-muted">Total borrowed</dt>
            <dd className="mt-1">{n ? <TokenAmount token="USDC" value={usdc(n.borrowedUsdc)} unit={null} size="lg" /> : <Skeleton className="h-7 w-28" />}</dd>
          </div>
          <div>
            <dt className="text-[13px] font-semibold text-muted">Borrow rate</dt>
            <dd className="mt-1 text-[22px] font-bold tabular-nums font-stretch-112%">{n ? formatWadPercent(n.borrowApyWad) : "—"}</dd>
            <dd className="text-[12px] text-muted">a year, compounded</dd>
          </div>
          <div>
            <dt className="text-[13px] font-semibold text-muted">Lenders earn</dt>
            <dd className="mt-1 text-[22px] font-bold text-ok tabular-nums font-stretch-112%">{n ? formatWadPercent(n.supplyApyWad) : "—"}</dd>
            <dd className="text-[12px] text-muted">the supply rate</dd>
          </div>
        </dl>
        <div className="border-t border-hairline px-[clamp(20px,2.6vw,32px)] pt-4 pb-2">
          <p className="flex justify-between text-[13px]">
            <span className="font-semibold text-muted">Utilization</span>
            <span className="tabular-nums">
              {n ? formatWadPercent(n.utilizationWad) : "—"}
              {n ? <span className="text-muted"> · {usdc(n.availableUsdc)} available to borrow or withdraw</span> : null}
            </span>
          </p>
          <Meter value={utilization} max={100} tone="info" markers={[{ at: 90, label: "90 % target", tone: "muted" }]} />
        </div>
      </section>

      <div className="grid gap-6 lg:grid-cols-2">
        <Card title="The collateral behind it" icon={<ZecMark size={20} />}>
          {s && cap ? (
            <div className="space-y-4">
              <div className="flex flex-wrap items-end justify-between gap-3">
                <TokenAmount token="TAZ" value={formatZec(cap.lockedZat)} size="xl" />
                <p className="text-[13.5px] text-muted">in {s.openVaults.toString()} open vault{s.openVaults === 1n ? "" : "s"}</p>
              </div>
              <KeyValue
                rows={[
                  ["Its value", `${formatUsdc(collateralValue(cap.lockedZat, s.price))} USDC at ${formatUsdPrice(s.price)} per ZEC`],
                  ["Price source", priceSourceName()],
                  ["Coverage cap used", cap.fillBps === null ? "—" : `${formatBps(cap.fillBps)} (pending deposits count too)`],
                ]}
              />
            </div>
          ) : (
            <p className="text-sm text-muted">—</p>
          )}
          <p className="mt-4 text-xs text-muted">
            The ZEC stays in shielded vaults on Zcash, controlled by five signers (three must sign each payout). Morpho holds only
            zZEC, a token that exists while the ZEC is locked.
          </p>
        </Card>

        <Card title="If a vault's ZEC is ever taken" icon={<IconShield size={18} />}>
          <div className="space-y-4">
            <p className="text-sm text-soft">{COMPENSATION_SENTENCE}</p>
            {bonds ? (
              <dl className="grid gap-3 sm:grid-cols-3">
                <Stat label="Signers' bonds" value={`${formatUsdc(bonds.sum)}`} hint={`USDC · ${bonds.bonded} of ${SIGNERS} at the minimum`} />
                <Stat label="Counted (B_eff)" value={`${formatUsdc(bonds.effective)}`} hint="USDC · 5/3 × the three smallest" />
                <Stat
                  label="Per dollar locked"
                  value={bonds.perCollateralWad === null ? "—" : formatWadPercent(bonds.perCollateralWad)}
                  hint={s ? `kept at ${formatBps(BigInt(s.coverageBps))} or more` : undefined}
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
      </div>

      {config.contractsDeployed ? <LendCard available={n?.availableUsdc ?? null} onChanged={() => void p.refresh()} /> : null}
    </div>
  );
}
