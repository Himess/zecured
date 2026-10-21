// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ZecuredIntake} from "../../src/ZecuredIntake.sol";
import {IntakeConfig} from "../../src/interfaces/v3/IZecuredIntake.sol";

/// @notice TEST ONLY. The production Intake plus shortcuts that call the Registry's deposit hooks without a proof, so
///         that suites about everything after activation can open vaults in one line. Nothing like it is ever deployed:
///         minting needs a deposit proof (signer-attested minting is gone in v3).
contract IntakeHarness is ZecuredIntake {
    constructor(IntakeConfig memory c) ZecuredIntake(c) {}

    /// @notice Reserves and activates a vault at once (the Registry's own checks still apply).
    function harnessActivate(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external {
        _registry.reserveDeposit(vaultId, account, amount, depositTag);
        _registry.activate(vaultId, account, amount, depositTag);
    }

    /// @notice Opens a refund for a deposit without a proof.
    function harnessRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag)
        external
        returns (bytes32 claimId)
    {
        return _registry.openRefund(vaultId, account, amount, depositTag);
    }
}
