import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { decodeFunctionData, encodeErrorResult, encodeFunctionData, slice, type Address, type Hex } from "viem";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import {
  adapterAbi,
  bondAbi,
  CLAIM_KIND,
  CLAIM_STATUS,
  compensatorAbi,
  depositRefundedEvent,
  disputesAbi,
  erc20Abi,
  intakeAbi,
  MINT_BLOCK,
  morphoAbi,
  ONCHAIN_VAULT_STATUS,
  REFUND_REASON,
  registryAbi,
  registryErrorsAbi,
  registryEventsAbi,
  relayAbi,
  revertErrorsAbi,
  slasherAbi,
} from "./abi";
import { adapterV4Abi, bondV4Abi, refundPendingEvent, v4ErrorsAbi } from "./abi4";
import { PRE_LIQUIDATION_PRESETS, termsOf } from "./preliq";
import {
  armLivenessCalls,
  assertAllowlisted,
  authorizeAdapterCalls,
  borrowCalls,
  clearPreLiquidationCalls,
  describeRevert,
  setPreLiquidationCalls,
  disputePayoutCalls,
  formatUtc,
  invalidateNonceCalls,
  NotAllowlistedError,
  partialReleaseCalls,
  refundDestinationCalls,
  refundPendingCalls,
  releaseCalls,
  reopenDisputedClaimCalls,
  repayAndReleaseCalls,
  slashLivenessCalls,
  transferUsdcCalls,
  type Call,
  type NamedTargets,
} from "./calls";
import { buildConfig, marketParams } from "./config";

const config = buildConfig(DRY_RUN_ENV);
const ACCOUNT: Address = "0x3333333333333333333333333333333333333333";
const STRANGER: Address = "0x5555555555555555555555555555555555555555";
const VAULT: Hex = `0x${"ab".repeat(32)}`;
const COMMITMENT: Hex = `0x${"cd".repeat(32)}`;

const morphoCall = (data: Hex): Call => ({ to: config.morpho, data });

