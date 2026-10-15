// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZcashHeaderRelay, ZCASH_TESTNET} from "../../src/relay/IZcashHeaderRelay.sol";

/// @title MockHeaderRelay
/// @notice MOCK, tests only. A header relay whose best chain the test sets directly, so that the Registry's pending
///         activations (K3) can be driven through reorganisations that real Equihash headers cannot produce in a test:
///         an anchor that leaves the best chain, work that is not yet enough, a tip that has not moved on.
contract MockHeaderRelay is IZcashHeaderRelay {
    mapping(bytes32 blockHash => bool) public onBestChain;
    mapping(bytes32 blockHash => uint256) public work;
    uint256 public tipHeight;
    /// @notice A Testnet relay unless the test says otherwise.
    bytes32 public networkId = ZCASH_TESTNET;

    function setNetworkId(bytes32 id) external {
        networkId = id;
    }

    function setBlock(bytes32 blockHash, bool best, uint256 confirmationWork_) external {
        onBestChain[blockHash] = best;
        work[blockHash] = confirmationWork_;
    }

    function setTipHeight(uint256 height) external {
        tipHeight = height;
    }

    function isInBestChain(bytes32 blockHash) external view returns (bool) {
        return onBestChain[blockHash];
    }

    function confirmationWork(bytes32 blockHash) external view returns (uint256) {
        return onBestChain[blockHash] ? work[blockHash] : 0;
    }

    function bestChainTip() external view returns (bytes32, uint256, uint256) {
        return (bytes32(0), tipHeight, 0);
    }

    function confirmations(bytes32) external pure returns (uint256) {
        revert("MockHeaderRelay: not implemented");
    }

    function submitHeader(bytes calldata) external pure returns (bytes32) {
        revert("MockHeaderRelay: not implemented");
    }

    function submitHeaders(bytes[] calldata) external pure {
        revert("MockHeaderRelay: not implemented");
    }

    function headerOf(bytes32) external pure returns (HeaderInfo memory) {
        revert("MockHeaderRelay: not implemented");
    }

    function bestBlockHashAt(uint256) external pure returns (bytes32) {
        revert("MockHeaderRelay: not implemented");
    }
}
