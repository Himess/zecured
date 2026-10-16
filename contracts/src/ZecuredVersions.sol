// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {IZecuredVersions, Version, VersionKind} from "./interfaces/v3/IZecuredVersions.sol";
import {IZecuredRegistry} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake} from "./interfaces/v3/IZecuredIntake.sol";
import {IRiscZeroVerifierRouter, IRiscZeroEmergencyStop} from "./interfaces/IRiscZeroVerifierRouter.sol";

/// @title ZecuredVersions
/// @notice The verifier version registry (design notes T-07a; rules in `IZecuredVersions`): the Intake verifies deposit
///         proofs, and the slasher violation proofs, only with an active version of their kind. No owner, no upgrade
///         path: versions change only by the signers' signatures, a new one only by all five and after a public delay.
contract ZecuredVersions is IZecuredVersions, EIP712 {
    uint256 private constant SIGNER_COUNT = 5;
    uint256 private constant REMOVE_THRESHOLD = 3;
    bytes32 private constant SCHEDULE_VERSION_TYPEHASH = keccak256(
        "ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)"
    );
    bytes32 private constant REMOVE_VERSION_TYPEHASH =
        keccak256("RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)");

    /// @inheritdoc IZecuredVersions
    address public immutable router;
    /// @inheritdoc IZecuredVersions
    address public immutable registry;
    /// @inheritdoc IZecuredVersions
    uint64 public immutable depositDelay;
    /// @inheritdoc IZecuredVersions
    uint64 public immutable violationDelay;
    /// @inheritdoc IZecuredVersions
    uint64 public immutable estopDelay;

    // The Registry's signer set, strictly increasing (read once at construction).
    address private immutable _signer0;
    address private immutable _signer1;
    address private immutable _signer2;
    address private immutable _signer3;
    address private immutable _signer4;

    mapping(bytes32 versionId => Version) private _versions;
    mapping(VersionKind kind => bytes32[]) private _ids;
    /// @inheritdoc IZecuredVersions
    uint256 public nonce;

    /// @param registry_ The Registry: its five signers sign here, and its Intake must verify through `router_`.
    /// @param router_ RISC Zero's verifier router (the Intake's and the slasher's `verifier`).
    /// @param depositDelay_ Delay of a new deposit version (14 days on mainnet).
    /// @param violationDelay_ Delay of a new violation version (7 days on mainnet).
    /// @param estopDelay_ Delay of a new violation version while `violationStopped()` (1 day on mainnet); at most
    ///        `violationDelay_`.
    /// @param initial The versions active from the start (`activeAt` and `removed` ignored): at least one of each kind,
    ///        every violation version running.
    constructor(
        address registry_,
        address router_,
        uint64 depositDelay_,
        uint64 violationDelay_,
        uint64 estopDelay_,
        Version[] memory initial
    ) EIP712("ZecuredVersions", "1") {
        if (registry_ == address(0) || router_ == address(0)) revert ZeroAddress();
        if (depositDelay_ == 0 || estopDelay_ == 0 || estopDelay_ > violationDelay_) revert InvalidConfig();
        if (address(IZecuredIntake(IZecuredRegistry(registry_).intake()).verifier()) != router_) {
            revert InvalidConfig();
        }
        address[5] memory s = IZecuredRegistry(registry_).signers();
        _signer0 = s[0];
        _signer1 = s[1];
        _signer2 = s[2];
        _signer3 = s[3];
        _signer4 = s[4];
        router = router_;
        registry = registry_;
        depositDelay = depositDelay_;
        violationDelay = violationDelay_;
        estopDelay = estopDelay_;

        bool deposit;
        bool violation;
        for (uint256 i; i < initial.length; ++i) {
            Version memory v = initial[i];
            _checkShape(v.kind, v.selector, v.imageId, v.branches);
            bytes32 id = versionIdOf(v.kind, v.selector, v.imageId);
            if (_versions[id].activeAt != 0) revert VersionExists(id);
            if (v.kind == VersionKind.Violation) {
                if (_stopped(router_, v.selector)) revert InvalidConfig();
                violation = true;
            } else {
                deposit = true;
            }
            // A uint64 of seconds outlasts the chain.
            // forge-lint: disable-next-line(unsafe-typecast)
            _store(id, v.kind, v.selector, v.imageId, v.branches, uint64(block.timestamp), true);
        }
        if (!deposit || !violation) revert InvalidConfig();
    }

    // ------------------------------------------------------------------ changes

    /// @inheritdoc IZecuredVersions
    function scheduleVersion(
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] calldata branches,
        uint256 nonce_,
        uint64 deadline,
        bytes[] calldata sigs
    ) external returns (bytes32 versionId) {
        _checkMessage(nonce_, deadline);
        _verifySignerSignatures(
            hashScheduleVersion(kind, selector, imageId, branches, nonce_, deadline), sigs, SIGNER_COUNT
        );
        nonce = nonce_ + 1;
        _checkShape(kind, selector, imageId, branches);
        versionId = versionIdOf(kind, selector, imageId);
        Version storage existing = _versions[versionId];
        bool known = existing.activeAt != 0;
        if (known && !existing.removed) revert VersionExists(versionId);
        uint64 delay = kind == VersionKind.Deposit ? depositDelay : (violationStopped() ? estopDelay : violationDelay);
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 activeAt = uint64(block.timestamp) + delay;
        _store(versionId, kind, selector, imageId, branches, activeAt, !known);
        emit VersionScheduled(versionId, kind, selector, imageId, activeAt);
    }

    /// @inheritdoc IZecuredVersions
    function removeVersion(bytes32 versionId, uint256 nonce_, uint64 deadline, bytes[] calldata sigs) external {
        _checkMessage(nonce_, deadline);
        _verifySignerSignatures(hashRemoveVersion(versionId, nonce_, deadline), sigs, REMOVE_THRESHOLD);
        nonce = nonce_ + 1;
        Version storage v = _versions[versionId];
        if (v.activeAt == 0 || v.removed) revert UnknownVersion(versionId);
        v.removed = true;
        // I28: slashing always has a verifier (a version only scheduled does not count yet).
        if (v.kind == VersionKind.Violation && !_anyActive(VersionKind.Violation)) {
            revert LastViolationVersion(versionId);
        }
        emit VersionRemoved(versionId);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredVersions
    function versionIdOf(VersionKind kind, bytes4 selector, bytes32 imageId) public pure returns (bytes32) {
        return keccak256(abi.encode(kind, selector, imageId));
    }

    /// @inheritdoc IZecuredVersions
    function isActive(bytes32 versionId) external view returns (bool) {
        return _isActive(_versions[versionId]);
    }

    /// @inheritdoc IZecuredVersions
    function activeVersion(VersionKind kind, bytes4 selector, bytes32 imageId) external view returns (Version memory) {
        bytes32 id = versionIdOf(kind, selector, imageId);
        Version storage v = _versions[id];
        if (!_isActive(v)) revert VersionNotActive(id);
        return v;
    }

    /// @inheritdoc IZecuredVersions
    function branchAllowed(bytes32 versionId, uint32 branchId) external view returns (bool) {
        if (branchId == 0) return false;
        uint32[4] storage b = _versions[versionId].branches;
        return b[0] == branchId || b[1] == branchId || b[2] == branchId || b[3] == branchId;
    }

    /// @inheritdoc IZecuredVersions
    function violationStopped() public view returns (bool) {
        bytes32[] storage ids = _ids[VersionKind.Violation];
        bool any;
        for (uint256 i; i < ids.length; ++i) {
            Version storage v = _versions[ids[i]];
            if (!_isActive(v)) continue;
            if (!_stopped(router, v.selector)) return false;
            any = true;
        }
        return any;
    }

    /// @inheritdoc IZecuredVersions
    function version(bytes32 versionId) external view returns (Version memory) {
        return _versions[versionId];
    }

    /// @inheritdoc IZecuredVersions
    function versionIds(VersionKind kind) external view returns (bytes32[] memory) {
        return _ids[kind];
    }

    /// @inheritdoc IZecuredVersions
    function hashScheduleVersion(
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] calldata branches,
        uint256 nonce_,
        uint64 deadline
    ) public view returns (bytes32) {
        return _hashTypedDataV4(
            keccak256(
                abi.encode(
                    SCHEDULE_VERSION_TYPEHASH,
                    uint8(kind),
                    selector,
                    imageId,
                    keccak256(abi.encode(branches)),
                    nonce_,
                    deadline
                )
            )
        );
    }

    /// @inheritdoc IZecuredVersions
    function hashRemoveVersion(bytes32 versionId, uint256 nonce_, uint64 deadline) public view returns (bytes32) {
        return _hashTypedDataV4(keccak256(abi.encode(REMOVE_VERSION_TYPEHASH, versionId, nonce_, deadline)));
    }

    /// @inheritdoc IZecuredVersions
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ internal

    function _store(
        bytes32 id,
        VersionKind kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] memory branches,
        uint64 activeAt,
        bool isNew
    ) internal {
        _versions[id] = Version({
            kind: kind, selector: selector, imageId: imageId, branches: branches, activeAt: activeAt, removed: false
        });
        if (isNew) _ids[kind].push(id);
    }

    /// @dev A selector and an image id; a violation version names no branch (its journal has none).
    function _checkShape(VersionKind kind, bytes4 selector, bytes32 imageId, uint32[4] memory branches) internal pure {
        if (selector == bytes4(0) || imageId == bytes32(0)) revert InvalidVersion();
        if (kind == VersionKind.Violation && (branches[0] | branches[1] | branches[2] | branches[3]) != 0) {
            revert InvalidVersion();
        }
    }

    function _isActive(Version storage v) internal view returns (bool) {
        return v.activeAt != 0 && v.activeAt <= block.timestamp && !v.removed;
    }

    function _anyActive(VersionKind kind) internal view returns (bool) {
        bytes32[] storage ids = _ids[kind];
        for (uint256 i; i < ids.length; ++i) {
            if (_isActive(_versions[ids[i]])) return true;
        }
        return false;
    }

    /// @dev The router no longer verifies `selector`'s seals: it refuses the selector (unknown or removed), or the
    ///      emergency-stop wrapper it dispatches to is paused. Anything else counts as running.
    function _stopped(address router_, bytes4 selector) internal view returns (bool) {
        try IRiscZeroVerifierRouter(router_).getVerifier(selector) returns (address wrapper) {
            try IRiscZeroEmergencyStop(wrapper).paused() returns (bool paused) {
                return paused;
            } catch {
                return false;
            }
        } catch {
            return true;
        }
    }

    function _checkMessage(uint256 nonce_, uint64 deadline) internal view {
        if (nonce_ != nonce) revert BadNonce(nonce);
        if (block.timestamp > deadline) revert SignatureExpired(deadline);
    }

    /// @dev At least `required` signatures; each a valid, non-malleable ECDSA signature of a signer, recovered signers
    ///      strictly increasing (so no signer counts twice).
    function _verifySignerSignatures(bytes32 digest, bytes[] calldata sigs, uint256 required) internal view {
        if (sigs.length < required) revert InsufficientSignatures(sigs.length);
        address previous = address(0);
        for (uint256 i; i < sigs.length; ++i) {
            address recovered = ECDSA.recoverCalldata(digest, sigs[i]);
            if (
                recovered != _signer0 && recovered != _signer1 && recovered != _signer2 && recovered != _signer3
                    && recovered != _signer4
            ) revert UnknownSigner(recovered);
            if (recovered <= previous) revert SignaturesNotSorted(i);
            previous = recovered;
        }
    }
}