describe("call batches", () => {
  it("first borrow authorizes the Registry and the Adapter, then borrows to the account", () => {
    const calls = borrowCalls({ account: ACCOUNT, assets: 160_000n, registryAuthorized: false, adapterAuthorized: false, config });
    expect(calls).toHaveLength(3);
    const decoded = calls.map((c) => decodeFunctionData({ abi: morphoAbi, data: c.data }));
    expect(decoded[0]).toMatchObject({ functionName: "setAuthorization", args: [config.registry, true] });
    expect(decoded[1]).toMatchObject({ functionName: "setAuthorization", args: [config.adapter, true] });
    expect(decoded[2]).toMatchObject({ functionName: "borrow", args: [marketParams(config), 160_000n, 0n, ACCOUNT, ACCOUNT] });
    assertAllowlisted(calls, ACCOUNT, config);
  });

  it("skips authorizations that are already set (Morpho reverts on re-setting)", () => {
    const calls = borrowCalls({
      account: ACCOUNT,
      assets: 1n,
      receiver: STRANGER,
      registryAuthorized: true,
      adapterAuthorized: true,
      config,
    });
    expect(calls).toHaveLength(1);
    expect(decodeFunctionData({ abi: morphoAbi, data: calls[0]!.data }).args?.[4]).toBe(STRANGER);
  });

  it("repay + release: approve Morpho, repay by shares without callback data, then requestRelease", () => {
    const calls = repayAndReleaseCalls({
      account: ACCOUNT,
      borrowShares: 160_000_000_000n,
      approveAssets: 161_600n,
      vaultId: VAULT,
      commitment: COMMITMENT,
      registryAuthorized: true,
      config,
    });
    expect(calls.map((c) => c.to)).toEqual([config.usdc, config.morpho, config.registry]);
    expect(decodeFunctionData({ abi: erc20Abi, data: calls[0]!.data })).toMatchObject({
      functionName: "approve",
      args: [config.morpho, 161_600n],
    });
    expect(decodeFunctionData({ abi: morphoAbi, data: calls[1]!.data })).toMatchObject({
      functionName: "repay",
      args: [marketParams(config), 0n, 160_000_000_000n, ACCOUNT, "0x"],
    });
    expect(decodeFunctionData({ abi: registryAbi, data: calls[2]!.data })).toMatchObject({
      functionName: "requestRelease",
      args: [VAULT, COMMITMENT],
    });
    assertAllowlisted(calls, ACCOUNT, config);
  });

  it("release authorizes the Registry first when needed", () => {
    const calls = releaseCalls({ vaultId: VAULT, commitment: COMMITMENT, registryAuthorized: false, config });
    expect(calls.map((c) => c.to)).toEqual([config.morpho, config.registry]);
    assertAllowlisted(calls, ACCOUNT, config);
  });

  it("partial release (D14): authorizes the Registry when needed, then requestPartialRelease (0x6990cc7d)", () => {
    const first = partialReleaseCalls({ vaultId: VAULT, amount: 200_000n, commitment: COMMITMENT, registryAuthorized: false, config });
    expect(first.map((c) => c.to)).toEqual([config.morpho, config.registry]);
    expect(decodeFunctionData({ abi: morphoAbi, data: first[0]!.data })).toMatchObject({
      functionName: "setAuthorization",
      args: [config.registry, true],
    });
    expect(slice(first[1]!.data, 0, 4)).toBe("0x6990cc7d");
    expect(decodeFunctionData({ abi: registryAbi, data: first[1]!.data })).toMatchObject({
      functionName: "requestPartialRelease",
      args: [VAULT, 200_000n, COMMITMENT],
    });
    assertAllowlisted(first, ACCOUNT, config);
    const later = partialReleaseCalls({ vaultId: VAULT, amount: 100_000n, commitment: COMMITMENT, registryAuthorized: true, config });
    expect(later).toHaveLength(1);
    assertAllowlisted(later, ACCOUNT, config);
  });

  it("partial release refuses amounts the Registry or the ABI cannot take", () => {
    const p = { vaultId: VAULT, commitment: COMMITMENT, registryAuthorized: true, config };
    expect(() => partialReleaseCalls({ ...p, amount: 99_999n })).toThrow(/at least 0.001 TAZ/);
    expect(() => partialReleaseCalls({ ...p, amount: 2n ** 64n })).toThrow(/range/);
  });

  it("transfer, invalidateNonce and adapter authorization are allowlisted", () => {
    assertAllowlisted(transferUsdcCalls({ to: STRANGER, amount: 5n, config }), ACCOUNT, config);
    const inv = invalidateNonceCalls({ nonce: 2n ** 100n, config });
    expect(decodeFunctionData({ abi: adapterAbi, data: inv[0]!.data }).args).toEqual([2n ** 100n]);
    assertAllowlisted(inv, ACCOUNT, config);
    assertAllowlisted(authorizeAdapterCalls({ config }), ACCOUNT, config);
  });

  it("rejects empty or non-positive amounts", () => {
    expect(() => borrowCalls({ account: ACCOUNT, assets: 0n, registryAuthorized: true, adapterAuthorized: true, config })).toThrow();
    expect(() => transferUsdcCalls({ to: STRANGER, amount: 0n, config })).toThrow();
  });
});

