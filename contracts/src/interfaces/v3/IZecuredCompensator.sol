// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ViolationKind} from "./IZecuredSlasher.sol";

/// @notice One incident: every theft case executed within `incidentWindow` of the first one, paid at one price and one
///         set of ratios (design notes T-04).
/// @param price P*, the market oracle's price (Morpho scale) at finalization.
/// @param layer2 USDC owed to third-party claimants (Liquidation, CloseMarketMaker): their pending zatoshi times P*.
/// @param layer3 USDC owed to lenders: the authorized victim vaults' Morpho debt at tally time plus a 1 % interest
///        allowance (`CompensationMath.INTEREST_ALLOWANCE_BPS`: the debt keeps accruing until the vault is settled).
/// @param layer4 USDC owed to the owners: their own pending claims (Release, CloseUser, Refund) times P*, plus each
///        authorized victim vault's equity, max(0, outstanding * P* - debt with the allowance).
/// @param r2, r3, r4 Payout ratios (1e18 = in full), filled in layer order from the funds: a layer is paid only once
///        every earlier layer is paid in full.
struct Incident {
    uint64 openedAt;
    uint64 closesAt;
    bool finalized;
    uint32 victims;
    uint32 talliedVictims;
    uint256 price;
    uint256 layer2;
    uint256 layer3;
    uint256 layer4;
    uint256 r2;
    uint256 r3;
    uint256 r4;
}

/// @notice A compromised vault (F1 or F2) in an incident.
/// @param pendingClaims Its claims tallied with an entitlement. `tallyClaim` settles every pending claim of the vault
///        with the Registry (Fee and bond-paid ones too, without an entitlement); `talliedClaims` counts them all, and
///        `tallyVault` needs the Registry's `unpaidClaims` at zero.
/// @param authorized The account still lets the Registry withdraw its collateral on Morpho (else its debt cannot be
///        repaid safely: layers 3 and 4 skip it).
struct Victim {
    uint256 incident;
    bool tallied;
    bool settled;
    bool authorized;
    uint64 pendingClaims;
    uint64 talliedClaims;
    uint64 outstanding;
    uint256 debt;
}

/// @notice What one claim gets: its layer (2 or 4) and zatoshi; paid at the incident's price and its layer's ratio.
struct Entitlement {
    uint256 incident;
    uint8 layer;
    uint64 zat;
    address to;
    bool paid;
}

/// @title IZecuredCompensator
/// @notice Pays the victims of a proven theft from the slashed bonds (design notes T-04). Layers, in order: (1) the
///         prover's bounty (in the bond contract), (2) third-party claimants, (3) the victim vaults' lenders, by
///         repaying their Morpho debt, after which the vault's unbacked zZEC is burned, (4) the owners: their own
///         claims and their equity. Within a layer, pro rata. Everything is pulled, one vault or one claim per call
///         (EIP-7825). No owner, no upgrade path.
/// @dev Flow: `onFunds` and `onCase` (bond and slasher only) -> `tallyClaim` for each pending claim of a victim vault,
///      then `tallyVault` (anyone; the vault must not be Pending: convert it with the Intake's `refundPending` first)
///      -> after `closesAt`, `finalizeIncident` (anyone) -> `settleVault` per vault (repays, retires, fixes the
///      vault's equity) -> `withdrawEntitlement` / `withdrawEquity` (anyone; paid to the claimant or the account).
///      Settling a vault: debt now `D`; the vault's share is `debt * r3 + equity * r4`; if it covers `D`, the debt is
///      repaid in full, the vault retired, and the rest is the account's equity; otherwise the share is repaid and the
///      collateral withdrawn and burned down to the LLTV, so that no more can be borrowed against it. Borrowing after
///      the tally only moves value from the owner's equity to the lenders. The freeze does not stop anyone repaying the
///      debt directly on Morpho, which only lowers what the vault needs.
///      Leftover: kept for later incidents while any vault is open; then `sweepLeftover` sends it to `leftoverSink`
///      (a burn address on testnet; a public-good address is a mainnet option). It never goes back to the signers.
interface IZecuredCompensator {
    event FundsReceived(bytes32 indexed caseId, uint256 amount);
    event IncidentOpened(uint256 indexed incident, uint64 closesAt);
    event VictimRegistered(uint256 indexed incident, bytes32 indexed vaultId, bytes32 indexed caseId);
    event ShortfallRegistered(uint256 indexed incident, bytes32 indexed claimId, uint64 zat);
    event ClaimTallied(bytes32 indexed claimId, uint8 layer, uint64 zat);
    event VaultTallied(bytes32 indexed vaultId, uint64 outstanding, uint256 debt, bool authorized);
    event IncidentFinalized(
        uint256 indexed incident,
        uint256 price,
        uint256 layer2,
        uint256 layer3,
        uint256 layer4,
        uint256 r2,
        uint256 r3,
        uint256 r4
    );
    event VaultSettled(bytes32 indexed vaultId, uint256 repaid, uint64 retired, uint256 equity);
    event EntitlementPaid(bytes32 indexed id, address indexed to, uint256 amount);
    event LeftoverSwept(address indexed sink, uint256 amount);

    error ZeroAddress();
    error InvalidConfig();
    error OnlyBond(address caller);
    error OnlySlasher(address caller);
    error NotVictim(bytes32 vaultId);
    error NotVictimClaim(bytes32 claimId);
    error AlreadyTallied(bytes32 id);
    error VaultStillPending(bytes32 vaultId);
    error ClaimsNotTallied(bytes32 vaultId, uint64 left);
    error IncidentStillOpen(uint256 incident, uint64 closesAt);
    error VictimsNotTallied(uint256 incident, uint32 left);
    error AlreadyFinalized(uint256 incident);
    error NotFinalized(uint256 incident);
    error AlreadySettled(bytes32 vaultId);
    error NotSettled(bytes32 vaultId);
    error NothingOwed(bytes32 id);
    error LeftoverLocked();

    function onFunds(bytes32 caseId, uint256 amount) external;
    function onCase(bytes32 caseId, ViolationKind kind, bytes32 vaultId, bytes32 claimId, uint64 shortfallZat) external;

    function tallyClaim(bytes32 claimId) external;
    function tallyVault(bytes32 vaultId) external;
    function finalizeIncident(uint256 incident) external;
    function settleVault(bytes32 vaultId) external;
    function withdrawEntitlement(bytes32 claimId) external returns (uint256 amount);
    function withdrawEquity(bytes32 vaultId) external returns (uint256 amount);
    function sweepLeftover() external returns (uint256 amount);

    function incident(uint256 id) external view returns (Incident memory);
    function victim(bytes32 vaultId) external view returns (Victim memory);
    function entitlement(bytes32 claimId) external view returns (Entitlement memory);
    function equityOf(bytes32 vaultId) external view returns (uint256);
    function currentIncident() external view returns (uint256);
    /// @notice USDC not yet set aside for any incident.
    function funds() external view returns (uint256);

    function registry() external view returns (address);
    function slasher() external view returns (address);
    function signerBond() external view returns (address);
    function usdc() external view returns (IERC20);
    function incidentWindow() external view returns (uint64);
    function leftoverSink() external view returns (address);
}
