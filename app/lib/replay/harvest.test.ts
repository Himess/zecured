import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { createPublicClient, encodeAbiParameters, http, sha256, toEventSelector, toFunctionSelector, type Hex, type PublicClient } from "viem";
import { baseSepolia } from "viem/chains";
import { swapAbi } from "../abi4";
import {
  accrueInterestEvent,
  approvalEvent,
  assertAllSteps,
  attestAbi,
  buildRecording,
  checkRecording,
  closeAbi,
  decodeJournal,
  findSecrets,
  gatherFacts,
  openingFeeOf,
  parseLeakScan,
  summary,
  userOperationEvent,
  type HarvestFacts,
  type HarvestInputs,
  type StepFact,
} from "./harvest";
import { buildPlaceholderRecording } from "./placeholder";
import { REPLAY_STEP_IDS, type ReplayRecording, type ReplayStepId } from "./types";

const RECORDING = fileURLToPath(new URL("./recording.json", import.meta.url));

/** The facts a recording was built from: its sections, and each step's hashes and time. */
function factsOf(r: ReplayRecording, feePaidAlone = false): HarvestFacts {
  const { steps, ...recording } = r;
  const meta = (id: ReplayStepId) => r.steps.find((s) => s.id === id)!.artifacts.find((a) => a.kind === "proof")!.meta!;
  const facts: Partial<Record<ReplayStepId, StepFact>> = {};
  for (const s of steps) {
    facts[s.id] = { at: s.at === null ? null : Date.parse(s.at) / 1000, tx: s.tx, artifacts: s.artifacts.map((a) => a.tx), extra: [] };
  }
  assertAllSteps(facts);
  return {
    recording,
    imageIds: { deposit: meta("proven").imageId as Hex, violation: meta("payouts-confirmed").imageId as Hex, payment: meta("swap-proven").imageId as Hex },
    prover: meta("proven").prover!,
    feePaidAlone,
    steps: facts,
  };
}

describe("the harvester's reading of Base", () => {
  it("decodes the calls by the compiled contracts' selectors, and the events by their topics", () => {
    // contracts/out: ZecuredIntake, SettlementAdapter and ZecuredSwap's methodIdentifiers.
    expect(toFunctionSelector(attestAbi[0])).toBe("0x0f6bed49");
    expect(toFunctionSelector(closeAbi[0])).toBe("0x8abc8999");
    expect(toFunctionSelector(swapAbi.find((i) => i.type === "function" && i.name === "open")!)).toBe("0xba451cb0");
    // EntryPoint v0.7, Morpho Blue's EventsLib, ERC-20.
    expect(toEventSelector(userOperationEvent)).toBe("0x49628fd1471006c1482da88028e9ce4dbb080b815c9b0344d39e5a8e6ec1419f");
    expect(toEventSelector(accrueInterestEvent)).toBe("0x9d9bd501d0657d7dfe415f779a620a62b78bc508ddc0891fbbd8b7ac0f8fce87");
    expect(toEventSelector(approvalEvent)).toBe("0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925");
  });

  it("reads a deposit proof's journal v2 word by word: the journal RISC Zero's router accepted on 10-27", () => {
    // contracts/test/fork/RealSealsV2.t.sol `_journal(NU7, AMOUNT)`: the TEST-ONLY vault 9's real deposit.
    const values = [
      0,
      0x77190ad9,
      "0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000",
      "0x482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b",
      "0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F",
      1_000_000n,
      "0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e",
      "0xaaccb41983fa9d9288296dd099d5947119c2abc7e89b57237c00c76cf2680432",
    ] as const;
    const journal = encodeAbiParameters(
      [
        { type: "uint8" },
        { type: "uint32" },
        { type: "bytes32" },
        { type: "bytes32" },
        { type: "address" },
        { type: "uint64" },
        { type: "bytes32" },
        { type: "bytes32" },
      ],
      values,
    );
    // The digest the live router verified the seal against (README, "Check a real proof").
    expect(sha256(journal)).toBe("0x1a9dcbc10182a251b4e24fdc01740df36a90f7eb9397cb89be1a9316d35867f4");
    const j = decodeJournal(journal);
    expect([j.mode, j.branchId, j.tipHash, j.vaultId, j.account, j.amount, j.depositTag, j.vaultOpenedHash]).toEqual(values);
  });

  it("reads the leak scan's summary, and the opening fee as the Registry charges it", () => {
    const out = "identifiers: 302 (from 3 wallet(s) and 2 vault(s)), held in memory only\nscanned: 77 events, 55 transactions' calldata, 0 storage words\nmatches: 0\n";
    expect(parseLeakScan(out)).toEqual({ identifiers: 302, matches: 0 });
    expect(() => parseLeakScan("scanned: 1 events")).toThrow(/no identifiers or matches/);
    expect(openingFeeOf(5_000_000n, 50, 20_000)).toBe(25_000n);
    expect(openingFeeOf(1_000_000n, 50, 20_000)).toBe(0n);
  });
});

