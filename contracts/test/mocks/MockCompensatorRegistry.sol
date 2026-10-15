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
}

/// @title MockCompensatorRegistry
/// @notice MOCK, tests only. The part of ZecuredRegistry v3 the compensator reads and calls (design notes T-04), with
///         the rules Registry v3 applies: `settleCompensatedClaim` marks a pending claim of a compromised vault
///         Compensated (the vault no longer counts as open once it is closed or cancelled and none is left);
///         `retireVault` needs a compromised active vault without Morpho debt whose Morpho collateral equals its
///         outstanding, withdraws that collateral with the account's authorization (Morpho reverts without it), records
///         a Retired claim (status Compensated, never pending) and closes the vault. Vaults and claims are set by the
///         test (`setClaimStatus` also allows what the Registry refuses on a compromised vault, such as reviving a
///         refund); no zZEC, no signatures.
contract MockCompensatorRegistry {
    MockMorphoForCompensator public immutable morphoMock;
    MarketParams internal _params;

    address public compensator;
    address public slasher;
    address public signerBond;

    mapping(bytes32 vaultId => VaultDetails) internal _vaults;
    mapping(bytes32 claimId => ClaimDetails) internal _claims;
    bytes32[] internal _vaultIds;

    event VaultRetired(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 retired);
    event ClaimCompensated(bytes32 indexed vaultId, bytes32 indexed claimId);

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
    ///         `unpaidClaims` in step.
    function setClaimStatus(bytes32 claimId, ClaimStatus status) external {
        ClaimDetails storage c = _claims[claimId];
        VaultDetails storage v = _vaults[c.vaultId];
        if (c.status == ClaimStatus.Pending) v.unpaidClaims -= 1;
        if (status == ClaimStatus.Pending) v.unpaidClaims += 1;
        c.status = status;
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

    /// @notice IZecuredSpendCommits: pending or active vaults, and closed or cancelled ones with a claim still pending.
    function openVaultCount() external view returns (uint256 open) {
        for (uint256 i; i < _vaultIds.length; ++i) {
            VaultDetails storage v = _vaults[_vaultIds[i]];
            if (v.status == VaultStatus.Active || v.status == VaultStatus.Pending || v.unpaidClaims != 0) open += 1;
        }
    }

    function settleCompensatedClaim(bytes32 claimId) external {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        ClaimDetails storage c = _claims[claimId];
        require(c.status == ClaimStatus.Pending, "MockRegistry: not pending");
        require(ICompromised(slasher).isCompromised(c.vaultId), "MockRegistry: not compromised");
        c.status = ClaimStatus.Compensated;
        _vaults[c.vaultId].unpaidClaims -= 1;
        emit ClaimCompensated(c.vaultId, claimId);
    }

    function retireVault(bytes32 vaultId) external returns (uint64 retired) {
        require(msg.sender == compensator, "MockRegistry: only compensator");
        VaultDetails storage v = _vaults[vaultId];
        require(v.status == VaultStatus.Active, "MockRegistry: not active");
        require(ICompromised(slasher).isCompromised(vaultId), "MockRegistry: not compromised");
        require(morphoMock.borrowSharesOf(v.account) == 0, "MockRegistry: debt outstanding");
        require(morphoMock.collateralOf(v.account) == v.outstanding, "MockRegistry: collateral mismatch");
        retired = v.outstanding;
        v.outstanding = 0;
        v.status = VaultStatus.Closed;
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
