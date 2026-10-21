// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ZecuredIntake} from "../../src/ZecuredIntake.sol";
import {IntakeConfig} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredRefunds} from "../../src/interfaces/v3/IZecuredRefunds.sol";

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

    /// @notice Reserves a refund for a deposit without a proof and opens its claim at once (R-12's wait skipped; the
    ///         Registry's own checks still apply, so a pending vault refuses it).
    function harnessRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag)
        external
        returns (bytes32 claimId)
    {
        _registry.reserveRefund(vaultId, account, amount, depositTag);
        return IZecuredRefunds(address(_registry)).openReservedRefund(vaultId, depositTag);
    }

    /// @notice Only reserves a refund for a deposit without a proof (its claim is opened by `openReservedRefund`).
    function harnessReserveRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external {
        _registry.reserveRefund(vaultId, account, amount, depositTag);
    }

    /// @notice Opens the claim of a refund `harnessReserveRefund` reserved (as `finalizeRefund` would, without its
    ///         readiness, anchor and version checks; the Registry's own checks still apply).
    function harnessOpenRefund(bytes32 vaultId, bytes32 depositTag) external returns (bytes32 claimId) {
        return IZecuredRefunds(address(_registry)).openReservedRefund(vaultId, depositTag);
    }

    /// @notice Releases a refund `harnessReserveRefund` reserved (as `dropPendingRefund` would for a fallen proof).
    function harnessReleaseRefund(bytes32 vaultId, bytes32 depositTag) external {
        IZecuredRefunds(address(_registry)).releaseRefund(vaultId, depositTag);
    }
}
