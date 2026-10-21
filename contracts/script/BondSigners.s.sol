// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SignerBondV2} from "../src/SignerBondV2.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {IZecuredRegistry} from "../src/interfaces/v3/IZecuredRegistry.sol";

/// @title BondSigners (v3)
/// @notice Each of the Registry's five signers brings its bond to `BOND_USDC` from its own key (design note T-01: USDC
///         bonds; docs/TIER3.md § 7). The Intake mints nothing until all five hold at least `minBond`, and the coverage
///         cap admits deposits up to the effective bond. A signer short of USDC or of gas is topped up by the deployer
///         first (with MockUSDC on a local fork, the deployer mints it).
/// @dev Environment (the repo-root .env is loaded by forge):
///        SIGNER_BOND                                  the SignerBondV2 (deployments/<name>.json, contracts.SignerBond)
///        SIGNER_1_PRIVATE_KEY .. SIGNER_5_PRIVATE_KEY  the signers' attestation keys (never logged)
///        DEPLOYER_PRIVATE_KEY                         tops up a signer that needs it
///        BOND_USDC                                    optional: whole USDC per signer (default: the bond's minBond)
///      forge script script/BondSigners.s.sol --rpc-url base_sepolia --broadcast
contract BondSigners is Script {
    /// Gas money a signer keeps besides its bond (deposits and, later, resubmitted commitments).
    uint256 internal constant GAS_RESERVE = 0.002 ether;

    function run() external {
        SignerBondV2 bond = SignerBondV2(vm.envAddress("SIGNER_BOND"));
        IERC20 usdc = bond.usdc();
        address[5] memory signers = IZecuredRegistry(bond.registry()).signers();
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY"); // never logged
        uint256 target = vm.envOr("BOND_USDC", bond.minBond() / 1e6) * 1e6;
        require(target >= bond.minBond(), "BondSigners: BOND_USDC is below the bond's minBond");
        for (uint256 n = 1; n <= 5; ++n) {
            uint256 key = vm.envUint(string.concat("SIGNER_", vm.toString(n), "_PRIVATE_KEY")); // never logged
            address signer = vm.addr(key);
            uint8 index = _indexOf(signers, signer);
            uint256 bonded = bond.bondOf(index);
            if (bonded >= target) {
                console.log("already bonded:", signer);
                continue;
            }
            uint256 amount = target - bonded;
            _topUp(usdc, deployerKey, signer, amount);
            vm.startBroadcast(key);
            usdc.approve(address(bond), amount);
            bond.deposit(amount);
            vm.stopBroadcast();
            console.log("bonded:", signer, "index", index);
        }
        require(bond.fullyBonded(), "BondSigners: not all five signers are bonded");
        console.log("all five signers bonded; effective bond (USDC units):", bond.effectiveBond());
    }

    /// The deployer funds what the signer lacks: gas, and USDC (minted when the loan token is the deployer's MockUSDC).
    function _topUp(IERC20 usdc, uint256 deployerKey, address signer, uint256 amount) internal {
        address deployer = vm.addr(deployerKey);
        if (signer.balance < GAS_RESERVE) {
            vm.broadcast(deployerKey);
            (bool ok,) = payable(signer).call{value: GAS_RESERVE - signer.balance}("");
            require(ok, "BondSigners: gas top-up failed");
        }
        uint256 held = usdc.balanceOf(signer);
        if (held >= amount) return;
        uint256 missing = amount - held;
        vm.startBroadcast(deployerKey);
        if (_isMockOwnedBy(address(usdc), deployer)) {
            MockUSDC(address(usdc)).mint(signer, missing);
        } else {
            require(
                usdc.balanceOf(deployer) >= missing, "BondSigners: neither the signer nor the deployer has the USDC"
            );
            require(usdc.transfer(signer, missing), "BondSigners: USDC top-up failed");
        }
        vm.stopBroadcast();
    }

    function _isMockOwnedBy(address token, address deployer) internal view returns (bool) {
        (bool ok, bytes memory ret) = token.staticcall(abi.encodeWithSignature("owner()"));
        return ok && ret.length == 32 && abi.decode(ret, (address)) == deployer;
    }

    function _indexOf(address[5] memory signers, address signer) internal pure returns (uint8) {
        for (uint8 i; i < 5; ++i) {
            if (signers[i] == signer) return i;
        }
        revert("BondSigners: a key is not one of the Registry's signers");
    }
}
