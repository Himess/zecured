"use client";

import { useCallback, useEffect, useId, useState } from "react";
import { isHex, type Hex } from "viem";
import { destinationError, IronwoodAddressInput } from "./DestinationForm";
import { Badge, Button, Card, Mono, Notice } from "./ui";
import { api, describeError, type NodeHealth, type NodeMode } from "@/lib/api";
import { readPrice } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction, useLoans } from "@/lib/hooks";
import { short } from "@/lib/keys";
import { formatUsdPrice } from "@/lib/morpho";
import { formatTaz } from "@/lib/zcash";

const TOKEN_KEY = "zecured:demo-token";
const MODES: { mode: NodeMode; label: string; help: string }[] = [
  { mode: "normal", label: "Honest", help: "Enforces the policy" },
  { mode: "compromised", label: "Compromised", help: "Signs anything (demo)" },
  { mode: "offline", label: "Offline", help: "Does not answer (demo)" },
];

function readToken(): string {
  try {
    return window.sessionStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

function writeToken(token: string | null): void {
  try {
    if (token) window.sessionStorage.setItem(TOKEN_KEY, token);
    else window.sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    // sessionStorage unavailable: the token lives in memory for this page only.
  }
}

export function DemoPanel() {
  const tokenId = useId();
  const [token, setToken] = useState("");
  const [draft, setDraft] = useState("");
  useEffect(() => setToken(readToken()), []);

  return (
    <div className="space-y-6">
      <Notice tone="warn" title="Demo controls">
        <p>
          These endpoints exist for the live demo only and need the admin token. Switching a node to &ldquo;compromised&rdquo;
          makes it sign anything: that is the point of the threshold demonstration.
        </p>
      </Notice>
      <Card title="Admin token">
        {token ? (
          <div className="flex flex-wrap items-center gap-3 text-sm">
            <span className="text-muted">Token set for this tab (session storage).</span>
            <Button
              variant="ghost"
              onClick={() => {
                writeToken(null);
                setToken("");
              }}
            >
              Forget token
            </Button>
          </div>
        ) : (
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              writeToken(draft.trim());
              setToken(draft.trim());
              setDraft("");
            }}
          >
            <div className="grow space-y-1">
              <label htmlFor={tokenId} className="block text-sm font-medium">
                DEMO_ADMIN_TOKEN
              </label>
              <input
                id={tokenId}
                type="password"
                autoComplete="off"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                className="w-full rounded-lg border border-border bg-bg px-3 py-2 text-sm text-text focus:border-accent"
              />
            </div>
            <Button type="submit" disabled={!draft.trim()}>
              Use token
            </Button>
          </form>
        )}
      </Card>

      {token ? (
        <>
          <NodesCard token={token} />
          <SweepCard token={token} />
        </>
      ) : null}

      <div className="grid gap-4 md:grid-cols-2">
        <OracleCard />
        <Card title="Bots">
          <p className="text-sm text-muted">
            Placeholder. The liquidator and market-maker bots (bots/) will report their status here: watched positions,
            last quote, last liquidation.
          </p>
        </Card>
      </div>
    </div>
  );
}

function NodesCard({ token }: { token: string }) {
  const [nodes, setNodes] = useState<NodeHealth[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      setNodes(await api.adminNodes(token));
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, [token]);
  useEffect(() => {
    void load();
  }, [load]);
  const setMode = useAction(async (index: number, mode: NodeMode) => {
    await api.adminSetMode(token, index, mode);
    await load();
  });

  return (
    <Card
      title="Signer nodes"
      action={
        <Button variant="ghost" onClick={() => void load()}>
          Refresh
        </Button>
      }
    >
      {error ? (
        <Notice tone="danger" title="Nodes unavailable" live>
          <p>{error}</p>
        </Notice>
      ) : null}
      {!nodes && !error ? <p className="text-sm text-muted">Loading…</p> : null}
      {nodes ? (
        <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {nodes.map((n) => (
            <li key={n.index} className="space-y-3 rounded-lg border border-border bg-surface-2 p-3">
              <div className="flex items-center justify-between">
                <span className="font-semibold">Node {n.index}</span>
                <Badge tone={n.mode === "normal" ? "ok" : n.mode === "compromised" ? "danger" : "warn"}>{n.mode}</Badge>
              </div>
              {n.vaults !== undefined ? <p className="text-xs text-muted">{n.vaults} vault share(s)</p> : null}
              <div className="flex flex-wrap gap-1" role="group" aria-label={`Mode of node ${n.index}`}>
                {MODES.map((m) => (
                  <Button
                    key={m.mode}
                    variant={n.mode === m.mode ? "primary" : "secondary"}
                    className="min-h-9 px-2 text-xs"
                    aria-pressed={n.mode === m.mode}
                    disabled={n.mode === "unreachable" || setMode.busy}
                    onClick={() => setMode.run(n.index, m.mode)}
                  >
                    {m.label}
                  </Button>
                ))}
              </div>
            </li>
          ))}
        </ul>
      ) : null}
      <p className="mt-3 text-xs text-muted">
        {MODES.map((m) => `${m.label}: ${m.help.toLowerCase()}.`).join(" ")}
      </p>
      {setMode.error ? <p className="mt-3 text-sm text-danger">{setMode.error}</p> : null}
    </Card>
  );
}

