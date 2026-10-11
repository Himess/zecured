import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { decodeFunctionData, encodeErrorResult, encodeFunctionData, slice, type Address, type Hex } from "viem";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import { adapterAbi, erc20Abi, morphoAbi, registryAbi, registryErrorsAbi } from "./abi";
import {
  assertAllowlisted,
  authorizeAdapterCalls,
  borrowCalls,
  describeRevert,
  invalidateNonceCalls,
  NotAllowlistedError,
  partialReleaseCalls,
  releaseCalls,
  repayAndReleaseCalls,
  transferUsdcCalls,
  type Call,
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

describe("ABI fragments match the compiled ZecuredRegistry (contracts/abi)", () => {
  const compiled = JSON.parse(
    readFileSync(fileURLToPath(new URL("../../contracts/abi/ZecuredRegistry.json", import.meta.url)), "utf8"),
  ) as { type: string; name?: string; inputs?: { type: string }[] }[];
  const signature = (item: { name?: string; inputs?: readonly { type: string }[] }) =>
    `${item.name}(${(item.inputs ?? []).map((i) => i.type).join(",")})`;
  const compiledSignatures = new Set(compiled.filter((i) => i.type === "function" || i.type === "error").map(signature));

  it("every Registry function and error the app encodes or decodes exists with the same types", () => {
    for (const item of [...registryAbi.filter((i) => i.type === "function"), ...registryErrorsAbi]) {
      expect(compiledSignatures.has(signature(item)), signature(item)).toBe(true);
    }
  });
});

describe("Registry revert explanations", () => {
  const encode = (errorName: (typeof registryErrorsAbi)[number]["name"], args: readonly unknown[]) =>
    encodeErrorResult({ abi: registryErrorsAbi, errorName, args } as never) as Hex;

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
