// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IRiscZeroVerifierRouter
/// @notice The two read functions of RISC Zero's router and emergency-stop wrapper that `ZecuredVersions` calls to see
///         whether a verifier still runs (design notes T-07a). Checked on Base Sepolia (2026-10-16): the router at
///         0x0b144e07a0826182b6b59788c34b32bfa86fb711 returns, for the Groth16 selector 0x73c457ba, its
///         `RiscZeroVerifierEmergencyStop` wrapper, whose `paused()` is false and whose `verifier()` is the Groth16
///         verifier; an unknown selector reverts.
interface IRiscZeroVerifierRouter {
    /// @notice The verifier the router dispatches `selector`'s seals to; reverts for an unknown or removed selector.
    function getVerifier(bytes4 selector) external view returns (address);
}

/// @notice RISC Zero's `RiscZeroVerifierEmergencyStop` (OpenZeppelin `Pausable`): once its owner (or anyone holding a
///         proof of a forged receipt) stops it, it is paused for good and every `verify` through it reverts.
interface IRiscZeroEmergencyStop {
    function paused() external view returns (bool);
}
