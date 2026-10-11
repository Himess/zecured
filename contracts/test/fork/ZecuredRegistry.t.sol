// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {EventsLib} from "morpho-blue/src/libraries/EventsLib.sol";
import {MorphoStorageLib} from "morpho-blue/src/libraries/periphery/MorphoStorageLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {IZecuredRegistry, VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {ISettlementAdapter} from "../../src/interfaces/ISettlementAdapter.sol";
import {DepositAttestation, ZecuredEIP712} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice ZecuredRegistry against the real Morpho Blue on a Base Sepolia fork: deployment wiring, every function and
///         every revert path (I1, I3, I4, I5, I6, I9).
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

        assertEq(registry.THRESHOLD(), 3);
        assertEq(adapter.MIN_SEIZE(), 100_000);
        assertEq(adapter.MAX_MARGIN_BPS(), 500);
        address[5] memory s = registry.signers();
        for (uint256 i; i < 5; ++i) {
            assertEq(s[i], signerSet[i]);
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

    /// The deployed Registry's digest equals the one an independent EIP-712 implementation derives from wallet JSON.
    function test_deployedDigests_matchTypedDataJson() public view {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        string memory domain = string.concat(
            '"domain":{"name":"ZecuredRegistry","version":"1","chainId":84532,"verifyingContract":"',
            vm.toString(address(registry)),
            '"}'
        );
        string memory json = string.concat(
            '{"types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"DepositAttestation":[{"name":"vaultId","type":"bytes32"},{"name":"account","type":"address"},{"name":"amount","type":"uint64"},{"name":"depositTag","type":"bytes32"}]},',
            '"primaryType":"DepositAttestation",',
            domain,
            ',"message":{"vaultId":"',
            vm.toString(att.vaultId),
            '","account":"',
            vm.toString(att.account),
            '","amount":"1000000","depositTag":"',
            vm.toString(att.depositTag),
            '"}}'
        );
        assertEq(registry.hashDepositAttestation(att), vm.eip712HashTypedData(json));

        bytes32[] memory ids = new bytes32[](2);
        ids[0] = registry.claimIdOf(att.vaultId, 0);
        ids[1] = registry.claimIdOf(att.vaultId, 1);
        json = string.concat(
            '{"types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"PayoutConfirmation":[{"name":"vaultId","type":"bytes32"},{"name":"claimIdsHash","type":"bytes32"}]},',
            '"primaryType":"PayoutConfirmation",',
            domain,
            ',"message":{"vaultId":"',
            vm.toString(att.vaultId),
            '","claimIdsHash":"',
            vm.toString(keccak256(abi.encodePacked(ids))),
            '"}}'
        );
        assertEq(registry.hashPayoutConfirmation(att.vaultId, ids), vm.eip712HashTypedData(json));
    }

    function test_constructor_reusesAMarketCreatedByAFrontRunner() public {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);
        ZZEC zzec2 = new ZZEC(predictedRegistry, predictedAdapter, address(MORPHO));
        // Someone creates the market between the zZEC deployment and the Registry deployment.
        MarketParams memory p = MarketParams({
            loanToken: address(usdc),
            collateralToken: address(zzec2),
            oracle: address(oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV
        });
        vm.prank(stranger);
        MORPHO.createMarket(p);
        ZecuredRegistry registry2 = new ZecuredRegistry(
            MORPHO,
            zzec2,
            predictedAdapter,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );
        SettlementAdapter adapter2 = new SettlementAdapter(registry2);
        assertEq(address(registry2), predictedRegistry);
        assertEq(address(adapter2), predictedAdapter);
    }

    function test_constructor_revertsOnZeroAddress() public {
        address a = makeAddr("zecured.registry.someAdapter");
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(
            IMorpho(address(0)),
            zzec,
            a,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(
            MORPHO,
            IZZEC(address(0)),
            a,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(
            MORPHO, zzec, address(0), address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, signerSet, _noProofs()
        );
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(
            MORPHO, zzec, a, address(0), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, signerSet, _noProofs()
        );
        vm.expectRevert(IZecuredRegistry.ZeroAddress.selector);
        new ZecuredRegistry(
            MORPHO, zzec, a, address(usdc), address(0), ADAPTIVE_CURVE_IRM, LLTV, signerSet, _noProofs()
        );
    }

    function test_constructor_revertsWhenZZECIsWiredElsewhere() public {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);

        // zZEC built for another registry.
        ZZEC wrongRegistry = new ZZEC(stranger, predictedAdapter, address(MORPHO));
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(
            MORPHO,
            wrongRegistry,
            predictedAdapter,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );

        nonce = vm.getNonce(address(this));
        predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        // zZEC built for another adapter.
        ZZEC wrongAdapter = new ZZEC(predictedRegistry, stranger, address(MORPHO));
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(
            MORPHO,
            wrongAdapter,
            predictedAdapter,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );

        nonce = vm.getNonce(address(this));
        predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);
        // zZEC built for another Morpho.
        ZZEC wrongMorpho = new ZZEC(predictedRegistry, predictedAdapter, stranger);
        vm.expectRevert(IZecuredRegistry.ZZECMisconfigured.selector);
        new ZecuredRegistry(
            MORPHO,
            wrongMorpho,
            predictedAdapter,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            _noProofs()
        );
    }

    function _deployZZECForNextRegistry() internal returns (ZZEC z, address predictedAdapter) {
        uint64 nonce = vm.getNonce(address(this));
        predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);
        z = new ZZEC(vm.computeCreateAddress(address(this), nonce + 1), predictedAdapter, address(MORPHO));
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
            (ZZEC z, address a) = _deployZZECForNextRegistry();
            vm.expectRevert(IZecuredRegistry.InvalidSignerSet.selector);
            new ZecuredRegistry(
                MORPHO, z, a, address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, bad[i], _noProofs()
            );
        }
    }

    function test_constructor_revertsWhenMorphoRefusesTheMarket() public {
        (ZZEC z, address a) = _deployZZECForNextRegistry();
        vm.expectRevert(bytes("IRM not enabled"));
        new ZecuredRegistry(MORPHO, z, a, address(usdc), address(oracle), stranger, LLTV, signerSet, _noProofs());
        (z, a) = _deployZZECForNextRegistry();
        vm.expectRevert(bytes("LLTV not enabled"));
        new ZecuredRegistry(
            MORPHO, z, a, address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, 0.6e18, signerSet, _noProofs()
        );
    }

    function test_adapterConstructor_revertsAtAnUnexpectedAddress() public {
        // The fixture's Registry expects its Adapter at a fixed address; any other deployment is refused.
        vm.expectRevert(ISettlementAdapter.AdapterMisconfigured.selector);
        new SettlementAdapter(registry);
    }

    // ================================================================== attestDeposit

    function test_attestDeposit_activatesVaultAndSuppliesCollateral() public {
        DepositAttestation memory att = _attestation(account, VAULT_005);
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        assertEq(account.code.length, 0, "counterfactual account: no code, no transaction yet");

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(att.vaultId, account, VAULT_005, att.depositTag);
        vm.expectEmit(address(MORPHO));
        emit EventsLib.SupplyCollateral(marketId, address(registry), account, VAULT_005);
        vm.prank(stranger); // anyone may relay
        registry.attestDeposit(att, sigs);

        (address vAccount, uint64 outstanding, VaultStatus status) = registry.vault(att.vaultId);
        assertEq(vAccount, account);
        assertEq(outstanding, VAULT_005);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(registry.vaultOf(account), att.vaultId);
        assertTrue(registry.depositTagUsed(att.depositTag));
        assertEq(registry.claimCount(att.vaultId), 0);

        Position memory p = _position(account);
        assertEq(p.collateral, VAULT_005);
        assertEq(p.borrowShares, 0);
        assertEq(zzec.totalSupply(), VAULT_005, "I1");
        _assertZZECOnlyInMorpho(_list(account, stranger));
        assertEq(zzec.allowance(address(registry), address(MORPHO)), 0, "exact approval consumed");
        assertFalse(MORPHO.isAuthorized(account, address(registry)), "supplyCollateral needs no authorization");
    }

    function test_attestDeposit_acceptsAnyThreeOrMoreDistinctSignersInOrder() public {
        uint256[][] memory sets = new uint256[][](5);
        sets[0] = _idx(0, 1, 2);
        sets[1] = _idx(2, 3, 4);
        sets[2] = _idx(0, 3, 4);
        sets[3] = new uint256[](4);
        (sets[3][0], sets[3][1], sets[3][2], sets[3][3]) = (0, 1, 3, 4);
        sets[4] = new uint256[](5);
        (sets[4][0], sets[4][1], sets[4][2], sets[4][3], sets[4][4]) = (0, 1, 2, 3, 4);
        uint256 total;
        for (uint256 i; i < sets.length; ++i) {
            address acct = makeAddr(string.concat("zecured.registry.multi", vm.toString(i)));
            DepositAttestation memory att = _attestation(acct, VAULT_001);
            registry.attestDeposit(att, _signerSigs(registry.hashDepositAttestation(att), sets[i]));
            total += VAULT_001;
            assertEq(_outstanding(att.vaultId), VAULT_001);
        }
        assertEq(zzec.totalSupply(), total, "I1");
    }

    function test_attestDeposit_revertsWithFewerThanThreeSignatures() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32 digest = registry.hashDepositAttestation(att);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InsufficientSignatures.selector, 2));
        registry.attestDeposit(att, _signerSigs(digest, _idx(0, 4)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InsufficientSignatures.selector, 0));
        registry.attestDeposit(att, new bytes[](0));
    }

    function test_attestDeposit_revertsWithThreeSignaturesFromTwoSigners() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32 digest = registry.hashDepositAttestation(att);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 2));
        registry.attestDeposit(att, _signerSigs(digest, _idx(0, 1, 1)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 1));
        registry.attestDeposit(att, _signerSigs(digest, _idx(3, 3, 4)));
        // Four signatures, still only two distinct signers.
        uint256[] memory four = new uint256[](4);
        (four[0], four[1], four[2], four[3]) = (1, 1, 2, 2);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 1));
        registry.attestDeposit(att, _signerSigs(digest, four));
    }

    function test_attestDeposit_revertsWithUnsortedSignatures() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32 digest = registry.hashDepositAttestation(att);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 1));
        registry.attestDeposit(att, _signerSigs(digest, _idx(1, 0, 2)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 2));
        registry.attestDeposit(att, _signerSigs(digest, _idx(0, 4, 2)));
    }

    function test_attestDeposit_revertsWithANonSignerSignature() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32 digest = registry.hashDepositAttestation(att);
        (address outsider, uint256 outsiderKey) = makeAddrAndKey("zecured.registry.outsider");
        bytes[] memory sigs = _signerSigs(digest, _idx(0, 1, 2));
        sigs[2] = _sign(outsiderKey, digest);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.UnknownSigner.selector, outsider));
        registry.attestDeposit(att, sigs);
    }

    function test_attestDeposit_revertsWhenTheSignedDataDiffers() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        DepositAttestation memory other = _attestation(account, VAULT_005); // signers attested another amount
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(other));
        vm.expectPartialRevert(IZecuredRegistry.UnknownSigner.selector);
        registry.attestDeposit(att, sigs);
    }

    /// Every request kind has its own type: a PayoutConfirmation signature never verifies as a DepositAttestation,
    /// and a signature without the Registry domain (raw struct hash) is refused too.
    function test_attestDeposit_revertsWithSignaturesOfAnotherTypeOrDomain() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = att.vaultId;
        bytes[] memory payoutSigs = _threeSigs(registry.hashPayoutConfirmation(att.vaultId, ids));
        vm.expectPartialRevert(IZecuredRegistry.UnknownSigner.selector);
        registry.attestDeposit(att, payoutSigs);
        bytes[] memory rawSigs = _threeSigs(ZecuredEIP712.hashDepositAttestation(att));
        vm.expectPartialRevert(IZecuredRegistry.UnknownSigner.selector);
        registry.attestDeposit(att, rawSigs);
    }

    function test_attestDeposit_revertsWithMalleableOrMalformedSignatures() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes32 digest = registry.hashDepositAttestation(att);
        bytes[] memory sigs = _threeSigs(digest);

        // High-s twin of a valid signature: same signer for ecrecover, rejected by OpenZeppelin ECDSA.
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKeys[2], digest);
        bytes32 highS = bytes32(SECP256K1_N - uint256(s));
        sigs[1] = abi.encodePacked(r, highS, v == 27 ? uint8(28) : uint8(27));
        vm.expectRevert(abi.encodeWithSelector(ECDSA.ECDSAInvalidSignatureS.selector, highS));
        registry.attestDeposit(att, sigs);

        // 64-byte (ERC-2098 compact) signatures are not accepted.
        sigs = _threeSigs(digest);
        (bytes32 cr, bytes32 vs) = vm.signCompact(signerKeys[2], digest);
        sigs[1] = abi.encodePacked(cr, vs);
        vm.expectRevert(abi.encodeWithSelector(ECDSA.ECDSAInvalidSignatureLength.selector, 64));
        registry.attestDeposit(att, sigs);
    }

    function test_attestDeposit_revertsOnReplay() public {
        DepositAttestation memory att = _attestation(account, VAULT_001);
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        registry.attestDeposit(att, sigs);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, att.vaultId));
        registry.attestDeposit(att, sigs);
        assertEq(zzec.totalSupply(), VAULT_001, "no second mint");
    }

    function test_attestDeposit_revertsOnReusedVaultId() public {
        DepositAttestation memory first = _attestation(account, VAULT_001);
        registry.attestDeposit(first, _threeSigs(registry.hashDepositAttestation(first)));
        address other = makeAddr("zecured.registry.other");
        DepositAttestation memory att = _attestation(other, VAULT_001);
        att.vaultId = first.vaultId; // fresh account and tag, reused vault id
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, first.vaultId));
        registry.attestDeposit(att, sigs);
    }

    function test_attestDeposit_revertsOnReusedDepositTag() public {
        DepositAttestation memory first = _attestation(account, VAULT_001);
        registry.attestDeposit(first, _threeSigs(registry.hashDepositAttestation(first)));
        address other = makeAddr("zecured.registry.other");
        DepositAttestation memory att = _attestation(other, VAULT_001);
        att.depositTag = first.depositTag; // fresh account and vault id, reused note tag
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, first.depositTag));
        registry.attestDeposit(att, sigs);
    }

    function test_attestDeposit_revertsOnASecondVaultForTheSameAccount() public {
        bytes32 first = _attest(account, VAULT_001);
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256("zecured.registry.secondVault"),
            account: account,
            amount: VAULT_001,
            depositTag: keccak256("zecured.registry.secondTag")
        });
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, account, first));
        registry.attestDeposit(att, sigs);

        // Still refused after the first vault is closed: one account <-> one vault, forever (I4).
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(account);
        registry.requestRelease(first, RELEASE_COMMITMENT);
        assertEq(uint8(_status(first)), uint8(VaultStatus.Closed));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, account, first));
        registry.attestDeposit(att, sigs);
    }

    function test_attestDeposit_revertsOnZeroFields() public {
        // (Signatures are computed before vm.expectRevert: the digest view would otherwise be "the next call".)
        DepositAttestation memory att = _attestation(account, VAULT_001);
        att.vaultId = bytes32(0);
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredRegistry.ZeroVaultId.selector);
        registry.attestDeposit(att, sigs);

        att = _attestation(account, VAULT_001);
        att.account = address(0);
        sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredRegistry.ZeroAccount.selector);
        registry.attestDeposit(att, sigs);

        att = _attestation(account, 0);
        sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        registry.attestDeposit(att, sigs);

        att = _attestation(account, VAULT_001);
        att.depositTag = bytes32(0);
        sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredRegistry.ZeroDepositTag.selector);
        registry.attestDeposit(att, sigs);
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
            registry.recordLiquidation(vaultId, uint64(MIN_SEIZE), LIQUIDATOR_COMMITMENT);
            vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyAdapter.selector, callers[i]));
            registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, USER_COMMITMENT);
            vm.stopPrank();
        }
    }

    function test_recordLiquidation_reverts() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.startPrank(address(adapter));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.recordLiquidation(unknown, 1, LIQUIDATOR_COMMITMENT);
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        registry.recordLiquidation(vaultId, 0, LIQUIDATOR_COMMITMENT);
        // I9: never more than the outstanding.
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.SeizeExceedsOutstanding.selector, vaultId, VAULT_005 + 1, VAULT_005)
        );
        registry.recordLiquidation(vaultId, VAULT_005 + 1, LIQUIDATOR_COMMITMENT);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordLiquidation(vaultId, 1, bytes32(0));
        // The Adapter must actually hold the seized zZEC.
        vm.expectRevert(abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, address(adapter), 0, 1));
        registry.recordLiquidation(vaultId, 1, LIQUIDATOR_COMMITMENT);
        vm.stopPrank();
    }

    function test_recordClose_reverts() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 unknown = keccak256("zecured.registry.unknownVault");
        vm.startPrank(address(adapter));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultNotActive.selector, unknown));
        registry.recordClose(unknown, 1, 1, MM_COMMITMENT, USER_COMMITMENT);
        // I7: the two parts must add up to the outstanding exactly.
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, 1, VAULT_005, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005, MM_COMMITMENT, USER_COMMITMENT);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, 1, VAULT_005 - 2, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005 - 2, MM_COMMITMENT, USER_COMMITMENT);
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredRegistry.CloseAmountsMismatch.selector, vaultId, type(uint64).max, 1, VAULT_005
            )
        );
        registry.recordClose(vaultId, type(uint64).max, 1, MM_COMMITMENT, USER_COMMITMENT);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordClose(vaultId, 1, VAULT_005 - 1, bytes32(0), USER_COMMITMENT);
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, bytes32(0));
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, address(adapter), 0, VAULT_005)
        );
        registry.recordClose(vaultId, 1, VAULT_005 - 1, MM_COMMITMENT, USER_COMMITMENT);
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
        bytes32 digest = registry.hashPayoutConfirmation(vaultId, ids);

        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InsufficientSignatures.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 1)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 2));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(0, 2, 2)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 1));
        registry.confirmPayout(vaultId, ids, _signerSigs(digest, _idx(4, 2, 3)));

        // Signatures over another claim list (here: an extra id) do not verify.
        bytes32[] memory more = _ids(claimId, keccak256("zecured.registry.otherClaim"));
        bytes[] memory otherListSigs = _threeSigs(registry.hashPayoutConfirmation(vaultId, more));
        vm.expectPartialRevert(IZecuredRegistry.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, otherListSigs);
        // ... nor do DepositAttestation signatures.
        bytes[] memory attestationSigs = _threeSigs(registry.hashDepositAttestation(_attestation(account, VAULT_005)));
        vm.expectPartialRevert(IZecuredRegistry.UnknownSigner.selector);
        registry.confirmPayout(vaultId, ids, attestationSigs);
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
