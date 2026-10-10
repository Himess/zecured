// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";

/// @title SpikeCloseRequestVerifier
/// @notice MOCK, SPIKE-ONLY (throwaway, test/spike/). Stands in for the SettlementAdapter's CloseRequest signature
///         check (docs/INTERFACES.md § 3): an EIP-712 struct digest under the adapter's own domain, verified against the
///         account (a smart account) with OpenZeppelin SignatureChecker, i.e. ERC-1271 for contracts.
contract SpikeCloseRequestVerifier is EIP712 {
    /// @dev Exactly the type in docs/INTERFACES.md 2.4.
    struct CloseRequest {
        bytes32 vaultId;
        address account;
        address executor;
        uint16 marginBps;
        uint64 minUserAmount;
        bytes32 userCommitment;
        uint256 nonce;
        uint64 deadline;
    }

    bytes32 public constant CLOSE_REQUEST_TYPEHASH = keccak256(
        "CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)"
    );

    /// @dev Domain from docs/INTERFACES.md 2.1 (name, version, chainId, verifyingContract).
    constructor() EIP712("ZecuredSettlementAdapter", "1") {}

    function hashCloseRequest(CloseRequest calldata req) public view returns (bytes32) {
        return _hashTypedDataV4(
            keccak256(
                abi.encode(
                    CLOSE_REQUEST_TYPEHASH,
                    req.vaultId,
                    req.account,
                    req.executor,
                    req.marginBps,
                    req.minUserAmount,
                    req.userCommitment,
                    req.nonce,
                    req.deadline
                )
            )
        );
    }

    function isValidCloseRequestSignature(CloseRequest calldata req, bytes calldata signature)
        external
        view
        returns (bool)
    {
        return SignatureChecker.isValidSignatureNow(req.account, hashCloseRequest(req), signature);
    }
}