describe("ABI fragments match the compiled contracts (contracts/abi)", () => {
  interface Param {
    type: string;
    components?: readonly Param[];
    indexed?: boolean;
  }
  interface Item {
    type: string;
    name?: string;
    inputs?: readonly Param[];
    outputs?: readonly Param[];
  }
  const compiled = (name: string) =>
    JSON.parse(readFileSync(fileURLToPath(new URL(`../../contracts/abi/${name}.json`, import.meta.url)), "utf8")) as Item[];
  /** A type with its tuple components spelled out: the whole shape the app decodes. */
  const shape = (p: Param): string => (p.components ? `(${p.components.map(shape).join(",")})` : p.type);
  const signature = (item: Item) =>
    `${item.type} ${item.name}(${(item.inputs ?? []).map(shape).join(",")}) -> (${(item.outputs ?? []).map(shape).join(",")})`;
  const check = (file: string, items: readonly Item[]) => {
    const known = new Set(compiled(file).map(signature));
    for (const item of items) expect(known.has(signature(item)), `${file}: ${signature(item)}`).toBe(true);
  };

  it("every Registry function and error the app encodes or decodes exists with the same types (0x6990cc7d)", () => {
    check("ZecuredRegistry", [...registryAbi.filter((i) => i.type !== "event"), ...registryErrorsAbi]);
  });

  it("the dispute path, at the Registry's address, is IZecuredDisputes'", () => {
    check("IZecuredDisputes", disputesAbi);
    // Served by the Registry's fallback: not in its own ABI.
    expect(compiled("ZecuredRegistry").some((i) => i.name === "disputePayout")).toBe(false);
    expect(compiled("ZecuredRegistry").some((i) => i.type === "fallback")).toBe(true);
  });

  it("every event the app decodes, with its indexed fields, where the contracts emit it (v4's at the Registry's address)", () => {
    const flagged = (p: Param): string => `${shape(p)}${p.indexed ? " indexed" : ""}`;
    const eventSig = (e: Item) => `${e.name}(${(e.inputs ?? []).map(flagged).join(",")})`;
    const emitted = (files: string[]) => new Set(files.flatMap((f) => compiled(f).filter((i) => i.type === "event").map(eventSig)));
    // The Registry's own events, and those its fallback (the spend logic) emits at its address.
    const atRegistry = emitted(["ZecuredRegistry", "IZecuredPayouts", "IZecuredDisputes", "IZecuredRefunds"]);
    for (const e of registryEventsAbi) expect(atRegistry.has(eventSig(e)), eventSig(e)).toBe(true);
    const atIntake = emitted(["ZecuredIntake"]);
    for (const e of [depositRefundedEvent, refundPendingEvent]) expect(atIntake.has(eventSig(e)), eventSig(e)).toBe(true);
    // R-1: the confirmation names its payout.
    expect(eventSig(registryEventsAbi.find((e) => e.name === "PayoutConfirmed")!)).toBe("PayoutConfirmed(bytes32 indexed,bytes32 indexed,bytes32[])");
  });

  it("the Intake, the relay, the slasher, the bond and the compensator", () => {
    check("ZecuredIntake", intakeAbi);
    check("ZecuredSlasher", slasherAbi);
    check("SignerBondV2", bondAbi);
    check("ZecuredCompensator", compensatorAbi);
    // The relay's two views, as the Intake's relay implements them (IZcashHeaderRelay).
    const relaySource = readFileSync(fileURLToPath(new URL("../../contracts/src/relay/IZcashHeaderRelay.sol", import.meta.url)), "utf8");
    expect(relaySource).toMatch(/function bestChainTip\(\) external view returns \(bytes32 blockHash, uint256 height, uint256 chainWork\)/);
    expect(relaySource).toMatch(/function isInBestChain\(bytes32 blockHash\) external view returns \(bool\)/);
    expect(relayAbi.map((i) => i.name)).toEqual(["bestChainTip", "isInBestChain"]);
  });

  it("the enums the app reads as numbers are the contracts' (append-only)", () => {
    const source = (path: string) => readFileSync(fileURLToPath(new URL(`../../contracts/src/${path}`, import.meta.url)), "utf8");
    const members = (text: string, name: string) => {
      const body = new RegExp(`enum ${name} \\{([^}]*)\\}`).exec(text)?.[1] ?? "";
      return body
        .split("\n")
        .map((l) => l.replace(/\/\/.*$/, "").trim().replace(/,$/, ""))
        .filter((l) => /^[A-Z]\w*$/.test(l));
    };
    const registry = source("interfaces/v3/IZecuredRegistry.sol");
    const intake = source("interfaces/v3/IZecuredIntake.sol");
    expect(members(registry, "VaultStatus")).toEqual([...ONCHAIN_VAULT_STATUS]);
    expect(members(registry, "ClaimKind")).toEqual([...CLAIM_KIND]);
    expect(members(registry, "ClaimStatus")).toEqual([...CLAIM_STATUS]);
    expect(members(intake, "MintBlock")).toEqual([...MINT_BLOCK]);
    expect(members(intake, "RefundReason")).toEqual([...REFUND_REASON]);
  });
});

