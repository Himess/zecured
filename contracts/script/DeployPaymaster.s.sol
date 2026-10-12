// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";

/// @dev EntryPoint v0.7's deposit entry point.
interface IEntryPointDeposits {
    function depositTo(address account) external payable;
}

/// @dev The part of Pimlico's SingletonPaymasterV7 this script calls.
interface ISingletonPaymasterV7 {
    function entryPoint() external view returns (address);
    function hasRole(bytes32 role, address account) external view returns (bool);
    function signers(address account) external view returns (bool);
    function getDeposit() external view returns (uint256);
}

/// @title DeployPaymaster
/// @notice RQ-05: deploys our own ERC-4337 paymaster, Pimlico's SingletonPaymasterV7 (MIT) for EntryPoint v0.7, with
///         our own owner, manager and signer, and funds its EntryPoint deposit. The backend signs its `paymasterData`
///         (verifying mode) only for UserOps that pass the allowlist (docs/INTERFACES.md § 4), so no third-party
///         paymaster sees a UserOp or decides what is sponsored.
/// @dev The creation code (script/artifacts/SingletonPaymasterV7.creation.hex, 16,263 bytes, SHA-256
///      f79f7dce…cf54) is the one Pimlico's npm package mock-paymaster 0.0.8 deploys on the local fork, so the local
///      tests and Base Sepolia run the same paymaster code. Constructor: (entryPoint, owner, manager, signers[]);
///      the owner gets DEFAULT_ADMIN_ROLE and the manager MANAGER_ROLE (AccessControl, no owner()).
///      Environment: DEPLOYER_PRIVATE_KEY (owner and manager; never logged), PAYMASTER_SIGNER_ADDRESS (the backend's
///      signing key), PAYMASTER_DEPOSIT_WEI (default 0.05 ether), DEPLOYMENT_NAME (default base-sepolia): writes
///      deployments/<name>.paymaster.json.
///        forge script script/DeployPaymaster.s.sol --rpc-url <rpc> --broadcast
contract DeployPaymaster is Script {
    address internal constant ENTRY_POINT_V07 = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;
    /// The deterministic CREATE2 deployer (present on Base Sepolia), as the local fork's paymaster uses it.
    address internal constant CREATE2_DEPLOYER = 0x4e59b44847b379578588920cA78FbF26c0B4956C;
    bytes32 internal constant SALT = keccak256("zecured.paymaster.v1");

    function run() external returns (address paymaster) {
        // forge-lint: disable-next-line(unsafe-cheatcode)
        bytes memory creation =
            vm.parseBytes(vm.trim(vm.readFile("script/artifacts/SingletonPaymasterV7.creation.hex")));
        require(sha256(creation) == 0xf79f7dce4390a01de5e6efd0d9fe0fb0c58a9c544d26a5a2d5fef36b66bdcf54, "creation code");
        address signer = vm.envAddress("PAYMASTER_SIGNER_ADDRESS");
        uint256 key = vm.envUint("DEPLOYER_PRIVATE_KEY");
        uint256 depositWei = vm.envOr("PAYMASTER_DEPOSIT_WEI", uint256(0.05 ether));
        address owner = vm.addr(key);
        address[] memory signers = new address[](1);
        signers[0] = signer;
        bytes memory initCode = abi.encodePacked(creation, abi.encode(ENTRY_POINT_V07, owner, owner, signers));

        // A plain call to the CREATE2 deployer: in a script, an assembly CREATE would be simulated from another
        // nonce than the broadcaster's, and the address it returned would not be the deployed one.
        paymaster = vm.computeCreate2Address(SALT, keccak256(initCode), CREATE2_DEPLOYER);
        vm.startBroadcast(key);
        (bool created,) = CREATE2_DEPLOYER.call(abi.encodePacked(SALT, initCode));
        require(created && paymaster.code.length > 0, "DeployPaymaster: creation failed");
        // Credited to the paymaster, as its own deposit() would do.
        IEntryPointDeposits(ENTRY_POINT_V07).depositTo{value: depositWei}(paymaster);
        vm.stopBroadcast();

        ISingletonPaymasterV7 pm = ISingletonPaymasterV7(paymaster);
        require(pm.entryPoint() == ENTRY_POINT_V07, "DeployPaymaster: entry point");
        // Roles (AccessControl): the admin can withdraw the deposit, the manager can add signers.
        require(pm.hasRole(bytes32(0), owner), "DeployPaymaster: admin");
        require(pm.hasRole(keccak256("MANAGER_ROLE"), owner), "DeployPaymaster: manager");
        require(pm.signers(signer), "DeployPaymaster: signer");
        console.log("SingletonPaymasterV7:", paymaster);
        console.log("signer:", signer);
        console.log("EntryPoint deposit (wei):", pm.getDeposit());

        string memory out = "paymaster";
        vm.serializeAddress(out, "SingletonPaymasterV7", paymaster);
        vm.serializeAddress(out, "entryPoint", ENTRY_POINT_V07);
        vm.serializeAddress(out, "owner", owner);
        out = vm.serializeAddress(out, "signer", signer);
        string memory name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        vm.writeJson(out, string.concat("deployments/", name, ".paymaster.json"));
    }
}
