// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MathLib} from "morpho-blue/src/libraries/MathLib.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";

contract MockZecOracleTest is Test {
    using MathLib for uint256;

    event PriceSet(uint256 oldPrice, uint256 newPrice);

    MockZecOracle internal oracle;
    address internal owner = makeAddr("zecured.oracle.owner");
    address internal stranger = makeAddr("zecured.oracle.stranger");

    uint256 internal constant ZEC = 1e8; // 1 ZEC in zatoshi (collateral decimals)
    uint256 internal constant USDC = 1e6; // 1 USDC (loan decimals)

    function setUp() public {
        oracle = new MockZecOracle(owner, 40 * 1e34);
    }

    function test_scaleConstantMatchesMorphoFormula() public view {
        // 1e36 * 10^(loanDecimals - collateralDecimals) = 1e36 * 10^(6 - 8)
        assertEq(oracle.USD_SCALE(), ORACLE_PRICE_SCALE / 1e2);
        assertEq(oracle.USD_SCALE(), 1e34);
    }

    function test_initialPriceAndOwner() public view {
        assertEq(oracle.price(), 40e34);
        assertEq(IOracle(address(oracle)).price(), 40e34);
        assertEq(oracle.owner(), owner);
    }

    function test_constructorEmitsPriceSet() public {
        vm.expectEmit(false, false, false, true);
        emit PriceSet(0, 55e34);
        new MockZecOracle(owner, 55e34);
    }

    function test_constructorRejectsZeroPrice() public {
        vm.expectRevert(MockZecOracle.ZeroPrice.selector);
        new MockZecOracle(owner, 0);
    }

    function test_ownerCanSetPriceAndEventIsEmitted() public {
        vm.expectEmit(address(oracle));
        emit PriceSet(40e34, 2550e32);
        vm.prank(owner);
        oracle.setPrice(2550e32); // $25.50
        assertEq(oracle.price(), 2550e32);
    }

    function test_nonOwnerCannotSetPrice() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger));
        oracle.setPrice(1e34);
    }

    function test_setPriceRejectsZero() public {
        vm.prank(owner);
        vm.expectRevert(MockZecOracle.ZeroPrice.selector);
        oracle.setPrice(0);
    }

    /// 1 ZEC at $40 must quote to exactly 40 USDC with Morpho's own math (`_isHealthy`: collateral * price / 1e36).
    function test_quoteOneZecAtFortyDollars() public view {
        uint256 quoted = ZEC.mulDivDown(oracle.price(), ORACLE_PRICE_SCALE);
        assertEq(quoted, 40 * USDC);
    }

    /// Morpho's max borrow for 10 ZEC at $40 and LLTV 62.5% is 250 USDC.
    function test_maxBorrowAtLltv() public view {
        uint256 maxBorrow = (10 * ZEC).mulDivDown(oracle.price(), ORACLE_PRICE_SCALE).wMulDown(0.625e18);
        assertEq(maxBorrow, 250 * USDC);
    }

    /// For any whole-cent price and zatoshi amount, quoting with the 1e34 scale gives the exact USDC value,
    /// rounded down like Morpho does: (zatoshi / 1e8 ZEC) * (cents / 100 USD) * 1e6 = zatoshi * cents / 1e4.
    function testFuzz_quoteMatchesCentsArithmetic(uint64 zatoshi, uint32 cents) public {
        vm.assume(cents > 0);
        vm.prank(owner);
        oracle.setPrice(uint256(cents) * 1e32);
        uint256 quoted = uint256(zatoshi).mulDivDown(oracle.price(), ORACLE_PRICE_SCALE);
        assertEq(quoted, uint256(zatoshi) * cents / 1e4);
    }
}
