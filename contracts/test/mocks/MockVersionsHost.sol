// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title MockVersionsHost
/// @notice MOCK, tests only. The Registry and the Intake as far as ZecuredVersions' constructor reads them (its signer
///         set, its Intake and the Intake's verifier), so that a version registry can be built without a Base Sepolia
///         fork: ZecuredSwap's tests use a real ZecuredVersions with real signer signatures on top of it.
contract MockVersionsHost {
    address[5] internal _signers;
    address public immutable verifier;

    constructor(address[5] memory signers_, address verifier_) {
        _signers = signers_;
        verifier = verifier_;
    }

    /// @notice The Registry's signer set (strictly increasing).
    function signers() external view returns (address[5] memory) {
        return _signers;
    }

    /// @notice The Registry's Intake: this contract plays both.
    function intake() external view returns (address) {
        return address(this);
    }
}
