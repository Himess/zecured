// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Vm.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {ZecuredSwap} from "../../src/ZecuredSwap.sol";
import {
    IZecuredSwap,
    SwapQuote,
    SwapConfig,
    Order,
    OrderStatus,
    PendingSettlement,
    SIDE_BUY,
    SIDE_SELL
} from "../../src/interfaces/IZecuredSwap.sol";
import {IZecuredVersions, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {WorkTier} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {SwapFixture} from "../utils/SwapFixture.sol";

/// A maker that is a contract (ERC-1271): it accepts what its owner key signed.
contract Erc1271Maker is IERC1271 {
    address public immutable owner;

    constructor(address owner_) {
        owner = owner_;
    }

    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4) {
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecoverCalldata(hash, signature);
        return err == ECDSA.RecoverError.NoError && signer == owner ? IERC1271.isValidSignature.selector : bytes4(0);
    }

    function approve(ERC20 token, address spender) external {
        token.approve(spender, type(uint256).max);
    }
}

/// A contract maker whose "signature" is the digest itself: it lets a test put a maker at a fixed address (the
/// cross-language vector of tier2/payment-k4) without its key.
contract DigestWallet is IERC1271 {
    function isValidSignature(bytes32 hash, bytes calldata signature) external pure returns (bytes4) {
        return
            keccak256(signature) == keccak256(abi.encodePacked(hash)) ? IERC1271.isValidSignature.selector : bytes4(0);
    }
}

/// A contract maker that tries to change state while its signature is checked (a staticcall: it cannot). Not an
/// `IERC1271` in Solidity's eyes: its `isValidSignature` is not `view`.
contract MeddlingMaker {
    uint256 public calls;

    function isValidSignature(bytes32, bytes calldata) external returns (bytes4) {
        ++calls;
        return 0x1626ba7e; // IERC1271.isValidSignature.selector
    }
}

/// MOCK token that keeps 1 % of every transfer (USDC does not; the escrow must refuse such a token).
contract FeeToken is ERC20 {
    constructor() ERC20("Fee", "FEE") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (from != address(0) && to != address(0)) {
            uint256 fee = value / 100;
            super._update(from, address(0), fee);
            value -= fee;
        }
        super._update(from, to, value);
    }
}

/// MOCK token that calls back into the swap when it pays out (reentrancy).
contract ReentrantToken is ERC20 {
    ZecuredSwap public swap;
    bytes32 public orderId;
    bool public armed;

    constructor() ERC20("Re", "RE") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function arm(ZecuredSwap swap_, bytes32 orderId_) external {
        (swap, orderId, armed) = (swap_, orderId_, true);
    }

    function _update(address from, address to, uint256 value) internal override {
        super._update(from, to, value);
        if (armed && from == address(swap)) {
            armed = false;
            swap.refund(orderId);
        }
    }
}

