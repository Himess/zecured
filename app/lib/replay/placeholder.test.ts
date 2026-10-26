import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { getAddress } from "viem";
import { checkVaultAddress } from "../owner";
import { buildConfig, marketIdOf, marketParams } from "../config";
import { parseRemoteConfig } from "../api";
import { verifyVaultOpened } from "../signers";
import { buildPlaceholderRecording } from "./placeholder";
import { parseRecording } from "./recording";
import { replayBuildEnv } from "./pins";

const FILE = fileURLToPath(new URL("./recording.json", import.meta.url));

describe("the placeholder recording", () => {
  it("is generated deterministically (REPLAY_GENERATE=1 writes recording.json)", async () => {
    const built = await buildPlaceholderRecording();
    if (process.env.REPLAY_GENERATE === "1") writeFileSync(FILE, `${JSON.stringify(built, null, 2)}\n`);
    expect(existsSync(FILE)).toBe(true);
    const onDisk = JSON.parse(readFileSync(FILE, "utf8")) as { placeholder: boolean };
    // A real recording replaces the placeholder: only then may the file differ from it.
    if (onDisk.placeholder) expect(onDisk).toEqual(JSON.parse(JSON.stringify(built)));
  });

  it("is a well-formed recording whose deployment the app accepts, pinned", async () => {
    const r = parseRecording(JSON.parse(JSON.stringify(await buildPlaceholderRecording())));
    const config = buildConfig(replayBuildEnv("production", r), parseRemoteConfig(r.config));
    expect(config.problems).toEqual([]);
    expect(config.contractsDeployed).toBe(true);
    expect(config.signersPinned).toBe(true);
    expect(config.marketLabels?.loanToken).toBe("MockUSDC");
    expect(marketIdOf(marketParams(config))).toBe(r.config.market.marketId);
    // The vault's address is an Ironwood-only testnet UA, and its payment request is the app's own.
    expect(checkVaultAddress(r.loan.vault.address, BigInt(r.loan.amount), r.loan.vault.zip321)).toEqual({
      ok: true,
      problems: [],
      uri: r.loan.vault.zip321,
    });
    // All five pinned signers vouch for it, checked by the app's own code.
    const check = await verifyVaultOpened(
      {
        vaultId: r.loan.vault.vaultId,
        owner: r.loan.owner,
        account: r.loan.account,
        amount: BigInt(r.loan.amount),
        expiryHeight: r.loan.vault.expiryHeight,
        address: r.loan.vault.address,
        tagKeyHash: r.loan.vault.tagKeyHash,
      },
      r.loan.vault.signatures,
      { config },
    );
    expect(check).toBe("pinned");
    expect(getAddress(r.swap.swap)).toBe(config.swap);
    // I7 of the recorded close: the two payouts add up to the collateral (the deposit less the opening fee).
    const fee = (BigInt(r.loan.amount) * BigInt(r.config.v3.feeBps)) / 10_000n;
    expect(BigInt(r.loan.close.mmAmount) + BigInt(r.loan.close.userAmount)).toBe(BigInt(r.loan.amount) - fee);
  });
});
