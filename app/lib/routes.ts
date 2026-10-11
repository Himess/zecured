/**
 * Links to a vault's pages carry the vault id in the URL fragment (`/position#0x…`), never in
 * the path or the query (security review M-6). Browsers do not send fragments to the server,
 * so the app host's access logs and prefetches cannot tie a visitor's IP to a vault (vault ids
 * are public on Base). Links to these pages also set `prefetch={false}`.
 */
import { isHex, type Hex } from "viem";

export type VaultPage = "position" | "verify" | "refund";

/** `/position#0x…`: the page path without the vault id, the id in the fragment. */
export function vaultHref(page: VaultPage, vaultId: string): string {
  return `/${page}#${vaultId.toLowerCase()}`;
}

/** The vault id in a URL fragment (with or without its "#"), lowercased; null if it is not one. */
export function vaultIdFromFragment(fragment: string): Hex | null {
  const v = fragment.replace(/^#/, "").trim();
  return isHex(v) && v.length === 66 ? (v.toLowerCase() as Hex) : null;
}
