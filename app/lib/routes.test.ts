import { describe, expect, it } from "vitest";
import { vaultHref, vaultIdFromFragment } from "./routes";

const VAULT = `0x${"ab".repeat(32)}`;

describe("vault links (M-6: the vault id never reaches the app host)", () => {
  it("puts the vault id in the fragment, never in the path or the query", () => {
    for (const page of ["position", "verify", "refund"] as const) {
      const href = vaultHref(page, VAULT.toUpperCase().replace("0X", "0x"));
      expect(href).toBe(`/${page}#${VAULT}`);
      const url = new URL(href, "https://app.example.test");
      // What a browser sends to the host: path and query only.
      expect(`${url.pathname}${url.search}`).toBe(`/${page}`);
      expect(vaultIdFromFragment(url.hash)).toBe(VAULT);
    }
  });

  it("accepts only a bytes32 vault id", () => {
    expect(vaultIdFromFragment(`#${VAULT.toUpperCase().replace("0X", "0x")}`)).toBe(VAULT);
    expect(vaultIdFromFragment(VAULT)).toBe(VAULT);
    expect(vaultIdFromFragment("")).toBeNull();
    expect(vaultIdFromFragment("#0x1234")).toBeNull();
    expect(vaultIdFromFragment(`#${VAULT}00`)).toBeNull();
    expect(vaultIdFromFragment(`#${VAULT.slice(2)}`)).toBeNull();
    expect(vaultIdFromFragment("#top")).toBeNull();
  });
});
