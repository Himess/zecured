"use client";

import Link from "next/link";
import { useEffect, useId, useState, type ReactNode } from "react";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, Qr, useMounted } from "@/components/client";
import { IconAlert, IconArrowRight, IconCheck, IconClock, IconEyeOff, IconKey, IconShield, IconSigners, IconVault } from "@/components/icons";
import { KeyBackup } from "@/components/KeyBackup";
import { ProofTrail } from "@/components/ProofTrail";
import { TokenAmount, usdc } from "@/components/loan";
import { BaseMark, MorphoMark, UsdcMark, ZecMark, ZzecMark } from "@/components/marks";
import { ChainLabel, ProofSeal, StepMark, type Chain, type MarkState } from "@/components/seam";
import { Badge, Button, buttonClass, Card, cx, Mono, Notice, Steps } from "@/components/ui";
import { apiErrorCode, type Vault } from "@/lib/api";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY } from "@/lib/replay/flag";
import {
  notifyLoansChanged,
  useAction,
  useLoans,
  useMintingStatus,
  usePosition,
  useProtocol,
  useTypicalDurations,
  useVault,
  useVaultLedger,
  useVaultProof,
  useVaultStats,
} from "@/lib/hooks";
import { createLoan, loanStore, short, type StoredLoan } from "@/lib/keys";
import { loanTokenName } from "@/lib/labels";
import { collateralValue, maxBorrowAtTargetLtv } from "@/lib/morpho";
import { checkVaultAddress, openVault, requestRefundProof } from "@/lib/owner";
import { vaultHref } from "@/lib/routes";
import { vaultCheckUnavailable, VaultProofError, type VaultCheck } from "@/lib/signers";
import { feeNotice, formatPeriod, openGate } from "@/lib/v3";
import { awaitingDeposit, capWait, openingsNotice, openRefusal, refundProofRefusal } from "@/lib/v4";
import { blocksToDuration, formatTaz, formatZec, TICKER } from "@/lib/zcash";
import { DemoPayment } from "./DemoPayment";

const STEPS = ["Amount", "Back up key", "Deposit", "Borrow"];

type Phase = "amount" | "backup" | "deposit" | "borrow" | "done";

function phaseOf(loan: StoredLoan | null, skippedBackup: boolean, vault: Vault | null): Phase {
  if (!loan || !loan.vaultId) return "amount";
  if (!loan.backedUp && !skippedBackup) return "backup";
  if (vault?.status === "active") return "borrow";
  if (vault?.status === "closing" || vault?.status === "closed") return "done";
  return "deposit";
}

/** Saves a loan best-effort and returns the updated object (the wizard keeps it in memory too). */
function persist(loan: StoredLoan): StoredLoan {
  loanStore().save(loan);
  notifyLoansChanged();
  return loan;
}

export function BorrowWizard() {
  const mounted = useMounted();
  const { loans, storageAvailable } = useLoans();
  const [loan, setLoan] = useState<StoredLoan | null>(null);
  const [skippedBackup, setSkippedBackup] = useState(false);
  const [continueToBorrow, setContinueToBorrow] = useState(false);
  const vault = useVault(loan?.vaultId, 10_000);
  const phase = phaseOf(loan, skippedBackup, vault.data);
  const stepIndex = { amount: 0, backup: 1, deposit: 2, borrow: 3, done: 4 }[phase];

  // Keep the in-memory loan in sync with storage changes made elsewhere (e.g. backup flag).
  useEffect(() => {
    if (!loan) return;
    const stored = loans.find((l) => l.id === loan.id);
    if (stored && stored.backedUp !== loan.backedUp) setLoan(stored);
  }, [loans, loan]);

  const inProgress = loans.filter((l) => l.id !== loan?.id);
  // DEMO: the replay has one loan; coming back to this page picks it up where the judge left it.
  useEffect(() => {
    if (REPLAY && !loan && loans.length === 1 && loans[0]!.vaultId) setLoan(loans[0]!);
  }, [loan, loans]);
  // Each step starts at the top of the page, where its progress is.
  useEffect(() => {
    if (mounted) window.scrollTo({ top: 0, behavior: "smooth" });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only when the step changes
  }, [phase, continueToBorrow]);

  return (
    <div className="space-y-6">
      <div className="panel px-5 py-4 sm:px-6">
        <Steps steps={STEPS} current={stepIndex} />
      </div>
      {mounted && !storageAvailable ? (
        <Notice tone="danger" title="This browser cannot store your loan key">
          <p>Local storage is blocked (private mode or site settings). You can continue, but you must download the key backup before closing this tab.</p>
        </Notice>
      ) : null}

      {phase === "amount" ? (
        <AmountStep
          loan={loan}
          onLoan={(l) => setLoan(persist(l))}
          onDiscard={() => {
            if (loan) loanStore().remove(loan.id);
            notifyLoansChanged();
            setLoan(null);
          }}
        />
      ) : null}

      {phase === "backup" && loan ? (
        <Card lead title="Back up your loan key before you deposit" icon={<IconKey size={18} />}>
          <KeyBackup
            loan={loan}
            onConfirmed={(backedUp) =>
              // DEMO: the replay's key is a dummy; skipping its backup is remembered, so a reload does not ask again.
              backedUp ? setLoan({ ...loan, backedUp: true }) : REPLAY ? setLoan(persist({ ...loan, backedUp: true })) : setSkippedBackup(true)
            }
          />
        </Card>
      ) : null}

      {phase === "deposit" && loan ? <DepositStep loan={loan} vault={vault.data} vaultError={vault.error} /> : null}

      {phase === "done" && loan ? (
        <Card title="This loan is already closed" icon={<IconVault size={18} />}>
          <p className="mb-4 text-sm text-muted">Its vault no longer accepts deposits. Start a new loan for new collateral.</p>
          <div className="flex flex-wrap gap-2">
            <Link href={vaultHref("position", loan.vaultId!)} prefetch={false} className={buttonClass("secondary")}>
              See its position
            </Link>
            <Button onClick={() => setLoan(null)}>Start a new loan</Button>
          </div>
        </Card>
      ) : null}

      {phase === "borrow" && loan ? (
        continueToBorrow ? (
          <BorrowStep loan={loan} />
        ) : (
          <ActiveCard loan={loan} onContinue={() => setContinueToBorrow(true)} />
        )
      ) : null}

      {mounted && phase === "amount" && !loan && inProgress.length > 0 ? (
        <Card title="Continue a loan" icon={<IconClock size={18} />}>
          <ul className="space-y-2">
            {inProgress.map((l) => (
              <li
                key={l.id}
                className="flex flex-wrap items-center justify-between gap-3 rounded-2xl bg-bg/70 px-4 py-3 text-sm shadow-[inset_0_0_0_1px_var(--color-hairline)]"
              >
                <span className="flex flex-wrap items-center gap-3">
                  <TokenAmount token="TAZ" value={formatZec(BigInt(l.amount))} size="sm" />
                  <span className="text-muted">
                    account <Mono>{short(l.account)}</Mono>
                  </span>
                  {l.vaultId ? null : <Badge tone="warn">vault not opened</Badge>}
                </span>
                <span className="flex gap-2">
                  {l.vaultId ? (
                    <Link href={vaultHref("position", l.vaultId)} prefetch={false} className={buttonClass("ghost", undefined, "sm")}>
                      Position
                    </Link>
                  ) : null}
                  <Button variant="secondary" size="sm" onClick={() => setLoan(l)}>
                    Continue
                  </Button>
                </span>
              </li>
            ))}
          </ul>
        </Card>
      ) : null}
    </div>
  );
}

