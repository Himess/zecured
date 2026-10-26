"use client";

import Link from "next/link";
import { useEffect, useState, type ReactNode } from "react";
import { isHex, type Hex } from "viem";
import { useMounted } from "./client";
import { IconGlobe, IconKey, IconSigners } from "./icons";
import { BaseMark } from "./marks";
import { ChainChip, Leaky, type Chain } from "./seam";
import { STATUS_LABEL } from "./status";
import { Badge, Mono, Notice } from "./ui";
import { describeError } from "@/lib/api";
import { readMarket, readPrice, readRecentRegistryEvents, type RegistryEvent } from "@/lib/chain";
import { config } from "@/lib/config";
import { useLoans, useVault } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { formatUsdc, formatUsdPrice, type MarketState } from "@/lib/morpho";
import { priceSourceName } from "@/lib/labels";
import { REPLAY } from "@/lib/replay/flag";
import { SWAP_PRIVACY_SENTENCE } from "@/lib/v4";
import { formatTaz } from "@/lib/zcash";

interface PublicData {
  market: MarketState | null;
  price: bigint | null;
  events: RegistryEvent[];
  error: string | null;
  loading: boolean;
}

function usePublicBase(): PublicData {
  const [data, setData] = useState<PublicData>({ market: null, price: null, events: [], error: null, loading: config.contractsDeployed });
  useEffect(() => {
    if (!config.contractsDeployed) return;
    let stop = false;
    Promise.all([readMarket(), readPrice(), readRecentRegistryEvents()])
      .then(([market, price, events]) => !stop && setData({ market, price, events, error: null, loading: false }))
      .catch((e: unknown) => !stop && setData((d) => ({ ...d, error: describeError(e), loading: false })));
    return () => {
      stop = true;
    };
  }, []);
  return data;
}

/** What everyone reads on the two chains, each record with its chain; the leak mark only on what ties it to you. */
const EVERYONE_SEES: { chain: Chain; text: ReactNode }[] = [
  { chain: "zcash", text: "Ironwood → Ironwood transactions, with senders, recipients and amounts encrypted. No pool crossing." },
  { chain: "base", text: "A fresh smart account with no history and no ETH (gas is sponsored)." },
  { chain: "base", text: "A standard-size zZEC position, the USDC borrowed, interest, closes and liquidations." },
  {
    chain: "base",
    text: (
      <>
        Recipients only as salted commitments <code className="text-[0.85em]">keccak256(salt || receiver)</code>.
      </>
    ),
  },
  { chain: "base", text: <Leaky text={SWAP_PRIVACY_SENTENCE} leak="your Base account is visible" /> },
];

const EVERYONE_CANNOT_SEE = [
  "Who owns the account.",
  "Your Zcash wallet, balance or history.",
  "Any Zcash address, transaction id or note on Base.",
];

const PANEL_TITLE = "text-[22px] leading-[1.15] font-extrabold tracking-[-0.006em] font-stretch-118%";

/** Base's side of the page, as a block explorer shows it: light, sharp corners, ruled rows. */
function PublicPanel({ children }: { children: ReactNode }) {
  return (
    <section aria-label="Everyone" className="paper min-w-0 space-y-7 p-5 shadow-[0_30px_60px_-40px_rgb(0_0_0/0.9)] sm:p-7">
      <h2 className={`${PANEL_TITLE} flex items-center justify-between gap-3`}>
        <span className="flex items-center gap-2.5">
          <IconGlobe size={22} /> Everyone
        </span>
        <BaseMark size={20} title="Base" />
      </h2>
      {children}
    </section>
  );
}

/** The shielded side: dark, rounded, no rules. */
function ShieldedPanel({ title, icon, children }: { title: string; icon: ReactNode; children: ReactNode }) {
  return (
    <section aria-label={title} className="panel min-w-0 space-y-6 p-5 sm:p-7">
      <h2 className={`${PANEL_TITLE} flex items-center gap-2.5`}>
        <span className="text-pool-soft">{icon}</span>
        {title}
      </h2>
      {children}
    </section>
  );
}

