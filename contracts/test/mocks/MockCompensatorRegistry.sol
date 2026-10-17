// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MarketParams} from "morpho-blue/src/interfaces/IMorpho.sol";
import {
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails,
    VaultDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {MockMorphoForCompensator} from "./MockMorphoForCompensator.sol";

interface ICompromised {
    function isCompromised(bytes32 vaultId) external view returns (bool);
    function frozenSince(bytes32 vaultId) external view returns (uint64);
}

/// @title MockCompensatorRegistry
/// @notice MOCK, tests only. The part of ZecuredRegistry v3 the compensator reads and calls (design notes T-04), with
///         the rules Registry v3 applies: `settleCompensatedClaim` marks a pending claim of a compromised vault
///         Compensated (the vault no longer counts as open once it is closed or cancelled and none is left);
///         `retireVault(vaultId, amount)` needs a compromised active vault whose Morpho collateral equals its
///         outstanding, withdraws `amount` of it with the account's authorization (Morpho reverts without it, and on an
///         unhealthy position), records a Retired claim (status Compensated, never pending), and closes the vault when
///         nothing is left (which needs the debt repaid). Vaults and claims are set by the
///         test (`setClaimStatus` also allows what the Registry refuses on a compromised vault, such as reviving a
///         refund); no zZEC, no signatures. v4: the claim hooks of `IZecuredPayouts` with the spend logic's rules
///         (R-3 confirmations shortly before the freeze, R-7 F3 closure, R-20 settled active vaults, R-21 abandoned
///         refunds), and the views the compensator reads (`claimInput`, `payoutDispute`, the dispute windows,
///         `unprovenConfirmations`).
contract MockCompensatorRegistry {
    MockMorphoForCompensator public immutable morphoMock;
    MarketParams internal _params;

    address public compensator;
    address public slasher;
    address public signerBond;

    mapping(bytes32 vaultId => VaultDetails) internal _vaults;
    mapping(bytes32 claimId => ClaimDetails) internal _claims;
    bytes32[] internal _vaultIds;
    mapping(bytes32 claimId => bytes32) public claimInput;
    mapping(bytes32 claimId => uint64) internal _paidAt;
    mapping(bytes32 vaultId => bool) public vaultCompensated;

    uint64 public constant disputeWindow = 1 days;
    uint64 public constant inclusionWindow = 6 hours;

    event VaultRetired(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 retired);
    event ClaimCompensated(bytes32 indexed vaultId, bytes32 indexed claimId);
    event VaultCompensated(bytes32 indexed vaultId, uint64 outstanding);

    constructor(MockMorphoForCompensator morpho_, MarketParams memory params) {
        morphoMock = morpho_;
        _params = params;
    }

    // ------------------------------------------------------------------ test controls

    function setWiring(address compensator_, address slasher_, address signerBond_) external {
        compensator = compensator_;
        slasher = slasher_;
        signerBond = signerBond_;
    }

    function setVault(bytes32 vaultId, address account, uint64 outstanding, VaultStatus status) external {
        if (_vaults[vaultId].status == VaultStatus.None && _vaults[vaultId].account == address(0)) {
            _vaultIds.push(vaultId);
        }
        VaultDetails storage v = _vaults[vaultId];
        v.account = account;
        v.outstanding = outstanding;
        v.status = status;
    }

    function addClaim(bytes32 vaultId, ClaimKind kind, uint64 amount, address claimant, bool bondPaid)
        external
        returns (bytes32 claimId)
    {
        claimId = _newClaim(vaultId, kind, ClaimStatus.Pending, amount, claimant);
        _claims[claimId].bondPaid = bondPaid;
    }

    /// @notice Moves a claim to another status (a confirmed payout, a write-off, a revived refund), keeping
    ///         `unpaidClaims` in step. A claim made `Paid` here is final (`paidAt` zero): see `confirmClaim`.
    function setClaimStatus(bytes32 claimId, ClaimStatus status) external {
        ClaimDetails storage c = _claims[claimId];
        VaultDetails storage v = _vaults[c.vaultId];
        if (c.status == ClaimStatus.Pending) v.unpaidClaims -= 1;
        if (status == ClaimStatus.Pending) v.unpaidClaims += 1;
        c.status = status;
        _paidAt[claimId] = 0;
    }

    /// @notice A pending claim, committed from `input`, confirmed by three signers now (Paid, `paidAt = now`).
    function confirmClaim(bytes32 claimId, bytes32 input) external {
        ClaimDetails storage c = _claims[claimId];
        require(c.status == ClaimStatus.Pending, "MockRegistry: not pending");
        _vaults[c.vaultId].unpaidClaims -= 1;
        c.status = ClaimStatus.Paid;
        claimInput[claimId] = input;
        _paidAt[claimId] = uint64(block.timestamp);
    }

    /// @notice Binds a claim to a lineage note (a committed payout of it).
    function setClaimInput(bytes32 claimId, bytes32 input) external {
        claimInput[claimId] = input;
    }

    // ------------------------------------------------------------------ the IZecuredRegistry subset

    function morpho() external view returns (address) {
        return address(morphoMock);
    }

    function marketParams() external view returns (MarketParams memory) {
        return _params;
    }

    function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status) {
        VaultDetails storage v = _vaults[vaultId];
        return (v.account, v.outstanding, v.status);
    }

    function vaultDetails(bytes32 vaultId) external view returns (VaultDetails memory) {
        return _vaults[vaultId];
    }

    function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory) {
        return _claims[claimId];
    }

    function claimIdOf(bytes32 vaultId, uint64 sequence) public pure returns (bytes32) {
        return keccak256(abi.encode(vaultId, sequence));
    }

    /// @notice IZecuredSpendCommits: pending or active vaults, and closed or cancelled ones with a claim still pending;
    ///         R-20: not the active vaults the compensator settled.
    function openVaultCount() external view returns (uint256 open) {
        for (uint256 i; i < _vaultIds.length; ++i) {
            VaultDetails storage v = _vaults[_vaultIds[i]];
            bool active = v.status == VaultStatus.Active && !vaultCompensated[_vaultIds[i]];
            if (active || v.status == VaultStatus.Pending || v.unpaidClaims != 0) open += 1;
        }
    }

    function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt) {
        return (_paidAt[claimId], 0);
    }

    function unprovenConfirmations(bytes32 vaultId) external view returns (uint256 n) {
        VaultDetails storage v = _vaults[vaultId];
        for (uint64 s; s < v.claimCount; ++s) {
            if (_paidAt[claimIdOf(vaultId, s)] != 0) n += 1;
        }
    }

    function settleCompensatedClaim(bytes32 claimId) external {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        ClaimDetails storage c = _claims[claimId];
        require(ICompromised(slasher).isCompromised(c.vaultId), "MockRegistry: not compromised");
        if (c.status == ClaimStatus.Pending) {
            _vaults[c.vaultId].unpaidClaims -= 1;
        } else if (c.status == ClaimStatus.Paid && _paidAt[claimId] != 0) {
            // R-3: confirmed within the dispute window before the freeze.
            require(
                _paidAt[claimId] + disputeWindow > ICompromised(slasher).frozenSince(c.vaultId),
                "MockRegistry: confirmation not suspect"
            );
            _paidAt[claimId] = 0;
        } else {
            require(c.status == ClaimStatus.WrittenOff && c.kind == ClaimKind.Refund, "MockRegistry: not compensable");
        }
        c.status = ClaimStatus.Compensated;
        emit ClaimCompensated(c.vaultId, claimId);
    }

    /// @notice R-7: an executed F3 case closes its claim.
    function closeMisdirectedClaim(bytes32 claimId) external {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        ClaimDetails storage c = _claims[claimId];
        if (c.status == ClaimStatus.Pending || c.status == ClaimStatus.Disputed) {
            _vaults[c.vaultId].unpaidClaims -= 1;
        } else if (c.status == ClaimStatus.Paid && _paidAt[claimId] != 0) {
            _paidAt[claimId] = 0;
        } else {
            return;
        }
        c.status = ClaimStatus.Compensated;
        emit ClaimCompensated(c.vaultId, claimId);
    }

    /// @notice R-20: a settled compromised vault left active no longer counts as open.
    function markVaultCompensated(bytes32 vaultId) external {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        VaultDetails storage v = _vaults[vaultId];
        require(v.status == VaultStatus.Active && !vaultCompensated[vaultId], "MockRegistry: not compensable vault");
        require(ICompromised(slasher).isCompromised(vaultId), "MockRegistry: not compromised");
        vaultCompensated[vaultId] = true;
        emit VaultCompensated(vaultId, v.outstanding);
    }

    function retireVault(bytes32 vaultId, uint64 amount) external returns (uint64 retired) {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        VaultDetails storage v = _vaults[vaultId];
        require(v.status == VaultStatus.Active, "MockRegistry: not active");
        require(ICompromised(slasher).isCompromised(vaultId), "MockRegistry: not compromised");
        require(amount != 0 && amount <= v.outstanding, "MockRegistry: amount");
        require(morphoMock.collateralOf(v.account) == v.outstanding, "MockRegistry: collateral mismatch");
        retired = amount;
        v.outstanding -= amount;
        if (v.outstanding == 0) {
            require(morphoMock.borrowSharesOf(v.account) == 0, "MockRegistry: debt outstanding");
            v.status = VaultStatus.Closed;
        }
        bytes32 claimId = _newClaim(vaultId, ClaimKind.Retired, ClaimStatus.Compensated, retired, address(0));
        morphoMock.withdrawCollateral(_params, retired, v.account, address(this));
        emit VaultRetired(vaultId, claimId, retired);
    }

    // ------------------------------------------------------------------ internal

    function _newClaim(bytes32 vaultId, ClaimKind kind, ClaimStatus status, uint64 amount, address claimant)
        internal
        returns (bytes32 claimId)
    {
        VaultDetails storage v = _vaults[vaultId];
        claimId = claimIdOf(vaultId, v.claimCount);
        v.claimCount += 1;
        if (status == ClaimStatus.Pending) v.unpaidClaims += 1;
        ClaimDetails storage c = _claims[claimId];
        c.vaultId = vaultId;
        c.kind = kind;
        c.status = status;
        c.amount = amount;
        c.commitment = keccak256(abi.encode("commitment", claimId));
        c.createdAt = uint40(block.timestamp);
        c.claimant = claimant;
    }
}