describe("v3 calls: refunds (T-02, T-03) and disputes (T-05 (a))", () => {
  const CLAIM: Hex = `0x${"c1".repeat(32)}`;

  it("a refund's destination goes to the Registry with the pre-validated commitment, never zero", () => {
    const calls = refundDestinationCalls({ claimId: CLAIM, commitment: COMMITMENT, config });
    expect(calls).toEqual([{ to: config.registry, data: calls[0]!.data }]);
    expect(decodeFunctionData({ abi: registryAbi, data: calls[0]!.data })).toMatchObject({
      functionName: "setRefundDestination",
      args: [CLAIM, COMMITMENT],
    });
    assertAllowlisted(calls, ACCOUNT, config);
    expect(() => refundDestinationCalls({ claimId: CLAIM, commitment: `0x${"00".repeat(32)}`, config })).toThrow(/cannot be zero/);
  });

  it("a pending activation's refund goes to the pinned Intake", () => {
    const calls = refundPendingCalls({ vaultId: VAULT, config });
    expect(calls[0]!.to).toBe(config.intake);
    expect(decodeFunctionData({ abi: intakeAbi, data: calls[0]!.data })).toMatchObject({ functionName: "refundPending", args: [VAULT] });
    assertAllowlisted(calls, ACCOUNT, config);
  });

  it("disputing and reopening go to the Registry's address (its fallback)", () => {
    const dispute = disputePayoutCalls({ claimId: CLAIM, config });
    const reopen = reopenDisputedClaimCalls({ claimId: CLAIM, config });
    expect([dispute[0]!.to, reopen[0]!.to]).toEqual([config.registry, config.registry]);
    expect(decodeFunctionData({ abi: disputesAbi, data: dispute[0]!.data })).toMatchObject({ functionName: "disputePayout", args: [CLAIM] });
    expect(decodeFunctionData({ abi: disputesAbi, data: reopen[0]!.data })).toMatchObject({
      functionName: "reopenDisputedClaim",
      args: [CLAIM],
    });
    assertAllowlisted([...dispute, ...reopen], ACCOUNT, config);
  });

  it("the mirror refuses everything else of v3: other Intake and Registry calls, a zero destination, the wrong contract", () => {
    const reject = (calls: Call[], pattern: RegExp) => expect(() => assertAllowlisted(calls, ACCOUNT, config)).toThrow(pattern);
    const intakeView = encodeFunctionData({ abi: intakeAbi, functionName: "pendingActivation", args: [VAULT] });
    reject([{ to: config.intake, data: intakeView }], /Intake\.pendingActivation/);
    reject([{ to: config.registry, data: encodeFunctionData({ abi: disputesAbi, functionName: "payoutDispute", args: [CLAIM] }) }], /payoutDispute/);
    const zero = encodeFunctionData({ abi: registryAbi, functionName: "setRefundDestination", args: [CLAIM, `0x${"00".repeat(32)}`] });
    reject([{ to: config.registry, data: zero }], /zero refund destination/);
    // The right call on the wrong contract.
    reject([{ to: config.intake, data: refundDestinationCalls({ claimId: CLAIM, commitment: COMMITMENT, config })[0]!.data }], /unknown Intake function/);
    reject([{ to: config.registry, data: refundPendingCalls({ vaultId: VAULT, config })[0]!.data }], /unknown Registry function/);
    reject([{ to: config.adapter, data: disputePayoutCalls({ claimId: CLAIM, config })[0]!.data }], /unknown Adapter function/);
  });
});

