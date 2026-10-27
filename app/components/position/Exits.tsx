"use client";

import { useId, useState, type ReactNode } from "react";
import { getAddress, isAddress, type Hex } from "viem";
import { ActionError } from "@/components/ActionError";
import { DestinationStep } from "@/components/DestinationForm";
import { IconArrowRight, IconCheck, IconRefresh } from "@/components/icons";
import { SplitBar, usdc } from "@/components/loan";
import { TxResult } from "@/components/TxResult";
import { Button, cx, Disclosure, Hint, KeyValue, Mono, Notice } from "@/components/ui";
import { loanAccount } from "@/lib/account";
import { api, type CloseQuote, type Vault } from "@/lib/api";
import { authorizeAdapterCalls, invalidateNonceCalls, releaseCalls, repayAndReleaseCalls, transferUsdcCalls } from "@/lib/calls";
import { formatUtc } from "@/lib/calls";
import { sampleTransactions } from "@/lib/explorer";
import { isNonceUsed, type PositionSnapshot } from "@/lib/chain";
import { CLOSE_TTL_SECONDS, deadlineIn, randomNonce128, type CloseRequestMessage } from "@/lib/eip712";
import { notifyLoansChanged, useAction } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import { loanTokenName } from "@/lib/labels";
import { formatUsdc, parseUsdc } from "@/lib/morpho";
import { formatTaz } from "@/lib/zcash";
import { ChainRef } from "@/components/ChainRef";

/** The borrower accepts up to 1 % less ZEC than quoted (price moves between quote and execution). */
const CLOSE_SLIPPAGE_BPS = 100n;

type ExitKind = "close" | "repay" | "release";

/**
 * Exits from a loan. Each asks for the Ironwood destination first (DestinationStep), then
 * runs its own Base action.
 */
export function Exits({
  loan,
  vault,
  snapshot,
  onChanged,
}: {
  loan: StoredLoan;
  vault: Vault;
  snapshot: PositionSnapshot;
  onChanged: () => void;
}) {
  const hasDebt = snapshot.position.borrowShares > 0n;
  const token = loanTokenName();
  const options: { kind: ExitKind; title: string; body: string }[] = hasDebt
    ? [
        {
          kind: "close",
          title: "Close from collateral (recommended)",
          body: `A market maker repays your debt and takes its value in ZEC from the vault; the rest returns to a new Ironwood address. You keep the ${token} you borrowed, and no outside funds touch your account.`,
        },
        {
          kind: "repay",
          title: `Repay with ${token}, then release`,
          body: `Repay the debt with ${token} held by your loan account and take all your ZEC back. Topping the account up from an exchange or your main wallet links them publicly.`,
        },
      ]
    : [
        {
          kind: "release",
          title: "Release your collateral",
          body: "There is no debt: release the remaining ZEC to a new Ironwood address.",
        },
      ];
  const [kind, setKind] = useState<ExitKind>(options[0]!.kind);
  const [commitment, setCommitment] = useState<Hex | null>(null);
  const groupId = useId();
  const current = options.find((o) => o.kind === kind) ?? options[0]!;

  return (
    <div className="space-y-5">
      <fieldset className="grid gap-2 md:grid-cols-2">
        <legend className="mb-2 text-[14px] font-semibold text-soft">How do you want to {hasDebt ? "close" : "exit"}?</legend>
        {options.map((o) => (
          <label
            key={o.kind}
            className={cx(
              "flex cursor-pointer items-start gap-3 rounded-2xl px-4 py-3.5 transition-[box-shadow,background-color]",
              kind === o.kind
                ? "bg-surface-3 shadow-[inset_0_0_0_2px_var(--color-text)]"
                : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-control)] hover:bg-surface-2",
            )}
          >
            <input
              type="radio"
              name={groupId}
              checked={kind === o.kind}
              onChange={() => {
                setKind(o.kind);
                setCommitment(null);
              }}
              className="mt-1.5"
            />
            <span>
              <span className="block font-bold">{o.title}</span>
              <span className="mt-0.5 block text-[13.5px] text-muted">{o.body}</span>
            </span>
          </label>
        ))}
      </fieldset>

      <ol className="space-y-5">
        <li className="space-y-3 rounded-2xl bg-bg/50 p-5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
          <h3 className="flex items-center gap-2.5 font-bold">
            <span className={cx("inline-flex size-6 items-center justify-center rounded-full text-[12px]", commitment ? "bg-ok/15 text-ok" : "bg-surface-3")}>
              {commitment ? <IconCheck size={14} strokeWidth={2.6} /> : "1"}
            </span>
            Where should your ZEC go?
          </h3>
          {commitment ? (
            <p className="flex flex-wrap items-center gap-2 text-[14px] text-soft">
              <IconCheck size={16} strokeWidth={2.6} className="text-ok" />
              Validated by the signers · Base will only see its salted commitment <Mono>{short(commitment, 8, 6)}</Mono>
              <button type="button" className="min-h-8 font-semibold text-text underline underline-offset-4" onClick={() => setCommitment(null)}>
                Change
              </button>
            </p>
          ) : (
            <DestinationStep loan={loan} vaultId={vault.vaultId} onDone={setCommitment} />
          )}
        </li>
        <li className="space-y-3 rounded-2xl bg-bg/50 p-5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
          <h3 className={cx("flex items-center gap-2.5 font-bold", !commitment && "text-faint")}>
            <span className="inline-flex size-6 items-center justify-center rounded-full bg-surface-3 text-[12px]">2</span>
            {current.title.replace(" (recommended)", "")}
          </h3>
          {commitment ? (
            kind === "close" ? (
              <CloseFlow loan={loan} vault={vault} snapshot={snapshot} commitment={commitment} onChanged={onChanged} />
            ) : kind === "repay" ? (
              <RepayFlow loan={loan} vault={vault} snapshot={snapshot} commitment={commitment} onChanged={onChanged} />
            ) : (
              <ReleaseFlow loan={loan} vault={vault} snapshot={snapshot} commitment={commitment} onChanged={onChanged} />
            )
          ) : (
            <p className="text-sm text-faint">Validate a destination first.</p>
          )}
        </li>
      </ol>
    </div>
  );
}

