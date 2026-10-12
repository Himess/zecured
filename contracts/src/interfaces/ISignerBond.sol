// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title ISignerBond
/// @notice Tier 3 consortium bond (docs/TIER3.md § 7; review S5, S6). Each of the Registry's five signers deposits
///         `BOND` in Base ETH. A proven violation slashes the bonds of the signers it names (all five for an
///         uncommitted spend, since FROST hides which three signed; the commitment's signers for a false record), pays
///         the prover a bounty and keeps the rest in a compensation pool (nothing is burned). The first slash halts
///         minting for good. Testnet bonds show the mechanism, not economic security. Interface locked 2026-10-12.
/// @dev Rules:
///      - `deposit()`: the sender is one of the Registry's signers, has no bond yet, sends exactly `BOND`; not after a
///        halt.
///      - `slash(caseId, signerBitmap, beneficiary)`: the slasher only. Takes the whole bond of every signer in the
///        bitmap; bounty = min(slashed * BOUNTY_BPS / 10,000, BOUNTY_CAP), credited to `beneficiary` (pulled with
///        `claimBounty`, so a reverting beneficiary cannot block a slash); the rest goes to the pool; sets `halted`.
///      - `withdraw(to)`: a signer takes its whole bond back only if not halted, no violation case is open, the
///        Registry has no open vault (`openVaultCount() == 0`) and `challengeWindow` has passed since
///        `lastVaultSettledAt()`: the same five signers hold every vault, so "its vaults" are all vaults.
///      - The compensation pool has no exit in this version (paying victims from it is roadmap).
interface ISignerBond {
    event BondDeposited(uint8 indexed signer, address indexed signerAddress, uint256 amount);
    event BondSlashed(bytes32 indexed caseId, uint8 indexed signer, uint256 amount);
    event BountyCredited(bytes32 indexed caseId, address indexed beneficiary, uint256 amount);
    event BountyClaimed(address indexed beneficiary, uint256 amount);
    event Halted(bytes32 indexed caseId);
    event BondWithdrawn(uint8 indexed signer, address indexed to, uint256 amount);

    error NotASigner(address caller);
    error NotTheSlasher(address caller);
    error WrongBondAmount(uint256 sent, uint256 required);
    error AlreadyBonded(uint8 signer);
    error NoBond(uint8 signer);
    error BondsHalted();
    error WithdrawalLocked(uint256 openVaults, uint256 openCases, uint64 unlocksAt);
    error NothingToClaim(address beneficiary);
    error TransferFailed(address to, uint256 amount);

    /// @notice Bond per signer (0.01 ether on the testnet deployments).
    function BOND() external view returns (uint256);
    /// @notice Bounty share of a slash in basis points (1,000 = 10 %).
    function BOUNTY_BPS() external view returns (uint256);
    /// @notice Most one slash pays its prover (below one bond, so a false slash pays little).
    function BOUNTY_CAP() external view returns (uint256);

    function registry() external view returns (address);
    function slasher() external view returns (address);
    /// @notice Same window as the slasher's (24 hours on the judge deployment, 2 minutes on the theft demo).
    function challengeWindow() external view returns (uint64);

    function bondOf(uint8 signer) external view returns (uint256);
    function halted() external view returns (bool);
    function compensationPool() external view returns (uint256);
    function bountyOf(address beneficiary) external view returns (uint256);

    function deposit() external payable;
    function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary) external returns (uint256 slashed);
    function claimBounty() external;
    function withdraw(address payable to) external;
}
