import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { toEventSignature, toFunctionSignature, type AbiEvent, type AbiFunction } from "viem";
import {
  adapterV4Abi,
  bondV4Abi,
  bondViewsAbi,
  erc20AllowanceAbi,
  intakeViewsAbi,
  morphoSupplyAbi,
  refundsV4Abi,
  registryV4EventsAbi,
  registryViewsAbi,
  swapAbi,
} from "./abi4";

type Item = { type: string; name?: string; inputs?: readonly { type: string; indexed?: boolean; components?: unknown }[] };

const signature = (i: Item) =>
  i.type === "function" ? toFunctionSignature(i as AbiFunction) : i.type === "event" ? toEventSignature(i as AbiEvent) : `${i.name}`;
const indexed = (i: Item) => (i.inputs ?? []).map((p) => Boolean(p.indexed));
const find = (abi: readonly Item[], type: string, name: string) => {
  const item = abi.find((i) => i.type === type && i.name === name);
  if (!item) throw new Error(`${type} ${name} missing`);
  return item;
};

describe("v4 fragments: the V4 interface freeze's signatures", () => {
  it("SignerBondV2 (section 3): armLiveness, hashDestinationReceived, LivenessArmed", () => {
    expect(signature(find(bondV4Abi, "function", "armLiveness"))).toBe("armLiveness(bytes32,uint64,bytes)");
    expect(signature(find(bondV4Abi, "function", "hashDestinationReceived"))).toBe("hashDestinationReceived(bytes32,bytes32,uint64)");
    const armed = find(bondV4Abi, "event", "LivenessArmed");
    expect(signature(armed)).toBe("LivenessArmed(bytes32,bytes32,uint64)");
    expect(indexed(armed)).toEqual([true, false, false]);
  });

  it("the Settlement Adapter (section 5): the terms, their setting and clearing, and the events", () => {
    const terms = "(uint256,uint256,uint256,uint256,uint256,uint64)";
    expect(signature(find(adapterV4Abi, "function", "setPreLiquidation"))).toBe(`setPreLiquidation(${terms})`);
    expect(signature(find(adapterV4Abi, "function", "clearPreLiquidation"))).toBe("clearPreLiquidation()");
    expect(signature(find(adapterV4Abi, "function", "preLiquidationTerms"))).toBe("preLiquidationTerms(address)");
    const set = find(adapterV4Abi, "event", "PreLiquidationSet");
    expect(signature(set)).toBe(`PreLiquidationSet(address,${terms})`);
    expect(indexed(set)).toEqual([true, false]);
    expect(signature(find(adapterV4Abi, "event", "PreLiquidationCleared"))).toBe("PreLiquidationCleared(address)");
    const pre = find(adapterV4Abi, "event", "PreLiquidated");
    expect(signature(pre)).toBe("PreLiquidated(bytes32,address,address,uint256,uint64)");
    expect(indexed(pre)).toEqual([true, true, true, false, false]);
  });

  it("the Registry (sections 1, 2, 4): PayoutConfirmed names the payout, PayoutProven, RefundDormant", () => {
    const confirmed = find(registryV4EventsAbi, "event", "PayoutConfirmed");
    expect(signature(confirmed)).toBe("PayoutConfirmed(bytes32,bytes32,bytes32[])");
    expect(indexed(confirmed)).toEqual([true, true, false]);
    const proven = find(registryV4EventsAbi, "event", "PayoutProven");
    expect(signature(proven)).toBe("PayoutProven(bytes32,bytes32,bytes32)");
    expect(indexed(proven)).toEqual([true, true, false]);
    expect(signature(find(registryV4EventsAbi, "event", "RefundDormant"))).toBe("RefundDormant(bytes32,bytes32)");
  });

  it("ZecuredSwap (section 5): open with the SwapQuote, refund, and the five events", () => {
    const quote = "(address,address,uint8,uint64,uint256,bytes32,uint32,uint64,uint256)";
    expect(signature(find(swapAbi, "function", "open"))).toBe(`open(${quote},bytes,bytes32)`);
    expect(signature(find(swapAbi, "function", "refund"))).toBe("refund(bytes32)");
    const opened = find(swapAbi, "event", "OrderOpened");
    expect(signature(opened)).toBe("OrderOpened(bytes32,address,address,uint8,uint64,uint256,bytes32,uint64)");
    expect(indexed(opened)).toEqual([true, true, true, false, false, false, false, false]);
    expect(signature(find(swapAbi, "event", "SettlementPending"))).toBe("SettlementPending(bytes32,bytes32,uint256,uint256)");
    for (const name of ["OrderSettled", "SettlementDropped", "OrderRefunded"]) {
      expect(signature(find(swapAbi, "event", name))).toBe(`${name}(bytes32)`);
    }
  });
});

