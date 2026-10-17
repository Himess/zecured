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
import {IZecuredPayouts} from "../interfaces/v3/IZecuredPayouts.sol";
import {IZecuredRefunds} from "../interfaces/v3/IZecuredRefunds.sol";
import {IZecuredSlasher} from "../interfaces/v3/IZecuredSlasher.sol";
import {ZecuredEIP712, SpendCommit} from "../libraries/ZecuredEIP712.sol";
import {ZecuredRegistryStorage} from "./ZecuredRegistryStorage.sol";

/// @title ZecuredSpendLogic
/// @notice The code of `ZecuredRegistry.commitSpend` (docs/TIER3.md § 4), of the dispute path (`IZecuredDisputes`), of
///         the refund lifecycle's later steps (`IZecuredRefunds`) and of the payout confirmations and the compensator's
///         hooks (`IZecuredPayouts`, v4), the last three served by the Registry's fallback, run by the Registry with
///         delegatecall on its own storage: it lives in a separate contract only to keep the Registry under EIP-170
///         (T-11). It shares the Registry's storage layout, EIP-712 domain name and signer set through
///         `ZecuredRegistryStorage`; its domain separator is computed for the Registry's address, since under
///         delegatecall `address(this)` is the Registry. Called directly, it does nothing (`NotDelegated`).
contract ZecuredSpendLogic is
    ZecuredRegistryStorage,
    IZecuredSpendCommitsEvents,
    IZecuredDisputes,
    IZecuredRefunds,
    IZecuredPayouts
{
    /// @notice The only Registry that may run this code (its address, predicted before deployment).
    address public immutable registry;
    /// @inheritdoc IZecuredDisputes
    uint64 public immutable disputeWindow;
    /// @inheritdoc IZecuredDisputes
    uint64 public immutable inclusionWindow;
    /// @inheritdoc IZecuredRefunds
    uint64 public immutable refundDormancy;

    error NotDelegated();
    error InvalidDisputeWindows();
    error InvalidRefundDormancy();

    /// @param registry_ The Registry's (predicted) address.
    /// @param signers_ The Registry's signer set, strictly increasing.
    /// @param disputeWindow_ How long after its confirmation a claim can be disputed (T-05 (a)).
    /// @param inclusionWindow_ How long the signers have to prove a disputed payout (at least the time a payout's block
    ///        takes to gain the slasher's `slashConfirmationWork`, plus proving).
    /// @param refundDormancy_ R-11: how long a Refund claim without a destination counts in `lockedZatoshi` (48 hours
    ///        on the judge deployment, 7 days on mainnet).
    constructor(
        address registry_,
        address[5] memory signers_,
        uint64 disputeWindow_,
        uint64 inclusionWindow_,
        uint64 refundDormancy_
    ) ZecuredRegistryStorage(signers_) {
        if (disputeWindow_ == 0 || inclusionWindow_ == 0) revert InvalidDisputeWindows();
        if (refundDormancy_ == 0) revert InvalidRefundDormancy();
        registry = registry_;
        disputeWindow = disputeWindow_;
        inclusionWindow = inclusionWindow_;
        refundDormancy = refundDormancy_;
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
    ///      note of the vault, which is not pending; 1 to MAX_CLAIMS claims with one ref each; a change tag exactly when
    ///      there is change, not already a lineage note. Returns the input's value.
    function _checkSpendShape(SpendCommit calldata c) internal view returns (uint64) {
        if (c.payoutRef == bytes32(0)) revert ZeroPayoutRef();
        if (_spends[c.payoutRef].vaultId != bytes32(0)) revert PayoutAlreadyCommitted(c.payoutRef);
        LineageNote memory input = _lineage[c.inputTag];
        if (c.vaultId == bytes32(0) || input.vaultId != c.vaultId) revert NotALineageNote(c.vaultId, c.inputTag);
        // K3: nothing is paid from a vault whose activation is pending (its reservation may still be released).
        if (_vaults[c.vaultId].status == VaultStatus.Pending) revert VaultPending(c.vaultId);
        // R-12: nor from a refunded deposit whose claim is not open yet.
        if (_reservedRefund[c.inputTag] != address(0)) revert RefundNotOpen(c.inputTag);
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
    ///      A new commitment of the claim's own input may also name it once confirmed or disputed (D57): its input
    ///      carries the same claims (RT-4), so while that input is unspent no payout of it landed, whatever was
    ///      confirmed (a false confirmation, or one a reorganization undid), and only this payout can pay them.
    ///      R-16: a pending claim bound to an input that is no longer a lineage note of its vault (a proven deposit took
    ///      over that tag from a change record, which could never have been a real note) is bound to this input instead.
    function _commitClaim(SpendCommit calldata c, uint256 i, uint64 fee) internal returns (uint64 amount) {
        bytes32 id = c.claimIds[i];
        uint8 output = c.claimOutputs[i];
        for (uint256 k; k < i; ++k) {
            if (c.claimIds[k] == id) revert DuplicateClaim(id);
            if (c.claimOutputs[k] == output) revert DuplicateClaimOutput(output);
        }
        Claim storage cl = _claims[id];
        ClaimStatus status = cl.status;
        bytes32 bound = _claimInput[id];
        bool again = bound == c.inputTag && (status == ClaimStatus.Paid || status == ClaimStatus.Disputed);
        if (cl.vaultId != c.vaultId || (status != ClaimStatus.Pending && !again)) revert ClaimNotPayable(id);
        // F4 (RT-1): each claim's reference is kept under its recipient commitment, distinct within a payout (the
        // payout reference is new, so an entry can only come from an earlier claim of this commitment).
        bytes32 commitment = cl.commitment;
        if (commitment == bytes32(0)) revert ClaimDestinationUnset(id);
        if (_claimRefOf[c.payoutRef][commitment] != bytes32(0)) revert DuplicateClaimCommitment(commitment);
        if (bound == bytes32(0) || _lineage[bound].vaultId != c.vaultId) _claimInput[id] = c.inputTag;
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

    // ------------------------------------------------------------------ confirmations (R-1, R-3)

    /// @inheritdoc IZecuredPayouts
    function confirmPayout(bytes32 vaultId, bytes32 payoutRef, bytes32[] calldata claimIds, bytes[] calldata sigs)
        external
    {
        _onlyDelegated();
        uint256 n = claimIds.length;
        if (n == 0) revert NoClaims();
        // R-3: the signers' word is not taken while a theft case is open on the vault, nor after one was executed.
        address slasher = _slasher();
        if (slasher != address(0) && IZecuredSlasher(slasher).vaultFrozen(vaultId)) revert VaultFrozen(vaultId);
        bytes32 input = _spends[payoutRef].inputTag;
        if (vaultId == bytes32(0) || _spends[payoutRef].vaultId != vaultId) {
            revert PayoutNotOfVault(vaultId, payoutRef);
        }
        if (_payoutConfirmed[payoutRef]) revert PayoutAlreadyConfirmed(payoutRef);
        _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, payoutRef, claimIds)), sigs, SIGNER_THRESHOLD
        );
        _payoutConfirmed[payoutRef] = true;

        for (uint256 i; i < n; ++i) {
            bytes32 id = claimIds[i];
            Claim storage c = _claims[id];
            ClaimStatus status = c.status;
            if (status == ClaimStatus.None) revert UnknownClaim(id);
            if (c.vaultId != vaultId) revert ClaimVaultMismatch(id, vaultId);
            if (status == ClaimStatus.Paid) revert ClaimAlreadyPaid(id); // also rejects duplicate ids
            if (status != ClaimStatus.Pending) revert ClaimNotPayable(id);
            // R-1: a reopened claim's confirmation was disputed and not upheld: only a proof pays it now.
            if (_reopened[id]) revert ClaimReopened(id);
            // Tier 3: a claim is paid by a committed payout only, which its claimant can check (F3); every commitment
            // of its input pays the same claims (RT-4).
            bytes32 bound = _claimInput[id];
            if (bound == bytes32(0)) revert ClaimNotCommitted(id);
            if (bound != input) revert PayoutNotOfClaim(id, payoutRef);
            // A uint40 of seconds outlasts the chain.
            // forge-lint: disable-next-line(unsafe-typecast)
            c.paidAt = uint40(block.timestamp); // its claimant may dispute this for `disputeWindow`
            _closeClaim(c, ClaimStatus.Paid);
        }
        _unproven[vaultId] += n;
        emit PayoutConfirmed(vaultId, payoutRef, claimIds);
    }

    /// @inheritdoc IZecuredPayouts
    function hashPayoutConfirmation(bytes32 vaultId, bytes32 payoutRef, bytes32[] calldata claimIds)
        external
        view
        returns (bytes32)
    {
        _onlyDelegated(); // the Registry's domain: under delegatecall `address(this)` is the Registry
        return _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, payoutRef, claimIds));
    }

    /// @inheritdoc IZecuredPayouts
    function payoutConfirmed(bytes32 payoutRef) external view returns (bool) {
        return _payoutConfirmed[payoutRef];
    }

    /// @inheritdoc IZecuredPayouts
    function unprovenConfirmations(bytes32 vaultId) external view returns (uint256) {
        return _unproven[vaultId];
    }

    /// @inheritdoc IZecuredPayouts
    function expireConfirmation(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        uint64 paidAt = c.paidAt;
        if (c.status != ClaimStatus.Paid || paidAt == 0) revert NotUnproven(claimId);
        uint64 closesAt = paidAt + disputeWindow;
        if (block.timestamp <= closesAt) revert DisputeWindowOpen(claimId, closesAt);
        bytes32 vaultId = c.vaultId;
        if (_suspect(vaultId, paidAt)) revert ConfirmationSuspect(claimId);
        c.paidAt = 0;
        _unproven[vaultId] -= 1;
        emit ConfirmationExpired(vaultId, claimId);
    }

    // ------------------------------------------------------------------ disputes and proofs (T-05 (a), v4)

    /// @inheritdoc IZecuredDisputes
    function disputePayout(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        uint64 paidAt = c.paidAt;
        // A bond-paid claim's claimant has its USDC; a final claim (proven, or expired) has `paidAt` zero.
        if (c.status != ClaimStatus.Paid || paidAt == 0 || c.bondPaid) revert NotDisputable(claimId);
        address claimant = _claimant(claimId, c);
        if (claimant == address(0)) revert NotDisputable(claimId);
        if (msg.sender != claimant) revert NotClaimant(claimId, msg.sender);
        uint64 closesAt = paidAt + disputeWindow;
        if (block.timestamp > closesAt) revert DisputeWindowClosed(claimId, closesAt);
        bytes32 vaultId = c.vaultId;
        // The compensator has tallied a compromised vault's claims: a claim unpaid again there would never settle.
        if (_compromised(vaultId)) revert DisputeVaultCompromised(vaultId);

        // Unpaid again until resolved: it keeps (or reopens) the vault and counts in `lockedZatoshi`.
        Vault storage v = _vaults[vaultId];
        if (_settled(v)) _openVaults += 1;
        v.unpaidClaims += 1;
        _pendingClaimZat += c.amount;
        _unproven[vaultId] -= 1;
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
        address slasher = _slasher();
        if (msg.sender != slasher || slasher == address(0)) revert OnlySlasher(msg.sender);
        Claim storage c = _claims[claimId];
        ClaimStatus status = c.status;
        if (status == ClaimStatus.Paid) {
            if (c.paidAt == 0) revert AlreadyFinal(claimId);
        } else if (status != ClaimStatus.Pending && status != ClaimStatus.Disputed) {
            revert NotUpholdable(claimId);
        }
        // Every commitment of an input pays the same claims (RT-4), so any committed payout of this claim's input, of
        // its vault, pays it. On a frozen or compromised vault too: the compensator waits for such proofs (R-2) and
        // takes over only the claims nobody proved.
        bytes32 vaultId = c.vaultId;
        bytes32 input = _claimInput[claimId];
        SpendRecord storage s = _spends[payoutRef];
        if (input == bytes32(0) || s.inputTag != input || s.vaultId != vaultId) {
            revert PayoutNotOfClaim(claimId, payoutRef);
        }
        if (status == ClaimStatus.Paid) {
            // Confirmed by signatures, now proven: the dispute window closes.
            c.paidAt = 0;
            _unproven[vaultId] -= 1;
            emit PayoutProven(vaultId, claimId, payoutRef);
            return;
        }
        // D57: a proof that comes after the inclusion window still upholds a reopened claim, also one the bonds paid
        // meanwhile (their reimbursement is then forgone: the claim's ZEC reached its claimant).
        if (status == ClaimStatus.Disputed || _reopened[claimId]) emit PayoutUpheld(vaultId, claimId, payoutRef);
        else emit PayoutProven(vaultId, claimId, payoutRef);
        c.disputeDueAt = 0;
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
        _reopened[claimId] = true;
        emit DisputedClaimReopened(c.vaultId, claimId);
    }

    /// @inheritdoc IZecuredDisputes
    function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt) {
        Claim storage c = _claims[claimId];
        return (c.paidAt, c.disputeDueAt);
    }

    // ------------------------------------------------------------------ refunds (R-8, R-11, R-12)

    /// @inheritdoc IZecuredRefunds
    function openReservedRefund(bytes32 vaultId, bytes32 depositTag) external returns (bytes32 claimId) {
        _onlyDelegated();
        _onlyIntake();
        address account = _reservedRefund[depositTag];
        uint64 amount = _lineage[depositTag].value;
        if (account == address(0) || _lineage[depositTag].vaultId != vaultId) {
            revert NoReservedRefund(vaultId, depositTag);
        }
        _refuseCompromised(vaultId);
        Vault storage v = _vaults[vaultId];
        VaultStatus status = v.status;
        // R-8: no claim on a vault whose activation is pending (none could be paid there, yet it would accrue
        // liveness): the refund waits until the vault activates, converts or drops.
        if (status == VaultStatus.Pending) revert VaultPending(vaultId);
        if (status == VaultStatus.None) {
            v.account = account;
            v.status = VaultStatus.Cancelled;
        } else if (v.account != account) {
            revert IZecuredRegistry.ReservationMismatch(vaultId);
        }
        delete _reservedRefund[depositTag];
        claimId = _newClaim(vaultId, v, ClaimKind.Refund, amount, bytes32(0));
        emit IZecuredRegistry.RefundOpened(vaultId, claimId, account, amount, depositTag);
    }

    /// @inheritdoc IZecuredRefunds
    function releaseRefund(bytes32 vaultId, bytes32 depositTag) external {
        _onlyDelegated();
        _onlyIntake();
        if (_reservedRefund[depositTag] == address(0) || _lineage[depositTag].vaultId != vaultId) {
            revert NoReservedRefund(vaultId, depositTag);
        }
        delete _reservedRefund[depositTag];
        delete _lineage[depositTag];
        _depositTagUsed[depositTag] = false;
    }

    /// @inheritdoc IZecuredRefunds
    function setRefundDestination(bytes32 claimId, bytes32 destCommitment) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        if (c.kind != ClaimKind.Refund || c.status == ClaimStatus.None) revert IZecuredRegistry.NotARefund(claimId);
        bytes32 vaultId = c.vaultId;
        Vault storage v = _vaults[vaultId];
        if (msg.sender != v.account) revert IZecuredRegistry.NotVaultAccount(vaultId, msg.sender);
        if (destCommitment == bytes32(0)) revert IZecuredRegistry.ZeroCommitment();
        // Bound to a payout, or paid by the bonds (its ZEC now reimburses them, T-05): no longer the account's to move.
        if (_claimInput[claimId] != bytes32(0) || c.bondPaid) revert IZecuredRegistry.RefundBound(claimId);
        ClaimStatus status = c.status;
        if (status == ClaimStatus.WrittenOff) {
            // Revives an abandoned refund (its note is still a lineage note of the vault); not on a compromised vault,
            // whose claims the compensator has tallied.
            _refuseCompromised(vaultId);
            if (_settled(v)) _openVaults += 1;
            c.status = ClaimStatus.Pending;
            v.unpaidClaims += 1;
            _pendingClaimZat += c.amount;
        } else if (status != ClaimStatus.Pending) {
            revert IZecuredRegistry.RefundBound(claimId);
        } else {
            // R-11: a dormant refund counts in `lockedZatoshi` again.
            uint256 slot = _slotOf(c);
            if (_dormant[slot]) {
                delete _dormant[slot];
                _pendingClaimZat += c.amount;
            }
        }
        c.commitment = destCommitment;
        c.createdAt = uint40(block.timestamp);
        emit IZecuredRegistry.RefundDestinationSet(vaultId, claimId, destCommitment);
    }

    /// @inheritdoc IZecuredRefunds
    function abandonRefund(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        if (c.kind != ClaimKind.Refund || c.status != ClaimStatus.Pending || c.commitment != bytes32(0) || c.bondPaid) {
            revert IZecuredRegistry.NotARefund(claimId);
        }
        uint64 at = uint64(c.createdAt) + IZecuredRegistry(address(this)).refundAbandonPeriod();
        if (block.timestamp < at) revert IZecuredRegistry.NotAbandonable(claimId, at);
        _refuseCompromised(c.vaultId); // the compensator settles a compromised vault's claims
        emit IZecuredRegistry.RefundAbandoned(c.vaultId, claimId);
        _closeClaim(c, ClaimStatus.WrittenOff);
    }

    /// @inheritdoc IZecuredRefunds
    function markRefundDormant(bytes32 claimId) external {
        _onlyDelegated();
        Claim storage c = _claims[claimId];
        uint256 slot = _slotOf(c);
        if (
            c.kind != ClaimKind.Refund || c.status != ClaimStatus.Pending || c.commitment != bytes32(0) || c.bondPaid
                || _dormant[slot]
        ) revert NotDormantable(claimId);
        uint64 at = uint64(c.createdAt) + refundDormancy;
        if (block.timestamp < at) revert NotDormantYet(claimId, at);
        _dormant[slot] = true;
        _pendingClaimZat -= c.amount;
        emit IZecuredRegistry.RefundDormant(c.vaultId, claimId);
    }

    /// @inheritdoc IZecuredRefunds
    function isRefundDormant(bytes32 claimId) external view returns (bool) {
        return _dormant[_slotOf(_claims[claimId])];
    }

    /// @inheritdoc IZecuredRefunds
    function reservedRefundOf(bytes32 depositTag) external view returns (address) {
        return _reservedRefund[depositTag];
    }

    // ------------------------------------------------------------------ hooks: the compensator (T-04, v4)

    /// @inheritdoc IZecuredPayouts
    function settleCompensatedClaim(bytes32 claimId) external {
        _onlyDelegated();
        _onlyCompensator();
        Claim storage c = _claims[claimId];
        bytes32 vaultId = c.vaultId;
        if (!_compromised(vaultId)) revert NotCompensable(claimId);
        ClaimStatus status = c.status;
        if (status == ClaimStatus.Pending) {
            _closeClaim(c, ClaimStatus.Compensated);
        } else if (status == ClaimStatus.Paid && c.paidAt != 0 && _suspect(vaultId, c.paidAt)) {
            // R-3: confirmed shortly before the freeze and never proven: counted unpaid (already off the books).
            c.paidAt = 0;
            c.status = ClaimStatus.Compensated;
            _unproven[vaultId] -= 1;
        } else if (status == ClaimStatus.WrittenOff && c.kind == ClaimKind.Refund) {
            // R-21: an abandoned refund (off the books, its note taken with the vault's lineage).
            c.status = ClaimStatus.Compensated;
        } else {
            revert NotCompensable(claimId);
        }
        emit ClaimCompensated(vaultId, claimId);
    }

    /// @inheritdoc IZecuredPayouts
    function closeMisdirectedClaim(bytes32 claimId) external {
        _onlyDelegated();
        _onlyCompensator();
        Claim storage c = _claims[claimId];
        ClaimStatus status = c.status;
        if (status == ClaimStatus.Pending || status == ClaimStatus.Disputed) {
            c.disputeDueAt = 0;
            _closeClaim(c, ClaimStatus.Compensated);
        } else if (status == ClaimStatus.Paid && c.paidAt != 0) {
            c.paidAt = 0;
            c.status = ClaimStatus.Compensated;
            _unproven[c.vaultId] -= 1;
        } else {
            return; // final, compensated already, or never paid by a payout: nothing more is owed on Zcash
        }
        emit ClaimCompensated(c.vaultId, claimId);
    }

    /// @inheritdoc IZecuredPayouts
    function markVaultCompensated(bytes32 vaultId) external {
        _onlyDelegated();
        _onlyCompensator();
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active || _compensatedVault[vaultId] || !_compromised(vaultId)) {
            revert NotCompensableVault(vaultId);
        }
        _compensatedVault[vaultId] = true;
        uint64 outstanding = v.outstanding;
        _compensatedZat += outstanding;
        // Active, so counted as open until now; its claims were all tallied, and a compromised vault takes no new one.
        _settle();
        emit VaultCompensated(vaultId, outstanding);
    }

    /// @inheritdoc IZecuredPayouts
    function vaultCompensated(bytes32 vaultId) external view returns (bool) {
        return _compensatedVault[vaultId];
    }

    // ------------------------------------------------------------------ internal

    function _onlyDelegated() internal view {
        if (address(this) != registry) revert NotDelegated();
    }

    /// @dev The Registry's Intake only (read through the Registry: under delegatecall `address(this)` is the Registry).
    function _onlyIntake() internal view {
        if (msg.sender != IZecuredRegistry(address(this)).intake()) revert IZecuredRegistry.OnlyIntake(msg.sender);
    }

    /// @dev As `ZecuredRegistry._checkNotCompromised`: the compensator settles a compromised vault's claims.
    function _refuseCompromised(bytes32 vaultId) internal view {
        address slasher = IZecuredRegistry(address(this)).slasher();
        if (slasher != address(0) && IZecuredSlasher(slasher).isCompromised(vaultId)) {
            revert IZecuredRegistry.VaultCompromised(vaultId);
        }
    }

    function _onlyCompensator() internal view {
        address compensator = IZecuredRegistry(address(this)).compensator();
        if (msg.sender != compensator || compensator == address(0)) revert OnlyCompensator(msg.sender);
    }

    /// @dev The Registry's slasher (zero on deployments without bonds).
    function _slasher() internal view returns (address) {
        return IZecuredRegistry(address(this)).slasher();
    }

    function _compromised(bytes32 vaultId) internal view returns (bool) {
        address slasher = _slasher();
        return slasher != address(0) && IZecuredSlasher(slasher).isCompromised(vaultId);
    }

    /// @dev R-3: a confirmation by signatures made within `disputeWindow` before its vault froze (an F1/F2 case open,
    ///      or executed). No confirmation is accepted while the vault is frozen, so every confirmation precedes the freeze.
    function _suspect(bytes32 vaultId, uint64 paidAt) internal view returns (bool) {
        address slasher = _slasher();
        if (slasher == address(0)) return false;
        uint64 frozenSince = IZecuredSlasher(slasher).frozenSince(vaultId);
        return frozenSince != 0 && paidAt + disputeWindow > frozenSince;
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
