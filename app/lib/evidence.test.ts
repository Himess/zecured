import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { EVIDENCE_SCHEMA, EvidenceError, explorerLink, orderedSections, parseEvidence } from "./evidence";

const SAMPLE = JSON.parse(readFileSync(fileURLToPath(new URL("./__fixtures__/evidence.sample.json", import.meta.url)), "utf8")) as Record<
  string,
  unknown
>;

/** A deep copy of the sample, changed by `f`. */
function variant(f: (e: any) => void): unknown {
  const e = structuredClone(SAMPLE);
  f(e);
  return e;
}

describe("the evidence file (P-3, schema zecured-evidence/1)", () => {
  it("parses the sample: manifest, flows with their steps, the leak scan, the mainnet section", () => {
    const e = parseEvidence(SAMPLE);
    expect(EVIDENCE_SCHEMA).toBe("zecured-evidence/1");
    expect(e.sections.map((s) => s.id)).toEqual(["judge", "mainnet-beta"]);
    const judge = e.sections[0]!;
    expect(judge.manifest.contracts[0]).toEqual({ name: "ZecuredRegistry", address: "0x2849a4FA18C4bD42687bD9B8eBDC2F4D89ECfB2F" });
    expect(judge.manifest.mocks.map((m) => m.label)).toEqual(["DEMO", "MOCK"]);
    expect(judge.flows[0]!.steps.map((s) => s.chain)).toEqual(["zcash", "base"]);
    expect(judge.leakScan?.result).toBe("0 matches");
    expect(e.durations.get("payout")).toBe(900);
    expect(e.sections[1]!.leakScan).toBeNull();
  });

  it("links each step to its network's explorer, or to none", () => {
    const e = parseEvidence(SAMPLE);
    const judge = e.sections[0]!;
    expect(explorerLink(e, judge, judge.flows[0]!.steps[0]!)).toBe(
      "https://testnet.cipherscan.app/tx/00000000000000000000000000000000000000000000000000000000000000a1",
    );
    expect(explorerLink(e, judge, judge.flows[0]!.steps[1]!)).toMatch(/^https:\/\/sepolia\.basescan\.org\/tx\/0x0+b1$/);
    const mainnet = e.sections[1]!;
    expect(explorerLink(e, mainnet, { label: "x", chain: "zcash", tx: "aa".repeat(32), at: null })).toBeNull();
  });

  it("shows this build's own deployment first", () => {
    const e = parseEvidence(SAMPLE);
    expect(orderedSections(e, false).map((s) => s.id)).toEqual(["judge", "mainnet-beta"]);
    expect(orderedSections(e, true).map((s) => s.id)).toEqual(["mainnet-beta", "judge"]);
  });

  it("refuses anything malformed, so a typo never shows a wrong transaction", () => {
    const bad: ((e: any) => void)[] = [
      (e) => (e.schema = "zecured-evidence/2"),
      (e) => (e.updatedAt = "yesterday"),
      (e) => (e.sections[0].id = "Judge Deployment"),
      (e) => (e.sections[1].id = "judge"),
      (e) => (e.sections[0].base = "ethereum"),
      (e) => (e.sections[0].manifest.commit = "xyz"),
      (e) => (e.sections[0].manifest.contracts.ZecuredRegistry = "0x1234"),
      (e) => (e.sections[0].manifest.imageIds["deposit-test"] = "0x11"),
      (e) => (e.sections[0].manifest.mocks[0].label = "FAKE"),
      // A Zcash txid with 0x, a Base hash without, a short one.
      (e) => (e.sections[0].flows[0].steps[0].tx = `0x${"a1".repeat(32)}`),
      (e) => (e.sections[0].flows[0].steps[1].tx = "b1".repeat(32)),
      (e) => (e.sections[0].flows[0].steps[1].tx = "0x1234"),
      (e) => (e.sections[0].flows[0].steps[0].chain = "solana"),
      (e) => (e.explorers["base-sepolia"] = "http://sepolia.basescan.org/tx/"),
      (e) => (e.explorers.etherscan = "https://etherscan.io/tx/"),
      (e) => (e.durations.payout = -1),
      (e) => (e.sections[0].leakScan.identifiers = 1.5),
    ];
    for (const f of bad) expect(() => parseEvidence(variant(f))).toThrow(EvidenceError);
    expect(() => parseEvidence(null)).toThrow(/root/);
  });

  it("explorers and durations are optional", () => {
    const e = parseEvidence(
      variant((x) => {
        delete x.explorers;
        delete x.durations;
      }),
    );
    expect(e.explorers).toEqual({});
    expect(e.durations.size).toBe(0);
  });
});
