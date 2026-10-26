/**
 * REPLAY (DEMO): the judges' build of the app (`NEXT_PUBLIC_REPLAY=1`, a static export with no
 * backend). In it, an in-browser simulator (lib/replay) answers every backend call, every Base read,
 * the loan account's UserOps and the swap's wallet from a recording of the live Base Sepolia run;
 * nothing ever leaves the page. Every other build leaves this false, so none of the replay code
 * runs there: each hook point tests this constant, which Next.js inlines at build time (the
 * variable is read with its literal name for that reason).
 */
export const REPLAY: boolean = process.env.NEXT_PUBLIC_REPLAY === "1";

/**
 * The backend URL of a replay build. `.invalid` is reserved (RFC 2606), so it can never resolve:
 * the replay's fetch answers every request to it in the page, and a request that somehow escaped
 * the simulator would fail rather than reach anyone.
 */
export const REPLAY_BASE_URL = "https://replay.zecured.invalid";

/**
 * The Content Security Policy of the static export, for a `<meta http-equiv>` tag (a static host
 * sends no headers of ours). `connect-src 'self'`: the replay fetches nothing, not even our backend.
 */
export const REPLAY_CSP = [
  "default-src 'self'",
  "script-src 'self' 'unsafe-inline'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "font-src 'self'",
  "connect-src 'self'",
  "worker-src 'self' blob:",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
].join("; ");
