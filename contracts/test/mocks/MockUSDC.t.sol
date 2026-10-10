// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";

contract MockUSDCTest is Test {
    MockUSDC internal usdc;
    address internal owner = makeAddr("zecured.mockusdc.owner");
    address internal alice = makeAddr("zecured.mockusdc.alice");
    address internal bob = makeAddr("zecured.mockusdc.bob");

    function setUp() public {
        usdc = new MockUSDC(owner);
    }

    function test_metadata() public view {
        assertEq(usdc.decimals(), 6);
        assertEq(usdc.name(), "Zecured Mock USDC");
        assertEq(usdc.symbol(), "mUSDC");
        assertEq(usdc.owner(), owner);
        assertEq(usdc.totalSupply(), 0);
    }

    function test_ownerCanMint() public {
        vm.prank(owner);
        usdc.mint(alice, 1_000e6);
        assertEq(usdc.balanceOf(alice), 1_000e6);
        assertEq(usdc.totalSupply(), 1_000e6);
    }

    function test_nonOwnerCannotMint() public {
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, alice));
        usdc.mint(alice, 1);
    }

    function test_transferAndTransferFrom() public {
        vm.prank(owner);
        usdc.mint(alice, 100e6);

        vm.prank(alice);
        assertTrue(usdc.transfer(bob, 40e6));
        assertEq(usdc.balanceOf(alice), 60e6);
        assertEq(usdc.balanceOf(bob), 40e6);

        vm.prank(alice);
        usdc.approve(bob, 10e6);
        vm.prank(bob);
        assertTrue(usdc.transferFrom(alice, bob, 10e6));
        assertEq(usdc.balanceOf(bob), 50e6);
        assertEq(usdc.allowance(alice, bob), 0);
    }

    function testFuzz_mint(address to, uint128 amount) public {
        vm.assume(to != address(0));
        vm.prank(owner);
        usdc.mint(to, amount);
        assertEq(usdc.balanceOf(to), amount);
        assertEq(usdc.totalSupply(), amount);
    }
}
