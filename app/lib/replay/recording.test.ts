import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const FILE = fileURLToPath(new URL("./recording.json", import.meta.url));

/** Names a secret would go by: a recording never carries one (types.ts). */
const SECRET_KEY = /^(salt|ufvk|uivk|viewing_?key|owner_?key|private_?key|spending_?key|secret|mnemonic|seed)$/i;
/** Zcash viewing and spending keys, by their encodings' prefixes. */
const SECRET_VALUE = /^(uview|uivk|zxview|secret-extended-key)/i;

function secrets(v: unknown, path: string, found: string[]): string[] {
  if (typeof v === "string" && SECRET_VALUE.test(v)) found.push(path);
  if (Array.isArray(v)) v.forEach((x, i) => secrets(x, `${path}[${i}]`, found));
  else if (v && typeof v === "object") {
    for (const [k, x] of Object.entries(v)) {
      if (SECRET_KEY.test(k)) found.push(`${path}.${k}`);
      secrets(x, `${path}.${k}`, found);
    }
  }
  return found;
}

describe("the recording a replay build ships", () => {
  it("carries no private key, salt or viewing key", () => {
    expect(secrets(JSON.parse(readFileSync(FILE, "utf8")), "$", [])).toEqual([]);
  });

  it("would be caught carrying one", () => {
    expect(secrets({ loan: { salt: "0x01" }, keys: ["uviewtest1abc"], demo: { viewingKey: "x" } }, "$", [])).toEqual([
      "$.loan.salt",
      "$.keys[0]",
      "$.demo.viewingKey",
    ]);
  });
});
