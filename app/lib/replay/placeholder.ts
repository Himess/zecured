/**
 * The placeholder recording (DEMO), until the live Base Sepolia run replaces it: the contracts of
 * the local fork's deployment (contracts/deployments/anvil.json, anvil.swap.json), the numbers of
 * a testnet run (0.05 TAZ, ZEC at the deploy script's $1,265, MockUSDC, 10 confirmations as the
 * signers' default), five DEMO signer keys that really sign the
 * vault (so the app's own check of the signers' signatures runs and passes against them, pinned),
 * a valid Ironwood-only testnet address, and obviously dummy hashes (0x000…0001) with no explorer
 * link. Only lib/replay/placeholder.test.ts calls this: it writes and checks recording.json.
 */
import { concatBytes, getAddress, hexToBytes, type Address, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { vaultOpenedTypedData } from "../eip712";
import { predictAccountAddress } from "../safe";
import { vaultOpenedMessage } from "../signers";
import { orderIdOf } from "../swap";
import { encodeUnifiedAddress, TYPECODE_ORCHARD } from "../ua";
import { formatUsdc } from "../morpho";
import { formatZec, zip321Uri } from "../zcash";
import { demoBytes32, demoKey } from "./demo-keys";
import { REPLAY_SCHEMA, type ReplayRecording, type ReplayStep, type ReplayStepId } from "./types";

const CHAIN_ID = 84532;

/** The local fork's deployment (contracts/deployments/anvil.json): real addresses, on no public chain. */
const CONTRACTS = {
  BaseOracle: "0xE4079E41738c5c365B9EAdE39e9ec67971DF63dE",
  CapPrice: "0xd6a41044a7485e4f0B2d2ab079af2EDa3c231fa4",
  DemoShockOracle: "0xb36157EA2E28df291e6f4805E0582Bf0d3BDc113",
  LoanToken: "0x8228b67b43E671511E07a4A547add6471de9d4F0",
  MarketOracle: "0xb36157EA2E28df291e6f4805E0582Bf0d3BDc113",
  MockUSDC: "0x8228b67b43E671511E07a4A547add6471de9d4F0",
  MockZecOracle: "0xE4079E41738c5c365B9EAdE39e9ec67971DF63dE",
  SettlementAdapter: "0xD146f298980980dD574620abFaeB5B89A8BC9F66",
  SignerBond: "0xa9D70e64Ff000DbFab3332A665d9CA5e190E53Ce",
  ZZEC: "0x1E2e751452CaB7a3ef59f319D08D4Fbba861440d",
  ZecuredCompensator: "0x5CF271B75eC511FB12C3667a1dfc69AD566A12a9",
  ZecuredIntake: "0xdD86fEbD0551b97b5c1298324590B6B27cc64285",
  ZecuredRegistry: "0xa3f846F7C7a8a62F65bBf8be34D9A1f108118b21",
  ZecuredSlasher: "0xEc34aA21cDe1366b397a99A4d90fc6B2f02cFaEa",
  ZecuredSpendLogic: "0x1c1068d4Cf5CE052da15ff3467fcb5A27cF2C508",
  ZecuredSwap: "0x3983F1e1C7929A2208296895F01cE0d422917594",
  ZecuredVersions: "0x53a3D268683Cd748b3B71B55a202dF48B7cE633A",
} as const;

const MARKET = {
  collateralToken: CONTRACTS.ZZEC,
  irm: "0x46415998764C29aB2a25CbeA6254146D50D22687",
  lltv: "625000000000000000",
  loanToken: CONTRACTS.LoanToken,
  marketId: "0xdb98e90105d5872cd5066976f0ee5ab6188496db83472c48db3165c5ad26558f",
  morphoBlue: "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
  oracle: CONTRACTS.MarketOracle,
} as const;

const RELAY: Address = "0x637BBA9C47190C7a1ab2412F72cF0A9c088dE58b";
const FEE_COMMITMENT: Hex = "0x4c240d3fbe45206bf8b96a11c5a1ca09130dee390c70068ca8bf988161154ad8";
const DEPOSIT_IMAGE_ID: Hex = "0x406c6d268f29270226c775db1fe4bec04acf51ba919e250078125d71c10bff41";
const VIOLATION_IMAGE_ID: Hex = "0x65705f75c5482b2b0cf6fee12760b2364d48213bb2933f3882dc2d92f824866e";
const PAYMENT_IMAGE_ID: Hex = "0x199bf772d0780413a07081b4889f0473c8ae558c6d8c4dca96eef49b5e486134";

/** An obviously dummy Base hash: 0x000…0n. */
function dummyBase(n: number): Hex {
  return `0x${n.toString(16).padStart(64, "0")}`;
}

/** An obviously dummy Zcash txid: 000…0n (display order). */
function dummyZcash(n: number): string {
  return n.toString(16).padStart(64, "0");
}

/** A testnet Ironwood-only unified address whose 43-byte Orchard receiver is derived from `label`. */
function demoUnifiedAddress(label: string): string {
  const receiver = concatBytes([hexToBytes(demoBytes32(`${label} a`)), hexToBytes(demoBytes32(`${label} b`)).slice(0, 11)]);
  return encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: receiver }]);
}

