// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../src/mocks/MockZecOracle.sol";
import {DemoShockOracle} from "../src/mocks/DemoShockOracle.sol";
import {ZZEC} from "../src/ZZEC.sol";
import {ZecuredRegistry} from "../src/ZecuredRegistry.sol";
import {ISignerBond} from "../src/interfaces/ISignerBond.sol";
import {SignerBond} from "../src/SignerBond.sol";
import {ZecuredSlasher} from "../src/ZecuredSlasher.sol";
import {SettlementAdapter} from "../src/SettlementAdapter.sol";
import {DepositProofConfig, MintMode} from "../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../src/interfaces/IRiscZeroVerifier.sol";
import {MockRiscZeroVerifier} from "../src/mocks/MockRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../src/relay/IZcashHeaderRelay.sol";

/// @title DeployZecured
/// @notice Deploys the Zecured contracts on Base Sepolia:
///         1. the market's assets (RQ-06): the loan token (Circle's testnet USDC on Base Sepolia, a fresh MockUSDC on
///            the local fork), the ZEC/USD base oracle (BASE_ORACLE, or a fresh MockZecOracle, MOCK) and the
///            DemoShockOracle (MOCK / DEMO) around it, which is the market's oracle;
///         2. ZZEC, ZecuredRegistry and SettlementAdapter at the deployer's next three CREATE addresses (predicted
///            first, because each contract needs the others' addresses as immutables; the constructors verify the
///            wiring). The Registry constructor creates the zZEC / MockUSDC / MockZecOracle / AdaptiveCurveIRM / 62.5 %
///            market on Morpho Blue;
///            With DEPOSIT_IMAGE_ID set, the Registry also accepts Tier 2 deposit proofs (docs/TIER2.md);
///         3. with VIOLATION_IMAGE_ID, Tier 3 (docs/TIER3.md): SignerBond and ZecuredSlasher at the next two
///            addresses (the Registry names the bond, the bond the slasher); the Registry then mints only once all five
///            signers have bonded (script/BondSigners.s.sol);
///         4. optionally a demo lender supply (the deployer supplies LENDER_SEED_USDC freshly minted MockUSDC);
///         5. writes deployments/base-sepolia.json (broadcast) or deployments/base-sepolia.dry-run.json (simulation).
/// @dev Environment (the repo-root .env is loaded by forge):
///        DEPLOYER_PRIVATE_KEY  deployer key (never logged)
///        SIGNER_ADDRESSES      the 5 signer addresses, comma-separated, strictly increasing
///        LOAN_TOKEN            optional: the market's loan token, 6 decimals (default: Circle's testnet USDC on Base
///                              Sepolia; a fresh MockUSDC with DEPLOYMENT_NAME=anvil)
///        BASE_ORACLE           optional: the ZEC/USD price oracle the DemoShockOracle wraps (Morpho scale); default:
///                              a fresh MockZecOracle (MOCK) at ZEC_USD_PRICE_CENTS
///        ZEC_USD_PRICE_CENTS   initial MockZecOracle price when one is deployed (default 126500 = $1,265.00)
///        LENDER_SEED_USDC      optional: whole USDC to supply as demo lender (default 0 = none); minted when the loan
///                              token is the MockUSDC, otherwise taken from the deployer's balance
///        DEPLOYMENT_NAME       optional: output file name (default base-sepolia; scripts/local-fork.sh uses anvil)
///        DEPOSIT_IMAGE_ID      optional: image id of the deposit guest (tier2/); enables attestDepositWithProof
///        ZCASH_RELAY           with DEPOSIT_IMAGE_ID: the ZcashHeaderRelay to anchor proofs to (script/DeployRelay.s.sol)
///        RISC0_VERIFIER        with DEPOSIT_IMAGE_ID: RISC Zero verifier (default: the Base Sepolia router)
///        RISC0_VERIFIER_MOCK   with DEPOSIT_IMAGE_ID: true deploys MockRiscZeroVerifier instead (MOCK, accepts
///                              unproven journals); only allowed with DEPLOYMENT_NAME=anvil
///        MIN_CONFIRMATION_WORK with DEPOSIT_IMAGE_ID: relay work required on top of a proof's anchor header (default
///                              3,500,000: about ten normal NU7 Testnet blocks, which carry 364,000 to 402,000 each
///                              around height 4,488,500); at least 1,000,000 outside the local fork
///        MINT_MODE             with DEPOSIT_IMAGE_ID: proof-only (default, K1) or signer-attested; outside the local
///                              fork, signer-attested also needs ALLOW_SIGNER_MINT=true
///        DEPOSIT_SEAL_SELECTOR with DEPOSIT_IMAGE_ID and a real verifier: the seal selector the Registry pins (Y1;
///                              default RISC Zero's Groth16 v3.0 selector 0x73c457ba, which the router must know); the
///                              MOCK verifier always pins its own 0xffffffff
///        FINALIZE_DEPTH        with DEPOSIT_IMAGE_ID: relay headers to wait for after a proof (K3; default 6)
///        FINALIZE_DELAY        with DEPOSIT_IMAGE_ID: seconds to wait after a proof (K3; default 150, six 25 s blocks)
///        VIOLATION_IMAGE_ID    optional, needs DEPOSIT_IMAGE_ID: image id of the violation guest; deploys Tier 3
///        CHALLENGE_WINDOW      with VIOLATION_IMAGE_ID: seconds between a violation proof and its slash (default and
///                              minimum 86,400; THEFT_DEMO=true allows 120, T3-6)
///        SLASH_CONFIRMATION_WORK with VIOLATION_IMAGE_ID: relay work above a violation's anchor (default 38,000,000:
///                              about 100 normal Testnet blocks, S4; at least 30,000,000 outside the local fork)
///        THEFT_DEMO            true: the separate theft-demonstration deployment (T3-6): a 2-minute window allowed,
///                              and DEPLOYMENT_NAME must contain "theft-demo"; never the judge deployment
///        JUDGE_SIGNER_ADDRESSES  with THEFT_DEMO: the judge network's five signers, none of which the theft demo
///                              may use
///      Dry run (simulation only, sends nothing):
///        forge script script/DeployZecured.s.sol --rpc-url https://sepolia.base.org
///      Real deployment (only once the deployer is funded):
///        forge script script/DeployZecured.s.sol --rpc-url base_sepolia --broadcast
/// @dev The part of RISC Zero's RiscZeroVerifierRouter this script reads.
interface IRiscZeroVerifierRouter {
    function getVerifier(bytes4 selector) external view returns (address);
}

