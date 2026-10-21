"use client";

import Link from "next/link";
import { Badge, Card, KeyValue, Mono, Notice, Stat } from "@/components/ui";
import type { MintBlock, StatusReport, VersionView } from "@/lib/api";
import { formatUtc } from "@/lib/calls";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useEvidence, useProtocol, useStatusReport, useVaultStats } from "@/lib/hooks";
import { short } from "@/lib/keys";
import { formatBps, formatUsdc } from "@/lib/morpho";
import { formatWadPercent } from "@/lib/preliq";
import { anonymitySets, bondNumbers, capNumbers, relayLag, SIGNERS } from "@/lib/public";
import { formatTaz } from "@/lib/zcash";

const MODE_TONE: Record<StatusReport["nodes"][number]["mode"], "ok" | "warn" | "danger"> = {
  normal: "ok",
  offline: "warn",
  unreachable: "warn",
  compromised: "danger",
};

const BLOCK_TEXT: Record<MintBlock, string> = {
  None: "mintable",
  Halted: "halted for good",
  NotBonded: "waits: bonds short",
  Paused: "waits: paused",
  CapExceeded: "waits: cap full",
};

function VersionTable({ kind, list }: { kind: string; list: VersionView[] }) {
  return (
    <div className="space-y-2">
      <h3 className="font-medium capitalize">{kind}</h3>
      {list.length === 0 ? (
        <p className="text-sm text-muted">None.</p>
      ) : (
        <ul className="space-y-2 text-sm">
          {list.map((v) => (
            <li key={v.versionId} className="rounded-lg border border-border p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Mono>{short(v.imageId, 12, 8)}</Mono>
                <Badge tone={v.removed ? "neutral" : v.active ? "ok" : "warn"}>
                  {v.removed ? "Removed" : v.active ? "Active" : `Active from ${formatUtc(v.activeAt)}`}
                </Badge>
              </div>
              <p className="mt-1 text-xs text-muted">
                Router selector <Mono>{v.selector}</Mono>
                {v.branches.length > 0 ? ` · branch ids ${v.branches.map((b) => `0x${b.toString(16)}`).join(", ")}` : ""}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * P-2, the status page: what the signer network and the contracts are doing now, from public
 * sources only (the backend's `GET /status` and `GET /stats/vaults`, and Base through the proxy).
 * Nothing about any vault or user.
 */
export function StatusView() {
  const report = useStatusReport(30_000);
  const stats = useVaultStats(120_000);
  const chain = useProtocol(60_000);
  const evidence = useEvidence();
  const r = report.data;
  const s = chain.data;
  const cap = s ? capNumbers(s) : null;
  const bonds = s?.bonds ? bondNumbers(s.bonds, s.lockedZat, s.price) : null;
  const sets = anonymitySets(config.denominations, stats.data ?? null);
  const lag = r && s ? relayLag(s.relayTip, r.zcashTip) : null;
  const mainnet = DEPLOYMENT.kind === "mainnet-beta";
  // The evidence section of this build's own deployment, if one is published.
  const own = evidence.data?.sections.find((x) => (x.base === "base") === mainnet);
  const commitMatches = own && r ? r.commit.startsWith(own.manifest.commit) || own.manifest.commit.startsWith(r.commit) : null;

  return (
    <div className="space-y-6">
      {report.error ? (
        <Notice tone="warn" title="The backend's status is unavailable" live>
          <p>{report.error}</p>
        </Notice>
      ) : null}
      {chain.error ? (
        <Notice tone="warn" title="Base could not be read" live>
          <p>{chain.error}</p>
        </Notice>
      ) : null}

      <Card title="Signer network">
        {r ? (
          <div className="space-y-4">
            <ul className="flex flex-wrap gap-2">
              {r.nodes.map((n) => (
                <li key={n.index}>
                  <Badge tone={MODE_TONE[n.mode]}>
                    Signer {n.index}: {n.mode}
                  </Badge>
                </li>
              ))}
            </ul>
            <p className="text-xs text-muted">
              Three of the five signers must sign every payout; a signer switched to &quot;compromised&quot; exists only in the
              DEMO panel of the judge deployment.
            </p>
            <KeyValue
              rows={[
                ["Zcash tip (light wallet server)", r.zcashTip ? `#${r.zcashTip.toLocaleString("en-US")}` : "—"],
                [
                  "Zcash header relay on Base",
                  s?.relayTip != null ? (
                    <span key="relay">
                      #{s.relayTip.toLocaleString("en-US")}
                      {lag !== null ? <span className="text-muted">{lag === 0 ? " (level)" : ` (${lag} blocks behind)`}</span> : null}
                    </span>
                  ) : (
                    "—"
                  ),
                ],
                [
                  "Deposit proofs",
                  r.depositProofs
                    ? `${r.depositProofs.running} running, ${r.depositProofs.queued} waiting (at most ${r.depositProofs.concurrency} at once)`
                    : "Not made by this backend",
                ],
              ]}
            />
          </div>
        ) : (
          <p className="text-sm text-muted">{report.loading ? "Reading the backend's status…" : "—"}</p>
        )}
      </Card>

      <Card title="Minting and the coverage cap">
        <div className="space-y-4">
          {r?.minting ? (
            <KeyValue
              rows={[
                [
                  "New minting",
                  r.minting.halted
                    ? "Halted for good (a theft was proven)"
                    : r.minting.paused
                      ? r.minting.pausedUntil > (r.minting.readAt ?? 0)
                        ? `Paused until ${formatUtc(r.minting.pausedUntil)}`
                        : "Paused while a theft case is open"
                      : "Running",
                ],
                ["Signers' bonds", r.minting.fullyBonded ? "All five at the minimum or more" : "Not every signer holds the minimum"],
                [
                  "Per vault size",
                  <ul key="sizes" className="space-y-1">
                    {config.denominations.map((d) => {
                      const b = r.minting!.mintBlock.get(d);
                      return (
                        <li key={d.toString()}>
                          {formatTaz(d)}: {b === null ? "waits: the cap's price is stale" : b ? BLOCK_TEXT[b] : "—"}
                        </li>
                      );
                    })}
                  </ul>,
                ],
              ]}
            />
          ) : (
            <p className="text-sm text-muted">The backend reports no minting gates.</p>
          )}
          {cap ? (
            <dl className="grid gap-3 sm:grid-cols-4">
              <Stat label="Locked" value={formatTaz(cap.lockedZat)} hint={`${s!.openVaults.toString()} open vault(s)`} />
              <Stat label="Pending" value={formatTaz(cap.pendingZat)} hint="activations and refunds" />
              <Stat label="Room" value={cap.roomZat === null ? "—" : formatTaz(cap.roomZat)} hint={cap.roomZat === null ? "the cap's price is stale" : undefined} />
              <Stat label="Cap used" value={cap.fillBps === null ? "—" : formatBps(cap.fillBps)} hint={`absolute cap ${formatTaz(cap.maxTotalZat)}`} />
            </dl>
          ) : null}
          <p className="text-xs text-muted">
            New collateral is accepted only while the signers&apos; bonds cover its value (k ={" "}
            {s ? formatBps(BigInt(s.coverageBps)) : "—"}). A full cap holds new deposits back; it never stops an exit.
          </p>
        </div>
      </Card>

      <Card title="The signers' bonds">
        {bonds ? (
          <div className="space-y-4">
            <dl className="grid gap-3 sm:grid-cols-3">
              <Stat label="ΣB (sum of the bonds)" value={`${formatUsdc(bonds.sum)} USDC`} hint={`${bonds.bonded} of ${SIGNERS} at the ${formatUsdc(bonds.minBond)} USDC minimum`} />
              <Stat label="B_eff (what the cap counts)" value={`${formatUsdc(bonds.effective)} USDC`} hint="min(ΣB, 5/3 × the three smallest)" />
              <Stat
                label="Bonds per dollar locked"
                value={bonds.perCollateralWad === null ? "—" : formatWadPercent(bonds.perCollateralWad)}
                hint="B_eff over the locked collateral at the market price"
              />
            </dl>
            <ul className="grid gap-2 text-sm sm:grid-cols-5">
              {s!.bonds!.each.map((b, i) => (
                <li key={i} className="rounded-lg border border-border p-2">
                  <span className="block text-xs text-faint">Signer {i + 1}</span>
                  <span className="tabular-nums">{formatUsdc(b)} USDC</span>
                </li>
              ))}
            </ul>
            <p className="text-xs text-muted">A slash of a proven theft takes up to {formatUsdc(bonds.totalSlashable)} USDC (bonds and unbonding amounts).</p>
          </div>
        ) : (
          <p className="text-sm text-muted">
            {!config.contractsDeployed
              ? "This build does not pin the deployment's contracts, so it reads nothing from Base."
              : chain.loading
                ? "Reading the bonds…"
                : "No signer bond on this deployment."}
          </p>
        )}
      </Card>

      <Card title="Anonymity sets">
        {sets ? (
          <div className="space-y-3">
            <ul className="grid gap-2 sm:grid-cols-3">
              {sets.map((x) => (
                <li key={x.amount.toString()} className="rounded-lg border border-border p-3">
                  <span className="block text-xs text-faint">{formatTaz(x.amount)} vaults open</span>
                  <span className="text-lg font-semibold tabular-nums">{x.open}</span>
                </li>
              ))}
            </ul>
            <p className="text-xs text-muted">
              Every vault holds one of these sizes, so a position on Base hides among the open vaults of its size.
              {stats.data && !stats.data.complete ? " Still counting: the scan of Base has not reached the latest block." : ""}
              {stats.data?.readAt ? ` Read ${formatUtc(stats.data.readAt)}.` : ""}
            </p>
          </div>
        ) : (
          <p className="text-sm text-muted">{stats.loading ? "Counting the open vaults…" : "This backend publishes no counts."}</p>
        )}
      </Card>

      <Card title="Proof versions">
        {r?.versions ? (
          <div className="grid gap-6 sm:grid-cols-2">
            {Object.entries(r.versions).map(([kind, list]) => (
              <VersionTable key={kind} kind={kind} list={list} />
            ))}
          </div>
        ) : (
          <p className="text-sm text-muted">The backend lists no proof versions.</p>
        )}
        {r?.deployment ? (
          <p className="mt-4 text-xs text-muted">
            Deployed with deposit guest <Mono>{r.deployment.depositImageId ? short(r.deployment.depositImageId, 10, 6) : "—"}</Mono>
            {r.deployment.violationImageId ? (
              <>
                {" "}
                and violation guest <Mono>{short(r.deployment.violationImageId, 10, 6)}</Mono>
              </>
            ) : null}
            .{" "}
            {r.deployment.verifierIsMock ? <Badge tone="warn">MOCK verifier (local fork)</Badge> : null}
          </p>
        ) : null}
      </Card>

      <Card title="The running backend">
        {r ? (
          <KeyValue
            rows={[
              [
                "Commit",
                <span key="c">
                  <Mono>{r.commit}</Mono> {r.treeClean ? null : <Badge tone="warn">uncommitted changes</Badge>}
                </span>,
              ],
              ["Started", r.startedAt ? formatUtc(r.startedAt) : "—"],
              [
                "Published evidence",
                own ? (
                  <span key="e">
                    {commitMatches ? "The evidence manifest names this commit." : `The evidence manifest names ${own.manifest.commit}.`}{" "}
                    <Link href="/evidence">See the evidence</Link>
                  </span>
                ) : (
                  "None published yet."
                ),
              ],
            ]}
          />
        ) : (
          <p className="text-sm text-muted">—</p>
        )}
      </Card>
    </div>
  );
}