describe("v3 calls: the bonds pay an overdue claim (T-05)", () => {
  const CLAIM: Hex = `0x${"c1".repeat(32)}`;
  const OTHER_CLAIM: Hex = `0x${"c2".repeat(32)}`;
  const BOND: Address = "0xf42821eF06F68a01bba08c91a16368A83C002586";
  const named: NamedTargets = { bond: BOND };

  it("slashLiveness goes to the bond the pinned Registry names, one call per claim", () => {
    const calls = slashLivenessCalls({ claimId: CLAIM, bond: BOND });
    expect(calls).toEqual([{ to: BOND, data: calls[0]!.data }]);
    expect(decodeFunctionData({ abi: bondAbi, data: calls[0]!.data })).toMatchObject({ functionName: "slashLiveness", args: [CLAIM] });
    assertAllowlisted(calls, ACCOUNT, config, named);
    assertAllowlisted([...calls, ...slashLivenessCalls({ claimId: OTHER_CLAIM, bond: BOND })], ACCOUNT, config, named);
  });

  it("the mirror refuses the bond's other calls, a claim paid twice, an unknown bond and the call elsewhere", () => {
    const reject = (calls: Call[], pattern: RegExp, n = named) => expect(() => assertAllowlisted(calls, ACCOUNT, config, n)).toThrow(pattern);
    const slash = slashLivenessCalls({ claimId: CLAIM, bond: BOND });
    reject([{ to: BOND, data: encodeFunctionData({ abi: bondAbi, functionName: "totalSlashable" }) }], /SignerBond\.totalSlashable/);
    reject([{ to: BOND, data: encodeFunctionData({ abi: bondAbi, functionName: "livenessDue", args: [CLAIM] }) }], /SignerBond\.livenessDue/);
    reject([...slash, ...slash], /the same claim paid from the bonds twice/);
    // Without the Registry's bond (not read, or unreadable), nothing to a bond is sponsorable.
    reject(slash, /call to/, { bond: null });
    expect(() => assertAllowlisted(slash, ACCOUNT, config)).toThrow(/call to/);
    reject([{ to: config.registry, data: slash[0]!.data }], /unknown Registry function/);
    // A "bond" that is a pinned contract never widens that contract's rules.
    reject([{ to: config.registry, data: slash[0]!.data }], /unknown Registry function/, { bond: config.registry });
  });
});

