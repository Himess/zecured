// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {console} from "forge-std/console.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {VaultStatus, ClaimKind, ClaimStatus, ClaimDetails} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {LineageNote} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {PendingActivation, PendingRefund} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredDisputes} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {IZecuredRefunds} from "../../src/interfaces/v3/IZecuredRefunds.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {MockCapPrice} from "../mocks/MockCapPrice.sol";
import {ZecuredHandler, HandlerSystem} from "./ZecuredHandler.sol";

/// @notice Handler-based invariant tests of v3 against the real Morpho Blue on a Base Sepolia fork: CLAUDE.md I1, I2,
///         I3 (on-chain part), I4 to I9, the review's I11 to I14, Tier 3's I15 to I17, and the v3 round's I18 and
///         I20 to I24 and I27 (docs/INVARIANTS.md). Deposits arrive only as deposit proofs through the Intake (MOCK
///         relay and verifier); the bonds and the slasher are a MOCK the handler drives (halt, bonding, effective bond,
///         cases, freeze), with the coverage cap (T-01) on and an opening fee (T-06). Runs/depth:
///         [profile.default.invariant] in foundry.toml.
contract ZecuredInvariantsTest is ZecuredFixture {
    ZecuredHandler internal handler;
    MockSignerBond internal gates;
    MockCapPrice internal capPrice;

    bytes32 internal constant TREASURY = keccak256("zecured.inv.treasury");
    uint16 internal constant FEE_BPS = 50; // 0.5 %: charged on 0.05 ZEC (25,000 zat), waived on 0.01 ZEC (5,000)
    uint64 internal constant MIN_FEE = 20_000;

    /// The bonds and the slasher: one MOCK the handler drives.
    function _mockBondAndSlasher() internal override returns (address) {
        gates = new MockSignerBond();
        gates.setEffectiveBond(20e6); // 20 USDC covers 0.5 ZEC at $40; the handler moves it
        return address(gates);
    }

    /// T-01 on: k = 100 %, at most 0.5 ZEC locked and pending, a cap price fresh for an hour.
    function _capSetup() internal override returns (CapSetup memory) {
        capPrice = new MockCapPrice(PRICE_40);
        return
            CapSetup({
                coverageBps: 10_000, maxTotalZat: 50_000_000, capPriceMaxAge: 1 hours, capOracle: address(capPrice)
            });
    }

    /// T-06 on.
    function _feeSetup() internal pure override returns (FeeSetup memory) {
        return FeeSetup({feeBps: FEE_BPS, minFeeZat: MIN_FEE, feeCommitment: TREASURY});
    }

    function setUp() public override {
        super.setUp();
        handler = new ZecuredHandler(
            HandlerSystem({
                morpho: MORPHO,
                registry: registry,
                adapter: adapter,
                intake: intake,
                usdc: usdc,
                oracle: oracle,
                relay: MockHeaderRelay(address(headerRelay)),
                verifier: MockRiscZeroVerifier(address(proofVerifier)),
                imageId: proofs.imageId,
                gates: gates,
                capPrice: capPrice
            }),
            signerKeys
        );
        usdc.transferOwnership(address(handler)); // MOCK mint for the handler's actors
        oracle.transferOwnership(address(handler)); // MOCK price moves

        bytes4[] memory selectors = new bytes4[](62);
        selectors[0] = ZecuredHandler.prove.selector;
        selectors[1] = ZecuredHandler.prove.selector; // weighted: deposits first
        selectors[2] = ZecuredHandler.prove.selector;
        selectors[3] = ZecuredHandler.finalize.selector;
        selectors[4] = ZecuredHandler.finalize.selector;
        selectors[5] = ZecuredHandler.drop.selector;
        selectors[6] = ZecuredHandler.refundPending.selector;
        selectors[7] = ZecuredHandler.pause.selector;
        selectors[8] = ZecuredHandler.unpause.selector;
        selectors[9] = ZecuredHandler.setGates.selector;
        selectors[10] = ZecuredHandler.setGates.selector;
        selectors[11] = ZecuredHandler.giveRefundDestination.selector;
        selectors[12] = ZecuredHandler.abandon.selector;
        selectors[13] = ZecuredHandler.writeOffDust.selector;
        selectors[14] = ZecuredHandler.borrow.selector;
        selectors[15] = ZecuredHandler.repay.selector;
        selectors[16] = ZecuredHandler.release.selector;
        selectors[17] = ZecuredHandler.partialRelease.selector;
        selectors[18] = ZecuredHandler.setPrice.selector;
        selectors[19] = ZecuredHandler.warp.selector;
        selectors[20] = ZecuredHandler.liquidate.selector;
        selectors[21] = ZecuredHandler.close.selector;
        selectors[22] = ZecuredHandler.deleverage.selector;
        selectors[23] = ZecuredHandler.confirm.selector;
        selectors[24] = ZecuredHandler.confirm.selector;
        selectors[25] = ZecuredHandler.commitOnly.selector;
        selectors[26] = ZecuredHandler.attackClaimReuse.selector;
        selectors[27] = ZecuredHandler.attackUncommittedConfirm.selector;
        selectors[28] = ZecuredHandler.attackUnsetDestination.selector;
        selectors[29] = ZecuredHandler.attackRefundDestination.selector;
        selectors[30] = ZecuredHandler.attackDoubleLanding.selector;
        selectors[31] = ZecuredHandler.attackWeakOpened.selector;
        selectors[32] = ZecuredHandler.attackActivation.selector;
        selectors[33] = ZecuredHandler.attackAccountWithdraw.selector;
        selectors[34] = ZecuredHandler.attackDirectLiquidation.selector;
        selectors[35] = ZecuredHandler.attackPartialRelease.selector;
        selectors[36] = ZecuredHandler.attackFlashLoan.selector;
        selectors[37] = ZecuredHandler.attackDeleverage.selector;
        selectors[38] = ZecuredHandler.attackCloseRequest.selector;
        selectors[39] = ZecuredHandler.release.selector;
        selectors[40] = ZecuredHandler.dispute.selector;
        selectors[41] = ZecuredHandler.resolveDispute.selector;
        selectors[42] = ZecuredHandler.dispute.selector; // weighted: a dispute needs a confirmed claim first
        selectors[43] = ZecuredHandler.resolveDispute.selector;
        // v4 pre-liquidation
        selectors[44] = ZecuredHandler.setPreLiquidationTerms.selector;
        selectors[45] = ZecuredHandler.preLiquidate.selector;
        selectors[46] = ZecuredHandler.preLiquidate.selector; // weighted: a pre-liquidation needs terms and a loan first
        selectors[47] = ZecuredHandler.attackPreLiquidation.selector;
        // v4 refund lifecycle
        selectors[48] = ZecuredHandler.finalizeRefund.selector;
        selectors[49] = ZecuredHandler.finalizeRefund.selector; // weighted: most refunds open
        selectors[50] = ZecuredHandler.dropRefund.selector;
        selectors[51] = ZecuredHandler.markDormant.selector;
        selectors[52] = ZecuredHandler.attackSpendPendingRefund.selector;
        selectors[53] = ZecuredHandler.giveRefundDestination.selector;
        selectors[54] = ZecuredHandler.proveSecond.selector;
        selectors[55] = ZecuredHandler.proveSecond.selector;
        selectors[56] = ZecuredHandler.markDormant.selector;
        // v4: proven delivery, expired confirmations (R-3), replays and final claims (R-1).
        selectors[57] = ZecuredHandler.provePayout.selector;
        selectors[58] = ZecuredHandler.provePayout.selector;
        selectors[59] = ZecuredHandler.expireConfirmation.selector;
        selectors[60] = ZecuredHandler.attackReplayOrFinal.selector;
        // W-2: F1 cases about the notes of pending deposits (a usurped tag).
        selectors[61] = ZecuredHandler.usurpedTagCase.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
        // The handler pranks its own actors, so the caller does not matter. A fixed caller matters on a fork: every
        // new random sender would be an RPC round trip.
        targetSender(makeAddr("zecured.inv.caller"));
    }

    /// The refund lifecycle's actions (R-8, R-11, R-12) run, in a fixed order, without a violation: the campaign
    /// explores them at random, this shows each of them does what the handler checks.
    /// W-2: an F1 case about a pending deposit's note (its tag usurped by a false change record) keeps the note from
    /// backing a mint; the deposit can still be taken back as a refund.
    function test_handlerSmoke_usurpedTag() public {
        handler.prove(1, false, false); // a vault, pending
        uint256 seed;
        while (uint256(keccak256(abi.encode(seed))) % 4 != 0) ++seed; // the handler's op 0: a case opens on the tag
        handler.usurpedTagCase(seed);
        handler.finalize(0);
        assertEq(handler.calls("finalize"), 0, "no mint against a note an F1 case is about");
        assertEq(handler.calls("finalize (gated)"), 1);
        handler.refundPending(0, true);
        assertFalse(handler.violated(), handler.violation());
        invariant_I13_I20_eachNoteLandsOnce();
        invariant_R8_noClaimOnAPendingVault();
    }

    function test_handlerSmoke_refundLifecycle() public {
        handler.prove(1, false, false); // a vault, pending
        handler.prove(2, false, false);
        handler.proveSecond(3); // a second note: a pending refund, while the vault pends (R-8)
        handler.finalizeRefund(0); // waits for the vault, or opens
        handler.finalize(0);
        handler.finalize(1);
        handler.proveSecond(4);
        handler.proveSecond(5);
        handler.finalizeRefund(0);
        handler.finalizeRefund(1);
        handler.dropRefund(0);
        handler.markDormant(0);
        handler.giveRefundDestination(0);
        handler.markDormant(1);
        handler.abandon(1);
        handler.giveRefundDestination(1);
        handler.attackSpendPendingRefund(0);
        handler.refundPending(0, true);
        assertFalse(handler.violated(), handler.violation());
        assertFalse(handler.attackSucceeded());
        assertGt(handler.calls("finalize refund") + handler.calls("finalize refund (vault pending)"), 0);
        assertGt(handler.calls("mark dormant"), 0, "a refund went dormant");
        assertGt(handler.calls("refund destination"), 0, "a dormant refund was woken");
        invariant_R11_dormantRefunds();
        invariant_disputes_andLockedZatoshi();
        invariant_I13_I20_eachNoteLandsOnce();
        invariant_I15_liveNotesHoldWhatBaseOwes();
        invariant_R8_noClaimOnAPendingVault();
    }

    // ------------------------------------------------------------------ supply and custody

    /// I1: zZEC total supply == sum of outstanding over active vaults; only an active vault has outstanding.
    function invariant_I1_supplyEqualsActiveOutstanding() public view {
        uint256 sum;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            (, uint64 outstanding, VaultStatus status) = registry.vault(handler.vaultIds(i));
            if (status == VaultStatus.Active) sum += outstanding;
            else assertEq(outstanding, 0, "a vault that is not active has outstanding");
        }
        assertEq(zzec.totalSupply(), sum, "I1");
    }

    /// I2: at rest all zZEC is in Morpho; the Registry, the Adapter, the Intake and every actor hold none; no
    /// expectation is left armed.
    function invariant_I2_zzecOnlyInMorpho() public view {
        assertEq(zzec.balanceOf(address(MORPHO)), zzec.totalSupply(), "I2: zZEC outside Morpho");
        assertEq(zzec.balanceOf(address(registry)), 0, "I2: registry");
        assertEq(zzec.balanceOf(address(adapter)), 0, "I2: adapter");
        assertEq(zzec.balanceOf(address(intake)), 0, "I2: intake");
        assertEq(zzec.balanceOf(handler.liquidator()), 0, "I2: liquidator");
        assertEq(zzec.balanceOf(handler.marketMaker()), 0, "I2: market maker");
        assertEq(zzec.balanceOf(handler.stranger()), 0, "I2: stranger");
        for (uint256 i; i < 8; ++i) {
            assertEq(zzec.balanceOf(handler.accounts(i)), 0, "I2: account");
        }
        (address recipient,) = zzec.pendingExpectation();
        assertEq(recipient, address(0), "expectation left armed");
    }

    // ------------------------------------------------------------------ conservation

    /// I3 (Base side), I9, I11 and I12, generalized to refunds: everything that landed on a vault (its activating
    /// deposit and every refunded deposit) is exactly its outstanding plus its claims, of every kind and status; the
    /// outstanding never exceeds the activating deposit (I9); Morpho's collateral for an activated vault's account is
    /// its outstanding (I12).
    function invariant_I3_I9_I11_I12_everyZatoshiIsAccounted() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (, uint64 outstanding,) = registry.vault(vaultId);
            uint256 claimed;
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                (bytes32 claimVault,, uint64 amount,,) = registry.claim(registry.claimIdOf(vaultId, j));
                assertEq(claimVault, vaultId, "a claim of another vault");
                claimed += amount;
            }
            uint64 activated = handler.activated(vaultId);
            assertEq(claimed + outstanding, uint256(activated) + handler.refunded(vaultId), "I11: landed == owed");
            assertLe(outstanding, activated, "I9");
            if (activated != 0) {
                assertEq(
                    _position(handler.accountOf(vaultId)).collateral, outstanding, "I12: collateral == outstanding"
                );
            }
        }
        assertFalse(handler.i9Violated(), "I9 / D11 violated by a liquidation or a deleverage");
    }

    /// I27 (T-06): an activated vault owes exactly one Fee claim of `amount * feeBps / 10,000` to the treasury when that
    /// reaches `minFeeZat`, and none otherwise; a vault that never activated owes no fee.
    function invariant_I27_openingFee() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint256 fees;
            uint256 feeClaims;
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                (, ClaimKind kind, uint64 amount, bytes32 commitment,) = registry.claim(registry.claimIdOf(vaultId, j));
                if (kind != ClaimKind.Fee) continue;
                fees += amount;
                ++feeClaims;
                assertEq(commitment, TREASURY, "I27: the fee goes to the treasury");
            }
            uint256 due = uint256(handler.activated(vaultId)) * FEE_BPS / 10_000;
            if (due >= MIN_FEE) {
                assertEq(feeClaims, 1, "I27: one Fee claim");
                assertEq(fees, due, "I27: the fee");
            } else {
                assertEq(feeClaims, 0, "I27: a fee below the floor is waived");
            }
        }
    }

    /// I15 (Tier 3, Base side of I10), generalized: the vault's lineage notes that no committed payout spent hold
    /// exactly what Base still owes from the vault: its outstanding, its pending activation, and every claim no payout
    /// was committed for, pending or written off (an abandoned refund's or a dust claim's zatoshi stay in the vault).
    function invariant_I15_liveNotesHoldWhatBaseOwes() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
            uint256 owed = outstanding + handler.pendingRefundOf(vaultId); // R-12: pending refunds' notes
            if (status == VaultStatus.Pending) owed += intake.pendingActivation(vaultId).amount;
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                (,, uint64 amount,, ClaimStatus s) = registry.claim(claimId);
                bool uncommitted = registry.claimInput(claimId) == bytes32(0);
                if (uncommitted && (s == ClaimStatus.Pending || s == ClaimStatus.WrittenOff)) owed += amount;
            }
            uint256 held;
            bytes32[] memory notes = handler.liveNotesOf(vaultId);
            for (uint256 n; n < notes.length; ++n) {
                LineageNote memory note = registry.lineageOf(notes[n]);
                assertEq(note.vaultId, vaultId, "I15: a note of another vault");
                held += note.value;
            }
            assertEq(held, owed, "I15: live notes == outstanding + pending activation + uncommitted claims");
        }
    }

    /// I16 (Tier 3): every claim confirmed paid was committed, against a lineage note of its own vault.
    /// I21 (T-02, T-05): a claim without a destination is never committed or paid.
    function invariant_I16_I21_paidClaimsWereCommittedWithADestination() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                (,,, bytes32 commitment, ClaimStatus status) = registry.claim(claimId);
                bytes32 input = registry.claimInput(claimId);
                if (status == ClaimStatus.Paid || status == ClaimStatus.Disputed) {
                    assertTrue(input != bytes32(0), "I16: paid (or disputed) without a commitment");
                }
                if (input != bytes32(0)) {
                    assertEq(registry.lineageOf(input).vaultId, vaultId, "I16: a foreign input");
                    assertTrue(commitment != bytes32(0), "I21: committed without a destination");
                }
            }
        }
    }

    // ------------------------------------------------------------------ bookkeeping

    /// I4 / I5: one account <-> one vault. A pending or activated vault is its account's vault; any vault an account is
    /// bound to has that account; a refund never binds one.
    function invariant_I4_I5_bookkeeping() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (address account,, VaultStatus status) = registry.vault(vaultId);
            if (status == VaultStatus.None) continue; // a dropped reservation
            assertEq(account, handler.accountOf(vaultId), "the vault's account");
            if (handler.activated(vaultId) != 0 || status == VaultStatus.Pending) {
                assertEq(registry.vaultOf(account), vaultId, "I4");
            }
        }
        for (uint256 i; i < 8; ++i) {
            address account = handler.accounts(i);
            bytes32 bound = registry.vaultOf(account);
            if (bound == bytes32(0)) continue;
            (address boundAccount,,) = registry.vault(bound);
            assertEq(boundAccount, account, "I4: bound to a vault of another account");
        }
    }

    /// I13 and I20 (T-01, T-02, R-12): every note the Intake accepted lands exactly once: as a pending reservation, an
    /// activation, a pending refund or a refund, its tag used and a lineage note of its vault for good; a reservation
    /// or a pending refund a reorganisation dropped frees its tag. `pendingTotal` is the sum of the pending
    /// activations, `pendingRefundTotal` the sum of the pending refunds, each reserved in the Registry for its account.
    function invariant_I13_I20_eachNoteLandsOnce() public view {
        uint256 pendingSum;
        uint256 pendingRefundSum;
        for (uint256 i; i < handler.tagCount(); ++i) {
            bytes32 tag = handler.tags(i);
            uint8 state = handler.tagState(tag);
            bytes32 vaultId = handler.tagVault(tag);
            if (state == handler.TAG_DROPPED()) {
                assertFalse(registry.depositTagUsed(tag), "a dropped reservation keeps its tag");
                assertEq(registry.lineageOf(tag).vaultId, bytes32(0), "a dropped reservation keeps its lineage");
                continue;
            }
            assertTrue(registry.depositTagUsed(tag), "I13: a landed tag is used");
            assertEq(registry.lineageOf(tag).vaultId, vaultId, "I20: a landed note is a lineage note of its vault");
            if (state == handler.TAG_PENDING()) {
                PendingActivation memory p = intake.pendingActivation(vaultId);
                assertEq(p.depositTag, tag, "I20: the vault's reservation");
                pendingSum += p.amount;
            }
            address reservedFor = IZecuredRefunds(address(registry)).reservedRefundOf(tag);
            if (state == handler.TAG_REFUND_PENDING()) {
                PendingRefund memory r = intake.pendingRefund(tag);
                assertEq(r.vaultId, vaultId, "R-12: the pending refund's vault");
                assertEq(reservedFor, r.account, "R-12: reserved in the Registry for its account");
                pendingRefundSum += r.amount;
            } else {
                assertEq(reservedFor, address(0), "R-12: only a pending refund is reserved");
            }
        }
        assertEq(intake.pendingTotal(), pendingSum, "I20: pendingTotal == sum of the pending activations");
        assertEq(
            intake.pendingRefundTotal(), pendingRefundSum, "R-12: pendingRefundTotal == sum of the pending refunds"
        );
    }

    /// R-8: no claim ever exists on a pending vault (a refund proven meanwhile waits for it), so none accrues liveness
    /// where no payout may be committed.
    function invariant_R8_noClaimOnAPendingVault() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (,, VaultStatus status) = registry.vault(vaultId);
            if (status == VaultStatus.Pending) {
                assertEq(registry.claimCount(vaultId), 0, "R-8: a claim on a pending vault");
            }
        }
    }

    /// R-11: a dormant claim is a pending Refund claim without a destination, not paid by the bonds, and exactly the
    /// handler's dormant claims are.
    function invariant_R11_dormantRefunds() public view {
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                bool isDormant = refunds.isRefundDormant(claimId);
                assertEq(isDormant, handler.dormant(claimId), "R-11: dormant as the handler made it");
                if (!isDormant) continue;
                ClaimDetails memory c = registry.claimDetails(claimId);
                assertEq(uint8(c.kind), uint8(ClaimKind.Refund), "R-11: only refunds go dormant");
                assertEq(uint8(c.status), uint8(ClaimStatus.Pending), "R-11: a dormant refund is pending");
                assertEq(c.commitment, bytes32(0), "R-11: a dormant refund has no destination");
                assertFalse(c.bondPaid, "R-11: never one the bonds paid");
            }
        }
    }

    /// I14 (v3): Cancelled <=> never activated. A cancelled vault has no outstanding and only Refund claims; an
    /// activated vault is active or closed for good.
    function invariant_I14_cancelledIffNeverActivated() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
            if (handler.activated(vaultId) != 0) {
                assertTrue(status == VaultStatus.Active || status == VaultStatus.Closed, "I14: activated vault");
            }
            if (status != VaultStatus.Cancelled) continue;
            assertEq(handler.activated(vaultId), 0, "I14: a cancelled vault was activated");
            assertEq(outstanding, 0, "I14: a cancelled vault with outstanding");
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                (, ClaimKind kind,,,) = registry.claim(registry.claimIdOf(vaultId, j));
                assertEq(uint8(kind), uint8(ClaimKind.Refund), "I14: a cancelled vault owes only refunds");
            }
        }
    }

    /// I17 (Tier 3, S5): the open-vault count the bonds wait on is the number of vaults that are pending or active, or
    /// closed or cancelled with a claim still pending or disputed.
    function invariant_I17_openVaultCount() public view {
        uint256 open;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (,, VaultStatus status) = registry.vault(vaultId);
            bool unpaid;
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                (,,,, ClaimStatus s) = registry.claim(registry.claimIdOf(vaultId, j));
                if (s == ClaimStatus.Pending || s == ClaimStatus.Disputed) unpaid = true;
            }
            if (status == VaultStatus.Active || status == VaultStatus.Pending || unpaid) ++open;
        }
        assertEq(registry.openVaultCount(), open, "I17");
    }

    /// T-05 (a) and the cap's basis (T-01): a disputed claim has an inclusion deadline and no confirmation time; only
    /// a confirmed claim has a confirmation time; `lockedZatoshi` is the zZEC supply plus every pending or disputed
    /// claim that is not a dormant refund (R-11).
    function invariant_disputes_andLockedZatoshi() public view {
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        uint256 unpaid;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                (,, uint64 amount,, ClaimStatus s) = registry.claim(claimId);
                (uint64 paidAt, uint64 dueAt) = disputes.payoutDispute(claimId);
                if (s == ClaimStatus.Disputed) {
                    assertTrue(dueAt != 0 && paidAt == 0, "a disputed claim's times");
                } else {
                    assertEq(dueAt, 0, "an inclusion deadline outside a dispute");
                }
                if (paidAt != 0) assertEq(uint8(s), uint8(ClaimStatus.Paid), "a confirmation time on an unpaid claim");
                bool counted =
                    (s == ClaimStatus.Pending || s == ClaimStatus.Disputed) && !refunds.isRefundDormant(claimId);
                if (counted) unpaid += amount;
            }
        }
        assertEq(registry.lockedZatoshi(), zzec.totalSupply() + unpaid, "lockedZatoshi");
    }

    /// v4 (R-3, proven delivery): a vault's `unprovenConfirmations` counts exactly its claims confirmed by signatures
    /// and not yet final (Paid with `paidAt != 0`); a final claim (`paidAt == 0`) is never counted.
    function invariant_unprovenConfirmations() public view {
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint256 open;
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                (,,,, ClaimStatus s) = registry.claim(claimId);
                (uint64 paidAt,) = disputes.payoutDispute(claimId);
                if (s == ClaimStatus.Paid && paidAt != 0) ++open;
            }
            assertEq(IZecuredPayouts(address(registry)).unprovenConfirmations(vaultId), open, "unproven confirmations");
        }
    }

    /// The v4 actions do reach their success paths (the review found the campaign never exercised payout proofs): one
    /// deterministic sequence through the handler, a pending claim proven, a confirmation proven, another expired.
    function test_handler_exercisesTheV4PayoutPaths() public {
        uint256 confirmed = _seedWithMix(0);
        uint256 pendingOnly = _seedWithMix(1);
        for (uint256 i; handler.pendingCount() < 2 && i < 40; ++i) {
            handler.prove(i, false, false);
        }
        assertEq(handler.pendingCount(), 2, "two deposits pending");
        handler.finalize(0);
        handler.finalize(0);
        // Vault 0: a partial release whose payout is committed, then proven before anyone confirms it.
        handler.partialRelease(0, 0);
        handler.provePayout(0, pendingOnly);
        assertEq(handler.calls("prove pending"), 1, "a committed pending claim proven");
        // Vault 1: a release committed and confirmed, then proven.
        handler.release(1);
        handler.confirm(1, 7, false);
        assertEq(handler.calls("confirm"), 1);
        handler.provePayout(1, confirmed);
        assertEq(handler.calls("prove confirmed"), 1, "a confirmation proven");
        // Vault 0 again: a confirmation nobody disputes expires.
        handler.release(0);
        handler.confirm(0, 3, false);
        assertEq(handler.calls("confirm"), 2);
        handler.expireConfirmation(0);
        assertEq(handler.calls("expire"), 1, "an old confirmation expired");
        assertFalse(handler.violated(), handler.violation());
        assertFalse(handler.attackSucceeded());
    }

    /// A seed whose hash (the handler's `_mix`) has the given parity.
    function _seedWithMix(uint256 parity) internal pure returns (uint256 seed) {
        while (uint256(keccak256(abi.encode(seed))) % 2 != parity) ++seed;
    }

    // ------------------------------------------------------------------ violations caught while acting

    /// D14: every partial release left the position at <= 50 % LTV and at least MIN_RELEASE in the vault, kept the
    /// vault Active and created a Release claim of exactly the released amount.
    function invariant_D14_partialReleaseKeepsLtvAtMostFiftyPercent() public view {
        assertFalse(handler.d14Violated(), "D14");
    }

    /// I7: every close burned exactly the outstanding and split it exactly into the two claims.
    function invariant_I7_closeConservation() public view {
        assertFalse(handler.i7Violated(), "I7");
    }

    /// I18 (a reservation only within the coverage cap), I22 (no gate stops an exit or a payout), I23 (no mint while
    /// halted, paused, underbonded, or with a case open on the vault; a deposit proof never mints), I24 (no exit from a
    /// frozen vault), and no unexpected revert of an honest action.
    function invariant_I18_I22_I23_I24_gatesHoldWhileActing() public view {
        assertFalse(handler.violated(), handler.violation());
    }

    /// No attack ever succeeded: a claim paid twice or confirmed without a commitment, a refund committed without its
    /// destination or redirected by a stranger, one note landing twice, a deposit without all five VaultOpened
    /// signatures, an early or direct activation, an account withdrawing its own collateral, a direct Morpho
    /// liquidation, a zZEC flash loan, a partial release by anyone but the account, a close or deleverage request
    /// executed by a stranger or after its deadline, a payout spending a pending refund's note (R-12).
    function invariant_attacksAlwaysFail() public view {
        assertFalse(handler.attackSucceeded(), "an attack succeeded");
    }

    function afterInvariant() external view {
        string[73] memory names = [
            "prove: pending",
            "prove: refund (known vault)",
            "prove: refund (account bound)",
            "prove: refund (halted)",
            "prove: cap full (waits)",
            "prove (stale cap price)",
            "prove (compromised vault)",
            "finalize",
            "finalize (gated)",
            "drop",
            "drop (case open)",
            "refundPending",
            "refundPending (refused)",
            "finalize refund",
            "finalize refund (vault pending)",
            "finalize refund (compromised)",
            "finalize refund (anchor gone)",
            "drop refund",
            "drop refund (case open)",
            "mark dormant",
            "pause",
            "unpause",
            "gates",
            "refund destination",
            "abandon",
            "write off dust",
            "borrow",
            "repay",
            "release",
            "release (frozen)",
            "partialRelease",
            "partialRelease (reverted)",
            "setPrice",
            "warp",
            "liquidate",
            "liquidate (reverted)",
            "close",
            "close (reverted)",
            "deleverage",
            "deleverage (reverted)",
            "commit",
            "commit only",
            "confirm",
            "confirm (frozen)",
            "prove pending",
            "prove confirmed",
            "expire",
            "expire (suspect)",
            "dispute",
            "uphold",
            "uphold late",
            "reopen",
            "attack: claim reuse",
            "attack: uncommitted confirm",
            "attack: unset destination",
            "attack: refund destination",
            "attack: double landing",
            "attack: weak VaultOpened",
            "attack: activation",
            "attack: account withdraw",
            "attack: direct liquidation",
            "attack: partial release",
            "attack: flash loan",
            "attack: deleverage",
            "attack: close request",
            "preLiquidation terms set",
            "preLiquidation terms cleared",
            "preLiquidate",
            "preLiquidate (refused)",
            "attack: pre-liquidation",
            "attack: spend pending refund",
            "attack: replay or final",
            "usurped tag case"
        ];
        for (uint256 i; i < names.length; ++i) {
            console.log(names[i], handler.calls(names[i]));
        }
    }
}
