// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {console} from "forge-std/console.sol";
import {VaultStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {ZecuredHandler} from "./ZecuredHandler.sol";

/// @notice Handler-based invariant tests of CLAUDE.md I1, I2, I3 (on-chain part), I4, I5, I6, I7, I8 and I9 against the
///         real Morpho Blue on a Base Sepolia fork. Runs/depth: [profile.default.invariant] in foundry.toml.
contract ZecuredInvariantsTest is ZecuredFixture {
    ZecuredHandler internal handler;

    function setUp() public override {
        super.setUp();
        handler = new ZecuredHandler(MORPHO, registry, adapter, usdc, oracle, signerKeys);
        usdc.transferOwnership(address(handler)); // MOCK mint for the handler's actors
        oracle.transferOwnership(address(handler)); // MOCK price moves

        bytes4[] memory selectors = new bytes4[](15);
        selectors[0] = ZecuredHandler.attest.selector;
        selectors[1] = ZecuredHandler.attest.selector; // weighted: vaults first
        selectors[2] = ZecuredHandler.borrow.selector;
        selectors[3] = ZecuredHandler.repay.selector;
        selectors[4] = ZecuredHandler.release.selector;
        selectors[5] = ZecuredHandler.setPrice.selector;
        selectors[6] = ZecuredHandler.warp.selector;
        selectors[7] = ZecuredHandler.liquidate.selector;
        selectors[8] = ZecuredHandler.close.selector;
        selectors[9] = ZecuredHandler.confirm.selector;
        selectors[10] = ZecuredHandler.attackAccountWithdraw.selector;
        selectors[11] = ZecuredHandler.attackDirectLiquidation.selector;
        selectors[12] = ZecuredHandler.attackFlashLoan.selector;
        selectors[13] = ZecuredHandler.attackWeakAttestation.selector;
        selectors[14] = ZecuredHandler.attackCloseRequest.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
        // The handler pranks its own actors, so the caller does not matter. A fixed caller matters on a fork: every
        // new random sender would be an RPC round trip.
        targetSender(makeAddr("zecured.inv.caller"));
    }

    /// I1: zZEC total supply == sum of outstanding over active vaults.
    function invariant_I1_supplyEqualsActiveOutstanding() public view {
        uint256 sum;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            (, uint64 outstanding, VaultStatus status) = registry.vault(handler.vaultIds(i));
            if (status == VaultStatus.Active) sum += outstanding;
            else assertEq(outstanding, 0, "closed vault with outstanding");
        }
        assertEq(zzec.totalSupply(), sum, "I1");
    }

    /// I2: at rest all zZEC is in Morpho; Registry, Adapter and every actor hold none; no expectation left armed.
    function invariant_I2_zzecOnlyInMorpho() public view {
        assertEq(zzec.balanceOf(address(MORPHO)), zzec.totalSupply(), "I2: zZEC outside Morpho");
        assertEq(zzec.balanceOf(address(registry)), 0, "I2: registry");
        assertEq(zzec.balanceOf(address(adapter)), 0, "I2: adapter");
        assertEq(zzec.balanceOf(handler.liquidator()), 0, "I2: liquidator");
        assertEq(zzec.balanceOf(handler.marketMaker()), 0, "I2: market maker");
        assertEq(zzec.balanceOf(handler.stranger()), 0, "I2: stranger");
        for (uint256 i; i < 6; ++i) {
            assertEq(zzec.balanceOf(handler.accounts(i)), 0, "I2: account");
        }
        (address recipient,) = zzec.pendingExpectation();
        assertEq(recipient, address(0), "expectation left armed");
    }

    /// I3 (Base side) and I9: every vault's attested amount is exactly its outstanding plus its claims (nothing left
    /// Morpho without a burn and a claim), outstanding never exceeds the attested amount, and Morpho's collateral for
    /// the account always equals the outstanding.
    function invariant_I3_I9_burnsAreClaimedAndCollateralMatches() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (address account, uint64 outstanding,) = registry.vault(vaultId);
            uint256 claimed;
            for (uint64 j; j < registry.claimCount(vaultId); ++j) {
                (bytes32 claimVault,, uint64 amount,,) = registry.claim(registry.claimIdOf(vaultId, j));
                assertEq(claimVault, vaultId, "claim of another vault");
                claimed += amount;
            }
            assertEq(claimed + outstanding, handler.attested(vaultId), "I3: attested == outstanding + claims");
            assertLe(outstanding, handler.attested(vaultId), "I9");
            assertEq(_position(account).collateral, outstanding, "I9: Morpho collateral == outstanding");
        }
        assertFalse(handler.i9Violated(), "I9 / D11 violated by a liquidation");
    }

    /// I7: every close burned exactly the outstanding and split it exactly into the two claims.
    function invariant_I7_closeConservation() public view {
        assertFalse(handler.i7Violated(), "I7");
    }

    /// I4 / I5: one account <-> one vault; vault ids and deposit tags single-use (vaultOf is consistent).
    function invariant_I4_I5_bookkeeping() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            (address account,,) = registry.vault(vaultId);
            assertEq(registry.vaultOf(account), vaultId, "I4");
        }
    }

    /// I2 / I6 / I8: no attack ever succeeded (account self-withdrawal, direct Morpho liquidation, zZEC flash loan,
    /// attestation with < 3 distinct signers, close request executed by a stranger or after its deadline).
    function invariant_attacksAlwaysFail() public view {
        assertFalse(handler.attackSucceeded(), "an attack succeeded");
    }

    function afterInvariant() external view {
        string[16] memory names = [
            "attest",
            "borrow",
            "repay",
            "release",
            "setPrice",
            "warp",
            "liquidate",
            "liquidate (reverted)",
            "close",
            "close (reverted)",
            "confirm",
            "attack: account withdraw",
            "attack: direct liquidation",
            "attack: flash loan",
            "attack: weak attestation",
            "attack: close request"
        ];
        for (uint256 i; i < names.length; ++i) {
            console.log(names[i], handler.calls(names[i]));
        }
    }
}
