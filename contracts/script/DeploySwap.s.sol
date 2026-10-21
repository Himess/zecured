// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {ZecuredSwap} from "../src/ZecuredSwap.sol";
import {SwapConfig} from "../src/interfaces/IZecuredSwap.sol";
import {WorkTier} from "../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredVersions, VersionKind} from "../src/interfaces/v3/IZecuredVersions.sol";
import {IRiscZeroVerifierRouter} from "../src/interfaces/IRiscZeroVerifierRouter.sol";
import {IZcashHeaderRelay, ZCASH_TESTNET} from "../src/relay/IZcashHeaderRelay.sol";

/// @title DeploySwap
/// @notice Deploys ZecuredSwap (Zecured Swap) on the network a config file describes
///         (`contracts/config/swap-<name>.json`), on Zecured's Zcash light client: the header relay, RISC Zero's
///         router and the version registry of the lending deployment (whose `Swap` versions pin the payment guest).
///         Writes deployments/<name>.swap.json (broadcast) or deployments/<name>.swap.dry-run.json (simulation).
/// @dev Environment (the repo-root .env is loaded by forge):
///        DEPLOYER_PRIVATE_KEY   deployer key (never logged)
///        DEPLOYMENT_NAME        the network: base-sepolia (default), anvil, base-mainnet-beta
///        SWAP_CONFIG            optional: the config file (default config/swap-<DEPLOYMENT_NAME>.json)
///        ZECURED_VERSIONS       the ZecuredVersions of the lending deployment (deployments/<name>.json "versions")
///        ZCASH_RELAY            the ZcashHeaderRelay (script/DeployRelay.s.sol)
///        RISC0_VERIFIER         optional: the verifier (default: the config's router; a local fork takes the version
///                               registry's, the MOCK verifier)
///        SWAP_USDC              the escrowed token on a local fork (the lending deployment's MockUSDC); elsewhere
///                               optional, overriding the config's
///        PAYMENT_IMAGE_ID       optional: the payment guest's image id (payment-test / payment-main); the script
///                               reports whether ZecuredVersions has an active Swap version for it
///        PAYMENT_SEAL_SELECTOR  optional: that version's seal selector (default Groth16 v3.0 0x73c457ba; the MOCK
///                               verifier's 0xffffffff on a local fork)
///      A Swap version is scheduled by all five signers through ZecuredVersions (a public delay, D54), or is one of
///      its initial versions; until then the swap takes no proof. Nothing else is needed: no owner, no role.
///      Dry run: forge script script/DeploySwap.s.sol --rpc-url https://sepolia.base.org
///      Deployment: forge script script/DeploySwap.s.sol --rpc-url base_sepolia --broadcast
contract DeploySwap is Script {
    string internal constant OUT_DIR = "deployments";
    bytes4 internal constant GROTH16_SELECTOR = 0x73c457ba;
    bytes4 internal constant MOCK_SELECTOR = 0xffffffff;
    uint256 internal constant WORK_FLOOR = 1_000_000;

    /// One network's swap parameters: contracts/config/swap-<name>.json.
    struct SwapNetworkConfig {
        string name;
        uint256 chainId;
        /// "zcash-testnet" or "zcash-mainnet": the relay's network and the payment guest's.
        string zcashNetwork;
        bool localFork;
        address usdc;
        address riscZeroRouter;
        /// The branch ids the Swap version should allow (informational: the version carries them).
        uint32[4] branchIds;
        WorkTier[] workTiers;
        uint256 minWorkFloor;
        uint64 finalizeDepth;
        uint64 finalizeDelay;
        uint256 maxOrderUsdc;
        uint64 maxOrderZat;
        uint32 minPayWindow;
        uint32 maxPayWindow;
        uint16 feeBps;
        address feeRecipient;
    }

    struct Deployment {
        address deployer;
        bool dryRun;
        address usdc;
        address verifier;
        address versions;
        address relay;
        bytes32 paymentImageId;
        bool swapVersionActive;
        ZecuredSwap swap;
    }

    function run() external returns (Deployment memory d) {
        SwapNetworkConfig memory cfg = _readConfig();
        require(block.chainid == cfg.chainId, "DeploySwap: the RPC's chain is not the config's");
        _checkConfig(cfg);
        d.dryRun = !vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) && !vm.isContext(VmSafe.ForgeContext.ScriptResume);
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY"); // never logged
        d.deployer = vm.addr(deployerKey);
        d.versions = vm.envAddress("ZECURED_VERSIONS");
        d.relay = vm.envAddress("ZCASH_RELAY");
        _checkRelay(d.relay, cfg);
        d.usdc = vm.envOr("SWAP_USDC", cfg.usdc);
        require(d.usdc != address(0), "DeploySwap: no USDC (a local fork needs SWAP_USDC)");
        require(IERC20Metadata(d.usdc).decimals() == 6, "DeploySwap: the escrowed token has 6 decimals (USDC)");
        address registryRouter = IZecuredVersions(d.versions).router();
        d.verifier = vm.envOr("RISC0_VERIFIER", cfg.localFork ? registryRouter : cfg.riscZeroRouter);
        require(d.verifier == registryRouter, "DeploySwap: the verifier is not the version registry's router");
        require(d.verifier.code.length > 0, "DeploySwap: the verifier has no code");
        d.paymentImageId = vm.envOr("PAYMENT_IMAGE_ID", bytes32(0));
        if (d.paymentImageId != bytes32(0)) {
            bytes memory raw =
                vm.envOr("PAYMENT_SEAL_SELECTOR", abi.encodePacked(cfg.localFork ? MOCK_SELECTOR : GROTH16_SELECTOR));
            require(raw.length == 4, "DeploySwap: PAYMENT_SEAL_SELECTOR must be 4 bytes");
            // forge-lint: disable-next-line(unsafe-typecast)
            bytes4 selector = bytes4(raw); // checked: exactly four bytes
            if (!cfg.localFork) {
                require(
                    IRiscZeroVerifierRouter(d.verifier).getVerifier(selector) != address(0),
                    "DeploySwap: the router has no verifier for the seal selector"
                );
            }
            bytes32 versionId = IZecuredVersions(d.versions).versionIdOf(VersionKind.Swap, selector, d.paymentImageId);
            d.swapVersionActive = IZecuredVersions(d.versions).isActive(versionId);
        }

        vm.startBroadcast(deployerKey);
        d.swap = new ZecuredSwap(_swapConfig(cfg, d));
        vm.stopBroadcast();

        _log(cfg, d);
        _writeJson(cfg, d);
    }

    // ------------------------------------------------------------------ config

    function _configPath() internal view returns (string memory name, string memory path) {
        name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        path = vm.envOr("SWAP_CONFIG", string.concat("config/swap-", name, ".json"));
    }

    function _readConfig() internal view returns (SwapNetworkConfig memory) {
        (string memory name, string memory path) = _configPath();
        return _readConfigAt(name, path);
    }

    function _readConfigAt(string memory name, string memory path) internal view returns (SwapNetworkConfig memory c) {
        string memory json = vm.readFile(path);
        c.name = name;
        require(
            keccak256(bytes(vm.parseJsonString(json, ".network"))) == keccak256(bytes(name)),
            "DeploySwap: the config is for another DEPLOYMENT_NAME"
        );
        c.chainId = vm.parseJsonUint(json, ".chainId");
        c.zcashNetwork = vm.parseJsonString(json, ".zcashNetwork");
        c.localFork = vm.parseJsonBool(json, ".localFork");
        c.usdc = vm.parseJsonAddress(json, ".usdc");
        c.riscZeroRouter = vm.parseJsonAddress(json, ".riscZeroRouter");
        uint256[] memory branches = vm.parseJsonUintArray(json, ".branchIds");
        require(branches.length <= 4, "DeploySwap: at most four branch ids");
        for (uint256 i; i < branches.length; ++i) {
            // forge-lint: disable-next-line(unsafe-typecast)
            c.branchIds[i] = uint32(branches[i]);
        }
        uint256[] memory maxAmounts = vm.parseJsonUintArray(json, ".tierMaxAmountZat");
        uint256[] memory minWorks = vm.parseJsonUintArray(json, ".tierMinWork");
        require(maxAmounts.length == minWorks.length, "DeploySwap: work tiers: two arrays of one length");
        c.workTiers = new WorkTier[](maxAmounts.length);
        for (uint256 i; i < maxAmounts.length; ++i) {
            // forge-lint: disable-next-line(unsafe-typecast)
            c.workTiers[i] = WorkTier({maxAmountZat: uint64(maxAmounts[i]), minWork: minWorks[i]});
        }
        c.minWorkFloor = vm.parseJsonUint(json, ".minWorkFloor");
        c.finalizeDepth = uint64(vm.parseJsonUint(json, ".finalizeDepth"));
        c.finalizeDelay = uint64(vm.parseJsonUint(json, ".finalizeDelay"));
        c.maxOrderUsdc = vm.parseJsonUint(json, ".maxOrderUsdc");
        c.maxOrderZat = uint64(vm.parseJsonUint(json, ".maxOrderZat"));
        c.minPayWindow = uint32(vm.parseJsonUint(json, ".minPayWindow"));
        c.maxPayWindow = uint32(vm.parseJsonUint(json, ".maxPayWindow"));
        c.feeBps = uint16(vm.parseJsonUint(json, ".feeBps"));
        c.feeRecipient = vm.parseJsonAddress(json, ".feeRecipient");
    }

    /// The rules the contract cannot know: what a judge or mainnet deployment must never get wrong. (The constructor
    /// checks the rest: ascending tiers that never ask less work for more value, at most four, the order cap within
    /// them, the pay windows, the fee and its recipient.)
    function _checkConfig(SwapNetworkConfig memory c) internal pure {
        for (uint256 i; i < c.workTiers.length; ++i) {
            require(c.workTiers[i].minWork >= c.minWorkFloor, "DeploySwap: a work tier below the floor");
        }
        if (!c.localFork) {
            require(c.minWorkFloor >= WORK_FLOOR, "DeploySwap: the work floor is at least 1,000,000 off a local fork");
            require(c.usdc != address(0), "DeploySwap: the config names the USDC off a local fork");
            // A payer needs the work tier's confirmations and a proof inside the window: an hour at least.
            require(c.minPayWindow >= 1 hours, "DeploySwap: a pay window under an hour is for a local fork only");
        }
        require(c.finalizeDepth != 0 && c.finalizeDelay != 0, "DeploySwap: K3 needs a depth and a delay (R-18)");
        require(c.branchIds[0] != 0, "DeploySwap: the Swap version's branch ids");
        require(c.maxPayWindow <= 7 days, "DeploySwap: a locker's USDC waits at most a week");
    }

    function _checkRelay(address relay, SwapNetworkConfig memory c) internal view {
        require(relay.code.length > 0, "DeploySwap: ZCASH_RELAY has no code");
        (bytes32 tip,,) = IZcashHeaderRelay(relay).bestChainTip();
        require(tip != bytes32(0), "DeploySwap: ZCASH_RELAY is not a header relay");
        bytes32 wanted = keccak256(bytes(c.zcashNetwork));
        try IZcashHeaderRelay(relay).networkId() returns (bytes32 id) {
            require(id == wanted, "DeploySwap: ZCASH_RELAY follows another Zcash network");
        } catch {
            require(wanted == ZCASH_TESTNET, "DeploySwap: a relay without a network id follows Testnet only");
        }
    }

    function _swapConfig(SwapNetworkConfig memory c, Deployment memory d) internal pure returns (SwapConfig memory) {
        return SwapConfig({
            usdc: d.usdc,
            verifier: d.verifier,
            versions: d.versions,
            relay: d.relay,
            workTiers: c.workTiers,
            finalizeDepth: c.finalizeDepth,
            finalizeDelay: c.finalizeDelay,
            maxOrderUsdc: c.maxOrderUsdc,
            maxOrderZat: c.maxOrderZat,
            minPayWindow: c.minPayWindow,
            maxPayWindow: c.maxPayWindow,
            feeBps: c.feeBps,
            feeRecipient: c.feeRecipient
        });
    }

    // ------------------------------------------------------------------ output

    function _log(SwapNetworkConfig memory c, Deployment memory d) internal pure {
        console.log("ZecuredSwap on", c.name);
        console.log("  swap     ", address(d.swap));
        console.log("  usdc     ", d.usdc);
        console.log("  verifier ", d.verifier);
        console.log("  versions ", d.versions);
        console.log("  relay    ", d.relay);
        if (d.paymentImageId == bytes32(0)) {
            console.log("  PAYMENT_IMAGE_ID not given: schedule the payment guest as a Swap version (5/5, delay)");
        } else if (!d.swapVersionActive) {
            console.log("  the payment guest is NOT an active Swap version yet: proofs revert until it is");
        } else {
            console.log("  the payment guest is an active Swap version");
        }
    }

    function _writeJson(SwapNetworkConfig memory c, Deployment memory d) internal {
        string memory k = "swap";
        vm.serializeString(k, "network", c.name);
        vm.serializeUint(k, "chainId", c.chainId);
        vm.serializeString(k, "zcashNetwork", c.zcashNetwork);
        vm.serializeAddress(k, "ZecuredSwap", address(d.swap));
        // As the lending record: the deployment block and a `contracts` object, which the leak scanner reads.
        vm.serializeUint(k, "block", block.number);
        string memory contracts = vm.serializeAddress("swapContracts", "ZecuredSwap", address(d.swap));
        vm.serializeString(k, "contracts", contracts);
        vm.serializeAddress(k, "usdc", d.usdc);
        vm.serializeAddress(k, "verifier", d.verifier);
        vm.serializeAddress(k, "versions", d.versions);
        vm.serializeAddress(k, "relay", d.relay);
        vm.serializeBytes32(k, "paymentImageId", d.paymentImageId);
        vm.serializeBool(k, "swapVersionActive", d.swapVersionActive);
        vm.serializeUint(k, "finalizeDepth", c.finalizeDepth);
        vm.serializeUint(k, "finalizeDelay", c.finalizeDelay);
        vm.serializeUint(k, "maxOrderUsdc", c.maxOrderUsdc);
        vm.serializeUint(k, "maxOrderZat", c.maxOrderZat);
        vm.serializeUint(k, "minPayWindow", c.minPayWindow);
        vm.serializeUint(k, "maxPayWindow", c.maxPayWindow);
        vm.serializeUint(k, "feeBps", c.feeBps);
        vm.serializeAddress(k, "feeRecipient", c.feeRecipient);
        string memory json = vm.serializeAddress(k, "deployer", d.deployer);
        string memory file = string.concat(OUT_DIR, "/", c.name, d.dryRun ? ".swap.dry-run.json" : ".swap.json");
        vm.writeJson(json, file);
        console.log("written", file);
    }
}