describe("fragments against the compiled contracts (contracts/abi), once they contain them", () => {
  interface Param {
    type: string;
    components?: readonly Param[];
    indexed?: boolean;
  }
  interface Compiled {
    type: string;
    name?: string;
    inputs?: readonly Param[];
    outputs?: readonly Param[];
  }
  const path = (name: string) => fileURLToPath(new URL(`../../contracts/abi/${name}.json`, import.meta.url));
  const compiled = (name: string): Compiled[] => (existsSync(path(name)) ? (JSON.parse(readFileSync(path(name), "utf8")) as Compiled[]) : []);
  const shape = (p: Param): string => (p.components ? `(${p.components.map(shape).join(",")})` : p.type);
  const full = (i: Compiled) =>
    `${i.type} ${i.name}(${(i.inputs ?? []).map((p) => `${shape(p)}${p.indexed ? " indexed" : ""}`).join(",")}) -> (${(i.outputs ?? []).map(shape).join(",")})`;
  /** What v4 changes: the v3 shape still in today's ABIs until they are regenerated (R-1 adds the payout's reference). */
  const V3_SHAPES: Record<string, string> = {
    PayoutConfirmed: "event PayoutConfirmed(bytes32 indexed,bytes32[]) -> ()",
  };
  /** Every fragment whose name the compiled ABI has must match it exactly; v3 views must already be there. */
  const check = (file: string, items: readonly Compiled[], mustExist: boolean) => {
    const known = compiled(file);
    for (const item of items) {
      const theirs = known.filter((k) => k.type === item.type && k.name === item.name);
      if (theirs.length === 0) {
        expect(mustExist, `${file}: ${item.type} ${item.name} is not compiled yet`).toBe(false);
        continue;
      }
      const shapes = theirs.map(full);
      if (item.name && V3_SHAPES[item.name] && shapes.every((s) => s === V3_SHAPES[item.name!])) continue;
      expect(shapes, `${file}: ${item.name}`).toContain(full(item));
    }
  };

  it("v3 views the status and market pages read are in today's ABIs", () => {
    check("SignerBondV2", bondViewsAbi, true);
    check("ZecuredRegistry", registryViewsAbi, true);
    check("ZecuredIntake", intakeViewsAbi, true);
  });

  it("v4 fragments match the compiled v4 contracts where they exist", () => {
    check("SignerBondV2", bondV4Abi.filter((i) => i.type !== "error"), false);
    check("SettlementAdapter", adapterV4Abi.filter((i) => i.type !== "error"), false);
    check("ZecuredRegistry", registryV4EventsAbi, false);
    check("ZecuredSwap", swapAbi, false);
  });

  it("the refunds' dormancy views are served at the Registry's address (its spend logic), not in its own ABI", () => {
    expect(refundsV4Abi.map((i) => i.name)).toEqual(["refundDormancy", "isRefundDormant"]);
    expect(compiled("ZecuredRegistry").some((i) => i.type === "fallback")).toBe(true);
  });

  it("ERC-20 allowance and Morpho's supply side are the standard ones", () => {
    expect(signature(find(erc20AllowanceAbi, "function", "allowance"))).toBe("allowance(address,address)");
    const market = "(address,address,address,address,uint256)";
    expect(signature(find(morphoSupplyAbi, "function", "supply"))).toBe(`supply(${market},uint256,uint256,address,bytes)`);
    expect(signature(find(morphoSupplyAbi, "function", "withdraw"))).toBe(`withdraw(${market},uint256,uint256,address,address)`);
  });
});