// ------------------------------------------------------------------ step 1

/** One line of what happens next, with its icon. */
function Fact({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <li className="flex gap-3 text-[14px] text-soft">
      <span className="mt-0.5 inline-flex size-7 flex-none items-center justify-center rounded-lg bg-surface-3 text-soft">{icon}</span>
      <span className="min-w-0">{children}</span>
    </li>
  );
}

function AmountStep({
  loan,
  onLoan,
  onDiscard,
}: {
  loan: StoredLoan | null;
  onLoan: (l: StoredLoan) => void;
  onDiscard: () => void;
}) {
  const groupId = useId();
  const [amount, setAmount] = useState<bigint>(loan ? BigInt(loan.amount) : config.denominations[0]!);
  const [ready, setReady] = useState(false);
  // T-01, T-03, R-11: a vault whose deposit would only come back as a refund, or wait for room under the cap, is not opened.
  const minting = useMintingStatus();
  const gate = openGate(minting.data, amount);
  const fee = feeNotice(amount, config.v3);
  // What each size is worth and lends at the market's price, and how many open vaults share it (its anonymity set).
  const protocol = useProtocol(60_000);
  const stats = useVaultStats(120_000);
  const price = protocol.data?.price ?? null;
  // The signers did not vouch for the vault the backend returned (C-4).
  const [unvouched, setUnvouched] = useState(false);
  // The mainnet beta opens vaults by invitation only; the backend's refusal code, if any.
  const [invite, setInvite] = useState("");
  const invitesOn = DEPLOYMENT.invites || config.openings?.inviteOnly === true;
  const [refusalCode, setRefusalCode] = useState<string | null>(null);
  const inviteId = useId();
  const cannotCheck = vaultCheckUnavailable();
  const open = useAction(async (current: StoredLoan | null) => {
    setUnvouched(false);
    setRefusalCode(null);
    // The key is saved *before* the backend is contacted, so a crash never loses it.
    const l = current ?? createLoan(amount);
    if (!current) onLoan(l);
    try {
      const res = await openVault(l, invitesOn ? { invite } : {});
      // Stored as payable only now that every signer vouches for the address (lib/signers.ts).
      onLoan({
        ...l,
        vaultId: res.vaultId,
        vaultAddress: res.address,
        expiryHeight: res.expiryHeight,
        tagKeyHash: res.tagKeyHash,
        vaultSignatures: res.signatures,
      });
      return res;
    } catch (e) {
      if (e instanceof VaultProofError) setUnvouched(true);
      setRefusalCode(apiErrorCode(e));
      throw e;
    }
  });
  const alreadyBound = open.error?.includes("already has a vault");
  const refusal = openRefusal(refusalCode);
  const needsInvite = invitesOn && !invite.trim();
  // P-8: the operator's switch on new vaults (never on existing vaults' exits and payouts).
  const openings = openingsNotice(config.openings);
  const openingsClosed = config.openings?.open === false;

  return (
    <Card lead title="How much ZEC do you want to lock?" icon={<ZecMark size={20} />}>
      <fieldset className="space-y-3" disabled={Boolean(loan) || open.busy}>
        <legend className="sr-only">Collateral amount</legend>
        <div className={cx("grid gap-3", config.denominations.length >= 3 ? "md:grid-cols-3" : "sm:grid-cols-2")}>
          {config.denominations.map((d) => {
            const selected = amount === d;
            const blocked = !openGate(minting.data, d).allowed;
            const peers = stats.data?.byAmount.get(d);
            return (
              <label
                key={d.toString()}
                className={cx(
                  "relative flex cursor-pointer flex-col gap-3 rounded-[20px] p-5 transition-[box-shadow,background-color]",
                  selected
                    ? "bg-[linear-gradient(180deg,rgb(244_183_40/0.10),rgb(244_183_40/0.02))] shadow-[inset_0_0_0_2px_var(--color-accent)]"
                    : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-border)] hover:shadow-[inset_0_0_0_1px_var(--color-line)]",
                )}
              >
                <input type="radio" name={groupId} checked={selected} onChange={() => setAmount(d)} className="sr-only" />
                <span className="flex items-center justify-between">
                  <ZecMark size={34} />
                  <span
                    aria-hidden="true"
                    className={cx(
                      "inline-flex size-6 items-center justify-center rounded-full",
                      selected ? "bg-accent text-on-accent" : "shadow-[inset_0_0_0_1.5px_var(--color-line)]",
                    )}
                  >
                    {selected ? <IconCheck size={14} strokeWidth={3} /> : null}
                  </span>
                </span>
                <span>
                  <span className="block text-[30px] leading-none font-extrabold tabular-nums font-stretch-118%">
                    {formatZec(d)} <span className="text-[17px] font-bold text-muted">{TICKER}</span>
                  </span>
                  <span className="mt-1.5 block text-[13.5px] text-muted">
                    {price !== null ? `≈ $${usdc(collateralValue(d, price))} at the market's price` : "Standard vault size"}
                  </span>
                </span>
                <span className="flex items-center gap-2 border-t border-hairline pt-3 text-[13.5px]">
                  <UsdcMark size={16} />
                  {price !== null ? (
                    <span>
                      Borrow up to <strong className="tabular-nums">{usdc(maxBorrowAtTargetLtv(d, price))}</strong> {loanTokenName()}
                    </span>
                  ) : (
                    <span className="text-muted">Borrow up to 40 % of its value</span>
                  )}
                </span>
                {peers !== undefined ? (
                  <span className="flex items-center gap-2 text-[12.5px] text-muted">
                    <IconEyeOff size={14} />
                    {peers === 0 ? "No other open vault of this size yet" : `Hides among ${peers} open vault${peers === 1 ? "" : "s"} of this size`}
                  </span>
                ) : null}
                {blocked ? <span className="text-xs font-semibold text-warn">A deposit of this size cannot back a loan right now.</span> : null}
              </label>
            );
          })}
        </div>
        <p className="text-[13px] text-muted">
          Fixed sizes are what hide your amount: every vault holds one of these, so your position on Base cannot be told apart
          from the others of the same size. A custom amount would single you out.
        </p>
      </fieldset>

      <ul className="mt-6 grid gap-3 md:grid-cols-2">
        <Fact icon={<ZecMark size={16} />}>You pay exactly this amount from Ironwood notes, plus the network fee.</Fact>
        <Fact icon={<IconKey size={16} />}>
          A fresh owner key and a fresh smart account are created in this browser for this loan only. No wallet connection, no
          ETH.
        </Fact>
        <Fact icon={<IconEyeOff size={16} />}>
          Opening the vault happens off-chain: nothing touches Base until your deposit is proven there.
        </Fact>
        <Fact icon={<IconSigners size={16} />}>
          Five independent signer nodes generate the vault&apos;s key together; any three are needed to move it.
        </Fact>
        {fee ? <Fact icon={<IconAlert size={16} />}>{fee}</Fact> : null}
      </ul>

      <div className="mt-6 space-y-3">
        {gate.message ? (
          <Notice tone={gate.allowed ? "info" : "danger"} title={gate.allowed ? "Your deposit would wait" : "Not now"}>
            <p>{gate.message}</p>
          </Notice>
        ) : null}
        {openings ? (
          <Notice tone={openingsClosed ? "warn" : "info"} title={openings.title}>
            <p>{openings.body}</p>
          </Notice>
        ) : null}
        <Notice tone="warn" title="Get your ZEC into Ironwood well before this step">
          <p>
            Moving exactly {formatTaz(amount)} into Ironwood and depositing right away links the two: the move into Ironwood is
            public. Move a different amount, and wait hours or days.
            {config.vaultExpiryBlocks !== null
              ? ` Once open, the vault accepts the deposit for ${blocksToDuration(config.vaultExpiryBlocks)} (${config.vaultExpiryBlocks} Zcash blocks).`
              : null}
          </p>
        </Notice>
      </div>

      <label
        className={cx(
          "mt-5 flex cursor-pointer items-start gap-3 rounded-2xl px-4 py-3.5 text-[14px] transition-colors",
          ready ? "bg-ok/[0.07] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_35%,transparent)]" : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-border)]",
        )}
      >
        <input type="checkbox" checked={ready} onChange={(e) => setReady(e.target.checked)} className="mt-1 size-4" />
        <span className="text-soft">
          My ZEC is already in the Ironwood pool, and I deposit in one Ironwood → Ironwood payment (
          <Link href="/prepare">why this matters</Link>).
        </span>
      </label>
      {invitesOn ? (
        <div className="mt-4 space-y-1.5">
          <label htmlFor={inviteId} className="block text-sm font-semibold text-soft">
            Invite code
          </label>
          <input id={inviteId} value={invite} onChange={(e) => setInvite(e.target.value)} autoComplete="off" spellCheck={false} className="field max-w-sm" />
          <p className="text-xs text-muted">The mainnet beta opens vaults by invitation only; each code opens one vault.</p>
        </div>
      ) : null}
      {!config.ownerSigning ? (
        <div className="mt-4">
          <Notice tone="danger" title="Opening a vault is disabled">
            <p>This build cannot sign owner requests right now: see the configuration notice at the top of the page.</p>
          </Notice>
        </div>
      ) : cannotCheck ? (
        <div className="mt-4">
          <Notice tone="danger" title="Opening a vault is disabled">
            <p>{cannotCheck}</p>
          </Notice>
        </div>
      ) : null}
      <div className="mt-6 flex flex-wrap items-center gap-3">
        <Button
          size="lg"
          onClick={() => open.run(loan)}
          busy={open.busy}
          disabled={(!ready && !loan) || !config.ownerSigning || cannotCheck !== null || !gate.allowed || needsInvite || openingsClosed}
        >
          {open.busy ? "Opening the vault…" : loan ? "Retry opening the vault" : `Open a ${formatTaz(amount)} vault`}
          {open.busy ? null : <IconArrowRight size={18} />}
        </Button>
        {loan ? (
          <Button variant="ghost" onClick={onDiscard} disabled={open.busy}>
            Discard this key and start over
          </Button>
        ) : null}
      </div>
      {open.busy ? <OpeningProgress /> : null}
      {open.error && refusal ? (
        <div className="mt-4">
          <Notice tone={refusal.openingsClosed ? "info" : "warn"} title={refusal.title} live>
            <p>{refusal.body}</p>
            <p>Your new key is saved in this browser; you can retry.</p>
          </Notice>
        </div>
      ) : open.error ? (
        <div className="mt-4">
          <Notice tone="danger" title="The vault could not be opened" live>
            <p>{open.error}</p>
            {unvouched ? (
              <p>
                Nothing was kept for this vault. Discard this key and start over; if it happens again, the backend may be
                compromised.
              </p>
            ) : alreadyBound ? (
              <p>This key already opened a vault whose details were lost. Discard it and start over with a new key.</p>
            ) : (
              <p>Your new key is saved in this browser; you can retry.</p>
            )}
          </Notice>
        </div>
      ) : null}
    </Card>
  );
}