const addressOf = (label: string): Address => privateKeyToAccount(demoKey(label)).address;

/** Ascending, as the Registry and NEXT_PUBLIC_SIGNERS keep them. */
function sortAddresses(a: Address[]): Address[] {
  return [...a].sort((x, y) => (x.toLowerCase() < y.toLowerCase() ? -1 : 1));
}

export async function buildPlaceholderRecording(): Promise<ReplayRecording> {
  const amount = 5_000_000n;
  const signerKeys = [0, 1, 2, 3, 4].map((i) => demoKey(`signer ${i}`));
  const signers = sortAddresses(signerKeys.map((k) => privateKeyToAccount(k).address));
  const owner = addressOf("owner");
  const account = predictAccountAddress(owner);
  const vaultId = demoBytes32("vault id");
  const address = demoUnifiedAddress("vault");
  const birthdayHeight = 4_541_150;
  const expiryHeight = birthdayHeight + 480;
  const tagKeyHash = demoBytes32("tag key hash");
  const registry = getAddress(CONTRACTS.ZecuredRegistry);
  const message = vaultOpenedMessage({ vaultId, owner, account, amount, expiryHeight, address, tagKeyHash });
  const signatures = await Promise.all(
    signerKeys.map(async (k, index) => ({
      index,
      signature: await privateKeyToAccount(k).signTypedData(vaultOpenedTypedData(registry, message)),
    })),
  );
  const maker = addressOf("swap maker");
  const taker = addressOf("swap wallet");
  const swapNonce = 1n;
  const executor = addressOf("market maker");

  // ZEC at $1,265 (the deploy script's ZEC_USD_PRICE_CENTS): 1.265e37 USDC units per zatoshi × 1e36. 0.05 TAZ less
  // the 0.5 % opening fee (25,000 zat) is 4,975,000 zat of collateral, worth 62.93 USDC; 22.00 USDC is a 35 %
  // loan-to-value. The market maker repays 22.0001 USDC (the debt and a little interest) and takes its value plus
  // 1 %, rounded up: 1,756,530 zat.
  const mmAmount = 1_756_530n;
  const userAmount = 4_975_000n - mmAmount;

  const borrowAssets = 22_000_000n;
  const swapUsdc = 25_000_000n;
  const base = (n: number) => dummyBase(n);
  const steps = withTx(
    replaySteps({
      amount,
      borrowAssets,
      swapUsdc,
      vaultId,
      imageIds: { deposit: DEPOSIT_IMAGE_ID, violation: VIOLATION_IMAGE_ID, payment: PAYMENT_IMAGE_ID },
      prover: "RISC Zero zkVM, Groth16",
      feePaidAlone: false,
    }),
    {
      "deposit-paid": { zcash: dummyZcash(2) },
      proven: { base: base(4) },
      activated: { base: base(5) },
      borrowed: { base: base(6), userOp: dummyBase(0x60) },
      "close-executed": { base: base(8) },
      "payouts-committed": { base: base(9) },
      "payouts-sent": { zcash: dummyZcash(10) },
      "payouts-confirmed": { base: base(11) },
      "swap-opened": { base: base(12), approve: dummyBase(0xc0) },
      "swap-paid": { zcash: dummyZcash(13) },
      "swap-proven": { base: base(14) },
      "swap-settled": { base: base(15) },
    },
  );

  return {
    schema: REPLAY_SCHEMA,
    placeholder: true,
    recordedAt: null,
    note: "Placeholder until the live Base Sepolia run is recorded: its hashes are dummies and nothing links to an explorer.",
    network: { base: "base-sepolia", zcash: "zcash-testnet", chainId: CHAIN_ID },
    explorers: { base: "https://sepolia.basescan.org/tx/", zcash: null },
    config: {
      chainId: CHAIN_ID,
      registry: CONTRACTS.ZecuredRegistry.toLowerCase(),
      adapter: CONTRACTS.SettlementAdapter.toLowerCase(),
      intake: CONTRACTS.ZecuredIntake,
      confirmations: 10,
      vaultExpiryBlocks: 480,
      openVaultStampBits: 8,
      sponsorship: true,
      denominations: [amount.toString()],
      signers,
      openings: { open: true, inviteOnly: false },
      contracts: { ...CONTRACTS },
      market: { ...MARKET },
      v3: {
        feeBps: 50,
        minFeeZat: 20_000,
        minRefundZat: 20_000,
        refundAbandonPeriod: 86_400,
        pendingRefundDelay: 300,
        maxPause: 2_592_000,
        livenessPeriod: 600,
        commitGrace: 7_200,
        livenessPremiumBps: 500,
        coverageBps: 10_000,
        maxTotalZat: 150_000_000,
        incidentWindow: 600,
      },
    },
    links: { relay: RELAY, disputeWindow: 600, inclusionWindow: 1_200, refundDormancy: 2_592_000, maxMarginBps: 500, feeCommitment: FEE_COMMITMENT },
    status: {
      commit: "0000000",
      treeClean: true,
      deployment: {
        chainId: CHAIN_ID,
        registry: CONTRACTS.ZecuredRegistry,
        intake: CONTRACTS.ZecuredIntake,
        signerBond: CONTRACTS.SignerBond,
        mintMode: "proof-only",
        verifierIsMock: false,
        depositImageId: DEPOSIT_IMAGE_ID,
        violationImageId: VIOLATION_IMAGE_ID,
      },
      versions: {
        deposit: [
          { versionId: demoBytes32("deposit version"), selector: "0x73c457ba", imageId: DEPOSIT_IMAGE_ID, branches: [1998129881], activeAt: 1_792_591_229, active: true, removed: false },
        ],
        violation: [
          { versionId: demoBytes32("violation version"), selector: "0x73c457ba", imageId: VIOLATION_IMAGE_ID, branches: [1998129881], activeAt: 1_792_591_229, active: true, removed: false },
        ],
      },
    },
    chain: {
      baseBlock: 48_600_000,
      zcashTip: 4_541_200,
      price: (1265n * 10n ** 34n).toString(),
      // AdaptiveCurveIRM at near-zero utilization: a quarter of the 4 % initial rate at target.
      borrowRate: "317097919",
      market: {
        totalSupplyAssets: "100000000000",
        totalSupplyShares: (100_000_000_000n * 10n ** 6n).toString(),
        totalBorrowAssets: "0",
        totalBorrowShares: "0",
        fee: "0",
      },
      otherLockedZat: "0",
      otherOpenVaults: { [amount.toString()]: 0 },
      bonds: { each: ["100000000", "100000000", "100000000", "100000000", "100000000"], effective: "500000000", totalSlashable: "500000000", minBond: "100000000" },
      relayChainWork: "1000000000",
    },
    loan: {
      owner,
      account,
      requestId: demoBytes32("request id"),
      amount: amount.toString(),
      vault: { vaultId, address, zip321: zip321Uri(address, amount), expiryHeight, birthdayHeight, tagKeyHash, signatures },
      depositTag: demoBytes32("deposit tag"),
      anchor: demoBytes32("deposit anchor"),
      versionId: demoBytes32("deposit version"),
      borrow: { assets: borrowAssets.toString(), protected: false },
      close: {
        executor,
        marginBps: 100,
        repayAssets: "22000100",
        mmAmount: mmAmount.toString(),
        userAmount: userAmount.toString(),
        userCommitment: demoBytes32("user commitment"),
        mmCommitment: demoBytes32("market maker commitment"),
        payoutRef: demoBytes32("payout ref"),
        inputTag: demoBytes32("payout input tag"),
      },
    },
    demo: { returnAddress: demoUnifiedAddress("demo wallet"), stampBits: 8, dailyCapZat: "50000000" },
    swap: {
      swap: getAddress(CONTRACTS.ZecuredSwap),
      maker,
      taker,
      takerUsdc: "100000000",
      zatoshi: "1956500",
      usdc: swapUsdc.toString(),
      makerReceiverCommitment: demoBytes32("maker receiver commitment"),
      receiverCommitment: demoBytes32("swap receiver commitment"),
      payWindow: 3_600,
      nonce: swapNonce.toString(),
      orderId: orderIdOf(CHAIN_ID, getAddress(CONTRACTS.ZecuredSwap), maker, swapNonce),
      anchor: demoBytes32("swap anchor"),
      signing: "demo-key",
      signature: null,
    },
    evidence: {
      commit: "0000000",
      tag: null,
      deployedAtBlock: null,
      mocks: [
        { name: "MockUSDC", address: CONTRACTS.MockUSDC, label: "MOCK", why: "The loan token, minted by the deployer." },
        { name: "DemoShockOracle", address: CONTRACTS.DemoShockOracle, label: "MOCK / DEMO", why: "An owner-set shock on the price, to show a liquidation." },
        { name: "MockZecOracle", address: CONTRACTS.MockZecOracle, label: "MOCK", why: "An owner-set ZEC/USD price." },
        { name: "This replay", address: null, label: "DEMO", why: "The judges' build replays the run in the browser: no backend, no chain." },
      ],
      leakScan: null,
    },
    steps,
  };
}

