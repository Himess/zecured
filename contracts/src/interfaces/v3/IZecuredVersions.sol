// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Which proofs a version verifies.
enum VersionKind {
    Deposit,
    Violation
}

/// @notice A verifier version: a RISC Zero router selector, a guest image id and (deposits) the consensus branch ids a
///         v2 journal may name. `versionId = keccak256(abi.encode(kind, selector, imageId))`.
/// @param branches Deposit versions: the branch ids a v2 journal may name (zero entries unused); zero for violations.
/// @param activeAt When the version starts verifying (zero: never scheduled).
/// @param removed Removed by three signers; it verifies nothing any more.
struct Version {
    VersionKind kind;
    bytes4 selector;
    bytes32 imageId;
    uint32[4] branches;
    uint64 activeAt;
    bool removed;
}

/// @title IZecuredVersions
/// @notice The verifier version registry (design notes T-07a): which RISC Zero verifier (router selector) and guest
///         (image id) the Intake takes deposit proofs from, and the slasher violation proofs from. Without it, a guest
///         fix or a network upgrade would need a new deployment and a migration of every position, and an emergency
///         stop of the verifier by RISC Zero would end minting and slashing for good.
/// @dev - Adding a version takes all five signers (one honest signer vetoes) and a public delay: `depositDelay` (14
///        days on mainnet) or `violationDelay` (7 days on mainnet), `estopDelay` (1 day on mainnet) instead while every
///        active violation version's verifier is stopped (`violationStopped`: the router refuses its selector, or its
///        emergency-stop wrapper is paused, which only RISC Zero can do).
///      - Removing one takes three signers and is immediate, except that the last active violation version stays:
///        slashing always has a verifier.
///      - The initial versions are given to the constructor and are active at once.
///      - The registry holds no funds and touches no claim, vault or bond. Its signers are the Registry's.
///      - The Adapter registry of the design (T-07b, a second SettlementAdapter scheduled the same way should USDC
///        ever blacklist the first) is not part of this deployment (docs/MAINNET.md).
///      EIP-712 domain "ZecuredVersions" "1":
///      ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)
///      RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)
interface IZecuredVersions {
    event VersionScheduled(
        bytes32 indexed versionId, VersionKind kind, bytes4 selector, bytes32 imageId, uint64 activeAt
    );
    event VersionRemoved(bytes32 indexed versionId);

    error ZeroAddress();
    error InvalidConfig();
    error InvalidVersion();
    error BadNonce(uint256 expected);
    error SignatureExpired(uint64 deadline);
    error InsufficientSignatures(uint256 provided);
    error UnknownSigner(address recovered);
    error SignaturesNotSorted(uint256 index);
    error VersionExists(bytes32 versionId);
    error UnknownVersion(bytes32 versionId);
    error LastViolationVersion(bytes32 versionId);
    error VersionNotActive(bytes32 versionId);

    /// @notice All five signers. Schedules a version, active `depositDelay` or `violationDelay` (`estopDelay` while
    ///         `violationStopped()`) from now. A removed version may be scheduled again.
    function scheduleVersion(
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] calldata branches,
        uint256 nonce,
        uint64 deadline,
        bytes[] calldata sigs
    ) external returns (bytes32 versionId);

    /// @notice Three signers. Removes a version at once (also one not active yet); never the last active violation
    ///         version.
    function removeVersion(bytes32 versionId, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;

    function versionIdOf(VersionKind kind, bytes4 selector, bytes32 imageId) external pure returns (bytes32);

    /// @notice Scheduled, past `activeAt`, not removed.
    function isActive(bytes32 versionId) external view returns (bool);

    /// @notice The version of `(kind, selector, imageId)`, which must be active (`VersionNotActive` otherwise): what
    ///         the Intake and the slasher call before they verify a seal.
    function activeVersion(VersionKind kind, bytes4 selector, bytes32 imageId) external view returns (Version memory);

    /// @notice `branchId` is non-zero and one of the version's branches.
    function branchAllowed(bytes32 versionId, uint32 branchId) external view returns (bool);

    /// @notice Every active violation version's verifier is stopped (and there is one): the estop shortcut is open.
    function violationStopped() external view returns (bool);

    function version(bytes32 versionId) external view returns (Version memory);
    /// @notice Every version id ever scheduled for `kind`, in order.
    function versionIds(VersionKind kind) external view returns (bytes32[] memory);
    function nonce() external view returns (uint256);
    function depositDelay() external view returns (uint64);
    function violationDelay() external view returns (uint64);
    function estopDelay() external view returns (uint64);
    function router() external view returns (address);
    function registry() external view returns (address);
    function hashScheduleVersion(
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] calldata branches,
        uint256 nonce,
        uint64 deadline
    ) external view returns (bytes32);
    function hashRemoveVersion(bytes32 versionId, uint256 nonce, uint64 deadline) external view returns (bytes32);
    function domainSeparator() external view returns (bytes32);
}
