// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Deposit attestation signed by at least 3 of the 5 signer nodes (docs/INTERFACES.md § 2.2), verified on-chain
///         by `ZecuredRegistry.attestDeposit` under the Registry domain.
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

/// @notice Tier 3 payout commitment (docs/TIER3.md § 3), signed by at least 3 signers in FROST round 1, before any of
///         them releases a signature share, and recorded by the Registry's `commitSpend` under the Registry domain.
///         Every field is a keyed or salted hash, a Base id or a value Base can compute: nothing links to Zcash.
/// @param vaultId The paying vault.
/// @param inputTag `noteTag` of the lineage note the payout spends: the activated deposit, or the change of the
///        vault's previous committed payout. A payout spends exactly one lineage note.
/// @param claimIds The claims the payout pays (at least one, all Pending), in plan order; `claimIds[0]` bears the fee.
/// @param claimRefs `keccak256(salt_i || txid)` of `claimIds[i]` (`salt_i` from its recipient commitment, txid in
///        internal byte order): lets the claimant, who knows `salt_i` but not the vault's key, find the payout.
/// @param payoutRef BLAKE2b-256 of the payout's txid keyed with the vault's tag key (`policy::derive::payout_ref`).
/// @param changeTag `noteTag` of the change note the payout returns to the vault; zero if it returns none.
/// @param changeValue Zatoshi of that change note; zero if none.
struct SpendCommit {
    bytes32 vaultId;
    bytes32 inputTag;
    bytes32[] claimIds;
    bytes32[] claimRefs;
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
    /// @notice Domain name of SettlementAdapter: CloseRequest.
    string internal constant ADAPTER_DOMAIN_NAME = "ZecuredSettlementAdapter";
    /// @notice Domain version of both domains.
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
    ///      bytes32 addressHash,bytes32 ufvkHash)") (one line, no spaces). Signed by all five signers when a vault's DKG
    ///      completes (D20). The owner's app checks it off-chain; `attestDepositWithProof` checks it on-chain against the
    ///      struct hash a deposit proof computed, so the address and viewing-key hashes never reach Base.
    bytes32 internal constant VAULT_OPENED_TYPEHASH = keccak256(
        "VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 ufvkHash)"
    );

    /// @dev keccak256("VaultCancellation(bytes32 vaultId)"): signed by at least 3 signers before they refund the deposit
    ///      of a vault that was never activated (L-8); afterwards the vault can never be activated.
    bytes32 internal constant VAULT_CANCELLATION_TYPEHASH = keccak256("VaultCancellation(bytes32 vaultId)");

    /// @dev keccak256("CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,
    ///      uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)") (one line, no spaces).
    bytes32 internal constant CLOSE_REQUEST_TYPEHASH = keccak256(
        "CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)"
    );

    /// @dev keccak256("SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,
    ///      bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)") (one line, no spaces). Tier 3 (docs/TIER3.md):
    ///      verified by the Registry's `commitSpend` (Deploy 2).
    bytes32 internal constant SPEND_COMMIT_TYPEHASH = keccak256(
        "SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)"
    );

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
        bytes32 ufvkHash
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(VAULT_OPENED_TYPEHASH, vaultId, owner, account, amount, expiryHeight, addressHash, ufvkHash)
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
    ///         concatenated 32-byte elements (as `claimIdsHash`).
    function hashSpendCommit(SpendCommit memory c) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                SPEND_COMMIT_TYPEHASH,
                c.vaultId,
                c.inputTag,
                keccak256(abi.encodePacked(c.claimIds)),
                keccak256(abi.encodePacked(c.claimRefs)),
                c.payoutRef,
                c.changeTag,
                c.changeValue
            )
        );
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
