// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Errors of the signer-signature check shared by the Registry and its spend logic (I6).
interface IZecuredSignerErrors {
    /// @notice The signer set is not 5 strictly increasing, non-zero addresses.
    error InvalidSignerSet();
    /// @notice Fewer signatures than required (I6).
    error InsufficientSignatures(uint256 provided);
    /// @notice A signature recovers to an address outside the signer set.
    error UnknownSigner(address recovered);
    /// @notice The signer recovered at `index` is not strictly greater than the previous one (unsorted or duplicate).
    error SignaturesNotSorted(uint256 index);
}
