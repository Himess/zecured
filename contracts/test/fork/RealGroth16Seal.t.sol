// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZecuredRegistry, VaultStatus, DepositProofConfig, MintMode} from "../../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {ZcashHeaderRelay} from "../../src/relay/ZcashHeaderRelay.sol";
import {RelayFixtures} from "../relay/RelayFixtures.sol";
import {SignerBond} from "../../src/SignerBond.sol";
import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {ViolationKind} from "../../src/interfaces/IZecuredSlasher.sol";
import {DepositAttestation} from "../../src/libraries/ZecuredEIP712.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The real deposit of DepositProofs.t.sol with a REAL Groth16 seal: nothing in the path is a MOCK. RISC Zero's
///         verifier router on Base Sepolia checks the seal, the real header relay checks the anchor, and the Registry
///         runs as the judge deployment does (`ProofOnly`, K1).
/// @dev The seal proves the deposit statement (tier2/statement) on the TEST-ONLY vault 9 witness
///      (tier2/statement/tests/fixtures) for the guest's reproducible image id. The STARK was proven on an RTX 4090 and
///      wrapped with RISC Zero's Docker Groth16 prover (docs/TESTING.md § 2b).
contract RealGroth16SealTest is ZecuredFixture, RelayFixtures {
    /// RISC Zero's verifier router on Base Sepolia (docs/TIER2.md § 5).
    IRiscZeroVerifier internal constant ROUTER = IRiscZeroVerifier(0x0b144E07A0826182B6b59788c34b32Bfa86Fb711);
    /// The deposit guest's reproducible image id (the `guests` workflow).
    bytes32 internal constant IMAGE_ID = 0x62feed13b2948aa427275fb6003e717ccc88945d033b99b196936ca99a80cdf5;
    /// The router's Groth16 verifier, version 3.0.0.
    bytes4 internal constant GROTH16 = 0x73c457ba;

    string internal constant ANCHOR = "test/relay/fixtures/testnet_deposit_anchor.json";
    uint256 internal constant MIN_WORK = 1_000_000;
    uint64 internal constant FINALIZE_DEPTH = 6;
    uint64 internal constant FINALIZE_DELAY = 150;
    uint256 internal constant CONFIRMED = 4;

    // The real deposit's journal, as in DepositProofs.t.sol.
    bytes32 internal constant TIP = 0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000; // 4,488,560
    bytes32 internal constant VAULT_ID = 0x482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b;
    address internal constant ACCOUNT = 0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F;
    uint64 internal constant AMOUNT = 1_000_000;
    bytes32 internal constant TAG = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
    bytes32 internal constant OPENED = 0x385c78ae562975cc3c03af28374fd4e9d9e3f3405a18fbf81fe16d4f9e28c0f5;

    /// The Groth16 seal of that journal, with the router's selector in front (`zecured-tier2-host groth16`).
    bytes internal constant SEAL = hex"73c457ba26aabd16a21f26e3f94dbe4d02921fa00004fa005de989ee97a0211cca3bcceb2982a6e7cba92e47a6b3c1c6"
        hex"631f9200ef45650431b540ac856badc2a2a250f01ebb1a84b34dbc8512bedcbb52e6168f4c263afa6fafba9f244fed67"
        hex"a893c42f02a93367a43a5c37b2ee4f954fb6b303797078ea5935a6d23cbda7d743442dc80f5d2c889a6c8a20dd13be9e"
        hex"177086eec91b2c6c5943acd12a237b65256f212819526d50eb9252701f20d2eb7d69ddf74ee254328085f729b95d9c56"
        hex"211ddc220e364d3dda1c9fa422ad09875e86e1b51d74d40e387bbd3bfa1f4ce98ac510ff1b6cb497c3c42854128ec617"
        hex"52e80a1c2bf53f6b9557710b6ecb2af825cab82e";

    ZcashHeaderRelay internal relay;
    RealFixture internal anchor;
    uint256 internal relayed;

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        anchor = _loadReal(ANCHOR);
        relay = _deploy(anchor);
        return DepositProofConfig({
            verifier: ROUTER,
            imageId: IMAGE_ID,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: MIN_WORK,
            mintMode: MintMode.ProofOnly,
            sealSelector: GROTH16,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    /// Submits the anchor's headers up to 4,488,560 + n - 1 (those not submitted yet).
    function _relayHeaders(uint256 n) internal {
        for (uint256 i = relayed; i < n; ++i) {
            _warpTo(anchor.headers[i]);
            relay.submitHeader(anchor.headers[i]);
        }
        if (n > relayed) relayed = n;
    }

    function _journal(uint64 amount) internal pure returns (bytes memory) {
        return abi.encode(TIP, VAULT_ID, ACCOUNT, amount, TAG, OPENED);
    }

    function _openedSigs() internal view returns (bytes[] memory) {
        uint256[] memory all5 = new uint256[](5);
        (all5[0], all5[1], all5[2], all5[3], all5[4]) = (0, 1, 2, 3, 4);
        return _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), all5);
    }

    function test_realSeal_isAcceptedByTheRouter() public view {
        ROUTER.verify(SEAL, IMAGE_ID, sha256(_journal(AMOUNT)));
    }

    function test_realSeal_mintsTheRealDepositThroughTheRealRouter() public {
        _relayHeaders(CONFIRMED);
        uint256 gasBefore = gasleft();
        registry.attestDepositWithProof(_journal(AMOUNT), SEAL, _openedSigs());
        emit log_named_uint("attestDepositWithProof gas, real Groth16 verification included", gasBefore - gasleft());

        // K3: minted once the relay has moved FINALIZE_DEPTH headers on and the anchor is still on its best chain.
        _relayHeaders(CONFIRMED + FINALIZE_DEPTH);
        (,,,, uint256 readyHeight, uint256 readyTime) = registry.pendingActivation(VAULT_ID);
        (, uint256 height,) = relay.bestChainTip();
        assertGe(height, readyHeight, "the relay is past the ready height");
        if (block.timestamp < readyTime) vm.warp(readyTime);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT_ID, ACCOUNT, AMOUNT, TAG);
        registry.finalizeActivation(VAULT_ID);

        (address account, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(account, ACCOUNT);
        assertEq(outstanding, AMOUNT);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), AMOUNT, "I1: the proven amount, minted");
        Position memory p = MORPHO.position(registry.marketId(), ACCOUNT);
        assertEq(p.collateral, AMOUNT, "supplied straight into the account's Morpho position");
        assertEq(zzec.balanceOf(ACCOUNT), 0, "I2: the account holds no zZEC");
    }

    /// The seal proves this journal only: one more zatoshi and the router refuses it.
    function test_realSeal_provesNoOtherJournal() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _journal(AMOUNT + 1);
        bytes[] memory sigs = _openedSigs();
        vm.expectRevert();
        registry.attestDepositWithProof(journal, SEAL, sigs);
    }
}

