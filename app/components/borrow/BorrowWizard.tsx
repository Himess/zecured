"use client";

import Link from "next/link";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { ActionError } from "@/components/ActionError";
import { BorrowForm } from "@/components/BorrowForm";
import { clock, CopyButton, CountUp, Qr, useElapsed, useMounted } from "@/components/client";
import { IconArrowRight, IconClock, IconEyeOff, IconShield } from "@/components/icons";
import { KeyBackup } from "@/components/KeyBackup";
import { ProofTrail } from "@/components/ProofTrail";
import { TokenAmount, usd, usdc } from "@/components/loan";
import { UsdcMark } from "@/components/marks";
import { SeamTracker, SignerRing, type TrackerRow } from "@/components/seam";
import { Badge, Button, buttonClass, Card, cx, Disclosure, Hint, Mono, Notice, Steps } from "@/components/ui";
import { apiErrorCode, type Vault } from "@/lib/api";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY } from "@/lib/replay/flag";
import { recording } from "@/lib/replay/recording";
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
import { formatPeriod, openGate, openingFee } from "@/lib/v3";
import { awaitingDeposit, capWait, openingsNotice, openRefusal, refundProofRefusal } from "@/lib/v4";
import { blocksToDuration, formatTaz, formatZec } from "@/lib/zcash";
import { DemoPayment } from "./DemoPayment";

/** The flow in five chapters, as the site tells it; the π of "Prove" turns gold once the deposit is proven. */
const STEPS = ["Vault", "Key", "Deposit", "Prove", "Borrow"];
const PROVE = 3;

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

