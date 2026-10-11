/**
 * What the five signers vouch for when a vault opens (security review C-4).
 *
 * At the end of the DKG every signer node signs `VaultOpened` (docs/INTERFACES.md § 2.5) with
 * its attestation key: the vault id, the owner's request (owner, account, amount), the deposit
 * expiry height, and the hashes of the vault's address and viewing key as that node derived
 * them. The backend only relays the signatures, so it cannot hand out an address of its own (nor,
 * later, a viewing key to match it). The app shows a vault's address, as text or as a QR code,
 * only once all five signatures verify against the signer set pinned in this build
 * (`NEXT_PUBLIC_SIGNERS`, never a set the backend reports), and shows a viewing key only if it
 * hashes to the `ufvkHash` they signed.
 *
 * A development build without `NEXT_PUBLIC_SIGNERS` falls back to the signer set the backend
 * reports (or, without one, to no check at all) and says so; a production build refuses.
 */
import { isAddressEqual, keccak256, recoverTypedDataAddress, stringToBytes, type Address, type Hex } from "viem";
import type { VaultSignature } from "./api";
import { config as appConfig, SIGNER_COUNT, type AppConfig } from "./config";
import { vaultOpenedTypedData, type VaultOpenedMessage } from "./eip712";
import type { StoredLoan } from "./keys";

/** keccak256 of a string's UTF-8 bytes (`VaultOpened.addressHash`, `.ufvkHash`); never read as hex. */
export function textHash(text: string): Hex {
  return keccak256(stringToBytes(text));
}

/** What the signers sign about a vault, with the address itself rather than its hash. */
export interface VaultOpenedFields {
  vaultId: Hex;
  owner: Address;
  account: Address;
  /** zatoshi */
  amount: bigint;
  expiryHeight: number;
  /** The vault's unified address, exactly as the backend returned it. */
  address: string;
  ufvkHash: Hex;
}

export function vaultOpenedMessage(f: VaultOpenedFields): VaultOpenedMessage {
  return {
    vaultId: f.vaultId,
    owner: f.owner,
    account: f.account,
    amount: f.amount,
    expiryHeight: f.expiryHeight,
    addressHash: textHash(f.address),
    ufvkHash: f.ufvkHash,
  };
}

/**
 * How a vault's signatures were checked:
 * - `pinned`: all five against this build's `NEXT_PUBLIC_SIGNERS`;
 * - `backend`: development build without that pin, against the signer set the backend reports
 *   (no protection against a compromised backend);
 * - `unchecked`: development build without any signer set: not checked at all.
 */
export type VaultCheck = "pinned" | "backend" | "unchecked";

/** The signers do not vouch for the vault, or this build cannot check that they do. */
export class VaultProofError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "VaultProofError";
  }
}

const DO_NOT_PAY = "Do not pay any address shown for this vault.";

/** The signer set vaults are checked against; throws in a production build without the pin. */
export function signerSet(c: AppConfig = appConfig): { signers: readonly Address[] | null; check: VaultCheck } {
  if (c.signersPinned && c.signers) return { signers: c.signers, check: "pinned" };
  if (!c.development) {
    throw new VaultProofError(
      "This build does not pin the signer set (NEXT_PUBLIC_SIGNERS), so it cannot check that the signers vouch for a vault: opening and paying vaults is disabled.",
    );
  }
  return c.signers ? { signers: c.signers, check: "backend" } : { signers: null, check: "unchecked" };
}

/** Why this build cannot check new vaults, or null when it can. */
export function vaultCheckUnavailable(c: AppConfig = appConfig): string | null {
  try {
    signerSet(c);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : "The signer set is unavailable.";
  }
}

/**
 * Checks that every address of `signers` signed `message`, once. The app pins addresses, not
 * node indices, so the signers are compared as a set; `index` only names them in errors.
 */
export async function checkSignatures(
  registry: Address,
  message: VaultOpenedMessage,
  signatures: readonly VaultSignature[],
  signers: readonly Address[],
): Promise<void> {
  const typedData = vaultOpenedTypedData(registry, message);
  const seen: Address[] = [];
  for (const s of signatures) {
    let signer: Address;
    try {
      signer = await recoverTypedDataAddress({ ...typedData, signature: s.signature });
    } catch {
      throw new VaultProofError(`Signer ${s.index}'s signature on this vault is malformed. ${DO_NOT_PAY}`);
    }
    if (!signers.some((a) => isAddressEqual(a, signer))) {
      throw new VaultProofError(
        `Signer ${s.index}'s signature is not from the signer set pinned in this build, or not for this vault's address, viewing key, request and deadline. ${DO_NOT_PAY}`,
      );
    }
    if (seen.some((a) => isAddressEqual(a, signer))) {
      throw new VaultProofError(`One signer's signature is listed twice. ${DO_NOT_PAY}`);
    }
    seen.push(signer);
  }
  if (seen.length !== signers.length) {
    throw new VaultProofError(`Only ${seen.length} of the ${signers.length} signers vouch for this vault. ${DO_NOT_PAY}`);
  }
}

export interface CheckOptions {
  config?: AppConfig;
  /** The Registry whose EIP-712 domain the signers sign under (default: the configured one). */
  registry?: Address;
  /** Check against this set as if it were pinned (tests). */
  signers?: readonly Address[];
}

/**
 * Checks the signers' `VaultOpened` signatures for a vault (one the backend just opened, or a
 * stored loan's). Throws `VaultProofError` unless its address may be shown and paid; returns how
 * the check was made.
 */
export async function verifyVaultOpened(
  fields: VaultOpenedFields,
  signatures: readonly VaultSignature[],
  opts: CheckOptions = {},
): Promise<VaultCheck> {
  const c = opts.config ?? appConfig;
  const set = opts.signers ? { signers: opts.signers, check: "pinned" as const } : signerSet(c);
  if (!set.signers) return "unchecked";
  if (set.signers.length !== SIGNER_COUNT) {
    throw new VaultProofError(`The signer set has ${set.signers.length} addresses instead of ${SIGNER_COUNT}. ${DO_NOT_PAY}`);
  }
  await checkSignatures(opts.registry ?? c.registry, vaultOpenedMessage(fields), signatures, set.signers);
  return set.check;
}

/**
 * What a loan stored about its vault when it opened: the signed fields and the signatures.
 * Null when it has no vault, or for a loan opened before the signers signed vaults.
 */
export function storedVault(loan: StoredLoan): { fields: VaultOpenedFields; signatures: VaultSignature[] } | null {
  const { vaultId, vaultAddress, expiryHeight, ufvkHash, vaultSignatures } = loan;
  if (!vaultId || !vaultAddress || expiryHeight === undefined || !ufvkHash || !vaultSignatures) return null;
  return {
    fields: {
      vaultId,
      owner: loan.owner,
      account: loan.account,
      amount: BigInt(loan.amount),
      expiryHeight,
      address: vaultAddress,
      ufvkHash,
    },
    signatures: vaultSignatures,
  };
}
