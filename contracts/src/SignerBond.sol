// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ISignerBond} from "./interfaces/ISignerBond.sol";
import {IZecuredRegistry} from "./interfaces/IZecuredRegistry.sol";
import {IZecuredSpendCommits} from "./interfaces/IZecuredSpendCommits.sol";
import {IZecuredSlasher} from "./interfaces/IZecuredSlasher.sol";

/// @title SignerBond
/// @notice Tier 3 consortium bond of the Registry's five signers (docs/TIER3.md § 7, rules in `ISignerBond`). It has no
///         owner and no upgrade path: only the slasher can take bonds, only after a proven violation and its challenge
///         window, and a signer can only take its own bond back once nothing is at stake.
contract SignerBond is ISignerBond {
    /// @inheritdoc ISignerBond
    uint256 public constant BOND = 0.01 ether;
    /// @inheritdoc ISignerBond
    uint256 public constant BOUNTY_BPS = 1_000;
    /// @inheritdoc ISignerBond
    uint256 public constant BOUNTY_CAP = 0.005 ether;
    uint256 private constant BPS = 10_000;
    uint256 private constant SIGNER_COUNT = 5;

    /// @inheritdoc ISignerBond
    address public immutable registry;
    /// @inheritdoc ISignerBond
    address public immutable slasher;
    /// @inheritdoc ISignerBond
    uint64 public immutable challengeWindow;

    /// @inheritdoc ISignerBond
    mapping(uint8 signer => uint256) public bondOf;
    /// @inheritdoc ISignerBond
    bool public halted;
    /// @inheritdoc ISignerBond
    uint256 public compensationPool;
    /// @inheritdoc ISignerBond
    mapping(address beneficiary => uint256) public bountyOf;

    /// @param registry_ The Registry, whose signer set and open vaults the bond follows (it reads this contract's
    ///        `halted` and `fullyBonded` before minting).
    /// @param slasher_ The ZecuredSlasher (its predicted address): the only caller of `slash`.
    /// @param challengeWindow_ The slasher's challenge window, also the wait after the last vault settled.
    constructor(address registry_, address slasher_, uint64 challengeWindow_) {
        if (registry_ == address(0) || slasher_ == address(0)) revert ZeroAddress();
        if (challengeWindow_ == 0) revert ZeroWindow();
        registry = registry_;
        slasher = slasher_;
        challengeWindow = challengeWindow_;
    }

    /// @inheritdoc ISignerBond
    function fullyBonded() external view returns (bool) {
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if (bondOf[i] == 0) return false;
        }
        return true;
    }

    /// @inheritdoc ISignerBond
    function deposit() external payable {
        if (halted) revert BondsHalted();
        uint8 signer = _signerIndex(msg.sender);
        if (msg.value != BOND) revert WrongBondAmount(msg.value, BOND);
        if (bondOf[signer] != 0) revert AlreadyBonded(signer);
        bondOf[signer] = BOND;
        emit BondDeposited(signer, msg.sender, BOND);
    }

    /// @inheritdoc ISignerBond
    function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary) external returns (uint256 slashed) {
        if (msg.sender != slasher) revert NotTheSlasher(msg.sender);
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if ((signerBitmap >> i) & 1 == 0) continue;
            uint256 amount = bondOf[i];
            if (amount == 0) continue;
            bondOf[i] = 0;
            slashed += amount;
            emit BondSlashed(caseId, i, amount);
        }
        uint256 bounty = slashed * BOUNTY_BPS / BPS;
        if (bounty > BOUNTY_CAP) bounty = BOUNTY_CAP;
        if (bounty != 0) {
            bountyOf[beneficiary] += bounty;
            emit BountyCredited(caseId, beneficiary, bounty);
        }
        compensationPool += slashed - bounty;
        if (!halted) {
            halted = true;
            emit Halted(caseId);
        }
    }

    /// @inheritdoc ISignerBond
    function claimBounty() external {
        uint256 amount = bountyOf[msg.sender];
        if (amount == 0) revert NothingToClaim(msg.sender);
        bountyOf[msg.sender] = 0;
        emit BountyClaimed(msg.sender, amount);
        _send(payable(msg.sender), amount);
    }

    /// @inheritdoc ISignerBond
    function withdraw(address payable to) external {
        uint8 signer = _signerIndex(msg.sender);
        uint256 amount = bondOf[signer];
        if (amount == 0) revert NoBond(signer);
        uint256 openVaults = IZecuredSpendCommits(registry).openVaultCount();
        uint256 openCases = IZecuredSlasher(slasher).openCaseCount();
        uint64 unlocksAt = IZecuredSpendCommits(registry).lastVaultSettledAt() + challengeWindow;
        if (openVaults != 0 || openCases != 0 || block.timestamp < unlocksAt) {
            revert WithdrawalLocked(openVaults, openCases, unlocksAt);
        }
        bondOf[signer] = 0;
        emit BondWithdrawn(signer, to, amount);
        _send(to, amount);
    }

    /// @dev The caller's index in the Registry's sorted signer set; reverts for anyone else.
    function _signerIndex(address account) internal view returns (uint8) {
        address[5] memory signers = IZecuredRegistry(registry).signers();
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if (signers[i] == account) return i;
        }
        revert NotASigner(account);
    }

    function _send(address payable to, uint256 amount) internal {
        (bool ok,) = to.call{value: amount}("");
        if (!ok) revert TransferFailed(to, amount);
    }
}
