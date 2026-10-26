/**
 * REPLAY (DEMO): the judges' build of the app (`NEXT_PUBLIC_REPLAY=1`, a static export with no
 * backend). In it, an in-browser simulator (lib/replay) answers every backend call, every Base read,
 * the loan account's UserOps and the swap's wallet from a recording of the live Base Sepolia run;
 * nothing ever leaves the page. Every other build leaves this false, so no replay path ever runs
 * there: each hook point tests this constant (read with its literal name, so that Next.js inlines
 * the variable when it is set). The simulator's code still ships in a live build: the bundler does
 * not drop it across modules.
 */
export const REPLAY: boolean = process.env.NEXT_PUBLIC_REPLAY === "1";

/**
 * True for an error the replay raised because the recorded run did not take that action at this point (its
 * simulator's `replay_off_script` or `replay_unavailable`, or its wallet's EIP-1193 4200), anywhere in the
 * error's causes: the page then says so as a note, not as a failure. Always false outside a replay build.
 */
export function isReplayRefusal(e: unknown): boolean {
  if (!REPLAY) return false;
  let x: unknown = e;
  for (let i = 0; x && i < 6; i++) {
    const code = (x as { code?: unknown }).code;
    if (code === "replay_off_script" || code === "replay_unavailable" || code === 4200) return true;
    x = (x as { cause?: unknown }).cause;
  }
  return false;
}

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
  "worker-src 'none'",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
].join("; ");