interface FlowProps {
  loan: StoredLoan;
  vault: Vault;
  snapshot: PositionSnapshot;
  commitment: Hex;
  onChanged: () => void;
}

// ------------------------------------------------------------------ P2 without debt

function ReleaseFlow({ loan, vault, snapshot, commitment, onChanged }: FlowProps) {
  const release = useAction(async () => {
    const r = await loanAccount(loan.ownerKey).send(
      releaseCalls({ vaultId: vault.vaultId, commitment, registryAuthorized: snapshot.registryAuthorized }),
    );
    onChanged();
    return r;
  });
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted">
        This withdraws your remaining {formatTaz(snapshot.position.collateral)} of collateral, burns the receipt and asks
        the signers to pay you on Zcash (minus the network fee).
      </p>
      <Button onClick={() => release.run()} busy={release.busy}>
        Release {formatTaz(snapshot.position.collateral)}
      </Button>
      <ActionError error={release.error} refusal={release.refusal} title="Release failed" />
      {release.result ? <TxResult result={release.result} what="Release requested" /> : null}
    </div>
  );
}

// ------------------------------------------------------------------ P2 with debt (repay)

function RepayFlow({ loan, vault, snapshot, commitment, onChanged }: FlowProps) {
  // Interest keeps accruing until inclusion: the allowance covers 1 % more than the current
  // debt (Morpho pulls only what is owed), and the balance must cover a 0.01 % margin.
  const approve = snapshot.debt + snapshot.debt / 100n + 1n;
  const needed = snapshot.debt + snapshot.debt / 10_000n + 1n;
  const enough = snapshot.usdcBalance >= needed;
  const repay = useAction(async () => {
    const r = await loanAccount(loan.ownerKey).send(
      repayAndReleaseCalls({
        account: loan.account,
        borrowShares: snapshot.position.borrowShares,
        approveAssets: approve,
        vaultId: vault.vaultId,
        commitment,
        registryAuthorized: snapshot.registryAuthorized,
      }),
    );
    onChanged();
    return r;
  });
  return (
    <div className="space-y-3">
      <Hint>
        Topping the loan account up with {loanTokenName()} from an exchange or your main wallet is public and links the two.
        Closing from collateral avoids it.
      </Hint>
      <KeyValue
        rows={[
          ["Debt now", `${formatUsdc(snapshot.debt)} ${loanTokenName()} (+ interest until inclusion)`],
          [`${loanTokenName()} in your loan account`, `${formatUsdc(snapshot.usdcBalance)} ${loanTokenName()}`],
          ["Needed in the account", `${formatUsdc(needed)} ${loanTokenName()}`],
        ]}
      />
      {!enough ? (
        <Notice tone="info" title={`Not enough ${loanTokenName()} in the loan account`}>
          <p>
            Send at least {formatUsdc(needed - snapshot.usdcBalance)} more {loanTokenName()} to <ChainRef kind="base-address" value={loan.account} />, then
            refresh. That transfer is public: send it from an address with no link to you, or close from collateral
            instead.
          </p>
        </Notice>
      ) : null}
      <Button onClick={() => repay.run()} busy={repay.busy} disabled={!enough}>
        Repay {formatUsdc(snapshot.debt)} {loanTokenName()} and release {formatTaz(snapshot.position.collateral)}
      </Button>
      <ActionError error={repay.error} refusal={repay.refusal} title="Repay failed" />
      {repay.result ? <TxResult result={repay.result} what="Repaid and release requested" /> : null}
    </div>
  );
}

