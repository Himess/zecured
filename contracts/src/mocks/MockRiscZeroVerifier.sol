// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IRiscZeroVerifier} from "../interfaces/IRiscZeroVerifier.sol";
import {IRiscZeroVerifierRouter, IRiscZeroEmergencyStop} from "../interfaces/IRiscZeroVerifierRouter.sol";

/// @title MockRiscZeroVerifier
/// @notice MOCK. Accepts a "seal" that is only a hash of the image id and the journal digest: it proves nothing. It
///         stands in for RISC Zero's Groth16 verifier on a local fork, where no Groth16 proof can be produced (the
///         Groth16 wrapper needs an x86 machine with Docker). The local tools run the deposit guest in the zkVM
///         executor, check its journal against the native statement, and build this seal from it. Base Sepolia
///         deployments use the real `RiscZeroVerifierRouter` instead (contracts/README.md).
///         It also plays the router and its emergency-stop wrapper for the version registry (T-07a): `getVerifier`
///         of its selector is itself, and anyone may stop it (`stop`), after which `verify` reverts, as a paused
///         wrapper's does.
contract MockRiscZeroVerifier is IRiscZeroVerifier, IRiscZeroVerifierRouter, IRiscZeroEmergencyStop {
    /// @notice First four bytes of a mock seal (RISC Zero's own mock verifier uses the same marker).
    bytes4 public constant SELECTOR = 0xffffffff;

    /// @inheritdoc IRiscZeroEmergencyStop
    bool public paused;

    error MockSealInvalid();
    error MockStopped();
    error SelectorUnknown(bytes4 selector);

    /// @notice The only seal this mock accepts for `imageId` and `journalDigest`.
    function mockSeal(bytes32 imageId, bytes32 journalDigest) public pure returns (bytes memory) {
        return abi.encodePacked(SELECTOR, sha256(abi.encodePacked(imageId, journalDigest)));
    }

    /// @inheritdoc IRiscZeroVerifier
    function verify(bytes calldata seal, bytes32 imageId, bytes32 journalDigest) external view {
        if (paused) revert MockStopped();
        if (keccak256(seal) != keccak256(mockSeal(imageId, journalDigest))) revert MockSealInvalid();
    }

    /// @inheritdoc IRiscZeroVerifierRouter
    function getVerifier(bytes4 selector) external view returns (address) {
        if (selector != SELECTOR) revert SelectorUnknown(selector);
        return address(this);
    }

    /// @notice MOCK emergency stop: from now on every `verify` reverts.
    function stop() external {
        paused = true;
    }
}
