import { describe, expect, it } from "vitest";
import { createStagger } from "./hooks";

describe("createStagger (L-11: a browser's vaults are not fetched in one burst)", () => {
  it("gives each request its own slot, at least the minimum gap apart, with jitter", () => {
    let t = 1_000;
    const draws = [0.5, 0, 0.99, 0.2];
    const delay = createStagger(1_500, 2_500, () => t, () => draws.shift() ?? 0);
    // Three rows mounting at the same instant.
    const starts = [delay(), delay(), delay()].map((d) => t + d);
    expect(starts).toEqual([1_000 + 1_250, 1_000 + 1_250 + 1_500, 1_000 + 1_250 + 1_500 + 1_500 + 2_475]);
    for (let i = 1; i < starts.length; i++) expect(starts[i]! - starts[i - 1]!).toBeGreaterThanOrEqual(1_500);
    // Much later, a request only waits for its jitter.
    t = 60_000;
    expect(delay()).toBe(500);
  });

  it("with real randomness, many simultaneous requests still never share a moment", () => {
    const delay = createStagger(1_500, 2_500, () => 0);
    const starts = Array.from({ length: 20 }, () => delay());
    for (let i = 1; i < starts.length; i++) expect(starts[i]! - starts[i - 1]!).toBeGreaterThanOrEqual(1_500);
    expect(starts[0]!).toBeLessThan(2_500);
  });
});
