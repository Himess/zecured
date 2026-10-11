// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {VaultStatus, ClaimStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {DepositAttestation, CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";

/// @notice Invariant-test handler: drives vault lifecycles (attest, borrow, repay, release, price moves, liquidation,
///         close, payout confirmation, time) and attacks that must always fail, against the real Morpho Blue on a
///         Base Sepolia fork. Ghost flags record any I7 / I9 / attack violation at the moment it happens.
/// @dev A fixed set of actors keeps the set of Morpho storage slots read from the fork small and cacheable.
contract ZecuredHandler is Test {
    using MorphoBalancesLib for IMorpho;

    IMorpho internal immutable morpho;
    ZecuredRegistry internal immutable registry;
    SettlementAdapter internal immutable adapter;
    ZZEC internal immutable zzec;
    MockUSDC internal immutable usdc;
    MockZecOracle internal immutable oracle;
    MarketParams internal params;
    Id internal marketId;
    uint256[5] internal signerKeys;

    uint256 internal constant ACCOUNT_COUNT = 6;
    address[ACCOUNT_COUNT] public accounts;
    uint256[ACCOUNT_COUNT] internal accountKeys;
    address public immutable liquidator = makeAddr("zecured.inv.liquidator");
    address public immutable marketMaker = makeAddr("zecured.inv.marketMaker");
    address public immutable stranger = makeAddr("zecured.inv.stranger");

    // ------------------------------------------------------------------ ghost state

    bytes32[] public vaultIds;
    bytes32[] public cancelledIds;
    mapping(bytes32 vaultId => uint64) public attested;
    mapping(bytes32 vaultId => bytes32) public tagOf;
    mapping(bytes32 vaultId => uint256) public accountIndexOf;
    uint256 public nextAccount;
    uint256 internal nextNonce;

    bool public attackSucceeded;
    bool public i7Violated;
    bool public i9Violated;
    bool public d14Violated;
    mapping(string => uint256) public calls;

    constructor(
        IMorpho morpho_,
        ZecuredRegistry registry_,
        SettlementAdapter adapter_,
        MockUSDC usdc_,
        MockZecOracle oracle_,
        uint256[5] memory signerKeys_
    ) {
        morpho = morpho_;
        registry = registry_;
        adapter = adapter_;
        zzec = ZZEC(address(registry_.zzec()));
        usdc = usdc_;
        oracle = oracle_;
        params = registry_.marketParams();
        marketId = registry_.marketId();
        signerKeys = signerKeys_;
        for (uint256 i; i < ACCOUNT_COUNT; ++i) {
            (accounts[i], accountKeys[i]) = makeAddrAndKey(string.concat("zecured.inv.account", vm.toString(i)));
        }
    }

    function vaultCount() external view returns (uint256) {
        return vaultIds.length;
    }

    function cancelledCount() external view returns (uint256) {
        return cancelledIds.length;
    }

    // ------------------------------------------------------------------ helpers

    function _sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    /// 3 distinct signers in increasing order, chosen by `seed` (indices i < j < k of the sorted set).
    function _threeSigs(bytes32 digest, uint256 seed) internal view returns (bytes[] memory sigs) {
        uint256[3][10] memory subsets = [
            [uint256(0), 1, 2],
            [uint256(0), 1, 3],
            [uint256(0), 1, 4],
            [uint256(0), 2, 3],
            [uint256(0), 2, 4],
            [uint256(0), 3, 4],
            [uint256(1), 2, 3],
            [uint256(1), 2, 4],
            [uint256(1), 3, 4],
            [uint256(2), 3, 4]
        ];
        uint256[3] memory pick = subsets[seed % 10];
        sigs = new bytes[](3);
        for (uint256 i; i < 3; ++i) {
            sigs[i] = _sign(signerKeys[pick[i]], digest);
        }
    }

    /// An attested vault picked by `seed`, or nothing.
    function _vault(uint256 seed) internal view returns (bool found, bytes32 vaultId, address account) {
        if (vaultIds.length == 0) return (false, bytes32(0), address(0));
        vaultId = vaultIds[seed % vaultIds.length];
        return (true, vaultId, accounts[accountIndexOf[vaultId]]);
    }

    function _active(bytes32 vaultId) internal view returns (bool active, uint64 outstanding) {
        VaultStatus status;
        (, outstanding, status) = registry.vault(vaultId);
        active = status == VaultStatus.Active;
    }

    function _authorize(address account) internal {
        vm.startPrank(account);
        if (!morpho.isAuthorized(account, address(registry))) morpho.setAuthorization(address(registry), true);
        if (!morpho.isAuthorized(account, address(adapter))) morpho.setAuthorization(address(adapter), true);
        vm.stopPrank();
    }

    function _fund(address who, address spender, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(spender, amount);
    }

    // ------------------------------------------------------------------ lifecycle actions

    function attest(uint256 amountSeed, uint256 signerSeed) external {
        if (nextAccount == ACCOUNT_COUNT) return;
        uint256 index = nextAccount++;
        address account = accounts[index];
        uint64 amount = amountSeed % 2 == 0 ? 1_000_000 : 5_000_000; // D7 denominations
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256(abi.encode("zecured.inv.vault", index)),
            account: account,
            amount: amount,
            depositTag: keccak256(abi.encode("zecured.inv.tag", index))
        });
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att), signerSeed));
        vaultIds.push(att.vaultId);
        attested[att.vaultId] = amount;
        tagOf[att.vaultId] = att.depositTag;
        accountIndexOf[att.vaultId] = index;
        calls["attest"]++;
    }

    /// L-8: 3 of 5 signers cancel a vault that was never activated (its deposit becomes refundable off-chain).
    function cancel(uint256 signerSeed) external {
        bytes32 vaultId = keccak256(abi.encode("zecured.inv.cancelled", cancelledIds.length));
        registry.cancelVault(vaultId, _threeSigs(registry.hashVaultCancellation(vaultId), signerSeed));
        cancelledIds.push(vaultId);
        calls["cancel"]++;
    }

    function borrow(uint256 vaultSeed, uint256 ltvBps) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstanding) = _active(vaultId);
        if (!active) return;
        _authorize(account);
        ltvBps = bound(ltvBps, 500, 5_500);
        uint256 target = uint256(outstanding) * oracle.price() / ORACLE_PRICE_SCALE * ltvBps / 10_000;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        if (target <= debt + 1) return;
        vm.prank(account);
        try morpho.borrow(params, target - debt, 0, account, stranger) {
            calls["borrow"]++;
        } catch {}
    }

    function repay(uint256 vaultSeed) external {
        (bool found,, address account) = _vault(vaultSeed);
        if (!found) return;
        uint256 shares = morpho.position(marketId, account).borrowShares;
        if (shares == 0) return;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        _fund(account, address(morpho), debt);
        vm.prank(account);
        morpho.repay(params, 0, shares, account, "");
        calls["repay"]++;
    }

    function release(uint256 vaultSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active,) = _active(vaultId);
        if (!active || morpho.position(marketId, account).borrowShares != 0) return;
        _authorize(account);
        vm.prank(account);
        registry.requestRelease(vaultId, keccak256(abi.encode("zecured.inv.release", vaultId)));
        calls["release"]++;
    }

    /// D14: the account withdraws part of its collateral (debt or not); refused when the LTV would exceed 50 %.
    function partialRelease(uint256 vaultSeed, uint256 amountSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        uint64 minRelease = registry.MIN_RELEASE();
        if (!active || outstandingBefore < 2 * minRelease) return;
        _authorize(account);
        uint64 amount = uint64(bound(amountSeed, minRelease, outstandingBefore - minRelease));
        uint64 claimsBefore = registry.claimCount(vaultId);

        vm.prank(account);
        try registry.requestPartialRelease(vaultId, amount, keccak256(abi.encode("zecured.inv.partial", vaultId))) {
            calls["partialRelease"]++;
            _checkPartialRelease(vaultId, account, claimsBefore, outstandingBefore, amount);
        } catch {
            calls["partialRelease (reverted)"]++;
        }
    }

    /// D14 at the moment of a successful partial release, with the LTV recomputed independently (Morpho's expected
    /// debt, rounded up; collateral value rounded down).
    function _checkPartialRelease(
        bytes32 vaultId,
        address account,
        uint64 claimsBefore,
        uint64 outstandingBefore,
        uint64 amount
    ) internal {
        (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
        (,, uint64 claimed,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        uint256 value = uint256(outstandingAfter) * oracle.price() / ORACLE_PRICE_SCALE;
        if (
            !stillActive || outstandingAfter != outstandingBefore - amount || claimed != amount
                || outstandingAfter < registry.MIN_RELEASE() || debt * 10_000 > value * registry.MAX_WITHDRAW_LTV_BPS()
        ) d14Violated = true;
    }

    function setPrice(uint256 cents) external {
        oracle.setPrice(bound(cents, 1_500, 6_000) * 1e32); // $15 .. $60
        calls["setPrice"]++;
    }

    function warp(uint256 secondsToSkip) external {
        skip(bound(secondsToSkip, 1, 30 days));
        calls["warp"]++;
    }

    /// Opens a loan of `ltvBps` (10 %..55 %) of the vault's value at the current price if it has none. Never reverts.
    function _ensureDebt(address account, uint64 outstanding, uint256 ltvBps) internal returns (bool) {
        if (morpho.position(marketId, account).borrowShares != 0) return true;
        _authorize(account);
        uint256 assets =
            uint256(outstanding) * oracle.price() / ORACLE_PRICE_SCALE * bound(ltvBps, 1_000, 5_500) / 10_000;
        if (assets == 0) return false;
        vm.prank(account);
        try morpho.borrow(params, assets, 0, account, stranger) {
            calls["borrow"]++;
            return true;
        } catch {
            return false;
        }
    }

    /// Goal-directed: makes sure the vault has a loan, moves the price so that its LTV is 63 %..95 %, then
    /// liquidates through the Adapter (either Morpho path; the amount may be below the D11 floor on purpose).
    function liquidate(uint256 vaultSeed, bool bySeize, uint256 amount, uint256 ltvSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, 5_000)) return;
        if (!_makeUnhealthy(account, outstandingBefore, ltvSeed)) return;
        (uint256 seizedAssets, uint256 repaidShares) = _liquidationAmounts(account, outstandingBefore, bySeize, amount);
        uint64 claimsBefore = registry.claimCount(vaultId);

        vm.prank(liquidator);
        try adapter.liquidate(account, seizedAssets, repaidShares, keccak256("zecured.inv.liquidator")) returns (
            uint256 seized, uint256
        ) {
            calls["liquidate"]++;
            _checkLiquidation(vaultId, claimsBefore, outstandingBefore, seized);
        } catch {
            calls["liquidate (reverted)"]++;
        }
    }

    /// Sets the price so that the account's LTV is 63 %..95 % and funds the liquidator.
    function _makeUnhealthy(address account, uint64 outstanding, uint256 ltvSeed) internal returns (bool) {
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        uint256 price = debt * 10_000 * ORACLE_PRICE_SCALE / (bound(ltvSeed, 6_300, 9_500) * outstanding);
        if (price == 0) return false;
        oracle.setPrice(price);
        _fund(liquidator, address(adapter), debt + 1e6);
        return true;
    }

    function _liquidationAmounts(address account, uint64 outstanding, bool bySeize, uint256 amount)
        internal
        view
        returns (uint256 seizedAssets, uint256 repaidShares)
    {
        // Mostly around the 100,000 zat floor; sub-floor remainders (now allowed by D11) can be smaller than that.
        if (bySeize) return (bound(amount, outstanding < 50_000 ? 1 : 50_000, outstanding), 0);
        uint256 shares = morpho.position(marketId, account).borrowShares;
        return (0, bound(amount, 1, shares + shares / 4)); // above the position: capped by the Adapter
    }

    /// I9 (and D11) at the moment of a successful liquidation.
    function _checkLiquidation(bytes32 vaultId, uint64 claimsBefore, uint64 outstandingBefore, uint256 seized)
        internal
    {
        (, uint64 outstandingAfter,) = registry.vault(vaultId);
        (,, uint64 claimed,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
        // D11: at least MIN_SEIZE, or the whole remaining outstanding.
        bool belowFloor = seized < adapter.MIN_SEIZE() && seized != outstandingBefore;
        if (
            seized > outstandingBefore || claimed != seized || outstandingAfter != outstandingBefore - seized
                || belowFloor
        ) i9Violated = true;
    }

    /// Goal-directed: makes sure the vault has a loan, then the market maker closes it with a signed request.
    function close(uint256 vaultSeed, uint256 marginBps, uint256 ltvSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, ltvSeed)) return;
        _authorize(account);
        CloseRequest memory req = CloseRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: uint16(bound(marginBps, 0, 500)),
            minUserAmount: 0,
            userCommitment: keccak256(abi.encode("zecured.inv.user", vaultId)),
            nonce: nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashCloseRequest(req));
        _fund(marketMaker, address(adapter), morpho.expectedBorrowAssets(params, account));
        uint64 claimsBefore = registry.claimCount(vaultId);
        uint256 supplyBefore = zzec.totalSupply();

        vm.prank(marketMaker);
        try adapter.closeWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), type(uint256).max, 0) {
            calls["close"]++;
            (,, uint64 userAmount,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
            (,, uint64 mmAmount,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore + 1));
            (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
            // I7 at the moment of the close.
            if (
                uint256(userAmount) + mmAmount != outstandingBefore
                    || supplyBefore - zzec.totalSupply() != outstandingBefore || stillActive || outstandingAfter != 0
                    || registry.claimCount(vaultId) != claimsBefore + 2
            ) i7Violated = true;
        } catch {
            calls["close (reverted)"]++;
        }
    }

    function confirm(uint256 vaultSeed, uint256 signerSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        uint64 count = registry.claimCount(vaultId);
        for (uint64 i; i < count; ++i) {
            bytes32 claimId = registry.claimIdOf(vaultId, i);
            (,,,, ClaimStatus status) = registry.claim(claimId);
            if (status != ClaimStatus.Pending) continue;
            bytes32[] memory ids = new bytes32[](1);
            ids[0] = claimId;
            registry.confirmPayout(vaultId, ids, _threeSigs(registry.hashPayoutConfirmation(vaultId, ids), signerSeed));
            calls["confirm"]++;
            return;
        }
    }

    // ------------------------------------------------------------------ attacks (must always fail)

    function attackAccountWithdraw(uint256 vaultSeed, uint256 receiverSeed, uint256 amount) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (, uint64 outstanding) = _active(vaultId);
        if (outstanding == 0) return;
        address[4] memory receivers = [account, stranger, address(registry), address(adapter)];
        vm.prank(account);
        try morpho.withdrawCollateral(params, bound(amount, 1, outstanding), account, receivers[receiverSeed % 4]) {
            attackSucceeded = true;
        } catch {}
        calls["attack: account withdraw"]++;
    }

    function attackDirectLiquidation(uint256 vaultSeed, uint256 amount) external {
        (bool found,, address account) = _vault(vaultSeed);
        if (!found) return;
        uint256 shares = morpho.position(marketId, account).borrowShares;
        if (shares == 0) return;
        _fund(liquidator, address(morpho), 100 * 1e6);
        vm.prank(liquidator);
        try morpho.liquidate(params, account, 0, bound(amount, 1, shares), "") {
            attackSucceeded = true;
        } catch {}
        calls["attack: direct liquidation"]++;
    }

    /// Nobody but the vault's account can partially release its collateral.
    function attackPartialRelease(uint256 vaultSeed, uint256 amountSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        (, uint64 outstanding) = _active(vaultId);
        uint64 minRelease = registry.MIN_RELEASE();
        if (outstanding < 2 * minRelease) return;
        address[3] memory intruders = [stranger, liquidator, marketMaker];
        vm.prank(intruders[amountSeed % 3]);
        try registry.requestPartialRelease(
            vaultId, uint64(bound(amountSeed, minRelease, outstanding - minRelease)), keccak256("zecured.inv.x")
        ) {
            attackSucceeded = true;
        } catch {}
        calls["attack: partial release"]++;
    }

    function attackFlashLoan(uint256 amount) external {
        uint256 held = zzec.balanceOf(address(morpho));
        if (held == 0) return;
        vm.prank(stranger);
        try morpho.flashLoan(address(zzec), bound(amount, 1, held), "") {
            attackSucceeded = true;
        } catch {}
        calls["attack: flash loan"]++;
    }

    /// I6: two signatures, or three from two signers, never activate a vault.
    function attackWeakAttestation(uint256 seed) external {
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256(abi.encode("zecured.inv.forged", seed)),
            account: stranger,
            amount: 1_000_000,
            depositTag: keccak256(abi.encode("zecured.inv.forgedTag", seed))
        });
        bytes32 digest = registry.hashDepositAttestation(att);
        bytes[] memory sigs = _threeSigs(digest, seed);
        if (seed % 2 == 0) {
            sigs[2] = sigs[1]; // three signatures, two signers
        } else {
            bytes[] memory two = new bytes[](2);
            (two[0], two[1]) = (sigs[0], sigs[1]);
            sigs = two;
        }
        try registry.attestDeposit(att, sigs) {
            attackSucceeded = true;
        } catch {}
        calls["attack: weak attestation"]++;
    }

    /// I14: a cancelled vault is never activated (even by a valid 3-of-5 attestation), and an activated vault is never
    /// cancelled (even by valid signatures).
    function attackCancellation(uint256 seed, uint256 signerSeed) external {
        if (seed % 2 == 0 && cancelledIds.length != 0) {
            bytes32 vaultId = cancelledIds[seed % cancelledIds.length];
            DepositAttestation memory att = DepositAttestation({
                vaultId: vaultId,
                account: stranger,
                amount: 1_000_000,
                depositTag: keccak256(abi.encode("zecured.inv.cancelledTag", seed))
            });
            try registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att), signerSeed)) {
                attackSucceeded = true;
            } catch {}
        } else if (vaultIds.length != 0) {
            bytes32 vaultId = vaultIds[seed % vaultIds.length];
            try registry.cancelVault(vaultId, _threeSigs(registry.hashVaultCancellation(vaultId), signerSeed)) {
                attackSucceeded = true;
            } catch {}
        }
        calls["attack: cancellation"]++;
    }

    /// I8: a valid signed request executed by someone else, or after its deadline, never withdraws collateral.
    function attackCloseRequest(uint256 vaultSeed, bool expired) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        CloseRequest memory req = CloseRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: 100,
            minUserAmount: 0,
            userCommitment: keccak256("zecured.inv.user"),
            nonce: type(uint256).max - nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashCloseRequest(req));
        _fund(stranger, address(adapter), 100 * 1e6);
        _fund(marketMaker, address(adapter), 100 * 1e6);
        address caller = stranger;
        if (expired) {
            vm.warp(uint256(req.deadline) + 1);
            caller = marketMaker;
        }
        vm.prank(caller);
        try adapter.closeWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), type(uint256).max, 0) {
            attackSucceeded = true;
        } catch {}
        calls["attack: close request"]++;
    }
}
