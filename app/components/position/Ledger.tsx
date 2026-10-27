"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import type { Hex } from "viem";
import { CopyButton } from "@/components/client";
import { DestinationStep } from "@/components/DestinationForm";
import { ZecMark } from "@/components/marks";
import { PayoutNotice } from "@/components/PayoutNotice";
import { ProofSeal, StepMark, type MarkState } from "@/components/seam";
import { TxResult } from "@/components/TxResult";
import { Badge, Button, Card, cx, KeyValue, Mono, Notice } from "@/components/ui";
import { loanAccount, type SentUserOp } from "@/lib/account";
import { api, describeError, type Vault } from "@/lib/api";
import {
  armLivenessCalls,
  describeRevert,
  disputePayoutCalls,
  formatUtc,
  refundDestinationCalls,
  refundPendingCalls,
  reopenDisputedClaimCalls,
  slashLivenessCalls,
  type Call,
} from "@/lib/calls";
import { readCompensation, type Compensation, type LedgerClaim, type VaultLedger } from "@/lib/chain";
import { config } from "@/lib/config";
import { notifyLoansChanged, useAction, useRefundReason } from "@/lib/hooks";
import { browserStorage, loanStore, short, type StoredLoan } from "@/lib/keys";
import { loanTokenName } from "@/lib/labels";
import { formatUsdc } from "@/lib/morpho";
import { armedNote, armingState, checkReceipt, rememberArming, rememberedArmings, wasArmedHere } from "@/lib/receipt";
import {
  bondPayment,
  bondPaymentExplainer,
  claimView,
  COMPENSATION_SENTENCE,
  compensatedClaimIds,
  disputeState,
  formatPeriod,
  needsRefundDestination,
  pendingState,
  REFUND_REASON_TEXT,
  refundClaims,
} from "@/lib/v3";
import { blocksToDuration, formatTaz } from "@/lib/zcash";

const now = () => Math.floor(Date.now() / 1000);

/** One sponsored UserOp; a revert is explained in plain words when the app knows it. */
async function send(loan: StoredLoan, calls: Call[]): Promise<SentUserOp> {
  try {
    return await loanAccount(loan.ownerKey).send(calls);
  } catch (e) {
    const why = describeRevert(e);
    if (why) throw new Error(why);
    throw e;
  }
}

/** The browser's key controls the account that owns the vault's claims. */
function ownsVault(loan: StoredLoan | null, ledger: VaultLedger): loan is StoredLoan {
  return Boolean(loan) && loan!.account.toLowerCase() === ledger.account.toLowerCase();
}

// ------------------------------------------------------------------ K3 / T-03: a pending activation

/**
 * The deposit is proven on Base and waits to be activated (`ActivationPending`): when it is
 * ready, why it may wait longer, and, after a long pause or a halt, turning it into a refund.
 */