// ------------------------------------------------------------------ P6 close from collateral

function CloseFlow({ loan, vault, snapshot, commitment, onChanged }: FlowProps) {
  const [quote, setQuote] = useState<CloseQuote | null>(null);
  const getQuote = useAction(async () => {
    const q = await api.getCloseQuote(vault.vaultId);
    setQuote(q);
    return q;
  });
  const authorize = useAction(async () => {
    const r = await loanAccount(loan.ownerKey).send(authorizeAdapterCalls());
    onChanged();
    return r;
  });
  const submit = useAction(async (q: CloseQuote) => {
    const req: CloseRequestMessage = {
      vaultId: vault.vaultId,
      account: loan.account,
      executor: q.executor,
      marginBps: q.marginBps,
      minUserAmount: (q.userAmount * (10_000n - CLOSE_SLIPPAGE_BPS)) / 10_000n,
      userCommitment: commitment,
      nonce: randomNonce128(),
      deadline: deadlineIn(CLOSE_TTL_SECONDS),
    };
    const signature = await loanAccount(loan.ownerKey).signCloseRequest(req);
    // Remember the request first: if the hand-off fails midway, the user can still cancel it.
    const pending = {
      nonce: req.nonce.toString(),
      deadline: req.deadline.toString(),
      executor: req.executor,
      submittedAt: Date.now(),
    };
    loanStore().update(loan.id, { pendingClose: pending });
    notifyLoansChanged();
    const txHash = await api.submitCloseRequest(req, signature);
    loanStore().update(loan.id, { pendingClose: txHash ? { ...pending, txHash } : pending });
    notifyLoansChanged();
    onChanged();
    return req;
  });

  if (loan.pendingClose) return <PendingClose loan={loan} vault={vault} onChanged={onChanged} />;

  const nowSeconds = Math.floor(Date.now() / 1000);
  const quoteExpired = quote ? quote.expiresAt <= nowSeconds : false;

  return (
    <div className="space-y-3">
      {!snapshot.accountDeployed ? (
        <Notice tone="info" title="Your account is not deployed yet">
          <p>A close request is signed by the account itself (ERC-1271). Borrow first, or release directly if there is no debt.</p>
        </Notice>
      ) : null}
      {snapshot.accountDeployed && !snapshot.adapterAuthorized ? (
        <Notice tone="info" title="Authorize the Settlement Adapter first">
          <p>The adapter needs Morpho authorization to withdraw the collateral when the close executes.</p>
          <Button variant="secondary" onClick={() => authorize.run()} busy={authorize.busy}>
            Authorize (sponsored)
          </Button>
          {authorize.error ? <p className="text-danger">{authorize.error}</p> : null}
        </Notice>
      ) : null}
      {quote ? null : (
        <Button variant="secondary" data-replay="get-quote" onClick={() => getQuote.run()} busy={getQuote.busy}>
          {getQuote.busy ? null : <IconRefresh size={16} />}
          Get a market maker&apos;s quote
        </Button>
      )}
      <ActionError error={getQuote.error} refusal={getQuote.refusal} title="No quote" />
      {quote ? (
        <div className="animate-rise space-y-4">
          <div className="space-y-3 rounded-2xl bg-bg/70 p-5 shadow-[inset_0_0_0_1px_var(--color-border)]">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-[14px] font-semibold">Your {formatTaz(quote.mmAmount + quote.userAmount)} splits in two</p>
              <Button variant="ghost" size="sm" onClick={() => getQuote.run()} busy={getQuote.busy}>
                {getQuote.busy ? null : <IconRefresh size={15} />}
                Refresh
              </Button>
            </div>
            <SplitBar
              mm={quote.mmAmount}
              user={quote.userAmount}
              mmFor={`repays your ${usdc(quote.repayAssets)} ${loanTokenName()} debt on Morpho (margin ${quote.marginBps / 100} %)`}
              userTo="to your new Ironwood address, shielded, less the network fee"
            />
          </div>
          <Disclosure summary="The terms you sign">
            <KeyValue
              rows={[
                ["Minimum you accept", formatTaz((quote.userAmount * (10_000n - CLOSE_SLIPPAGE_BPS)) / 10_000n)],
                ["Market maker", <ChainRef key="e" kind="base-address" value={quote.executor} />],
                ["Quote valid until", formatUtc(quote.expiresAt)],
              ]}
            />
            <p className="text-xs text-muted">
              Signing authorizes only this market maker, this margin and at least the minimum amount, for about 10 minutes. You can
              cancel until it executes.
            </p>
          </Disclosure>
          <Button
            size="lg"
            data-replay="sign-close"
            onClick={() => submit.run(quote)}
            busy={submit.busy}
            disabled={quoteExpired || !snapshot.accountDeployed || !snapshot.adapterAuthorized}
          >
            {submit.busy ? "Signing…" : "Sign and send to the market maker"}
            {submit.busy ? null : <IconArrowRight size={18} />}
          </Button>
          {quoteExpired ? <p className="text-sm text-warn">This quote expired. Refresh it.</p> : null}
        </div>
      ) : null}
      <ActionError error={submit.error} refusal={submit.refusal} title="Close request not sent" />
    </div>
  );
}

