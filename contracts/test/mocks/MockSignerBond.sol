// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): the views of a SignerBond the Registry reads, and of the slasher it names (itself).
contract MockSignerBond {
    bool public halted;
    bool public fullyBonded = true;
    mapping(bytes32 vaultId => uint256) public openCasesOf;

    function slasher() external view returns (address) {
        return address(this);
    }

    function setOpenCases(bytes32 vaultId, uint256 cases) external {
        openCasesOf[vaultId] = cases;
    }

    function setHalted(bool halted_) external {
        halted = halted_;
    }

    function setFullyBonded(bool fullyBonded_) external {
        fullyBonded = fullyBonded_;
    }
}
