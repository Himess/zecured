// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {VaultStatus, ClaimKind, ClaimStatus} from "../interfaces/v3/IZecuredRegistry.sol";
import {LineageNote, SpendRecord, ClaimPayment} from "../interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredSignerErrors} from "../interfaces/v3/IZecuredSignerErrors.sol";
import {ZecuredEIP712} from "../libraries/ZecuredEIP712.sol";

/// @title ZecuredRegistryStorage
/// @notice The Registry's storage layout, EIP-712 domain and signer set, in one place. `ZecuredRegistry` and
///         `ZecuredSpendLogic` both inherit exactly this contract, so the spend logic, which the Registry runs by
///         delegatecall (T-11: the Registry alone would exceed EIP-170), sees the same slots. Nothing else may declare
///         storage in either contract.
abstract contract ZecuredRegistryStorage is EIP712, IZecuredSignerErrors {
    uint256 internal constant SIGNER_SET_SIZE = 5;
    uint256 internal constant SIGNER_THRESHOLD = 3;
    uint256 internal constant MAX_CLAIMS = 8;
    uint64 internal constant MARGINAL_FEE = 5_000;
    uint256 internal constant GRACE_ACTIONS = 2;

    // The signer set, strictly increasing (fixed at construction).
    address internal immutable _signer0;
    address internal immutable _signer1;
    address internal immutable _signer2;
    address internal immutable _signer3;
    address internal immutable _signer4;

    struct Vault {
        address account;
        uint64 outstanding; // zatoshi still locked (= zZEC supplied to Morpho for this vault)
        VaultStatus status;
        uint64 claimCount; // sequence number of the next claim
        uint64 unpaidClaims; // pending claims (the vault is open while any is left)
    }

    struct Claim {
        bytes32 vaultId;
        bytes32 commitment; // keccak256(salt32 || receiver43) of the Ironwood recipient; zero while not yet given
        uint64 amount; // zatoshi
        ClaimKind kind;
        ClaimStatus status;
        uint40 createdAt; // when the claim became payable (T-05)
        bool bondPaid; // T-05
    }

    mapping(bytes32 vaultId => Vault) internal _vaults;
    mapping(address account => bytes32 vaultId) internal _vaultOf;
    mapping(bytes32 claimId => Claim) internal _claims;
    /// @dev The Base address of a Liquidation or CloseMarketMaker claim's claimant (T-04, T-05).
    mapping(bytes32 claimId => address) internal _claimantOf;
    mapping(bytes32 depositTag => bool used) internal _depositTagUsed;

    // Tier 3 (docs/TIER3.md § 4).
    mapping(bytes32 tag => LineageNote) internal _lineage;
    mapping(bytes32 payoutRef => SpendRecord) internal _spends;
    mapping(bytes32 claimRef => ClaimPayment) internal _claimPayments;
    mapping(bytes32 claimId => bytes32 inputTag) internal _claimInput;
    mapping(bytes32 inputTag => bytes32 claimSet) internal _inputClaimSet;
    mapping(bytes32 inputTag => uint40) internal _lastCommitAt;
    mapping(bytes32 payoutRef => mapping(bytes32 commitment => bytes32 claimRef)) internal _claimRefOf;
    uint256 internal _openVaults;
    uint64 internal _lastVaultSettledAt;
    /// @dev T-01: zatoshi of every pending claim (the vaults hold them for Base besides the zZEC supply).
    uint256 internal _pendingClaimZat;

    constructor(address[5] memory signers_) EIP712(ZecuredEIP712.REGISTRY_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION) {
        address previous = address(0);
        for (uint256 i; i < SIGNER_SET_SIZE; ++i) {
            if (signers_[i] <= previous) revert InvalidSignerSet(); // strictly increasing => distinct and non-zero
            previous = signers_[i];
        }
        _signer0 = signers_[0];
        _signer1 = signers_[1];
        _signer2 = signers_[2];
        _signer3 = signers_[3];
        _signer4 = signers_[4];
    }

    /// @dev I6: at least `required` (>= SIGNER_THRESHOLD) signatures; every one must be a valid, non-malleable ECDSA
    ///      signature (OpenZeppelin ECDSA rejects high-s and bad v) of a signer, and the recovered signers must be
    ///      strictly increasing, so the same signer can never be counted twice. Returns who signed: bit i for signer i.
    function _verifySignerSignatures(bytes32 digest, bytes[] calldata sigs, uint256 required)
        internal
        view
        returns (uint8 signerBitmap)
    {
        if (sigs.length < required) revert InsufficientSignatures(sigs.length);
        address previous = address(0);
        for (uint256 i; i < sigs.length; ++i) {
            address recovered = ECDSA.recoverCalldata(digest, sigs[i]);
            uint256 index = _signerIndex(recovered);
            if (index == SIGNER_SET_SIZE) revert UnknownSigner(recovered);
            if (recovered <= previous) revert SignaturesNotSorted(i);
            previous = recovered;
            // index < SIGNER_SET_SIZE: one of the five low bits, which fit in a uint8.
            // forge-lint: disable-next-line(incorrect-shift,unsafe-typecast)
            signerBitmap |= uint8(1 << index);
        }
    }

    /// @dev The signer's index in the sorted set, or SIGNER_SET_SIZE for any other address.
    function _signerIndex(address account) internal view returns (uint256) {
        if (account == _signer0) return 0;
        if (account == _signer1) return 1;
        if (account == _signer2) return 2;
        if (account == _signer3) return 3;
        if (account == _signer4) return 4;
        return SIGNER_SET_SIZE;
    }
}