export function PendingActivationCard({
  ledger,
  loan,
  onChanged,
}: {
  ledger: VaultLedger;
  loan: StoredLoan | null;
  onChanged: () => void;
}) {
  const p = ledger.pending!;
  const t = now();
  const s = pendingState(p, ledger.links.pendingRefundDelay, t);
  const blocksLeft = p.readyHeight > p.relayTip ? Number(p.readyHeight - p.relayTip) : 0;
  const mine = ownsVault(loan, ledger);
  const refund = useAction(async () => {
    const r = await send(loan!, refundPendingCalls({ vaultId: ledger.vaultId }));
    onChanged();
    return r;
  });
  return (
    <Card title="Your deposit is proven and waits to be activated" action={<Badge tone="info">Pending activation</Badge>}>
      <div className="space-y-4">
        <p className="text-sm text-muted">
          A zero-knowledge proof of your deposit is on Base and reserves this vault. Once the Zcash header relay has seen a
          few more blocks and a short delay has passed, anyone can activate it, and the backend&apos;s keeper does: zZEC is
          then minted into your loan account&apos;s Morpho position.
        </p>
        <KeyValue
          rows={[
            ["Deposit", formatTaz(p.amount)],
            [
              "Ready at Zcash height",
              <span key="h">
                #{p.readyHeight.toLocaleString("en-US")}{" "}
                <span className="text-muted">
                  (relay at #{p.relayTip.toLocaleString("en-US")}
                  {blocksLeft > 0 ? `, ${blocksLeft} blocks to go, ${blocksToDuration(blocksLeft)}` : ", reached"})
                </span>
              </span>,
            ],
            ["Not before", `${formatUtc(p.readyTime)}${t >= p.readyTime ? " (passed)" : ""}`],
          ]}
        />
        {s.dropped ? (
          <Notice tone="warn" title="The proof's Zcash block left the relay's best chain">
            <p>
              Zcash reorganized past the block the proof was anchored to. Nothing was minted: the pending activation is
              dropped and the deposit is proven again against the chain that won.
            </p>
          </Notice>
        ) : s.waits.length > 0 ? (
          <Notice tone="info" title={s.ready ? "Ready, but it waits" : "It will also wait"}>
            <ul className="list-disc space-y-1 pl-5">
              {s.waits.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          </Notice>
        ) : s.ready ? (
          <Notice tone="ok" title="Ready to be activated">
            <p>Anyone can activate it now; the keeper does so within minutes.</p>
          </Notice>
        ) : null}
        {s.refundable && mine ? (
          <div className="space-y-3 border-t border-border pt-4">
            <h3 className="font-semibold">Turn it into a refund instead</h3>
            <p className="text-sm text-muted">
              {p.halted
                ? "Minting has stopped for good, so this deposit will never back a loan."
                : `New minting has been paused for more than ${formatPeriod(ledger.links.pendingRefundDelay)}.`}{" "}
              You can give up the loan: the deposit becomes a Refund claim, which comes back to an Ironwood address you give,
              minus the payout&apos;s network fee.
            </p>
            <Button variant="secondary" onClick={() => refund.run()} busy={refund.busy}>
              Turn my deposit into a refund (sponsored)
            </Button>
            {refund.error ? (
              <Notice tone="danger" title="Not turned into a refund" live>
                <p>{refund.error}</p>
              </Notice>
            ) : null}
            {refund.result ? <TxResult result={refund.result} what="Refund requested" /> : null}
          </div>
        ) : s.refundableAt !== null && mine ? (
          <p className="text-xs text-muted">
            If the pause still runs at {formatUtc(s.refundableAt)}, you can turn this deposit into a refund instead of
            waiting.
          </p>
        ) : null}
      </div>
    </Card>
  );
}

// ------------------------------------------------------------------ R-12: pending refunds

/**
 * Deposits proven as refunds whose claims have not opened yet (R-12): each waits a few Zcash blocks,
 * like an activation, before anyone (the backend's keeper) opens its Refund claim; only then can it
 * take a destination.
 */
export function PendingRefundsCard({ ledger }: { ledger: VaultLedger }) {
  const list = ledger.pendingRefunds ?? [];
  if (list.length === 0) return null;
  const t = now();
  return (
    <Card
      title={list.length === 1 ? "A deposit was proven as a refund" : `${list.length} deposits were proven as refunds`}
      action={<Badge tone="info">Pending refund</Badge>}
    >
      <p className="mb-4 text-sm text-muted">
        The proof is on Base. Like an activation, it waits until the Zcash header relay has seen a few more blocks and a
        short delay has passed; then its Refund claim opens (the backend&apos;s keeper does it) and you give it an Ironwood
        destination here. Until then it is pending.
      </p>
      <ul className="space-y-3">
        {list.map((r) => (
          <li key={r.depositTag} className="rounded-2xl bg-bg p-4">
            <KeyValue
              rows={[
                ["Deposit", formatTaz(r.amount)],
                ["Why", r.reason ? REFUND_REASON_TEXT[r.reason] : "Not readable from Base."],
                ["Claim opens from Zcash height", `#${r.readyHeight.toLocaleString("en-US")}`],
                ["Not before", `${formatUtc(r.readyTime)}${t >= r.readyTime ? " (passed)" : ""}`],
              ]}
            />
          </li>
        ))}
      </ul>
    </Card>
  );
}

// ------------------------------------------------------------------ T-02: refunds

/** Every Refund claim of the vault: why it landed, and its destination (given, needed, or revived). */
export function RefundsCard({
  ledger,
  loan,
  onChanged,
}: {
  ledger: VaultLedger;
  loan: StoredLoan | null;
  onChanged: () => void;
}) {
  const refunds = refundClaims(ledger);
  if (refunds.length === 0) return null;
  return (
    <Card title={refunds.length === 1 ? "A deposit came back as a refund" : `${refunds.length} deposits came back as refunds`}>
      <p className="mb-4 text-sm text-muted">
        A deposit that cannot back a loan is still proven on Base, as a Refund claim: you give it an Ironwood destination,
        and the signers pay it like any payout, through a commitment on Base that makes taking it provable.
      </p>
      <ul className="space-y-4">
        {refunds.map((c) => (
          <RefundItem key={c.claimId} ledger={ledger} claim={c} loan={loan} onChanged={onChanged} />
        ))}
      </ul>
    </Card>
  );
}

function RefundItem({
  ledger,
  claim,
  loan,
  onChanged,
}: {
  ledger: VaultLedger;
  claim: LedgerClaim;
  loan: StoredLoan | null;
  onChanged: () => void;
}) {
  const view = claimView(claim, {
    refundAbandonPeriod: ledger.links.refundAbandonPeriod,
    nowSeconds: now(),
    disputeWindow: ledger.links.disputeWindow,
    refundDormancy: ledger.links.refundDormancy,
  });
  const needs = needsRefundDestination(claim);
  // The creation time is the claim's `createdAt` only until a destination is given.
  const reason = useRefundReason(ledger.vaultId, claim.claimId, claim.createdAt, needs && claim.status === "Pending");
  const [commitment, setCommitment] = useState<Hex | null>(null);
  const mine = ownsVault(loan, ledger);
  const setDestination = useAction(async (dest: Hex) => {
    const r = await send(loan!, refundDestinationCalls({ claimId: claim.claimId, commitment: dest }));
    if (r.success) {
      // This destination is used: the next payout asks for a fresh one.
      loanStore().update(loan!.id, { destinationCommitment: undefined });
      notifyLoansChanged();
      setCommitment(null);
    }
    onChanged();
    return r;
  });
  return (
    <li className="rounded-2xl bg-bg p-4">
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
        <span className="font-medium">
          {formatTaz(claim.amount)}
          <span className="text-sm text-muted">, claim {short(claim.claimId, 10, 6)}</span>
        </span>
        <Badge tone={view.badge.tone}>{view.badge.text}</Badge>
      </div>
      {needs && claim.status === "Pending" ? (
        <p className="text-sm text-muted">
          {reason === undefined
            ? "Looking up why it landed as a refund…"
            : reason === null
              ? "Why it landed as a refund could not be read from Base."
              : `Why: ${REFUND_REASON_TEXT[reason]}`}
        </p>
      ) : null}
      <p className="text-sm text-muted">{view.detail}</p>
      {needs && mine ? (
        <ol className="mt-4 space-y-4">
          <li className="space-y-3">
            <h3 className="font-semibold">1. Where should it go?</h3>
            {commitment ? (
              <Notice tone="ok" title="Destination validated by the signers">
                <p>
                  Commitment <Mono>{short(commitment, 10, 8)}</Mono>.{" "}
                  <button type="button" className="underline" onClick={() => setCommitment(null)}>
                    Change
                  </button>
                </p>
              </Notice>
            ) : (
              <DestinationStep loan={loan} vaultId={ledger.vaultId} onDone={setCommitment} />
            )}
          </li>
          <li className="space-y-3">
            <h3 className={cx("font-semibold", !commitment && "text-faint")}>
              2. {claim.status === "WrittenOff" ? "Revive the refund" : "Set it on Base"}
            </h3>
            <Button onClick={() => commitment && setDestination.run(commitment)} busy={setDestination.busy} disabled={!commitment}>
              {claim.status === "WrittenOff" ? "Revive with this destination (sponsored)" : "Set the refund's destination (sponsored)"}
            </Button>
            {setDestination.error ? (
              <Notice tone="danger" title="The destination was not set" live>
                <p>{setDestination.error}</p>
              </Notice>
            ) : null}
            {setDestination.result ? <TxResult result={setDestination.result} what="Refund destination set" /> : null}
          </li>
        </ol>
      ) : needs && !loan ? (
        <p className="mt-2 text-sm text-muted">
          The destination is given with the loan&apos;s owner key. <Link href="/loans">Import its backup</Link> to do it here.
        </p>
      ) : null}
      {claim.status === "Pending" && !needs && !claim.bondPaid ? (
        <div className="mt-3">
          <PayoutNotice />
        </div>
      ) : null}
    </li>
  );
}

// ------------------------------------------------------------------ claims, with disputes (T-05 (a))

/** Every claim of the vault as Base keeps it (v3 kinds and statuses), and what its account can do about each. */
export function ClaimsCard({
  ledger,
  loan,
  vault,
  onChanged,
}: {
  ledger: VaultLedger;
  loan: StoredLoan | null;
  vault: Vault | null;
  onChanged: () => void;
}) {
  if (ledger.claims.length === 0) return null;
  // Yours first, then the market maker's, then the fee and the rest, each kind in its order on Base.
  const rank = (k: LedgerClaim["kind"]) => ["Release", "CloseUser", "Refund", "CloseMarketMaker", "Liquidation", "Fee"].indexOf(k);
  const claims = [...ledger.claims].sort((a, b) => (rank(a.kind) + 7) % 7 - ((rank(b.kind) + 7) % 7) || a.sequence - b.sequence);
  const committed = claims.some((c) => c.committed || c.status === "Paid");
  return (
    <Card
      title="Payouts on Zcash"
      icon={<ZecMark size={20} />}
      action={committed ? <span className="text-[12.5px] text-muted">paid by 3 of 5 signers · final once proven, or once undisputed</span> : null}
    >
      <ul className="divide-y divide-hairline text-sm">
        {claims.map((c) => (
          <ClaimRow key={c.claimId} ledger={ledger} claim={c} loan={loan} onChanged={onChanged} />
        ))}
      </ul>
      {vault?.payoutInFlight ? <p className="mt-2 text-xs text-muted">A payout transaction is being confirmed on Zcash.</p> : null}
      {claims.some((c) => c.status === "Paid" && c.paidAt === 0) ? (
        <p className="mt-3 text-xs text-muted">Final once proven on Base in zero knowledge, or once its dispute window closes undisputed.</p>
      ) : null}
      {claims.some((c) => c.kind === "Release" || c.kind === "CloseUser") ? (
        <div className="mt-4">
          <PayoutNotice />
        </div>
      ) : null}
    </Card>
  );
}

function ClaimRow({
  ledger,
  claim,
  loan,
  onChanged,
}: {
  ledger: VaultLedger;
  claim: LedgerClaim;
  loan: StoredLoan | null;
  onChanged: () => void;
}) {
  const t = now();
  const view = claimView(claim, {
    refundAbandonPeriod: ledger.links.refundAbandonPeriod,
    nowSeconds: t,
    disputeWindow: ledger.links.disputeWindow,
    refundDormancy: ledger.links.refundDormancy,
  });
  const mine = ownsVault(loan, ledger);
  // The vault's account is the claimant of its owner's claims (Release, CloseUser, Refund).
  const d = disputeState(claim, {
    account: ledger.account,
    disputeWindow: ledger.links.disputeWindow,
    compromised: ledger.compromised,
    nowSeconds: t,
  });
  const [confirming, setConfirming] = useState(false);
  const dispute = useAction(async () => {
    const r = await send(loan!, disputePayoutCalls({ claimId: claim.claimId }));
    setConfirming(false);
    onChanged();
    return r;
  });
  const reopen = useAction(async () => {
    const r = await send(loan!, reopenDisputedClaimCalls({ claimId: claim.claimId }));
    onChanged();
    return r;
  });
  // T-05: the bonds pay the account's own overdue claim (the chain's time, as the bond decides).
  const bond = ledger.links.bond;
  const bp = bondPayment(claim, { nowSeconds: ledger.blockTime, slashable: ledger.bondsSlashable });
  const [confirmingBonds, setConfirmingBonds] = useState(false);
  const fromBonds = useAction(async () => {
    const r = await send(loan!, slashLivenessCalls({ claimId: claim.claimId, bond: bond! }));
    setConfirmingBonds(false);
    onChanged();
    return r;
  });
  return (
    <li className="space-y-2.5 py-4 first:pt-1">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="flex items-center gap-2.5 font-semibold">
          <ZecMark size={18} />
          {view.label}
        </span>
        <span className="flex items-center gap-3">
          <span className="font-semibold tabular-nums">{formatTaz(claim.amount)}</span>
          <span className="inline-flex min-w-[96px] justify-end">
            <Badge tone={view.badge.tone} dot>
              {view.badge.text}
            </Badge>
          </span>
        </span>
      </div>
      <PayoutTrack claim={claim} />
      {view.detail && view.badge.tone !== "proof" ? <p className="text-xs text-muted">{view.detail}</p> : null}
      <LivenessGuarantee ledger={ledger} claim={claim} loan={mine ? loan : null} onChanged={onChanged} />
      {mine && d.canDispute ? (
        <div className="space-y-2">
          {!confirming ? (
            <Button variant="ghost" onClick={() => setConfirming(true)}>
              My payout did not arrive
            </Button>
          ) : (
            <Notice tone="warn" title="Dispute this payout?">
              <p>
                Only if the ZEC really did not reach your Ironwood address: check your wallet first, a payout takes a few
                blocks to confirm. The signers then have {formatPeriod(ledger.links.inclusionWindow)} to prove on Base that
                the payout is in a Zcash block; if they do not, the claim can be reopened and is paid again.
              </p>
              <p className="text-xs">You can dispute until {formatUtc(d.until!)}.</p>
              <div className="flex flex-wrap gap-2">
                <Button variant="danger" onClick={() => dispute.run()} busy={dispute.busy}>
                  Dispute the payout (sponsored)
                </Button>
                <Button variant="ghost" onClick={() => setConfirming(false)}>
                  Cancel
                </Button>
              </div>
            </Notice>
          )}
        </div>
      ) : null}
      {mine && d.canReopen ? (
        <div className="space-y-2">
          <p className="text-xs text-muted">The signers did not prove this payout by {formatUtc(d.dueAt!)}.</p>
          <Button variant="secondary" onClick={() => reopen.run()} busy={reopen.busy}>
            Reopen the claim (sponsored)
          </Button>
        </div>
      ) : null}
      {dispute.error ? (
        <Notice tone="danger" title="Not disputed" live>
          <p>{dispute.error}</p>
        </Notice>
      ) : null}
      {dispute.result ? <TxResult result={dispute.result} what="Payout disputed" /> : null}
      {reopen.error ? (
        <Notice tone="danger" title="Not reopened" live>
          <p>{reopen.error}</p>
        </Notice>
      ) : null}
      {reopen.result ? <TxResult result={reopen.result} what="Claim reopened" /> : null}
      {mine && bp.kind === "later" ? (
        <p className="text-xs text-muted">
          If the signers have not paid it by {formatUtc(bp.dueAt)}, their bonds can pay you its value in USDC instead.
        </p>
      ) : null}
      {mine && bond && bp.kind === "now" ? (
        <div className="space-y-2">
          <p className="text-xs">
            Overdue: the signers&apos; bonds would pay you <strong>{formatUsdc(bp.usdc)} {loanTokenName()}</strong> now.
          </p>
          <p className="text-xs text-muted">{bondPaymentExplainer(config.v3?.livenessPremiumBps ?? null)}</p>
          {!bp.covered ? (
            <p className="text-xs text-warn">
              The bonds hold less than that right now ({formatUsdc(ledger.bondsSlashable ?? 0n)} {loanTokenName()}). The claim stays
              payable on Zcash; try again later.
            </p>
          ) : !confirmingBonds ? (
            <Button variant="secondary" onClick={() => setConfirmingBonds(true)}>
              Claim from the bonds
            </Button>
          ) : (
            <Notice tone="warn" title={`Take ${formatUsdc(bp.usdc)} ${loanTokenName()} from the bonds?`}>
              <p>
                This cannot be undone: the claim&apos;s ZEC then goes to the bonds, not to you. The amount follows the price
                until the transaction lands.
              </p>
              <div className="flex flex-wrap gap-2">
                <Button onClick={() => fromBonds.run()} busy={fromBonds.busy}>
                  Claim from the bonds (sponsored)
                </Button>
                <Button variant="ghost" onClick={() => setConfirmingBonds(false)}>
                  Cancel
                </Button>
              </div>
            </Notice>
          )}
        </div>
      ) : null}
      {fromBonds.error ? (
        <Notice tone="danger" title="Not paid by the bonds" live>
          <p>{fromBonds.error}</p>
        </Notice>
      ) : null}
      {fromBonds.result ? <TxResult result={fromBonds.result} what="Claim paid from the bonds" /> : null}
    </li>
  );
}

/** Payouts to a committed recipient: their way on Base is committed, then confirmed, then final. */
const TRACKED_KINDS: ReadonlySet<LedgerClaim["kind"]> = new Set(["Release", "CloseUser", "CloseMarketMaker", "Liquidation", "Refund"]);

/**
 * A payout's record on Base in the seam's language: a light strip with square marks, and the π seal
 * once it is final (its delivery proven, or its confirmation undisputed through the window: Base
 * keeps no trace of which). Display only, from the fields `claimView` reads; none for a claim
 * without a destination, one the bonds paid, or one disputed, written off or compensated.
 */
function PayoutTrack({ claim }: { claim: LedgerClaim }) {
  if (!TRACKED_KINDS.has(claim.kind) || claim.bondPaid) return null;
  if (claim.status !== "Pending" && claim.status !== "Paid") return null;
  if (claim.status === "Pending" && /^0x0{64}$/i.test(claim.commitment)) return null;
  const paid = claim.status === "Paid";
  const committed = paid || claim.committed === true;
  const final = paid && claim.paidAt === 0;
  // The seam in miniature: committed on Base (light), paid on Zcash (dark), final on Base (light, the π once proven).
  const steps: { label: string; chain: "base" | "zcash"; state: MarkState; seal?: boolean }[] = [
    { label: "Committed on Base", chain: "base", state: committed ? "done" : "now" },
    { label: "Paid on Zcash, 3 of 5 signed", chain: "zcash", state: paid ? "done" : committed ? "now" : "todo" },
    { label: "Final on Base", chain: "base", state: final ? "done" : paid ? "now" : "todo", seal: final },
  ];
  return (
    <ol aria-label="Its way, on both chains" className="grid w-full overflow-hidden rounded-[6px] text-xs sm:grid-cols-3">
      {steps.map((s) => (
        <li
          key={s.label}
          aria-current={s.state === "now" ? "step" : undefined}
          className={cx(
            "flex min-h-9 items-center gap-2 px-3 py-2",
            s.chain === "base" ? "paper" : "bg-[#0a1a12] shadow-[inset_0_0_0_1px_var(--color-border)]",
            s.chain === "base" ? (s.state === "now" ? "font-semibold text-ink" : s.state === "done" ? "text-ink" : "text-ink-faint") : s.state === "now" ? "font-semibold text-text" : s.state === "done" ? "text-soft" : "text-faint",
          )}
        >
          {s.seal ? <ProofSeal size="sm" stamp className="!size-[16px] !shadow-none" /> : <StepMark chain={s.chain} state={s.state} size="sm" />}
          <span>
            {s.label}
            {s.state === "done" ? <span className="sr-only"> (done)</span> : null}
          </span>
        </li>
      ))}
    </ol>
  );
}

// ------------------------------------------------------------------ R-5: the bonds' guarantee, armed with a receipt

/** How often, and how many times, an open page asks for a receipt the signers have not given yet. */
const RECEIPT_RETRY_MS = 30_000;
const RECEIPT_ATTEMPTS = 10;

class NoReceiptYet extends Error {
  constructor() {
    super("No signer has confirmed this destination yet: it usually takes a few minutes after the claim appears on Base.");
    this.name = "NoReceiptYet";
  }
}

/**
 * R-5: the bonds pay an overdue claim only once a signer's receipt for its destination is on Base
 * (`armLiveness`). For the account's own unpaid claims with a destination, this fetches a receipt
 * (`POST /claims/:id/receipt`), checks it against the claim and the pinned signers, and sends it in
 * one sponsored UserOp: on its own once per claim and destination in this browser, or on a click.
 */
function LivenessGuarantee({
  ledger,
  claim,
  loan,
  onChanged,
}: {
  ledger: VaultLedger;
  claim: LedgerClaim;
  loan: StoredLoan | null;
  onChanged: () => void;
}) {
  const state = armingState(claim, { account: ledger.account, frozen: ledger.frozen, compromised: ledger.compromised });
  const bond = ledger.links.bond;
  const [attempts, setAttempts] = useState(0);
  const arm = useAction(async () => {
    if (!loan || !bond) throw new Error("This browser does not hold the loan's key.");
    const receipt = await api.getLivenessReceipt(claim.claimId);
    if (!receipt) throw new NoReceiptYet();
    const check = await checkReceipt(receipt, claim, { bond, signers: config.signers, chainTime: ledger.blockTime });
    if (!check.ok) throw new Error(`The receipt was not sent: ${check.reason}.`);
    const sent = await send(
      loan,
      armLivenessCalls({ claimId: claim.claimId, receivedAt: receipt.receivedAt, signature: receipt.signature, bond }),
    );
    if (sent.success) rememberArming(browserStorage(), claim.claimId, claim.commitment);
    onChanged();
    return sent;
  });
  const runArm = arm.run;
  const auto =
    state.kind === "unarmed" &&
    loan !== null &&
    bond !== null &&
    config.sponsorship &&
    !wasArmedHere(rememberedArmings(browserStorage()), claim.claimId, claim.commitment);
  // On its own: right away, then again while no signer has a receipt, a bounded number of times.
  useEffect(() => {
    if (!auto || arm.busy || arm.result || attempts >= RECEIPT_ATTEMPTS) return;
    const timer = setTimeout(
      () => {
        setAttempts((n) => n + 1);
        void runArm();
      },
      attempts === 0 ? 0 : RECEIPT_RETRY_MS,
    );
    return () => clearTimeout(timer);
  }, [auto, attempts, arm.busy, arm.result, runArm]);

  if (state.kind === "none" || !bond) return null;
  if (state.kind === "armed") {
    return <p className="text-xs text-muted">{armedNote(state, formatUtc)}</p>;
  }
  if (!loan) {
    return (
      <p className="text-xs text-muted">
        The bonds&apos; guarantee for this payout is not armed yet. It is armed from the loan&apos;s browser, with its key.
      </p>
    );
  }
  const waiting = arm.error !== null && attempts < RECEIPT_ATTEMPTS && auto;
  return (
    <div className="space-y-1">
      <p className="text-xs text-muted">
        {arm.busy
          ? "Arming the bonds' guarantee for this payout (one sponsored transaction)…"
          : "The signers' bonds guarantee this payout once a signer confirms its destination on Base."}
      </p>
      {arm.error && !arm.busy ? (
        <p className="text-xs text-warn" role="status">
          {arm.error}
          {waiting ? " Trying again shortly." : ""}
        </p>
      ) : null}
      {!arm.busy && (!auto || attempts >= RECEIPT_ATTEMPTS || arm.result?.success === false) ? (
        <Button variant="ghost" onClick={() => void arm.run()}>
          Arm the bonds&apos; guarantee (sponsored)
        </Button>
      ) : null}
      {arm.result ? <TxResult result={arm.result} what="Bonds' guarantee armed" /> : null}
    </div>
  );
}

// ------------------------------------------------------------------ T-04: freeze and compensation

/** An open theft case freezes the vault's exits; an executed one makes the compensator pay its claims. */
export function TheftNotice({ ledger }: { ledger: VaultLedger }) {
  if (ledger.compromised) return <CompromisedCard ledger={ledger} />;
  if (!ledger.frozen) return null;
  return (
    <Notice tone="warn" title="A theft case is open on this vault">
      <p>
        Someone proved on Base that this vault&apos;s ZEC may have been taken without a commitment. Until the case is decided
        (dismissed, or executed after its challenge window), the Registry refuses this vault&apos;s exits. Anyone can still
        repay its debt on Morpho.
      </p>
    </Notice>
  );
}

/** What the compensator owes this vault and its claims (read-only, through the proxy). */
function CompromisedCard({ ledger }: { ledger: VaultLedger }) {
  const [data, setData] = useState<Compensation | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const ids = compensatedClaimIds(ledger).join(",");
  const load = useCallback(async () => {
    try {
      setData(await readCompensation(ledger.vaultId, ids ? (ids.split(",") as Hex[]) : []));
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, [ledger.vaultId, ids]);
  useEffect(() => {
    void load();
  }, [load]);
  const inc = data?.incidentState;
  return (
    <Card title="This vault was compromised" action={<Badge tone="danger">Compromised</Badge>}>
      <div className="space-y-4 text-sm">
        <p className="text-muted">
          A theft of this vault&apos;s ZEC was proven and executed on Base: the signers&apos; bonds were slashed. Its claims are
          no longer paid on Zcash: the compensator pays them in USDC, and repays the vault&apos;s lenders.
        </p>
        <p className="text-muted">{COMPENSATION_SENTENCE}</p>
        {error ? <p className="text-warn">The compensator could not be read: {error}</p> : null}
        {data === undefined && !error ? <p className="text-muted">Reading the compensator…</p> : null}
        {data === null ? <p className="text-muted">This deployment has no compensator.</p> : null}
        {data ? (
          <>
            <KeyValue
              rows={[
                [
                  "Incident",
                  data.incident === 0n
                    ? "Not registered yet"
                    : inc
                      ? `#${data.incident}, ${inc.finalized ? "finalized" : `open until ${formatUtc(inc.closesAt)}`}`
                      : `#${data.incident}`,
                ],
                [
                  "This vault",
                  !data.victim.tallied
                    ? "Not tallied yet"
                    : `Tallied: ${formatTaz(data.victim.outstanding)} of collateral, ${formatUsdc(data.victim.debt)} ${loanTokenName()} of debt${data.victim.settled ? "; settled" : ""}${data.victim.authorized ? "" : " (the account no longer authorizes the Registry: no repayment, no equity)"}`,
                ],
                ["Your equity", data.victim.settled ? `${formatUsdc(data.equity)} ${loanTokenName()}` : "Fixed once the vault is settled"],
              ]}
            />
            {data.entitlements.length > 0 ? (
              <ul className="divide-y divide-border">
                {data.entitlements.map((e) => (
                  <li key={e.claimId} className="flex flex-wrap items-center justify-between gap-2 py-2">
                    <span>
                      Claim <Mono>{short(e.claimId, 10, 6)}</Mono> ({e.layer === 2 ? "third party" : "yours"}): {formatTaz(e.zat)}
                    </span>
                    <span className="tabular-nums">
                      {e.usdc === null ? `${loanTokenName()} amount fixed when the incident is finalized` : `${formatUsdc(e.usdc)} ${loanTokenName()}`}
                      {e.paid ? ", paid" : ""}
                    </span>
                  </li>
                ))}
              </ul>
            ) : null}
            <p className="text-xs text-muted">
              Once the incident is finalized and the vault settled, anyone can have these paid: the USDC always goes to the
              recorded recipient, this loan account for your own claims and equity.
            </p>
            {ledger.links.compensator ? (
              <p className="flex flex-wrap items-center gap-2 text-xs text-muted">
                Compensator <Mono>{ledger.links.compensator}</Mono> <CopyButton value={ledger.links.compensator} />
              </p>
            ) : null}
          </>
        ) : null}
        <Button variant="ghost" onClick={() => void load()}>
          Refresh
        </Button>
      </div>
    </Card>
  );
}
