// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredSpendLogic} from "../../src/registry/ZecuredSpendLogic.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {SignerBond} from "../../src/SignerBond.sol";
import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {CloseRequest, SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {VaultStatus, ClaimKind, ClaimStatus, RegistryConfig} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IntakeConfig, WorkTier} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {IntakeHarness} from "./IntakeHarness.sol";

/// @notice Shared fixture of the fork tests: a fork of Base Sepolia with the REAL Morpho Blue and AdaptiveCurveIRM, plus
///         freshly deployed MockUSDC + MockZecOracle (MOCK) and the production v3 contracts (ZZEC, ZecuredSpendLogic,
///         ZecuredRegistry, SettlementAdapter, the Intake, and with `_bonded()` a SignerBond and the ZecuredSlasher),
///         wired like script/DeployZecured.s.sol (predicted CREATE addresses). The Intake is `IntakeHarness`: the
///         production contract plus TEST-ONLY shortcuts that activate a vault without a proof (`_attest`).
/// @dev RPC: $BASE_SEPOLIA_RPC_URL, or https://sepolia.base.org when unset. Block: $ZECURED_FORK_BLOCK, default
///      47,948,000 (pinned so that Foundry caches the fork state on disk); 0 = latest.
abstract contract ZecuredFixture is Test {
    using MarketParamsLib for MarketParams;
    using MorphoBalancesLib for IMorpho;

    // ------------------------------------------------------------------ Base Sepolia (docs/ref/FACTS.md)

    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    uint256 internal constant BASE_SEPOLIA_CHAIN_ID = 84532;
    string internal constant DEFAULT_RPC_URL = "https://sepolia.base.org";
    uint256 internal constant DEFAULT_FORK_BLOCK = 47_948_000;

    // ------------------------------------------------------------------ amounts

    uint64 internal constant VAULT_001 = 1_000_000; // 0.01 ZEC (docs/DECISIONS.md D7)
    uint64 internal constant VAULT_005 = 5_000_000; // 0.05 ZEC (D7)
    uint256 internal constant USDC = 1e6;
    uint256 internal constant PRICE_40 = 40e34; // $40.00 (Morpho scale: USD-per-ZEC * 1e34)
    uint256 internal constant PRICE_25_50 = 2550e32; // $25.50: a 40 % LTV position reaches 62.75 % > LLTV
    uint256 internal constant LENDER_SUPPLY = 1_000_000 * USDC;
    uint256 internal constant MIN_SEIZE = 100_000;

    bytes32 internal constant USER_COMMITMENT = keccak256("zecured.test.commitment.user");
    bytes32 internal constant MM_COMMITMENT = keccak256("zecured.test.commitment.marketMaker");
    bytes32 internal constant LIQUIDATOR_COMMITMENT = keccak256("zecured.test.commitment.liquidator");
    bytes32 internal constant RELEASE_COMMITMENT = keccak256("zecured.test.commitment.release");

    // ------------------------------------------------------------------ deposit proofs and Tier 3 (defaults)

    bytes32 internal constant DEFAULT_DEPOSIT_IMAGE = keccak256("zecured.test.depositImage");
    bytes32 internal constant VIOLATION_IMAGE = keccak256("tier3.violationImage");
    /// docs/TIER3.md § 4 / D26: the deposit-proof parameters of the deployments.
    uint256 internal constant MIN_WORK = 3_500_000;
    uint64 internal constant FINALIZE_DEPTH = 6;
    uint64 internal constant FINALIZE_DELAY = 150;
    /// docs/TIER3.md § 8: >= 100 normal Testnet blocks of work (~3.8e7), i.e. ~380,000 per block.
    uint256 internal constant SLASH_WORK = 38_000_000;
    uint256 internal constant WORK_PER_BLOCK = 380_000;
    uint64 internal constant WINDOW = 1 days;
    uint64 internal constant MAX_PAUSE = 30 days;
    uint64 internal constant PENDING_REFUND_DELAY = 1 days;
    uint64 internal constant MIN_REFUND = 20_000;
    uint64 internal constant ABANDON_PERIOD = 30 days;

    /// @notice The Intake's deposit-proof setup (`_proofSetup`).
    struct ProofSetup {
        address relay;
        address verifier;
        bytes32 imageId;
        bytes4 sealSelector;
        uint256 minWork;
        uint64 finalizeDepth;
        uint64 finalizeDelay;
    }

    /// @notice T-01 knobs of the Intake (`_capSetup`); all zero: no coverage cap.
    struct CapSetup {
        uint16 coverageBps;
        uint64 maxTotalZat;
        uint64 capPriceMaxAge;
        address capOracle;
    }

    /// @notice T-06 knobs of the Registry (`_feeSetup`); all zero: no opening fee.
    struct FeeSetup {
        uint16 feeBps;
        uint64 minFeeZat;
        bytes32 feeCommitment;
    }

    // ------------------------------------------------------------------ system

    MockUSDC internal usdc;
    MockZecOracle internal oracle;
    ZZEC internal zzec;
    ZecuredSpendLogic internal spendLogic;
    ZecuredRegistry internal registry;
    SettlementAdapter internal adapter;
    IntakeHarness internal intake;
    /// With `_bonded()` only.
    SignerBond internal bond;
    ZecuredSlasher internal slasher;
    /// The Intake's relay and verifier (MOCKs unless a suite's `_proofSetup` says otherwise).
    IZcashHeaderRelay internal headerRelay;
    IRiscZeroVerifier internal proofVerifier;
    ProofSetup internal proofs;
    /// The compensator the Registry names (`_compensator`).
    address internal compensatorAddress;
    MarketParams internal params;
    Id internal marketId;

    /// Signer keys and addresses, sorted by address (index order = the required signature order).
    uint256[5] internal signerKeys;
    address[5] internal signerSet;

    address internal lender = makeAddr("zecured.lender");
    address internal liquidator = makeAddr("zecured.liquidator");
    address internal marketMaker = makeAddr("zecured.marketMaker");
    address internal stranger = makeAddr("zecured.stranger");
    address internal borrowReceiver = makeAddr("zecured.borrowReceiver");

    function setUp() public virtual {
        _selectBaseSepoliaFork();
        _createSigners();
        _deploySystem();
        _supplyAsLender(LENDER_SUPPLY);
    }

    // ------------------------------------------------------------------ setup

    function _selectBaseSepoliaFork() internal {
        string memory rpcUrl = vm.envOr("BASE_SEPOLIA_RPC_URL", string(""));
        if (bytes(rpcUrl).length == 0) rpcUrl = DEFAULT_RPC_URL;
        uint256 forkBlock = vm.envOr("ZECURED_FORK_BLOCK", DEFAULT_FORK_BLOCK);
        if (forkBlock == 0) vm.createSelectFork(rpcUrl);
        else vm.createSelectFork(rpcUrl, forkBlock);
        assertEq(block.chainid, BASE_SEPOLIA_CHAIN_ID, "fork is not Base Sepolia");
        assertGt(address(MORPHO).code.length, 0, "Morpho Blue missing");
    }

    function _createSigners() internal {
        for (uint256 i; i < 5; ++i) {
            (signerSet[i], signerKeys[i]) = makeAddrAndKey(string.concat("zecured.signer.", vm.toString(i)));
        }
        // Insertion sort by address.
        for (uint256 i = 1; i < 5; ++i) {
            for (uint256 j = i; j > 0 && signerSet[j] < signerSet[j - 1]; --j) {
                (signerSet[j], signerSet[j - 1]) = (signerSet[j - 1], signerSet[j]);
                (signerKeys[j], signerKeys[j - 1]) = (signerKeys[j - 1], signerKeys[j]);
            }
        }
    }

    /// The Intake's deposit proofs: a MOCK relay and the MOCK verifier unless a suite overrides this (it may deploy
    /// contracts; it runs before the addresses are predicted).
    function _proofSetup() internal virtual returns (ProofSetup memory) {
        MockHeaderRelay mock = new MockHeaderRelay();
        MockRiscZeroVerifier mockVerifier = new MockRiscZeroVerifier();
        return ProofSetup({
            relay: address(mock),
            verifier: address(mockVerifier),
            imageId: DEFAULT_DEPOSIT_IMAGE,
            sealSelector: mockVerifier.SELECTOR(),
            minWork: MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    /// Tier 3: deploy a SignerBond and the ZecuredSlasher with the system (Tier3Base and some suites).
    function _bonded() internal virtual returns (bool) {
        return false;
    }

    /// The slasher's violation guest image id (a suite with real violation seals overrides it).
    function _violationImageId() internal virtual returns (bytes32) {
        return VIOLATION_IMAGE;
    }

    /// A MOCK named as both the bond and the slasher instead of deploying them (suites that drive their views by hand).
    function _mockBondAndSlasher() internal virtual returns (address) {}

    /// T-01: the coverage cap (off unless a suite turns it on).
    function _capSetup() internal virtual returns (CapSetup memory) {}

    /// T-06: the opening fee (none unless a suite sets one).
    function _feeSetup() internal virtual returns (FeeSetup memory) {}

    /// T-04: the compensator the Registry names (none unless a suite names one).
    function _compensator() internal virtual returns (address) {}

    /// T-08: the deposit-proof work tiers (one tier for every amount unless a suite sets more).
    function _workTiers() internal view virtual returns (WorkTier[] memory tiers) {
        tiers = new WorkTier[](1);
        tiers[0] = WorkTier({maxAmountZat: type(uint64).max, minWork: proofs.minWork});
    }

    /// T-09: whether the Intake reads journal v2, and the branch ids it allows (v1 unless a suite says otherwise).
    function _journalV2() internal view virtual returns (bool v2, uint32[4] memory branchIds) {}

    /// T-03: whether an open F1/F2 case pauses minting (off unless a suite turns it on).
    function _autoPauseOnCase() internal view virtual returns (bool) {}

    /// The addresses the system's contracts will have (they name each other as immutables).
    struct Predicted {
        address registry;
        address adapter;
        address intake;
        address bond;
        address slasher;
    }

    /// Same ordering as script/DeployZecured.s.sol: ZZEC, the spend logic, the Registry, the Adapter, the Intake, and
    /// (bonded) the SignerBond and the slasher, at the deployer's next nonces.
    function _deploySystem() internal {
        usdc = new MockUSDC(address(this));
        oracle = new MockZecOracle(address(this), PRICE_40);
        proofs = _proofSetup();
        headerRelay = IZcashHeaderRelay(proofs.relay);
        proofVerifier = IRiscZeroVerifier(proofs.verifier);
        CapSetup memory cap = _capSetup();
        FeeSetup memory fee = _feeSetup();
        bool bonded = _bonded();
        address mockBond = _mockBondAndSlasher();
        bytes32 violationImage = _violationImageId();
        compensatorAddress = _compensator();

        Predicted memory p = _predict(bonded, mockBond);
        zzec = new ZZEC(p.registry, p.adapter, address(MORPHO));
        spendLogic = new ZecuredSpendLogic(p.registry, signerSet);
        registry = new ZecuredRegistry(_registryConfig(p, fee));
        adapter = new SettlementAdapter(registry, MIN_SEIZE, 0, 500);
        intake = new IntakeHarness(_intakeConfig(p.bond, p.slasher, cap));
        if (bonded) _deployBondAndSlasher(p, violationImage);
        assertEq(address(registry), p.registry, "registry address prediction");
        assertEq(address(adapter), p.adapter, "adapter address prediction");
        assertEq(address(intake), p.intake, "intake address prediction");

        params = registry.marketParams();
        marketId = params.id();

        vm.label(address(MORPHO), "MorphoBlue");
        vm.label(ADAPTIVE_CURVE_IRM, "AdaptiveCurveIRM");
        vm.label(address(usdc), "MockUSDC");
        vm.label(address(oracle), "MockZecOracle");
        vm.label(address(zzec), "zZEC");
        vm.label(address(registry), "ZecuredRegistry");
        vm.label(address(adapter), "SettlementAdapter");
        vm.label(address(intake), "ZecuredIntake");
        vm.label(address(spendLogic), "ZecuredSpendLogic");
        for (uint256 i; i < 5; ++i) {
            vm.label(signerSet[i], string.concat("signer", vm.toString(i)));
        }
    }

    function _predict(bool bonded, address mockBond) internal view returns (Predicted memory p) {
        uint64 n = vm.getNonce(address(this));
        p.registry = vm.computeCreateAddress(address(this), n + 2);
        p.adapter = vm.computeCreateAddress(address(this), n + 3);
        p.intake = vm.computeCreateAddress(address(this), n + 4);
        p.bond = bonded ? vm.computeCreateAddress(address(this), n + 5) : mockBond;
        p.slasher = bonded ? vm.computeCreateAddress(address(this), n + 6) : mockBond;
    }

    function _registryConfig(Predicted memory p, FeeSetup memory fee) internal view returns (RegistryConfig memory) {
        return RegistryConfig({
            morpho: MORPHO,
            zzec: zzec,
            adapter: p.adapter,
            loanToken: address(usdc),
            oracle: address(oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV,
            signers: signerSet,
            spendLogic: address(spendLogic),
            intake: p.intake,
            signerBond: p.bond,
            slasher: p.slasher,
            compensator: compensatorAddress,
            feeBps: fee.feeBps,
            minFeeZat: fee.minFeeZat,
            feeCommitment: fee.feeCommitment,
            refundAbandonPeriod: ABANDON_PERIOD
        });
    }

    function _deployBondAndSlasher(Predicted memory p, bytes32 violationImage) internal {
        bond = new SignerBond(p.registry, p.slasher, WINDOW);
        slasher = new ZecuredSlasher(
            p.registry,
            p.bond,
            compensatorAddress,
            proofs.relay,
            proofs.verifier,
            violationImage,
            proofs.sealSelector,
            SLASH_WORK,
            WINDOW
        );
        assertEq(address(bond), p.bond, "bond address prediction");
        assertEq(address(slasher), p.slasher, "slasher address prediction");
    }

    function _intakeConfig(address bond_, address slasher_, CapSetup memory cap)
        internal
        view
        returns (IntakeConfig memory)
    {
        (bool v2, uint32[4] memory branchIds) = _journalV2();
        return IntakeConfig({
            registry: address(registry),
            signerBond: bond_,
            slasher: slasher_,
            verifier: IRiscZeroVerifier(proofs.verifier),
            imageId: proofs.imageId,
            sealSelector: proofs.sealSelector,
            journalV2: v2,
            allowedBranchIds: branchIds,
            relay: IZcashHeaderRelay(proofs.relay),
            workTiers: _workTiers(),
            finalizeDepth: proofs.finalizeDepth,
            finalizeDelay: proofs.finalizeDelay,
            coverageBps: cap.coverageBps,
            maxTotalZat: cap.maxTotalZat,
            capPriceMaxAge: cap.capPriceMaxAge,
            capOracle: cap.capOracle,
            maxPause: MAX_PAUSE,
            pendingRefundDelay: PENDING_REFUND_DELAY,
            autoPauseOnCase: _autoPauseOnCase(),
            minRefundZat: MIN_REFUND
        });
    }

    function _supplyAsLender(uint256 amount) internal {
        usdc.mint(lender, amount);
        vm.startPrank(lender);
        usdc.approve(address(MORPHO), amount);
        MORPHO.supply(params, amount, 0, lender, "");
        vm.stopPrank();
    }

    // ------------------------------------------------------------------ signatures

    function _sign(uint256 privateKey, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, digest);
        return abi.encodePacked(r, s, v);
    }

    /// Signatures of the signers at the given indices of the sorted set, in the given order.
    function _signerSigs(bytes32 digest, uint256[] memory indices) internal view returns (bytes[] memory sigs) {
        sigs = new bytes[](indices.length);
        for (uint256 i; i < indices.length; ++i) {
            sigs[i] = _sign(signerKeys[indices[i]], digest);
        }
    }

    function _idx(uint256 a, uint256 b) internal pure returns (uint256[] memory r) {
        r = new uint256[](2);
        (r[0], r[1]) = (a, b);
    }

    function _idx(uint256 a, uint256 b, uint256 c) internal pure returns (uint256[] memory r) {
        r = new uint256[](3);
        (r[0], r[1], r[2]) = (a, b, c);
    }

    /// A valid 3-of-5 set (signers 0, 2, 4 of the sorted set).
    function _threeSigs(bytes32 digest) internal view returns (bytes[] memory) {
        return _signerSigs(digest, _idx(0, 2, 4));
    }

    // ------------------------------------------------------------------ vaults

    function _vaultIdFor(address account) internal pure returns (bytes32) {
        return keccak256(abi.encode("zecured.test.vaultId", account));
    }

    function _depositTagFor(address account) internal pure returns (bytes32) {
        return keccak256(abi.encode("zecured.test.depositTag", account));
    }

    /// Opens and activates the vault of `account` without a proof (TEST-ONLY harness of the Intake).
    function _attest(address account, uint64 amount) internal returns (bytes32 vaultId) {
        vaultId = _vaultIdFor(account);
        intake.harnessActivate(vaultId, account, amount, _depositTagFor(account));
    }

    /// A deposit proof through the Intake with the fixture's default MOCK relay and MOCK verifier (v1 journal): the
    /// anchor gets the work the Intake needs, the five signers sign the vault's (stand-in) `VaultOpened`.
    function _mockProof(bytes32 vaultId, address account, uint64 amount, bytes32 tag, bytes32 anchor) internal {
        MockHeaderRelay(address(headerRelay)).setBlock(anchor, true, proofs.minWork);
        bytes32 opened = keccak256(abi.encode("zecured.test.opened", vaultId));
        bytes memory journal = abi.encode(anchor, vaultId, account, amount, tag, opened);
        bytes memory seal = MockRiscZeroVerifier(address(proofVerifier)).mockSeal(proofs.imageId, sha256(journal));
        uint256[] memory five = new uint256[](5);
        (five[0], five[1], five[2], five[3], five[4]) = (0, 1, 2, 3, 4);
        bytes[] memory sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), five);
        intake.attestDepositWithProof(journal, seal, sigs);
    }

    // ------------------------------------------------------------------ Tier 3 commitments

    /// A commitment of a payout of `claimIds` (claims of `vaultId`) from the lineage note `inputTag`, returning the
    /// rest to the vault as change (conservation). References and tags are derived from `salt`.
    function _spendCommit(bytes32 vaultId, bytes32 inputTag, bytes32[] memory claimIds, bytes32 salt)
        internal
        view
        returns (SpendCommit memory c)
    {
        uint64 claimed;
        bytes32[] memory refs = new bytes32[](claimIds.length);
        uint8[] memory outputs = new uint8[](claimIds.length);
        for (uint256 i; i < claimIds.length; ++i) {
            (,, uint64 amount,,) = registry.claim(claimIds[i]);
            claimed += amount;
            refs[i] = keccak256(abi.encode("zecured.test.claimRef", salt, i));
            outputs[i] = uint8(i); // claim i paid by action i
        }
        uint64 change = registry.lineageOf(inputTag).value - claimed;
        c = SpendCommit({
            vaultId: vaultId,
            inputTag: inputTag,
            claimIds: claimIds,
            claimRefs: refs,
            claimOutputs: outputs,
            payoutRef: keccak256(abi.encode("zecured.test.payoutRef", salt)),
            changeTag: change == 0 ? bytes32(0) : keccak256(abi.encode("zecured.test.changeTag", salt)),
            changeValue: change
        });
    }

    /// Commits the payout of `claimIds` from `inputTag` (signers 0, 2, 4); returns the commitment.
    function _commit(bytes32 vaultId, bytes32 inputTag, bytes32[] memory claimIds)
        internal
        returns (SpendCommit memory c)
    {
        c = _spendCommit(vaultId, inputTag, claimIds, keccak256(abi.encode(vaultId, inputTag, claimIds)));
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
    }

    /// The vault's current lineage note, as the tests' payouts moved it (zero: still the deposit).
    mapping(bytes32 vaultId => bytes32 tag) internal lineageHead;

    /// Commits the next payout of `vaultId`, paying `claimIds` from its current lineage note (the deposit of a vault
    /// attested with `_attest`, then each payout's change), as the signers would before signing it.
    function _commitNext(bytes32 vaultId, bytes32[] memory claimIds) internal returns (SpendCommit memory c) {
        bytes32 head = lineageHead[vaultId];
        if (head == bytes32(0)) {
            (address account,,) = registry.vault(vaultId);
            head = _depositTagFor(account);
        }
        c = _commit(vaultId, head, claimIds);
        lineageHead[vaultId] = c.changeTag;
    }

    /// The account's first UserOp: authorize Registry and Adapter on Morpho.
    function _authorizeBoth(address account) internal {
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        MORPHO.setAuthorization(address(adapter), true);
        vm.stopPrank();
    }

    function _borrow(address account, uint256 assets) internal returns (uint256 shares) {
        vm.prank(account);
        (, shares) = MORPHO.borrow(params, assets, 0, account, borrowReceiver);
    }

    /// The account repays all its debt by shares (USDC minted to it first, exact approval).
    function _repayAll(address account) internal returns (uint256 repaid) {
        uint256 shares = MORPHO.position(marketId, account).borrowShares;
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        usdc.mint(account, debt);
        vm.startPrank(account);
        usdc.approve(address(MORPHO), debt);
        (repaid,) = MORPHO.repay(params, 0, shares, account, "");
        vm.stopPrank();
    }

    function _fundAndApproveAdapter(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(address(adapter), amount);
    }

    // ------------------------------------------------------------------ close requests

    function _closeRequest(bytes32 vaultId, address account, uint16 marginBps, uint64 minUserAmount, uint256 nonce)
        internal
        view
        returns (CloseRequest memory)
    {
        return CloseRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: marginBps,
            minUserAmount: minUserAmount,
            userCommitment: USER_COMMITMENT,
            nonce: nonce,
            deadline: uint64(block.timestamp + 10 minutes)
        });
    }

    /// ECDSA signature of an EOA account over the CloseRequest digest (SignatureChecker's EOA path).
    function _signClose(uint256 accountKey, CloseRequest memory req) internal view returns (bytes memory) {
        return _sign(accountKey, adapter.hashCloseRequest(req));
    }

    /// Independent restatement of docs/INTERFACES.md § 3.3:
    /// mmAmount = ceil(repaidAssets * (10_000 + marginBps) * 1e36 / (10_000 * price)).
    function _expectedMmAmount(uint256 repaidAssets, uint16 marginBps) internal view returns (uint256) {
        uint256 numerator = repaidAssets * (10_000 + uint256(marginBps)) * 1e36;
        uint256 denominator = 10_000 * oracle.price();
        return (numerator + denominator - 1) / denominator;
    }

    // ------------------------------------------------------------------ views and assertions

    function _position(address account) internal view returns (Position memory) {
        return MORPHO.position(marketId, account);
    }

    function _outstanding(bytes32 vaultId) internal view returns (uint64 outstanding) {
        (, outstanding,) = registry.vault(vaultId);
    }

    function _status(bytes32 vaultId) internal view returns (VaultStatus status) {
        (,, status) = registry.vault(vaultId);
    }

    function _assertClaim(
        bytes32 claimId,
        bytes32 vaultId,
        ClaimKind kind,
        uint64 amount,
        bytes32 commitment,
        ClaimStatus status
    ) internal view {
        (bytes32 cVault, ClaimKind cKind, uint64 cAmount, bytes32 cCommitment, ClaimStatus cStatus) =
            registry.claim(claimId);
        assertEq(cVault, vaultId, "claim vault");
        assertEq(uint8(cKind), uint8(kind), "claim kind");
        assertEq(cAmount, amount, "claim amount");
        assertEq(cCommitment, commitment, "claim commitment");
        assertEq(uint8(cStatus), uint8(status), "claim status");
    }

    /// I2 at rest: all zZEC is in Morpho; the Registry, the Adapter and every listed address hold none.
    function _assertZZECOnlyInMorpho(address[] memory accounts) internal view {
        assertEq(zzec.balanceOf(address(registry)), 0, "registry holds zZEC at rest");
        assertEq(zzec.balanceOf(address(adapter)), 0, "adapter holds zZEC at rest");
        assertEq(zzec.balanceOf(address(MORPHO)), zzec.totalSupply(), "zZEC outside Morpho");
        for (uint256 i; i < accounts.length; ++i) {
            assertEq(zzec.balanceOf(accounts[i]), 0, "an account holds zZEC");
        }
        (address recipient, uint256 maxAmount) = zzec.pendingExpectation();
        assertEq(recipient, address(0), "expectation left armed");
        assertEq(maxAmount, 0, "expectation left armed");
    }

    function _list(address a) internal pure returns (address[] memory r) {
        r = new address[](1);
        r[0] = a;
    }

    function _list(address a, address b) internal pure returns (address[] memory r) {
        r = new address[](2);
        (r[0], r[1]) = (a, b);
    }
}
