// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Deposit attestation of the signer nodes (Tier 1, docs/INTERFACES.md § 2.2) under the Registry domain. No v3
///         contract verifies it: every deposit is proven (docs/DECISIONS.md D41).
/// @param vaultId Random 32-byte vault id chosen off-chain when the vault is opened. Never zero.
/// @param account The vault's smart account (the Morpho position owner). Never zero.
/// @param amount Collateral in zatoshi (1 zZEC wei = 1 zatoshi). Never zero.
/// @param depositTag Keyed tag of the conforming note; unlinkable to Zcash data without the vault secret. Never zero.
struct DepositAttestation {
    bytes32 vaultId;
    address account;
    uint64 amount;
    bytes32 depositTag;
}

/// @notice Close-from-collateral request signed by the vault owner through the smart account (ERC-1271)
///         (docs/INTERFACES.md § 2.4), verified on-chain by `SettlementAdapter.closeWithCollateral` under the Adapter
///         domain.
/// @param vaultId The vault to close.
/// @param account The vault's smart account (ERC-1271 signer).
/// @param executor The only address allowed to execute the request (the quoting market maker).
/// @param marginBps The market maker's quoted margin in basis points (capped by `SettlementAdapter.MAX_MARGIN_BPS`).
/// @param minUserAmount Minimum zatoshi the borrower must receive (price-move protection).
/// @param userCommitment keccak256(salt32 || receiver43) of the borrower's Ironwood destination.
/// @param nonce Per-account nonce; each value is usable once (and can be invalidated by the account).
/// @param deadline Unix timestamp; the request is valid while block.timestamp <= deadline.
struct CloseRequest {
    bytes32 vaultId;
    address account;
    address executor;
    uint16 marginBps;
    uint64 minUserAmount;
    bytes32 userCommitment;
    uint256 nonce;
    uint64 deadline;
}

/// @notice Self-deleverage request (S18) signed by the vault owner through the smart account (ERC-1271), verified
///         on-chain by `SettlementAdapter.deleverageWithCollateral` under the Adapter domain: the executor repays part of
///         the debt and is paid in collateral, and the vault stays open with a lower LTV.
/// @param vaultId The vault.
/// @param account The vault's smart account (ERC-1271 signer).
/// @param executor The only address allowed to execute the request (the quoting market maker).
/// @param marginBps The market maker's quoted margin in basis points (capped by `SettlementAdapter.MAX_MARGIN_BPS`).
/// @param repayAssets The debt the executor repays, in loan-token units.
/// @param maxCollateral The most zatoshi the borrower gives for it (price-move protection).
/// @param nonce Per-account nonce, shared with CloseRequest; each value is usable once.
/// @param deadline Unix timestamp; the request is valid while block.timestamp <= deadline.
struct DeleverageRequest {
    bytes32 vaultId;
    address account;
    address executor;
    uint16 marginBps;
    uint256 repayAssets;
    uint64 maxCollateral;
    uint256 nonce;
    uint64 deadline;
}

/// @notice Tier 3 payout commitment (docs/TIER3.md § 3), signed by at least 3 signers in FROST round 1, before any of
///         them releases a signature share, and recorded by the Registry's `commitSpend` under the Registry domain.
///         Every field is a keyed or salted hash, a Base id or a value Base can compute: nothing links to Zcash.
/// @param vaultId The paying vault.
/// @param inputTag `noteTag` of the lineage note the payout spends: the activated deposit, or the change of the
///        vault's previous committed payout. A payout spends exactly one lineage note.
/// @param claimIds The claims the payout pays (at least one, all Pending), in plan order; `claimIds[0]` bears the fee.
/// @param claimRefs `keccak256(salt_i || txid)` of `claimIds[i]` (`salt_i` from its recipient commitment, txid in
///        internal byte order): lets the claimant, who knows `salt_i` but not the vault's key, find the payout.
/// @param claimOutputs The index, among the payout's Ironwood actions, of the output that pays `claimIds[i]`; distinct
///        (E1): each claim is paid by its own output, which F3 checks exactly.
/// @param payoutRef BLAKE2b-256 of the payout's txid keyed with the vault's tag key (`policy::derive::payout_ref`).
/// @param changeTag `noteTag` of the change note the payout returns to the vault; zero if it returns none.
/// @param changeValue Zatoshi of that change note; zero if none.
struct SpendCommit {
    bytes32 vaultId;
    bytes32 inputTag;
    bytes32[] claimIds;
    bytes32[] claimRefs;
    uint8[] claimOutputs;
    bytes32 payoutRef;
    bytes32 changeTag;
    uint64 changeValue;
}

