// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Which proofs a version verifies.
enum VersionKind {
    Deposit,
    Violation
}

/// @notice A verifier version: a RISC Zero router selector, a guest image id and (deposits) the consensus branch ids its
///         journal may name. `versionId = keccak256(abi.encode(kind, selector, imageId))`.
struct Version {
    VersionKind kind;
    bytes4 selector;
    bytes32 imageId;
    uint32[4] branches;
    uint64 activeAt;
    bool removed;
}

/// @title IZecuredVersions
/// @notice The verifier version registry (design notes T-07a) and, optionally, the Adapter registry (T-07b).
/// @dev - Adding a version takes all five signers (one honest signer vetoes) and a public delay: `depositDelay` (14
///        days on mainnet) or `violationDelay` (7 days on mainnet; 1 day while every active violation version's
///        verifier is stopped by the router's emergency stop, which only RISC Zero can trigger).
///      - Removing one takes three signers and is immediate, except that the last active violation version stays.
///      - Initial versions (and adapters) are given to the constructor and active at once.
///      - The registry holds no funds and touches no claim, vault or bond.
///      EIP-712 domain "ZecuredVersions" "1":
///      ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)
///      RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)
///      ScheduleAdapter(address adapter,uint256 nonce,uint64 deadline)
///      RemoveAdapter(address adapter,uint256 nonce,uint64 deadline)
interface IZecuredVersions {
    event VersionScheduled(
        bytes32 indexed versionId, VersionKind kind, bytes4 selector, bytes32 imageId, uint64 activeAt
    );
    event VersionRemoved(bytes32 indexed versionId);
    event AdapterScheduled(address indexed adapter, uint64 activeAt);
    event AdapterRemoved(address indexed adapter);

    error ZeroAddress();
    error InvalidConfig();
    error BadNonce(uint256 expected);
    error SignatureExpired(uint64 deadline);
    error InsufficientSignatures(uint256 provided);
    error UnknownSigner(address recovered);
    error SignaturesNotSorted(uint256 index);
    error VersionExists(bytes32 versionId);
    error UnknownVersion(bytes32 versionId);
    error LastViolationVersion(bytes32 versionId);
    error AdapterExists(address adapter);
    error UnknownAdapter(address adapter);
    error LastAdapter(address adapter);

    function scheduleVersion(
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] calldata branches,
        uint256 nonce,
        uint64 deadline,
        bytes[] calldata sigs
    ) external returns (bytes32 versionId);
    function removeVersion(bytes32 versionId, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;
    function scheduleAdapter(address adapter, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;
    function removeAdapter(address adapter, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;

    function versionIdOf(VersionKind kind, bytes4 selector, bytes32 imageId) external pure returns (bytes32);
    function isActive(bytes32 versionId) external view returns (bool);
    function branchAllowed(bytes32 versionId, uint32 branchId) external view returns (bool);
    function isAdapter(address adapter) external view returns (bool);
    function version(bytes32 versionId) external view returns (Version memory);
    function nonce() external view returns (uint256);
    function depositDelay() external view returns (uint64);
    function violationDelay() external view returns (uint64);
}
