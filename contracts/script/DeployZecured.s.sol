// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../src/mocks/MockZecOracle.sol";
import {DemoShockOracle} from "../src/mocks/DemoShockOracle.sol";
import {DemoCapPrice} from "../src/mocks/DemoCapPrice.sol";
import {MockRiscZeroVerifier} from "../src/mocks/MockRiscZeroVerifier.sol";
import {ZZEC} from "../src/ZZEC.sol";
import {ZecuredRegistry} from "../src/ZecuredRegistry.sol";
import {ZecuredSpendLogic} from "../src/registry/ZecuredSpendLogic.sol";
import {SettlementAdapter} from "../src/SettlementAdapter.sol";
import {ZecuredIntake} from "../src/ZecuredIntake.sol";
import {SignerBondV2} from "../src/SignerBondV2.sol";
import {ZecuredSlasher} from "../src/ZecuredSlasher.sol";
import {ZecuredCompensator} from "../src/ZecuredCompensator.sol";
import {ZecuredVersions} from "../src/ZecuredVersions.sol";
import {Version, VersionKind} from "../src/interfaces/v3/IZecuredVersions.sol";
import {IRiscZeroVerifierRouter} from "../src/interfaces/IRiscZeroVerifierRouter.sol";
import {RegistryConfig} from "../src/interfaces/v3/IZecuredRegistry.sol";
import {IntakeConfig, WorkTier} from "../src/interfaces/v3/IZecuredIntake.sol";
import {BondConfig} from "../src/interfaces/v3/ISignerBond.sol";
import {IRiscZeroVerifier} from "../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay, ZCASH_TESTNET} from "../src/relay/IZcashHeaderRelay.sol";

