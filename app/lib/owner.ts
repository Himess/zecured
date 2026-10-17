/**
 * The borrower's off-chain requests, signed with the per-loan owner key (docs/INTERFACES.md
 * § 2.3, § 4): open a vault, pre-validate a payout destination (an exit's or a refund's), fetch
 * the viewing key. Each builds the EIP-712 message, signs it in the browser and calls the API.
 * A vault's address and viewing key are accepted only with the signers' signatures (C-4,
 * lib/signers.ts). There is no off-chain refund any more (v3): a deposit that cannot back a loan
 * comes back as a Refund claim on Base, whose destination the account sets after this same
 * pre-validation (lib/calls.ts `refundDestinationCalls`).
 */
import type { Address, Hex } from "viem";
import { api as defaultApi, type Api, type OpenVaultResult } from "./api";
import { prepareDestination, decodeUnifiedAddress, isIronwoodOnly } from "./commitment";
import { config as appConfig, type AppConfig } from "./config";
import { DEPLOYMENT } from "./deployment";
import { signerSet, storedVault, tagKeyHash, verifyVaultOpened, VaultProofError, type VaultCheck } from "./signers";
import { solveStamp, STAMP_DEFAULT_BITS } from "./stamp";
import {
  deadlineIn,
  destinationTypedData,
  openVaultTypedData,
  randomNonce64,
  REQUEST_TTL_SECONDS,
  signWithOwnerKey,
  viewKeyRequestTypedData,
} from "./eip712";
import type { StoredLoan } from "./keys";
import { zip321Uri } from "./zcash";

export interface OwnerContext {
  api?: Api;
  /** Sign for this Registry instead of the configured one (tests). */
  registry?: Address;
  /** Check the signers' signatures against this set as if it were pinned (tests). */
  signers?: readonly Address[];
  config?: AppConfig;
  now?: number;
  /** Compute the open-vault stamp with these zero bits instead of the backend's (tests). */
  stampBits?: number;
}

/** The configuration forbids owner signatures (H-6: pins and backend disagree, or no Registry pin). */
export class OwnerSigningDisabledError extends Error {
  constructor() {
    super(
      "Owner requests are disabled: this build's pinned Registry is missing or disagrees with the backend (see the configuration notice).",
    );
    this.name = "OwnerSigningDisabledError";
  }
}

/** The Registry whose EIP-712 domain owner requests are signed under; never one the backend chose. */
function signingRegistry(ctx: OwnerContext): Address {
  if (ctx.registry) return ctx.registry;
  const c = ctx.config ?? appConfig;
  if (!c.ownerSigning) throw new OwnerSigningDisabledError();
  return c.registry;
}

/**
 * Format checks of the vault address returned by the backend, before anyone pays it (that the
 * signers vouch for it is checked separately: lib/signers.ts).
 */
export interface VaultAddressCheck {
  ok: boolean;
  problems: string[];
  /** The ZIP 321 URI built locally from the checked address and the loan amount. */
  uri: string | null;
}

/**
 * The vault must be an Ironwood-only UA of the deployment's Zcash network (D4: a single
 * Orchard-protocol receiver), and the payment request must be for exactly the loan's denomination.
 */
export function checkVaultAddress(address: string, amount: bigint, backendZip321?: string): VaultAddressCheck {
  const problems: string[] = [];
  let uri: string | null = null;
  try {
    const d = decodeUnifiedAddress(address);
    if (d.network !== DEPLOYMENT.zcashNetwork) {
      problems.push(`The vault address is not a ${DEPLOYMENT.zcashNetwork === "test" ? "testnet" : "mainnet"} address.`);
    }
    if (!isIronwoodOnly(d)) problems.push("The vault address is not Ironwood-only (it must have a single Orchard-protocol receiver).");
    if (d.expiryHeight !== undefined || d.expiryTime !== undefined) problems.push("The vault address carries an expiry.");
    uri = zip321Uri(address.trim(), amount);
    if (backendZip321 !== undefined && backendZip321 !== uri) {
      problems.push("The backend's payment request differs from the one built in this browser; using the local one.");
    }
  } catch (e) {
    problems.push(`The vault address does not decode: ${e instanceof Error ? e.message : "invalid"}.`);
  }
  const fatal = problems.some((p) => !p.startsWith("The backend's payment request differs"));
  return { ok: !fatal, problems, uri: fatal ? null : uri };
}

/** A vault the backend opened, which every signer vouches for. */
export interface OpenedVault extends OpenVaultResult {
  /** How the signers' signatures were checked (lib/signers.ts). */
  check: VaultCheck;
}

/**
 * POST /vaults with a fresh OpenVault signature and the request's hashcash stamp (review H-01:
 * a second or two of work in this browser, with the zero bits `GET /config` reports). Throws
 * `VaultProofError` unless all five signers signed `VaultOpened` for the returned address and
 * tag key and for this request: nothing it returns may be shown or stored as payable before
 * that.
 */