/** While the vault opens: the browser's proof of work, then the five signers' key generation, as they happen. */
function OpeningProgress() {
  const [t, setT] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setT((x) => x + 1), 900);
    return () => clearInterval(timer);
  }, []);
  const lit = Math.min(5, Math.max(0, t - 2));
  return (
    <div className="mt-5 grid gap-4 rounded-2xl bg-bg/70 p-5 shadow-[inset_0_0_0_1px_var(--color-border)] sm:grid-cols-[auto_minmax(0,1fr)] sm:items-center" aria-live="polite">
      <div aria-hidden="true" className="flex items-center gap-2">
        {[0, 1, 2, 3, 4].map((i) => (
          <span
            key={i}
            className={cx(
              "inline-flex size-9 items-center justify-center rounded-full text-[12px] font-bold transition-colors duration-500",
              i < lit ? "bg-pool/25 text-pool-soft shadow-[inset_0_0_0_1.5px_var(--color-pool)]" : "bg-surface-3 text-faint",
            )}
          >
            {i + 1}
          </span>
        ))}
      </div>
      <p className="text-[14px] text-soft">
        {t < 3 ? (
          <>This browser computes a small proof of work, which keeps vault openings from being spammed…</>
        ) : (
          <>
            The five signer nodes are generating your vault&apos;s threshold key together (FROST key generation). Each holds
            one share; none ever sees the whole key. This can take a little while.
          </>
        )}
      </p>
    </div>
  );
}

