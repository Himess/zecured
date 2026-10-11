// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IRiscZeroVerifier} from "../interfaces/IRiscZeroVerifier.sol";

/// @title MockRiscZeroVerifier
/// @notice MOCK. Accepts a "seal" that is only a hash of the image id and the journal digest: it proves nothing. It
///         stands in for RISC Zero's Groth16 verifier on a local fork, where no Groth16 proof can be produced (the
///         Groth16 wrapper needs an x86 machine with Docker). The local tools run the deposit guest in the zkVM
///         executor, check its journal against the native statement, and build this seal from it. Base Sepolia
///         deployments use the real `RiscZeroVerifierRouter` instead (contracts/README.md).
contract MockRiscZeroVerifier is IRiscZeroVerifier {
    /// @notice First four bytes of a mock seal (RISC Zero's own mock verifier uses the same marker).
    bytes4 public constant SELECTOR = 0xffffffff;

    error MockSealInvalid();

    /// @notice The only seal this mock accepts for `imageId` and `journalDigest`.
    function mockSeal(bytes32 imageId, bytes32 journalDigest) public pure returns (bytes memory) {
        return abi.encodePacked(SELECTOR, sha256(abi.encodePacked(imageId, journalDigest)));
    }

    /// @inheritdoc IRiscZeroVerifier
    function verify(bytes calldata seal, bytes32 imageId, bytes32 journalDigest) external pure {
        if (keccak256(seal) != keccak256(mockSeal(imageId, journalDigest))) revert MockSealInvalid();
    }
}
