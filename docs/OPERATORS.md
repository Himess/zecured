# Running a signer node

Zecured's security rests on five signer nodes: a vault's ZEC moves only with 3 of 5 FROST
shares, and a deposit is attested on Base only with 3 of 5 signatures. During the hackathon all
five nodes are run by us (see [THREAT_MODEL.md](THREAT_MODEL.md)). This guide is for the next
step: nodes run by independent operators. It describes what a node does, what it trusts, how
to set one up, and what to protect.

## What a node does, and what it checks

A node never acts on the coordinator's word alone. Before every signature it rebuilds the
decision from its own view of both chains:

| Request (from the coordinator) | The node checks |
|---|---|
| DKG for a new vault (`/v1/dkg/round1..3`) | The owner's EIP-712 `OpenVault` signature and deadline, the denomination; round-2 packages travel sealed node-to-node (X25519 + ChaCha20-Poly1305), so the coordinator relays them blind |
| Deposit attestation (`/v1/vaults/:id/attest`) | Policy P1 on its own scan of the vault: an Ironwood-only note of exactly the denomination, before the vault's expiry height, with enough confirmations, first in chain order |
| Destination / refund pre-validation | The owner's signature, nonce and deadline; a valid Ironwood receiver; the refund note is an unspent non-conforming deposit |
| Payout signing (`/v1/sign/commit`, `/v1/sign/share`) | Its own plan from Base claims (Registry events up to Base's `safe` head) and its own ledger (I10), then the PCZT against that plan: inputs, recipients, amounts, fee, change, value balance, `ovk = None` for third parties, expiry window |
| Payout confirmation (`/v1/vaults/:id/confirm`) | The transaction is mined with enough confirmations and spends the vault notes of the payout it signed |

## What a node trusts

- **Its Zcash data source.** A node scans vaults through a light wallet server. A server that
  lies about deposits could get a fake deposit attested. Each operator should use **their own**
  `zebrad` + `lightwalletd` (or at least a server different from the coordinator's), set with
  `NODE_LIGHTWALLETD_URL`, and can add witnesses with `NODE_LIGHTWALLETD_WITNESS_URLS`
  (comma-separated): before attesting, every witness must report the same block hash at the
  deposit's height, the same transaction bytes and a tip within 3 blocks, or the node refuses.
- **Its Base RPC.** Set with `NODE_BASE_RPC_URL`. The node only ingests Registry events at or
  below the `safe` head, drops logs that are not from the Registry or were removed, and checks
  every event against Registry state with `eth_call`. A witness RPC
  (`NODE_BASE_RPC_WITNESS_URL`) must return the same evidence; a disagreement stops ingestion.
- **The network configuration** (`network.json`): the Registry and Adapter addresses, chain id,
  denominations, confirmations, the five nodes' public keys and the coordinator's address. Check
  it against the deployment (the Registry's `signers()` must list the five node addresses). It
  also holds `commit_depth_blocks`, the Base blocks a payout's commitment must be under before any
  share is released (default 3, about 6 s at Base's 2 s blocks; raise it if Base's blocks get
  faster, as its planned Denim upgrade's 200 ms blocks would), and the L23 and D15 random delays.

The coordinator is trusted for liveness only: it schedules work, builds PCZTs and relays
messages. It holds no key share and cannot make a node sign anything outside policy.

## Setup

Requirements: a small Linux server (2 vCPU, 2 GB RAM is plenty: nodes verify, the coordinator
proves), NTP-synchronized time (requests older or newer than 60 s are refused), Rust 1.99.

```bash
cargo build --release -p zecured-node

# 1. Create the node identity: a Base attestation key and an X25519 key.
#    Prints the public part to send to whoever assembles network.json.
./target/release/zecured-node init --index <1..5> --data /var/lib/zecured-node \
    --url https://node-<n>.example.org

# 2. With the shared network.json in hand:
NODE_LIGHTWALLETD_URL=https://your-lightwalletd:443 \
NODE_LIGHTWALLETD_WITNESS_URLS=https://another-lightwalletd:443 \
NODE_BASE_RPC_URL=https://your-base-sepolia-rpc \
NODE_BASE_RPC_WITNESS_URL=https://another-base-sepolia-rpc \
./target/release/zecured-node run --data /var/lib/zecured-node --config network.json \
    --bind 127.0.0.1:9101
```

Put the API behind TLS (a reverse proxy) and allow inbound connections from the coordinator
only. TLS is not optional: payout PCZTs carry recipient addresses in the clear for
verification. Outbound, the node needs only its light wallet server and its Base RPC.

`network.json` must set `coordinator`: nodes accept only requests signed by that address
within ±60 s, each signed request once (D16; `/v1/health` stays open for monitoring). A node
refuses to start without it, unless started with `--insecure-no-auth` (local development only).

Build production nodes **without** the `demo` feature: the threshold demo's "compromised" mode
(signing without policy checks) does not exist in such a build, and a node started with
`--demo-allow-compromise` refuses to run. Don't set `DEMO_ADMIN_TOKEN` either: it enables the
demo panel's switch that takes a node "offline".

The node writes its identity, vault and recipient files with mode 0600 in 0700 directories,
and tightens older files when it opens them.

## What to protect

| File | Contains | If lost | If leaked |
|---|---|---|---|
| `identity.json` | Base attestation key, X25519 secret | Re-deploy the Registry with a new signer set | One of five attestation keys; harmless below the threshold |
| `store/vaults/*.json` | One FROST share per vault | That vault's other four shares still sign (any three) | One share of each vault; harmless below the threshold |

Back both up together, encrypted, and keep the data directory on an encrypted disk. Losing three
nodes' stores freezes the affected vaults; three leaked stores are enough to steal them.

## Operations

- **Health:** `GET /v1/health` → `{ index, mode, vaults }`.
- **Event logs:** Registry events are read with `eth_getLogs` in ranges the provider accepts: up
  to `ZECURED_LOG_RANGE` blocks (default 2,000), halved whenever the provider refuses a range
  (Base Sepolia's public RPC allows 200), so no setting is needed for a stricter provider.
- **Restarts and upgrades:** stop, replace the binary, start. State is on disk; a payout in flight
  survives a restart (the node still recognizes and confirms the transaction it signed).
- **Downtime:** any two nodes may be down; payouts and attestations continue with the other three.
  A node that was down catches up from Base events and its own scan.
- **Cancellations (D23):** a node signs `VaultCancellation` only for the owner's refund request of
  a conforming deposit, only past the vault's deposit window, and only while the Registry (on
  every Base provider it is configured with) has not activated the vault. The deposit becomes
  refundable when the node applies the checked `VaultCancelled` event.
- **Deposit proofs (Tier 2):** a node needs nothing for them; it applies the resulting
  `VaultActivated` like any other. Proving (the coordinator, or the borrower's CLI) and feeding
  the header relay (`scripts/relay-feed.sh`, anyone) are separate, permissionless jobs.

## Not yet supported

- Changing the signer set: the Registry's five signers are immutable, and FROST shares of
  existing vaults cannot be re-shared to a new party yet. Replacing an operator means a new
  deployment for new vaults.
- Bonds and slashing for misbehaving operators are on the roadmap ([TIER2.md](TIER2.md) § 9).
  Base-side verification of deposits is implemented (Tier 2): a MOCK verifier on the local fork,
  real Groth16 seals through RISC Zero's router in the fork tests (`RealGroth16Seal.t.sol`).