describe("the recording, from its facts", () => {
  it("builds the placeholder back from its own facts: its words, hashes and times", async () => {
    const placeholder = await buildPlaceholderRecording();
    expect(buildRecording(factsOf(placeholder))).toEqual(placeholder);
  });

  it("builds a run's recording: real hashes in every artifact, durations per track, the fee's own payout", async () => {
    const placeholder = await buildPlaceholderRecording();
    const facts = factsOf({ ...placeholder, placeholder: false }, true);
    let t = 1_793_000_000;
    for (const id of REPLAY_STEP_IDS) {
      const f = facts.steps[id];
      t += 60;
      f.at = id === "destination" ? null : t;
      const s = placeholder.steps.find((x) => x.id === id)!;
      f.artifacts = s.artifacts.map((a) =>
        a.chain === "offchain" ? null : a.chain === "zcash" ? (s.tx.zcash ?? null) : a.label === "MockUSDC approval" ? (s.tx.approve ?? null) : (s.tx.base ?? null),
      );
    }
    facts.steps.activated.extra = [{ label: "Opening fee: paid on Zcash, alone", chain: "zcash", tx: "ab".repeat(32), kind: "tx" }];
    const r = buildRecording(facts);
    const step = (id: ReplayStepId) => r.steps.find((s) => s.id === id)!;
    // Every artifact names its transaction, as parseRecording requires of a recorded run.
    expect(r.steps.flatMap((s) => s.artifacts).filter((a) => a.chain !== "offchain" && a.tx === null)).toEqual([]);
    expect(step("swap-opened").artifacts.map((a) => a.tx)).toEqual([step("swap-opened").tx.approve, step("swap-opened").tx.base]);
    // A minute between steps, each on its own track; none across the off-chain destination, whose time is unknown.
    expect(step("deposit-paid").durationSeconds).toBe(60);
    expect(step("destination").durationSeconds).toBeNull();
    expect(step("close-executed").durationSeconds).toBeNull();
    expect(step("payouts-committed").durationSeconds).toBe(60);
    expect(step("swap-opened").durationSeconds).toBeNull();
    expect(step("swap-paid").durationSeconds).toBe(60);
    expect(step("opened").at).toBe(new Date((1_793_000_000 + 60) * 1000).toISOString());
    // The fee went out alone, after the activation: the close's payout pays two.
    expect(step("payouts-sent").detail).toBe("Three of five signers sign one FROST transaction paying you and the market maker.");
    expect(step("activated").artifacts.at(-1)!.label).toBe("Opening fee: paid on Zcash, alone");
  });

  it("takes the run's amounts into the words the dock shows", async () => {
    const placeholder = await buildPlaceholderRecording();
    const facts = factsOf(placeholder);
    facts.recording = { ...facts.recording, loan: { ...facts.recording.loan, borrow: { assets: "25000000", protected: false } }, swap: { ...facts.recording.swap, usdc: "40000000" } };
    const r = buildRecording(facts);
    expect(r.steps.find((s) => s.id === "borrowed")!.hint).toContain("“DEMO: fill in 25.00”");
    expect(r.steps.find((s) => s.id === "swap-opened")!.hint).toContain("“DEMO: fill in 40.00”");
  });
});