function SweepCard({ token }: { token: string }) {
  const { loans } = useLoans();
  const vaultInputId = useId();
  const [vaultId, setVaultId] = useState("");
  const [to, setTo] = useState("");
  const validVault = isHex(vaultId.trim()) && vaultId.trim().length === 66;
  const drain = useAction(async () => api.adminDrain(token, vaultId.trim().toLowerCase() as Hex, to.trim()));
  const report = drain.result;
  const signed = report?.votes.filter((v) => v.signed).length ?? 0;

  return (
    <Card title="Threshold demo: try to sweep a vault">
      <p className="mb-4 text-sm text-muted">
        The coordinator builds a transaction moving everything in the vault to an arbitrary address and asks every node to
        sign it. Honest nodes refuse because it matches no payout plan. Nothing is ever broadcast.
      </p>
      <div className="space-y-4">
        <div className="space-y-1">
          <label htmlFor={vaultInputId} className="block text-sm font-medium">
            Vault id
          </label>
          <input
            id={vaultInputId}
            value={vaultId}
            onChange={(e) => setVaultId(e.target.value)}
            list={`${vaultInputId}-list`}
            placeholder="0x…"
            spellCheck={false}
            autoComplete="off"
            className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-xs text-text focus:border-accent"
          />
          <datalist id={`${vaultInputId}-list`}>
            {loans
              .filter((l) => l.vaultId)
              .map((l) => (
                <option key={l.id} value={l.vaultId} />
              ))}
          </datalist>
        </div>
        <IronwoodAddressInput
          value={to}
          onChange={setTo}
          label="Attacker's address (any testnet unified address)"
          hint="Where the sweep would send the vault's ZEC. Honest signers refuse to sign it."
        />
        <Button
          variant="danger"
          onClick={() => drain.run()}
          busy={drain.busy}
          disabled={!validVault || !to.trim() || destinationError(to) !== null}
        >
          Attempt sweep (never broadcast)
        </Button>
        {drain.busy ? (
          <p className="text-sm text-muted" aria-live="polite">
            Building and proving the sweep transaction, then collecting votes…
          </p>
        ) : null}
        {drain.error ? (
          <Notice tone="danger" title="Demo failed" live>
            <p>{drain.error}</p>
          </Notice>
        ) : null}
        {report ? (
          <div className="space-y-3" aria-live="polite">
            <Notice
              tone={report.thresholdReached ? "danger" : "ok"}
              title={
                report.thresholdReached
                  ? `${signed} of ${report.votes.length} nodes would sign: threshold reached`
                  : `${signed} of ${report.votes.length} nodes would sign: below the threshold of 3, nothing moves`
              }
              live
            >
              <p>
                Sweep of {formatTaz(report.amount)} from vault <Mono>{short(report.vaultId, 10, 6)}</Mono>.
                {report.thresholdReached ? " Three colluding signers can move funds: that is the stated trust assumption." : null}
              </p>
            </Notice>
            <ul className="space-y-2 text-sm">
              {report.votes.map((v) => (
                <li key={v.index} className="flex flex-wrap items-start gap-2 rounded-lg border border-border p-3">
                  <span className="font-medium">Node {v.index}</span>
                  <Badge tone={v.signed ? "danger" : "ok"}>{v.signed ? "signed" : "refused"}</Badge>
                  {v.reason ? <span className="w-full text-xs break-words text-muted">{v.reason}</span> : null}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>
    </Card>
  );
}

function OracleCard() {
  const [price, setPrice] = useState<string | null>(null);
  useEffect(() => {
    if (!config.contractsDeployed) return;
    readPrice()
      .then((p) => setPrice(formatUsdPrice(p)))
      .catch(() => setPrice(null));
  }, []);
  return (
    <Card title="Oracle price (MockZecOracle)">
      <p className="text-2xl font-semibold tabular-nums">{price ?? "—"}</p>
      <p className="mt-2 text-sm text-muted">
        Placeholder. The price is a mock set by its owner with <Mono>setPrice</Mono>; drop it to demonstrate a liquidation.
        The owner key never touches the browser, so this panel will call a backend demo endpoint once it exists.
      </p>
    </Card>
  );
}