// ------------------------------------------------------------------ step 3

/** "2 of 3", or the signers' requirement from GET /config while the vault reports none. */
function confirmationsText(seen: number | undefined): string {
  const needed = config.confirmations;
  if (seen !== undefined) return needed !== null ? `${seen} of ${needed}` : String(seen);
  return needed !== null ? `${needed} needed` : "—";
}

const STATUS_STEPS = [
  { status: "awaiting_deposit", label: "Waiting for your Ironwood deposit", duration: null },
  { status: "deposit_seen", label: "Deposit on Zcash; its zero-knowledge proof is being made for Base", duration: "deposit-to-proof" },
  { status: "pending", label: "Proven on Base; waiting to be activated", duration: "proof-to-activation" },
  { status: "active", label: "zZEC minted into your Morpho position", duration: null },
] as const;

/**
 * Where the deposit is: the signers' status, and Base's own word: its pending activation (K3) between proof and mint,
 * then the active vault (the backend follows Base's events a little later).
 */
function statusStep(vault: Vault | null, basePending: boolean, baseActive: boolean): number {
  if (vault?.status === "active" || baseActive) return 3;
  if (basePending) return 2;
  // The deposit is mined with its confirmations; its proof is not on Base yet.
  if (vault?.status === "awaiting_deposit" && vault.depositSeen) return 1;
  const i = vault ? STATUS_STEPS.findIndex((s) => s.status === vault.status) : 0;
  return Math.max(0, i);
}

/** What the address label says about the signers' signatures (C-4). */
const VOUCHED: Record<VaultCheck, string> = {
  pinned: "Ironwood-only, signed by all 5 signers and checked in this browser",
  backend: "Ironwood-only, signed by the 5 signers the backend lists (development build: not pinned)",
  unchecked: "Ironwood-only, NOT checked against any signer set (development build)",
};

/** The status rows' text, by chain and state: dim once done, bright while current. */
const STATUS_TEXT: Record<Chain, Record<MarkState, string>> = {
  zcash: { done: "text-muted", now: "text-text", todo: "text-faint" },
  base: { done: "text-ink-muted", now: "text-ink", todo: "text-ink-muted" },
};

/** A figure under the payment request: a label and its value. */
function DepositFact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 rounded-2xl bg-bg/70 px-4 py-3 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      <p className="text-[12.5px] text-muted">{label}</p>
      <div className="mt-1 text-[14.5px] font-semibold">{children}</div>
    </div>
  );
}

