// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredSignerErrors} from "../../src/interfaces/v3/IZecuredSignerErrors.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {EventsLib} from "morpho-blue/src/libraries/EventsLib.sol";
import {MorphoStorageLib} from "morpho-blue/src/libraries/periphery/MorphoStorageLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredSpendLogic} from "../../src/registry/ZecuredSpendLogic.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails,
    VaultDetails,
    RegistryConfig
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ISettlementAdapter} from "../../src/interfaces/ISettlementAdapter.sol";
import {SpendCommit, ZecuredEIP712} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice ZecuredRegistry v3 against the real Morpho Blue on a Base Sepolia fork: deployment wiring, the deposit hooks
///         of the Intake, refunds (T-02), the exits, payouts, and every revert path (I1, I3, I4, I5, I6, I9). The
///         bonds' and the compensator's hooks and the opening fee: RegistryV3Hooks.t.sol.
contract ZecuredRegistryTest is ZecuredFixture {
    using MorphoBalancesLib for IMorpho;

    uint256 internal constant SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    address internal account;
    uint256 internal accountKey;

    function setUp() public override {
        super.setUp();
        (account, accountKey) = makeAddrAndKey("zecured.registry.account");
    }

    // ================================================================== deployment

    function test_deployment_wiring() public view {
        assertEq(zzec.registry(), address(registry));
        assertEq(zzec.adapter(), address(adapter));
        assertEq(zzec.morpho(), address(MORPHO));
        assertEq(address(registry.morpho()), address(MORPHO));
        assertEq(address(registry.zzec()), address(zzec));
        assertEq(registry.adapter(), address(adapter));
        assertEq(address(adapter.registry()), address(registry));
        assertEq(address(adapter.morpho()), address(MORPHO));
        assertEq(address(adapter.zzec()), address(zzec));
        assertEq(address(adapter.loanToken()), address(usdc));
        assertEq(address(adapter.oracle()), address(oracle));
        assertEq(Id.unwrap(adapter.marketId()), Id.unwrap(marketId));
        assertEq(Id.unwrap(registry.marketId()), Id.unwrap(marketId));

        assertEq(registry.intake(), address(intake));
        assertEq(intake.registry(), address(registry));
        assertEq(registry.spendLogic(), address(spendLogic));
        assertEq(spendLogic.registry(), address(registry));
        assertEq(registry.signerBond(), address(0), "this suite deploys no bonds");
        assertEq(registry.slasher(), address(0));
        assertEq(registry.compensator(), address(0));
        assertEq(registry.feeBps(), 0);
        assertEq(registry.minFeeZat(), 0);
        assertEq(registry.feeCommitment(), bytes32(0));
        assertEq(registry.refundAbandonPeriod(), ABANDON_PERIOD);

        assertEq(registry.THRESHOLD(), 3);
        assertEq(adapter.MIN_SEIZE(), 100_000);
        assertEq(adapter.minSeizeBps(), 0);
        assertEq(adapter.MAX_MARGIN_BPS(), 500);
        address[5] memory logicSigners = spendLogic.signers();
        address[5] memory s = registry.signers();
        for (uint256 i; i < 5; ++i) {
            assertEq(s[i], signerSet[i]);
            assertEq(logicSigners[i], signerSet[i]);
            if (i > 0) assertGt(uint160(s[i]), uint160(s[i - 1]), "strictly increasing");
        }
    }

    function test_deployment_createsTheMarket() public view {
        assertEq(params.loanToken, address(usdc));
        assertEq(params.collateralToken, address(zzec));
        assertEq(params.oracle, address(oracle));
        assertEq(params.irm, ADAPTIVE_CURVE_IRM);
        assertEq(params.lltv, LLTV);
        MarketParams memory onchain = MORPHO.idToMarketParams(marketId);
        assertEq(onchain.collateralToken, address(zzec));
        assertEq(onchain.loanToken, address(usdc));
        assertEq(onchain.oracle, address(oracle));
        assertEq(onchain.irm, ADAPTIVE_CURVE_IRM);
        assertEq(onchain.lltv, LLTV);
        assertGt(MORPHO.market(marketId).lastUpdate, 0);
    }

    function test_deployment_eip712Domains() public view {
        (, string memory name, string memory version, uint256 chainId, address verifyingContract,,) =
            registry.eip712Domain();
        assertEq(name, "ZecuredRegistry");
        assertEq(version, "1");
        assertEq(chainId, 84532);
        assertEq(verifyingContract, address(registry));
        (, name, version, chainId, verifyingContract,,) = adapter.eip712Domain();
        assertEq(name, "ZecuredSettlementAdapter");
        assertEq(version, "1");
        assertEq(chainId, 84532);
        assertEq(verifyingContract, address(adapter));

        bytes32 domainTypehash =
            keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
        assertEq(
            registry.domainSeparator(),
            keccak256(
                abi.encode(domainTypehash, keccak256("ZecuredRegistry"), keccak256("1"), 84532, address(registry))
            )
        );
        assertEq(
            adapter.domainSeparator(),
            keccak256(
                abi.encode(
                    domainTypehash, keccak256("ZecuredSettlementAdapter"), keccak256("1"), 84532, address(adapter)
                )
            )
        );
    }

    /// The deployed Registry's digests equal the ones an independent EIP-712 implementation derives from wallet JSON.
    function test_deployedDigests_matchTypedDataJson() public view {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 claimId = registry.claimIdOf(vaultId, 0);
        bytes32 commitment = keccak256("zecured.registry.reimburse");
        string memory domain = string.concat(
            '"domain":{"name":"ZecuredRegistry","version":"1","chainId":84532,"verifyingContract":"',
            vm.toString(address(registry)),
            '"}'
        );
        string memory types =
            '{"types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],';
        string memory json = string.concat(
            types,
            '"ReimburseDestination":[{"name":"claimId","type":"bytes32"},{"name":"commitment","type":"bytes32"},{"name":"deadline","type":"uint64"}]},',
            '"primaryType":"ReimburseDestination",',
            domain,
            ',"message":{"claimId":"',
            vm.toString(claimId),
            '","commitment":"',
            vm.toString(commitment),
            '","deadline":"1800000000"}}'
        );
        assertEq(registry.hashReimburseDestination(claimId, commitment, 1_800_000_000), vm.eip712HashTypedData(json));

        bytes32[] memory ids = new bytes32[](2);
        ids[0] = claimId;
        ids[1] = registry.claimIdOf(vaultId, 1);
        json = string.concat(
            types,
            '"PayoutConfirmation":[{"name":"vaultId","type":"bytes32"},{"name":"claimIdsHash","type":"bytes32"}]},',
            '"primaryType":"PayoutConfirmation",',
            domain,
            ',"message":{"vaultId":"',
            vm.toString(vaultId),
            '","claimIdsHash":"',
            vm.toString(keccak256(abi.encodePacked(ids))),
            '"}}'
        );
        assertEq(registry.hashPayoutConfirmation(vaultId, ids), vm.eip712HashTypedData(json));
    }

    // ================================================================== constructor

    /// The fixture's Registry configuration around a zZEC, an Adapter address and a spend logic.
    function _config(IZZEC z, address adapter_, address logic) internal view returns (RegistryConfig memory) {
        return RegistryConfig({
            morpho: MORPHO,
            zzec: z,
            adapter: adapter_,
            loanToken: address(usdc),
            oracle: address(oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV,
            signers: signerSet,
            spendLogic: logic,
            intake: address(intake),
            signerBond: address(0),
            slasher: address(0),
            compensator: address(0),
            feeBps: 0,
            minFeeZat: 0,
            feeCommitment: bytes32(0),
            refundAbandonPeriod: ABANDON_PERIOD
        });
    }

    /// Deploys zZEC and a spend logic (with `logicSigners`) for the Registry this contract deploys right after them, its
    /// Adapter after it; returns that Registry's configuration.
    function _nextRegistry(address[5] memory logicSigners) internal returns (RegistryConfig memory) {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 2);
        address predictedAdapter = vm.computeCreateAddress(address(this), nonce + 3);
        ZZEC z = new ZZEC(predictedRegistry, predictedAdapter, address(MORPHO));
        ZecuredSpendLogic logic = new ZecuredSpendLogic(predictedRegistry, logicSigners);
        return _config(z, predictedAdapter, address(logic));
    }

    function _nextRegistry() internal returns (RegistryConfig memory) {
        return _nextRegistry(signerSet);
    }

    function test_constructor_reusesAMarketCreatedByAFrontRunner() public {
        RegistryConfig memory c = _nextRegistry();
        // Someone creates the market between the zZEC deployment and the Registry deployment.
        MarketParams memory p = MarketParams({
            loanToken: address(usdc),
            collateralToken: address(c.zzec),
            oracle: address(oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV
        });
        vm.prank(stranger);
        MORPHO.createMarket(p);
        ZecuredRegistry registry2 = new ZecuredRegistry(c);
        SettlementAdapter adapter2 = new SettlementAdapter(registry2, MIN_SEIZE, 0, 500);
        assertEq(address(registry2), c.zzec.registry());
        assertEq(address(adapter2), c.adapter);
        assertEq(Id.unwrap(registry2.marketId()), Id.unwrap(adapter2.marketId()));
    }

    function test_constructor_revertsOnZeroAddress() public {
        // The zero checks come first: the rest of the configuration does not matter.
        RegistryConfig memory c = _nextRegistry();
        c.morpho = IMorpho(address(0));
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c.morpho = MORPHO;
        c.zzec = IZZEC(address(0));
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.adapter = address(0);
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.loanToken = address(0);
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.oracle = address(0);
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.intake = address(0);
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.spendLogic = address(0);
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(c);
    }

    function test_constructor_revertsWhenZZECIsWiredElsewhere() public {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);

        // zZEC built for another registry. (The zZEC check comes before the spend logic's.)
        ZZEC wrongRegistry = new ZZEC(stranger, predictedAdapter, address(MORPHO));
        RegistryConfig memory c = _config(wrongRegistry, predictedAdapter, address(spendLogic));
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(c);

        nonce = vm.getNonce(address(this));
        predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        // zZEC built for another adapter.
        ZZEC wrongAdapter = new ZZEC(predictedRegistry, stranger, address(MORPHO));
        c = _config(wrongAdapter, predictedAdapter, address(spendLogic));
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(c);

        nonce = vm.getNonce(address(this));
        predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);
        // zZEC built for another Morpho.
        ZZEC wrongMorpho = new ZZEC(predictedRegistry, predictedAdapter, stranger);
        c = _config(wrongMorpho, predictedAdapter, address(spendLogic));
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(c);
    }

    /// T-11: `commitSpend` runs the spend logic's code on the Registry's storage, so the logic must have been built for
    /// this very Registry and its signer set.
    function test_constructor_revertsOnASpendLogicOfAnotherRegistryOrSignerSet() public {
        RegistryConfig memory c = _nextRegistry();
        c.spendLogic = address(spendLogic); // built for the fixture's Registry
        vm.expectRevert(IZecuredRegistry.SpendLogicMismatch.selector);
        new ZecuredRegistry(c);

        address[5] memory others;
        for (uint256 i; i < 5; ++i) {
            others[i] = address(uint160(0x1000 + i));
        }
        c = _nextRegistry(others);
        vm.expectRevert(IZecuredRegistry.SpendLogicMismatch.selector);
        new ZecuredRegistry(c);
        // One signer differs.
        others = signerSet;
        others[4] = address(uint160(signerSet[4]) + 1);
        c = _nextRegistry(others);
        vm.expectRevert(IZecuredRegistry.SpendLogicMismatch.selector);
        new ZecuredRegistry(c);
    }

    /// Called directly (not by delegatecall from its Registry), the spend logic does nothing.
    function test_spendLogic_refusesDirectCalls() public {
        SpendCommit memory c;
        vm.expectRevert(ZecuredSpendLogic.NotDelegated.selector);
        spendLogic.commitSpend(c, new bytes[](0));
    }

    /// T-06: a fee needs a treasury (and a treasury a fee), at most 10 %, and a waiver floor above dust and above the
    /// output it bears; T-02: refunds can be abandoned only after a positive period.
    function test_constructor_revertsOnInvalidFeeOrRefundConfig() public {
        bytes32 treasury = keccak256("zecured.registry.treasury");
        RegistryConfig memory c = _nextRegistry();
        (c.feeBps, c.minFeeZat, c.feeCommitment) = (1_001, 20_000, treasury);
        vm.expectRevert(IZecuredRegistry.InvalidFeeConfig.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        (c.feeBps, c.minFeeZat) = (50, 20_000);
        vm.expectRevert(IZecuredRegistry.InvalidFeeConfig.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.feeCommitment = treasury;
        vm.expectRevert(IZecuredRegistry.InvalidFeeConfig.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        (c.feeBps, c.minFeeZat, c.feeCommitment) = (50, 10_000, treasury); // at DUST_LIMIT
        vm.expectRevert(IZecuredRegistry.InvalidFeeConfig.selector);
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.refundAbandonPeriod = 0;
        vm.expectRevert(IZecuredRegistry.InvalidFeeConfig.selector);
        new ZecuredRegistry(c);

        // The bounds themselves are fine.
        c = _nextRegistry();
        (c.feeBps, c.minFeeZat, c.feeCommitment, c.refundAbandonPeriod) = (1_000, 10_001, treasury, 1);
        ZecuredRegistry r = new ZecuredRegistry(c);
        assertEq(r.feeBps(), 1_000);
        assertEq(r.minFeeZat(), 10_001);
        assertEq(r.feeCommitment(), treasury);
        assertEq(r.refundAbandonPeriod(), 1);
    }

    function test_constructor_revertsOnInvalidSignerSet() public {
        address[5] memory unsorted = signerSet;
        (unsorted[1], unsorted[2]) = (unsorted[2], unsorted[1]);
        address[5] memory duplicate = signerSet;
        duplicate[3] = duplicate[2];
        address[5] memory zeroFirst = signerSet;
        zeroFirst[0] = address(0);
        address[5][3] memory bad = [unsorted, duplicate, zeroFirst];
        for (uint256 i; i < bad.length; ++i) {
            RegistryConfig memory c = _nextRegistry();
            c.signers = bad[i];
            vm.expectRevert(IZecuredSignerErrors.InvalidSignerSet.selector);
            new ZecuredRegistry(c);
            // The spend logic checks its set the same way.
            vm.expectRevert(IZecuredSignerErrors.InvalidSignerSet.selector);
            new ZecuredSpendLogic(address(registry), bad[i]);
        }
    }

    function test_constructor_revertsWhenMorphoRefusesTheMarket() public {
        RegistryConfig memory c = _nextRegistry();
        c.irm = stranger;
        vm.expectRevert(bytes("IRM not enabled"));
        new ZecuredRegistry(c);
        c = _nextRegistry();
        c.lltv = 0.6e18;
        vm.expectRevert(bytes("LLTV not enabled"));
        new ZecuredRegistry(c);
    }

    function test_adapterConstructor_revertsAtAnUnexpectedAddress() public {
        // The fixture's Registry expects its Adapter at a fixed address; any other deployment is refused.
        vm.expectRevert(ISettlementAdapter.AdapterMisconfigured.selector);
        new SettlementAdapter(registry, MIN_SEIZE, 0, 500);
    }

    // ================================================================== signer signatures (every 3-of-5 call)

    /// A released vault whose claim is committed: ready for `confirmPayout`.
    function _committedClaim(address acct) internal returns (bytes32 vaultId, bytes32[] memory ids) {
        bytes32 claimId;
        (vaultId, claimId) = _releasedVault(acct, VAULT_001);
        ids = _ids(claimId);
        _commitNext(vaultId, ids);
    }

    function test_signatures_acceptAnyThreeOrMoreDistinctSignersInOrder() public {
        uint256[][] memory sets = new uint256[][](5);
        sets[0] = _idx(0, 1, 2);
        sets[1] = _idx(2, 3, 4);
        sets[2] = _idx(0, 3, 4);
        sets[3] = new uint256[](4);
        (sets[3][0], sets[3][1], sets[3][2], sets[3][3]) = (0, 1, 3, 4);
        sets[4] = new uint256[](5);
        (sets[4][0], sets[4][1], sets[4][2], sets[4][3], sets[4][4]) = (0, 1, 2, 3, 4);
        for (uint256 i; i < sets.length; ++i) {
            (bytes32 vaultId, bytes32[] memory ids) =
                _committedClaim(makeAddr(string.concat("zecured.registry.multi", vm.toString(i))));
            registry.confirmPayout(vaultId, ids, _signerSigs(registry.hashPayoutConfirmation(vaultId, ids), sets[i]));
            (,,,, ClaimStatus status) = registry.claim(ids[0]);
            assertEq(uint8(status), uint8(ClaimStatus.Paid));
        }
    }

    function test_signatures_revertWithFewerThanThree() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 4)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 0));
        registry.confirmPayout(vaultId, ids, new bytes[](0));
    }

    function test_signatures_revertWithThreeSignaturesFromTwoSigners() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 1, 1)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 1));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(3, 3, 4)));
        // Four signatures, still only two distinct signers.
        uint256[] memory four = new uint256[](4);
        (four[0], four[1], four[2], four[3]) = (1, 1, 2, 2);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 1));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, four));
    }

    function test_signatures_revertWhenUnsorted() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 1));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(1, 0, 2)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 4, 2)));
    }

    function test_signatures_revertWithANonSignerSignature() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);
        (address outsider, uint256 outsiderKey) = makeAddrAndKey("zecured.registry.outsider");
        bytes[] memory sigs = _signerSigs(digest, _idx(0, 1, 2));
        sigs[2] = _sign(outsiderKey, digest);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.UnknownSigner.selector, outsider));
        registry.confirmPayout(vaultId, ids, sigs);
    }

    function test_signatures_revertWhenTheSignedDataDiffers() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        // The signers confirmed another vault's payout of the same list.
        bytes[] memory sigs = _threeSigs(registry.hashPayoutConfirmation(keccak256("zecured.registry.otherVault"), ids));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, sigs);
    }

    /// Every request kind has its own type: a ReimburseDestination signature never verifies as a PayoutConfirmation,
    /// and a signature without the Registry domain (raw struct hash) is refused too.
    function test_signatures_revertWhenOfAnotherTypeOrDomain() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes[] memory otherType =
            _threeSigs(registry.hashReimburseDestination(ids[0], vaultId, uint64(block.timestamp)));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, otherType);
        bytes[] memory raw = _threeSigs(ZecuredEIP712.hashPayoutConfirmation(vaultId, ids));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, raw);
    }

    function test_signatures_revertWhenMalleableOrMalformed() public {
        (bytes32 vaultId, bytes32[] memory ids) = _committedClaim(account);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);
        bytes[] memory sigs = _threeSigs(digest);

        // High-s twin of a valid signature: same signer for ecrecover, rejected by OpenZeppelin ECDSA.
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKeys[2], digest);
        bytes32 highS = bytes32(SECP256K1_N - uint256(s));
        sigs[1] = abi.encodePacked(r, highS, v == 27 ? uint8(28) : uint8(27));
        vm.expectRevert(abi.encodeWithSelector(ECDSA.ECDSAInvalidSignatureS.selector, highS));
        registry.confirmPayout(vaultId, ids, sigs);

        // 64-byte (ERC-2098 compact) signatures are not accepted.
        sigs = _threeSigs(digest);
        (bytes32 cr, bytes32 vs) = vm.signCompact(signerKeys[2], digest);
        sigs[1] = abi.encodePacked(cr, vs);
        vm.expectRevert(abi.encodeWithSelector(ECDSA.ECDSAInvalidSignatureLength.selector, 64));
        registry.confirmPayout(vaultId, ids, sigs);
    }

    // ================================================================== deposit hooks (ZecuredIntake only)

    function test_hooks_onlyTheirCaller() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        address[4] memory callers = [stranger, account, address(adapter), signerSet[0]];
        for (uint256 i; i < callers.length; ++i) {
            vm.startPrank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyIntake.selector, callers[i]));
            registry.reserveDeposit(vaultId, account, VAULT_001, tag);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyIntake.selector, callers[i]));
            registry.releaseDeposit(vaultId, account, tag);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyIntake.selector, callers[i]));
            registry.activate(vaultId, account, VAULT_001, tag);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyIntake.selector, callers[i]));
            registry.openRefund(vaultId, account, VAULT_001, tag);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyIntake.selector, callers[i]));
            registry.convertToRefund(vaultId, account, VAULT_001, tag);
            // Roles this deployment does not have (zero) admit nobody.
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyCompensator.selector, callers[i]));
            registry.retireVault(vaultId, VAULT_001);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyCompensator.selector, callers[i]));
            registry.settleCompensatedClaim(tag);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyBond.selector, callers[i]));
            registry.markBondPaid(tag);
            vm.stopPrank();
        }
        // Not even the zero address stands in for a missing role.
        vm.startPrank(address(0));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyCompensator.selector, address(0)));
        registry.retireVault(vaultId, VAULT_001);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyBond.selector, address(0)));
        registry.markBondPaid(tag);
        vm.stopPrank();
    }

    function test_activate_activatesTheVaultAndSuppliesCollateral() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        assertEq(account.code.length, 0, "counterfactual account: no code, no transaction yet");

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultReserved(vaultId, account, VAULT_005, tag);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(vaultId, account, VAULT_005, tag);
        vm.expectEmit(address(MORPHO));
        emit EventsLib.SupplyCollateral(marketId, address(registry), account, VAULT_005);
        vm.prank(stranger); // the harness stands in for the Intake, which anyone may call
        intake.harnessActivate(vaultId, account, VAULT_005, tag);

        (address vAccount, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        assertEq(vAccount, account);
        assertEq(outstanding, VAULT_005);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        VaultDetails memory d = registry.vaultDetails(vaultId);
        assertEq(d.claimCount, 0);
        assertEq(d.unpaidClaims, 0);
        assertEq(registry.vaultOf(account), vaultId);
        assertTrue(registry.depositTagUsed(tag));
        assertEq(registry.lineageOf(tag).vaultId, vaultId, "K2-a: the deposit heads the lineage");
        assertEq(registry.lineageOf(tag).value, VAULT_005);
        assertEq(registry.claimCount(vaultId), 0);
        assertEq(registry.openVaultCount(), 1);
        assertEq(registry.lockedZatoshi(), VAULT_005, "T-01: the zZEC supply");

        Position memory p = _position(account);
        assertEq(p.collateral, VAULT_005);
        assertEq(p.borrowShares, 0);
        assertEq(zzec.totalSupply(), VAULT_005, "I1");
        _assertZZECOnlyInMorpho(_list(account, stranger));
        assertEq(zzec.allowance(address(registry), address(MORPHO)), 0, "exact approval consumed");
        assertFalse(MORPHO.isAuthorized(account, address(registry)), "supplyCollateral needs no authorization");
    }

    function test_reserveDeposit_revertsOnAUsedVaultIdTagOrAccount() public {
        bytes32 first = _attest(account, VAULT_001);
        bytes32 firstTag = _depositTagFor(account);
        address other = makeAddr("zecured.registry.other");

        // Replay: the same deposit again.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, first));
        intake.harnessActivate(first, account, VAULT_001, firstTag);
        // A fresh account and tag, the vault id reused.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, first));
        intake.harnessActivate(first, other, VAULT_001, _depositTagFor(other));
        // A fresh account and vault id, the note's tag reused (I5).
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, firstTag));
        intake.harnessActivate(_vaultIdFor(other), other, VAULT_001, firstTag);
        // A second vault for the same account (I4) ...
        bytes32 second = keccak256("zecured.registry.secondVault");
        bytes32 secondTag = keccak256("zecured.registry.secondTag");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, account, first));
        intake.harnessActivate(second, account, VAULT_001, secondTag);
        assertEq(zzec.totalSupply(), VAULT_001, "no second mint");

        // ... still refused after the first vault is closed: one account <-> one vault, forever.
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(account);
        registry.requestRelease(first, RELEASE_COMMITMENT);
        assertEq(uint8(_status(first)), uint8(VaultStatus.Closed));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, account, first));
        intake.harnessActivate(second, account, VAULT_001, secondTag);
    }

    function test_depositHooks_revertOnZeroFields() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        vm.expectRevert(IZecuredRegistry.ZeroVaultId.selector);
        intake.harnessActivate(bytes32(0), account, VAULT_001, tag);
        vm.expectRevert(IZecuredRegistry.ZeroAccount.selector);
        intake.harnessActivate(vaultId, address(0), VAULT_001, tag);
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        intake.harnessActivate(vaultId, account, 0, tag);
        vm.expectRevert(IZecuredRegistry.ZeroDepositTag.selector);
        intake.harnessActivate(vaultId, account, VAULT_001, bytes32(0));
        // Refunds check the same fields.
        vm.expectRevert(IZecuredRegistry.ZeroVaultId.selector);
        intake.harnessRefund(bytes32(0), account, VAULT_001, tag);
        vm.expectRevert(IZecuredRegistry.ZeroAccount.selector);
        intake.harnessRefund(vaultId, address(0), VAULT_001, tag);
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        intake.harnessRefund(vaultId, account, 0, tag);
        vm.expectRevert(IZecuredRegistry.ZeroDepositTag.selector);
        intake.harnessRefund(vaultId, account, VAULT_001, bytes32(0));
    }

    /// K3: a reservation binds the vault, the tag and the account, and makes the note a lineage note; releasing it (its
    /// anchor left the best chain) frees all of them.
    function test_reserveAndReleaseDeposit_freeEverything() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultReserved(vaultId, account, VAULT_001, tag);
        vm.prank(address(intake));
        registry.reserveDeposit(vaultId, account, VAULT_001, tag);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Pending));
        assertEq(registry.vaultOf(account), vaultId);
        assertTrue(registry.depositTagUsed(tag));
        assertEq(registry.lineageOf(tag).value, VAULT_001, "a lineage note from the reservation on");
        assertEq(registry.openVaultCount(), 1);
        assertEq(zzec.totalSupply(), 0, "nothing minted yet");

        skip(1 hours);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultReservationReleased(vaultId);
        vm.prank(address(intake));
        registry.releaseDeposit(vaultId, account, tag);
        (address a, uint64 o, VaultStatus s) = registry.vault(vaultId);
        assertEq(a, address(0));
        assertEq(o, 0);
        assertEq(uint8(s), uint8(VaultStatus.None));
        assertEq(registry.vaultOf(account), bytes32(0));
        assertFalse(registry.depositTagUsed(tag));
        assertEq(registry.lineageOf(tag).vaultId, bytes32(0));
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lastVaultSettledAt(), block.timestamp);

        // Everything can be used again.
        intake.harnessActivate(vaultId, account, VAULT_001, tag);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
    }

    function test_pendingHooks_revertOnAnyOtherReservation() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        address other = makeAddr("zecured.registry.other");
        vm.startPrank(address(intake));
        // Nothing reserved yet.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotPending.selector, vaultId));
        registry.activate(vaultId, account, VAULT_001, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotPending.selector, vaultId));
        registry.releaseDeposit(vaultId, account, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotPending.selector, vaultId));
        registry.convertToRefund(vaultId, account, VAULT_001, tag);

        registry.reserveDeposit(vaultId, account, VAULT_001, tag);
        // Another account, another tag, another amount.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        registry.activate(vaultId, other, VAULT_001, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        registry.activate(vaultId, account, VAULT_001, keccak256("zecured.registry.otherTag"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        registry.activate(vaultId, account, VAULT_005, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        registry.convertToRefund(vaultId, account, VAULT_005, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        registry.releaseDeposit(vaultId, other, tag);

        // Once active, it is no longer pending.
        registry.activate(vaultId, account, VAULT_001, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotPending.selector, vaultId));
        registry.activate(vaultId, account, VAULT_001, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotPending.selector, vaultId));
        registry.releaseDeposit(vaultId, account, tag);
        vm.stopPrank();
    }

    // ================================================================== refunds (T-02)

    bytes32 internal constant REFUND_COMMITMENT = keccak256("zecured.test.commitment.refund");

    /// A deposit to a new vault that landed as a refund (the harness stands in for a proof).
    function _refund(address acct, uint64 amount) internal returns (bytes32 vaultId, bytes32 claimId) {
        vaultId = _vaultIdFor(acct);
        claimId = intake.harnessRefund(vaultId, acct, amount, _depositTagFor(acct));
    }

    function test_openRefund_cancelsANewVault() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        bytes32 expectedClaimId = registry.claimIdOf(vaultId, 0);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.RefundOpened(vaultId, expectedClaimId, account, VAULT_001, tag);
        bytes32 claimId = intake.harnessRefund(vaultId, account, VAULT_001, tag);

        assertEq(claimId, expectedClaimId);
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, bytes32(0), ClaimStatus.Pending);
        ClaimDetails memory d = registry.claimDetails(claimId);
        assertEq(d.claimant, account);
        assertEq(d.createdAt, block.timestamp);
        assertFalse(d.bondPaid);
        (address a, uint64 o, VaultStatus s) = registry.vault(vaultId);
        assertEq(a, account);
        assertEq(o, 0);
        assertEq(uint8(s), uint8(VaultStatus.Cancelled));
        assertEq(registry.vaultOf(account), bytes32(0), "a refund binds no account");
        assertEq(registry.lineageOf(tag).value, VAULT_001, "the refunded note is a lineage note");
        assertTrue(registry.depositTagUsed(tag));
        assertEq(zzec.totalSupply(), 0);
        assertEq(registry.lockedZatoshi(), VAULT_001, "T-01: a pending claim counts");
        assertEq(registry.openVaultCount(), 1);

        // The vault id is spent; the account may still open a vault under another id.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, vaultId));
        intake.harnessActivate(vaultId, account, VAULT_001, keccak256("zecured.registry.retry"));
        intake.harnessActivate(
            keccak256("zecured.registry.retryVault"), account, VAULT_001, keccak256("zecured.registry.retry")
        );
    }

    function test_openRefund_onAnExistingVault() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        bytes32 secondTag = keccak256("zecured.registry.secondDeposit");
        bytes32 claimId = intake.harnessRefund(vaultId, account, VAULT_005, secondTag);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "a second deposit leaves the vault active");
        assertEq(_outstanding(vaultId), VAULT_001);
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_005, bytes32(0), ClaimStatus.Pending);
        assertEq(registry.lockedZatoshi(), VAULT_001 + VAULT_005);
        assertEq(registry.openVaultCount(), 1);

        // Another account's deposit to this vault is refused (the account comes from the vault's VaultOpened).
        address other = makeAddr("zecured.registry.other");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReservationMismatch.selector, vaultId));
        intake.harnessRefund(vaultId, other, VAULT_001, keccak256("zecured.registry.thirdDeposit"));
        // A note lands once.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, secondTag));
        intake.harnessRefund(vaultId, account, VAULT_005, secondTag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, _depositTagFor(account)));
        intake.harnessRefund(vaultId, account, VAULT_001, _depositTagFor(account));
    }

    /// A settled vault (closed, every claim paid) reopens for a late deposit's refund.
    function test_openRefund_reopensASettledVault() public {
        (bytes32 vaultId, bytes32 releaseId) = _releasedVault(account, VAULT_001);
        bytes32[] memory ids = _ids(releaseId);
        _commitNext(vaultId, ids);
        registry.confirmPayout(vaultId, ids, _threeSigs(registry.hashPayoutConfirmation(vaultId, ids)));
        assertEq(registry.openVaultCount(), 0);

        bytes32 claimId = intake.harnessRefund(vaultId, account, VAULT_005, keccak256("zecured.registry.late"));
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(registry.openVaultCount(), 1, "open again until the refund is paid");
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_005, bytes32(0), ClaimStatus.Pending);
    }

    function test_convertToRefund_cancelsAPendingVault() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        vm.startPrank(address(intake));
        registry.reserveDeposit(vaultId, account, VAULT_001, tag);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.RefundOpened(vaultId, registry.claimIdOf(vaultId, 0), account, VAULT_001, tag);
        bytes32 claimId = registry.convertToRefund(vaultId, account, VAULT_001, tag);
        vm.stopPrank();
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, bytes32(0), ClaimStatus.Pending);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Cancelled));
        assertEq(registry.vaultOf(account), vaultId, "the account stays bound (I4)");
        assertEq(registry.lineageOf(tag).value, VAULT_001);
        assertEq(registry.openVaultCount(), 1);
        assertEq(registry.lockedZatoshi(), VAULT_001);
        assertEq(zzec.totalSupply(), 0);
    }

    /// A refund opened while another proof holds the vault's reservation keeps the vault (cancelled) when the
    /// reservation is released.
    function test_releaseDeposit_keepsAVaultWithARefund() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 tag = _depositTagFor(account);
        vm.startPrank(address(intake));
        registry.reserveDeposit(vaultId, account, VAULT_001, tag);
        bytes32 claimId = registry.openRefund(vaultId, account, VAULT_005, keccak256("zecured.registry.second"));
        registry.releaseDeposit(vaultId, account, tag);
        vm.stopPrank();
        (address a,, VaultStatus s) = registry.vault(vaultId);
        assertEq(a, account, "the refund's account stays");
        assertEq(uint8(s), uint8(VaultStatus.Cancelled));
        assertEq(registry.vaultOf(account), bytes32(0));
        assertFalse(registry.depositTagUsed(tag));
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_005, bytes32(0), ClaimStatus.Pending);
        assertEq(registry.openVaultCount(), 1);
        assertEq(registry.lockedZatoshi(), VAULT_005);
    }

    function test_refund_isPaidToTheDestinationItsAccountGives() public {
        (bytes32 vaultId, bytes32 claimId) = _refund(account, VAULT_001);
        bytes32[] memory ids = _ids(claimId);
        // No payout before the account names a destination.
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(account), ids, "zecured.registry.refund");
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimDestinationUnset.selector, claimId));
        registry.commitSpend(c, sigs);

        skip(1 hours);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.RefundDestinationSet(vaultId, claimId, REFUND_COMMITMENT);
        vm.prank(account);
        registry.setRefundDestination(claimId, REFUND_COMMITMENT);
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, REFUND_COMMITMENT, ClaimStatus.Pending);
        assertEq(registry.claimDetails(claimId).createdAt, block.timestamp, "T-05: the claim's clock restarts");

        // The account may change its mind until a payout binds the claim.
        vm.prank(account);
        registry.setRefundDestination(claimId, USER_COMMITMENT);
        _commitNext(vaultId, ids);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.RefundBound.selector, claimId));
        registry.setRefundDestination(claimId, REFUND_COMMITMENT);

        registry.confirmPayout(vaultId, ids, _threeSigs(registry.hashPayoutConfirmation(vaultId, ids)));
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, USER_COMMITMENT, ClaimStatus.Paid);
        assertEq(registry.lockedZatoshi(), 0);
        assertEq(registry.openVaultCount(), 0, "the cancelled vault settled");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.RefundBound.selector, claimId));
        registry.setRefundDestination(claimId, REFUND_COMMITMENT);
    }

    function test_setRefundDestination_reverts() public {
        (bytes32 vaultId, bytes32 claimId) = _refund(account, VAULT_001);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotVaultAccount.selector, vaultId, stranger));
        registry.setRefundDestination(claimId, REFUND_COMMITMENT);
        vm.prank(account);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.setRefundDestination(claimId, bytes32(0));

        // Only Refund claims.
        bytes32 unknown = keccak256("zecured.registry.unknownClaim");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, unknown));
        registry.setRefundDestination(unknown, REFUND_COMMITMENT);
        address other = makeAddr("zecured.registry.other");
        (, bytes32 releaseId) = _releasedVault(other, VAULT_001);
        vm.prank(other);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, releaseId));
        registry.setRefundDestination(releaseId, REFUND_COMMITMENT);
    }

    function test_abandonRefund_writesItOffAndItsAccountRevivesIt() public {
        (bytes32 vaultId, bytes32 claimId) = _refund(account, VAULT_001);
        uint64 at = uint64(block.timestamp) + ABANDON_PERIOD;
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotAbandonable.selector, claimId, at));
        registry.abandonRefund(claimId);
        vm.warp(at - 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotAbandonable.selector, claimId, at));
        registry.abandonRefund(claimId);

        vm.warp(at);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.RefundAbandoned(vaultId, claimId);
        vm.prank(stranger); // anyone
        registry.abandonRefund(claimId);
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, bytes32(0), ClaimStatus.WrittenOff);
        assertEq(registry.lockedZatoshi(), 0);
        assertEq(registry.openVaultCount(), 0, "it no longer keeps the vault open");
        assertEq(registry.lastVaultSettledAt(), block.timestamp);
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        assertEq(
            registry.lineageOf(_depositTagFor(account)).value, VAULT_001, "still a lineage note: spending it is F1"
        );
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, claimId));
        registry.abandonRefund(claimId);

        // A year later its account gives a destination: the claim is pending again.
        skip(365 days);
        vm.prank(account);
        registry.setRefundDestination(claimId, REFUND_COMMITMENT);
        _assertClaim(claimId, vaultId, ClaimKind.Refund, VAULT_001, REFUND_COMMITMENT, ClaimStatus.Pending);
        assertEq(registry.lockedZatoshi(), VAULT_001);
        assertEq(registry.openVaultCount(), 1);
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 1);
        // With a destination it can no longer be abandoned.
        skip(ABANDON_PERIOD);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, claimId));
        registry.abandonRefund(claimId);

        bytes32[] memory ids = _ids(claimId);
        _commitNext(vaultId, ids);
        registry.confirmPayout(vaultId, ids, _threeSigs(registry.hashPayoutConfirmation(vaultId, ids)));
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lockedZatoshi(), 0);
    }

    function test_abandonRefund_onlyRefundsWithoutADestination() public {
        (, bytes32 releaseId) = _releasedVault(account, VAULT_001);
        skip(ABANDON_PERIOD);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, releaseId));
        registry.abandonRefund(releaseId);
        bytes32 unknown = keccak256("zecured.registry.unknownClaim");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, unknown));
        registry.abandonRefund(unknown);
    }

    // ================================================================== requestRelease

    function test_requestRelease_burnsAndCreatesAReleaseClaim() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 expectedClaimId = keccak256(abi.encode(vaultId, uint64(0)));

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ReleaseRequested(vaultId, expectedClaimId, VAULT_005, RELEASE_COMMITMENT);
        vm.expectEmit(address(MORPHO));
        emit EventsLib.WithdrawCollateral(marketId, address(registry), account, address(registry), VAULT_005);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);

        assertEq(claimId, expectedClaimId);
        assertEq(claimId, registry.claimIdOf(vaultId, 0));
        _assertClaim(claimId, vaultId, ClaimKind.Release, VAULT_005, RELEASE_COMMITMENT, ClaimStatus.Pending);
        (address vAccount, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        assertEq(vAccount, account);
        assertEq(outstanding, 0);
        assertEq(uint8(status), uint8(VaultStatus.Closed));
        assertEq(registry.claimCount(vaultId), 1);
        assertEq(registry.vaultOf(account), vaultId, "the mapping is permanent");
        assertEq(_position(account).collateral, 0);
        assertEq(zzec.totalSupply(), 0, "I1: burned");
        _assertZZECOnlyInMorpho(_list(account));
    }

    function test_requestRelease_revertsWhileDebtRemains() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        _borrow(account, 1 * USDC);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DebtOutstanding.selector, vaultId));
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    function test_requestRelease_revertsForAnyoneButTheAccount() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        address[4] memory callers = [stranger, address(adapter), signerSet[0], marketMaker];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotVaultAccount.selector, vaultId, callers[i]));
            registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        }
    }

    function test_requestRelease_revertsForUnknownOrClosedVault() public {
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.requestRelease(unknown, RELEASE_COMMITMENT);

        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, vaultId));
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    function test_requestRelease_revertsOnZeroCommitment() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.requestRelease(vaultId, bytes32(0));
    }

    function test_requestRelease_revertsWithoutTheAccountsMorphoAuthorization() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        vm.expectRevert(bytes("unauthorized"));
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    /// Defensive check (unreachable through the public API): the Registry refuses to release when Morpho's collateral
    /// and the vault's outstanding disagree. Forced here by editing Morpho's storage.
    function test_requestRelease_revertsOnCollateralMismatch() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 slot = MorphoStorageLib.positionBorrowSharesAndCollateralSlot(marketId, account);
        vm.store(address(MORPHO), slot, bytes32(uint256(VAULT_005 - 1) << 128));
        vm.prank(account);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.CollateralMismatch.selector, vaultId, VAULT_005 - 1, VAULT_005)
        );
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    /// Defensive check (unreachable with the real Morpho): the Registry burns only what it actually received.
    /// Forced here by mocking zZEC.balanceOf(registry).
    function test_requestRelease_revertsWhenTheReceivedAmountDiffers() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        // Y5: the amount comes from the guard's receipt, never from a balance difference.
        vm.mockCall(address(zzec), abi.encodeCall(zzec.settleExpectation, ()), abi.encode(uint256(0)));
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.UnexpectedCollateralReceived.selector, 0, VAULT_005));
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    // ================================================================== requestPartialRelease (D14)

    bytes32 internal constant PARTIAL_COMMITMENT = keccak256("zecured.test.commitment.partial");

    /// 0.05 ZEC vault (2,000,000 USDC units of collateral at $40), both authorizations, optional loan.
    function _vaultWithLoan(uint256 borrowAssets) internal returns (bytes32 vaultId) {
        vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        if (borrowAssets != 0) _borrow(account, borrowAssets);
    }

    function test_requestPartialRelease_withoutDebt() public {
        bytes32 vaultId = _vaultWithLoan(0);
        bytes32 expectedClaimId = registry.claimIdOf(vaultId, 0);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ReleaseRequested(vaultId, expectedClaimId, 2_000_000, PARTIAL_COMMITMENT);
        vm.expectEmit(address(MORPHO));
        emit EventsLib.WithdrawCollateral(marketId, address(registry), account, address(registry), 2_000_000);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 2_000_000, PARTIAL_COMMITMENT);

        assertEq(claimId, expectedClaimId);
        _assertClaim(claimId, vaultId, ClaimKind.Release, 2_000_000, PARTIAL_COMMITMENT, ClaimStatus.Pending);
        assertEq(_outstanding(vaultId), VAULT_005 - 2_000_000);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "the vault stays Active");
        assertEq(_position(account).collateral, VAULT_005 - 2_000_000);
        assertEq(zzec.totalSupply(), VAULT_005 - 2_000_000, "I1");
        _assertZZECOnlyInMorpho(_list(account));

        // Without debt everything but MIN_RELEASE may go (the vault stays Active, its rest stays payable).
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseRemainderTooSmall.selector, 99_999, 100_000));
        registry.requestPartialRelease(vaultId, VAULT_005 - 2_000_000 - 99_999, PARTIAL_COMMITMENT);
        vm.prank(account);
        registry.requestPartialRelease(vaultId, VAULT_005 - 2_000_000 - 100_000, PARTIAL_COMMITMENT);
        assertEq(_outstanding(vaultId), 100_000);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
        assertEq(registry.claimCount(vaultId), 2);
    }

    /// With debt the LTV after the withdrawal may reach exactly 50 %, not more.
    function test_requestPartialRelease_withDebt_upToExactlyFiftyPercent() public {
        bytes32 vaultId = _vaultWithLoan(600_000); // 30 % LTV at $40
        // 2,000,001 zat out leaves 2,999,999 zat = 1,199,999 units (rounded down): just above 50 %.
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseLtvTooHigh.selector, 600_000, 1_199_999));
        registry.requestPartialRelease(vaultId, 2_000_001, PARTIAL_COMMITMENT);

        // 2,000,000 zat out leaves 3,000,000 zat = 1,200,000 units: exactly 50 %.
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 2_000_000, PARTIAL_COMMITMENT);
        _assertClaim(claimId, vaultId, ClaimKind.Release, 2_000_000, PARTIAL_COMMITMENT, ClaimStatus.Pending);
        assertEq(_outstanding(vaultId), 3_000_000);
        assertGt(_position(account).borrowShares, 0, "the loan stays open");
        assertEq(usdc.balanceOf(borrowReceiver), 600_000);
        assertEq(zzec.totalSupply(), 3_000_000, "I1");
        _assertZZECOnlyInMorpho(_list(account));
    }

    /// Between 50 % and LLTV (62.5 %) Morpho would allow the withdrawal; the Registry refuses it.
    function test_requestPartialRelease_revertsAboveFiftyPercentBelowLltv() public {
        bytes32 vaultId = _vaultWithLoan(800_000); // 40 % LTV
        // 1,500,000 zat out leaves 3,500,000 zat = 1,400,000 units: LTV 57.1 %.
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseLtvTooHigh.selector, 800_000, 1_400_000));
        registry.requestPartialRelease(vaultId, 1_500_000, PARTIAL_COMMITMENT);
        assertEq(_outstanding(vaultId), VAULT_005, "nothing released");
        assertEq(registry.claimCount(vaultId), 0, "no claim");
    }

    /// Above LLTV Morpho's own health check refuses the withdrawal.
    function test_requestPartialRelease_revertsAboveLltv() public {
        bytes32 vaultId = _vaultWithLoan(800_000);
        // 2,000,000 zat out leaves 3,000,000 zat = 1,200,000 units: LTV 66.7 % > 62.5 %.
        vm.prank(account);
        vm.expectRevert(bytes("insufficient collateral"));
        registry.requestPartialRelease(vaultId, 2_000_000, PARTIAL_COMMITMENT);
        assertEq(_outstanding(vaultId), VAULT_005);
    }

    function test_requestPartialRelease_revertsOnAmountBounds() public {
        bytes32 vaultId = _vaultWithLoan(0);
        vm.startPrank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseAmountTooSmall.selector, 99_999, 100_000));
        registry.requestPartialRelease(vaultId, 99_999, PARTIAL_COMMITMENT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseAmountTooSmall.selector, 0, 100_000));
        registry.requestPartialRelease(vaultId, 0, PARTIAL_COMMITMENT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseAmountNotPartial.selector, VAULT_005, VAULT_005));
        registry.requestPartialRelease(vaultId, VAULT_005, PARTIAL_COMMITMENT);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.ReleaseAmountNotPartial.selector, VAULT_005 + 1, VAULT_005)
        );
        registry.requestPartialRelease(vaultId, VAULT_005 + 1, PARTIAL_COMMITMENT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseRemainderTooSmall.selector, 1, 100_000));
        registry.requestPartialRelease(vaultId, VAULT_005 - 1, PARTIAL_COMMITMENT);
        // Exactly MIN_RELEASE is fine.
        registry.requestPartialRelease(vaultId, 100_000, PARTIAL_COMMITMENT);
        vm.stopPrank();
        assertEq(_outstanding(vaultId), VAULT_005 - 100_000);
        assertEq(registry.MIN_RELEASE(), 100_000);
        assertEq(registry.MAX_WITHDRAW_LTV_BPS(), 5_000);
    }

    function test_requestPartialRelease_revertsForAnyoneButTheAccount() public {
        bytes32 vaultId = _vaultWithLoan(0);
        address[4] memory callers = [stranger, address(adapter), signerSet[0], marketMaker];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotVaultAccount.selector, vaultId, callers[i]));
            registry.requestPartialRelease(vaultId, 1_000_000, PARTIAL_COMMITMENT);
        }
    }

    function test_requestPartialRelease_revertsOnZeroCommitmentUnknownOrClosedVaultOrMissingAuthorization() public {
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.requestPartialRelease(unknown, 1_000_000, PARTIAL_COMMITMENT);

        bytes32 vaultId = _attest(account, VAULT_005);
        vm.prank(account);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.requestPartialRelease(vaultId, 1_000_000, bytes32(0));
        vm.prank(account);
        vm.expectRevert(bytes("unauthorized")); // the account has not authorized the Registry on Morpho yet
        registry.requestPartialRelease(vaultId, 1_000_000, PARTIAL_COMMITMENT);

        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, vaultId));
        registry.requestPartialRelease(vaultId, 1_000_000, PARTIAL_COMMITMENT);
    }

    /// The debt includes the interest accrued up to this block (Morpho accrues inside withdrawCollateral).
    function test_requestPartialRelease_countsAccruedInterest() public {
        bytes32 vaultId = _vaultWithLoan(600_000); // releasing 2,000,000 lands exactly on 50 % without interest
        skip(30 days);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        assertGt(debt, 600_000, "interest accrued");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ReleaseLtvTooHigh.selector, debt, 1_200_000));
        registry.requestPartialRelease(vaultId, 2_000_000, PARTIAL_COMMITMENT);
        // A bit less still fits.
        vm.prank(account);
        registry.requestPartialRelease(vaultId, 1_900_000, PARTIAL_COMMITMENT);
        assertEq(_outstanding(vaultId), VAULT_005 - 1_900_000);
    }

    /// Property: a partial release is accepted exactly when the LTV after it, computed independently (Morpho's expected
    /// debt rounded up; collateral value rounded down), is at most 50 %. Above LLTV Morpho refuses, in between the
    /// Registry does.
    function testFuzz_requestPartialRelease_ltvRule(
        uint64 amountSeed,
        uint256 debtBps,
        uint256 priceCents,
        uint256 elapsed
    ) public {
        bytes32 vaultId = _vaultWithLoan(2_000_000 * bound(debtBps, 0, 6_000) / 10_000); // 0..60 % LTV at $40
        skip(bound(elapsed, 0, 365 days));
        oracle.setPrice(bound(priceCents, 2_000, 8_000) * 1e32); // $20..$80
        uint64 amount = uint64(bound(amountSeed, 100_000, VAULT_005 - 100_000));

        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        uint256 value = uint256(VAULT_005 - amount) * oracle.price() / 1e36;
        bool morphoAccepts = debt == 0 || value * LLTV / 1e18 >= debt;
        bool registryAccepts = debt * 10_000 <= value * 5_000;

        vm.prank(account);
        try registry.requestPartialRelease(vaultId, amount, PARTIAL_COMMITMENT) {
            assertTrue(morphoAccepts && registryAccepts, "accepted above 50 %");
            assertEq(_outstanding(vaultId), VAULT_005 - amount);
            assertEq(zzec.totalSupply(), VAULT_005 - amount, "I1");
        } catch (bytes memory reason) {
            assertFalse(morphoAccepts && registryAccepts, "refused at or below 50 %");
            if (!morphoAccepts) assertEq(reason, abi.encodeWithSignature("Error(string)", "insufficient collateral"));
            else assertEq(bytes4(reason), IZecuredRegistry.ReleaseLtvTooHigh.selector);
        }
    }

    // ================================================================== recordLiquidation / recordClose

    function test_recordLiquidationAndRecordClose_onlyAdapter() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        address[4] memory callers = [stranger, account, address(registry), signerSet[0]];
        for (uint256 i; i < callers.length; ++i) {
            vm.startPrank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyAdapter.selector, callers[i]));
            registry.recordLiquidation(vaultId, uint64(MIN_SEIZE), LIQUIDATOR_COMMITMENT, liquidator);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyAdapter.selector, callers[i]));
            registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
            vm.stopPrank();
        }
    }

    function test_recordLiquidation_reverts() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.startPrank(address(adapter));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.recordLiquidation(unknown, 1, LIQUIDATOR_COMMITMENT, liquidator);
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        registry.recordLiquidation(vaultId, 0, LIQUIDATOR_COMMITMENT, liquidator);
        // I9: never more than the outstanding.
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.SeizeExceedsOutstanding.selector, vaultId, VAULT_005 + 1, VAULT_005)
        );
        registry.recordLiquidation(vaultId, VAULT_005 + 1, LIQUIDATOR_COMMITMENT, liquidator);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordLiquidation(vaultId, 1, bytes32(0), liquidator);
        // The Adapter must actually hold the seized zZEC.
        vm.expectRevert(abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, address(adapter), 0, 1));
        registry.recordLiquidation(vaultId, 1, LIQUIDATOR_COMMITMENT, liquidator);
        vm.stopPrank();
    }

    function test_recordClose_reverts() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.startPrank(address(adapter));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.recordClose(unknown, 1, 1, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
        // I7: the two parts must add up to the outstanding exactly.
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, 1, VAULT_005, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, 1, VAULT_005 - 2, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005 - 2, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, type(uint64).max, 1, VAULT_005
            )
        );
        registry.recordClose(vaultId, type(uint64).max, 1, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordClose(vaultId, 1, VAULT_005 - 1, bytes32(0), USER_COMMITMENT, marketMaker);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, bytes32(0), marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, address(adapter), 0, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, USER_COMMITMENT, marketMaker);
        vm.stopPrank();
    }

    // ================================================================== confirmPayout

    function _releasedVault(address acct, uint64 amount) internal returns (bytes32 vaultId, bytes32 claimId) {
        vaultId = _attest(acct, amount);
        vm.prank(acct);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(acct);
        claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
    }

    function _ids(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _ids(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](2);
        (r[0], r[1]) = (a, b);
    }

    function test_confirmPayout_marksTheClaimPaid() public {
        (bytes32 vaultId, bytes32 claimId) = _releasedVault(account, VAULT_005);
        bytes32[] memory ids = _ids(claimId);
        _commitNext(vaultId, ids);
        bytes[] memory sigs = _signerSigs(registry.hashPayoutConfirmation(vaultId, ids), _idx(1, 2, 3));
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.PayoutConfirmed(vaultId, ids);
        vm.prank(stranger); // anyone may relay
        registry.confirmPayout(vaultId, ids, sigs);
        _assertClaim(claimId, vaultId, ClaimKind.Release, VAULT_005, RELEASE_COMMITMENT, ClaimStatus.Paid);
    }

    function test_confirmPayout_revertsOnBadSignatureSets() public {
        (bytes32 vaultId, bytes32 claimId) = _releasedVault(account, VAULT_005);
        bytes32[] memory ids = _ids(claimId);
        _commitNext(vaultId, ids);
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);

        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 1)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 2, 2)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 1));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(4, 2, 3)));

        // Signatures over another claim list (here: an extra id) do not verify.
        bytes32[] memory more = _ids(claimId, keccak256("zecured.registry.otherClaim"));
        bytes[] memory otherListSigs = _threeSigs(registry.hashPayoutConfirmation(vaultId, more));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, otherListSigs);
        // ... nor do signatures of another type (a ReimburseDestination).
        bytes[] memory otherTypeSigs =
            _threeSigs(registry.hashReimburseDestination(claimId, RELEASE_COMMITMENT, uint64(block.timestamp)));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, otherTypeSigs);
        _assertClaim(claimId, vaultId, ClaimKind.Release, VAULT_005, RELEASE_COMMITMENT, ClaimStatus.Pending);
    }

    function test_confirmPayout_revertsOnEmptyUnknownForeignOrPaidClaims() public {
        (bytes32 vaultA, bytes32 claimA) = _releasedVault(account, VAULT_005);
        (bytes32 vaultB, bytes32 claimB) = _releasedVault(makeAddr("zecured.registry.accountB"), VAULT_001);

        bytes32[] memory none = new bytes32[](0);
        bytes[] memory sigs = _threeSigs(registry.hashPayoutConfirmation(vaultA, none));
        vm.expectRevert(IZecuredRegistry.NoClaims.selector);
        registry.confirmPayout(vaultA, none, sigs);

        bytes32 unknown = keccak256("zecured.registry.unknownClaim");
        bytes32[] memory ids = _ids(unknown);
        sigs = _threeSigs(registry.hashPayoutConfirmation(vaultA, ids));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.UnknownClaim.selector, unknown));
        registry.confirmPayout(vaultA, ids, sigs);

        // A foreign vault's claim, even with valid signatures over (vaultA, [claimB]).
        ids = _ids(claimB);
        sigs = _threeSigs(registry.hashPayoutConfirmation(vaultA, ids));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ClaimVaultMismatch.selector, claimB, vaultA));
        registry.confirmPayout(vaultA, ids, sigs);

        // Already paid, and the same id twice in one call.
        ids = _ids(claimA);
        _commitNext(vaultA, ids);
        _commitNext(vaultB, _ids(claimB));
        sigs = _threeSigs(registry.hashPayoutConfirmation(vaultA, ids));
        registry.confirmPayout(vaultA, ids, sigs);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ClaimAlreadyPaid.selector, claimA));
        registry.confirmPayout(vaultA, ids, sigs);
        ids = _ids(claimB, claimB);
        sigs = _threeSigs(registry.hashPayoutConfirmation(vaultB, ids));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ClaimAlreadyPaid.selector, claimB));
        registry.confirmPayout(vaultB, ids, sigs);
        _assertClaim(claimB, vaultB, ClaimKind.Release, VAULT_001, RELEASE_COMMITMENT, ClaimStatus.Pending);
    }

    /// Tier 3: a claim is confirmed paid only once a payout of it was committed, which its claimant can check (F3).
    function test_confirmPayout_revertsOnAnUncommittedClaim() public {
        (bytes32 vaultId, bytes32 claimId) = _releasedVault(account, VAULT_005);
        bytes32[] memory ids = _ids(claimId);
        bytes[] memory sigs = _threeSigs(registry.hashPayoutConfirmation(vaultId, ids));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotCommitted.selector, claimId));
        registry.confirmPayout(vaultId, ids, sigs);
        _commitNext(vaultId, ids);
        registry.confirmPayout(vaultId, ids, sigs);
        _assertClaim(claimId, vaultId, ClaimKind.Release, VAULT_005, RELEASE_COMMITMENT, ClaimStatus.Paid);
    }

    // ================================================================== views

    function test_views_unknownIdsAreEmpty() public view {
        assertEq(registry.vaultOf(account), bytes32(0));
        (address a, uint64 o, VaultStatus s) = registry.vault(keccak256("nothing"));
        assertEq(a, address(0));
        assertEq(o, 0);
        assertEq(uint8(s), uint8(VaultStatus.None));
        (,, uint64 amount,, ClaimStatus cs) = registry.claim(keccak256("nothing"));
        assertEq(amount, 0);
        assertEq(uint8(cs), uint8(ClaimStatus.None));
        assertFalse(registry.depositTagUsed(keccak256("nothing")));
    }
}