/// @notice An F1 violation proven with a REAL Groth16 seal: the TEST-ONLY vault 9's real Testnet payout, which spent its
///         deposit note without a commitment, slashes all five bonds through RISC Zero's verifier router on Base Sepolia.
///         The journal is Tier3Slashing.t.sol's cross-language vector; only the header relay is a MOCK here, as in the
///         Tier 3 suites (the theft's anchor is not among the relay fixtures).
/// @dev The seal proves the violation statement on the TEST-ONLY F1 witness (tier2/statement/tests/fixtures) for the
///      guest's reproducible image id (docs/TESTING.md § 2b).
contract RealViolationSealTest is ZecuredFixture {
    IRiscZeroVerifier internal constant ROUTER = IRiscZeroVerifier(0x0b144E07A0826182B6b59788c34b32Bfa86Fb711);
    bytes32 internal constant DEPOSIT_IMAGE = 0x62feed13b2948aa427275fb6003e717ccc88945d033b99b196936ca99a80cdf5;
    bytes32 internal constant VIOLATION_IMAGE = 0x7d8c9dd23e813e0e44ffea2b18ec9ec451b52a8a7b634ae998583d49b73cc526;
    bytes4 internal constant GROTH16 = 0x73c457ba;
    uint256 internal constant SLASH_WORK = 38_000_000;
    uint64 internal constant WINDOW = 1 days;

    bytes32 internal constant TAG = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
    bytes32 internal constant THEFT_ANCHOR = 0xe47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000;
    address internal constant BENEFICIARY = 0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe;

    /// The F1 journal (tier2/statement, violations.rs) and its Groth16 seal.
    bytes internal constant JOURNAL = hex"0000000000000000000000000000000000000000000000000000000000000001"
        hex"e47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000"
        hex"000000000000000000000000bebebebebebebebebebebebebebebebebebebebe"
        hex"0a20962f2e701d49033b677adf918a7f8a38e3115e52f52ea3567140930e25ba"
        hex"d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000";
    bytes internal constant SEAL = hex"73c457ba23d8d3c365b896112fa5fadab4e90c0d22dbe47b3efceffac94763f48d729e64151278520eb65541879028b9"
        hex"4ba237670e6754f4262e40e79f0ffa04a18a7e7825b66b0f678fe3503da663410469126e0a03c72df212c420b875c2c4"
        hex"0439ed94144ff606fb587bffce52b895f23fc8858b86f05a1a49ccb114dc52ad97e0b19d2adf0b8d1314a70ff38f196c"
        hex"a3357956fb15d0bbdaf0fe288e13be7431bf53ad03e479e7547d006e8ae1a1730d83d1f18f2e541408482faf72874f11"
        hex"5f9745ad1f19b1d27bf23d4af0b0d6c3679c546eaa2652f850c6bbdf4fa72bf02996d5b9030d82648f0fe36b184ac037"
        hex"523507b54765da344c4ba584acccf3a078d6dad2";

    MockHeaderRelay internal relay;
    SignerBond internal bond;
    /// The nonce the bond predicted for the slasher.
    uint64 internal slasherNonce;
    ZecuredSlasher internal slasher;

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        relay = new MockHeaderRelay();
        return DepositProofConfig({
            verifier: ROUTER,
            imageId: DEPOSIT_IMAGE,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: 3_500_000,
            mintMode: MintMode.SignerAttested, // vault 9 is opened by attestation here
            sealSelector: GROTH16,
            finalizeDepth: 6,
            finalizeDelay: 150
        });
    }

    /// The bond at the next nonce, then ZZEC, Registry and Adapter (the fixture), then the slasher (setUp).
    function _signerBond() internal override returns (ISignerBond) {
        uint64 n = vm.getNonce(address(this));
        slasherNonce = n + 4;
        bond = new SignerBond(
            vm.computeCreateAddress(address(this), n + 2), vm.computeCreateAddress(address(this), n + 4), WINDOW
        );
        return ISignerBond(address(bond));
    }

    function setUp() public override {
        super.setUp();
        // `forge test --isolate` runs each call as a transaction, which also advances this contract's nonce: put it
        // back where the bond expects the slasher (a no-op without --isolate).
        vm.setNonceUnsafe(address(this), slasherNonce);
        slasher = new ZecuredSlasher(
            address(registry),
            address(bond),
            address(relay),
            address(ROUTER),
            VIOLATION_IMAGE,
            GROTH16,
            SLASH_WORK,
            WINDOW
        );
        assertEq(bond.slasher(), address(slasher), "slasher address prediction");
        for (uint256 i; i < 5; ++i) {
            vm.deal(signerSet[i], 1 ether);
            vm.prank(signerSet[i]);
            bond.deposit{value: 0.01 ether}();
        }
    }

    function _totalBonds() internal view returns (uint256 sum) {
        for (uint8 i; i < 5; ++i) {
            sum += bond.bondOf(i);
        }
    }

    function test_realSeal_slashesAllFiveBondsForTheRealUncommittedSpend() public {
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256("zecured.tier3.vault9"),
            account: makeAddr("zecured.tier3.vault9"),
            amount: VAULT_001,
            depositTag: TAG
        });
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att)));
        relay.setBlock(THEFT_ANCHOR, true, SLASH_WORK);

        uint256 gasBefore = gasleft();
        bytes32 caseId = slasher.proveViolation(ViolationKind.Uncommitted, JOURNAL, SEAL);
        emit log_named_uint("proveViolation gas, real Groth16 verification included", gasBefore - gasleft());
        assertEq(slasher.caseOf(caseId).vaultId, att.vaultId);

        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0, "all five bonds slashed");
        assertEq(bond.bountyOf(BENEFICIARY), 0.005 ether, "the bounty goes to the journal's beneficiary");
        assertTrue(bond.halted(), "minting halted");
    }

    /// The seal proves this journal only: another beneficiary and the router refuses it.
    function test_realSeal_provesNoOtherBeneficiary() public {
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256("zecured.tier3.vault9"),
            account: makeAddr("zecured.tier3.vault9"),
            amount: VAULT_001,
            depositTag: TAG
        });
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att)));
        relay.setBlock(THEFT_ANCHOR, true, SLASH_WORK);
        bytes memory journal = JOURNAL;
        journal[95] = 0xbf; // the beneficiary's last byte
        vm.expectRevert();
        slasher.proveViolation(ViolationKind.Uncommitted, journal, SEAL);
    }
}
