// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../src/mocks/MockZecOracle.sol";
import {ZZEC} from "../src/ZZEC.sol";
import {ZecuredRegistry} from "../src/ZecuredRegistry.sol";
import {SettlementAdapter} from "../src/SettlementAdapter.sol";
import {DepositProofConfig} from "../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../src/interfaces/IRiscZeroVerifier.sol";
import {MockRiscZeroVerifier} from "../src/mocks/MockRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../src/relay/IZcashHeaderRelay.sol";

/// @title DeployZecured
/// @notice Deploys the Zecured contracts on Base Sepolia:
///         1. MockUSDC + MockZecOracle (MOCK), or reuses them (MOCK_USDC / MOCK_ZEC_ORACLE);
///         2. ZZEC, ZecuredRegistry and SettlementAdapter at the deployer's next three CREATE addresses (predicted
///            first, because each contract needs the others' addresses as immutables; the constructors verify the
///            wiring). The Registry constructor creates the zZEC / MockUSDC / MockZecOracle / AdaptiveCurveIRM / 62.5 %
///            market on Morpho Blue;
///            With DEPOSIT_IMAGE_ID set, the Registry also accepts Tier 2 deposit proofs (docs/TIER2.md);
///         3. optionally a demo lender supply (the deployer supplies LENDER_SEED_USDC freshly minted MockUSDC);
///         4. writes deployments/base-sepolia.json (broadcast) or deployments/base-sepolia.dry-run.json (simulation).
/// @dev Environment (the repo-root .env is loaded by forge):
///        DEPLOYER_PRIVATE_KEY  deployer key (never logged)
///        SIGNER_ADDRESSES      the 5 signer addresses, comma-separated, strictly increasing
///        ZEC_USD_PRICE_CENTS   initial MockZecOracle price when the oracle is deployed (default 4000 = $40.00)
///        MOCK_USDC, MOCK_ZEC_ORACLE   optional: reuse already deployed mocks
///        LENDER_SEED_USDC      optional: whole MockUSDC to supply as demo lender (default 0 = none)
///        DEPLOYMENT_NAME       optional: output file name (default base-sepolia; scripts/local-fork.sh uses anvil)
///        DEPOSIT_IMAGE_ID      optional: image id of the deposit guest (tier2/); enables attestDepositWithProof
///        ZCASH_RELAY           with DEPOSIT_IMAGE_ID: the ZcashHeaderRelay to anchor proofs to (script/DeployRelay.s.sol)
///        RISC0_VERIFIER        with DEPOSIT_IMAGE_ID: RISC Zero verifier (default: the Base Sepolia router)
///        RISC0_VERIFIER_MOCK   with DEPOSIT_IMAGE_ID: true deploys MockRiscZeroVerifier instead (MOCK, accepts
///                              unproven journals); only allowed with DEPLOYMENT_NAME=anvil
///        MIN_CONFIRMATION_WORK with DEPOSIT_IMAGE_ID: relay work required on top of a proof's anchor header (default
///                              1,000,000: two to three NU7 Testnet blocks, which average ~490,000 each)
///      Dry run (simulation only, sends nothing):
///        forge script script/DeployZecured.s.sol --rpc-url https://sepolia.base.org
///      Real deployment (only once the deployer is funded):
///        forge script script/DeployZecured.s.sol --rpc-url base_sepolia --broadcast
contract DeployZecured is Script {
    using MarketParamsLib for MarketParams;

    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    uint256 internal constant BASE_SEPOLIA_CHAIN_ID = 84532;
    string internal constant OUT_DIR = "deployments";
    /// RISC Zero verifier router on Base Sepolia (docs/TIER2.md § 5).
    address internal constant RISC0_ROUTER = 0x0b144E07A0826182B6b59788c34b32Bfa86Fb711;
    uint256 internal constant DEFAULT_MIN_CONFIRMATION_WORK = 1_000_000;

    struct Deployment {
        address deployer;
        MockUSDC usdc;
        MockZecOracle oracle;
        bool mocksReused;
        ZZEC zzec;
        ZecuredRegistry registry;
        SettlementAdapter adapter;
        Id marketId;
        uint256 lenderSeed; // MockUSDC units (6 decimals)
        bool dryRun;
        DepositProofConfig proofs;
        bool verifierIsMock;
    }

    function run() external returns (Deployment memory d) {
        require(block.chainid == BASE_SEPOLIA_CHAIN_ID, "DeployZecured: not Base Sepolia (84532)");
        require(MORPHO.isIrmEnabled(ADAPTIVE_CURVE_IRM), "DeployZecured: IRM not enabled");
        require(MORPHO.isLltvEnabled(LLTV), "DeployZecured: LLTV not enabled");

        d.dryRun = !vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) && !vm.isContext(VmSafe.ForgeContext.ScriptResume);
        address[5] memory signerSet = _readSigners();
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY"); // never logged
        d.deployer = vm.addr(deployerKey);
        d.lenderSeed = vm.envOr("LENDER_SEED_USDC", uint256(0)) * 1e6;

        vm.startBroadcast(deployerKey);
        _mocks(d);
        _core(d, signerSet);
        if (d.lenderSeed != 0) _seedLender(d);
        vm.stopBroadcast();

        _log(d);
        _writeJson(d, signerSet);
    }

    // ------------------------------------------------------------------ steps

    function _mocks(Deployment memory d) internal {
        address usdc = vm.envOr("MOCK_USDC", address(0));
        address oracle = vm.envOr("MOCK_ZEC_ORACLE", address(0));
        require((usdc == address(0)) == (oracle == address(0)), "DeployZecured: set both MOCK_USDC and MOCK_ZEC_ORACLE");
        if (usdc != address(0)) {
            require(usdc.code.length > 0 && oracle.code.length > 0, "DeployZecured: mock address without code");
            d.usdc = MockUSDC(usdc);
            d.oracle = MockZecOracle(oracle);
            require(d.usdc.decimals() == 6, "DeployZecured: MOCK_USDC is not 6 decimals");
            require(d.oracle.price() != 0, "DeployZecured: MOCK_ZEC_ORACLE has no price");
            d.mocksReused = true;
        } else {
            uint256 priceCents = vm.envOr("ZEC_USD_PRICE_CENTS", uint256(4000));
            d.usdc = new MockUSDC(d.deployer);
            d.oracle = new MockZecOracle(d.deployer, priceCents * 1e32); // cents * 1e32 = USD * 1e34
        }
    }

    /// ZZEC needs the Registry and the Adapter, the Registry needs ZZEC and the Adapter, the Adapter needs the
    /// Registry: all three are created back to back at the deployer's next nonces, whose addresses are known upfront.
    function _core(Deployment memory d, address[5] memory signerSet) internal {
        _depositProofs(d);
        uint64 nonce = vm.getNonce(d.deployer);
        address predictedRegistry = vm.computeCreateAddress(d.deployer, nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(d.deployer, nonce + 2);

        d.zzec = new ZZEC(predictedRegistry, predictedAdapter, address(MORPHO));
        d.registry = new ZecuredRegistry(
            MORPHO,
            d.zzec,
            predictedAdapter,
            address(d.usdc),
            address(d.oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            d.proofs
        );
        d.adapter = new SettlementAdapter(d.registry);
        require(address(d.registry) == predictedRegistry, "DeployZecured: registry address mismatch");
        require(address(d.adapter) == predictedAdapter, "DeployZecured: adapter address mismatch");
        d.marketId = d.registry.marketId();
        require(MORPHO.market(d.marketId).lastUpdate != 0, "DeployZecured: market not created");
    }

    /// Tier 2 configuration (before the core contracts: a MOCK verifier would shift their predicted addresses).
    function _depositProofs(Deployment memory d) internal {
        bytes32 imageId = vm.envOr("DEPOSIT_IMAGE_ID", bytes32(0));
        if (imageId == bytes32(0)) return; // proofs disabled
        address relay = vm.envAddress("ZCASH_RELAY");
        require(relay.code.length > 0, "DeployZecured: ZCASH_RELAY has no code");
        (bytes32 tip,,) = IZcashHeaderRelay(relay).bestChainTip();
        require(tip != bytes32(0), "DeployZecured: ZCASH_RELAY is not a header relay");
        address verifier;
        if (vm.envOr("RISC0_VERIFIER_MOCK", false)) {
            require(
                keccak256(bytes(vm.envOr("DEPLOYMENT_NAME", string("")))) == keccak256("anvil"),
                "DeployZecured: the MOCK verifier is for the local fork only (DEPLOYMENT_NAME=anvil)"
            );
            verifier = address(new MockRiscZeroVerifier());
            d.verifierIsMock = true;
        } else {
            verifier = vm.envOr("RISC0_VERIFIER", RISC0_ROUTER);
            require(verifier.code.length > 0, "DeployZecured: RISC0_VERIFIER has no code");
        }
        d.proofs = DepositProofConfig({
            verifier: IRiscZeroVerifier(verifier),
            imageId: imageId,
            relay: IZcashHeaderRelay(relay),
            minConfirmationWork: vm.envOr("MIN_CONFIRMATION_WORK", DEFAULT_MIN_CONFIRMATION_WORK)
        });
    }

    function _seedLender(Deployment memory d) internal {
        require(d.usdc.owner() == d.deployer, "DeployZecured: the deployer does not own MockUSDC");
        MarketParams memory params = d.registry.marketParams();
        d.usdc.mint(d.deployer, d.lenderSeed);
        d.usdc.approve(address(MORPHO), d.lenderSeed);
        MORPHO.supply(params, d.lenderSeed, 0, d.deployer, "");
    }

    // ------------------------------------------------------------------ input / output

    function _readSigners() internal view returns (address[5] memory signerSet) {
        address[] memory list = vm.envAddress("SIGNER_ADDRESSES", ",");
        require(list.length == 5, "DeployZecured: SIGNER_ADDRESSES must list exactly 5 addresses");
        for (uint256 i; i < 5; ++i) {
            require(i == 0 || list[i] > list[i - 1], "DeployZecured: SIGNER_ADDRESSES must be strictly increasing");
            signerSet[i] = list[i];
        }
        require(signerSet[0] != address(0), "DeployZecured: zero signer");
    }

    function _log(Deployment memory d) internal pure {
        console.log(d.dryRun ? "DRY RUN (simulation, nothing broadcast)" : "BROADCAST");
        console.log("deployer:", d.deployer);
        console.log("MockUSDC (MOCK):", address(d.usdc), d.mocksReused ? "(reused)" : "(new)");
        console.log("MockZecOracle (MOCK):", address(d.oracle), d.mocksReused ? "(reused)" : "(new)");
        console.log("ZZEC:", address(d.zzec));
        console.log("ZecuredRegistry:", address(d.registry));
        console.log("SettlementAdapter:", address(d.adapter));
        console.log("Morpho market id:");
        console.logBytes32(Id.unwrap(d.marketId));
        console.log("demo lender supply (MockUSDC units):", d.lenderSeed);
        if (address(d.proofs.verifier) == address(0)) {
            console.log("deposit proofs: disabled");
        } else {
            console.log("deposit proofs: verifier", address(d.proofs.verifier), d.verifierIsMock ? "(MOCK)" : "");
            console.log("deposit proofs: relay", address(d.proofs.relay));
            console.log("deposit proofs: min confirmation work", d.proofs.minConfirmationWork);
        }
    }

    function _writeJson(Deployment memory d, address[5] memory signerSet) internal {
        string memory contracts = "contracts";
        vm.serializeAddress(contracts, "ZZEC", address(d.zzec));
        vm.serializeAddress(contracts, "ZecuredRegistry", address(d.registry));
        vm.serializeAddress(contracts, "SettlementAdapter", address(d.adapter));
        vm.serializeAddress(contracts, "MockUSDC", address(d.usdc));
        contracts = vm.serializeAddress(contracts, "MockZecOracle", address(d.oracle));

        string memory market = "market";
        vm.serializeAddress(market, "morphoBlue", address(MORPHO));
        vm.serializeBytes32(market, "marketId", Id.unwrap(d.marketId));
        vm.serializeAddress(market, "loanToken", address(d.usdc));
        vm.serializeAddress(market, "collateralToken", address(d.zzec));
        vm.serializeAddress(market, "oracle", address(d.oracle));
        vm.serializeAddress(market, "irm", ADAPTIVE_CURVE_IRM);
        market = vm.serializeString(market, "lltv", vm.toString(LLTV)); // decimal string: > 2^53

        string memory eip712 = "eip712";
        vm.serializeString(eip712, "registryDomain", "ZecuredRegistry");
        vm.serializeString(eip712, "adapterDomain", "ZecuredSettlementAdapter");
        vm.serializeString(eip712, "version", "1");
        vm.serializeBytes32(eip712, "registryDomainSeparator", d.registry.domainSeparator());
        eip712 = vm.serializeBytes32(eip712, "adapterDomainSeparator", d.adapter.domainSeparator());

        string memory proofs = "depositProofs";
        vm.serializeAddress(proofs, "verifier", address(d.proofs.verifier));
        vm.serializeBool(proofs, "verifierIsMock", d.verifierIsMock);
        vm.serializeBytes32(proofs, "imageId", d.proofs.imageId);
        vm.serializeAddress(proofs, "relay", address(d.proofs.relay));
        proofs = vm.serializeString(proofs, "minConfirmationWork", vm.toString(d.proofs.minConfirmationWork));

        address[] memory signerList = new address[](5);
        for (uint256 i; i < 5; ++i) {
            signerList[i] = signerSet[i];
        }

        string memory root = "deployment";
        vm.serializeString(root, "network", "base-sepolia");
        vm.serializeUint(root, "chainId", block.chainid);
        vm.serializeBool(root, "dryRun", d.dryRun);
        vm.serializeUint(root, "block", block.number);
        vm.serializeUint(root, "timestamp", block.timestamp);
        vm.serializeAddress(root, "deployer", d.deployer);
        vm.serializeString(root, "contracts", contracts);
        vm.serializeString(root, "market", market);
        vm.serializeString(root, "eip712", eip712);
        vm.serializeString(root, "depositProofs", proofs);
        vm.serializeAddress(root, "signers", signerList);
        vm.serializeUint(root, "threshold", d.registry.THRESHOLD());
        vm.serializeUint(root, "minSeize", d.adapter.MIN_SEIZE());
        vm.serializeUint(root, "maxMarginBps", d.adapter.MAX_MARGIN_BPS());
        vm.serializeString(root, "oraclePrice", vm.toString(d.oracle.price())); // USD * 1e34, decimal string
        vm.serializeBool(root, "mocksReused", d.mocksReused);
        vm.serializeUint(root, "lenderSeedUsdcUnits", d.lenderSeed);
        string memory json = vm.serializeString(
            root,
            "mocks",
            d.verifierIsMock
                ? "MockUSDC and MockZecOracle are MOCKS (testnet stand-ins, owner-controlled); MockRiscZeroVerifier is a MOCK (accepts unproven deposit journals)"
                : "MockUSDC and MockZecOracle are MOCKS (testnet stand-ins, owner-controlled)"
        );

        vm.createDir(OUT_DIR, true);
        string memory name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        string memory path = string.concat(OUT_DIR, "/", name, d.dryRun ? ".dry-run.json" : ".json");
        vm.writeJson(json, path);
        console.log("written:", path);
    }
}
