"use client";

import Link from "next/link";
import { useEffect, useId, useState } from "react";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, Qr, useMounted } from "@/components/client";
import { KeyBackup } from "@/components/KeyBackup";
import { ChainLabel, ProofSeal, StepMark, type Chain, type MarkState } from "@/components/seam";
import { Badge, Button, buttonClass, Card, cx, KeyValue, Mono, Notice, Steps } from "@/components/ui";
import { apiErrorCode, type Vault } from "@/lib/api";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import {
  notifyLoansChanged,
  useAction,
  useLoans,
  useMintingStatus,
  usePosition,
  useTypicalDurations,
  useVault,
  useVaultLedger,
  useVaultProof,
} from "@/lib/hooks";
import { createLoan, loanStore, short, type StoredLoan } from "@/lib/keys";
import { checkVaultAddress, openVault, requestRefundProof } from "@/lib/owner";
import { vaultHref } from "@/lib/routes";
import { vaultCheckUnavailable, VaultProofError, type VaultCheck } from "@/lib/signers";
import { feeNotice, formatPeriod, openGate } from "@/lib/v3";
import { awaitingDeposit, capWait, openingsNotice, openRefusal, refundProofRefusal } from "@/lib/v4";
import { blocksToDuration, formatTaz } from "@/lib/zcash";
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

  return (
    <div className="space-y-6">
      <Steps steps={STEPS} current={stepIndex} />
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
        <Card title="Back up your loan key before you deposit">
          <KeyBackup
            loan={loan}
            onConfirmed={(backedUp) => (backedUp ? setLoan({ ...loan, backedUp: true }) : setSkippedBackup(true))}
          />
        </Card>
      ) : null}

      {phase === "deposit" && loan ? <DepositStep loan={loan} vault={vault.data} vaultError={vault.error} /> : null}

      {phase === "done" && loan ? (
        <Card title="This loan is already closed">
          <p className="mb-4 text-sm text-muted">Its vault no longer accepts deposits. Start a new loan for new collateral.</p>
          <div className="flex flex-wrap gap-2">
            <Link href={vaultHref("position", loan.vaultId!)} prefetch={false} className={buttonClass("secondary", "no-underline")}>
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
          <Card title="Your deposit is active">
            <p className="mb-4 text-sm text-muted">
              Your Ironwood deposit was proven on Base and zZEC was minted straight into your account&apos;s Morpho position.
            </p>
            <Button onClick={() => setContinueToBorrow(true)}>Continue to borrow</Button>
          </Card>
        )
      ) : null}

      {mounted && phase === "amount" && !loan && inProgress.length > 0 ? (
        <Card title="Continue a loan">
          <ul className="divide-y divide-border">
            {inProgress.map((l) => (
              <li key={l.id} className="flex flex-wrap items-center justify-between gap-3 py-3 text-sm">
                <span>
                  {formatTaz(BigInt(l.amount))}, account <Mono>{short(l.account)}</Mono>
                  {l.vaultId ? null : <span className="ml-2"><Badge tone="warn">vault not opened</Badge></span>}
                </span>
                <span className="flex gap-2">
                  {l.vaultId ? (
                    <Link href={vaultHref("position", l.vaultId)} prefetch={false} className={buttonClass("ghost", "no-underline")}>
                      Position
                    </Link>
                  ) : null}
                  <Button variant="secondary" onClick={() => setLoan(l)}>
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
    <Card title="How much ZEC do you want to lock?">
      <fieldset className="space-y-3" disabled={Boolean(loan) || open.busy}>
        <legend className="sr-only">Collateral amount</legend>
        <div className="grid gap-3 sm:grid-cols-2">
          {config.denominations.map((d) => (
            <label
              key={d.toString()}
              className={cx(
                "flex cursor-pointer items-start gap-3 rounded-lg border p-4",
                amount === d ? "border-accent bg-surface-2" : "border-border hover:border-faint",
              )}
            >
              <input type="radio" name={groupId} checked={amount === d} onChange={() => setAmount(d)} className="mt-1.5" />
              <span>
                <span className="block text-lg font-semibold">{formatTaz(d)}</span>
                <span className="block text-xs text-muted">Standard vault size (everyone uses the same amounts)</span>
                {!openGate(minting.data, d).allowed ? (
                  <span className="block text-xs text-warn">A deposit of this size cannot back a loan right now.</span>
                ) : null}
              </span>
            </label>
          ))}
        </div>
        <p className="text-xs text-muted">
          Fixed sizes are what hide your amount: every vault holds one of these, so your position on Base cannot be told
          apart from the others of the same size. A custom amount would single you out.
        </p>
      </fieldset>
      <ul className="mt-4 list-disc space-y-1 pl-5 text-sm text-muted">
        <li>You will pay exactly this amount from Ironwood notes, plus the network fee.</li>
        <li>
          A fresh owner key and a fresh smart account are created in this browser for this loan only. No wallet
          connection, no ETH.
        </li>
        <li>Opening the vault happens off-chain: nothing touches Base until your deposit is proven there.</li>
        {fee ? <li>{fee}</li> : null}
      </ul>
      {gate.message ? (
        <div className="mt-4">
          <Notice tone={gate.allowed ? "info" : "danger"} title={gate.allowed ? "Your deposit would wait" : "Not now"}>
            <p>{gate.message}</p>
          </Notice>
        </div>
      ) : null}
      {openings ? (
        <div className="mt-4">
          <Notice tone={openingsClosed ? "warn" : "info"} title={openings.title}>
            <p>{openings.body}</p>
          </Notice>
        </div>
      ) : null}
      <div className="mt-4">
        <Notice tone="warn" title="Get your ZEC into Ironwood well before this step">
          <p>
            Moving exactly {formatTaz(amount)} into Ironwood and depositing right away links the two: the move into
            Ironwood is public. Move a different amount, and wait hours or days.
            {config.vaultExpiryBlocks !== null
              ? ` Once open, the vault accepts the deposit for ${blocksToDuration(config.vaultExpiryBlocks)} (${config.vaultExpiryBlocks} Zcash blocks).`
              : null}
          </p>
        </Notice>
      </div>
      <label className="mt-4 flex items-start gap-3 text-sm text-muted">
        <input type="checkbox" checked={ready} onChange={(e) => setReady(e.target.checked)} className="mt-1" />
        <span>
          My ZEC is already in the Ironwood pool, and I deposit in one Ironwood → Ironwood payment (
          <Link href="/prepare">why this matters</Link>).
        </span>
      </label>
      {invitesOn ? (
        <div className="mt-4 space-y-1">
          <label htmlFor={inviteId} className="block text-sm font-medium">
            Invite code
          </label>
          <input
            id={inviteId}
            value={invite}
            onChange={(e) => setInvite(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            className="w-full max-w-sm rounded-lg border border-border bg-bg px-3 py-2 font-mono text-sm text-text focus:border-accent"
          />
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
      <div className="mt-4 flex flex-wrap gap-2">
        <Button
          onClick={() => open.run(loan)}
          busy={open.busy}
          disabled={(!ready && !loan) || !config.ownerSigning || cannotCheck !== null || !gate.allowed || needsInvite || openingsClosed}
        >
          {loan ? "Retry opening the vault" : "Create loan and open vault"}
        </Button>
        {loan ? (
          <Button variant="ghost" onClick={onDiscard} disabled={open.busy}>
            Discard this key and start over
          </Button>
        ) : null}
      </div>
      {open.busy ? (
        <p className="mt-3 text-sm text-muted" aria-live="polite">
          This browser first computes a small proof of work (a few seconds; it keeps vault openings from being
          spammed), then the five signer nodes generate the vault&apos;s threshold key. This can take a little while.
        </p>
      ) : null}
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

// ------------------------------------------------------------------ step 3

/** "2 of 3", or the signers' requirement from GET /config while the vault reports none. */
function confirmationsText(seen: number | undefined): string {
  const needed = config.confirmations;
  if (seen !== undefined) return needed !== null ? `${seen} of ${needed}` : String(seen);
  return needed !== null ? `${needed} needed before the deposit is proven` : "—";
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
  pinned: "Vault address: Ironwood-only, signed by all 5 signers and checked in this browser",
  backend: "Vault address: Ironwood-only, signed by the 5 signers the backend lists (development build: not pinned)",
  unchecked: "Vault address: Ironwood-only, NOT checked against any signer set (development build)",
};

/** The status rows' text, by chain and state: dim once done, bright while current. */
const STATUS_TEXT: Record<Chain, Record<MarkState, string>> = {
  zcash: { done: "text-muted", now: "text-text", todo: "text-faint" },
  base: { done: "text-ink-muted", now: "text-ink", todo: "text-ink-muted" },
};

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
        <span className={cx("text-[15px] leading-[1.45]", STATUS_TEXT[chain][state])}>
          {s.label}
          {state === "done" ? <span className="sr-only"> (done)</span> : null}
          {s.duration && durations.get(s.duration) !== undefined ? (
            <span className={cx("mt-1 block text-[13px]", chain === "zcash" ? "text-muted" : "text-ink-muted")}>
              Typically {formatPeriod(durations.get(s.duration)!)} (measured)
            </span>
          ) : null}
        </span>
      </li>
    );
  };

  return (
    <div className="grid items-start gap-6 min-[900px]:grid-cols-[minmax(0,1fr)_minmax(0,25rem)] [&>*]:min-w-0">
      <Card title={awaiting ? `Pay ${formatTaz(amount)} from Ironwood` : "Your deposit"}>
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
            <Notice tone="ok" title="Deposit received on Zcash" live>
              <p>
                A zero-knowledge proof that your deposit is in a Zcash block is being made and sent to Base; nothing is
                minted on anyone&apos;s word. Do not pay again: a second payment does not back the loan (it can only come back
                as a refund).
              </p>
            </Notice>
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
                It waits a few Zcash blocks before anyone can activate it; the backend&apos;s keeper does, and zZEC is minted
                into your position.{" "}
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
            <div className="space-y-4">
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
              <div className="flex flex-col items-center gap-4 sm:flex-row sm:items-start">
                <Qr value={check.uri} label={`ZIP 321 payment request for ${formatTaz(amount)} to the vault`} size={220} />
                <div className="min-w-0 space-y-3 text-sm">
                  <p className="text-muted">Scan with an Ironwood-capable wallet, or copy the address and amount.</p>
                  <div>
                    <p className={cx("text-sm", vouched === "pinned" ? "text-muted" : "text-warn")}>{VOUCHED[vouched]}</p>
                    <Mono className="block">{loan.vaultAddress}</Mono>
                    <div className="mt-2 flex flex-wrap gap-2">
                      <CopyButton value={loan.vaultAddress!} label="Copy address" />
                      <CopyButton value={check.uri} label="Copy payment URI" />
                    </div>
                  </div>
                  <p>
                    Amount: <strong>{formatTaz(amount)}</strong> exactly, in a single payment.
                  </p>
                </div>
              </div>
              <KeyValue
                rows={[
                  [
                    "Deposit deadline",
                    expiry !== undefined ? (
                      <span>
                        Zcash block <strong>#{expiry.toLocaleString("en-US")}</strong>
                        {blocksLeft !== null ? (
                          <span className="text-muted">
                            , {blocksLeft > 0 ? `${blocksLeft} blocks left (${blocksToDuration(blocksLeft)})` : "already passed"}
                          </span>
                        ) : null}
                      </span>
                    ) : (
                      "—"
                    ),
                  ],
                  ["Vault id", <Mono key="v">{loan.vaultId}</Mono>],
                  ["Confirmations", confirmationsText(vault?.confirmations)],
                ]}
              />
              <Notice tone="warn" title="Pay from notes that have waited in Ironwood">
                <p>
                  Paying right after moving exactly {formatTaz(amount)} into Ironwood links that public move to this loan. If
                  your ZEC is not in Ironwood yet, let this vault expire and start a new loan later.
                </p>
              </Notice>
              <Notice tone="warn" title="Only an exact Ironwood → Ironwood payment counts">
                <p>
                  Paying from transparent, Sapling or Orchard funds, a different amount, or after the deadline does not back the
                  loan: such a payment comes back only as a refund proven on Base, minus the payout&apos;s network fee.
                </p>
              </Notice>
            </div>
          ) : null}
        </div>
      </Card>

      {/* The seam: the deposit's way from Zcash (dark, round marks) through its proof (the π seal) to Base (light, square marks). */}
      <div className="space-y-3">
        <aside aria-labelledby={statusId} className="overflow-hidden rounded-[22px] shadow-[0_0_0_1px_var(--color-border)]">
          <h2 id={statusId} className="sr-only">
            Status
          </h2>
          <div className="bg-bg px-[26px] pt-[26px] pb-11">
            <ChainLabel chain="zcash">On Zcash</ChainLabel>
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
          <div className="paper px-[26px] pt-11 pb-[26px]">
            <ChainLabel chain="base">On Base</ChainLabel>
            <ol start={3} className="mt-[18px] flex flex-col gap-4" aria-live="polite">
              {STATUS_STEPS.slice(2).map((s, i) => statusRow(s, i + 2, "base"))}
            </ol>
            <p className="mt-5 border-t border-rule pt-4 text-[13.5px] leading-[1.55] text-ink-muted">
              Base will see your vault id, the amount, a keyed tag and a later Zcash header. Never your deposit&apos;s block,
              transaction or address.
            </p>
          </div>
        </aside>
        <p className="text-xs text-muted">Updates every 10 seconds. You can close this page and come back from My loans.</p>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ step 4

function BorrowStep({ loan }: { loan: StoredLoan }) {
  const position = usePosition(loan.account, 30_000);
  const [done, setDone] = useState(false);
  return (
    <Card title="Borrow USDC">
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
        <Link
          href={vaultHref("position", loan.vaultId!)}
          prefetch={false}
          className={buttonClass(done ? "primary" : "secondary", "no-underline")}
        >
          Go to your position
        </Link>
      </div>
    </Card>
  );
}