contract DeployZecured is Script {
    using MarketParamsLib for MarketParams;

    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    uint256 internal constant BASE_SEPOLIA_CHAIN_ID = 84532;
    string internal constant OUT_DIR = "deployments";
    /// RISC Zero verifier router on Base Sepolia (docs/TIER2.md § 5).
    address internal constant RISC0_ROUTER = 0x0b144E07A0826182B6b59788c34b32Bfa86Fb711;
    uint256 internal constant DEFAULT_MIN_CONFIRMATION_WORK = 3_500_000;
    uint256 internal constant FLOOR_MIN_CONFIRMATION_WORK = 1_000_000;
    /// RISC Zero Groth16 verifier v3.0 selector (verifier parameters digest prefix) behind the router.
    bytes4 internal constant GROTH16_SELECTOR = 0x73c457ba;
    uint64 internal constant DEFAULT_FINALIZE_DEPTH = 6;
    uint64 internal constant DEFAULT_FINALIZE_DELAY = 150;
    uint64 internal constant JUDGE_CHALLENGE_WINDOW = 1 days;
    uint64 internal constant DEMO_CHALLENGE_WINDOW = 2 minutes;
    uint256 internal constant DEFAULT_SLASH_CONFIRMATION_WORK = 38_000_000;
    uint256 internal constant FLOOR_SLASH_CONFIRMATION_WORK = 30_000_000;

    struct Deployment {
        address deployer;
        address loanToken;
        bool loanTokenIsMock;
        IOracle baseOracle;
        bool baseOracleIsMock;
        DemoShockOracle oracle;
        ZZEC zzec;
        ZecuredRegistry registry;
        SettlementAdapter adapter;
        Id marketId;
        uint256 lenderSeed; // USDC units (6 decimals)
        bool dryRun;
        DepositProofConfig proofs;
        bool verifierIsMock;
        // Tier 3 (zero without VIOLATION_IMAGE_ID)
        SignerBond bond;
        ZecuredSlasher slasher;
        bytes32 violationImageId;
        uint64 challengeWindow;
        uint256 slashConfirmationWork;
        bool theftDemo;
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
        _marketAssets(d);
        _core(d, signerSet);
        if (d.lenderSeed != 0) _seedLender(d);
        vm.stopBroadcast();

        _log(d);
        _writeJson(d, signerSet);
    }

    // ------------------------------------------------------------------ steps

    /// Circle's testnet USDC on Base Sepolia (developers.circle.com/stablecoins/usdc-contract-addresses).
    address internal constant CIRCLE_USDC = 0x036CbD53842c5426634e7929541eC2318f3dCF7e;

    /// RQ-06: the loan token and the price. MarketParams are immutable, so this is fixed for the deployment.
    function _marketAssets(Deployment memory d) internal {
        bool localFork = keccak256(bytes(vm.envOr("DEPLOYMENT_NAME", string("")))) == keccak256("anvil");
        d.loanToken = vm.envOr("LOAN_TOKEN", localFork ? address(0) : CIRCLE_USDC);
        if (d.loanToken == address(0)) {
            d.loanToken = address(new MockUSDC(d.deployer));
            d.loanTokenIsMock = true;
        } else {
            require(d.loanToken.code.length > 0, "DeployZecured: LOAN_TOKEN has no code");
        }
        require(IERC20Metadata(d.loanToken).decimals() == 6, "DeployZecured: the loan token is not 6 decimals");

        address baseOracle = vm.envOr("BASE_ORACLE", address(0));
        if (baseOracle == address(0)) {
            uint256 priceCents = vm.envOr("ZEC_USD_PRICE_CENTS", uint256(126_500));
            baseOracle = address(new MockZecOracle(d.deployer, priceCents * 1e32)); // cents * 1e32 = USD * 1e34
            d.baseOracleIsMock = true;
        } else {
            require(baseOracle.code.length > 0, "DeployZecured: BASE_ORACLE has no code");
        }
        d.baseOracle = IOracle(baseOracle);
        require(d.baseOracle.price() != 0, "DeployZecured: the base oracle has no price");
        d.oracle = new DemoShockOracle(d.deployer, d.baseOracle);
    }

    /// ZZEC needs the Registry and the Adapter, the Registry needs ZZEC and the Adapter, the Adapter needs the
    /// Registry: all three are created back to back at the deployer's next nonces, whose addresses are known upfront.
    function _core(Deployment memory d, address[5] memory signerSet) internal {
        _depositProofs(d);
        _tier3Config(d);
        if (d.theftDemo) _requireOwnSigners(signerSet);
        uint64 nonce = vm.getNonce(d.deployer);
        address predictedRegistry = vm.computeCreateAddress(d.deployer, nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(d.deployer, nonce + 2);
        // Tier 3: the bond and the slasher right after the Adapter.
        address predictedBond =
            d.violationImageId == bytes32(0) ? address(0) : vm.computeCreateAddress(d.deployer, nonce + 3);
        address predictedSlasher = vm.computeCreateAddress(d.deployer, nonce + 4);

        d.zzec = new ZZEC(predictedRegistry, predictedAdapter, address(MORPHO));
        d.registry = new ZecuredRegistry(
            MORPHO,
            d.zzec,
            predictedAdapter,
            d.loanToken,
            address(d.oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            d.proofs,
            ISignerBond(predictedBond)
        );
        d.adapter = new SettlementAdapter(d.registry);
        require(address(d.registry) == predictedRegistry, "DeployZecured: registry address mismatch");
        require(address(d.adapter) == predictedAdapter, "DeployZecured: adapter address mismatch");
        if (predictedBond != address(0)) {
            d.bond = new SignerBond(address(d.registry), predictedSlasher, d.challengeWindow);
            // The slasher's constructor checks that the Registry, the bond and itself name each other.
            d.slasher = new ZecuredSlasher(
                address(d.registry),
                address(d.bond),
                address(d.proofs.relay),
                address(d.proofs.verifier),
                d.violationImageId,
                d.proofs.sealSelector,
                d.slashConfirmationWork,
                d.challengeWindow
            );
            require(address(d.bond) == predictedBond, "DeployZecured: bond address mismatch");
            require(address(d.slasher) == predictedSlasher, "DeployZecured: slasher address mismatch");
        }
        d.marketId = d.registry.marketId();
        require(MORPHO.market(d.marketId).lastUpdate != 0, "DeployZecured: market not created");
    }

    /// Tier 2 configuration (before the core contracts: a MOCK verifier would shift their predicted addresses).
    function _depositProofs(Deployment memory d) internal {
        bytes32 imageId = vm.envOr("DEPOSIT_IMAGE_ID", bytes32(0));
        bool localFork = keccak256(bytes(vm.envOr("DEPLOYMENT_NAME", string("")))) == keccak256("anvil");
        if (imageId == bytes32(0)) {
            // Signer attestations only: the weakest mint path, never by accident outside the local fork (K1).
            require(
                localFork || vm.envOr("ALLOW_SIGNER_MINT", false),
                "DeployZecured: no DEPOSIT_IMAGE_ID: signer-attested minting needs ALLOW_SIGNER_MINT=true"
            );
            return;
        }
        address relay = vm.envAddress("ZCASH_RELAY");
        require(relay.code.length > 0, "DeployZecured: ZCASH_RELAY has no code");
        (bytes32 tip,,) = IZcashHeaderRelay(relay).bestChainTip();
        require(tip != bytes32(0), "DeployZecured: ZCASH_RELAY is not a header relay");
        address verifier;
        bytes4 selector;
        if (vm.envOr("RISC0_VERIFIER_MOCK", false)) {
            require(localFork, "DeployZecured: the MOCK verifier is for the local fork only (DEPLOYMENT_NAME=anvil)");
            verifier = address(new MockRiscZeroVerifier());
            d.verifierIsMock = true;
            selector = MockRiscZeroVerifier(verifier).SELECTOR();
        } else {
            verifier = vm.envOr("RISC0_VERIFIER", RISC0_ROUTER);
            require(verifier.code.length > 0, "DeployZecured: RISC0_VERIFIER has no code");
            bytes memory raw = vm.envOr("DEPOSIT_SEAL_SELECTOR", abi.encodePacked(GROTH16_SELECTOR));
            require(raw.length == 4, "DeployZecured: DEPOSIT_SEAL_SELECTOR must be 4 bytes");
            // forge-lint: disable-next-line(unsafe-typecast)
            selector = bytes4(raw); // checked: exactly four bytes
            // Y1: the router must route the pinned selector to a verifier now (it reverts for unknown or removed ones).
            if (verifier == RISC0_ROUTER) {
                require(
                    IRiscZeroVerifierRouter(verifier).getVerifier(selector) != address(0),
                    "DeployZecured: the router has no verifier for DEPOSIT_SEAL_SELECTOR"
                );
            }
        }
        MintMode mode = _mintMode(localFork);
        uint256 minWork = vm.envOr("MIN_CONFIRMATION_WORK", DEFAULT_MIN_CONFIRMATION_WORK);
        require(
            localFork || minWork >= FLOOR_MIN_CONFIRMATION_WORK, "DeployZecured: MIN_CONFIRMATION_WORK below the floor"
        );
        d.proofs = DepositProofConfig({
            verifier: IRiscZeroVerifier(verifier),
            imageId: imageId,
            relay: IZcashHeaderRelay(relay),
            minConfirmationWork: minWork,
            mintMode: mode,
            sealSelector: selector,
            finalizeDepth: uint64(vm.envOr("FINALIZE_DEPTH", uint256(DEFAULT_FINALIZE_DEPTH))),
            finalizeDelay: uint64(vm.envOr("FINALIZE_DELAY", uint256(DEFAULT_FINALIZE_DELAY)))
        });
    }

    /// Tier 3 configuration (docs/TIER3.md § 8). The theft demo is its own deployment (T3-6): only there may the
    /// challenge window be shorter than a day.
    function _tier3Config(Deployment memory d) internal view {
        d.violationImageId = vm.envOr("VIOLATION_IMAGE_ID", bytes32(0));
        string memory name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        bool localFork = keccak256(bytes(name)) == keccak256("anvil");
        d.theftDemo = vm.envOr("THEFT_DEMO", false);
        if (d.violationImageId == bytes32(0)) {
            require(!d.theftDemo, "DeployZecured: THEFT_DEMO needs VIOLATION_IMAGE_ID");
            return;
        }
        require(address(d.proofs.verifier) != address(0), "DeployZecured: VIOLATION_IMAGE_ID needs DEPOSIT_IMAGE_ID");
        if (d.theftDemo) {
            require(
                vm.contains(name, "theft-demo"), "DeployZecured: THEFT_DEMO needs a DEPLOYMENT_NAME with theft-demo"
            );
        }
        d.challengeWindow =
            uint64(vm.envOr("CHALLENGE_WINDOW", uint256(d.theftDemo ? DEMO_CHALLENGE_WINDOW : JUDGE_CHALLENGE_WINDOW)));
        require(
            d.theftDemo || localFork || d.challengeWindow >= JUDGE_CHALLENGE_WINDOW,
            "DeployZecured: a challenge window under a day is for the theft demo only (T3-6)"
        );
        d.slashConfirmationWork = vm.envOr("SLASH_CONFIRMATION_WORK", DEFAULT_SLASH_CONFIRMATION_WORK);
        require(
            localFork || d.slashConfirmationWork >= FLOOR_SLASH_CONFIRMATION_WORK,
            "DeployZecured: SLASH_CONFIRMATION_WORK below the floor"
        );
    }

    /// K1: proof-only unless signer attestations are asked for explicitly (and allowed outside the local fork).
    function _mintMode(bool localFork) internal view returns (MintMode) {
        bytes32 mode = keccak256(bytes(vm.envOr("MINT_MODE", string("proof-only"))));
        if (mode == keccak256("proof-only")) return MintMode.ProofOnly;
        require(mode == keccak256("signer-attested"), "DeployZecured: MINT_MODE is proof-only or signer-attested");
        require(
            localFork || vm.envOr("ALLOW_SIGNER_MINT", false),
            "DeployZecured: MINT_MODE=signer-attested needs ALLOW_SIGNER_MINT=true"
        );
        return MintMode.SignerAttested;
    }

    function _seedLender(Deployment memory d) internal {
        MarketParams memory params = d.registry.marketParams();
        if (d.loanTokenIsMock) {
            require(MockUSDC(d.loanToken).owner() == d.deployer, "DeployZecured: the deployer does not own MockUSDC");
            MockUSDC(d.loanToken).mint(d.deployer, d.lenderSeed);
        } else {
            require(
                IERC20(d.loanToken).balanceOf(d.deployer) >= d.lenderSeed, "DeployZecured: the deployer lacks the USDC"
            );
        }
        IERC20(d.loanToken).approve(address(MORPHO), d.lenderSeed);
        MORPHO.supply(params, d.lenderSeed, 0, d.deployer, "");
    }

    /// T3-6: the theft demo runs on its own five keys. Its signer set never shares an address with the judge
    /// network's, which THEFT_DEMO requires in JUDGE_SIGNER_ADDRESSES (the judge's SIGNER_ADDRESSES).
    function _requireOwnSigners(address[5] memory signerSet) internal view {
        address[] memory judge = vm.envOr("JUDGE_SIGNER_ADDRESSES", ",", new address[](0));
        require(judge.length == 5, "DeployZecured: THEFT_DEMO needs the judge network's JUDGE_SIGNER_ADDRESSES");
        for (uint256 j; j < 5; ++j) {
            for (uint256 i; i < 5; ++i) {
                require(
                    signerSet[i] != judge[j], "DeployZecured: the theft demo shares a signer with the judge network"
                );
            }
        }
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
        console.log("loan token:", d.loanToken, d.loanTokenIsMock ? "(MockUSDC, MOCK)" : "");
        console.log("base oracle:", address(d.baseOracle), d.baseOracleIsMock ? "(MockZecOracle, MOCK)" : "");
        console.log("market oracle (DemoShockOracle, MOCK/DEMO):", address(d.oracle));
        console.log("ZZEC:", address(d.zzec));
        console.log("ZecuredRegistry:", address(d.registry));
        console.log("SettlementAdapter:", address(d.adapter));
        console.log("Morpho market id:");
        console.logBytes32(Id.unwrap(d.marketId));
        console.log("demo lender supply (USDC units):", d.lenderSeed);
        if (address(d.proofs.verifier) == address(0)) {
            console.log("deposit proofs: disabled; mint mode: signer-attested");
        } else {
            console.log("deposit proofs: verifier", address(d.proofs.verifier), d.verifierIsMock ? "(MOCK)" : "");
            console.log("deposit proofs: relay", address(d.proofs.relay));
            console.log("deposit proofs: min confirmation work", d.proofs.minConfirmationWork);
            console.log(
                "mint mode:", d.proofs.mintMode == MintMode.ProofOnly ? "proof-only (K1)" : "signer-attested and proofs"
            );
            console.log("finalize depth / delay (s):", d.proofs.finalizeDepth, d.proofs.finalizeDelay);
        }
        if (address(d.bond) == address(0)) {
            console.log("Tier 3: not deployed (no bonds: the Registry mints without them)");
        } else {
            console.log("Tier 3: SignerBond", address(d.bond), "ZecuredSlasher", address(d.slasher));
            console.log("Tier 3: challenge window (s)", d.challengeWindow, d.theftDemo ? "(THEFT DEMO)" : "");
            console.log("Tier 3: slash confirmation work", d.slashConfirmationWork);
            console.log("Tier 3: minting waits for all five bonds (script/BondSigners.s.sol)");
        }
    }

    /// What is a MOCK in this deployment, for README "What is real and what is a mock".
    function _mockNote(Deployment memory d) internal pure returns (string memory note) {
        note =
            "DemoShockOracle is a MOCK / DEMO control (owner-set shock on top of the base price, public ShockSet events)";
        if (d.loanTokenIsMock) note = string.concat(note, "; MockUSDC is a MOCK (owner-minted)");
        if (d.baseOracleIsMock) note = string.concat(note, "; MockZecOracle is a MOCK (owner-set price)");
        if (d.verifierIsMock) {
            note = string.concat(
                note, "; MockRiscZeroVerifier is a MOCK (accepts unproven deposit and violation journals)"
            );
        }
        if (d.theftDemo) {
            note = string.concat(note, "; THEFT DEMO deployment (T3-6): 2-minute challenge window, demo signer keys");
        }
    }

    function _writeJson(Deployment memory d, address[5] memory signerSet) internal {
        string memory contracts = "contracts";
        vm.serializeAddress(contracts, "ZZEC", address(d.zzec));
        vm.serializeAddress(contracts, "ZecuredRegistry", address(d.registry));
        vm.serializeAddress(contracts, "SettlementAdapter", address(d.adapter));
        vm.serializeAddress(contracts, "LoanToken", d.loanToken);
        if (d.loanTokenIsMock) vm.serializeAddress(contracts, "MockUSDC", d.loanToken);
        vm.serializeAddress(contracts, "BaseOracle", address(d.baseOracle));
        if (d.baseOracleIsMock) vm.serializeAddress(contracts, "MockZecOracle", address(d.baseOracle));
        if (address(d.bond) != address(0)) {
            vm.serializeAddress(contracts, "SignerBond", address(d.bond));
            vm.serializeAddress(contracts, "ZecuredSlasher", address(d.slasher));
        }
        contracts = vm.serializeAddress(contracts, "DemoShockOracle", address(d.oracle));

        string memory market = "market";
        vm.serializeAddress(market, "morphoBlue", address(MORPHO));
        vm.serializeBytes32(market, "marketId", Id.unwrap(d.marketId));
        vm.serializeAddress(market, "loanToken", d.loanToken);
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
        vm.serializeString(
            proofs, "mintMode", d.proofs.mintMode == MintMode.ProofOnly ? "proof-only" : "signer-attested"
        );
        vm.serializeBytes(proofs, "sealSelector", abi.encodePacked(d.proofs.sealSelector));
        vm.serializeUint(proofs, "finalizeDepth", d.proofs.finalizeDepth);
        vm.serializeUint(proofs, "finalizeDelay", d.proofs.finalizeDelay);
        proofs = vm.serializeString(proofs, "minConfirmationWork", vm.toString(d.proofs.minConfirmationWork));

        string memory tier3 = "tier3";
        vm.serializeBool(tier3, "enabled", address(d.bond) != address(0));
        vm.serializeBytes32(tier3, "violationImageId", d.violationImageId);
        vm.serializeUint(tier3, "challengeWindow", d.challengeWindow);
        vm.serializeString(tier3, "slashConfirmationWork", vm.toString(d.slashConfirmationWork));
        tier3 = vm.serializeBool(tier3, "theftDemo", d.theftDemo);

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
        vm.serializeString(root, "tier3", tier3);
        vm.serializeAddress(root, "signers", signerList);
        vm.serializeUint(root, "threshold", d.registry.THRESHOLD());
        vm.serializeUint(root, "minSeize", d.adapter.MIN_SEIZE());
        vm.serializeUint(root, "maxMarginBps", d.adapter.MAX_MARGIN_BPS());
        vm.serializeString(root, "oraclePrice", vm.toString(d.oracle.price())); // USD * 1e34, decimal string
        vm.serializeBool(root, "loanTokenIsMock", d.loanTokenIsMock);
        vm.serializeBool(root, "baseOracleIsMock", d.baseOracleIsMock);
        vm.serializeUint(root, "lenderSeedUsdcUnits", d.lenderSeed);
        string memory json = vm.serializeString(root, "mocks", _mockNote(d));

        vm.createDir(OUT_DIR, true);
        string memory name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        string memory path = string.concat(OUT_DIR, "/", name, d.dryRun ? ".dry-run.json" : ".json");
        vm.writeJson(json, path);
        console.log("written:", path);
    }
}
