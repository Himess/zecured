// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice MOCK (tests only): a Chainlink-style AggregatorV3 price source for ZecuredPriceFeed's unit tests. The test
///         sets its answer, its `updatedAt`, its decimals, and how it fails: by reverting, by returning four words
///         instead of five, by burning all the gas it is given, or by returning 64 KB (whose first five words are a
///         normal answer).
contract MockAggregator {
    enum Mode {
        Normal,
        Revert,
        ShortData,
        BurnGas,
        HugeData
    }

    /// @notice The raw word `decimals()` returns (a word above 255 is not a uint8: garbage).
    uint256 public decimalsWord;
    bool public decimalsReverts;
    int256 public answer;
    uint256 public updatedAt;
    uint80 public roundId;
    Mode public mode;

    constructor(uint8 decimals_) {
        decimalsWord = decimals_;
    }

    /// @notice MOCK control: the next answer and its `updatedAt` (a new round).
    function set(int256 answer_, uint256 updatedAt_) external {
        answer = answer_;
        updatedAt = updatedAt_;
        ++roundId;
    }

    /// @notice MOCK control: how `latestRoundData()` behaves.
    function setMode(Mode mode_) external {
        mode = mode_;
    }

    /// @notice MOCK control: the raw word `decimals()` returns, or a revert.
    function setDecimals(uint256 word, bool reverts) external {
        decimalsWord = word;
        decimalsReverts = reverts;
    }

    function decimals() external view returns (uint8) {
        if (decimalsReverts) revert("MockAggregator: decimals reverted");
        uint256 word = decimalsWord;
        assembly ("memory-safe") {
            mstore(0, word)
            return(0, 32)
        }
    }

    function description() external pure returns (string memory) {
        return "MOCK ZEC / USD";
    }

    function latestRoundData() external view returns (uint80, int256, uint256, uint256, uint80) {
        Mode m = mode;
        uint80 r = roundId;
        int256 a = answer;
        uint256 t = updatedAt;
        if (m == Mode.Revert) revert("MockAggregator: reverted");
        if (m == Mode.BurnGas) {
            assembly ("memory-safe") {
                invalid() // consumes all the gas the call was given
            }
        }
        if (m == Mode.ShortData) {
            // Four words of the five, a plausible answer and updatedAt among them: only the length is wrong.
            assembly {
                let p := mload(0x40)
                mstore(p, r)
                mstore(add(p, 0x20), a)
                mstore(add(p, 0x40), t)
                mstore(add(p, 0x60), t)
                return(p, 0x80)
            }
        }
        if (m == Mode.HugeData) {
            assembly {
                let p := mload(0x40)
                mstore(p, r)
                mstore(add(p, 0x20), a)
                mstore(add(p, 0x40), t)
                mstore(add(p, 0x60), t)
                mstore(add(p, 0x80), r)
                return(p, 65536)
            }
        }
        return (r, a, t, t, r);
    }
}