function step(
  id: ReplayStepId,
  track: ReplayStep["track"],
  auto: boolean,
  delayMs: number,
  rest: Pick<ReplayStep, "title" | "detail" | "hint" | "href" | "artifacts">,
): ReplayStep {
  // In the recording's key order: the hashes after the page, before the artifacts.
  const { title, detail, hint, href, artifacts } = rest;
  return { id, track, auto, delayMs, at: null, durationSeconds: null, title, detail, hint, href, tx: {}, artifacts };
}

/** What a step's words and artifacts depend on: the recorded amounts, the vault, the proofs' guests and prover. */
export interface StepParams {
  /** The vault's size, zatoshi. */
  amount: bigint;
  /** The borrow and the swap, in loan-token units (6 decimals). */
  borrowAssets: bigint;
  swapUsdc: bigint;
  vaultId: Hex;
  imageIds: { deposit: Hex; violation: Hex; payment: Hex };
  /** What made the proofs, as the dock names it. */
  prover: string;
  /** The opening fee went out in a payout of its own after the activation (a live run), not with the close's. */
  feePaidAlone: boolean;
}

/**
 * The run's fifteen steps as the replay's dock tells them, for the placeholder and for a recorded run alike (the
 * harvester, lib/replay/harvest.ts): their words, pages, which follow by themselves and how fast, and what each leaves
 * behind. No transaction yet: `tx` empty, every artifact's `tx` null, no times.
 */