export function PendingClose({ loan, vault, onChanged }: { loan: StoredLoan; vault: Vault; onChanged: () => void }) {
  const pending = loan.pendingClose!;
  const nonce = BigInt(pending.nonce);
  const expired = BigInt(Math.floor(Date.now() / 1000)) > BigInt(pending.deadline);
  const executed = vault.status === "closing" || vault.status === "closed" || vault.claims.some((c) => c.kind === "CloseUser");
  const check = useAction(async () => isNonceUsed(loan.account, nonce));
  const cancel = useAction(async () => {
    const r = await loanAccount(loan.ownerKey).send(invalidateNonceCalls({ nonce }));
    if (r.success) {
      loanStore().update(loan.id, { pendingClose: undefined });
      notifyLoansChanged();
    }
    onChanged();
    return r;
  });
  const forget = () => {
    loanStore().update(loan.id, { pendingClose: undefined });
    notifyLoansChanged();
    onChanged();
  };
  return (
    <div className="space-y-3">
      <KeyValue
        rows={[
          [
            "Status",
            executed
              ? "Executed: the close is on Base"
              : pending.txHash
                ? "Sent by the market maker; waiting for Base and the signers"
                : expired
                  ? "Expired without execution"
                  : "Waiting for the market maker",
          ],
          ["Market maker", <ChainRef key="e" kind="base-address" value={pending.executor} />],
          ["Valid until", formatUtc(Number(pending.deadline))],
          ["Nonce", <Mono key="n">{short(pending.nonce, 8, 6)}</Mono>],
          ...(pending.txHash
            ? ([
                [
                  "Market maker's transaction",
                  sampleTransactions() ? <span key="t" className="text-muted">sample data</span> : <ChainRef key="t" kind="base-tx" value={pending.txHash}>{short(pending.txHash, 10, 8)}</ChainRef>,
                ],
              ] as [string, ReactNode][])
            : []),
        ]}
      />
      {executed ? (
        <Notice tone="ok" title="Closed from collateral">
          <p>The debt is repaid and the vault is paying out. The signers now pay your remainder to your Ironwood destination.</p>
        </Notice>
      ) : (
        <div className="flex flex-wrap gap-2">
          {!expired ? (
            <Button variant="danger" onClick={() => cancel.run()} busy={cancel.busy}>
              Cancel this close request
            </Button>
          ) : (
            <Button variant="secondary" onClick={forget}>
              Dismiss and request a new quote
            </Button>
          )}
          <Button variant="ghost" onClick={() => check.run()} busy={check.busy}>
            Check whether it executed
          </Button>
        </div>
      )}
      {check.result !== undefined ? (
        <p className="text-sm text-muted" role="status">
          {check.result ? "Executed or cancelled on Base (its nonce is used)." : "Still open on Base: not executed, not cancelled."}
        </p>
      ) : null}
      <ActionError error={cancel.error} refusal={cancel.refusal} title="Cancel failed" />
      {cancel.result ? <TxResult result={cancel.result} what="Close request cancelled" /> : null}
    </div>
  );
}

