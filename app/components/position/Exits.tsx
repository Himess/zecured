"use client";

import { useId, useState, type ReactNode } from "react";
import { getAddress, isAddress, type Hex } from "viem";
import { DestinationStep } from "@/components/DestinationForm";
import { IconArrowRight, IconCheck, IconRefresh } from "@/components/icons";
import { TokenAmount, usdc, zec } from "@/components/loan";
import { TxResult } from "@/components/TxResult";
import { Button, cx, KeyValue, Mono, Notice } from "@/components/ui";
import { loanAccount } from "@/lib/account";
import { api, type CloseQuote, type Vault } from "@/lib/api";
import { authorizeAdapterCalls, invalidateNonceCalls, releaseCalls, repayAndReleaseCalls, transferUsdcCalls } from "@/lib/calls";
import { isNonceUsed, type PositionSnapshot } from "@/lib/chain";
import { CLOSE_TTL_SECONDS, deadlineIn, randomNonce128, type CloseRequestMessage } from "@/lib/eip712";
import { notifyLoansChanged, useAction } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import { loanTokenName } from "@/lib/labels";
import { formatUsdc, parseUsdc } from "@/lib/morpho";
import { formatTaz } from "@/lib/zcash";

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
  const options: { kind: ExitKind; title: string; body: string }[] = hasDebt
    ? [
        {
          kind: "close",
          title: "Close from collateral (recommended)",
          body: "A market maker repays your debt and takes the equivalent ZEC from the vault; the rest returns to a new Ironwood address. No outside funds touch your account.",
        },
        {
          kind: "repay",
          title: "Repay with USDC, then release",
          body: "Repay the debt with USDC held by your loan account and take all your ZEC back. Topping the account up from an exchange or your main wallet links them publicly.",
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
        <legend className="mb-2 text-[14px] font-semibold text-soft">Choose how to exit</legend>
        {options.map((o) => (
          <label
            key={o.kind}
            className={cx(
              "flex cursor-pointer items-start gap-3 rounded-2xl px-4 py-3.5 transition-[box-shadow,background-color]",
              kind === o.kind
                ? "bg-[linear-gradient(180deg,rgb(244_183_40/0.08),transparent)] shadow-[inset_0_0_0_2px_var(--color-accent)]"
                : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-border)] hover:shadow-[inset_0_0_0_1px_var(--color-line)]",
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
            <Notice tone="ok" title="Destination validated by the signers">
              <p>
                Commitment <Mono>{short(commitment, 10, 8)}</Mono>.{" "}
                <button type="button" className="underline" onClick={() => setCommitment(null)}>
                  Change
                </button>
              </p>
            </Notice>
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
      {release.error ? (
        <Notice tone="danger" title="Release failed" live>
          <p>{release.error}</p>
        </Notice>
      ) : null}
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
      <Notice tone="warn" title="Outside USDC can link this account">
        <p>
          If you top up the loan account with USDC from an exchange or your main wallet, that transfer is public and links
          the two. Closing from collateral avoids it.
        </p>
      </Notice>
      <KeyValue
        rows={[
          ["Debt now", `${formatUsdc(snapshot.debt)} USDC (+ interest until inclusion)`],
          ["USDC in your loan account", `${formatUsdc(snapshot.usdcBalance)} USDC`],
          ["Needed in the account", `${formatUsdc(needed)} USDC`],
        ]}
      />
      {!enough ? (
        <Notice tone="info" title="Not enough USDC in the loan account">
          <p>
            Send at least {formatUsdc(needed - snapshot.usdcBalance)} more {loanTokenName()} to <Mono>{loan.account}</Mono>, then
            refresh. That transfer is public: send it from an address with no link to you, or close from collateral
            instead.
          </p>
        </Notice>
      ) : null}
      <Button onClick={() => repay.run()} busy={repay.busy} disabled={!enough}>
        Repay {formatUsdc(snapshot.debt)} USDC and release {formatTaz(snapshot.position.collateral)}
      </Button>
      {repay.error ? (
        <Notice tone="danger" title="Repay failed" live>
          <p>{repay.error}</p>
        </Notice>
      ) : null}
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
      <Button variant="secondary" onClick={() => getQuote.run()} busy={getQuote.busy}>
        {getQuote.busy ? null : <IconRefresh size={16} />}
        {quote ? "Refresh quote" : "Get a market maker's quote"}
      </Button>
      {getQuote.error ? (
        <Notice tone="danger" title="No quote" live>
          <p>{getQuote.error}</p>
        </Notice>
      ) : null}
      {quote ? (
        <div className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-3">
            <div className="rounded-2xl bg-bg/80 p-4 shadow-[inset_0_0_0_1px_var(--color-border)]">
              <p className="text-[12.5px] text-muted">The market maker repays</p>
              <TokenAmount token="USDC" value={usdc(quote.repayAssets)} unit={loanTokenName()} size="lg" />
              <p className="mt-1 text-[12px] text-muted">your whole debt, on Morpho</p>
            </div>
            <div className="rounded-2xl bg-bg/80 p-4 shadow-[inset_0_0_0_1px_var(--color-border)]">
              <p className="text-[12.5px] text-muted">and takes from the vault</p>
              <TokenAmount token="TAZ" value={zec(quote.mmAmount)} size="lg" />
              <p className="mt-1 text-[12px] text-muted">margin {quote.marginBps / 100} %, shielded</p>
            </div>
            <div className="rounded-2xl bg-ok/[0.06] p-4 shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_35%,transparent)]">
              <p className="text-[12.5px] text-muted">You receive</p>
              <TokenAmount token="TAZ" value={zec(quote.userAmount)} size="lg" />
              <p className="mt-1 text-[12px] text-muted">at your new Ironwood address, less the network fee</p>
            </div>
          </div>
          <KeyValue
            rows={[
              ["Minimum you accept", formatTaz((quote.userAmount * (10_000n - CLOSE_SLIPPAGE_BPS)) / 10_000n)],
              ["Executor", <Mono key="e">{quote.executor}</Mono>],
              ["Quote valid until", new Date(quote.expiresAt * 1000).toLocaleTimeString()],
            ]}
          />
          <p className="text-xs text-muted">
            Signing authorizes only this executor, this margin and at least the minimum amount, for about 10 minutes. You can
            cancel until it executes.
          </p>
          <Button
            size="lg"
            onClick={() => submit.run(quote)}
            busy={submit.busy}
            disabled={quoteExpired || !snapshot.accountDeployed || !snapshot.adapterAuthorized}
          >
            Sign and send to the market maker
            {submit.busy ? null : <IconArrowRight size={18} />}
          </Button>
          {quoteExpired ? <p className="text-sm text-warn">This quote expired. Refresh it.</p> : null}
        </div>
      ) : null}
      {submit.error ? (
        <Notice tone="danger" title="Close request not sent" live>
          <p>{submit.error}</p>
        </Notice>
      ) : null}
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
          ["Executor", <Mono key="e">{pending.executor}</Mono>],
          ["Valid until", new Date(Number(pending.deadline) * 1000).toLocaleString()],
          ["Nonce", <Mono key="n">{short(pending.nonce, 8, 6)}</Mono>],
          ...(pending.txHash
            ? ([["Market maker's transaction", <Mono key="t">{pending.txHash}</Mono>]] as [string, ReactNode][])
            : []),
        ]}
      />
      {executed ? (
        <Notice tone="ok" title="Closed from collateral">
          <p>The debt is repaid and the vault is closing. The signers now pay your remainder to your Ironwood destination.</p>
        </Notice>
      ) : (
        <div className="flex flex-wrap gap-2">
          {!expired ? (
            <Button variant="danger" onClick={() => cancel.run()} busy={cancel.busy}>
              Cancel request (invalidate nonce)
            </Button>
          ) : (
            <Button variant="secondary" onClick={forget}>
              Dismiss and request a new quote
            </Button>
          )}
          <Button variant="ghost" onClick={() => check.run()} busy={check.busy}>
            Check nonce on Base
          </Button>
        </div>
      )}
      {check.result !== undefined ? (
        <p className="text-sm text-muted" role="status">
          Nonce {check.result ? "used or cancelled" : "still open"} on the Settlement Adapter.
        </p>
      ) : null}
      {cancel.error ? (
        <Notice tone="danger" title="Cancel failed" live>
          <p>{cancel.error}</p>
        </Notice>
      ) : null}
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
      else if (units > snapshot.usdcBalance) amountError = `The account holds ${formatUsdc(snapshot.usdcBalance)} USDC.`;
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
      <Notice tone="warn" title="Where the USDC goes is public">
        <p>
          Sending borrowed USDC straight to an exchange account (KYC) links the loan to your identity: the transfer from this
          account is public.
        </p>
      </Notice>
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
            Amount (USDC)
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
        Send USDC (sponsored)
      </Button>
      {send.error ? (
        <Notice tone="danger" title="Transfer failed" live>
          <p>{send.error}</p>
        </Notice>
      ) : null}
      {send.result ? <TxResult result={send.result} what="USDC sent" /> : null}
    </div>
  );
}
