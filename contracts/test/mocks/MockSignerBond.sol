// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): the two views of a SignerBond the Registry reads.
contract MockSignerBond {
    bool public halted;
    bool public fullyBonded = true;

    function setHalted(bool halted_) external {
        halted = halted_;
    }

    function setFullyBonded(bool fullyBonded_) external {
        fullyBonded = fullyBonded_;
    }
}
