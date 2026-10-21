// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): the views of a SignerBond the Intake reads (`halted`, `fullyBonded`, `effectiveBond`), and
///         of the slasher the Intake and the Registry read (open cases, the freeze, compromised vaults), in one contract
///         that a suite names as both.
contract MockSignerBond {
    bool public halted;
    bool public fullyBonded = true;
    uint256 public effectiveBond;
    uint256 public openTheftCaseCount;
    mapping(bytes32 vaultId => uint256) public openCasesOf;
    mapping(bytes32 vaultId => bool) public vaultFrozen;
    mapping(bytes32 vaultId => bool) public isCompromised;

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

    function setEffectiveBond(uint256 effectiveBond_) external {
        effectiveBond = effectiveBond_;
    }

    function setOpenTheftCaseCount(uint256 count) external {
        openTheftCaseCount = count;
    }

    function setFrozen(bytes32 vaultId, bool frozen) external {
        vaultFrozen[vaultId] = frozen;
    }

    function setCompromised(bytes32 vaultId, bool compromised) external {
        isCompromised[vaultId] = compromised;
    }
}