function DepositStep({ loan, vault, vaultError }: { loan: StoredLoan; vault: Vault | null; vaultError: string | null }) {
  const amount = BigInt(loan.amount);
  const check = loan.vaultAddress ? checkVaultAddress(loan.vaultAddress, amount) : null;
  // The stored address is shown, as text or QR code, only while every signer vouches for it.
  const proof = useVaultProof(loan);
  const vouched = proof.state === "vouched" ? proof.check : null;
  // Base: a proven deposit waiting to be activated, or one that came back as a refund (T-02).
  const ledger = useVaultLedger(loan.vaultId, 20_000);
  const l = ledger.data;
  const statusIndex = statusStep(vault, Boolean(l?.pending), l?.status === "Active");
  const refunded = Boolean(l && l.claims.some((c) => c.kind === "Refund"));
  // R-12: proven as a refund, its claim not open yet.
  const refundPending = (l?.pendingRefunds?.length ?? 0) > 0;
  // Paying again after the deposit was seen, or after expiry, does not back the loan.
  const awaiting = awaitingDeposit({
    vaultStatus: vault?.status,
    depositSeen: vault?.depositSeen,
    pending: Boolean(l?.pending),
    refunding: refunded || refundPending,
  });
  const expiry = vault?.expiryHeight ?? loan.expiryHeight;
  const blocksLeft = vault?.tipHeight !== undefined && expiry !== undefined ? expiry - vault.tipHeight : null;
  // R-11.1: the deposit is on Zcash but its proof waits for room under the coverage cap.
  const minting = useMintingStatus();
  const cap = capWait({
    vaultStatus: vault?.status,
    depositSeen: vault?.depositSeen,
    baseStatus: l?.status,
    pending: Boolean(l?.pending),
    refunding: refunded || refundPending,
    mintBlock: minting.data?.mintBlock.get(amount),
  });
  const refundInstead = useAction(async () => {
    try {
      await requestRefundProof(loan, loan.vaultId!);
    } catch (e) {
      const why = refundProofRefusal(apiErrorCode(e));
      if (why) throw new Error(why);
      throw e;
    }
    return true;
  });
  // P-8: typical durations measured on the deployment (the evidence file), when published.
  const durations = useTypicalDurations();
  const statusId = useId();
  // The seal pulses, and says so, while the deposit's proof is being made (not while the coverage cap holds it back).
  const proving = statusIndex === 1 && cap === "none";
  const statusRow = (s: (typeof STATUS_STEPS)[number], i: number, chain: Chain) => {
    const state: MarkState = i < statusIndex ? "done" : i === statusIndex ? "now" : "todo";
    return (
      <li key={s.status} aria-current={state === "now" ? "step" : undefined} className="grid grid-cols-[22px_minmax(0,1fr)] items-start gap-3">
        <StepMark chain={chain} state={state} className="mt-[3px]" />
        <span className={cx("text-[15px] leading-[1.45]", STATUS_TEXT[chain][state], state === "now" && "font-semibold")}>
          {s.label}
          {state === "done" ? <span className="sr-only"> (done)</span> : null}
          {s.duration && durations.get(s.duration) !== undefined ? (
            <span className={cx("mt-1 block text-[13px] font-normal", chain === "zcash" ? "text-muted" : "text-ink-muted")}>
              Typically {formatPeriod(durations.get(s.duration)!)} (measured)
            </span>
          ) : null}
        </span>
      </li>
    );
  };

  return (
    <div className="grid items-start gap-6 min-[980px]:grid-cols-[minmax(0,1fr)_minmax(0,24rem)] [&>*]:min-w-0">
      <Card
        lead
        title={awaiting ? `Pay ${formatTaz(amount)} from Ironwood` : "Your deposit"}
        icon={<ZecMark size={20} />}
        action={vault ? <Badge tone={awaiting ? "info" : "ok"} dot>{awaiting ? "Waiting for your payment" : "Deposit received"}</Badge> : null}
      >
        <div className="space-y-4">
          {check && !check.ok ? (
            <Notice tone="danger" title="Do not pay this address">
              {check.problems.map((p) => (
                <p key={p}>{p}</p>
              ))}
            </Notice>
          ) : null}
          {awaiting && proof.state === "refused" ? (
            <Notice tone="danger" title="Do not pay this vault" live>
              <p>{proof.message}</p>
              <p>The signers do not vouch for this address. Discard this loan and start a new one.</p>
            </Notice>
          ) : null}
          {awaiting && proof.state === "legacy" ? (
            <Notice tone="danger" title="This vault's address cannot be checked">
              <p>
                It was opened by an earlier version of Zecured, whose vault signatures this app does not check, so nothing
                shows that the signers control it. Do not pay it: let it expire and start a new loan.
              </p>
            </Notice>
          ) : null}
          {awaiting && proof.state === "checking" ? (
            <p className="text-sm text-muted" aria-live="polite">
              Checking the signers&apos; signatures on this vault…
            </p>
          ) : null}
          {!awaiting && (vault?.status === "deposit_seen" || vault?.depositSeen) && !l?.pending && cap === "none" ? (
            <>
              <ProofPipeline confirmations={vault?.confirmations} />
              <Notice tone="ok" title="Deposit received on Zcash" live>
                <p>
                  A zero-knowledge proof that your deposit is in a Zcash block is being made and sent to Base; nothing is minted
                  on anyone&apos;s word. Do not pay again: a second payment does not back the loan (it can only come back as a
                  refund).
                </p>
              </Notice>
            </>
          ) : null}
          {cap !== "none" ? (
            <Notice
              tone="warn"
              title={cap === "confirmed" ? "Your deposit waits for room under the coverage cap" : "The coverage cap is full right now"}
              live
            >
              <p>
                {cap === "confirmed" ? "Your deposit is on Zcash, but the" : "If you already paid this vault, your deposit is on Zcash, but the"}{" "}
                signers&apos; bonds cover no more collateral right now, so its proof cannot land yet. Nothing is lost and the vault
                stays yours: the proof is sent again once there is room, and zZEC is minted then.
              </p>
              {cap === "possible" ? <p>If you have not paid yet, wait until there is room before you pay.</p> : null}
              {refundInstead.result ? (
                <p>
                  Refund requested: the coordinator proves your deposit as a refund instead. It then appears on the{" "}
                  <Link href={vaultHref("refund", loan.vaultId!)} prefetch={false}>
                    refunds page
                  </Link>
                  , pending for a few Zcash blocks until its claim opens; you then give it an Ironwood destination there (the
                  payout&apos;s network fee is deducted).
                </p>
              ) : (
                <>
                  <p>If you would rather not wait, have it proven as a refund: it then comes back to an Ironwood address you give.</p>
                  <Button variant="secondary" onClick={() => void refundInstead.run()} busy={refundInstead.busy}>
                    Refund my deposit instead
                  </Button>
                </>
              )}
              {refundInstead.error ? <p className="text-danger">{refundInstead.error}</p> : null}
            </Notice>
          ) : null}
          {l?.pending ? (
            <Notice tone="ok" title="Deposit proven on Base" live>
              <p>
                It waits a few Zcash blocks before anyone can activate it; the backend&apos;s keeper does, and zZEC is minted into
                your position.{" "}
                <Link href={vaultHref("position", loan.vaultId!)} prefetch={false}>
                  See when
                </Link>
                .
              </p>
            </Notice>
          ) : null}
          {refundPending && !refunded ? (
            <Notice tone="info" title="Your deposit was proven as a refund" live>
              <p>
                Its proof is on Base and waits a few Zcash blocks, like an activation, before its refund opens (pending until
                then). The backend&apos;s keeper opens it; you then give it an Ironwood destination on the{" "}
                <Link href={vaultHref("refund", loan.vaultId!)} prefetch={false}>
                  refunds page
                </Link>
                .
              </p>
            </Notice>
          ) : null}
          {refunded ? (
            <Notice tone="warn" title="A deposit came back as a refund" live>
              <p>
                A deposit to this vault could not back the loan and was proven on Base as a refund.{" "}
                <Link href={vaultHref("refund", loan.vaultId!)} prefetch={false}>
                  Give it a destination
                </Link>{" "}
                to get it back.
              </p>
            </Notice>
          ) : null}
          {!awaiting && vault?.status === "expired" ? (
            <Notice tone="warn" title="This vault no longer accepts deposits">
              <p>Its deposit deadline passed. Do not pay this address; start a new loan instead.</p>
            </Notice>
          ) : null}
          {awaiting && vouched && check?.ok && check.uri ? (
            <div className="space-y-5">
              <DemoPayment loan={loan} />
              {vouched !== "pinned" ? (
                <Notice tone="warn" title="Development build: the signer set is not pinned">
                  <p>
                    {vouched === "backend"
                      ? "The vault's signatures were checked against the signer set the backend reports, which does not protect against a compromised backend."
                      : "The vault's signatures were not checked: neither this build nor the backend names the signer set."}{" "}
                    A production build pins NEXT_PUBLIC_SIGNERS and refuses vaults it cannot check.
                  </p>
                </Notice>
              ) : null}
              <div className="grid gap-5 sm:grid-cols-[auto_minmax(0,1fr)]">
                <div className="flex flex-col items-center gap-2">
                  <div className="rounded-[20px] bg-white p-3 shadow-[0_20px_40px_-24px_rgb(0_0_0/0.9)]">
                    <Qr value={check.uri} label={`ZIP 321 payment request for ${formatTaz(amount)} to the vault`} size={208} />
                  </div>
                  <p className="text-[12px] text-muted">Scan with an Ironwood-capable wallet</p>
                </div>
                <div className="min-w-0 space-y-4">
                  <div>
                    <p className="text-[12.5px] font-semibold text-muted">Send exactly</p>
                    <TokenAmount token="TAZ" value={formatZec(amount)} size="xl" />
                    <p className="mt-1 text-[13px] text-muted">in a single payment, plus the network fee</p>
                  </div>
                  <div>
                    <p className="flex items-center gap-1.5 text-[12.5px] font-semibold text-muted">
                      To the vault
                      <span className={cx("inline-flex items-center gap-1", vouched === "pinned" ? "text-ok" : "text-warn")}>
                        <IconShield size={14} />
                        {VOUCHED[vouched]}
                      </span>
                    </p>
                    <div className="mt-1.5 rounded-xl bg-bg/80 px-3 py-2.5 shadow-[inset_0_0_0_1px_var(--color-border)]">
                      <Mono className="block text-[12px] leading-relaxed">{loan.vaultAddress}</Mono>
                    </div>
                    <div className="mt-2 flex flex-wrap gap-2">
                      <CopyButton value={loan.vaultAddress!} label="Copy address" />
                      <CopyButton value={check.uri} label="Copy payment URI" />
                    </div>
                  </div>
                </div>
              </div>
              <div className="grid gap-3 sm:grid-cols-3">
                <DepositFact label="Deposit deadline">
                  {expiry !== undefined ? (
                    <>
                      <span className="tabular-nums">Block #{expiry.toLocaleString("en-US")}</span>
                      {blocksLeft !== null ? (
                        <span className="block text-[12.5px] font-normal text-muted">
                          {blocksLeft > 0 ? `${blocksLeft} blocks left (${blocksToDuration(blocksLeft)})` : "already passed"}
                        </span>
                      ) : null}
                    </>
                  ) : (
                    "—"
                  )}
                </DepositFact>
                <DepositFact label="Confirmations">
                  <span className="tabular-nums">{confirmationsText(vault?.confirmations)}</span>
                </DepositFact>
                <DepositFact label="Vault id">
                  <span className="flex items-center gap-2">
                    <Mono>{short(loan.vaultId!, 8, 6)}</Mono>
                    <CopyButton value={loan.vaultId!} label="Copy" />
                  </span>
                </DepositFact>
              </div>
              <ul className="space-y-2 rounded-2xl bg-warn/[0.05] p-4 text-[13.5px] text-soft shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_30%,transparent)]">
                <li className="flex gap-2.5">
                  <IconAlert size={16} className="mt-0.5 text-warn" />
                  <span>
                    <strong className="text-text">Pay from notes that have waited in Ironwood.</strong> Paying right after moving exactly{" "}
                    {formatTaz(amount)} into Ironwood links that public move to this loan. If your ZEC is not in Ironwood yet, let this
                    vault expire and start a new loan later.
                  </span>
                </li>
                <li className="flex gap-2.5">
                  <IconAlert size={16} className="mt-0.5 text-warn" />
                  <span>
                    <strong className="text-text">Only an exact Ironwood → Ironwood payment counts.</strong> Paying from transparent,
                    Sapling or Orchard funds, a different amount, or after the deadline does not back the loan: such a payment comes
                    back only as a refund proven on Base, minus the payout&apos;s network fee.
                  </span>
                </li>
              </ul>
            </div>
          ) : null}
        </div>
      </Card>

      {/* The seam: the deposit's way from Zcash (dark, round marks) through its proof (the π seal) to Base (light, square marks). */}
      <div className="space-y-3 min-[980px]:sticky min-[980px]:top-24">
        <aside aria-labelledby={statusId} className="overflow-hidden rounded-[22px] shadow-[0_0_0_1px_var(--color-border),0_30px_60px_-36px_rgb(0_0_0/0.9)]">
          <h2 id={statusId} className="sr-only">
            Status
          </h2>
          <div className="bg-[radial-gradient(120%_90%_at_20%_0%,#123224_0%,#0a1a12_45%,#060d09_100%)] px-[26px] pt-[24px] pb-11">
            <div className="flex items-center justify-between">
              <ChainLabel chain="zcash">On Zcash</ChainLabel>
              <ZecMark size={20} />
            </div>
            <ol className="mt-[18px] flex flex-col gap-4" aria-live="polite">
              {STATUS_STEPS.slice(0, 2).map((s, i) => statusRow(s, i, "zcash"))}
            </ol>
            {vault?.status === "expired" ? (
              <div className="mt-5">
                <Notice tone="warn" title="This vault expired without a valid deposit">
                  <p>
                    Start a new loan. Anything you sent comes back only as a refund proven on Base: see the vault&apos;s{" "}
                    <Link href={vaultHref("refund", loan.vaultId!)} prefetch={false}>
                      refunds
                    </Link>
                    .
                  </p>
                </Notice>
              </div>
            ) : null}
            {vault && vault.nonConforming.length > 0 ? (
              <div className="mt-5">
                <Notice tone="warn" title="A payment did not follow the rules">
                  <p>
                    {vault.nonConforming.length} payment(s) to this vault cannot back the loan. They come back only as refunds
                    proven on Base:{" "}
                    <Link href={vaultHref("refund", loan.vaultId!)} prefetch={false}>
                      see the refunds
                    </Link>
                    .
                  </p>
                </Notice>
              </div>
            ) : null}
            {vaultError ? (
              <p className="mt-5 text-sm text-danger" role="status">
                Status unavailable: {vaultError}
              </p>
            ) : null}
          </div>
          <div aria-hidden="true" className="relative z-[2] h-0">
            <ProofSeal pulse={proving} className="absolute top-[-30px] left-[22px]" />
            {proving ? (
              <span className="absolute top-[-13px] left-24 inline-flex min-h-[26px] items-center rounded-full bg-accent px-3 text-[12.5px] font-bold whitespace-nowrap text-on-accent">
                Proving your deposit
              </span>
            ) : null}
          </div>
          <div className="paper px-[26px] pt-11 pb-[24px]">
            <div className="flex items-center justify-between">
              <ChainLabel chain="base">On Base</ChainLabel>
              <BaseMark size={18} />
            </div>
            <ol start={3} className="mt-[18px] flex flex-col gap-4" aria-live="polite">
              {STATUS_STEPS.slice(2).map((s, i) => statusRow(s, i + 2, "base"))}
            </ol>
            <p className="mt-5 border-t border-rule pt-4 text-[13.5px] leading-[1.55] text-ink-muted">
              Base will see your vault id, the amount, a keyed tag and a later Zcash header. Never your deposit&apos;s block,
              transaction or address.
            </p>
          </div>
        </aside>
        {l?.pending || l?.status === "Active" ? (
          <div className="panel p-5">
            <p className="mb-3 flex items-center gap-2 text-[13px] font-semibold text-muted">
              <BaseMark size={14} /> On Base, with its transactions
            </p>
            <ProofTrail vaultId={loan.vaultId!} account={loan.account} since={loan.createdAt} compact />
          </div>
        ) : null}
        <p className="flex items-center gap-2 text-xs text-muted">
          <IconClock size={14} />
          Updates every 10 seconds. You can close this page and come back from My loans.
        </p>
      </div>
    </div>
  );
}

