// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): the one view of a SignerBond the Registry reads.
contract MockSignerBond {
    bool public halted;

    function setHalted(bool halted_) external {
        halted = halted_;
    }
}