/// @title ZecuredEIP712
/// @notice Single source of truth for the EIP-712 domains and type hashes of docs/INTERFACES.md § 2. Both
///         ZecuredRegistry and SettlementAdapter hash through this library, so they cannot drift apart; the Rust signer
///         and the TypeScript app must produce exactly these hashes (see test/unit/ZecuredEIP712.t.sol for vectors
///         cross-checked against an independent EIP-712 implementation).
/// @dev Domains use the full OpenZeppelin EIP712 domain:
///      EIP712Domain(string name,string version,uint256 chainId,address verifyingContract).
library ZecuredEIP712 {
    // ------------------------------------------------------------------ domains (§ 2.1)

    /// @notice Domain name of ZecuredRegistry: DepositAttestation, PayoutConfirmation, VaultOpened,
    ///         VaultCancellation, SpendCommit and every off-chain owner request (OpenVault, Destination,
    ///         RefundRequest, ViewKeyRequest).
    string internal constant REGISTRY_DOMAIN_NAME = "ZecuredRegistry";
    /// @notice Domain name of SettlementAdapter: CloseRequest and DeleverageRequest.
    string internal constant ADAPTER_DOMAIN_NAME = "ZecuredSettlementAdapter";
    /// @notice Domain name of ZecuredIntake (v3): PauseMinting, UnpauseMinting.
    string internal constant INTAKE_DOMAIN_NAME = "ZecuredIntake";
    /// @notice Domain name of ZecuredVersions (v3): ScheduleVersion, RemoveVersion, ScheduleAdapter, RemoveAdapter.
    string internal constant VERSIONS_DOMAIN_NAME = "ZecuredVersions";
    /// @notice Domain version of every domain.
    string internal constant DOMAIN_VERSION = "1";

    // ------------------------------------------------------------------ verified on-chain

    /// @dev keccak256("DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)")
    bytes32 internal constant DEPOSIT_ATTESTATION_TYPEHASH =
        keccak256("DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)");

    /// @dev keccak256("PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)").
    ///      `claimIdsHash = keccak256(abi.encodePacked(claimIds))`, i.e. the hash of the concatenated 32-byte ids.
    bytes32 internal constant PAYOUT_CONFIRMATION_TYPEHASH =
        keccak256("PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)");

    /// @dev keccak256("VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,
    ///      bytes32 addressHash,bytes32 tagKeyHash)") (one line, no spaces). v2 (guest round, design notes T-09). Signed by
    ///      all five signers when a vault's DKG completes (D20). The owner's app checks it off-chain;
    ///      `attestDepositWithProof` checks it on-chain against the struct hash a deposit proof computed, so the address
    ///      and tag-key hashes never reach Base. `tagKeyHash = keccak256(K)` of the vault's tag key
    ///      K = BLAKE2b-256("Zecured_TagKey__", UFVK): the proof derives its deposit tag with the K it hashes here, so the
    ///      signatures bind the tag to the vault and one note cannot land under two tags.
    bytes32 internal constant VAULT_OPENED_TYPEHASH = keccak256(
        "VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 tagKeyHash)"
    );

    /// @dev keccak256("VaultCancellation(bytes32 vaultId)"): signed by at least 3 signers before they refund the deposit
    ///      of a vault that was never activated (L-8); afterwards the vault can never be activated.
    bytes32 internal constant VAULT_CANCELLATION_TYPEHASH = keccak256("VaultCancellation(bytes32 vaultId)");

    /// @dev keccak256("CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,
    ///      uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)") (one line, no spaces).
    bytes32 internal constant CLOSE_REQUEST_TYPEHASH = keccak256(
        "CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)"
    );

    /// @dev keccak256("DeleverageRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,
    ///      uint256 repayAssets,uint64 maxCollateral,uint256 nonce,uint64 deadline)") (one line, no spaces). S18.
    bytes32 internal constant DELEVERAGE_REQUEST_TYPEHASH = keccak256(
        "DeleverageRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint256 repayAssets,uint64 maxCollateral,uint256 nonce,uint64 deadline)"
    );

    /// @dev keccak256("SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,
    ///      uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)") (one line, no spaces).
    ///      Tier 3 (docs/TIER3.md): verified by the Registry's `commitSpend`.
    bytes32 internal constant SPEND_COMMIT_TYPEHASH = keccak256(
        "SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)"
    );

    // ------------------------------------------------------------------ v3 (design notes T-03, T-05, T-07)

    /// @dev Registry domain (T-05): three signers point a claim the bonds paid at the destination that reimburses them.
    bytes32 internal constant REIMBURSE_DESTINATION_TYPEHASH =
        keccak256("ReimburseDestination(bytes32 claimId,bytes32 commitment,uint64 deadline)");

    /// @dev Intake domain (T-03): three signers pause new minting until `until` (at most 30 days ahead).
    bytes32 internal constant PAUSE_MINTING_TYPEHASH =
        keccak256("PauseMinting(uint64 until,uint256 nonce,uint64 deadline)");

    /// @dev Intake domain (T-03): three signers lift the pause.
    bytes32 internal constant UNPAUSE_MINTING_TYPEHASH = keccak256("UnpauseMinting(uint256 nonce,uint64 deadline)");

    /// @dev Versions domain (T-07a): all five signers schedule a verifier version. `branches` is encoded as EIP-712
    ///      encodes a fixed array: the keccak256 of its four 32-byte words.
    bytes32 internal constant SCHEDULE_VERSION_TYPEHASH = keccak256(
        "ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)"
    );

    /// @dev Versions domain (T-07a): three signers remove a verifier version.
    bytes32 internal constant REMOVE_VERSION_TYPEHASH =
        keccak256("RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)");

    /// @dev Versions domain (T-07b): all five signers schedule a SettlementAdapter.
    bytes32 internal constant SCHEDULE_ADAPTER_TYPEHASH =
        keccak256("ScheduleAdapter(address adapter,uint256 nonce,uint64 deadline)");

    /// @dev Versions domain (T-07b): three signers remove a SettlementAdapter.
    bytes32 internal constant REMOVE_ADAPTER_TYPEHASH =
        keccak256("RemoveAdapter(address adapter,uint256 nonce,uint64 deadline)");

    // ------------------------------------------------------------------ verified off-chain (Registry domain)
    // Signed by the per-loan owner EOA and verified by every signer node (§ 2.3). Not used by the contracts; pinned here
    // so every implementation is tested against the same type strings.

    bytes32 internal constant OPEN_VAULT_TYPEHASH =
        keccak256("OpenVault(address owner,address account,uint64 amount,bytes32 requestId,uint64 deadline)");
    bytes32 internal constant DESTINATION_TYPEHASH =
        keccak256("Destination(bytes32 vaultId,bytes32 commitment,uint64 nonce,uint64 deadline)");
    bytes32 internal constant REFUND_REQUEST_TYPEHASH =
        keccak256("RefundRequest(bytes32 vaultId,bytes32 noteId,bytes32 commitment,uint64 nonce,uint64 deadline)");
    bytes32 internal constant VIEW_KEY_REQUEST_TYPEHASH =
        keccak256("ViewKeyRequest(bytes32 vaultId,uint64 nonce,uint64 deadline)");

    // ------------------------------------------------------------------ struct hashes

    /// @notice EIP-712 struct hash of a DepositAttestation.
    function hashDepositAttestation(DepositAttestation memory att) internal pure returns (bytes32) {
        return keccak256(abi.encode(DEPOSIT_ATTESTATION_TYPEHASH, att.vaultId, att.account, att.amount, att.depositTag));
    }

    /// @notice EIP-712 struct hash of a VaultOpened (computed off-chain and inside deposit proofs; here for tests and
    ///         tools).
    function hashVaultOpened(
        bytes32 vaultId,
        address owner,
        address account,
        uint64 amount,
        uint32 expiryHeight,
        bytes32 addressHash,
        bytes32 tagKeyHash
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(VAULT_OPENED_TYPEHASH, vaultId, owner, account, amount, expiryHeight, addressHash, tagKeyHash)
        );
    }

    /// @notice EIP-712 struct hash of a VaultCancellation.
    function hashVaultCancellation(bytes32 vaultId) internal pure returns (bytes32) {
        return keccak256(abi.encode(VAULT_CANCELLATION_TYPEHASH, vaultId));
    }

    /// @notice `claimIdsHash` of docs/INTERFACES.md § 2.2: keccak256 of the concatenated claim ids, in the given order.
    function claimIdsHash(bytes32[] memory claimIds) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(claimIds));
    }

    /// @notice EIP-712 struct hash of a PayoutConfirmation for `vaultId` and the ordered `claimIds`.
    function hashPayoutConfirmation(bytes32 vaultId, bytes32[] memory claimIds) internal pure returns (bytes32) {
        return keccak256(abi.encode(PAYOUT_CONFIRMATION_TYPEHASH, vaultId, claimIdsHash(claimIds)));
    }

    /// @notice EIP-712 struct hash of a SpendCommit. Arrays are encoded as EIP-712 requires: the keccak256 of their
    ///         elements, each encoded as one 32-byte word (`abi.encodePacked` pads array elements to 32 bytes).
    function hashSpendCommit(SpendCommit memory c) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                SPEND_COMMIT_TYPEHASH,
                c.vaultId,
                c.inputTag,
                keccak256(abi.encodePacked(c.claimIds)),
                keccak256(abi.encodePacked(c.claimRefs)),
                keccak256(abi.encodePacked(c.claimOutputs)),
                c.payoutRef,
                c.changeTag,
                c.changeValue
            )
        );
    }

    /// @notice EIP-712 struct hash of a DeleverageRequest.
    function hashDeleverageRequest(DeleverageRequest memory req) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                DELEVERAGE_REQUEST_TYPEHASH,
                req.vaultId,
                req.account,
                req.executor,
                req.marginBps,
                req.repayAssets,
                req.maxCollateral,
                req.nonce,
                req.deadline
            )
        );
    }

    /// @notice EIP-712 struct hash of a ReimburseDestination (T-05).
    function hashReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(REIMBURSE_DESTINATION_TYPEHASH, claimId, commitment, deadline));
    }

    /// @notice EIP-712 struct hash of a PauseMinting (T-03).
    function hashPauseMinting(uint64 until, uint256 nonce, uint64 deadline) internal pure returns (bytes32) {
        return keccak256(abi.encode(PAUSE_MINTING_TYPEHASH, until, nonce, deadline));
    }

    /// @notice EIP-712 struct hash of an UnpauseMinting (T-03).
    function hashUnpauseMinting(uint256 nonce, uint64 deadline) internal pure returns (bytes32) {
        return keccak256(abi.encode(UNPAUSE_MINTING_TYPEHASH, nonce, deadline));
    }

    /// @notice EIP-712 struct hash of a ScheduleVersion (T-07a).
    function hashScheduleVersion(
        uint8 kind,
        bytes4 selector,
        bytes32 imageId,
        uint32[4] memory branches,
        uint256 nonce,
        uint64 deadline
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                SCHEDULE_VERSION_TYPEHASH,
                kind,
                selector,
                imageId,
                keccak256(
                    abi.encodePacked(
                        uint256(branches[0]), uint256(branches[1]), uint256(branches[2]), uint256(branches[3])
                    )
                ),
                nonce,
                deadline
            )
        );
    }

    /// @notice EIP-712 struct hash of a RemoveVersion (T-07a).
    function hashRemoveVersion(bytes32 versionId, uint256 nonce, uint64 deadline) internal pure returns (bytes32) {
        return keccak256(abi.encode(REMOVE_VERSION_TYPEHASH, versionId, nonce, deadline));
    }

    /// @notice EIP-712 struct hash of a ScheduleAdapter (T-07b).
    function hashScheduleAdapter(address adapter, uint256 nonce, uint64 deadline) internal pure returns (bytes32) {
        return keccak256(abi.encode(SCHEDULE_ADAPTER_TYPEHASH, adapter, nonce, deadline));
    }

    /// @notice EIP-712 struct hash of a RemoveAdapter (T-07b).
    function hashRemoveAdapter(address adapter, uint256 nonce, uint64 deadline) internal pure returns (bytes32) {
        return keccak256(abi.encode(REMOVE_ADAPTER_TYPEHASH, adapter, nonce, deadline));
    }

    /// @notice EIP-712 struct hash of a CloseRequest.
    function hashCloseRequest(CloseRequest memory req) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                CLOSE_REQUEST_TYPEHASH,
                req.vaultId,
                req.account,
                req.executor,
                req.marginBps,
                req.minUserAmount,
                req.userCommitment,
                req.nonce,
                req.deadline
            )
        );
    }
}