export async function openVault(loan: StoredLoan, ctx: OwnerContext = {}): Promise<OpenedVault> {
  const a = ctx.api ?? defaultApi;
  const c = ctx.config ?? appConfig;
  const registry = signingRegistry(ctx);
  // A build that cannot check the signers' signatures opens no vault (no DKG for nothing).
  if (!ctx.signers) signerSet(c);
  const amount = BigInt(loan.amount);
  // The stamp binds the request id and the owner key: a retry of the same loan finds the same
  // one, which the backend accepts again unless a vault opened with it. Computed first, so the
  // work does not eat into the request's deadline.
  const stamp = await solveStamp(loan.requestId, loan.owner, ctx.stampBits ?? c.openVaultStampBits ?? STAMP_DEFAULT_BITS);
  const deadline = deadlineIn(REQUEST_TTL_SECONDS, ctx.now);
  const message = { owner: loan.owner, account: loan.account, amount, requestId: loan.requestId, deadline };
  const signature = await signWithOwnerKey(loan.ownerKey, openVaultTypedData(registry, message));
  const res = await a.openVault({ ...message, signature, stamp });
  if (res.amount !== amount) throw new Error("The backend opened a vault for a different amount.");
  const check = await verifyVaultOpened(
    {
      vaultId: res.vaultId,
      owner: loan.owner,
      account: loan.account,
      amount,
      expiryHeight: res.expiryHeight,
      address: res.address,
      tagKeyHash: res.tagKeyHash,
    },
    res.signatures,
    { config: c, registry, signers: ctx.signers },
  );
  return { ...res, check };
}

/**
 * Pre-validates the Ironwood destination of an exit (P2 / P6) or of a Refund claim (T-02) with
 * the signers. The app computes the commitment itself and checks that the backend returns the
 * same value. Returns the commitment to use on Base. The salt is sent to the signers only.
 */
export async function submitDestination(
  loan: StoredLoan,
  vaultId: Hex,
  ua: string,
  ctx: OwnerContext = {},
): Promise<{ commitment: Hex; hasOtherReceivers: boolean }> {
  const a = ctx.api ?? defaultApi;
  const registry = signingRegistry(ctx);
  const dest = prepareDestination(ua, DEPLOYMENT.zcashNetwork);
  const nonce = randomNonce64();
  const deadline = deadlineIn(REQUEST_TTL_SECONDS, ctx.now);
  const signature = await signWithOwnerKey(
    loan.ownerKey,
    destinationTypedData(registry, { vaultId, commitment: dest.commitment, nonce, deadline }),
  );
  const returned = await a.postDestination(vaultId, { receiver: dest.ua, salt: dest.saltHex, nonce, deadline, signature });
  if (returned.toLowerCase() !== dest.commitment.toLowerCase()) {
    throw new Error("The signers acknowledged a different commitment than the one computed here. Nothing was sent to Base.");
  }
  return { commitment: dest.commitment, hasOtherReceivers: dest.hasOtherReceivers };
}

/** A vault's viewing key, as the backend returned it. */
export interface ViewingKey {
  ufvk: string;
  /**
   * Its tag key hashes to the `tagKeyHash` all five signers signed when the vault opened. False
   * only for a loan opened before vaults were signed (or under `VaultOpened` v1): nothing this
   * build can check it against.
   */
  checked: boolean;
}

/**
 * The vault's unified full viewing key (owner only). Never stored by the app. Throws
 * `VaultProofError` if it is not the key the signers signed for the vault (C-4).
 */
export async function fetchViewingKey(loan: StoredLoan, vaultId: Hex, ctx: OwnerContext = {}): Promise<ViewingKey> {
  const a = ctx.api ?? defaultApi;
  const registry = signingRegistry(ctx);
  if (loan.vaultId && loan.vaultId.toLowerCase() !== vaultId.toLowerCase()) {
    throw new Error("This loan belongs to another vault.");
  }
  // The stored hash counts only while the signers' signatures over it verify.
  const stored = storedVault(loan);
  if (stored) {
    await verifyVaultOpened(stored.fields, stored.signatures, {
      config: ctx.config ?? appConfig,
      registry,
      signers: ctx.signers,
    });
  }
  const nonce = randomNonce64();
  const deadline = deadlineIn(REQUEST_TTL_SECONDS, ctx.now);
  const signature = await signWithOwnerKey(loan.ownerKey, viewKeyRequestTypedData(registry, { vaultId, nonce, deadline }));
  const ufvk = await a.postViewingKey(vaultId, { nonce, deadline, signature });
  if (!stored) return { ufvk, checked: false };
  if (tagKeyHash(ufvk) !== stored.fields.tagKeyHash.toLowerCase()) {
    throw new VaultProofError(
      "The backend returned a viewing key that the signers did not sign for this vault. It is not shown: do not trust it.",
    );
  }
  return { ufvk, checked: true };
}
