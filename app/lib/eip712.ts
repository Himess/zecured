/**
 * EIP-712 domains and types, exactly as docs/INTERFACES.md § 2 (tests pin the type strings
 * against that file and against the Solidity golden vectors).
 *
 * - Registry domain ("ZecuredRegistry", "1", 84532, Registry): DepositAttestation,
 *   PayoutConfirmation, VaultCancellation, VaultOpened (signed by the signer nodes; the app checks
 *   VaultOpened: lib/signers.ts)
 *   and every off-chain owner request (OpenVault, Destination, RefundRequest, ViewKeyRequest).
 * - Adapter domain ("ZecuredSettlementAdapter", "1", 84532, Adapter): CloseRequest, which the
 *   owner signs *through the Safe* (ERC-1271, SafeMessage wrapping; see docs/spikes/SPIKE_C.md).
 */
import {
  hashTypedData,
  type Address,
  type Hex,
  type TypedData,
  type TypedDataDefinition,
  type TypedDataDomain,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { CHAIN_ID } from "./config";

export const REGISTRY_DOMAIN_NAME = "ZecuredRegistry";
export const ADAPTER_DOMAIN_NAME = "ZecuredSettlementAdapter";
export const DOMAIN_VERSION = "1";

export function registryDomain(registry: Address) {
  return { name: REGISTRY_DOMAIN_NAME, version: DOMAIN_VERSION, chainId: CHAIN_ID, verifyingContract: registry } as const;
}

export function adapterDomain(adapter: Address) {
  return { name: ADAPTER_DOMAIN_NAME, version: DOMAIN_VERSION, chainId: CHAIN_ID, verifyingContract: adapter } as const;
}

/** Every Zecured EIP-712 struct (field order matters: it defines the type string). */
export const types = {
  // § 2.2 — signed by signer nodes
  DepositAttestation: [
    { name: "vaultId", type: "bytes32" },
    { name: "account", type: "address" },
    { name: "amount", type: "uint64" },
    { name: "depositTag", type: "bytes32" },
  ],
  PayoutConfirmation: [
    { name: "vaultId", type: "bytes32" },
    { name: "claimIdsHash", type: "bytes32" },
  ],
  VaultCancellation: [{ name: "vaultId", type: "bytes32" }],
  // § 2.3 — signed by the per-loan owner key, verified off-chain by every signer node
  OpenVault: [
    { name: "owner", type: "address" },
    { name: "account", type: "address" },
    { name: "amount", type: "uint64" },
    { name: "requestId", type: "bytes32" },
    { name: "deadline", type: "uint64" },
  ],
  Destination: [
    { name: "vaultId", type: "bytes32" },
    { name: "commitment", type: "bytes32" },
    { name: "nonce", type: "uint64" },
    { name: "deadline", type: "uint64" },
  ],
  RefundRequest: [
    { name: "vaultId", type: "bytes32" },
    { name: "noteId", type: "bytes32" },
    { name: "commitment", type: "bytes32" },
    { name: "nonce", type: "uint64" },
    { name: "deadline", type: "uint64" },
  ],
  ViewKeyRequest: [
    { name: "vaultId", type: "bytes32" },
    { name: "nonce", type: "uint64" },
    { name: "deadline", type: "uint64" },
  ],
  // § 2.4 — signed by the owner through the smart account (ERC-1271)
  CloseRequest: [
    { name: "vaultId", type: "bytes32" },
    { name: "account", type: "address" },
    { name: "executor", type: "address" },
    { name: "marginBps", type: "uint16" },
    { name: "minUserAmount", type: "uint64" },
    { name: "userCommitment", type: "bytes32" },
    { name: "nonce", type: "uint256" },
    { name: "deadline", type: "uint64" },
  ],
  // § 2.5 — signed by every signer node when the vault opens, verified here (C-4)
  VaultOpened: [
    { name: "vaultId", type: "bytes32" },
    { name: "owner", type: "address" },
    { name: "account", type: "address" },
    { name: "amount", type: "uint64" },
    { name: "expiryHeight", type: "uint32" },
    { name: "addressHash", type: "bytes32" },
    { name: "tagKeyHash", type: "bytes32" },
  ],
} as const;

export type PrimaryType = keyof typeof types;

/** `Name(type1 name1,type2 name2,…)` — the EIP-712 `encodeType` of a dependency-free struct. */
export function typeString(primaryType: PrimaryType): string {
  return `${primaryType}(${types[primaryType].map((f) => `${f.type} ${f.name}`).join(",")})`;
}

// ------------------------------------------------------------------ messages

export interface OpenVaultMessage {
  owner: Address;
  account: Address;
  amount: bigint;
  requestId: Hex;
  deadline: bigint;
}
export interface DestinationMessage {
  vaultId: Hex;
  commitment: Hex;
  nonce: bigint;
  deadline: bigint;
}
export interface RefundRequestMessage {
  vaultId: Hex;
  noteId: Hex;
  commitment: Hex;
  nonce: bigint;
  deadline: bigint;
}
export interface ViewKeyRequestMessage {
  vaultId: Hex;
  nonce: bigint;
  deadline: bigint;
}
export interface CloseRequestMessage {
  vaultId: Hex;
  account: Address;
  executor: Address;
  marginBps: number;
  minUserAmount: bigint;
  userCommitment: Hex;
  nonce: bigint;
  deadline: bigint;
}
export interface VaultOpenedMessage {
  vaultId: Hex;
  owner: Address;
  account: Address;
  amount: bigint;
  /** uint32: a Zcash height. */
  expiryHeight: number;
  /** keccak256 of the vault address's UTF-8 bytes. */
  addressHash: Hex;
  /**
   * keccak256 of the vault's tag key K = BLAKE2b-256(personal "Zecured_TagKey__", UFVK string)
   * (VaultOpened v2, lib/signers.ts `tagKeyHash`).
   */
  tagKeyHash: Hex;
}

// Typed-data builders (one per request kind, so every message is fully typed).

export function openVaultTypedData(registry: Address, message: OpenVaultMessage) {
  return { domain: registryDomain(registry), types: { OpenVault: types.OpenVault }, primaryType: "OpenVault", message } as const;
}

export function destinationTypedData(registry: Address, message: DestinationMessage) {
  return {
    domain: registryDomain(registry),
    types: { Destination: types.Destination },
    primaryType: "Destination",
    message,
  } as const;
}

export function refundRequestTypedData(registry: Address, message: RefundRequestMessage) {
  return {
    domain: registryDomain(registry),
    types: { RefundRequest: types.RefundRequest },
    primaryType: "RefundRequest",
    message,
  } as const;
}

export function viewKeyRequestTypedData(registry: Address, message: ViewKeyRequestMessage) {
  return {
    domain: registryDomain(registry),
    types: { ViewKeyRequest: types.ViewKeyRequest },
    primaryType: "ViewKeyRequest",
    message,
  } as const;
}

export function closeRequestTypedData(adapter: Address, message: CloseRequestMessage) {
  return {
    domain: adapterDomain(adapter),
    types: { CloseRequest: types.CloseRequest },
    primaryType: "CloseRequest",
    message,
  } as const;
}

export function vaultOpenedTypedData(registry: Address, message: VaultOpenedMessage) {
  return {
    domain: registryDomain(registry),
    types: { VaultOpened: types.VaultOpened },
    primaryType: "VaultOpened",
    message,
  } as const;
}

/**
 * Signs typed data with the per-loan owner key. Returns the 65-byte `r || s || v` signature
 * (v = 27/28), which is what the signer nodes' `recover_typed` expects.
 */
export async function signWithOwnerKey<
  const typedData extends TypedData | Record<string, unknown>,
  primaryType extends keyof typedData | "EIP712Domain",
>(ownerKey: Hex, typedData: TypedDataDefinition<typedData, primaryType>): Promise<Hex> {
  return privateKeyToAccount(ownerKey).signTypedData(typedData);
}

/** EIP-712 digest of a CloseRequest under the Adapter domain (what the Safe validates). */
export function closeRequestDigest(adapter: Address, req: CloseRequestMessage): Hex {
  return hashTypedData(closeRequestTypedData(adapter, req));
}

// ------------------------------------------------------------------ Safe ERC-1271

/** Safe's own message domain: EIP712Domain(uint256 chainId,address verifyingContract). */
export function safeDomain(account: Address): TypedDataDomain {
  return { chainId: CHAIN_ID, verifyingContract: account };
}

export const SAFE_MESSAGE_TYPES = { SafeMessage: [{ name: "message", type: "bytes" }] } as const;

/** safeHash = EIP-712 hash of SafeMessage(bytes message = digest) under the Safe's domain. */
export function safeMessageHash(account: Address, digest: Hex): Hex {
  return hashTypedData({
    domain: safeDomain(account),
    types: SAFE_MESSAGE_TYPES,
    primaryType: "SafeMessage",
    message: { message: digest },
  });
}

/**
 * ERC-1271 signature of a CloseRequest for a 1-owner Safe 1.4.1 (docs/spikes/SPIKE_C.md,
 * "How the signature must be produced"): the owner signs SafeMessage(digest) under the
 * Safe's domain; 65 bytes r || s || v with v = 27/28. Never ERC-6492-wrapped: the Adapter's
 * SignatureChecker does not unwrap it, so the account must already be deployed.
 */
export async function signCloseRequestAsSafe(
  ownerKey: Hex,
  adapter: Address,
  account: Address,
  req: CloseRequestMessage,
): Promise<Hex> {
  if (req.account.toLowerCase() !== account.toLowerCase()) throw new Error("CloseRequest.account must be the Safe");
  const digest = closeRequestDigest(adapter, req);
  return privateKeyToAccount(ownerKey).signTypedData({
    domain: safeDomain(account),
    types: SAFE_MESSAGE_TYPES,
    primaryType: "SafeMessage",
    message: { message: digest },
  });
}

// ------------------------------------------------------------------ request parameters

/** Owner requests expire after this many seconds (the CLI uses the same value). */
export const REQUEST_TTL_SECONDS = 600n;
/** CloseRequest deadline: short, ≈ 10 min (INTERFACES § 2.4). */
export const CLOSE_TTL_SECONDS = 600n;

export function deadlineIn(seconds: bigint, nowMs: number = Date.now()): bigint {
  return BigInt(Math.floor(nowMs / 1000)) + seconds;
}

/**
 * Random per-vault nonce. Signer nodes accept any value not used before, so a random
 * 48-bit value avoids keeping (and syncing) a counter across devices.
 */
export function randomNonce64(): bigint {
  const b = new Uint8Array(6);
  globalThis.crypto.getRandomValues(b);
  return b.reduce((acc, x) => (acc << 8n) | BigInt(x), 0n) + 1n;
}

/** Random 128-bit CloseRequest nonce (uint256 on-chain; the signers keep it below 2^128). */
export function randomNonce128(): bigint {
  const b = new Uint8Array(16);
  globalThis.crypto.getRandomValues(b);
  return b.reduce((acc, x) => (acc << 8n) | BigInt(x), 0n);
}

/** Random bytes32 (OpenVault.requestId). */
export function randomBytes32(): Hex {
  const b = new Uint8Array(32);
  globalThis.crypto.getRandomValues(b);
  return `0x${Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("")}` as Hex;
}
