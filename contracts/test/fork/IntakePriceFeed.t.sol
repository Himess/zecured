// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredPriceFeed} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";
import {ZecuredPriceFeed} from "../../src/ZecuredPriceFeed.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {MockAggregator} from "../mocks/MockAggregator.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The Intake's coverage cap (T-01) reading a real ZecuredPriceFeed (T-10) over two MOCK aggregators, with
///         `capPriceMaxAge` = the feed's `maxSourceAge`, as a mainnet deployment sets it: minting goes on in every fresh
///         state at the higher of the two prices, stops at once when the feed is Stale (also when its sources broke
///         right after an update), and resumes with fresh prices. MOCK relay, verifier, bond and slasher, as in
///         IntakeGates.
contract IntakePriceFeedTest is ZecuredFixture {
    uint64 internal constant SMALL = 1_000_000; // 0.01 ZEC
    uint256 internal constant BOND = 4e6; // 4 USDC: 0.1 ZEC at $40 with k = 100 %
    uint64 internal constant MAX_AGE = 25 hours;
    int256 internal constant A40 = 40e8; // $40.00 with 8 decimals, PRICE_40 once scaled

    MockSignerBond internal gates;
    MockAggregator internal primarySource;
    MockAggregator internal secondarySource;
    ZecuredPriceFeed internal feed;
    uint256 internal anchorNonce;

    function _mockBondAndSlasher() internal override returns (address) {
        gates = new MockSignerBond();
        gates.setEffectiveBond(BOND);
        return address(gates);
    }

    function _capSetup() internal override returns (CapSetup memory) {
        primarySource = new MockAggregator(8);
        secondarySource = new MockAggregator(8);
        primarySource.set(A40, block.timestamp);
        secondarySource.set(A40, block.timestamp);
        feed = new ZecuredPriceFeed(address(primarySource), MAX_AGE, address(secondarySource), MAX_AGE, 300);
        return CapSetup({
            coverageBps: 10_000, maxTotalZat: 30_000_000, capPriceMaxAge: feed.maxSourceAge(), capOracle: address(feed)
        });
    }

    function test_theIntakeReadsTheFeed() public view {
        assertEq(intake.capOracle(), address(feed));
        assertEq(intake.capPriceMaxAge(), MAX_AGE);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        assertEq(intake.capRoom(), 10_000_000, "0.1 ZEC at $40");
    }

    /// The cap values the locked zatoshi at the higher fresh price: with agreeing sources too, and more so when they
    /// diverge.
    function test_theCapReadsTheHigherPrice() public {
        secondarySource.set(A40 * 102 / 100, block.timestamp); // 2 % higher: the sources agree
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        assertEq(intake.capRoom(), _coveredZat(PRICE_40 * 102 / 100));
        assertEq(feed.price(), PRICE_40, "the market keeps the primary's price");

        secondarySource.set(A40 * 2, block.timestamp); // twice as high: diverged
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Diverged));
        assertEq(intake.capRoom(), 5_000_000, "half as much at twice the price");
        assertEq(feed.price(), PRICE_40, "the market takes the lower");
        bytes32 a = _deposit("feed.a", SMALL);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Pending));
        assertEq(intake.capRoom(), 4_000_000);
    }

    /// Past the max age the feed is Stale: Morpho would keep its last good price, but the cap sees a stale price at
    /// once, so an activatable proof waits (it reverts) and no bond is freed; with fresh prices the proof lands.
    function test_aStaleFeedStopsNewMintingUntilItIsFreshAgain() public {
        bytes32 a = _deposit("feed.a", SMALL);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Pending));

        skip(MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), PRICE_40, "the market's price is the last good one");
        (, uint256 updatedAt) = feed.priceHigh();
        assertEq(intake.capRoom(), 0);
        assertFalse(intake.coverageHoldsWith(1_000_000e6), "a stale price never frees a bond");

        address b = makeAddr("feed.b");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapPriceStale.selector, updatedAt));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);

        primarySource.set(A40, block.timestamp);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        (journal, seal, sigs) = _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(_vaultIdFor(b))), uint8(VaultStatus.Pending));
    }

    /// Sources that stop answering a minute after their last update: the record is recent, yet the cap sees the price
    /// dated maxSourceAge + 1 s back and stops new minting at once.
    function test_brokenSourcesStopNewMintingAtOnce() public {
        skip(1 minutes);
        feed.poke();
        primarySource.setMode(MockAggregator.Mode.Revert);
        secondarySource.setMode(MockAggregator.Mode.Revert);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        (, uint256 updatedAt) = feed.priceHigh();
        assertEq(updatedAt, block.timestamp - MAX_AGE - 1);
        assertEq(intake.capRoom(), 0);

        address b = makeAddr("feed.b");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapPriceStale.selector, updatedAt));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    // ------------------------------------------------------------------ helpers (as in IntakeGates)

    /// The zatoshi the bond covers at `price` (ZecuredIntake.capRoom's formula, nothing locked yet).
    function _coveredZat(uint256 price) internal pure returns (uint256) {
        return Math.mulDiv(BOND * 10_000, 1e36, 10_000 * price);
    }

    function _proof(bytes32 vaultId, address account, uint64 amount, bytes32 tag, uint256 work)
        internal
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        bytes32 anchor = keccak256(abi.encode("zecured.feed.anchor", anchorNonce++));
        MockHeaderRelay(address(headerRelay)).setBlock(anchor, true, work);
        MockHeaderRelay(address(headerRelay)).setTipHeight(4_500_000);
        bytes32 opened = keccak256(abi.encode("zecured.feed.opened", vaultId));
        journal = abi.encode(anchor, vaultId, account, amount, tag, opened);
        seal = MockRiscZeroVerifier(address(proofVerifier)).mockSeal(proofs.imageId, sha256(journal));
        uint256[] memory five = new uint256[](5);
        (five[0], five[1], five[2], five[3], five[4]) = (0, 1, 2, 3, 4);
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), five);
    }

    function _deposit(string memory name, uint64 amount) internal returns (bytes32 vaultId) {
        address account = makeAddr(name);
        vaultId = _vaultIdFor(account);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(vaultId, account, amount, _depositTagFor(account), intake.minConfirmationWorkFor(amount));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }
}
