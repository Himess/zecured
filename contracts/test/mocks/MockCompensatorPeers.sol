// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IZecuredCompensator} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";

/// @title MockCompensatorSlasher
/// @notice MOCK, tests only. The part of ZecuredSlasher v2 the compensator sees: its wiring, its open cases, the
///         compromised flag, and an `execute` that does what `executeSlash` does after the bond's slash (F1 and F2 mark
///         the vault compromised, then the compensator's `onCase`). No proofs, no challenge window. v4: when the freeze
///         began and when the vault was compromised (the same moment here, unless the test sets an earlier freeze);
///         unlike `ZecuredSlasher`, `execute` does not burn the collateral (the tests call `burnCollateral`).
contract MockCompensatorSlasher {
    address public registry;
    address public signerBond;
    address public compensator;
    uint256 public openCaseCount;
    mapping(bytes32 vaultId => bool) public isCompromised;
    mapping(bytes32 vaultId => uint64) public frozenSince;
    mapping(bytes32 vaultId => uint64) public compromisedAt;

    /// @notice The vault froze at `at` (an F1/F2 case proven then), before the case that compromises it executes.
    function setFrozenSince(bytes32 vaultId, uint64 at) external {
        frozenSince[vaultId] = at;
    }

    function setWiring(address registry_, address signerBond_, address compensator_) external {
        registry = registry_;
        signerBond = signerBond_;
        compensator = compensator_;
    }

    function setOpenCaseCount(uint256 count) external {
        openCaseCount = count;
    }

    function execute(bytes32 caseId, ViolationKind kind, bytes32 vaultId, bytes32 claimId, uint64 shortfallZat)
        external
    {
        if ((kind == ViolationKind.Uncommitted || kind == ViolationKind.WithheldChange) && !isCompromised[vaultId]) {
            isCompromised[vaultId] = true;
            compromisedAt[vaultId] = uint64(block.timestamp);
            if (frozenSince[vaultId] == 0) frozenSince[vaultId] = uint64(block.timestamp);
        }
        IZecuredCompensator(compensator).onCase(caseId, kind, vaultId, claimId, shortfallZat);
    }
}

/// @title MockCompensatorBond
/// @notice MOCK, tests only. The part of SignerBond v2 the compensator sees: its wiring, and a `slash` that sends
///         `amount` of the USDC the test gave it to the compensator, then calls `onFunds` (the bounty and the halt are
///         out of scope).
contract MockCompensatorBond {
    address public registry;
    address public slasher;
    address public compensator;
    IERC20 public usdc;

    function setWiring(address registry_, address slasher_, address compensator_, IERC20 usdc_) external {
        registry = registry_;
        slasher = slasher_;
        compensator = compensator_;
        usdc = usdc_;
    }

    function slash(bytes32 caseId, uint256 amount) external {
        require(usdc.transfer(compensator, amount), "MockBond: transfer");
        IZecuredCompensator(compensator).onFunds(caseId, amount);
    }
}
