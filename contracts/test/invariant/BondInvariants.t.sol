// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {console} from "forge-std/console.sol";
import {ClaimDetails, ClaimStatus, VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {BondHandler, BondSystem} from "./BondHandler.sol";

/// @notice Handler-based invariant tests of the bonds (SignerBondV2) in a bonded deployment on a Base Sepolia fork, the
///         part of the system the main campaign drives through a MOCK: liveness with receipts (R-5), unbonding (partial,
///         full, cancelled, withdrawn), the bonds' reimbursement (T-05), deposits, time and price. The review found that
///         no campaign exercised liveness, unbonding or reimbursement; these invariants hold across all of them:
///         B1 the bond holds exactly its books (I19: no slash happens here, so no bounty);
///         B2 every USDC unit deposited is still slashable, paid as liveness, or withdrawn;
///         B3 a claim the bonds paid was armed by a receipt (R-5), was paid once (I26), and was due;
///         B4 a running unbonding has a ready time, an empty one none;
///         B5 `fullyBonded` means every signer holds `minBond`;
///         B6 the open-vault count the full exit waits on (I17);
///         and every violation the handler checks while acting (refused or wrongly allowed actions, amounts).
contract BondInvariantsTest is ZecuredFixture {
    BondHandler internal handler;

    function _bonded() internal pure override returns (bool) {
        return true;
    }

    function setUp() public override {
        super.setUp();
        _bondAll();
        handler = new BondHandler(
            BondSystem({morpho: MORPHO, registry: registry, intake: intake, bond: bond, usdc: usdc, oracle: oracle}),
            signerKeys,
            signerSet
        );
        handler.recordInitialDeposits(5 * MIN_BOND);
        usdc.transferOwnership(address(handler)); // MOCK mint for the signers' deposits
        oracle.transferOwnership(address(handler)); // MOCK price moves

        bytes4[] memory selectors = new bytes4[](22);
        selectors[0] = BondHandler.open.selector;
        selectors[1] = BondHandler.open.selector;
        selectors[2] = BondHandler.release.selector;
        selectors[3] = BondHandler.release.selector;
        selectors[4] = BondHandler.refund.selector;
        selectors[5] = BondHandler.giveDestination.selector;
        selectors[6] = BondHandler.arm.selector;
        selectors[7] = BondHandler.arm.selector;
        selectors[8] = BondHandler.arm.selector;
        selectors[9] = BondHandler.armForged.selector;
        selectors[10] = BondHandler.slashLiveness.selector;
        selectors[11] = BondHandler.slashLiveness.selector;
        selectors[12] = BondHandler.slashLiveness.selector;
        selectors[13] = BondHandler.pay.selector;
        selectors[14] = BondHandler.reimburse.selector;
        selectors[15] = BondHandler.deposit.selector;
        selectors[16] = BondHandler.requestUnbond.selector;
        selectors[17] = BondHandler.requestUnbond.selector;
        selectors[18] = BondHandler.cancelUnbond.selector;
        selectors[19] = BondHandler.withdraw.selector;
        selectors[20] = BondHandler.warp.selector;
        selectors[21] = BondHandler.setPrice.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
        targetSender(makeAddr("zecured.bondinv.caller"));
    }

    /// Each of the handler's actions runs, in a fixed order, without a violation: liveness with a receipt, the bonds'
    /// reimbursement, a partial unbonding withdrawn once ready, another cancelled, forged receipts refused.
    function test_handlerSmoke_bonds() public {
        handler.open(1);
        handler.open(2);
        handler.release(0);
        handler.release(1);
        handler.armForged(0, 0);
        handler.armForged(0, 1);
        handler.armForged(0, 2);
        handler.slashLiveness(0, true); // not armed: refused
        handler.arm(0, 3, 0);
        handler.slashLiveness(0, true); // armed: paid once due
        handler.slashLiveness(0, true); // never twice
        handler.reimburse(0);
        handler.pay(0, true); // the bonds' reimbursement, committed and confirmed
        handler.requestUnbond(1, 5e6, false);
        handler.withdraw(1, true);
        handler.requestUnbond(2, 5e6, false);
        handler.cancelUnbond(2);
        handler.refund(1);
        handler.giveDestination(2, 1);
        handler.arm(2, 4, type(uint256).max);
        handler.slashLiveness(2, true);
        assertFalse(handler.violated(), handler.violation());
        assertGe(handler.calls("liveness"), 2, "liveness paid");
        assertGe(handler.calls("liveness (refused)"), 2, "refused without a receipt, and a second time");
        assertEq(handler.calls("reimburse destination"), 1);
        assertEq(handler.calls("confirm"), 1, "the reimbursement paid");
        assertEq(handler.calls("withdraw"), 1);
        assertEq(handler.calls("unbond (cancelled)"), 1);
        invariant_B1_theBondHoldsItsBooks();
        invariant_B2_everyUnitIsAccounted();
        invariant_B3_bondPaidClaimsWereArmedOnce();
        invariant_B6_openVaultCount();
    }

    /// B1 (I19).
    function invariant_B1_theBondHoldsItsBooks() public view {
        assertEq(usdc.balanceOf(address(bond)), bond.totalSlashable(), "B1: USDC held == bonds + unbondings");
    }

    /// B2.
    function invariant_B2_everyUnitIsAccounted() public view {
        assertEq(
            handler.totalDeposited(),
            bond.totalSlashable() + handler.totalLivenessPaid() + handler.totalWithdrawn(),
            "B2: deposited == slashable + liveness paid + withdrawn"
        );
    }

    /// B3 (R-5, I26).
    function invariant_B3_bondPaidClaimsWereArmedOnce() public view {
        for (uint256 i; i < handler.claimCount(); ++i) {
            bytes32 claimId = handler.claimIds(i);
            ClaimDetails memory c = registry.claimDetails(claimId);
            if (c.bondPaid) {
                (, uint64 payableAt) = bond.livenessArming(claimId);
                assertGt(payableAt, 0, "B3: a claim the bonds paid without a receipt");
                assertEq(handler.paidCount(claimId), 1, "B3: paid once");
            } else {
                assertEq(handler.paidCount(claimId), 0, "B3: a payment the Registry does not know");
            }
        }
    }

    /// B4.
    function invariant_B4_unbondingsHaveReadyTimes() public view {
        for (uint8 i; i < 5; ++i) {
            (uint256 amount, uint64 readyAt) = bond.unbondingOf(i);
            assertEq(amount == 0, readyAt == 0, "B4: an unbonding's ready time");
        }
    }

    /// B5.
    function invariant_B5_fullyBonded() public view {
        bool all = true;
        for (uint8 i; i < 5; ++i) {
            if (bond.bondOf(i) < bond.minBond()) all = false;
        }
        assertEq(bond.fullyBonded(), all, "B5");
    }

    /// B6 (I17): pending or active, or closed or cancelled with a claim still pending.
    function invariant_B6_openVaultCount() public view {
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
        assertEq(registry.openVaultCount(), open, "B6");
    }

    /// Every check the handler makes while acting.
    function invariant_handlerChecks() public view {
        assertFalse(handler.violated(), handler.violation());
    }

    function afterInvariant() external view {
        string[20] memory names = [
            "open",
            "release",
            "refund",
            "refund destination",
            "arm",
            "arm (forged)",
            "liveness",
            "liveness (refused)",
            "commit",
            "confirm",
            "reimburse destination",
            "deposit",
            "unbond (partial)",
            "unbond (full)",
            "unbond (refused)",
            "unbond (cancelled)",
            "withdraw",
            "withdraw (not ready)",
            "warp",
            "setPrice"
        ];
        for (uint256 i; i < names.length; ++i) {
            console.log(names[i], handler.calls(names[i]));
        }
    }
}
