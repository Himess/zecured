/**
 * REPLAY (DEMO): points at the control the replay's next step needs ("Show me"). Pages mark those controls with
 * `data-replay="<target>"`; the first one shown and usable gets `data-spotlight` (a dashed ice outline, globals.css)
 * and is scrolled into view, until the next click or page. Client-only; nothing here runs outside a replay build.
 */
import type { ReplayStepId } from "@/lib/replay";

/** The controls each manual step needs, in the order the judge meets them. */
export const REPLAY_TARGETS: Partial<Record<ReplayStepId, string[]>> = {
  opened: ["ironwood-ready", "open-vault"],
  "deposit-paid": ["skip-backup", "demo-pay"],
  borrowed: ["continue-to-borrow", "use-recorded-borrow", "borrow"],
  destination: ["demo-destination"],
  "close-executed": ["get-quote", "sign-close"],
  "swap-opened": ["swap-use-amount", "swap-quote", "swap-demo-address", "connect-wallet", "buy"],
};

const ARRIVAL_KEY = "zecured:replay:show";

function usable(el: HTMLElement): boolean {
  if (el.getClientRects().length === 0) return false;
  if (el.matches(":disabled, [aria-disabled='true']")) return false;
  // A box already ticked needs nothing more.
  if (el instanceof HTMLInputElement && el.type === "checkbox" && el.checked) return false;
  return true;
}

let clear: (() => void) | null = null;

/** Spotlights the first usable control of `id`'s targets; false when the page shows none (yet). */
export function spotlight(id: ReplayStepId): boolean {
  if (typeof document === "undefined") return false;
  clear?.();
  for (const target of REPLAY_TARGETS[id] ?? []) {
    const el = [...document.querySelectorAll<HTMLElement>(`[data-replay="${target}"]`)].find(usable);
    if (!el) continue;
    // A checkbox is drawn by its label: light up the whole line.
    const shown = el instanceof HTMLInputElement ? (el.closest("label") ?? el) : el;
    shown.setAttribute("data-spotlight", "");
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    shown.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
    const off = () => {
      shown.removeAttribute("data-spotlight");
      document.removeEventListener("pointerdown", off, true);
      document.removeEventListener("keydown", off, true);
      clear = null;
    };
    document.addEventListener("pointerdown", off, true);
    document.addEventListener("keydown", off, true);
    clear = off;
    return true;
  }
  return false;
}

/** Asks the next page to spotlight `id`'s control once it shows it (a "Go there" or "Start" link). */
export function showOnArrival(id: ReplayStepId): void {
  try {
    sessionStorage.setItem(ARRIVAL_KEY, id);
  } catch {
    // No storage: the judge presses "Show me" there instead.
  }
}

/** The step a link asked this page to spotlight, once (null: none). */
export function takeArrival(): ReplayStepId | null {
  try {
    const id = sessionStorage.getItem(ARRIVAL_KEY);
    sessionStorage.removeItem(ARRIVAL_KEY);
    return id as ReplayStepId | null;
  } catch {
    return null;
  }
}

/** Removes a spotlight still shown (a new page). */
export function clearSpotlight(): void {
  clear?.();
}