/** "Sees" and "Cannot see" on the shielded side: round marks, no rules. */
function SeesList({ sees, hidden }: { sees: string[]; hidden: string[] }) {
  return (
    <div className="space-y-5 text-sm">
      {(
        [
          ["Sees", sees],
          ["Cannot see", hidden],
        ] as const
      ).map(([title, items]) => (
        <div key={title}>
          <h3 className="font-semibold text-text">{title}</h3>
          <ul className="mt-2 space-y-2 text-soft">
            {items.map((s) => (
              <li key={s} className="flex gap-2.5">
                <span aria-hidden="true" className="mt-[7px] size-1.5 flex-none rounded-full bg-muted" />
                <span>{s}</span>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

/** A ledger heading on paper: the rows below it hang from an ink rule. */
function LedgerHeading({ children }: { children: ReactNode }) {
  return <h3 className="text-sm font-semibold">{children}</h3>;
}

function fieldValue(event: RegistryEvent, key: string, value: string): ReactNode {
  if (/amount/i.test(key) && /^\d+$/.test(value)) return <Mono>{formatTaz(BigInt(value))}</Mono>;
  if (/^0x[0-9a-fA-F]{40,}$/.test(value)) return <Mono>{short(value, 8, 6)}</Mono>;
  return <Mono>{value}</Mono>;
}

export function WorldView() {
  const mounted = useMounted();
  const { loans } = useLoans();
  const base = usePublicBase();
  const [inspect, setInspect] = useState("");
  const inspectId = isHex(inspect.trim()) && inspect.trim().length === 66 ? (inspect.trim().toLowerCase() as Hex) : undefined;
  const withVault = loans.filter((l) => l.vaultId);

  return (
    <div className="space-y-6">
      <p className="text-[13px] text-muted">
        <mark className="leak">Marked like this</mark>: the one part of a record that ties it to you.
      </p>
      <div className="grid items-start gap-6 lg:grid-cols-3">
        <PublicPanel>
          <div>
            <LedgerHeading>Sees</LedgerHeading>
            <ul className="mt-3 border-t-[1.5px] border-ink">
              {EVERYONE_SEES.map((r, i) => (
                <li key={i} className="grid grid-cols-[4.25rem_minmax(0,1fr)] items-baseline gap-x-3 border-b border-rule py-3 text-sm leading-[1.5]">
                  <ChainChip chain={r.chain} />
                  <span className="min-w-0">{r.text}</span>
                </li>
              ))}
            </ul>
          </div>
          <div>
            <LedgerHeading>Cannot see</LedgerHeading>
            <ul className="mt-3 border-t-[1.5px] border-ink">
              {EVERYONE_CANNOT_SEE.map((s) => (
                <li key={s} className="border-b border-rule py-3 text-sm">
                  {s}
                </li>
              ))}
            </ul>
          </div>
          <div className="space-y-3">
            <LedgerHeading>{REPLAY ? "From the replay's Base Sepolia" : "Live from Base Sepolia"}</LedgerHeading>
            {!config.contractsDeployed ? (
              <p className="text-sm text-ink-muted">Appears once the contracts are deployed.</p>
            ) : base.loading ? (
              <p className="text-sm text-ink-muted">Reading public chain data…</p>
            ) : base.error ? (
              <p className="text-sm">Unavailable: {base.error}</p>
            ) : (
              <>
                <dl className="border-t-[1.5px] border-ink text-sm">
                  {(
                    [
                      ["Supplied", base.market ? `${formatUsdc(base.market.totalSupplyAssets)} USDC` : "—"],
                      ["Borrowed", base.market ? `${formatUsdc(base.market.totalBorrowAssets)} USDC` : "—"],
                      [`ZEC price (${priceSourceName()})`, base.price ? formatUsdPrice(base.price) : "—"],
                    ] as const
                  ).map(([k, v]) => (
                    <div key={k} className="flex items-baseline justify-between gap-4 border-b border-rule py-2.5">
                      <dt className="text-ink-muted">{k}</dt>
                      <dd className="text-right tabular-nums">{v}</dd>
                    </div>
                  ))}
                </dl>
                <LedgerHeading>Recent Zecured events</LedgerHeading>
                {base.events.length === 0 ? (
                  <p className="text-sm text-ink-muted">None in the last ~10,000 blocks.</p>
                ) : (
                  <ul className="border-t-[1.5px] border-ink">
                    {base.events.slice(0, 8).map((e) => (
                      <li key={`${e.transactionHash}-${e.name}-${e.vaultId}`} className="border-b border-rule py-3">
                        <p className="text-[15px] font-semibold break-words">{e.name}</p>
                        <p className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-[13px] text-ink-muted">
                          <span>
                            vault <Mono>{short(e.vaultId, 8, 6)}</Mono>
                          </span>
                          {Object.entries(e.fields).map(([k, v]) => (
                            <span key={k}>
                              {k} {fieldValue(e, k, v)}
                            </span>
                          ))}
                        </p>
                      </li>
                    ))}
                  </ul>
                )}
              </>
            )}
          </div>
        </PublicPanel>

        <ShieldedPanel title="Signer network" icon={<IconSigners size={22} />}>
          <SeesList
            sees={[
              "The vault's notes, balance and timing (they hold its viewing key).",
              "The Ironwood destinations revealed for payouts (receiver and salt).",
              "Payments to a vault that cannot back its loan, until they are refunded.",
            ]}
            hidden={[
              "Your identity, wallet or any ZEC outside the vault.",
              "Your IP address: our backend sees it and logs no IPs, addresses, salts or viewing keys; the app's host sees page loads, never a vault id (it stays in the URL fragment).",
            ]}
          />
          <div className="space-y-3">
            <h3 className="text-sm font-semibold">What they hold about your vaults</h3>
            {!mounted ? null : withVault.length === 0 ? (
              <p className="text-sm text-muted">No vault keys in this browser.</p>
            ) : (
              withVault.map((l) => <SignerView key={l.id} vaultId={l.vaultId!} staggered />)
            )}
            <label className="block text-sm">
              <span className="text-muted">Inspect a vault id</span>
              <input
                value={inspect}
                onChange={(e) => setInspect(e.target.value)}
                placeholder="0x…"
                spellCheck={false}
                autoComplete="off"
                className="mt-1 field font-mono text-xs"
              />
            </label>
            {inspectId ? <SignerView vaultId={inspectId} /> : null}
          </div>
        </ShieldedPanel>

        <ShieldedPanel title="You" icon={<IconKey size={22} />}>
          <SeesList
            sees={[
              "Everything about your loan: the owner key in this browser, the account, the vault address.",
              "Your vault itself, through its viewing key.",
            ]}
            hidden={["Where third-party payouts went: they are encrypted without an outgoing viewing key."]}
          />
          <div className="space-y-3">
            <h3 className="text-sm font-semibold">Held only in this browser</h3>
            {!mounted ? null : loans.length === 0 ? (
              <p className="text-sm text-muted">
                Nothing yet. <Link href="/borrow">Start a loan</Link>.
              </p>
            ) : (
              loans.map((l) => <YouView key={l.id} loan={l} />)
            )}
          </div>
        </ShieldedPanel>
      </div>
      <Notice tone="info" title="Deliberately public">
        <p>
          Like any DeFi position, the loan size, the collateral denomination, interest, closes and liquidations are visible on
          Base, unlinked to you. Testnet volume is low, which keeps the crowd you hide in small.
        </p>
      </Notice>
    </div>
  );
}

/** `staggered` for this browser's own vaults: not all requested in one burst (L-11). */
function SignerView({ vaultId, staggered = false }: { vaultId: Hex; staggered?: boolean }) {
  const vault = useVault(vaultId, 0, { staggered });
  const v = vault.data;
  return (
    <div className="rounded-[14px] bg-bg p-3.5 text-xs">
      <p className="text-muted">
        vault <Mono>{short(vaultId, 8, 6)}</Mono>
      </p>
      {vault.error ? <p className="mt-1 text-warn">Unavailable: {vault.error}</p> : null}
      {v ? (
        <ul className="mt-2 space-y-1 text-soft">
          <li className="flex flex-wrap items-center gap-2">
            Status <Badge tone={STATUS_LABEL[v.status].tone}>{STATUS_LABEL[v.status].text}</Badge>
          </li>
          <li>Amount {formatTaz(v.amount)}</li>
          {v.outstanding !== undefined ? <li>Outstanding {formatTaz(v.outstanding)}</li> : null}
          <li>Deposit deadline: block #{v.expiryHeight.toLocaleString("en-US")}</li>
          <li>Other payments (not collateral): {v.nonConforming.length}</li>
          <li>
            Payout claims: {v.claims.length} ({v.claims.filter((c) => c.status === "paid").length} paid)
          </li>
        </ul>
      ) : null}
    </div>
  );
}

function YouView({ loan }: { loan: StoredLoan }) {
  return (
    <div className="space-y-1 rounded-[14px] bg-bg p-3.5 text-xs text-soft">
      <p>
        Owner key: <span className="text-text">held here (secret)</span>, owner <Mono>{short(loan.owner)}</Mono>
      </p>
      <p>
        Account <Mono>{short(loan.account)}</Mono>
      </p>
      {loan.vaultAddress ? (
        <p>
          Vault address <Mono>{short(loan.vaultAddress, 12, 8)}</Mono>
        </p>
      ) : null}
      {loan.destinationCommitment ? (
        <p>
          Destination commitment <Mono>{short(loan.destinationCommitment, 8, 6)}</Mono> (the address and salt are not kept)
        </p>
      ) : null}
    </div>
  );
}