describe("v4 calls: arming a claim's liveness (R-5) and pre-liquidation terms (section 4)", () => {
  const CLAIM: Hex = `0x${"c1".repeat(32)}`;
  const BOND: Address = "0xf42821eF06F68a01bba08c91a16368A83C002586";
  const named: NamedTargets = { bond: BOND };
  const SIGNATURE: Hex = `0x${"1b".repeat(65)}`;

  it("armLiveness goes to the bond the Registry names, with the receipt, each claim once", () => {
    const calls = armLivenessCalls({ claimId: CLAIM, receivedAt: 1_800_000_000n, signature: SIGNATURE, bond: BOND });
    expect(calls).toEqual([{ to: BOND, data: calls[0]!.data }]);
    expect(decodeFunctionData({ abi: bondV4Abi, data: calls[0]!.data })).toMatchObject({
      functionName: "armLiveness",
      args: [CLAIM, 1_800_000_000n, SIGNATURE],
    });
    assertAllowlisted(calls, ACCOUNT, config, named);
    // Arming and having the bonds pay are two calls about one claim; arming it twice is one too many.
    assertAllowlisted([...calls, ...slashLivenessCalls({ claimId: CLAIM, bond: BOND })], ACCOUNT, config, named);
    expect(() => assertAllowlisted([...calls, ...calls], ACCOUNT, config, named)).toThrow(/armed twice/);
    // Without the Registry's bond nothing to a bond is sponsorable; elsewhere it is unknown.
    expect(() => assertAllowlisted(calls, ACCOUNT, config)).toThrow(/call to/);
    expect(() => assertAllowlisted([{ to: config.registry, data: calls[0]!.data }], ACCOUNT, config, named)).toThrow(
      /unknown Registry function/,
    );
    expect(() => armLivenessCalls({ claimId: CLAIM, receivedAt: 1n << 64n, signature: SIGNATURE, bond: BOND })).toThrow(/range/);
  });

  it("the account's own terms go to the pinned Adapter, within its bounds, alone or with the first borrow", () => {
    const terms = termsOf(PRE_LIQUIDATION_PRESETS[1]!, 1_800_000_000n);
    const set = setPreLiquidationCalls({ terms, config });
    expect(set).toEqual([{ to: config.adapter, data: set[0]!.data }]);
    expect(decodeFunctionData({ abi: adapterV4Abi, data: set[0]!.data })).toMatchObject({ functionName: "setPreLiquidation", args: [terms] });
    assertAllowlisted(set, ACCOUNT, config);
    const clear = clearPreLiquidationCalls({ config });
    expect(decodeFunctionData({ abi: adapterV4Abi, data: clear[0]!.data }).functionName).toBe("clearPreLiquidation");
    assertAllowlisted(clear, ACCOUNT, config);
    const first = borrowCalls({ account: ACCOUNT, assets: 1_000_000n, registryAuthorized: false, adapterAuthorized: false, config });
    assertAllowlisted([...first, ...set], ACCOUNT, config);
    // Out of bounds: refused when built, and by the mirror if built by hand.
    expect(() => setPreLiquidationCalls({ terms: { ...terms, preLltv: config.lltv }, config })).toThrow(/below the market's/);
    const raw = encodeFunctionData({ abi: adapterV4Abi, functionName: "setPreLiquidation", args: [{ ...terms, preLcf1: 0n }] });
    expect(() => assertAllowlisted([{ to: config.adapter, data: raw }], ACCOUNT, config)).toThrow(/out of bounds/);
    expect(() => assertAllowlisted([{ to: config.registry, data: set[0]!.data }], ACCOUNT, config)).toThrow(/unknown Registry function/);
    // Pre-liquidating a position is the liquidator's own business, never the account's.
    const preLiquidate = encodeFunctionData({
      abi: [
        {
          type: "function",
          name: "preLiquidate",
          stateMutability: "nonpayable",
          inputs: [
            { name: "account", type: "address" },
            { name: "repaidShares", type: "uint256" },
            { name: "mmCommitment", type: "bytes32" },
            { name: "minSeizedZat", type: "uint64" },
          ],
          outputs: [],
        },
      ],
      functionName: "preLiquidate",
      args: [STRANGER, 1n, COMMITMENT, 0n],
    });
    expect(() => assertAllowlisted([{ to: config.adapter, data: preLiquidate }], ACCOUNT, config)).toThrow(/unknown Adapter function/);
  });

  it("explains the bond's and the Adapter's v4 refusals", () => {
    const encode = (errorName: string, args: readonly unknown[]) => encodeErrorResult({ abi: v4ErrorsAbi, errorName, args } as never) as Hex;
    expect(describeRevert(new Error(encode("ReceiptFromTheFuture", [1_800_000_000n])))).toMatch(/dated after the latest block/);
    expect(describeRevert(new Error(encode("NotASigner", [STRANGER])))).toMatch(/not signed by one of the five signers/);
    expect(describeRevert(new Error(encode("PriceStale", [])))).toMatch(/price feed is stale/);
    expect(describeRevert(new Error(encode("NotAVaultAccount", [ACCOUNT])))).toMatch(/only the account of an active vault/);
    expect(describeRevert(new Error(encode("InvalidPreLif", [1n, 2n, 3n])))).toMatch(/outside its bounds/);
    expect(describeRevert(new Error(encode("InvalidExpiry", [1n])))).toMatch(/expiry has passed/);
  });
});

describe("Registry revert explanations", () => {
  const encode = (errorName: (typeof revertErrorsAbi)[number]["name"], args: readonly unknown[]) =>
    encodeErrorResult({ abi: revertErrorsAbi, errorName, args } as never) as Hex;

  it("explains v3's refusals: refunds, the freeze, disputes, pending activations", () => {
    const at = 1_800_000_000n;
    const cases: [Parameters<typeof encode>[0], readonly unknown[], RegExp][] = [
      ["NotARefund", [VAULT], /not a refund/],
      ["RefundBound", [VAULT], /can no longer change/],
      ["VaultFrozen", [VAULT], /theft case is open/],
      ["VaultCompromised", [VAULT], /compensator settles its claims in USDC/],
      ["DisputeVaultCompromised", [VAULT], /compensator settles its claims in USDC/],
      ["NotDisputable", [VAULT], /not a confirmed payout that can be disputed/],
      ["NotClaimant", [VAULT, ACCOUNT], /only the claim's own claimant/],
      ["NotDisputed", [VAULT], /not disputed/],
      ["NoPendingActivation", [VAULT], /no pending activation/],
      ["NotRefundableYet", [VAULT], /cannot be refunded yet/],
      ["NotPendingAccount", [VAULT, ACCOUNT], /only the vault's own account/],
    ];
    for (const [name, args, pattern] of cases) expect(describeRevert(new Error(encode(name, args))), name).toMatch(pattern);
    expect(describeRevert(new Error(encode("DisputeWindowClosed", [VAULT, at])))).toBe(
      `The Registry refused the dispute: it could be disputed until ${formatUtc(at)}.`,
    );
    expect(describeRevert(new Error(encode("InclusionWindowOpen", [VAULT, at])))).toMatch(/until 2027-01-15 08:00 UTC: it can be reopened after that/);
  });

  it("explains the bonds' refusals (T-05)", () => {
    expect(describeRevert(new Error(encode("NotTheClaimant", [VAULT, ACCOUNT])))).toMatch(/only the claim's own claimant/);
    expect(describeRevert(new Error(encode("NotLivenessEligible", [VAULT])))).toMatch(/cannot pay this claim/);
    expect(describeRevert(new Error(encode("LivenessNotDue", [VAULT, 1_800_000_000n])))).toBe(
      `The bonds can pay this claim only from ${formatUtc(1_800_000_000n)}.`,
    );
    expect(describeRevert(new Error(encode("InsufficientBond", [82_000n, 40_000n])))).toBe(
      "The bonds hold 0.04 USDC right now, less than the 0.082 USDC this claim needs. The claim stays payable on Zcash; try again later.",
    );
  });

  it("finds the revert data wherever viem nests the bundler's answer", () => {
    const data = encode("ReleaseLtvTooHigh", [160_161n, 320_000n]);
    const expected =
      "The Registry refused the withdrawal: it would leave 0.160161 USDC of debt against 0.32 USDC of collateral, above the 50 % limit. The price or the interest moved: try a smaller amount.";
    // In a message, in `data.revertData` deep in the cause chain, or as `data` itself.
    expect(describeRevert(new Error(`UserOperation reverted during simulation with reason: ${data}`))).toBe(expected);
    const nested = new Error("Execution reverted", {
      cause: Object.assign(new Error("RPC Request failed."), { data: { revertData: data } }),
    });
    expect(describeRevert(nested)).toBe(expected);
    expect(describeRevert({ shortMessage: "reverted", data })).toBe(expected);
  });

  it("explains each D14 limit with the Registry's own numbers", () => {
    expect(describeRevert(new Error(encode("ReleaseAmountTooSmall", [5n, 100_000n])))).toBe(
      "The Registry refused the withdrawal: it must be at least 0.001 TAZ.",
    );
    expect(describeRevert(new Error(encode("ReleaseAmountNotPartial", [1_000_000n, 1_000_000n])))).toMatch(
      /smaller than the collateral \(0\.01 TAZ\)/,
    );
    expect(describeRevert(new Error(encode("ReleaseRemainderTooSmall", [50_000n, 100_000n])))).toBe(
      "The Registry refused the withdrawal: at least 0.001 TAZ must stay in the vault (this would leave 0.0005 TAZ).",
    );
    expect(describeRevert(new Error(encode("NotVaultAccount", [VAULT, ACCOUNT])))).toMatch(/only the vault's own account/);
  });

  it("explains Morpho's string reverts and ignores everything else", () => {
    const morphoError = (reason: string) =>
      encodeErrorResult({
        abi: [{ type: "error", name: "Error", inputs: [{ name: "message", type: "string" }] }],
        errorName: "Error",
        args: [reason],
      });
    expect(describeRevert(new Error(morphoError("insufficient collateral")))).toMatch(/no longer be healthy/);
    expect(describeRevert(new Error(morphoError("something else")))).toBeNull();
    expect(describeRevert(new Error(`Not sponsorable: call to ${STRANGER}`))).toBeNull();
    expect(describeRevert("plain string")).toBeNull();
    const loop: { message: string; cause?: unknown } = { message: "loop" };
    loop.cause = loop;
    expect(describeRevert(loop)).toBeNull();
  });
});

describe("paymaster allowlist mirror (INTERFACES § 4.1)", () => {
  const reject = (calls: Call[], pattern: RegExp) => {
    expect(() => assertAllowlisted(calls, ACCOUNT, config)).toThrow(NotAllowlistedError);
    expect(() => assertAllowlisted(calls, ACCOUNT, config)).toThrow(pattern);
  };

  it("rejects setAuthorization for anything but the Registry and the Adapter", () => {
    reject(
      [morphoCall(encodeFunctionData({ abi: morphoAbi, functionName: "setAuthorization", args: [STRANGER, true] }))],
      /setAuthorization/,
    );
  });

  it("rejects borrow / repay on behalf of another account or on another market", () => {
    const other = { ...marketParams(config), oracle: STRANGER };
    reject(
      [morphoCall(encodeFunctionData({ abi: morphoAbi, functionName: "borrow", args: [marketParams(config), 1n, 0n, STRANGER, ACCOUNT] }))],
      /on behalf of another account/,
    );
    reject(
      [morphoCall(encodeFunctionData({ abi: morphoAbi, functionName: "borrow", args: [other, 1n, 0n, ACCOUNT, ACCOUNT] }))],
      /another market/,
    );
    reject(
      [morphoCall(encodeFunctionData({ abi: morphoAbi, functionName: "repay", args: [marketParams(config), 0n, 1n, ACCOUNT, "0x01"] }))],
      /callback data/,
    );
  });

  it("rejects other Morpho functions (e.g. withdrawCollateral) and unknown selectors", () => {
    const withdrawCollateral = encodeFunctionData({
      abi: [
        {
          type: "function",
          name: "withdrawCollateral",
          stateMutability: "nonpayable",
          inputs: [
            { name: "marketParams", type: "tuple", components: morphoAbi[2].inputs[0].components },
            { name: "assets", type: "uint256" },
            { name: "onBehalf", type: "address" },
            { name: "receiver", type: "address" },
          ],
          outputs: [],
        },
      ],
      functionName: "withdrawCollateral",
      args: [marketParams(config), 1n, ACCOUNT, ACCOUNT],
    });
    reject([morphoCall(withdrawCollateral)], /unknown Morpho function/);
    reject([morphoCall(encodeFunctionData({ abi: morphoAbi, functionName: "isAuthorized", args: [ACCOUNT, STRANGER] }))], /Morpho\.isAuthorized/);
  });

  it("rejects USDC approvals to anyone but Morpho, other contracts, ETH value and empty batches", () => {
    reject(
      [{ to: config.usdc, data: encodeFunctionData({ abi: erc20Abi, functionName: "approve", args: [STRANGER, 1n] }) }],
      /approve/,
    );
    reject([{ to: STRANGER, data: "0x" }], /call to/);
    reject(
      [{ ...transferUsdcCalls({ to: STRANGER, amount: 1n, config })[0]!, value: 1n as unknown as 0n }],
      /ETH value/,
    );
    reject([], /empty batch/);
    reject(
      [{ to: config.registry, data: encodeFunctionData({ abi: registryAbi, functionName: "vaultOf", args: [ACCOUNT] }) }],
      /Registry\.vaultOf/,
    );
  });
});