const reducedMotion = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export function BorrowWizard() {
  const mounted = useMounted();
  const { loans, storageAvailable } = useLoans();
  const [loan, setLoan] = useState<StoredLoan | null>(null);
  const [skippedBackup, setSkippedBackup] = useState(false);
  const [continueToBorrow, setContinueToBorrow] = useState(false);
  const vault = useVault(loan?.vaultId, 10_000);
  const phase = phaseOf(loan, skippedBackup, vault.data);
  // The deposit chapter ends once the signers see the deposit; its proof is the next one's.
  const seen = Boolean(vault.data && (vault.data.depositSeen || vault.data.status !== "awaiting_deposit"));
  const stepIndex =
    phase === "amount" ? 0 : phase === "backup" ? 1 : phase === "deposit" ? (seen ? PROVE : 2) : phase === "borrow" ? (continueToBorrow ? 4 : PROVE) : 5;
  const proven = phase === "borrow" || phase === "done";

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
  // Each chapter starts at the top of the page, its heading focused, so keyboards and screen readers keep their place.
  const heading = useRef<HTMLHeadingElement>(null);
  const first = useRef(true);
  useEffect(() => {
    if (!mounted) return;
    if (first.current) {
      first.current = false;
      return;
    }
    window.scrollTo({ top: 0, behavior: reducedMotion() ? "auto" : "smooth" });
    heading.current?.focus({ preventScroll: true });
  }, [stepIndex, mounted]);

  return (
    <div className="space-y-6">
      <Steps steps={STEPS} current={stepIndex} proof={{ index: PROVE, proven }} />
      <h2 ref={heading} tabIndex={-1} className="sr-only">
        {stepIndex < STEPS.length ? `Step ${stepIndex + 1} of ${STEPS.length}: ${STEPS[stepIndex]}` : "This loan is closed"}
      </h2>
      {mounted && !storageAvailable ? (
        <Notice tone="danger" title="This browser cannot store your loan key">
          <p>Local storage is blocked (private mode or site settings). You can continue, but you must download the key backup before closing this tab.</p>
        </Notice>
      ) : null}

      <div key={stepIndex} className="animate-rise space-y-6">
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
          <Card lead title="Save the key that controls this loan" className="mx-auto max-w-[52rem]">
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
          <Card title="This loan is already closed">
            <p className="mb-4 text-sm text-muted">Its vault no longer accepts deposits. Start a new loan for new collateral.</p>
            <div className="flex flex-wrap gap-2">
              <Link href={vaultHref("position", loan.vaultId!)} prefetch={false} className={buttonClass("secondary")}>
                See its position
              </Link>
              <Button onClick={() => setLoan(null)}>Start a new loan</Button>
            </div>
          </Card>
        ) : null}

        {phase === "borrow" && loan ? continueToBorrow ? <BorrowStep loan={loan} /> : <ProvenStep loan={loan} onContinue={() => setContinueToBorrow(true)} /> : null}
      </div>

      {mounted && phase === "amount" && !loan && inProgress.length > 0 ? (
        <Card title="Continue a loan" icon={<IconClock size={18} />}>
          <ul className="divide-y divide-hairline">
            {inProgress.map((l) => (
              <li key={l.id} className="flex flex-wrap items-center justify-between gap-3 py-3 text-sm">
                <span className="flex flex-wrap items-center gap-3">
                  <TokenAmount token="TAZ" value={formatZec(BigInt(l.amount))} size="sm" />
                  <span className="text-muted">
                    account <Mono>{short(l.account)}</Mono>
                  </span>
                  {l.vaultId ? null : <Badge tone="warn">Vault not opened</Badge>}
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

// ------------------------------------------------------------------ 01 Vault

/** A line of what opening a vault means, with a dot. */
function Fact({ children }: { children: ReactNode }) {
  return (
    <li className="flex gap-2.5 text-[13.5px] text-soft">
      <span aria-hidden="true" className="mt-[7px] size-1.5 flex-none rounded-full bg-pool-soft" />
      <span className="min-w-0">{children}</span>
    </li>
  );
}

type Stage = "work" | "keygen" | "check" | "signed";

const STAGE_TEXT: Record<Stage, string> = {
  work: "This browser computes a small proof of work, which keeps vault openings from being spammed…",
  keygen: "The five signer nodes generate the vault's key together (FROST): each keeps one share, and none ever holds the whole key…",
  check: "This browser checks the five signers' signatures on the vault's Ironwood address…",
  signed: "Signed by all 5 signers, checked in this browser.",
};

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
  // DEMO: the demo wallet pays from its own Ironwood notes, which is what the box asserts.
  const [ready, setReady] = useState(REPLAY);
  const [stage, setStage] = useState<Stage | null>(null);
  // T-01, T-03, R-11: a vault whose deposit would only come back as a refund, or wait for room under the cap, is not opened.
  const minting = useMintingStatus();
  const gate = openGate(minting.data, amount);
  const fee = config.v3 ? openingFee(amount, config.v3) : 0n;
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
    setStage(null);
    // The key is saved *before* the backend is contacted, so a crash never loses it.
    const l = current ?? createLoan(amount);
    if (!current) onLoan(l);
    try {
      const res = await openVault(l, { ...(invitesOn ? { invite } : {}), onStage: setStage });
      // Stored as payable only now that every signer vouches for the address (lib/signers.ts): at once, and the page
      // holds the five signatures in view a moment before the next chapter.
      const opened = {
        ...l,
        vaultId: res.vaultId,
        vaultAddress: res.address,
        expiryHeight: res.expiryHeight,
        tagKeyHash: res.tagKeyHash,
        vaultSignatures: res.signatures,
      };
      loanStore().save(opened);
      notifyLoansChanged();
      setStage("signed");
      await new Promise((r) => setTimeout(r, reducedMotion() ? 300 : 1_400));
      onLoan(opened);
      return res;
    } catch (e) {
      setStage(null);
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
  const single = config.denominations.length === 1;
  const collateral = amount - fee;
  const peers = stats.data?.byAmount.get(amount);
  const working = open.busy || stage === "signed";

  return (
    <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)] [&>*]:min-w-0">
      <Card lead title="How much ZEC do you lock?">
        <fieldset className="space-y-4" disabled={Boolean(loan) || working}>
          <legend className="sr-only">Collateral amount</legend>
          {single ? (
            <div>
              <p className="text-[13px] font-semibold text-muted">Every vault holds</p>
              <div className="mt-2">
                <TokenAmount token="TAZ" value={formatZec(amount)} size="hero" />
              </div>
              <p className="mt-3 text-[14px] text-soft">
                testnet ZEC
                {price !== null ? (
                  <>
                    {" "}
                    · {usd(collateralValue(amount, price))} · borrow up to{" "}
                    <strong className="font-semibold text-text tabular-nums">{usdc(maxBorrowAtTargetLtv(collateral, price))}</strong> {loanTokenName()} at
                    40 %
                  </>
                ) : null}
              </p>
            </div>
          ) : (
            <div className={cx("grid gap-3", config.denominations.length >= 3 ? "md:grid-cols-3" : "sm:grid-cols-2")}>
              {config.denominations.map((d) => {
                const selected = amount === d;
                const blocked = !openGate(minting.data, d).allowed;
                const dFee = config.v3 ? openingFee(d, config.v3) : 0n;
                return (
                  <label
                    key={d.toString()}
                    className={cx(
                      "relative flex cursor-pointer flex-col gap-2.5 rounded-[18px] p-4 transition-[box-shadow,background-color]",
                      selected ? "bg-surface-3 shadow-[inset_0_0_0_2px_var(--color-text)]" : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-control)] hover:bg-surface-2",
                    )}
                  >
                    <input type="radio" name={groupId} checked={selected} onChange={() => setAmount(d)} className="sr-only" />
                    <TokenAmount token="TAZ" value={formatZec(d)} size="lg" />
                    <span className="text-[13px] text-muted">
                      {price !== null ? `${usd(collateralValue(d, price))} · borrow up to ${usdc(maxBorrowAtTargetLtv(d - dFee, price))} ${loanTokenName()}` : "Standard vault size"}
                    </span>
                    {blocked ? <span className="text-xs font-semibold text-warn">A deposit of this size cannot back a loan right now.</span> : null}
                  </label>
                );
              })}
            </div>
          )}
          <Hint icon={<IconEyeOff size={15} />}>
            {peers === undefined
              ? "Fixed sizes hide your amount: on Base, your position looks like every other vault of this size."
              : `Fixed sizes hide your amount: on Base, your position looks like every other vault of this size (${peers === 0 ? "none open yet" : `${peers} open now`}).`}
          </Hint>
        </fieldset>

        <div className="mt-5 space-y-3">
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
        </div>

        <label
          className={cx(
            "mt-5 flex cursor-pointer items-start gap-3 rounded-2xl px-4 py-3.5 text-[14px] transition-colors",
            ready ? "bg-surface-2/80 shadow-[inset_0_0_0_1px_var(--color-soft)]" : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-control)]",
          )}
        >
          <input
            type="checkbox"
            data-replay="ironwood-ready"
            checked={ready}
            disabled={working}
            onChange={(e) => setReady(e.target.checked)}
            className="mt-1 size-4"
          />
          <span>
            <span className="font-semibold text-text">
              {REPLAY ? "DEMO: the demo wallet pays from its own Ironwood notes" : "My ZEC already sits in Ironwood, and I pay in one Ironwood → Ironwood payment."}
            </span>
            <span className="mt-0.5 block text-[13px] text-muted">
              Moving exactly {formatTaz(amount)} into Ironwood right before you pay links the two: that move is public. Move a
              different amount, and wait (<Link href="/prepare">why this matters</Link>).
            </span>
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
        <div className="mt-5 flex flex-wrap items-center gap-3">
          <Button
            size="lg"
            data-replay="open-vault"
            onClick={() => open.run(loan)}
            busy={working}
            disabled={(!ready && !loan) || !config.ownerSigning || cannotCheck !== null || !gate.allowed || needsInvite || openingsClosed}
          >
            {stage === "signed" ? "Vault opened" : open.busy ? "Opening the vault…" : loan ? "Retry opening the vault" : `Open a ${formatTaz(amount)} vault`}
            {working ? null : <IconArrowRight size={18} />}
          </Button>
          {loan && !working ? (
            <Button variant="ghost" onClick={onDiscard}>
              Discard this key and start over
            </Button>
          ) : null}
        </div>
        <div className="mt-4 space-y-3">
          {open.error && refusal ? (
            <Notice tone={refusal.openingsClosed ? "info" : "warn"} title={refusal.title} live>
              <p>{refusal.body}</p>
              <p>Your new key is saved in this browser; you can retry.</p>
            </Notice>
          ) : (
            <ActionError error={open.error} refusal={open.refusal} title="The vault could not be opened">
              {unvouched ? (
                <p>Nothing was kept for this vault. Discard this key and start over; if it happens again, the backend may be compromised.</p>
              ) : alreadyBound ? (
                <p>This key already opened a vault whose details were lost. Discard it and start over with a new key.</p>
              ) : (
                <p>Your new key is saved in this browser; you can retry.</p>
              )}
            </ActionError>
          )}
        </div>
      </Card>

      {/* The vault itself: five signer nodes around it, lit as their signatures arrive. */}
      <aside aria-label="Your vault" className="panel space-y-5 p-6">
        <div className="flex justify-center pt-1">
          <SignerRing state={stage === "signed" ? "signed" : open.busy ? "working" : "idle"} />
        </div>
        <p className="text-center text-[14px] font-semibold" aria-live="polite">
          {stage ? STAGE_TEXT[stage] : "Five signer nodes make its key together; any three move it, none alone."}
        </p>
        <ul className="space-y-2 border-t border-hairline pt-4">
          <Fact>A fresh key and a fresh account, made in this browser, for this loan only: no wallet, no ETH.</Fact>
          <Fact>Nothing touches Base until your deposit is proven there.</Fact>
          {fee > 0n ? <Fact>Opening fee {formatTaz(fee)} (0.5 %), paid to the signers from the vault&apos;s first payout.</Fact> : null}
          {config.vaultExpiryBlocks !== null ? (
            <Fact>
              Pay within {blocksToDuration(config.vaultExpiryBlocks)} ({config.vaultExpiryBlocks} Zcash blocks) of opening it.
            </Fact>
          ) : null}
        </ul>
      </aside>
    </div>
  );
}