/**
 * The deposit's way to Base while it is proven: its confirmations on Zcash, the zero-knowledge proof made of it, and
 * Base's check of that proof. The second stage runs while the confirmations are in; the third is the proof's landing.
 */
function ProofPipeline({ confirmations }: { confirmations: number | undefined }) {
  const needed = config.confirmations;
  const confirmed = needed !== null && confirmations !== undefined && confirmations >= needed;
  const stages = [
    {
      icon: <ZecMark size={20} />,
      title: "Confirmed on Zcash",
      body: needed !== null ? `${Math.min(confirmations ?? 0, needed)} of ${needed} confirmations` : "Its confirmations come in",
      state: confirmed ? "done" : "now",
    },
    {
      icon: <ProofSeal size="sm" className="!size-[20px]" />,
      title: "Proven in zero knowledge",
      body: "The RISC Zero zkVM proves the deposit is in a Zcash block whose header Base already verified",
      state: confirmed ? "now" : "todo",
    },
    {
      icon: <BaseMark size={18} />,
      title: "Verified on Base",
      body: "The Intake checks the proof: no signer vouches for the deposit",
      state: "todo",
    },
  ] as const;
  return (
    <ol className="grid gap-2 sm:grid-cols-3" aria-label="Your deposit's proof">
      {stages.map((s) => (
        <li
          key={s.title}
          aria-current={s.state === "now" ? "step" : undefined}
          className={cx(
            "rounded-2xl p-4",
            s.state === "now"
              ? "bg-accent/[0.07] shadow-[inset_0_0_0_1.5px_rgb(244_183_40/0.6)]"
              : s.state === "done"
                ? "bg-ok/[0.06] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_35%,transparent)]"
                : "bg-bg/60 shadow-[inset_0_0_0_1px_var(--color-hairline)]",
          )}
        >
          <p className="flex items-center justify-between">
            <span className="inline-flex size-8 items-center justify-center rounded-lg bg-bg">{s.icon}</span>
            {s.state === "done" ? (
              <IconCheck size={16} strokeWidth={2.6} className="text-ok" />
            ) : s.state === "now" ? (
              <span aria-hidden="true" className="dot-live size-2 rounded-full bg-accent" />
            ) : null}
          </p>
          <p className={cx("mt-2.5 text-[14px] font-bold", s.state === "todo" && "text-muted")}>{s.title}</p>
          <p className="mt-0.5 text-[12.5px] text-muted">{s.body}</p>
        </li>
      ))}
    </ol>
  );
}

