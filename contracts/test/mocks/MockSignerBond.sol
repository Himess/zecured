// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): the views of a SignerBond the Intake reads (`halted`, `fullyBonded`, `effectiveBond`), and
///         of the slasher the Intake and the Registry read (open cases, the freeze and when it began, compromised vaults
///         and when), in one contract that a suite names as both.
contract MockSignerBond {
    bool public halted;
    bool public fullyBonded = true;
    uint256 public effectiveBond;
    uint256 public openTheftCaseCount;
    mapping(bytes32 vaultId => uint256) public openCasesOf;
    mapping(bytes32 vaultId => bool) public vaultFrozen;
    mapping(bytes32 vaultId => bool) public isCompromised;
    /// @notice v4 (R-3): when the vault's current freeze began, as `ZecuredSlasher.frozenSince` (kept once compromised).
    mapping(bytes32 vaultId => uint64) public frozenSince;
    /// @notice v4 (R-2): when the vault was compromised, as `ZecuredSlasher.compromisedAt`.
    mapping(bytes32 vaultId => uint64) public compromisedAt;
    /// @notice v4.1 (W-2): open F1 cases about a note, and notes proven spent, as the slasher's views.
    mapping(bytes32 noteTag => uint256) public openF1CasesOf;
    mapping(bytes32 noteTag => bool) public noteSpendProven;

    function slasher() external view returns (address) {
        return address(this);
    }

    function setOpenCases(bytes32 vaultId, uint256 cases) external {
        openCasesOf[vaultId] = cases;
    }

    function setOpenF1Cases(bytes32 noteTag, uint256 cases) external {
        openF1CasesOf[noteTag] = cases;
    }

    function setNoteSpendProven(bytes32 noteTag, bool proven) external {
        noteSpendProven[noteTag] = proven;
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
        if (frozen && frozenSince[vaultId] == 0) frozenSince[vaultId] = uint64(block.timestamp);
        if (!frozen && !isCompromised[vaultId]) frozenSince[vaultId] = 0;
    }

    function setCompromised(bytes32 vaultId, bool compromised) external {
        isCompromised[vaultId] = compromised;
        if (compromised && compromisedAt[vaultId] == 0) compromisedAt[vaultId] = uint64(block.timestamp);
    }
}