// ------------------------------------------------------------------ 03 Deposit and 04 Prove

/** "2 / 3", or the signers' requirement from GET /config while the vault reports none. */
function confirmationsText(seen: number | undefined): string {
  const needed = config.confirmations;
  if (seen !== undefined) return needed !== null ? `${Math.min(seen, needed)} / ${needed}` : String(seen);
  return needed !== null ? `0 / ${needed}` : "—";
}

/** What the address label says about the signers' signatures (C-4). */
const VOUCHED: Record<VaultCheck, string> = {
  pinned: "Ironwood-only, signed by all 5 signers and checked in this browser",
  backend: "Ironwood-only, signed by the 5 signers the backend lists (development build: not pinned)",
  unchecked: "Ironwood-only, NOT checked against any signer set (development build)",
};

/** What Base learns from a deposit, under every tracker. */
const BASE_SEES = (
  <>
    <span className="block">
      <strong className="font-semibold text-ink">Base sees</strong> the vault id, the amount, a keyed tag and a later Zcash header.
    </span>
    <span className="block">
      <strong className="font-semibold text-ink">Never</strong> your deposit&apos;s block, transaction or address.
    </span>
  </>
);

function DepositStep({ loan, vault, vaultError }: { loan: StoredLoan; vault: Vault | null; vaultError: string | null }) {
  const amount = BigInt(loan.amount);
  const check = loan.vaultAddress ? checkVaultAddress(loan.vaultAddress, amount) : null;
  // The stored address is shown, as text or QR code, only while every signer vouches for it.
  const proof = useVaultProof(loan);
  const vouched = proof.state === "vouched" ? proof.check : null;
  // Base: a proven deposit waiting to be activated, or one that came back as a refund (T-02).
  const ledger = useVaultLedger(loan.vaultId, 20_000);
  const l = ledger.data;
  const basePending = Boolean(l?.pending);
  const baseActive = l?.status === "Active" || vault?.status === "active";
  const refunded = Boolean(l && l.claims.some((c) => c.kind === "Refund"));
  // R-12: proven as a refund, its claim not open yet.
  const refundPending = (l?.pendingRefunds?.length ?? 0) > 0;
  // Paying again after the deposit was seen, or after expiry, does not back the loan.
  const awaiting = awaitingDeposit({
    vaultStatus: vault?.status,
    depositSeen: vault?.depositSeen,
    pending: basePending,
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
    pending: basePending,
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
  const seen = Boolean(vault && (vault.status === "deposit_seen" || vault.depositSeen));
  const needed = config.confirmations;
  const confirmed = seen && (needed === null || (vault?.confirmations ?? 0) >= needed);
  // The seal pulses, and says so, while the deposit's proof is being made (not while the coverage cap holds it back).
  const proving = confirmed && !basePending && !baseActive && cap === "none";
  const provingFor = useElapsed(proving);
  const activatingFor = useElapsed(basePending && !baseActive);
  const typicalProof = durations.get("deposit-to-proof");
  const typicalActivation = durations.get("proof-to-activation");

  const zcashRows: TrackerRow[] = [
    { key: "opened", label: "Vault opened, signed by 5 of 5", state: "done" },
    {
      key: "paid",
      label: seen ? `Paid ${formatTaz(amount)}, Ironwood → Ironwood` : `Waiting for your ${formatTaz(amount)}`,
      state: seen ? "done" : awaiting ? "now" : "todo",
    },
    {
      key: "confirmations",
      label: "Confirmed on Zcash",
      detail: seen ? confirmationsText(vault?.confirmations) : undefined,
      state: confirmed || basePending || baseActive ? "done" : seen ? "now" : "todo",
    },
    {
      key: "prove",
      label: "Proven in zero knowledge (RISC Zero)",
      detail: proving ? clock(provingFor) : undefined,
      note: proving && typicalProof !== undefined ? `usually ${formatPeriod(typicalProof)} from the deposit` : undefined,
      state: basePending || baseActive ? "done" : proving ? "now" : "todo",
    },
  ];
  const baseRows: TrackerRow[] = [
    { key: "attest", label: "Proof verified on Base", proof: true, note: "No signer vouches for the deposit", state: basePending || baseActive ? "done" : "todo" },
    {
      key: "activation",
      label: "Activated, a few Zcash blocks later",
      detail: basePending && !baseActive ? clock(activatingFor) : undefined,
      note: basePending && !baseActive && typicalActivation !== undefined ? `usually ${formatPeriod(typicalActivation)}` : undefined,
      state: baseActive ? "done" : basePending ? "now" : "todo",
    },
    { key: "minted", label: "zZEC minted into your Morpho position", state: baseActive ? "done" : "todo" },
  ];
  const seam = basePending || baseActive ? "proven" : proving ? "proving" : "idle";
  const zcashExtra = (
    <>
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
              {vault.nonConforming.length} payment(s) to this vault cannot back the loan. They come back only as refunds proven
              on Base:{" "}
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
    </>
  );
  // One sentence for screen readers, changing only with the stage (the tracker's rows are not announced).
  const stageText = baseActive
    ? "zZEC minted into your Morpho position."
    : basePending
      ? "Proven on Base; activation in a few Zcash blocks."
      : proving
        ? "Confirmed on Zcash; its zero-knowledge proof is being made."
        : seen
          ? "Deposit seen on Zcash; confirmations coming in."
          : "Waiting for your payment.";

  const notices = (
    <>
      {cap !== "none" ? (
        <Notice tone="warn" title={cap === "confirmed" ? "Your deposit waits for room under the coverage cap" : "The coverage cap is full right now"} live>
          <p>
            {cap === "confirmed" ? "Your deposit is on Zcash, but the" : "If you already paid this vault, your deposit is on Zcash, but the"} signers&apos;
            bonds cover no more collateral right now, so its proof cannot land yet. Nothing is lost and the vault stays yours: the
            proof is sent again once there is room, and zZEC is minted then.
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
      {refundPending && !refunded ? (
        <Notice tone="info" title="Your deposit was proven as a refund" live>
          <p>
            Its proof is on Base and waits a few Zcash blocks, like an activation, before its refund opens (pending until then).
            The backend&apos;s keeper opens it; you then give it an Ironwood destination on the{" "}
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
    </>
  );

  // 04 Prove: once the deposit is seen, the crossing is the page.
  if (!awaiting) {
    return (
      <div className="mx-auto max-w-[46rem] space-y-5">
        <p className="sr-only" aria-live="polite">
          {stageText}
        </p>
        <SeamTracker
          title="Your deposit, from Zcash to Base"
          size="hero"
          zcash={zcashRows}
          base={baseRows}
          seam={seam}
          provingLabel={`Proving your deposit · ${clock(provingFor)}`}
          zcashExtra={zcashExtra}
          footer={BASE_SEES}
        />
        {seen && !basePending && !baseActive && cap === "none" ? (
          <Hint>Paid. Do not pay again: a second payment does not back the loan, it only comes back as a refund.</Hint>
        ) : null}
        {basePending && !baseActive ? (
          <Hint>
            Proven on Base. zZEC is minted after a short safety delay (a few more Zcash blocks): our keeper finalizes it, and anyone
            could.{REPLAY ? " The replay runs it in seconds." : ""}
          </Hint>
        ) : null}
        {notices}
        {basePending || baseActive ? (
          <section aria-label="What Base saw" className="space-y-3">
            <h3 className="text-[15px] font-bold">What Base saw</h3>
            <ProofTrail vaultId={loan.vaultId!} account={loan.account} since={loan.createdAt} compact />
          </section>
        ) : null}
        <Hint icon={<IconClock size={15} />}>Updates every 10 seconds. You can close this page and come back from Loans.</Hint>
      </div>
    );
  }

  // 03 Deposit: the payment request, with the tracker beside it.
  const sampleVault = REPLAY && recording().placeholder;
  return (
    <div className="grid items-start gap-6 min-[980px]:grid-cols-[minmax(0,1fr)_minmax(0,24rem)] [&>*]:min-w-0">
      <p className="sr-only" aria-live="polite">
        {stageText}
      </p>
      <Card lead title={`Pay exactly ${formatTaz(amount)} from Ironwood`} action={vault ? <Badge tone="info" dot>Waiting for your payment</Badge> : null}>
        <div className="space-y-5">
          {check && !check.ok ? (
            <Notice tone="danger" title="Do not pay this address">
              {check.problems.map((p) => (
                <p key={p}>{p}</p>
              ))}
            </Notice>
          ) : null}
          {proof.state === "refused" ? (
            <Notice tone="danger" title="Do not pay this vault" live>
              <p>{proof.message}</p>
              <p>The signers do not vouch for this address. Discard this loan and start a new one.</p>
            </Notice>
          ) : null}
          {proof.state === "legacy" ? (
            <Notice tone="danger" title="This vault's address cannot be checked">
              <p>
                It was opened by an earlier version of Zecured, whose vault signatures this app does not check, so nothing shows
                that the signers control it. Do not pay it: let it expire and start a new loan.
              </p>
            </Notice>
          ) : null}
          {proof.state === "checking" ? (
            <p className="text-sm text-muted" aria-live="polite">
              Checking the signers&apos; signatures on this vault…
            </p>
          ) : null}
          {notices}
          {vouched && check?.ok && check.uri ? (
            <>
              {REPLAY ? <DemoPayment loan={loan} /> : null}
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
                    <Qr value={check.uri} label={`ZIP 321 payment request for ${formatTaz(amount)} to the vault`} size={196} />
                  </div>
                  <p className="max-w-[13rem] text-center text-[12px] text-muted">
                    {REPLAY
                      ? sampleVault
                        ? "The replay's sample vault: it accepts nothing. Use “Pay it with demo TAZ”."
                        : "The recorded run's vault: it no longer accepts deposits. Use “Pay it with demo TAZ”."
                      : "Scan with an Ironwood-capable wallet"}
                  </p>
                </div>
                <div className="min-w-0 space-y-4">
                  <div>
                    <p className="text-[12.5px] font-semibold text-muted">Send exactly</p>
                    <div className="mt-1.5">
                      <TokenAmount token="TAZ" value={formatZec(amount)} size="xl" />
                    </div>
                    <p className="mt-1.5 text-[13px] text-muted">in one payment, plus the network fee</p>
                  </div>
                  <div>
                    <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[12.5px] font-semibold text-muted">
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
              {REPLAY ? null : <DemoPayment loan={loan} />}
              <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-muted">
                <span>
                  Vault <Mono>{short(loan.vaultId!, 8, 6)}</Mono>
                </span>
                <CopyButton value={loan.vaultId!} label="Copy id" />
                {expiry !== undefined ? (
                  <span className="tabular-nums">
                    · pay before Zcash block #{expiry.toLocaleString("en-US")}
                    {blocksLeft !== null ? (blocksLeft > 0 ? ` (${blocksToDuration(blocksLeft)} left)` : " (passed)") : ""}
                  </span>
                ) : null}
              </p>
              <Disclosure summary="Before you pay · 2 rules">
                <p>
                  <strong className="text-text">Pay from notes that have waited in Ironwood.</strong> Paying right after moving exactly{" "}
                  {formatTaz(amount)} into Ironwood links that public move to this loan. If your ZEC is not in Ironwood yet, let this
                  vault expire and start a new loan later.
                </p>
                <p>
                  <strong className="text-text">Only an exact Ironwood → Ironwood payment counts.</strong> Paying from transparent,
                  Sapling or Orchard funds, a different amount, or after the deadline does not back the loan: such a payment comes back
                  only as a refund proven on Base, minus the payout&apos;s network fee.
                </p>
              </Disclosure>
            </>
          ) : null}
        </div>
      </Card>

      <div className="space-y-3">
        <SeamTracker title="Your deposit, from Zcash to Base" zcash={zcashRows} base={baseRows} seam={seam} zcashExtra={zcashExtra} footer={BASE_SEES} />
        <Hint icon={<IconClock size={15} />}>Updates every 10 seconds. You can close this page and come back from Loans.</Hint>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ 04 Prove, done: the deposit is active

function ProvenStep({ loan, onContinue }: { loan: StoredLoan; onContinue: () => void }) {
  // The collateral as Morpho holds it: the deposit less the opening fee, if any.
  const position = usePosition(loan.account, 30_000);
  const collateral = position.data?.position.collateral ?? (config.v3 ? BigInt(loan.amount) - openingFee(BigInt(loan.amount), config.v3) : BigInt(loan.amount));
  const done = (key: string, label: string, extra: Partial<TrackerRow> = {}): TrackerRow => ({ key, label, state: "done", ...extra });
  return (
    <div className="space-y-8">
      <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,24rem)] [&>*]:min-w-0">
      <SeamTracker
        title="Your deposit, from Zcash to Base"
        zcash={[
          done("opened", "Vault opened, signed by 5 of 5"),
          done("paid", `Paid ${formatTaz(BigInt(loan.amount))}, Ironwood → Ironwood`),
          done("confirmations", "Confirmed on Zcash"),
          done("prove", "Proven in zero knowledge (RISC Zero)"),
        ]}
        base={[
          done("attest", "Proof verified on Base", { proof: true, note: "No signer vouches for the deposit" }),
          done("activation", "Activated, a few Zcash blocks later"),
          done("minted", "zZEC minted into your Morpho position"),
        ]}
        seam="proven"
        footer={BASE_SEES}
      />
      <div className="panel panel-lead space-y-5 p-6 max-lg:order-first">
        <h3 className="text-[length:clamp(24px,2.4vw,30px)] leading-tight font-extrabold font-stretch-116%">Proven on Base. zZEC minted.</h3>
        <div className="space-y-1.5">
          <TokenAmount token="zZEC" value={<CountUp value={formatZec(collateral)} />} unit="zZEC" size="xl" className="animate-pop" />
          <p className="text-[13px] text-muted">collateral in your Morpho position</p>
        </div>
        <p className="text-[14.5px] text-soft">
          A zero-knowledge proof of your Ironwood deposit was verified on Base, and zZEC was minted straight into your
          account&apos;s Morpho position on that proof alone.
        </p>
        <Button size="lg" className="w-full" data-replay="continue-to-borrow" onClick={onContinue}>
          Borrow against it
          <IconArrowRight size={18} />
        </Button>
      </div>
      </div>
      <section aria-label="What Base saw" className="space-y-3">
        <h3 className="text-[15px] font-bold">What Base saw</h3>
        <ProofTrail vaultId={loan.vaultId!} account={loan.account} since={loan.createdAt} compact />
      </section>
    </div>
  );
}

// ------------------------------------------------------------------ 05 Borrow

function BorrowStep({ loan }: { loan: StoredLoan }) {
  const position = usePosition(loan.account, 30_000);
  const [done, setDone] = useState(false);
  return (
    <Card
      lead
      title={`Borrow ${loanTokenName()}`}
      icon={<UsdcMark size={20} />}
      action={
        <Link href={vaultHref("position", loan.vaultId!)} prefetch={false} className={buttonClass(done ? "primary" : "ghost", undefined, done ? "md" : "sm")}>
          Go to your loan
          <IconArrowRight size={16} />
        </Link>
      }
    >
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
    </Card>
  );
}