// ------------------------------------------------------------------ the deposit is active

function ActiveCard({ loan, onContinue }: { loan: StoredLoan; onContinue: () => void }) {
  // The collateral as Morpho holds it: the deposit less the opening fee, if any.
  const position = usePosition(loan.account, 30_000);
  const collateral = position.data?.position.collateral ?? BigInt(loan.amount);
  return (
    <Card lead>
      <div className="grid items-center gap-6 md:grid-cols-[minmax(0,1fr)_auto]">
        <div className="space-y-3">
          <span className="inline-flex size-12 items-center justify-center rounded-2xl bg-ok/15 text-ok shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_40%,transparent)]">
            <IconCheck size={26} strokeWidth={2.6} />
          </span>
          <h2 className="text-[26px] leading-tight font-extrabold font-stretch-118%">Your deposit is proven and active</h2>
          <p className="max-w-xl text-[15px] text-soft">
            A zero-knowledge proof of your Ironwood deposit was verified on Base, and zZEC was minted straight into your
            account&apos;s Morpho position. No signer vouched for it.
          </p>
          <Button size="lg" onClick={onContinue}>
            Continue to borrow
            <IconArrowRight size={18} />
          </Button>
        </div>
        <div className="flex items-center gap-3 rounded-2xl bg-bg/70 px-5 py-4 shadow-[inset_0_0_0_1px_var(--color-border)]">
          <ZzecMark size={40} />
          <div>
            <p className="text-[12.5px] text-muted">Collateral in Morpho</p>
            <p className="text-[20px] font-bold tabular-nums">
              {formatZec(collateral)} <span className="text-[14px] text-muted">zZEC</span>
            </p>
          </div>
          <MorphoMark size={30} className="ml-2" />
        </div>
      </div>
      <div className="mt-6 border-t border-hairline pt-5">
        <p className="mb-3 flex items-center gap-2 text-[13px] font-semibold text-muted">
          <BaseMark size={14} /> Where Base verified it
        </p>
        <ProofTrail vaultId={loan.vaultId!} account={loan.account} since={loan.createdAt} compact />
      </div>
    </Card>
  );
}

// ------------------------------------------------------------------ step 4

function BorrowStep({ loan }: { loan: StoredLoan }) {
  const position = usePosition(loan.account, 30_000);
  const [done, setDone] = useState(false);
  return (
    <Card lead title={`Borrow ${loanTokenName()}`} icon={<UsdcMark size={20} />}>
      {position.error && config.contractsDeployed ? (
        <div className="mb-4">
          <Notice tone="danger" title="Could not read your position" live>
            <p>{position.error}</p>
          </Notice>
        </div>
      ) : null}
      <BorrowForm
        loan={loan}
        snapshot={position.data}
        onBorrowed={() => {
          setDone(true);
          void position.refresh();
        }}
      />
      <div className="mt-6 flex flex-wrap gap-2">
        <Link href={vaultHref("position", loan.vaultId!)} prefetch={false} className={buttonClass(done ? "primary" : "secondary")}>
          Go to your position
          <IconArrowRight size={16} />
        </Link>
      </div>
    </Card>
  );
}
