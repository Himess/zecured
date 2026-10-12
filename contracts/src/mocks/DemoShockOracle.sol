// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";

/// @title DemoShockOracle
/// @notice MOCK / DEMO. The oracle of the Zecured market: a real ZEC/USD price (the base oracle) times a demo
///         "shock" the owner sets, so that a liquidation can be shown on demand without faking the price itself:
///         effective price = base price * shockBps / 10,000. With `shockBps = 10,000` (the default) the market runs
///         on the real price. Every change is public (`ShockSet`), and the app shows base, shock and effective price.
/// @dev Morpho assumes the price never drops, in one step, below old price * LLTV * LIF (0.625 * 1.12676 = 0.704
///      for this market): beyond that a liquidation can leave bad debt. So a lower shock must keep at least
///      `MIN_STEP_BPS` (75 %) of the previous one, and two lowerings are at least `MIN_STEP_INTERVAL` apart, which
///      gives the liquidation bot time to act between steps; a deeper shock is made in steps. Raising the shock is
///      never limited, nor is the base price (it is what it is). The scale is Morpho's (USD-per-ZEC * 1e34 for
///      zZEC / USDC), the base oracle's.
contract DemoShockOracle is IOracle, Ownable {
    uint256 public constant BPS = 10_000;
    /// @notice A lower shock keeps at least this share of the previous one.
    uint256 public constant MIN_STEP_BPS = 7_500;
    /// @notice Two lowerings of the shock are at least this far apart.
    uint256 public constant MIN_STEP_INTERVAL = 2 minutes;

    /// @notice The real price source.
    IOracle public immutable base;
    /// @notice The demo multiplier, in basis points of the base price (at most 10,000).
    uint256 public shockBps;
    /// @notice When the shock was last lowered.
    uint256 public lastLoweredAt;

    event ShockSet(uint256 oldShockBps, uint256 newShockBps);

    error ZeroAddress();
    error InvalidShock(uint256 shockBps);
    error StepTooDeep(uint256 oldShockBps, uint256 newShockBps);
    error StepTooSoon(uint256 nextAllowedAt);

    constructor(address initialOwner, IOracle base_) Ownable(initialOwner) {
        if (address(base_) == address(0)) revert ZeroAddress();
        base = base_;
        shockBps = BPS;
        emit ShockSet(0, BPS);
    }

    /// @inheritdoc IOracle
    /// @dev Reverts if the base reverts (for example on a stale price): Morpho then pauses borrowing, withdrawals and
    ///      liquidations of this market rather than using a wrong price.
    function price() external view returns (uint256) {
        return base.price() * shockBps / BPS;
    }

    /// @notice DEMO control: sets the shock (1 to 10,000 basis points of the base price). Owner only. A lower shock
    ///         keeps at least `MIN_STEP_BPS` of the current one and comes at least `MIN_STEP_INTERVAL` after the last
    ///         lowering.
    function setShock(uint256 newShockBps) external onlyOwner {
        if (newShockBps == 0 || newShockBps > BPS) revert InvalidShock(newShockBps);
        uint256 old = shockBps;
        if (newShockBps < old) {
            if (newShockBps * BPS < old * MIN_STEP_BPS) revert StepTooDeep(old, newShockBps);
            if (block.timestamp < lastLoweredAt + MIN_STEP_INTERVAL) {
                revert StepTooSoon(lastLoweredAt + MIN_STEP_INTERVAL);
            }
            lastLoweredAt = block.timestamp;
        }
        shockBps = newShockBps;
        emit ShockSet(old, newShockBps);
    }
}
