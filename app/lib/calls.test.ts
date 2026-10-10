import { describe, expect, it } from "vitest";
import { decodeFunctionData, encodeFunctionData, type Address, type Hex } from "viem";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import { adapterAbi, erc20Abi, morphoAbi, registryAbi } from "./abi";
import {
  assertAllowlisted,
  authorizeAdapterCalls,
  borrowCalls,
  invalidateNonceCalls,
  NotAllowlistedError,
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
