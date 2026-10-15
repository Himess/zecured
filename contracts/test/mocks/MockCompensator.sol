// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredRegistry} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";

/// @notice MOCK (tests only): stands in for ZecuredCompensator where a suite drives the Registry's compensator hooks by
///         hand (the real compensator has its own suite). It records what the slasher and the bond hand it, and calls
///         `retireVault` / `settleCompensatedClaim` when the test says so.
contract MockCompensator {
    struct CaseCall {
        bytes32 caseId;
        ViolationKind kind;
        bytes32 vaultId;
        bytes32 claimId;
        uint64 shortfallZat;
    }

    CaseCall[] internal _cases;
    uint256 public funds;

    function onCase(bytes32 caseId, ViolationKind kind, bytes32 vaultId, bytes32 claimId, uint64 shortfallZat)
        external
    {
        _cases.push(CaseCall(caseId, kind, vaultId, claimId, shortfallZat));
    }

    function onFunds(bytes32, uint256 amount) external {
        funds += amount;
    }

    function caseCount() external view returns (uint256) {
        return _cases.length;
    }

    function caseAt(uint256 i) external view returns (CaseCall memory) {
        return _cases[i];
    }

    function retire(IZecuredRegistry registry, bytes32 vaultId) external returns (uint64) {
        return registry.retireVault(vaultId);
    }

    function settle(IZecuredRegistry registry, bytes32 claimId) external {
        registry.settleCompensatedClaim(claimId);
    }
}
