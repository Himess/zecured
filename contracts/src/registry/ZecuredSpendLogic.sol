// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IZecuredRegistry, ClaimKind, ClaimStatus, VaultStatus} from "../interfaces/v3/IZecuredRegistry.sol";
import {
    IZecuredSpendCommitsEvents,
    LineageNote,
    ClaimPayment,
    SpendRecord
} from "../interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredDisputes} from "../interfaces/v3/IZecuredDisputes.sol";
import {IZecuredSlasher} from "../interfaces/v3/IZecuredSlasher.sol";
import {ZecuredEIP712, SpendCommit} from "../libraries/ZecuredEIP712.sol";
import {ZecuredRegistryStorage} from "./ZecuredRegistryStorage.sol";

/// @title ZecuredSpendLogic
/// @notice The code of `ZecuredRegistry.commitSpend` (docs/TIER3.md § 4) and of the dispute path (`IZecuredDisputes`,
///         which the Registry's fallback serves), run by the Registry with delegatecall on its own storage: it lives in
///         a separate contract only to keep the Registry under EIP-170 (T-11). It shares the Registry's storage layout,
///         EIP-712 domain name and signer set through `ZecuredRegistryStorage`; its domain separator is computed for the
///         Registry's address, since under delegatecall `address(this)` is the Registry. Called directly, it does
///         nothing (`NotDelegated`).
contract ZecuredSpendLogic is ZecuredRegistryStorage, IZecuredSpendCommitsEvents, IZecuredDisputes {
    /// @notice The only Registry that may run this code (its address, predicted before deployment).
    address public immutable registry;
    /// @inheritdoc IZecuredDisputes
    uint64 public immutable disputeWindow;
    /// @inheritdoc IZecuredDisputes
    uint64 public immutable inclusionWindow;

    error NotDelegated();
    error InvalidDisputeWindows();

    /// @param registry_ The Registry's (predicted) address.
    /// @param signers_ The Registry's signer set, strictly increasing.
    /// @param disputeWindow_ How long after its confirmation a claim can be disputed (T-05 (a)).
    /// @param inclusionWindow_ How long the signers have to prove a disputed payout (at least the time a payout's block
    ///        takes to gain the slasher's `slashConfirmationWork`, plus proving).
    constructor(address registry_, address[5] memory signers_, uint64 disputeWindow_, uint64 inclusionWindow_)
        ZecuredRegistryStorage(signers_)
    {
        if (disputeWindow_ == 0 || inclusionWindow_ == 0) revert InvalidDisputeWindows();
        registry = registry_;
        disputeWindow = disputeWindow_;
        inclusionWindow = inclusionWindow_;
    }

    /// @notice The signer set this logic verifies signatures against (the Registry checks it equals its own).
    function signers() external view returns (address[5] memory) {
        return [_signer0, _signer1, _signer2, _signer3, _signer4];
    }

    /// @notice See `IZecuredSpendCommits.commitSpend`. The Registry has already refused a compromised vault.
    function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external {
        _onlyDelegated();
        uint64 inputValue = _checkSpendShape(c);
        bytes32 recordHash = ZecuredEIP712.hashSpendCommit(c);
        uint8 signerBitmap = _verifySignerSignatures(_hashTypedDataV4(recordHash), sigs, SIGNER_THRESHOLD);

        uint256 n = c.claimIds.length;
        uint64 fee = MARGINAL_FEE * uint64(Math.max(GRACE_ACTIONS, c.changeValue != 0 ? n + 1 : n));
        // T-06: each Fee claim after the first position bears its own output; `claimIds[0]` bears the rest.
        for (uint256 i = 1; i < n; ++i) {
            if (_claims[c.claimIds[i]].kind == ClaimKind.Fee) fee -= MARGINAL_FEE;
        }
        uint256 claimed;
        for (uint256 i; i < n; ++i) {
            claimed += _commitClaim(c, i, fee);
        }
        if (claimed + c.changeValue != inputValue) revert ConservationBroken(inputValue, claimed, c.changeValue);
        _recordSpend(c, recordHash, signerBitmap);
    }

    /// @dev The parts of a commitment that need no claim lookup: a new, non-zero payout ref; an input that is a lineage
    ///      note of the vault, which is not pending; 1 to MAX_CLAIMS claims with one ref each; a change tag exactly when there is change, not
    ///      already a lineage note. Returns the input's value.
    function _checkSpendShape(SpendCommit calldata c) internal view returns (uint64) {
        if (c.payoutRef == bytes32(0)) revert ZeroPayoutRef();
        if (_spends[c.payoutRef].vaultId != bytes32(0)) revert PayoutAlreadyCommitted(c.payoutRef);
        LineageNote memory input = _lineage[c.inputTag];
        if (c.vaultId == bytes32(0) || input.vaultId != c.vaultId) revert NotALineageNote(c.vaultId, c.inputTag);
        // K3: nothing is paid from a vault whose activation is pending (its reservation may still be released).
        if (_vaults[c.vaultId].status == VaultStatus.Pending) revert VaultPending(c.vaultId);
        uint256 n = c.claimIds.length;
        if (n == 0 || n > MAX_CLAIMS || c.claimRefs.length != n || c.claimOutputs.length != n) {
            revert InvalidClaimCount(n, c.claimRefs.length == n ? c.claimOutputs.length : c.claimRefs.length);
        }
        bool withChange = c.changeValue != 0;
        if (withChange != (c.changeTag != bytes32(0)) || (withChange && _lineage[c.changeTag].vaultId != bytes32(0))) {
            revert InvalidChange(c.changeTag, c.changeValue);
        }
        // RT-4: one claim set per input, so a claim can never stay bound to a note a payout without it spent.
        bytes32 claimSet = _inputClaimSet[c.inputTag];
        if (claimSet != bytes32(0) && claimSet != keccak256(abi.encodePacked(c.claimIds))) {
            revert ClaimSetMismatch(c.inputTag);
        }
        return input.value;
    }

    /// @dev Records a checked commitment; its change becomes a lineage note of the vault.
    function _recordSpend(SpendCommit calldata c, bytes32 recordHash, uint8 signerBitmap) internal {
        if (_inputClaimSet[c.inputTag] == bytes32(0)) {
            _inputClaimSet[c.inputTag] = keccak256(abi.encodePacked(c.claimIds));
        }
        _lastCommitAt[c.inputTag] = uint40(block.timestamp);
        _spends[c.payoutRef] = SpendRecord({
            recordHash: recordHash,
            vaultId: c.vaultId,
            inputTag: c.inputTag,
            changeTag: c.changeTag,
            changeValue: c.changeValue,
            claimCount: uint8(c.claimIds.length),
            signerBitmap: signerBitmap
        });
        if (c.changeValue != 0) _lineage[c.changeTag] = LineageNote({vaultId: c.vaultId, value: c.changeValue});
        emit SpendCommitted(
            c.vaultId, c.payoutRef, c.inputTag, c.claimIds, c.claimRefs, c.changeTag, c.changeValue, signerBitmap
        );
    }

    /// @dev One claim of a commitment (docs/TIER3.md § 4): distinct, of the vault, pending, with a destination, bound to
    ///      this input (the first commitment binds it), paid by its own output (E1), with an unused claim ref, which
    ///      records what the claimant must receive and where. `claimIds[0]` bears `fee`; a Fee claim elsewhere bears
    ///      its own output (T-06). Returns the claim's amount.
    function _commitClaim(SpendCommit calldata c, uint256 i, uint64 fee) internal returns (uint64 amount) {
        bytes32 id = c.claimIds[i];
        uint8 output = c.claimOutputs[i];
        for (uint256 k; k < i; ++k) {
            if (c.claimIds[k] == id) revert DuplicateClaim(id);
            if (c.claimOutputs[k] == output) revert DuplicateClaimOutput(output);
        }
        Claim storage cl = _claims[id];
        if (cl.vaultId != c.vaultId || cl.status != ClaimStatus.Pending) revert ClaimNotPayable(id);
        // F4 (RT-1): each claim's reference is kept under its recipient commitment, distinct within a payout (the
        // payout reference is new, so an entry can only come from an earlier claim of this commitment).
        bytes32 commitment = cl.commitment;
        if (commitment == bytes32(0)) revert ClaimDestinationUnset(id);
        if (_claimRefOf[c.payoutRef][commitment] != bytes32(0)) revert DuplicateClaimCommitment(commitment);
        bytes32 bound = _claimInput[id];
        if (bound == bytes32(0)) _claimInput[id] = c.inputTag;
        else if (bound != c.inputTag) revert ClaimBoundToAnotherInput(id, bound);

        bytes32 ref = c.claimRefs[i];
        if (ref == bytes32(0) || _claimPayments[ref].claimId != bytes32(0)) revert ClaimRefAlreadyUsed(ref);
        amount = cl.amount;
        uint64 share = i == 0 ? fee : (cl.kind == ClaimKind.Fee ? MARGINAL_FEE : 0);
        if (share != 0 && amount <= share) revert FeeBearerIsDust(id, amount, share);
        _claimPayments[ref] =
            ClaimPayment({claimId: id, payoutRef: c.payoutRef, expected: amount - share, outputIndex: output});
        _claimRefOf[c.payoutRef][commitment] = ref;
    }

    // ------------------------------------------------------------------ disputes (T-05 (a))

    /// @inheritdoc IZecuredDisputes
    function disputePayout(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        uint64 paidAt = c.paidAt;
        // A bond-paid claim's claimant has its USDC; an upheld confirmation has `paidAt` zero.
        if (c.status != ClaimStatus.Paid || paidAt == 0 || c.bondPaid) revert NotDisputable(claimId);
        address claimant = _claimant(claimId, c);
        if (claimant == address(0)) revert NotDisputable(claimId);
        if (msg.sender != claimant) revert NotClaimant(claimId, msg.sender);
        uint64 closesAt = paidAt + disputeWindow;
        if (block.timestamp > closesAt) revert DisputeWindowClosed(claimId, closesAt);
        bytes32 vaultId = c.vaultId;
        // The compensator has tallied a compromised vault's claims: a claim unpaid again there would never settle.
        address slasher = IZecuredRegistry(address(this)).slasher();
        if (slasher != address(0) && IZecuredSlasher(slasher).isCompromised(vaultId)) {
            revert DisputeVaultCompromised(vaultId);
        }

        // Unpaid again until resolved: it keeps (or reopens) the vault and counts in `lockedZatoshi`.
        Vault storage v = _vaults[vaultId];
        if (_settled(v)) _openVaults += 1;
        v.unpaidClaims += 1;
        _pendingClaimZat += c.amount;
        // A uint64 of seconds outlasts the chain.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 dueAt = uint64(block.timestamp) + inclusionWindow;
        c.status = ClaimStatus.Disputed;
        c.paidAt = 0;
        // forge-lint: disable-next-line(unsafe-typecast)
        c.disputeDueAt = uint40(dueAt);
        emit PayoutDisputed(vaultId, claimId, dueAt);
    }

    /// @inheritdoc IZecuredDisputes
    function upholdPayout(bytes32 claimId, bytes32 payoutRef) external {
        _onlyDelegated();
        address slasher = IZecuredRegistry(address(this)).slasher();
        if (msg.sender != slasher || slasher == address(0)) revert OnlySlasher(msg.sender);
        Claim storage c = _claims[claimId];
        if (c.status != ClaimStatus.Disputed) revert NotDisputed(claimId);
        // Every commitment of an input pays the same claims (RT-4), so any committed payout of this claim's input pays
        // it.
        bytes32 input = _claimInput[claimId];
        if (input == bytes32(0) || _spends[payoutRef].inputTag != input) revert PayoutNotOfClaim(claimId, payoutRef);
        c.disputeDueAt = 0;
        emit PayoutUpheld(c.vaultId, claimId, payoutRef);
        _closeClaim(c, ClaimStatus.Paid);
    }

    /// @inheritdoc IZecuredDisputes
    function reopenDisputedClaim(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        if (c.status != ClaimStatus.Disputed) revert NotDisputed(claimId);
        uint64 dueAt = c.disputeDueAt;
        if (block.timestamp <= dueAt) revert InclusionWindowOpen(claimId, dueAt);
        // Already counted unpaid; still bound to its input, and its liveness clock (`createdAt`) never restarted.
        c.status = ClaimStatus.Pending;
        c.disputeDueAt = 0;
        emit DisputedClaimReopened(c.vaultId, claimId);
    }

    /// @inheritdoc IZecuredDisputes
    function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt) {
        Claim storage c = _claims[claimId];
        return (c.paidAt, c.disputeDueAt);
    }

    // ------------------------------------------------------------------ internal

    function _onlyDelegated() internal view {
        if (address(this) != registry) revert NotDelegated();
    }

    /// @dev As `ZecuredRegistry.claimDetails`: the third party of a Liquidation or CloseMarketMaker claim, the vault's
    ///      account for the owner's claims, nobody for Fee and Retired claims.
    function _claimant(bytes32 claimId, Claim storage c) internal view returns (address) {
        ClaimKind kind = c.kind;
        if (kind == ClaimKind.Liquidation || kind == ClaimKind.CloseMarketMaker) return _claimantOf[claimId];
        if (kind == ClaimKind.Fee || kind == ClaimKind.Retired) return address(0);
        return _vaults[c.vaultId].account;
    }
}
