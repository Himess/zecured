// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {SignerBond} from "../src/SignerBond.sol";
import {IZecuredRegistry} from "../src/interfaces/IZecuredRegistry.sol";

/// @title BondSigners
/// @notice Tier 3 (docs/TIER3.md § 7): each of the Registry's five signers deposits its bond from its own key. The
///         Registry mints nothing until all five have. A signer whose balance cannot cover the bond and a little gas
///         is topped up by the deployer first.
/// @dev Environment (the repo-root .env is loaded by forge):
///        SIGNER_BOND                              the SignerBond (deployments/<name>.json, contracts.SignerBond)
///        SIGNER_1_PRIVATE_KEY .. SIGNER_5_PRIVATE_KEY  the signers' attestation keys (never logged)
///        DEPLOYER_PRIVATE_KEY                     tops up a signer that needs it
///      forge script script/BondSigners.s.sol --rpc-url base_sepolia --broadcast
contract BondSigners is Script {
    /// Gas money a signer keeps besides its bond (deposits and, later, resubmitted commitments).
    uint256 internal constant GAS_RESERVE = 0.002 ether;

    function run() external {
        SignerBond bond = SignerBond(vm.envAddress("SIGNER_BOND"));
        address[5] memory signers = IZecuredRegistry(bond.registry()).signers();
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY"); // never logged
        uint256 amount = bond.BOND();
        for (uint256 n = 1; n <= 5; ++n) {
            uint256 key = vm.envUint(string.concat("SIGNER_", vm.toString(n), "_PRIVATE_KEY")); // never logged
            address signer = vm.addr(key);
            uint8 index = _indexOf(signers, signer);
            if (bond.bondOf(index) != 0) {
                console.log("already bonded:", signer);
                continue;
            }
            uint256 need = amount + GAS_RESERVE;
            if (signer.balance < need) {
                vm.broadcast(deployerKey);
                (bool ok,) = payable(signer).call{value: need - signer.balance}("");
                require(ok, "BondSigners: top-up failed");
            }
            vm.broadcast(key);
            bond.deposit{value: amount}();
            console.log("bonded:", signer, "index", index);
        }
        require(bond.fullyBonded(), "BondSigners: not all five signers are bonded");
        console.log("all five signers bonded; the Registry can mint");
    }

    function _indexOf(address[5] memory signers, address signer) internal pure returns (uint8) {
        for (uint8 i; i < 5; ++i) {
            if (signers[i] == signer) return i;
        }
        revert("BondSigners: a key is not one of the Registry's signers");
    }
}
