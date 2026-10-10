// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Vm.sol";

/// @notice TEST HELPER. Runs several calls, each with its own pranked sender, inside ONE top-level call of the test,
///         i.e. inside one transaction even under `forge test --isolate` (where every top-level call is a separate
///         transaction and EIP-1153 transient storage is cleared in between). Reverts bubble up unchanged.
contract OneTx {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    struct Call {
        address sender;
        address target;
        bytes data;
    }

    function run(Call[] memory calls) external returns (bytes[] memory results) {
        results = new bytes[](calls.length);
        for (uint256 i; i < calls.length; ++i) {
            VM.prank(calls[i].sender);
            (bool ok, bytes memory ret) = calls[i].target.call(calls[i].data);
            if (!ok) {
                assembly ("memory-safe") {
                    revert(add(ret, 0x20), mload(ret))
                }
            }
            results[i] = ret;
        }
    }
}

/// @notice TEST HELPER. Detects whether the test runs with `--isolate`: a TSTORE made in one top-level call is still
///         visible in the next one only when top-level calls are NOT separate transactions.
contract TransientProbe {
    function set() external {
        assembly ("memory-safe") {
            tstore(0, 1)
        }
    }

    function get() external view returns (uint256 value) {
        assembly ("memory-safe") {
            value := tload(0)
        }
    }
}