/// ZecuredSwap (Zecured Swap, work order § 5): open, proveSettlement (K3), finalizeSettlement, dropSettlement and
/// refund, and every attack of § 5.8 as a test. MOCK verifier and relay; a real version registry.
contract ZecuredSwapTest is SwapFixture {
    // ------------------------------------------------------------------ config

    function test_config() public {
        assertEq(swap.usdc(), address(usdcToken));
        assertEq(swap.verifier(), address(verifier));
        assertEq(swap.versions(), address(versions));
        assertEq(swap.relay(), address(relay));
        assertEq(swap.finalizeDepth(), FINALIZE_DEPTH);
        assertEq(swap.finalizeDelay(), FINALIZE_DELAY);
        assertEq(swap.maxOrderUsdc(), MAX_USDC);
        assertEq(swap.maxOrderZat(), MAX_ZAT);
        assertEq(swap.minPayWindow(), MIN_WINDOW);
        assertEq(swap.maxPayWindow(), PAY_WINDOW);
        assertEq(swap.feeBps(), 0);
        assertEq(swap.lockedUsdc(), 0);
        WorkTier[] memory t = swap.workTiers();
        assertEq(t.length, 2);
        assertEq(swap.minConfirmationWorkFor(1), TIER1_WORK);
        assertEq(swap.minConfirmationWorkFor(TIER1_ZAT), TIER1_WORK);
        assertEq(swap.minConfirmationWorkFor(TIER1_ZAT + 1), TIER2_WORK);
        assertEq(swap.minConfirmationWorkFor(TIER2_ZAT), TIER2_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.AmountAboveLargestTier.selector, TIER2_ZAT + 1));
        swap.minConfirmationWorkFor(TIER2_ZAT + 1);
    }

    /// The frozen EIP-712 type and domain ("ZecuredSwap" "1").
    function test_eip712() public view {
        assertEq(
            swap.SWAP_QUOTE_TYPEHASH(),
            keccak256(
                "SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,uint32 payWindow,uint64 quoteDeadline,uint256 nonce)"
            )
        );
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("ZecuredSwap"),
                keccak256("1"),
                block.chainid,
                address(swap)
            )
        );
        assertEq(swap.domainSeparator(), domain);
    }

    function _badConfig(SwapConfig memory c) internal {
        vm.expectRevert(IZecuredSwap.InvalidConfig.selector);
        new ZecuredSwap(c);
    }

    function test_constructor_refusesBadConfigs() public {
        SwapConfig memory c = _config(address(usdcToken), 0, address(0));
        c.usdc = address(0);
        vm.expectRevert(IZecuredSwap.ZeroAddress.selector);
        new ZecuredSwap(c);

        // Another router than the version registry's.
        c = _config(address(usdcToken), 0, address(0));
        c.verifier = address(new MockRiscZeroVerifier());
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.finalizeDepth = 0;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.finalizeDelay = 0;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.maxOrderUsdc = 0;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.minPayWindow = 0;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.minPayWindow = c.maxPayWindow + 1;
        _badConfig(c);
        // A fee above 1 %, or without a recipient.
        _badConfig(_config(address(usdcToken), 101, stranger));
        _badConfig(_config(address(usdcToken), 50, address(0)));
        // Tiers: none, five, not ascending, zero work, less work for more value, an order cap above the last tier.
        c = _config(address(usdcToken), 0, address(0));
        c.workTiers = new WorkTier[](0);
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.workTiers = new WorkTier[](5);
        for (uint256 i; i < 5; ++i) {
            c.workTiers[i] = WorkTier({maxAmountZat: uint64(i + 1) * TIER2_ZAT, minWork: TIER2_WORK});
        }
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.workTiers[1].maxAmountZat = TIER1_ZAT;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.workTiers[0].minWork = 0;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.workTiers[1].minWork = TIER1_WORK - 1;
        _badConfig(c);
        c = _config(address(usdcToken), 0, address(0));
        c.maxOrderZat = TIER2_ZAT + 1;
        _badConfig(c);
    }

    // ------------------------------------------------------------------ open

    function test_open_buy_locksTheTakersUsdc() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        bytes32 id = swap.orderIdOf(maker, q.nonce);
        uint64 deadline = uint64(block.timestamp) + PAY_WINDOW;
        vm.expectEmit(address(swap));
        emit IZecuredSwap.OrderOpened(id, maker, taker, SIDE_BUY, 50_000_000, 20e6, takerCommitment, deadline);
        vm.prank(taker);
        assertEq(swap.open(q, sig, takerCommitment), id);

        Order memory o = _order(id);
        assertEq(o.maker, maker);
        assertEq(o.taker, taker);
        assertEq(o.side, SIDE_BUY);
        assertEq(uint8(o.status), uint8(OrderStatus.Open));
        assertEq(o.zatoshi, 50_000_000);
        assertEq(o.usdc, 20e6);
        assertEq(o.payDeadline, deadline);
        assertEq(o.receiverCommitment, takerCommitment);
        assertEq(swap.lockedUsdc(), 20e6);
        assertEq(usdcToken.balanceOf(address(swap)), 20e6);
        assertEq(usdcToken.balanceOf(taker), 980e6);
        assertEq(usdcToken.balanceOf(maker), 1_000e6);
    }

    function test_open_sell_locksTheMakersUsdc() public {
        bytes32 id = _openSell(50_000_000, 20e6);
        Order memory o = _order(id);
        assertEq(o.side, SIDE_SELL);
        assertEq(o.receiverCommitment, makerCommitment, "the taker pays the maker's receiver");
        assertEq(usdcToken.balanceOf(maker), 980e6);
        assertEq(usdcToken.balanceOf(taker), 1_000e6);
        assertEq(swap.lockedUsdc(), 20e6);
    }

    /// § 5.8 (5): the order id binds the chain, this contract, the maker and its nonce; the vector of
    /// tier2/payment-k4 (`keccak256(abi.encode(84532, 0x1111..11, 0x2222..22, 7))`).
    function test_orderId_formulaAndVector() public view {
        assertEq(swap.orderIdOf(maker, 7), keccak256(abi.encode(block.chainid, address(swap), maker, uint256(7))));
        assertEq(
            keccak256(
                abi.encode(
                    uint256(84_532),
                    address(0x1111111111111111111111111111111111111111),
                    address(0x2222222222222222222222222222222222222222),
                    uint256(7)
                )
            ),
            0xd13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f
        );
    }

    /// § 5.8 (6): a quote opens once (its nonce is spent), also by its own taker.
    function test_open_replayOfAQuote() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        bytes32 id = swap.open(q, sig, takerCommitment);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.NonceUsed.selector, id));
        swap.open(q, sig, takerCommitment);
        // Settled or refunded, the nonce stays spent.
        vm.warp(block.timestamp + PAY_WINDOW + 1);
        swap.refund(id);
        q.quoteDeadline = uint64(block.timestamp + 60);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.NonceUsed.selector, id));
        swap.open(q, sig, takerCommitment);
    }

    /// A quote for a taker is that taker's (no front-running); an open quote anyone's.
    function test_open_takerBinding() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        usdcToken.mint(stranger, 100e6);
        vm.startPrank(stranger);
        usdcToken.approve(address(swap), type(uint256).max);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.NotQuoteTaker.selector, stranger));
        swap.open(q, sig, takerCommitment);
        vm.stopPrank();

        q.taker = address(0);
        sig = _sign(swap, q, makerKey);
        vm.prank(stranger);
        bytes32 id = swap.open(q, sig, takerCommitment);
        assertEq(_order(id).taker, stranger);
        assertEq(usdcToken.balanceOf(stranger), 80e6);
    }

    function test_open_expiredQuote() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        vm.warp(q.quoteDeadline);
        vm.prank(taker);
        swap.open(q, sig, takerCommitment); // the deadline itself is fine
        q = _quote(SIDE_BUY, 50_000_000, 20e6);
        sig = _sign(swap, q, makerKey);
        vm.warp(q.quoteDeadline + 1);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.QuoteExpired.selector, q.quoteDeadline));
        swap.open(q, sig, takerCommitment);
    }

    /// A copy of `q` (a plain memory assignment would alias it).
    function _copy(SwapQuote memory q) internal pure returns (SwapQuote memory) {
        return abi.decode(abi.encode(q), (SwapQuote));
    }

    /// Any change of a signed field, another signer, a malformed signature: refused.
    function test_open_makerSignature() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        for (uint256 field; field < 6; ++field) {
            SwapQuote memory changed = _copy(q);
            if (field == 0) changed.usdc = 19e6;
            if (field == 1) changed.zatoshi = 50_000_001;
            if (field == 2) changed.payWindow = MIN_WINDOW;
            if (field == 3) changed.quoteDeadline += 1;
            if (field == 4) changed.nonce += 100;
            if (field == 5) changed.taker = address(0);
            vm.prank(taker);
            vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
            swap.open(changed, sig, takerCommitment);
        }

        (, uint256 otherKey) = makeAddrAndKey("zecured.swap.otherMaker");
        bytes memory otherSig = _sign(swap, q, otherKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open(q, otherSig, takerCommitment);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open(q, hex"1234", takerCommitment);
        // The high-s twin of a valid signature (malleability) is refused.
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(makerKey, swap.hashSwapQuote(q));
        bytes32 highS = bytes32(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141 - uint256(s));
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open(q, abi.encodePacked(r, highS, v == 27 ? uint8(28) : uint8(27)), takerCommitment);
        // A signature for another deployment (another verifying contract) is refused.
        ZecuredSwap other = new ZecuredSwap(_config(address(usdcToken), 0, address(0)));
        bytes memory elsewhere = _sign(other, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open(q, elsewhere, takerCommitment);
        // The genuine signature opens.
        vm.prank(taker);
        swap.open(q, sig, takerCommitment);
    }

    /// ERC-1271: a contract maker signs through `isValidSignature`.
    function test_open_erc1271Maker() public {
        (address owner, uint256 ownerKey) = makeAddrAndKey("zecured.swap.walletOwner");
        Erc1271Maker wallet = new Erc1271Maker(owner);
        usdcToken.mint(address(wallet), 100e6);
        wallet.approve(ERC20(address(usdcToken)), address(swap));
        SwapQuote memory q = _quote(SIDE_SELL, 50_000_000, 20e6);
        q.maker = address(wallet);
        bytes memory sig = _sign(swap, q, ownerKey);
        vm.prank(taker);
        bytes32 id = swap.open(q, sig, bytes32(0));
        assertEq(_order(id).maker, address(wallet));
        assertEq(usdcToken.balanceOf(address(wallet)), 80e6);
        // A signature of anyone else is refused by the wallet.
        q.nonce = nextNonce++;
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open(q, sig, bytes32(0));
    }

    /// The signature check is a staticcall: a contract maker cannot change state through it (its call fails and the
    /// quote is refused).
    function test_open_erc1271MakerCannotMeddle() public {
        MeddlingMaker meddler = new MeddlingMaker();
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        q.maker = address(meddler);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.BadMakerSignature.selector);
        swap.open{gas: 1_000_000}(q, hex"00", takerCommitment);
        assertEq(meddler.calls(), 0);
    }

    function test_open_amountsAndCaps() public {
        SwapQuote memory q = _quote(SIDE_BUY, 0, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.InvalidAmount.selector);
        swap.open(q, sig, takerCommitment);
        q = _quote(SIDE_BUY, 50_000_000, 0);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.InvalidAmount.selector);
        swap.open(q, sig, takerCommitment);
        q = _quote(SIDE_BUY, 50_000_000, MAX_USDC + 1);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderTooLarge.selector, MAX_USDC + 1, uint64(50_000_000)));
        swap.open(q, sig, takerCommitment);
        q = _quote(SIDE_BUY, MAX_ZAT + 1, 20e6);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderTooLarge.selector, uint256(20e6), MAX_ZAT + 1));
        swap.open(q, sig, takerCommitment);
        // The caps themselves open.
        usdcToken.mint(taker, MAX_USDC);
        _openBuy(MAX_ZAT, MAX_USDC);
    }

    function test_open_payWindowBounds() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        q.payWindow = MIN_WINDOW - 1;
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.PayWindowOutOfRange.selector, MIN_WINDOW - 1));
        swap.open(q, sig, takerCommitment);
        q.payWindow = PAY_WINDOW + 1;
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.PayWindowOutOfRange.selector, PAY_WINDOW + 1));
        swap.open(q, sig, takerCommitment);
        q.payWindow = MIN_WINDOW;
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        bytes32 id = swap.open(q, sig, takerCommitment);
        assertEq(_order(id).payDeadline, block.timestamp + MIN_WINDOW);
    }

    function test_open_sideAndCommitments() public {
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        q.side = 2;
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.InvalidSide.selector, uint8(2)));
        swap.open(q, sig, takerCommitment);
        // Buy: the taker's commitment, none in the quote.
        q = _quote(SIDE_BUY, 50_000_000, 20e6);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.MissingReceiverCommitment.selector);
        swap.open(q, sig, bytes32(0));
        q.makerReceiverCommitment = makerCommitment;
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.UnexpectedReceiverCommitment.selector);
        swap.open(q, sig, takerCommitment);
        // Sell: the maker's commitment, none from the taker.
        q = _quote(SIDE_SELL, 50_000_000, 20e6);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.UnexpectedReceiverCommitment.selector);
        swap.open(q, sig, takerCommitment);
        q.makerReceiverCommitment = bytes32(0);
        sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(IZecuredSwap.MissingReceiverCommitment.selector);
        swap.open(q, sig, bytes32(0));
    }

    /// What is locked is exactly what arrived: a token that keeps part of a transfer is refused.
    function test_open_refusesAFeeOnTransferToken() public {
        FeeToken fee = new FeeToken();
        ZecuredSwap feeSwap = new ZecuredSwap(_config(address(fee), 0, address(0)));
        fee.mint(taker, 100e6);
        vm.prank(taker);
        fee.approve(address(feeSwap), type(uint256).max);
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(feeSwap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.TransferMismatch.selector, uint256(19.8e6), uint256(20e6)));
        feeSwap.open(q, sig, takerCommitment);
    }

    /// Without the locker's USDC or allowance nothing opens.
    function test_open_needsTheLockersUsdc() public {
        vm.prank(maker);
        usdcToken.approve(address(swap), 0);
        SwapQuote memory q = _quote(SIDE_SELL, 50_000_000, 20e6);
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(taker);
        vm.expectRevert();
        swap.open(q, sig, bytes32(0));
        assertEq(swap.lockedUsdc(), 0);
    }

    // ------------------------------------------------------------------ prove

    function test_prove_pendsTheSettlement() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.SettlementPending(id, _anchor(id), TIP + FINALIZE_DEPTH, block.timestamp + FINALIZE_DELAY);
        vm.prank(stranger); // anyone may prove
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
        PendingSettlement memory p = swap.pendingSettlement(id);
        assertEq(p.anchor, _anchor(id));
        assertEq(p.readyHeight, TIP + FINALIZE_DEPTH);
        assertEq(p.readyTime, block.timestamp + FINALIZE_DELAY);
        assertEq(p.versionId, versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), PAYMENT_IMAGE));
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Open));
        assertFalse(swap.settlementStale(id));
    }

    /// § 5.8 (1) and (5): one payment, one order. A journal names one order of one contract on one chain: it proves
    /// nothing for another order (of the same maker or not), another deployment or another chain.
    function test_prove_aJournalOfAnotherOrder() public {
        bytes32 a = _openBuy(50_000_000, 20e6);
        bytes32 b = _openBuy(50_000_000, 20e6); // same taker, same receiver commitment, same amount
        (bytes memory journal, bytes memory seal) = _proof(a, PAYMENT_IMAGE);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.JournalOrderMismatch.selector, a));
        swap.proveSettlement(b, journal, seal, PAYMENT_IMAGE);
        swap.proveSettlement(a, journal, seal, PAYMENT_IMAGE);
        // The same quote (maker, nonce) on another chain or another deployment has another id.
        Order memory o = _order(b);
        bytes32 otherChain = keccak256(abi.encode(block.chainid + 1, address(swap), maker, uint256(2)));
        bytes32 otherContract = keccak256(abi.encode(block.chainid, address(0xBEEF), maker, uint256(2)));
        assertEq(swap.orderIdOf(maker, 2), b);
        for (uint256 i; i < 2; ++i) {
            bytes32 named = i == 0 ? otherChain : otherContract;
            bytes memory j = _journal(named, o.receiverCommitment, o.zatoshi, _anchor(b), NU7);
            relay.setBlock(_anchor(b), true, TIER1_WORK);
            bytes memory s = verifier.mockSeal(PAYMENT_IMAGE, sha256(j));
            vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.JournalOrderMismatch.selector, named));
            swap.proveSettlement(b, j, s, PAYMENT_IMAGE);
        }
    }

    /// § 5.8 (2): a payment to another receiver, or of less than the order's amount, proves nothing (the payer paid at
    /// their own loss); more is fine.
    function test_prove_wrongReceiverOrTooLittle() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        bytes32 anchor = _anchor(id);
        relay.setBlock(anchor, true, TIER1_WORK);
        bytes32 other = keccak256("zecured.swap.otherReceiver");
        bytes memory j = _journal(id, other, 50_000_000, anchor, NU7);
        bytes memory s = verifier.mockSeal(PAYMENT_IMAGE, sha256(j));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.ReceiverMismatch.selector, other));
        swap.proveSettlement(id, j, s, PAYMENT_IMAGE);
        // One zatoshi short (attack 10: no rounding in the payer's favour).
        j = _journal(id, takerCommitment, 49_999_999, anchor, NU7);
        s = verifier.mockSeal(PAYMENT_IMAGE, sha256(j));
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredSwap.AmountTooLow.selector, uint64(49_999_999), uint64(50_000_000))
        );
        swap.proveSettlement(id, j, s, PAYMENT_IMAGE);
        // An overpayment settles the order (its tier is the order's).
        j = _journal(id, takerCommitment, 70_000_000, anchor, NU7);
        s = verifier.mockSeal(PAYMENT_IMAGE, sha256(j));
        swap.proveSettlement(id, j, s, PAYMENT_IMAGE);
    }

    /// The branch id must be one the proof's Swap version allows (here NU7 only).
    function test_prove_branchIds() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        bytes32 anchor = _anchor(id);
        relay.setBlock(anchor, true, TIER1_WORK);
        uint32[3] memory refused = [NU6_3, uint32(0), uint32(0xdeadbeef)];
        for (uint256 i; i < refused.length; ++i) {
            bytes memory j = _journal(id, takerCommitment, 50_000_000, anchor, refused[i]);
            bytes memory s = verifier.mockSeal(PAYMENT_IMAGE, sha256(j));
            vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.BranchNotAllowed.selector, refused[i]));
            swap.proveSettlement(id, j, s, PAYMENT_IMAGE);
        }
    }

    /// Only an active Swap version's guest: an unknown image id, a deposit or violation guest's, a removed version.
    function test_prove_onlyAnActiveSwapVersion() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        bytes32[3] memory images = [keccak256("zecured.test.unknownImage"), DEPOSIT_IMAGE, VIOLATION_IMAGE];
        for (uint256 i; i < images.length; ++i) {
            (bytes memory journal, bytes memory seal) = _proof(id, images[i]);
            vm.expectRevert(
                abi.encodeWithSelector(
                    IZecuredVersions.VersionNotActive.selector,
                    versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), images[i])
                )
            );
            swap.proveSettlement(id, journal, seal, images[i]);
        }
        _removeVersion(PAYMENT_IMAGE);
        (bytes memory j, bytes memory s) = _proof(id, PAYMENT_IMAGE);
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredVersions.VersionNotActive.selector,
                versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), PAYMENT_IMAGE)
            )
        );
        swap.proveSettlement(id, j, s, PAYMENT_IMAGE);
        // The other active Swap version still proves.
        (j, s) = _proof(id, PAYMENT_IMAGE_B);
        swap.proveSettlement(id, j, s, PAYMENT_IMAGE_B);
    }

    function test_prove_badSealOrJournal() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE);
        // A seal of another journal.
        bytes memory other = _journal(id, takerCommitment, 50_000_001, _anchor(id), NU7);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        swap.proveSettlement(id, other, seal, PAYMENT_IMAGE);
        // A journal of another length, or of another kind (a deposit journal's first word is a mode).
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.InvalidJournalLength.selector, uint256(224)));
        swap.proveSettlement(id, bytes.concat(journal, bytes32(0)), seal, PAYMENT_IMAGE);
        bytes memory kind0 = bytes.concat(bytes32(0), _slice(journal, 32, 160));
        bytes memory kind0Seal = verifier.mockSeal(PAYMENT_IMAGE, sha256(kind0));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.InvalidJournalKind.selector, uint8(0)));
        swap.proveSettlement(id, kind0, kind0Seal, PAYMENT_IMAGE);
        // A dirty word (a uint64 with high bits) does not decode.
        bytes memory dirty = bytes.concat(_slice(journal, 0, 160), bytes32(uint256(1) << 64 | 50_000_000));
        bytes memory dirtySeal = verifier.mockSeal(PAYMENT_IMAGE, sha256(dirty));
        vm.expectRevert();
        swap.proveSettlement(id, dirty, dirtySeal, PAYMENT_IMAGE);
        // An empty seal names no version.
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredVersions.VersionNotActive.selector,
                versions.versionIdOf(VersionKind.Swap, bytes4(0), PAYMENT_IMAGE)
            )
        );
        swap.proveSettlement(id, journal, "", PAYMENT_IMAGE);
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
    }

    /// § 5.8 (4): a fake branch. The anchor must be on the relay's best chain with the tier's work of the order's
    /// amount on top.
    function test_prove_anchorAndWorkTiers() public {
        bytes32 small = _openBuy(TIER1_ZAT, 20e6);
        bytes32 large = _openBuy(TIER1_ZAT + 1, 20e6);
        (bytes memory journal, bytes memory seal) = _proof(small, PAYMENT_IMAGE);
        relay.setBlock(_anchor(small), false, TIER1_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.AnchorNotConfirmed.selector, _anchor(small), uint256(0)));
        swap.proveSettlement(small, journal, seal, PAYMENT_IMAGE);
        relay.setBlock(_anchor(small), true, TIER1_WORK - 1);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredSwap.AnchorNotConfirmed.selector, _anchor(small), TIER1_WORK - 1)
        );
        swap.proveSettlement(small, journal, seal, PAYMENT_IMAGE);
        relay.setBlock(_anchor(small), true, TIER1_WORK);
        swap.proveSettlement(small, journal, seal, PAYMENT_IMAGE);
        // Above the first tier, the second tier's work.
        (journal, seal) = _proof(large, PAYMENT_IMAGE);
        relay.setBlock(_anchor(large), true, TIER2_WORK - 1);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredSwap.AnchorNotConfirmed.selector, _anchor(large), TIER2_WORK - 1)
        );
        swap.proveSettlement(large, journal, seal, PAYMENT_IMAGE);
        relay.setBlock(_anchor(large), true, TIER2_WORK);
        swap.proveSettlement(large, journal, seal, PAYMENT_IMAGE);
    }

    /// § 5.8 (7): the proof must come by the pay deadline (the deadline itself counts); the finalization may be later.
    function test_prove_byTheDeadline() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        uint64 deadline = _order(id).payDeadline;
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE);
        vm.warp(deadline + 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.PayDeadlinePassed.selector, id, deadline));
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
        vm.warp(deadline);
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
        _makeReady(id);
        assertGt(block.timestamp, deadline);
        swap.finalizeSettlement(id);
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Settled));
    }

    /// A valid pending settlement is never replaced; a stale one is, by the next honest proof.
    function test_prove_pendingSettlements() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE_B);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SettlementAlreadyPending.selector, id));
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE_B);

        // The first anchor leaves the best chain (a fake branch lost): the next proof replaces it.
        relay.setBlock(_anchor(id), false, 0);
        assertTrue(swap.settlementStale(id));
        bytes32 honest = keccak256("zecured.swap.honestAnchor");
        relay.setBlock(honest, true, TIER1_WORK);
        bytes memory j = _journal(id, takerCommitment, 50_000_000, honest, NU7);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.SettlementDropped(id);
        swap.proveSettlement(id, j, verifier.mockSeal(PAYMENT_IMAGE, sha256(j)), PAYMENT_IMAGE);
        assertEq(swap.pendingSettlement(id).anchor, honest);
    }

    function test_prove_onlyOpenOrders() public {
        bytes32 none = keccak256("zecured.swap.noOrder");
        (bytes memory journal, bytes memory seal) = (bytes(""), bytes(""));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderNotOpen.selector, none));
        swap.proveSettlement(none, journal, seal, PAYMENT_IMAGE);
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        swap.finalizeSettlement(id);
        (journal, seal) = _proof(id, PAYMENT_IMAGE);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderNotOpen.selector, id));
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
    }

    // ------------------------------------------------------------------ finalize

    function test_finalize_buy_paysTheMaker() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.OrderSettled(id);
        vm.prank(stranger); // anyone may finalize
        swap.finalizeSettlement(id);
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Settled));
        assertEq(usdcToken.balanceOf(maker), 1_020e6);
        assertEq(usdcToken.balanceOf(taker), 980e6);
        assertEq(usdcToken.balanceOf(address(swap)), 0);
        assertEq(swap.lockedUsdc(), 0);
        assertEq(swap.pendingSettlement(id).anchor, bytes32(0));
    }

    function test_finalize_sell_paysTheTaker() public {
        bytes32 id = _openSell(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        swap.finalizeSettlement(id);
        assertEq(usdcToken.balanceOf(taker), 1_020e6);
        assertEq(usdcToken.balanceOf(maker), 980e6);
        assertEq(swap.lockedUsdc(), 0);
    }

    /// The fee comes out of the released USDC, rounded down (attack 10: 1 unit at 0.5 % pays no fee).
    function test_finalize_withAFee() public {
        address treasury = makeAddr("zecured.swap.treasury");
        swap = new ZecuredSwap(_config(address(usdcToken), 50, treasury));
        vm.prank(taker);
        usdcToken.approve(address(swap), type(uint256).max);
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        swap.finalizeSettlement(id);
        assertEq(usdcToken.balanceOf(treasury), 0.1e6);
        assertEq(usdcToken.balanceOf(maker), 1_000e6 + 19.9e6);
        bytes32 tiny = _openBuy(1, 1);
        _prove(tiny);
        _makeReady(tiny);
        swap.finalizeSettlement(tiny);
        assertEq(usdcToken.balanceOf(treasury), 0.1e6);
        assertEq(usdcToken.balanceOf(maker), 1_000e6 + 19.9e6 + 1);
        assertEq(usdcToken.balanceOf(address(swap)), 0);
    }

    /// K3: not before the relay reached the ready height and the ready time passed.
    function test_finalize_notBeforeReady() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        PendingSettlement memory p = swap.pendingSettlement(id);
        bytes memory notReady =
            abi.encodeWithSelector(IZecuredSwap.SettlementNotReady.selector, id, p.readyHeight, p.readyTime);
        vm.expectRevert(notReady);
        swap.finalizeSettlement(id);
        relay.setTipHeight(p.readyHeight);
        vm.expectRevert(notReady);
        swap.finalizeSettlement(id);
        relay.setTipHeight(p.readyHeight - 1);
        vm.warp(p.readyTime);
        vm.expectRevert(notReady);
        swap.finalizeSettlement(id);
        relay.setTipHeight(p.readyHeight);
        swap.finalizeSettlement(id);
    }

    /// § 5.8 (4): the anchor left the best chain before the finalization: no USDC moves; the record is dropped.
    function test_finalize_anchorReorgedOut() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        relay.setBlock(_anchor(id), false, 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.AnchorNotConfirmed.selector, _anchor(id), uint256(0)));
        swap.finalizeSettlement(id);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.SettlementDropped(id);
        swap.dropSettlement(id);
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Open));
        assertEq(swap.lockedUsdc(), 20e6);
        // The order can be proven again before its deadline.
        relay.setBlock(_anchor(id), true, TIER1_WORK);
        _prove(id);
    }

    /// § 5.8 (8), R-14: a broken guest's Swap version is removed by three signers at once; a settlement it proved no
    /// longer finalizes and anyone drops it; a payment can be proven again with another version.
    function test_finalize_versionRemoved() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        _removeVersion(PAYMENT_IMAGE);
        bytes32 versionId = versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), PAYMENT_IMAGE);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SwapVersionNotActive.selector, versionId));
        swap.finalizeSettlement(id);
        assertTrue(swap.settlementStale(id));
        vm.prank(stranger);
        swap.dropSettlement(id);
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE_B);
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE_B);
        _makeReady(id);
        swap.finalizeSettlement(id);
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Settled));
    }

    function test_finalize_once() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        _prove(id);
        _makeReady(id);
        swap.finalizeSettlement(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.NoPendingSettlement.selector, id));
        swap.finalizeSettlement(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderNotOpen.selector, id));
        swap.refund(id);
    }

    // ------------------------------------------------------------------ drop

    function test_drop_onlyAStaleSettlement() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.NoPendingSettlement.selector, id));
        swap.dropSettlement(id);
        _prove(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SettlementStillValid.selector, id));
        swap.dropSettlement(id);
        _makeReady(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SettlementStillValid.selector, id));
        swap.dropSettlement(id);
    }

    // ------------------------------------------------------------------ refund

    function test_refund_buy_returnsTheTakersUsdc() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        uint64 deadline = _order(id).payDeadline;
        vm.warp(deadline);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.PayDeadlineNotPassed.selector, id, deadline));
        swap.refund(id);
        vm.warp(deadline + 1);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.OrderRefunded(id);
        vm.prank(stranger); // anyone; the USDC goes back to whoever locked it
        swap.refund(id);
        assertEq(uint8(_order(id).status), uint8(OrderStatus.Refunded));
        assertEq(usdcToken.balanceOf(taker), 1_000e6);
        assertEq(swap.lockedUsdc(), 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.OrderNotOpen.selector, id));
        swap.refund(id);
    }

    function test_refund_sell_returnsTheMakersUsdc() public {
        bytes32 id = _openSell(50_000_000, 20e6);
        vm.warp(_order(id).payDeadline + 1);
        swap.refund(id);
        assertEq(usdcToken.balanceOf(maker), 1_000e6);
        assertEq(usdcToken.balanceOf(taker), 1_000e6);
    }

    /// § 5.8 (7): no refund while a proof made in time stands; it is finalized instead.
    function test_refund_notWhileAProofStands() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        vm.warp(_order(id).payDeadline);
        _prove(id);
        vm.warp(block.timestamp + 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SettlementIsPending.selector, id));
        swap.refund(id);
        _makeReady(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSwap.SettlementIsPending.selector, id));
        swap.refund(id);
        swap.finalizeSettlement(id);
        assertEq(usdcToken.balanceOf(maker), 1_020e6);
    }

    /// § 5.8 (4): a fake proof at the deadline delays the refund only until its anchor leaves the best chain.
    function test_refund_afterAFakeProofIsDropped() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        vm.warp(_order(id).payDeadline);
        _prove(id);
        vm.warp(block.timestamp + 1);
        relay.setBlock(_anchor(id), false, 0);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.SettlementDropped(id);
        vm.expectEmit(address(swap));
        emit IZecuredSwap.OrderRefunded(id);
        swap.refund(id);
        assertEq(usdcToken.balanceOf(taker), 1_000e6);
    }

    /// § 5.8 (9): RISC Zero stops the router's verifier: no proof verifies, and an unproven order is refunded after its
    /// deadline (the payer's risk; a maker's on Buy).
    function test_refund_afterARouterEstop() public {
        bytes32 id = _openBuy(50_000_000, 20e6);
        verifier.stop();
        (bytes memory journal, bytes memory seal) = _proof(id, PAYMENT_IMAGE);
        vm.expectRevert(MockRiscZeroVerifier.MockStopped.selector);
        swap.proveSettlement(id, journal, seal, PAYMENT_IMAGE);
        vm.warp(_order(id).payDeadline + 1);
        swap.refund(id);
        assertEq(usdcToken.balanceOf(taker), 1_000e6);
    }

    // ------------------------------------------------------------------ reentrancy

    /// § 5.8 (11): a token that calls back into the swap while it pays out finds every entry point locked.
    function test_reentrancy() public {
        ReentrantToken token = new ReentrantToken();
        ZecuredSwap reSwap = new ZecuredSwap(_config(address(token), 0, address(0)));
        token.mint(taker, 100e6);
        vm.prank(taker);
        token.approve(address(reSwap), type(uint256).max);
        SwapQuote memory q = _quote(SIDE_BUY, 50_000_000, 20e6);
        bytes memory sig = _sign(reSwap, q, makerKey);
        vm.prank(taker);
        bytes32 id = reSwap.open(q, sig, takerCommitment);
        vm.warp(block.timestamp + PAY_WINDOW + 1);
        token.arm(reSwap, id);
        vm.expectRevert(ReentrancyGuard.ReentrancyGuardReentrantCall.selector);
        reSwap.refund(id);
        assertEq(uint8(reSwap.order(id).status), uint8(OrderStatus.Open));
        assertEq(token.balanceOf(address(reSwap)), 20e6);
    }

    // ------------------------------------------------------------------ privacy

    /// § 5.8 (12): what Base sees of an order is its amounts, parties, deadline, the receiver's salted commitment and the
    /// anchor header: the events carry nothing else (no txid, note commitment, nullifier or address of Zcash).
    function test_events_carryNoZcashIdentifier() public {
        vm.recordLogs();
        bytes32 id = _openBuy(50_000_000, 20e6);
        uint64 deadline = _order(id).payDeadline;
        _prove(id);
        PendingSettlement memory p = swap.pendingSettlement(id);
        _makeReady(id);
        swap.finalizeSettlement(id);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 seen;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter != address(swap)) continue;
            bytes32 sig = logs[i].topics[0];
            assertEq(logs[i].topics[1], id, "every swap event is keyed by the order id");
            if (sig == IZecuredSwap.OrderOpened.selector) {
                assertEq(logs[i].topics.length, 4);
                assertEq(logs[i].topics[2], bytes32(uint256(uint160(maker))));
                assertEq(logs[i].topics[3], bytes32(uint256(uint160(taker))));
                assertEq(
                    logs[i].data, abi.encode(SIDE_BUY, uint64(50_000_000), uint256(20e6), takerCommitment, deadline)
                );
            } else if (sig == IZecuredSwap.SettlementPending.selector) {
                assertEq(logs[i].topics.length, 2);
                assertEq(logs[i].data, abi.encode(_anchor(id), uint256(p.readyHeight), uint256(p.readyTime)));
            } else {
                assertEq(sig, IZecuredSwap.OrderSettled.selector);
                assertEq(logs[i].topics.length, 2);
                assertEq(logs[i].data.length, 0);
            }
            ++seen;
        }
        assertEq(seen, 3);
    }

    // ------------------------------------------------------------------ cross-language vector

    /// The synthetic fixture of tier2/payment-k4 (`TEST-ONLY_swap_payment_witness.json`), as the payment guest commits
    /// its journal: on a ZecuredSwap at 0x1111..11 on chain 84532, the order of maker 0x2222..22's quote 7 to the
    /// fixture's receiver commitment settles on exactly those 192 bytes.
    function test_theRustFixturesJournalSettlesItsOrder() public {
        vm.chainId(84_532);
        address at = 0x1111111111111111111111111111111111111111;
        bytes memory creation =
            abi.encodePacked(type(ZecuredSwap).creationCode, abi.encode(_config(address(usdcToken), 0, address(0))));
        vm.etch(at, creation);
        (bool ok, bytes memory runtime) = at.call("");
        assertTrue(ok);
        vm.etch(at, runtime);
        ZecuredSwap pinned = ZecuredSwap(at);
        address fixedMaker = 0x2222222222222222222222222222222222222222;
        vm.etch(fixedMaker, address(new DigestWallet()).code);

        bytes32 rc = 0x0144dce00498a1cb4e3d04161414ccf1cd42b09f70fe83ddcf745973c522a0a7;
        SwapQuote memory q = SwapQuote({
            maker: fixedMaker,
            taker: taker,
            side: SIDE_BUY,
            zatoshi: 2_500_000,
            usdc: 1e6,
            makerReceiverCommitment: bytes32(0),
            payWindow: PAY_WINDOW,
            quoteDeadline: uint64(block.timestamp + 60),
            nonce: 7
        });
        vm.startPrank(taker);
        usdcToken.approve(at, type(uint256).max);
        bytes32 id = pinned.open(q, abi.encodePacked(pinned.hashSwapQuote(q)), rc);
        vm.stopPrank();
        assertEq(id, 0xd13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f);

        bytes memory journal = bytes.concat(
            bytes32(uint256(1)),
            bytes32(uint256(0x77190ad9)),
            bytes32(0x301c5d2807c6ed2eaebfe1e92ec0e25a2cb8bd4a7c3d8173c9d717148908d225),
            bytes32(0xd13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f),
            rc,
            bytes32(uint256(2_500_000))
        );
        assertEq(journal.length, 192);
        bytes32 anchor = 0x301c5d2807c6ed2eaebfe1e92ec0e25a2cb8bd4a7c3d8173c9d717148908d225;
        relay.setBlock(anchor, true, TIER1_WORK);
        pinned.proveSettlement(id, journal, verifier.mockSeal(PAYMENT_IMAGE, sha256(journal)), PAYMENT_IMAGE);
        PendingSettlement memory p = pinned.pendingSettlement(id);
        assertEq(p.anchor, anchor);
        relay.setTipHeight(p.readyHeight);
        vm.warp(p.readyTime);
        pinned.finalizeSettlement(id);
        assertEq(usdcToken.balanceOf(fixedMaker), 1e6);
    }

    /// The EIP-712 digest vector of swap/core (`quote::tests::the_digest_vector`, alloy's encoder): a Sell quote of maker
    /// 0x2222..22 for taker 0x3333..33 on a ZecuredSwap at 0x1111..11 on chain 84532.
    function test_eip712_digestVector() public {
        vm.chainId(84_532);
        address at = 0x1111111111111111111111111111111111111111;
        vm.etch(
            at, abi.encodePacked(type(ZecuredSwap).creationCode, abi.encode(_config(address(usdcToken), 0, address(0))))
        );
        (bool ok, bytes memory runtime) = at.call("");
        assertTrue(ok);
        vm.etch(at, runtime);
        SwapQuote memory q = SwapQuote({
            maker: 0x2222222222222222222222222222222222222222,
            taker: 0x3333333333333333333333333333333333333333,
            side: SIDE_SELL,
            zatoshi: 50_000_000,
            usdc: 19_800_000,
            makerReceiverCommitment: bytes32(
                uint256(0x4444444444444444444444444444444444444444444444444444444444444444)
            ),
            payWindow: 21_600,
            quoteDeadline: 1_760_000_120,
            nonce: 7
        });
        assertEq(ZecuredSwap(at).hashSwapQuote(q), EIP712_DIGEST_VECTOR);
    }

    bytes32 internal constant EIP712_DIGEST_VECTOR = 0xd3e594a519b8f06b57c3151ddf712e48edafdccf3d65f5079b0bdbbff4c89c00;

    function _slice(bytes memory b, uint256 from, uint256 len) internal pure returns (bytes memory out) {
        out = new bytes(len);
        for (uint256 i; i < len; ++i) {
            out[i] = b[from + i];
        }
    }
}