export function replaySteps(p: StepParams): ReplayStep[] {
  const taz = `${formatZec(p.amount)} TAZ`;
  const borrow = formatUsdc(p.borrowAssets);
  const swap = formatUsdc(p.swapUsdc);
  const position = `/position#${p.vaultId}`;
  return [
    step("opened", "loan", false, 0, {
      title: "Open a vault",
      detail: "Five signer nodes run a distributed key generation for a new 3-of-5 vault, and each signs its Ironwood address; nothing touches Base.",
      hint: `On Borrow, press “Open a ${taz} vault”: five signer nodes make its key together.`,
      href: "/borrow",
      artifacts: [
        { label: "The five signers' VaultOpened signatures", chain: "offchain", tx: null, kind: "signatures", meta: { signers: "5 of 5", scheme: "EIP-712 VaultOpened v2" } },
      ],
    }),
    step("deposit-paid", "loan", false, 0, {
      title: "Pay the vault from Ironwood",
      detail: `The demo wallet pays the vault ${taz} in one Ironwood → Ironwood transaction on Zcash testnet.`,
      hint: "Press “Skip the backup (demo key)”, then “Pay it with demo TAZ”.",
      href: "/borrow",
      artifacts: [{ label: "The deposit on Zcash", chain: "zcash", tx: null, kind: "tx" }],
    }),
    step("deposit-seen", "loan", true, 4_000, {
      title: "Deposit confirmed on Zcash",
      detail: "The signers see the deposit confirmed, then a proof that it is in a Zcash block is made.",
      hint: "Confirmations come in; the proof is being made.",
      href: "/borrow",
      artifacts: [],
    }),
    step("proven", "loan", true, 9_000, {
      title: "Deposit proven on Base",
      detail: "Base checks the proof against a Zcash header its relay already verified: no one's word is taken.",
      hint: "Base verifies the proof.",
      href: "/borrow",
      artifacts: [
        {
          label: "attestDepositWithProof: the deposit proof, verified on Base",
          chain: "base",
          tx: null,
          kind: "proof",
          meta: { imageId: p.imageIds.deposit, prover: p.prover },
        },
      ],
    }),
    step("activated", "loan", true, 7_000, {
      title: "zZEC minted into Morpho",
      detail: "A few Zcash blocks later the activation finalizes: zZEC is minted straight into Morpho Blue.",
      hint: "The keeper activates the deposit.",
      href: "/borrow",
      artifacts: [{ label: "Activation: zZEC supplied as collateral", chain: "base", tx: null, kind: "tx" }],
    }),
    step("borrowed", "loan", false, 0, {
      title: "Borrow MockUSDC",
      detail: `One sponsored UserOp deploys a fresh Safe, authorizes Zecured on Morpho Blue and borrows ${borrow} MockUSDC; the account never holds ETH.`,
      hint: `Press “Borrow against it”, then “DEMO: fill in ${borrow}” and “Create account and borrow”.`,
      href: "/borrow",
      artifacts: [{ label: "Sponsored UserOp: deploy, authorize, borrow", chain: "base", tx: null, kind: "tx" }],
    }),
    step("destination", "loan", false, 0, {
      title: "Give an Ironwood destination",
      detail: "The signers validate a fresh Ironwood address for the payout; Base only ever sees a salted commitment to it.",
      hint: "On your loan, under “What next?”, press “Send my ZEC back to the demo wallet”.",
      href: position,
      artifacts: [],
    }),
    step("close-executed", "loan", false, 0, {
      title: "Close from collateral",
      detail: "A market maker repays the debt on Morpho and takes its value in ZEC from the vault; the rest is owed to your destination.",
      hint: "Press “Get a market maker's quote”, then “Sign and send to the market maker”.",
      href: position,
      artifacts: [{ label: "closeWithCollateral, sent by the market maker", chain: "base", tx: null, kind: "tx" }],
    }),
    step("payouts-committed", "loan", true, 5_000, {
      title: "Payout committed on Base",
      detail: "The signers commit the payout on Base before signing it: paying anyone else is provable theft.",
      hint: "The signers commit the payout.",
      href: position,
      artifacts: [{ label: "The payout's commitment", chain: "base", tx: null, kind: "tx" }],
    }),
    step("payouts-sent", "loan", true, 5_000, {
      title: "Paid on Zcash",
      detail: p.feePaidAlone
        ? "Three of five signers sign one FROST transaction paying you and the market maker."
        : "Three of five signers sign one FROST transaction paying you, the market maker and the fee.",
      hint: "The signers pay on Zcash.",
      href: position,
      artifacts: [{ label: "The FROST 3-of-5 payout on Zcash", chain: "zcash", tx: null, kind: "tx" }],
    }),
    step("payouts-confirmed", "loan", true, 8_000, {
      title: "Payout proven on Base",
      detail: "A zero-knowledge proof that the payout is in a Zcash block makes every claim of the vault final.",
      hint: "The payout's proof lands on Base.",
      href: position,
      artifacts: [
        { label: "The payout, confirmed and proven on Base", chain: "base", tx: null, kind: "proof", meta: { imageId: p.imageIds.violation, prover: p.prover } },
      ],
    }),
    step("swap-opened", "swap", false, 0, {
      title: "Lock MockUSDC in the swap",
      detail: `Your wallet approves ${swap} MockUSDC and opens an order with the maker's signed quote; the MockUSDC waits in ZecuredSwap.`,
      hint: `On Swap, press “DEMO: fill in ${swap}”, then the one button each time it changes.`,
      href: "/buy",
      artifacts: [
        { label: "MockUSDC approval", chain: "base", tx: null, kind: "tx" },
        { label: "The order, opened on ZecuredSwap", chain: "base", tx: null, kind: "tx" },
      ],
    }),
    step("swap-paid", "swap", true, 6_000, {
      title: "The maker pays on Zcash",
      detail: "The maker pays the ZEC to your Ironwood address, with the order id in the memo.",
      hint: "The maker pays.",
      href: "/buy",
      artifacts: [{ label: "The maker's payment on Zcash", chain: "zcash", tx: null, kind: "tx" }],
    }),
    step("swap-proven", "swap", true, 8_000, {
      title: "Payment proven on Base",
      detail: "The maker proves its payment in zero knowledge; settling waits for a deep enough Zcash header.",
      hint: "The maker proves its payment.",
      href: "/buy",
      artifacts: [
        { label: "The payment proof, verified on Base", chain: "base", tx: null, kind: "proof", meta: { imageId: p.imageIds.payment, prover: p.prover } },
      ],
    }),
    step("swap-settled", "swap", true, 6_000, {
      title: "MockUSDC released to the maker",
      detail: "Settled: you hold shielded ZEC, the maker the MockUSDC; no custodian, no signer in between.",
      hint: "The order settles.",
      href: "/buy",
      artifacts: [{ label: "Settlement", chain: "base", tx: null, kind: "tx" }],
    }),
  ];
}

/** The steps with the hashes the app shows for each (`tx`), from `txs`. */
export function withTx(steps: ReplayStep[], txs: Partial<Record<ReplayStepId, ReplayStep["tx"]>>): ReplayStep[] {
  return steps.map((s) => ({ ...s, tx: txs[s.id] ?? {} }));
}