// ------------------------------------------------------------------ USDC transfer

export function SendUsdcForm({ loan, snapshot, onChanged }: { loan: StoredLoan; snapshot: PositionSnapshot; onChanged: () => void }) {
  const [to, setTo] = useState("");
  const [amount, setAmount] = useState("");
  const toId = useId();
  const amountId = useId();
  let units: bigint | null = null;
  let amountError: string | null = null;
  if (amount.trim()) {
    try {
      units = parseUsdc(amount);
      if (units === 0n) amountError = "Enter an amount above zero.";
      else if (units > snapshot.usdcBalance) amountError = `The account holds ${formatUsdc(snapshot.usdcBalance)} ${loanTokenName()}.`;
    } catch (e) {
      amountError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  const toValid = isAddress(to.trim(), { strict: false });
  const send = useAction(async () => {
    const r = await loanAccount(loan.ownerKey).send(transferUsdcCalls({ to: getAddress(to.trim()), amount: units! }));
    onChanged();
    return r;
  });
  return (
    <div className="space-y-3">
      <Hint>
        Where it goes is public: sending borrowed {loanTokenName()} straight to an exchange account (KYC) links the loan to your
        identity.
      </Hint>
      <div className="grid gap-3 sm:grid-cols-[1fr_10rem]">
        <div className="space-y-1">
          <label htmlFor={toId} className="block text-sm font-medium">
            Recipient (Base address)
          </label>
          <input
            id={toId}
            value={to}
            onChange={(e) => setTo(e.target.value)}
            placeholder="0x…"
            spellCheck={false}
            autoComplete="off"
            className="field font-mono text-sm"
          />
        </div>
        <div className="space-y-1">
          <label htmlFor={amountId} className="block text-sm font-medium">
            Amount ({loanTokenName()})
          </label>
          <input
            id={amountId}
            value={amount}
            inputMode="decimal"
            onChange={(e) => setAmount(e.target.value)}
            placeholder={formatUsdc(snapshot.usdcBalance)}
            className="field tabular-nums"
          />
        </div>
      </div>
      {amountError ? <p className="text-sm text-danger">{amountError}</p> : null}
      {to.trim() && !toValid ? <p className="text-sm text-danger">Not a Base (EVM) address.</p> : null}
      <Button onClick={() => send.run()} busy={send.busy} disabled={!units || Boolean(amountError) || !toValid}>
        Send {units && !amountError ? `${formatUsdc(units)} ` : ""}
        {loanTokenName()} (sponsored)
      </Button>
      <ActionError error={send.error} refusal={send.refusal} title="Transfer failed" />
      {send.result ? <TxResult result={send.result} what={`${loanTokenName()} sent`} /> : null}
    </div>
  );
}