/// @title DeployZecured (v3)
/// @notice Deploys Zecured v3 on the network a config file describes (`contracts/config/zecured-<name>.json`):
///         1. the market's assets: the loan token (the config's, or a fresh MockUSDC on a local fork), the ZEC/USD base
///            oracle (BASE_ORACLE, or a fresh MockZecOracle, MOCK) and, off mainnet, the DemoShockOracle (MOCK /
///            DEMO) around it, which is then the market's oracle (on Base mainnet BASE_ORACLE itself is, a
///            ZecuredPriceFeed, T-10); the coverage cap's price (CAP_PRICE, or a DemoCapPrice over the market
///            oracle, MOCK / DEMO, testnets only);
///         2. the deposit-proof verifier: RISC Zero's router (the selector it must route is checked), or the MOCK
///            verifier on a local fork only;
///         3. at the deployer's next nine CREATE addresses, predicted first because each contract names the others as
///            immutables and the constructors check the wiring: ZZEC, ZecuredSpendLogic, ZecuredRegistry (which
///            creates the zZEC / loan token / oracle / AdaptiveCurveIRM / LLTV market on Morpho Blue),
///            SettlementAdapter, ZecuredIntake, SignerBondV2, ZecuredSlasher, ZecuredCompensator, and
///            ZecuredVersions with the deposit and violation guests as its initial versions (T-07a);
///         4. optionally a demo lender supply;
///         5. writes deployments/<name>.json (broadcast) or deployments/<name>.dry-run.json (simulation): every address
///            and every parameter.
///         Minting needs a deposit proof and all five bonds (script/BondSigners.s.sol): there is no signer-attested
///         minting in v3.
/// @dev Environment (the repo-root .env is loaded by forge):
///        DEPLOYER_PRIVATE_KEY     deployer key (never logged)
///        SIGNER_ADDRESSES         the 5 signer addresses, comma-separated, strictly increasing
///        DEPLOYMENT_NAME          the network: base-sepolia (default), base-sepolia-theft-demo, anvil, ...
///        ZECURED_CONFIG           optional: the config file (default config/zecured-<DEPLOYMENT_NAME>.json)
///        DEPOSIT_IMAGE_ID         image id of the deposit guest (tier2/)
///        VIOLATION_IMAGE_ID       image id of the violation guest (tier2/)
///        ZCASH_RELAY              the ZcashHeaderRelay (script/DeployRelay.s.sol)
///        RISC0_VERIFIER           optional: the RISC Zero verifier (default: the config's router)
///        RISC0_VERIFIER_MOCK      true: a MockRiscZeroVerifier (MOCK, accepts unproven journals); local fork only
///        DEPOSIT_SEAL_SELECTOR    optional: the seal selector both proofs pin (Y1; default Groth16 v3.0 0x73c457ba)
///        LOAN_TOKEN               optional: overrides the config's loan token (6 decimals)
///        BASE_ORACLE              the ZEC/USD oracle (Morpho scale): the market's own on Base mainnet (required
///                                 there), else the one the DemoShockOracle wraps; default off mainnet a
///                                 fresh MockZecOracle (MOCK) at ZEC_USD_PRICE_CENTS (default 126500 = $1,265.00)
///        CAP_PRICE                optional: the coverage cap's IZecuredCapPrice; required outside testnets
///        FEE_COMMITMENT           with a fee in the config: the treasury's salted Ironwood commitment
///        LENDER_SEED_USDC         optional: whole USDC to supply as demo lender (default 0)
///        JUDGE_SIGNER_ADDRESSES   for the theft demo: the judge network's signers, none of which it may share
///      Dry run (simulation only): forge script script/DeployZecured.s.sol --rpc-url https://sepolia.base.org
///      Deployment: forge script script/DeployZecured.s.sol --rpc-url base_sepolia --broadcast
contract DeployZecured is Script {
    string internal constant OUT_DIR = "deployments";
    /// RISC Zero Groth16 verifier v3.0 selector (verifier parameters digest prefix) behind the router.
    bytes4 internal constant GROTH16_SELECTOR = 0x73c457ba;
    uint256 internal constant BASE_MAINNET = 8453;
    uint64 internal constant JUDGE_CHALLENGE_WINDOW = 1 days;

    /// One network's parameters: contracts/config/zecured-<name>.json.
    struct NetworkConfig {
        string name;
        /// The Zcash network the relay follows and the guests prove: "zcash-testnet" or "zcash-mainnet" (T-08).
        string zcashNetwork;
        uint256 chainId;
        bool localFork;
        bool theftDemo;
        IMorpho morpho;
        address irm;
        uint256 lltv;
        address loanToken;
        address riscZeroRouter;
        // Intake
        WorkTier[] workTiers;
        uint256 minWorkFloor;
        uint64 finalizeDepth;
        uint64 finalizeDelay;
        uint16 coverageBps;
        uint64 maxTotalZat;
        uint64 capPriceMaxAge;
        uint64 maxPause;
        uint64 pendingRefundDelay;
        bool autoPauseOnCase;
        uint64 minRefundZat;
        bool journalV2;
        uint32[4] branchIds;
        // Registry
        uint16 feeBps;
        uint64 minFeeZat;
        uint64 refundAbandonPeriod;
        // SettlementAdapter
        uint256 minSeize;
        uint16 minSeizeBps;
        uint16 maxMarginBps;
        // SignerBondV2
        uint256 minBond;
        uint64 unbondingPeriod;
        uint256 bountyBps;
        uint256 bountyCap;
        uint64 livenessPeriod;
        uint64 commitGrace;
        uint16 livenessPremiumBps;
        // ZecuredSlasher
        uint64 challengeWindow;
        uint256 slashConfirmationWork;
        uint256 slashWorkFloor;
        // ZecuredVersions (T-07a)
        uint64 versionDepositDelay;
        uint64 versionViolationDelay;
        uint64 versionEstopDelay;
        // ZecuredSpendLogic: disputes (T-05 (a))
        uint64 disputeWindow;
        uint64 inclusionWindow;
        // ZecuredCompensator
        uint64 incidentWindow;
        address leftoverSink;
    }

    /// What this run uses and creates.
    struct Deployment {
        address deployer;
        bool dryRun;
        address loanToken;
        bool loanTokenIsMock;
        IOracle baseOracle;
        bool baseOracleIsMock;
        IOracle oracle;
        bool oracleIsDemo;
        address capPrice;
        bool capPriceIsDemo;
        address verifier;
        bool verifierIsMock;
        bytes4 sealSelector;
        bytes32 depositImageId;
        bytes32 violationImageId;
        address relay;
        bytes32 feeCommitment;
        ZZEC zzec;
        ZecuredSpendLogic spendLogic;
        ZecuredRegistry registry;
        SettlementAdapter adapter;
        ZecuredIntake intake;
        SignerBondV2 bond;
        ZecuredSlasher slasher;
        ZecuredCompensator compensator;
        ZecuredVersions versions;
        Id marketId;
        uint256 lenderSeed; // USDC units (6 decimals)
    }

    /// The nine predicted addresses (deployment order).
    struct Predicted {
        address zzec;
        address spendLogic;
        address registry;
        address adapter;
        address intake;
        address bond;
        address slasher;
        address compensator;
        address versions;
    }

    function run() external returns (Deployment memory d) {
        NetworkConfig memory cfg = _readConfig();
        require(block.chainid == cfg.chainId, "DeployZecured: the RPC's chain is not the config's");
        require(cfg.morpho.isIrmEnabled(cfg.irm), "DeployZecured: IRM not enabled");
        require(cfg.morpho.isLltvEnabled(cfg.lltv), "DeployZecured: LLTV not enabled");
        _checkConfig(cfg);

        d.dryRun = !vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) && !vm.isContext(VmSafe.ForgeContext.ScriptResume);
        address[5] memory signerSet = _readSigners();
        if (cfg.theftDemo) _requireOwnSigners(signerSet);
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY"); // never logged
        d.deployer = vm.addr(deployerKey);
        d.lenderSeed = vm.envOr("LENDER_SEED_USDC", uint256(0)) * 1e6;
        d.depositImageId = vm.envBytes32("DEPOSIT_IMAGE_ID");
        d.violationImageId = vm.envBytes32("VIOLATION_IMAGE_ID");
        require(d.depositImageId != bytes32(0) && d.violationImageId != bytes32(0), "DeployZecured: zero image id");
        d.relay = vm.envAddress("ZCASH_RELAY");
        _checkRelay(d.relay, cfg);
        d.feeCommitment = cfg.feeBps == 0 ? bytes32(0) : vm.envBytes32("FEE_COMMITMENT");

        vm.startBroadcast(deployerKey);
        _marketAssets(cfg, d);
        _verifier(cfg, d);
        _core(cfg, d, signerSet);
        if (d.lenderSeed != 0) _seedLender(cfg, d);
        vm.stopBroadcast();

        _log(cfg, d);
        _writeJson(cfg, d, signerSet);
    }

    // ------------------------------------------------------------------ config

    function _configPath() internal view returns (string memory name, string memory path) {
        name = vm.envOr("DEPLOYMENT_NAME", string("base-sepolia"));
        path = vm.envOr("ZECURED_CONFIG", string.concat("config/zecured-", name, ".json"));
    }

    function _readConfig() internal view returns (NetworkConfig memory) {
        (string memory name, string memory path) = _configPath();
        return _readConfigAt(name, path);
    }

    function _readConfigAt(string memory name, string memory path) internal view returns (NetworkConfig memory c) {
        string memory json = vm.readFile(path);
        c.name = name;
        require(
            keccak256(bytes(vm.parseJsonString(json, ".network"))) == keccak256(bytes(name)),
            "DeployZecured: the config is for another DEPLOYMENT_NAME"
        );
        c.chainId = vm.parseJsonUint(json, ".chainId");
        c.zcashNetwork = vm.parseJsonString(json, ".zcashNetwork");
        c.localFork = vm.parseJsonBool(json, ".localFork");
        c.theftDemo = vm.parseJsonBool(json, ".theftDemo");
        c.morpho = IMorpho(vm.parseJsonAddress(json, ".morpho"));
        c.irm = vm.parseJsonAddress(json, ".irm");
        c.lltv = vm.parseJsonUint(json, ".lltv");
        c.loanToken = vm.envOr("LOAN_TOKEN", vm.parseJsonAddress(json, ".loanToken"));
        c.riscZeroRouter = vm.parseJsonAddress(json, ".riscZeroRouter");
        _readIntakeConfig(json, c);
        c.feeBps = uint16(vm.parseJsonUint(json, ".feeBps"));
        c.minFeeZat = uint64(vm.parseJsonUint(json, ".minFeeZat"));
        c.refundAbandonPeriod = uint64(vm.parseJsonUint(json, ".refundAbandonPeriod"));
        c.minSeize = vm.parseJsonUint(json, ".minSeize");
        c.minSeizeBps = uint16(vm.parseJsonUint(json, ".minSeizeBps"));
        c.maxMarginBps = uint16(vm.parseJsonUint(json, ".maxMarginBps"));
        c.minBond = vm.parseJsonUint(json, ".minBond");
        c.unbondingPeriod = uint64(vm.parseJsonUint(json, ".unbondingPeriod"));
        c.bountyBps = vm.parseJsonUint(json, ".bountyBps");
        c.bountyCap = vm.parseJsonUint(json, ".bountyCap");
        c.livenessPeriod = uint64(vm.parseJsonUint(json, ".livenessPeriod"));
        c.commitGrace = uint64(vm.parseJsonUint(json, ".commitGrace"));
        c.livenessPremiumBps = uint16(vm.parseJsonUint(json, ".livenessPremiumBps"));
        c.challengeWindow = uint64(vm.parseJsonUint(json, ".challengeWindow"));
        c.slashConfirmationWork = vm.parseJsonUint(json, ".slashConfirmationWork");
        c.slashWorkFloor = vm.parseJsonUint(json, ".slashWorkFloor");
        c.disputeWindow = uint64(vm.parseJsonUint(json, ".disputeWindow"));
        c.versionDepositDelay = uint64(vm.parseJsonUint(json, ".versionDepositDelay"));
        c.versionViolationDelay = uint64(vm.parseJsonUint(json, ".versionViolationDelay"));
        c.versionEstopDelay = uint64(vm.parseJsonUint(json, ".versionEstopDelay"));
        c.inclusionWindow = uint64(vm.parseJsonUint(json, ".inclusionWindow"));
        c.incidentWindow = uint64(vm.parseJsonUint(json, ".incidentWindow"));
        c.leftoverSink = vm.parseJsonAddress(json, ".leftoverSink");
    }

    function _readIntakeConfig(string memory json, NetworkConfig memory c) internal pure {
        uint256[] memory maxAmounts = vm.parseJsonUintArray(json, ".tierMaxAmountZat");
        uint256[] memory minWorks = vm.parseJsonUintArray(json, ".tierMinWork");
        require(maxAmounts.length == minWorks.length, "DeployZecured: work tiers: two arrays of one length");
        c.workTiers = new WorkTier[](maxAmounts.length);
        for (uint256 i; i < maxAmounts.length; ++i) {
            c.workTiers[i] = WorkTier({maxAmountZat: uint64(maxAmounts[i]), minWork: minWorks[i]});
        }
        c.minWorkFloor = vm.parseJsonUint(json, ".minWorkFloor");
        c.finalizeDepth = uint64(vm.parseJsonUint(json, ".finalizeDepth"));
        c.finalizeDelay = uint64(vm.parseJsonUint(json, ".finalizeDelay"));
        c.coverageBps = uint16(vm.parseJsonUint(json, ".coverageBps"));
        c.maxTotalZat = uint64(vm.parseJsonUint(json, ".maxTotalZat"));
        c.capPriceMaxAge = uint64(vm.parseJsonUint(json, ".capPriceMaxAge"));
        c.maxPause = uint64(vm.parseJsonUint(json, ".maxPause"));
        c.pendingRefundDelay = uint64(vm.parseJsonUint(json, ".pendingRefundDelay"));
        c.autoPauseOnCase = vm.parseJsonBool(json, ".autoPauseOnCase");
        c.minRefundZat = uint64(vm.parseJsonUint(json, ".minRefundZat"));
        c.journalV2 = vm.parseJsonBool(json, ".journalV2");
        uint256[] memory branches = vm.parseJsonUintArray(json, ".branchIds");
        require(branches.length <= 4, "DeployZecured: at most four branch ids");
        for (uint256 i; i < branches.length; ++i) {
            c.branchIds[i] = uint32(branches[i]);
        }
    }

    /// The rules the contracts cannot know: what a judge or mainnet deployment must never get wrong.
    function _checkConfig(NetworkConfig memory c) internal pure {
        require(c.coverageBps != 0, "DeployZecured: the coverage cap is on in every deployment (T-01)");
        for (uint256 i; i < c.workTiers.length; ++i) {
            require(c.workTiers[i].minWork >= c.minWorkFloor, "DeployZecured: a work tier below the floor");
        }
        require(c.slashConfirmationWork >= c.slashWorkFloor, "DeployZecured: slash confirmation work below the floor");
        if (!c.localFork) {
            require(c.minWorkFloor >= 1_000_000, "DeployZecured: the work floor is at least 1,000,000 off a local fork");
            require(c.slashWorkFloor >= 30_000_000, "DeployZecured: the slash floor is at least 30,000,000 (S4)");
            require(
                c.theftDemo || c.challengeWindow >= JUDGE_CHALLENGE_WINDOW,
                "DeployZecured: a challenge window under a day is for the theft demo only (T3-6)"
            );
        }
        require(!c.journalV2 || c.branchIds[0] != 0, "DeployZecured: journal v2 needs its branch ids");
        // A bond that starts to leave once the last vault settled is still slashable when a claim reopened by a dispute
        // falls due (IZecuredDisputes).
        require(
            uint256(c.disputeWindow) + c.inclusionWindow <= uint256(c.challengeWindow) + c.unbondingPeriod,
            "DeployZecured: the dispute and inclusion windows outlast the challenge window plus the unbonding"
        );
        require(
            c.versionDepositDelay != 0 && c.versionEstopDelay != 0 && c.versionEstopDelay <= c.versionViolationDelay,
            "DeployZecured: version delays (T-07a)"
        );
    }

    // ------------------------------------------------------------------ steps

    /// RQ-06: the loan token and the price. MarketParams are immutable, so this is fixed for the deployment.
    function _marketAssets(NetworkConfig memory c, Deployment memory d) internal {
        d.loanToken = c.loanToken;
        if (d.loanToken == address(0)) {
            require(c.localFork, "DeployZecured: a MockUSDC loan token on a local fork only");
            d.loanToken = address(new MockUSDC(d.deployer));
            d.loanTokenIsMock = true;
        } else {
            require(d.loanToken.code.length > 0, "DeployZecured: the loan token has no code");
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
        if (c.chainId == BASE_MAINNET) {
            // The market prices with the ZecuredPriceFeed itself (T-10): no demo control on mainnet.
            require(!d.baseOracleIsMock, "DeployZecured: mainnet needs BASE_ORACLE (a ZecuredPriceFeed, T-10)");
            d.oracle = d.baseOracle;
        } else {
            d.oracle = IOracle(address(new DemoShockOracle(d.deployer, d.baseOracle)));
            d.oracleIsDemo = true;
        }

        d.capPrice = vm.envOr("CAP_PRICE", address(0));
        if (d.capPrice == address(0)) {
            require(c.chainId != BASE_MAINNET, "DeployZecured: mainnet needs CAP_PRICE (a ZecuredPriceFeed, T-10)");
            d.capPrice = address(new DemoCapPrice(d.oracle));
            d.capPriceIsDemo = true;
        } else {
            require(d.capPrice.code.length > 0, "DeployZecured: CAP_PRICE has no code");
        }
    }

    /// The verifier both proof kinds use, and the seal selector they pin (Y1).
    function _verifier(NetworkConfig memory c, Deployment memory d) internal {
        if (vm.envOr("RISC0_VERIFIER_MOCK", false)) {
            require(c.localFork, "DeployZecured: the MOCK verifier is for the local fork only");
            MockRiscZeroVerifier mock = new MockRiscZeroVerifier();
            d.verifier = address(mock);
            d.verifierIsMock = true;
            d.sealSelector = mock.SELECTOR();
            return;
        }
        d.verifier = vm.envOr("RISC0_VERIFIER", c.riscZeroRouter);
        require(d.verifier.code.length > 0, "DeployZecured: the verifier has no code");
        bytes memory raw = vm.envOr("DEPOSIT_SEAL_SELECTOR", abi.encodePacked(GROTH16_SELECTOR));
        require(raw.length == 4, "DeployZecured: DEPOSIT_SEAL_SELECTOR must be 4 bytes");
        // forge-lint: disable-next-line(unsafe-typecast)
        d.sealSelector = bytes4(raw); // checked: exactly four bytes
        // Y1: the router must route the pinned selector to a verifier now (it reverts for unknown or removed ones).
        if (d.verifier == c.riscZeroRouter) {
            require(
                IRiscZeroVerifierRouter(d.verifier).getVerifier(d.sealSelector) != address(0),
                "DeployZecured: the router has no verifier for the seal selector"
            );
        }
    }

    /// The nine contracts at the deployer's next nonces; the constructors check that the others name them.
    function _core(NetworkConfig memory c, Deployment memory d, address[5] memory signerSet) internal {
        Predicted memory p = _predict(d.deployer);
        d.zzec = new ZZEC(p.registry, p.adapter, address(c.morpho));
        d.spendLogic = new ZecuredSpendLogic(p.registry, signerSet, c.disputeWindow, c.inclusionWindow);
        d.registry = new ZecuredRegistry(_registryConfig(c, d, p, signerSet));
        d.adapter = new SettlementAdapter(d.registry, c.minSeize, c.minSeizeBps, c.maxMarginBps);
        d.intake = new ZecuredIntake(_intakeConfig(c, d, p));
        d.bond = new SignerBondV2(_bondConfig(c, d, p));
        d.slasher = new ZecuredSlasher(
            p.registry,
            p.bond,
            p.compensator,
            d.relay,
            d.verifier,
            p.versions,
            c.slashConfirmationWork,
            c.challengeWindow
        );
        d.compensator = new ZecuredCompensator(
            p.registry, p.slasher, p.bond, IERC20(d.loanToken), c.incidentWindow, c.leftoverSink
        );
        d.versions = new ZecuredVersions(
            p.registry,
            d.verifier,
            c.versionDepositDelay,
            c.versionViolationDelay,
            c.versionEstopDelay,
            _initialVersions(c, d)
        );
        require(address(d.zzec) == p.zzec, "DeployZecured: zZEC address mismatch");
        require(address(d.spendLogic) == p.spendLogic, "DeployZecured: spend logic address mismatch");
        require(address(d.registry) == p.registry, "DeployZecured: registry address mismatch");
        require(address(d.adapter) == p.adapter, "DeployZecured: adapter address mismatch");
        require(address(d.intake) == p.intake, "DeployZecured: intake address mismatch");
        require(address(d.bond) == p.bond, "DeployZecured: bond address mismatch");
        require(address(d.slasher) == p.slasher, "DeployZecured: slasher address mismatch");
        require(address(d.compensator) == p.compensator, "DeployZecured: compensator address mismatch");
        require(address(d.versions) == p.versions, "DeployZecured: versions address mismatch");
        d.marketId = d.registry.marketId();
        require(c.morpho.market(d.marketId).lastUpdate != 0, "DeployZecured: market not created");
    }

    function _predict(address deployer) internal view returns (Predicted memory p) {
        uint64 n = vm.getNonce(deployer);
        p.zzec = vm.computeCreateAddress(deployer, n);
        p.spendLogic = vm.computeCreateAddress(deployer, n + 1);
        p.registry = vm.computeCreateAddress(deployer, n + 2);
        p.adapter = vm.computeCreateAddress(deployer, n + 3);
        p.intake = vm.computeCreateAddress(deployer, n + 4);
        p.bond = vm.computeCreateAddress(deployer, n + 5);
        p.slasher = vm.computeCreateAddress(deployer, n + 6);
        p.compensator = vm.computeCreateAddress(deployer, n + 7);
        p.versions = vm.computeCreateAddress(deployer, n + 8);
    }

    /// T-07a: the deposit guest (with the config's branch ids) and the violation guest, both through the pinned seal
    /// selector, active at once.
    function _initialVersions(NetworkConfig memory c, Deployment memory d)
        internal
        pure
        returns (Version[] memory initial)
    {
        uint32[4] memory none;
        initial = new Version[](2);
        initial[0] = Version({
            kind: VersionKind.Deposit,
            selector: d.sealSelector,
            imageId: d.depositImageId,
            branches: c.branchIds,
            activeAt: 0,
            removed: false
        });
        initial[1] = Version({
            kind: VersionKind.Violation,
            selector: d.sealSelector,
            imageId: d.violationImageId,
            branches: none,
            activeAt: 0,
            removed: false
        });
    }

    function _registryConfig(
        NetworkConfig memory c,
        Deployment memory d,
        Predicted memory p,
        address[5] memory signerSet
    ) internal pure returns (RegistryConfig memory) {
        return RegistryConfig({
            morpho: c.morpho,
            zzec: d.zzec,
            adapter: p.adapter,
            loanToken: d.loanToken,
            oracle: address(d.oracle),
            irm: c.irm,
            lltv: c.lltv,
            signers: signerSet,
            spendLogic: p.spendLogic,
            intake: p.intake,
            signerBond: p.bond,
            slasher: p.slasher,
            compensator: p.compensator,
            feeBps: c.feeBps,
            minFeeZat: c.minFeeZat,
            feeCommitment: d.feeCommitment,
            refundAbandonPeriod: c.refundAbandonPeriod
        });
    }

    function _intakeConfig(NetworkConfig memory c, Deployment memory d, Predicted memory p)
        internal
        pure
        returns (IntakeConfig memory)
    {
        return IntakeConfig({
            registry: p.registry,
            signerBond: p.bond,
            slasher: p.slasher,
            verifier: IRiscZeroVerifier(d.verifier),
            versions: p.versions,
            journalV2: c.journalV2,
            relay: IZcashHeaderRelay(d.relay),
            workTiers: c.workTiers,
            finalizeDepth: c.finalizeDepth,
            finalizeDelay: c.finalizeDelay,
            coverageBps: c.coverageBps,
            maxTotalZat: c.maxTotalZat,
            capPriceMaxAge: c.capPriceMaxAge,
            capOracle: d.capPrice,
            maxPause: c.maxPause,
            pendingRefundDelay: c.pendingRefundDelay,
            autoPauseOnCase: c.autoPauseOnCase,
            minRefundZat: c.minRefundZat
        });
    }

    function _bondConfig(NetworkConfig memory c, Deployment memory d, Predicted memory p)
        internal
        pure
        returns (BondConfig memory)
    {
        return BondConfig({
            registry: p.registry,
            slasher: p.slasher,
            intake: p.intake,
            compensator: p.compensator,
            usdc: IERC20(d.loanToken),
            oracle: IOracle(address(d.oracle)),
            minBond: c.minBond,
            unbondingPeriod: c.unbondingPeriod,
            challengeWindow: c.challengeWindow,
            bountyBps: c.bountyBps,
            bountyCap: c.bountyCap,
            livenessPeriod: c.livenessPeriod,
            commitGrace: c.commitGrace,
            livenessPremiumBps: c.livenessPremiumBps
        });
    }

    /// T-08: the relay follows the config's Zcash network. The first relay (deployed before the network parameters)
    /// has no `networkId`; it follows Testnet, so it is accepted for a Testnet deployment only.
    function _checkRelay(address relay, NetworkConfig memory c) internal view {
        require(relay.code.length > 0, "DeployZecured: ZCASH_RELAY has no code");
        (bytes32 tip,,) = IZcashHeaderRelay(relay).bestChainTip();
        require(tip != bytes32(0), "DeployZecured: ZCASH_RELAY is not a header relay");
        bytes32 wanted = keccak256(bytes(c.zcashNetwork));
        try IZcashHeaderRelay(relay).networkId() returns (bytes32 id) {
            require(id == wanted, "DeployZecured: ZCASH_RELAY follows another Zcash network");
        } catch {
            require(wanted == ZCASH_TESTNET, "DeployZecured: a relay without a network id follows Testnet only");
        }
    }

    function _seedLender(NetworkConfig memory c, Deployment memory d) internal {
        MarketParams memory params = d.registry.marketParams();
        if (d.loanTokenIsMock) {
            require(MockUSDC(d.loanToken).owner() == d.deployer, "DeployZecured: the deployer does not own MockUSDC");
            MockUSDC(d.loanToken).mint(d.deployer, d.lenderSeed);
        } else {
            require(
                IERC20(d.loanToken).balanceOf(d.deployer) >= d.lenderSeed, "DeployZecured: the deployer lacks the USDC"
            );
        }
        IERC20(d.loanToken).approve(address(c.morpho), d.lenderSeed);
        c.morpho.supply(params, d.lenderSeed, 0, d.deployer, "");
    }

    /// T3-6: the theft demo runs on its own five keys. Its signer set never shares an address with the judge
    /// network's, which it requires in JUDGE_SIGNER_ADDRESSES (the judge's SIGNER_ADDRESSES); its name says so.
    function _requireOwnSigners(address[5] memory signerSet) internal view {
        (string memory name,) = _configPath();
        require(vm.contains(name, "theft-demo"), "DeployZecured: a theft demo's DEPLOYMENT_NAME contains theft-demo");
        address[] memory judge = vm.envOr("JUDGE_SIGNER_ADDRESSES", ",", new address[](0));
        require(judge.length == 5, "DeployZecured: the theft demo needs the judge network's JUDGE_SIGNER_ADDRESSES");
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

    function _log(NetworkConfig memory c, Deployment memory d) internal pure {
        console.log(d.dryRun ? "DRY RUN (simulation, nothing broadcast)" : "BROADCAST");
        console.log("network:", c.name, c.theftDemo ? "(THEFT DEMO)" : "");
        console.log("deployer:", d.deployer);
        console.log("loan token:", d.loanToken, d.loanTokenIsMock ? "(MockUSDC, MOCK)" : "");
        console.log("base oracle:", address(d.baseOracle), d.baseOracleIsMock ? "(MockZecOracle, MOCK)" : "");
        console.log("market oracle:", address(d.oracle), d.oracleIsDemo ? "(DemoShockOracle, MOCK/DEMO)" : "");
        console.log("cap price:", d.capPrice, d.capPriceIsDemo ? "(DemoCapPrice, MOCK/DEMO)" : "");
        console.log("verifier:", d.verifier, d.verifierIsMock ? "(MOCK)" : "");
        console.log("ZZEC:", address(d.zzec));
        console.log("ZecuredSpendLogic:", address(d.spendLogic));
        console.log("ZecuredRegistry:", address(d.registry));
        console.log("SettlementAdapter:", address(d.adapter));
        console.log("ZecuredIntake:", address(d.intake));
        console.log("SignerBondV2:", address(d.bond));
        console.log("ZecuredSlasher:", address(d.slasher));
        console.log("ZecuredCompensator:", address(d.compensator));
        console.log("ZecuredVersions:", address(d.versions));
        console.log("Morpho market id:");
        console.logBytes32(Id.unwrap(d.marketId));
        console.log("demo lender supply (USDC units):", d.lenderSeed);
        console.log("minting waits for all five bonds (script/BondSigners.s.sol), minBond:", c.minBond);
    }

    /// What is a MOCK or DEMO in this deployment, for README "What is real and what is a mock".
    function _mockNote(NetworkConfig memory c, Deployment memory d) internal pure returns (string memory note) {
        if (d.oracleIsDemo) {
            note =
                "DemoShockOracle is a MOCK / DEMO control (owner-set shock on top of the base price, public ShockSet events)";
        }
        if (d.capPriceIsDemo) {
            note =
                string.concat(note, "; DemoCapPrice is a MOCK / DEMO (the cap reads the market oracle, always fresh)");
        }
        if (d.loanTokenIsMock) note = string.concat(note, "; MockUSDC is a MOCK (owner-minted)");
        if (d.baseOracleIsMock) note = string.concat(note, "; MockZecOracle is a MOCK (owner-set price)");
        if (d.verifierIsMock) {
            note = string.concat(
                note, "; MockRiscZeroVerifier is a MOCK (accepts unproven deposit and violation journals)"
            );
        }
        if (c.theftDemo) note = string.concat(note, "; THEFT DEMO deployment (T3-6): short windows, demo signer keys");
    }

    function _writeJson(NetworkConfig memory c, Deployment memory d, address[5] memory signerSet) internal {
        string memory root = "deployment";
        vm.serializeString(root, "network", c.name);
        vm.serializeUint(root, "chainId", block.chainid);
        vm.serializeBool(root, "dryRun", d.dryRun);
        vm.serializeUint(root, "block", block.number);
        vm.serializeUint(root, "timestamp", block.timestamp);
        vm.serializeAddress(root, "deployer", d.deployer);
        vm.serializeString(root, "contracts", _contractsJson(d));
        vm.serializeString(root, "market", _marketJson(c, d));
        vm.serializeString(root, "eip712", _eip712Json(d));
        vm.serializeString(root, "depositProofs", _proofsJson(c, d));
        vm.serializeString(root, "tier3", _tier3Json(c, d));
        vm.serializeString(root, "v3", _v3Json(c, d));
        address[] memory signerList = new address[](5);
        for (uint256 i; i < 5; ++i) {
            signerList[i] = signerSet[i];
        }
        vm.serializeAddress(root, "signers", signerList);
        vm.serializeUint(root, "threshold", d.registry.THRESHOLD());
        vm.serializeUint(root, "minSeize", d.adapter.MIN_SEIZE());
        vm.serializeUint(root, "maxMarginBps", d.adapter.MAX_MARGIN_BPS());
        vm.serializeString(root, "oraclePrice", vm.toString(d.oracle.price())); // USD * 1e34, decimal string
        vm.serializeBool(root, "loanTokenIsMock", d.loanTokenIsMock);
        vm.serializeBool(root, "baseOracleIsMock", d.baseOracleIsMock);
        vm.serializeUint(root, "lenderSeedUsdcUnits", d.lenderSeed);
        string memory json = vm.serializeString(root, "mocks", _mockNote(c, d));

        vm.createDir(OUT_DIR, true);
        string memory path = string.concat(OUT_DIR, "/", c.name, d.dryRun ? ".dry-run.json" : ".json");
        vm.writeJson(json, path);
        console.log("written:", path);
    }

    function _contractsJson(Deployment memory d) internal returns (string memory contracts) {
        contracts = "contracts";
        vm.serializeAddress(contracts, "ZZEC", address(d.zzec));
        vm.serializeAddress(contracts, "ZecuredSpendLogic", address(d.spendLogic));
        vm.serializeAddress(contracts, "ZecuredRegistry", address(d.registry));
        vm.serializeAddress(contracts, "SettlementAdapter", address(d.adapter));
        vm.serializeAddress(contracts, "ZecuredIntake", address(d.intake));
        vm.serializeAddress(contracts, "SignerBond", address(d.bond));
        vm.serializeAddress(contracts, "ZecuredSlasher", address(d.slasher));
        vm.serializeAddress(contracts, "ZecuredCompensator", address(d.compensator));
        vm.serializeAddress(contracts, "ZecuredVersions", address(d.versions));
        vm.serializeAddress(contracts, "CapPrice", d.capPrice);
        vm.serializeAddress(contracts, "LoanToken", d.loanToken);
        if (d.loanTokenIsMock) vm.serializeAddress(contracts, "MockUSDC", d.loanToken);
        vm.serializeAddress(contracts, "BaseOracle", address(d.baseOracle));
        if (d.baseOracleIsMock) vm.serializeAddress(contracts, "MockZecOracle", address(d.baseOracle));
        if (d.oracleIsDemo) vm.serializeAddress(contracts, "DemoShockOracle", address(d.oracle));
        contracts = vm.serializeAddress(contracts, "MarketOracle", address(d.oracle));
    }

    function _marketJson(NetworkConfig memory c, Deployment memory d) internal returns (string memory market) {
        market = "market";
        vm.serializeAddress(market, "morphoBlue", address(c.morpho));
        vm.serializeBytes32(market, "marketId", Id.unwrap(d.marketId));
        vm.serializeAddress(market, "loanToken", d.loanToken);
        vm.serializeAddress(market, "collateralToken", address(d.zzec));
        vm.serializeAddress(market, "oracle", address(d.oracle));
        vm.serializeAddress(market, "irm", c.irm);
        market = vm.serializeString(market, "lltv", vm.toString(c.lltv)); // decimal string: > 2^53
    }

    function _eip712Json(Deployment memory d) internal returns (string memory eip712) {
        eip712 = "eip712";
        vm.serializeString(eip712, "registryDomain", "ZecuredRegistry");
        vm.serializeString(eip712, "adapterDomain", "ZecuredSettlementAdapter");
        vm.serializeString(eip712, "intakeDomain", "ZecuredIntake");
        vm.serializeString(eip712, "versionsDomain", "ZecuredVersions");
        vm.serializeBytes32(eip712, "versionsDomainSeparator", d.versions.domainSeparator());
        vm.serializeString(eip712, "version", "1");
        vm.serializeBytes32(eip712, "registryDomainSeparator", d.registry.domainSeparator());
        vm.serializeBytes32(eip712, "intakeDomainSeparator", d.intake.domainSeparator());
        eip712 = vm.serializeBytes32(eip712, "adapterDomainSeparator", d.adapter.domainSeparator());
    }

    function _proofsJson(NetworkConfig memory c, Deployment memory d) internal returns (string memory proofs) {
        proofs = "depositProofs";
        vm.serializeAddress(proofs, "intake", address(d.intake));
        vm.serializeAddress(proofs, "verifier", d.verifier);
        vm.serializeAddress(proofs, "versions", address(d.versions));
        vm.serializeBool(proofs, "verifierIsMock", d.verifierIsMock);
        vm.serializeBytes32(proofs, "imageId", d.depositImageId);
        vm.serializeAddress(proofs, "relay", d.relay);
        vm.serializeString(proofs, "mintMode", "proof-only");
        vm.serializeBytes(proofs, "sealSelector", abi.encodePacked(d.sealSelector));
        vm.serializeUint(proofs, "finalizeDepth", c.finalizeDepth);
        vm.serializeUint(proofs, "finalizeDelay", c.finalizeDelay);
        vm.serializeBool(proofs, "journalV2", c.journalV2);
        uint256[] memory branches = new uint256[](4);
        // Decimal strings: a tier's bound can exceed 2^53, which JSON numbers lose in JavaScript.
        string[] memory tierMax = new string[](c.workTiers.length);
        string[] memory tierWork = new string[](c.workTiers.length);
        for (uint256 i; i < 4; ++i) {
            branches[i] = c.branchIds[i];
        }
        for (uint256 i; i < c.workTiers.length; ++i) {
            tierMax[i] = vm.toString(c.workTiers[i].maxAmountZat);
            tierWork[i] = vm.toString(c.workTiers[i].minWork);
        }
        vm.serializeUint(proofs, "branchIds", branches);
        vm.serializeString(proofs, "tierMaxAmountZat", tierMax);
        vm.serializeString(proofs, "tierMinWork", tierWork);
        // The first tier's work, as v2's single `minConfirmationWork` (decimal string).
        proofs = vm.serializeString(proofs, "minConfirmationWork", vm.toString(c.workTiers[0].minWork));
    }

    function _tier3Json(NetworkConfig memory c, Deployment memory d) internal returns (string memory tier3) {
        tier3 = "tier3";
        vm.serializeBool(tier3, "enabled", true);
        vm.serializeBytes32(tier3, "violationImageId", d.violationImageId);
        vm.serializeUint(tier3, "challengeWindow", c.challengeWindow);
        vm.serializeString(tier3, "slashConfirmationWork", vm.toString(c.slashConfirmationWork));
        tier3 = vm.serializeBool(tier3, "theftDemo", c.theftDemo);
    }

    /// The v3 parameters (design items T-01 to T-06).
    function _v3Json(NetworkConfig memory c, Deployment memory d) internal returns (string memory v3) {
        v3 = "v3";
        vm.serializeUint(v3, "coverageBps", c.coverageBps);
        vm.serializeUint(v3, "maxTotalZat", c.maxTotalZat);
        vm.serializeUint(v3, "capPriceMaxAge", c.capPriceMaxAge);
        vm.serializeUint(v3, "maxPause", c.maxPause);
        vm.serializeUint(v3, "pendingRefundDelay", c.pendingRefundDelay);
        vm.serializeBool(v3, "autoPauseOnCase", c.autoPauseOnCase);
        vm.serializeUint(v3, "minRefundZat", c.minRefundZat);
        vm.serializeUint(v3, "refundAbandonPeriod", c.refundAbandonPeriod);
        vm.serializeUint(v3, "feeBps", c.feeBps);
        vm.serializeUint(v3, "minFeeZat", c.minFeeZat);
        vm.serializeBytes32(v3, "feeCommitment", d.feeCommitment);
        vm.serializeUint(v3, "minSeizeBps", c.minSeizeBps);
        vm.serializeString(v3, "minBond", vm.toString(c.minBond));
        vm.serializeUint(v3, "unbondingPeriod", c.unbondingPeriod);
        vm.serializeUint(v3, "bountyBps", c.bountyBps);
        vm.serializeString(v3, "bountyCap", vm.toString(c.bountyCap));
        vm.serializeUint(v3, "livenessPeriod", c.livenessPeriod);
        vm.serializeUint(v3, "commitGrace", c.commitGrace);
        vm.serializeUint(v3, "livenessPremiumBps", c.livenessPremiumBps);
        vm.serializeUint(v3, "incidentWindow", c.incidentWindow);
        v3 = vm.serializeAddress(v3, "leftoverSink", c.leftoverSink);
    }
}