describe("the checks before a recording ships", () => {
  it("pass the placeholder, checked the way the app reads it", async () => {
    expect(await checkRecording(await buildPlaceholderRecording())).toEqual([]);
  });

  it("catch a close that breaks I7, an order that is not its own, a secret, and a dummy hash in a run", async () => {
    const r = await buildPlaceholderRecording();
    const broken: ReplayRecording = {
      ...r,
      placeholder: false,
      loan: { ...r.loan, close: { ...r.loan.close, mmAmount: (BigInt(r.loan.close.mmAmount) + 1n).toString() } },
      swap: { ...r.swap, nonce: "2" },
    };
    const problems = (await checkRecording(broken)).join("\n");
    expect(problems).toMatch(/I7/);
    expect(problems).toMatch(/order's id/);
    expect(problems).toMatch(/dummy hash/);
    expect(findSecrets({ loan: { ownerKey: "0x01" }, ufvk: "uviewtest1abc" })).toEqual(["$.loan.ownerKey", "$.ufvk", "$.ufvk"]);
  });

  it("pass the recording this build ships, when it is a run's", async () => {
    const shipped = JSON.parse(readFileSync(RECORDING, "utf8")) as ReplayRecording;
    if (shipped.placeholder) return;
    expect(await checkRecording(shipped)).toEqual([]);
  });
});

/**
 * The harvest itself (never in CI): REPLAY_HARVEST=1 HARVEST_DIR=<the run's directory> with run.json, snap-t0.json,
 * snap-t1.json (optional), snap-t2.json, loan-public.json, zcash.json, leak-scan.txt (optional), and the deployment
 * records in contracts/deployments (HARVEST_DEPLOYMENT, base-sepolia by default). Writes recording.json and the run's
 * summary.md (into HARVEST_DIR) only when every check passes.
 */
describe.runIf(process.env.REPLAY_HARVEST === "1")("the live run's harvest", () => {
  it(
    "reads the run from Base, builds its recording and writes it",
    async () => {
      const dir = process.env.HARVEST_DIR;
      if (!dir) throw new Error("set HARVEST_DIR");
      const read = (name: string) => JSON.parse(readFileSync(join(dir, name), "utf8")) as never;
      const optional = (name: string) => (existsSync(join(dir, name)) ? read(name) : null);
      const deployments = fileURLToPath(new URL("../../../contracts/deployments/", import.meta.url));
      const network = process.env.HARVEST_DEPLOYMENT ?? "base-sepolia";
      const inputs: HarvestInputs = {
        run: read("run.json"),
        t0: read("snap-t0.json"),
        t1: optional("snap-t1.json"),
        t2: read("snap-t2.json"),
        deployment: JSON.parse(readFileSync(join(deployments, `${network}.json`), "utf8")),
        swapDeployment: JSON.parse(readFileSync(join(deployments, `${network}.swap.json`), "utf8")),
        loan: read("loan-public.json"),
        zcash: read("zcash.json"),
        leakScan: existsSync(join(dir, "leak-scan.txt")) ? readFileSync(join(dir, "leak-scan.txt"), "utf8") : null,
      };
      const client = createPublicClient({ chain: baseSepolia, transport: http(inputs.run.rpc, { batch: false, retryCount: 4 }) }) as PublicClient;
      const recording = buildRecording(await gatherFacts(inputs, client));
      const problems = await checkRecording(recording);
      expect(problems).toEqual([]);
      writeFileSync(RECORDING, `${JSON.stringify(recording, null, 2)}\n`);
      writeFileSync(join(dir, "summary.md"), summary(recording));
    },
    600_000,
  );
});
