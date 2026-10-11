// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IRiscZeroVerifier
/// @notice The one function of RISC Zero's verifier interface (risc0-ethereum 3.0, `IRiscZeroVerifier`) that the
///         Registry calls. On Base Sepolia the `RiscZeroVerifierRouter` at 0x0b144e07a0826182b6b59788c34b32bfa86fb711
///         implements it (selector 0xab750e75, checked on-chain) and dispatches on the seal's first four bytes to the
///         matching verifier (Groth16 v3.0.0: 0x73c457ba); an unknown selector reverts with `SelectorUnknown`.
interface IRiscZeroVerifier {
    /// @notice Returns normally only if `seal` proves that the zkVM program `imageId` ran to completion and committed
    ///         a journal whose SHA-256 is `journalDigest`; reverts otherwise.
    function verify(bytes calldata seal, bytes32 imageId, bytes32 journalDigest) external view;
}
