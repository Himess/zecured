// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredSpendLogic} from "../../src/registry/ZecuredSpendLogic.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {RegistryConfig} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {MockMorphoForHalmos, ZZECWiringStub, SpendLogicWiringStub} from "./RegistryRules.t.sol";

/// @notice MOCK: the slasher views the spend logic reads (nothing frozen or compromised unless set).
contract SlasherViewsStub {
    mapping(bytes32 vaultId => uint64) public frozenSince;
    mapping(bytes32 vaultId => bool) public isCompromised;
    mapping(bytes32 vaultId => bool) public vaultFrozen;

    function setFrozenSince(bytes32 vaultId, uint64 at) external {
        frozenSince[vaultId] = at;
        vaultFrozen[vaultId] = at != 0;
    }
}

/// @notice Symbolic checks of the v4 payout rules with Halmos (docs/TESTING.md § 1b) on the real Registry and spend
///         logic (served by the Registry's fallback), with Morpho and the slasher's views MOCKs:
///         P1  proven delivery: `upholdPayout` succeeds exactly for a claim that is pending, disputed or confirmed by
///             signatures (not final, not closed) and a payout reference recorded for the claim's input and vault; the
///             claim is then paid for good, and the books (unpaid claims, `lockedZatoshi`'s pending zatoshi, the
///             unproven confirmations) move by exactly that claim;
///         P2  R-3: `expireConfirmation` makes a confirmation final exactly when its dispute window is over and its
///             vault did not freeze within that window.
/// @dev The claim's state is written into the Registry's storage (white box, slots and offsets from
///      `forge inspect ZecuredRegistry storageLayout`, which check_storage_layout.py also pins), with symbolic fields:
///      reaching each state through signed commitments and confirmations would leave Halmos with keccak and ecrecover
///      terms it cannot finish on.
contract PayoutRulesCheck is Test {
    address internal constant ADAPTER = address(0x2000);
    address internal constant INTAKE = address(0x3000);
    address internal constant BOND = address(0x3001);
    address internal constant SIGNER0 = address(0x5101);
    address internal constant SIGNER1 = address(0x5102);
    address internal constant SIGNER2 = address(0x5103);
    address internal constant SIGNER3 = address(0x5104);
    address internal constant SIGNER4 = address(0x5105);
    bytes32 internal constant VAULT = keccak256("vault");
    bytes32 internal constant OTHER_VAULT = keccak256("other vault");
    bytes32 internal constant CLAIM = keccak256("claim");
    bytes32 internal constant INPUT = keccak256("input");
    bytes32 internal constant REF = keccak256("payout");
    bytes32 internal constant OTHER_REF = keccak256("other payout");
    uint64 internal constant AMOUNT = 1_000_000;
    uint64 internal constant DISPUTE_WINDOW = 1 days;

    // ZecuredRegistryStorage's slots.
    uint256 internal constant VAULTS_SLOT = 2;
    uint256 internal constant CLAIMS_SLOT = 4;
    uint256 internal constant SPENDS_SLOT = 8;
    uint256 internal constant CLAIM_INPUT_SLOT = 10;
    uint256 internal constant PENDING_CLAIM_ZAT_SLOT = 16;
    uint256 internal constant REOPENED_SLOT = 17;
    uint256 internal constant UNPROVEN_SLOT = 19;
    // ClaimStatus values.
    uint8 internal constant PENDING = 1;
    uint8 internal constant PAID = 2;
    uint8 internal constant DISPUTED = 5;

    ZecuredRegistry internal registry;
    SlasherViewsStub internal slasher;

    function setUp() public {
        MockMorphoForHalmos morpho = new MockMorphoForHalmos();
        address token = address(new ZZECWiringStub(ADAPTER, address(morpho)));
        address[5] memory signers = [SIGNER0, SIGNER1, SIGNER2, SIGNER3, SIGNER4];
        address logic = address(new SpendLogicWiringStub(signers));
        slasher = new SlasherViewsStub();
        registry = new ZecuredRegistry(
            RegistryConfig({
                morpho: IMorpho(address(morpho)),
                zzec: IZZEC(token),
                adapter: ADAPTER,
                loanToken: address(0x6001),
                oracle: address(0x6002),
                irm: address(0x6003),
                lltv: 0.77e18,
                signers: signers,
                spendLogic: logic,
                intake: INTAKE,
                signerBond: BOND,
                slasher: address(slasher),
                compensator: address(0),
                feeBps: 0,
                minFeeZat: 0,
                feeCommitment: bytes32(0),
                refundAbandonPeriod: 30 days
            })
        );
        vm.etch(token, address(new ZZEC(address(registry), ADAPTER, address(morpho))).code);
        vm.etch(logic, address(new ZecuredSpendLogic(address(registry), signers, DISPUTE_WINDOW, 6 hours)).code);
    }

    // ------------------------------------------------------------------ P1: proven delivery

    /// `others`: whether the vault has another unpaid claim and another open confirmation, so that a wrong decrement
    /// of the books shows instead of underflowing.
    function check_upholdPayout_acceptsExactlyTheProofTransitions(
        uint8 status,
        bool confirmed,
        bool wasReopened,
        bool bound,
        bool sameVault,
        bool rightRef,
        bool others
    ) public {
        // A confirmation time only ever exists on a Paid claim (`disputePayout` and every proof clear it).
        vm.assume(status <= 5 && (status == PAID || !confirmed));
        uint40 paidAt = confirmed ? 1_000 : 0;
        bool unpaid = status == PENDING || status == DISPUTED;
        bool open = status == PAID && paidAt != 0;
        _writeClaim(status, paidAt, status == DISPUTED ? 2_000 : 0, wasReopened, bound, sameVault, unpaid, open);
        uint256 extra = others ? 1 : 0;
        _addOthers(extra, unpaid, open);
        bytes32 ref = rightRef ? REF : OTHER_REF;

        vm.prank(address(slasher));
        (bool ok,) = address(registry).call(abi.encodeCall(IZecuredDisputes.upholdPayout, (CLAIM, ref)));

        bool upholdable = unpaid || open;
        bool ofTheClaim = bound && sameVault && rightRef;
        assert(ok == (upholdable && ofTheClaim));
        if (!ok) return;
        (uint8 statusAfter, uint40 paidAfter, uint40 dueAfter) = _claimState();
        assert(statusAfter == PAID && paidAfter == 0 && dueAfter == 0);
        // The books move by exactly this claim: what was counted for it is gone, the others' counts stay.
        assert(_unpaidClaims() == extra);
        assert(uint256(vm.load(address(registry), bytes32(PENDING_CLAIM_ZAT_SLOT))) == extra * AMOUNT);
        assert(IZecuredPayouts(address(registry)).unprovenConfirmations(VAULT) == extra);
    }

    /// Another claim of the vault counted unpaid, and another open confirmation, when `extra` is 1.
    function _addOthers(uint256 extra, bool unpaid, bool open) internal {
        uint256 count = (unpaid ? 1 : 0) + extra;
        bytes32 vaultBase = keccak256(abi.encode(VAULT, VAULTS_SLOT));
        vm.store(address(registry), bytes32(uint256(vaultBase) + 1), bytes32(uint256(2) | (count << 64)));
        vm.store(address(registry), bytes32(PENDING_CLAIM_ZAT_SLOT), bytes32(count * AMOUNT));
        vm.store(address(registry), keccak256(abi.encode(VAULT, UNPROVEN_SLOT)), bytes32((open ? 1 : 0) + extra));
    }

    /// Only the slasher proves.
    function check_upholdPayout_onlyTheSlasher(address caller, uint8 status) public {
        vm.assume(caller != address(slasher) && status <= 5);
        _writeClaim(status, 0, 0, false, true, true, status == PENDING || status == DISPUTED, false);
        vm.prank(caller);
        (bool ok,) = address(registry).call(abi.encodeCall(IZecuredDisputes.upholdPayout, (CLAIM, REF)));
        assert(!ok);
    }

    // ------------------------------------------------------------------ P2: R-3 expiry

    function check_expireConfirmation_onlyOldConfirmationsBeforeAFreeze(
        uint8 status,
        uint40 paidAt,
        uint64 frozenSince,
        uint64 timestamp,
        bool others
    ) public {
        vm.assume(status <= 5 && paidAt < type(uint40).max - DISPUTE_WINDOW);
        bool open = status == PAID && paidAt != 0;
        bool unpaid = status == PENDING || status == DISPUTED;
        _writeClaim(status, paidAt, 0, false, true, true, unpaid, open);
        uint256 extra = others ? 1 : 0;
        _addOthers(extra, unpaid, open);
        slasher.setFrozenSince(VAULT, frozenSince);
        vm.warp(timestamp);

        (bool ok,) = address(registry).call(abi.encodeCall(IZecuredPayouts.expireConfirmation, (CLAIM)));

        bool windowOver = uint256(timestamp) > uint256(paidAt) + DISPUTE_WINDOW;
        bool suspect = frozenSince != 0 && uint256(paidAt) + DISPUTE_WINDOW > frozenSince;
        assert(ok == (open && windowOver && !suspect));
        if (!ok) return;
        (uint8 statusAfter, uint40 paidAfter,) = _claimState();
        assert(statusAfter == PAID && paidAfter == 0);
        assert(IZecuredPayouts(address(registry)).unprovenConfirmations(VAULT) == extra);
    }

    // ------------------------------------------------------------------ white box

    /// One claim of VAULT (an active vault), of AMOUNT, in the given state; bound to INPUT if `bound`; the payout REF
    /// recorded as spending INPUT, for VAULT or another vault; the books counting it as `unpaid` or as an `open`
    /// confirmation.
    function _writeClaim(
        uint8 status,
        uint40 paidAt,
        uint40 dueAt,
        bool wasReopened,
        bool bound,
        bool sameVault,
        bool unpaid,
        bool open
    ) internal {
        bytes32 claimBase = keccak256(abi.encode(CLAIM, CLAIMS_SLOT));
        vm.store(address(registry), claimBase, VAULT);
        vm.store(address(registry), bytes32(uint256(claimBase) + 1), keccak256("commitment"));
        uint256 word = uint256(AMOUNT) | (uint256(status) << 72) | (uint256(1) << 80) | (uint256(paidAt) << 128)
            | (uint256(dueAt) << 168);
        vm.store(address(registry), bytes32(uint256(claimBase) + 2), bytes32(word));
        vm.store(address(registry), keccak256(abi.encode(CLAIM, REOPENED_SLOT)), bytes32(uint256(wasReopened ? 1 : 0)));
        vm.store(address(registry), keccak256(abi.encode(CLAIM, CLAIM_INPUT_SLOT)), bound ? INPUT : bytes32(0));

        bytes32 spendBase = keccak256(abi.encode(REF, SPENDS_SLOT));
        vm.store(address(registry), bytes32(uint256(spendBase) + 1), sameVault ? VAULT : OTHER_VAULT);
        vm.store(address(registry), bytes32(uint256(spendBase) + 2), INPUT);

        // The vault: active (status 1 at byte 28), one claim, `unpaid` of them unpaid.
        bytes32 vaultBase = keccak256(abi.encode(VAULT, VAULTS_SLOT));
        vm.store(address(registry), vaultBase, bytes32(uint256(1) << 224));
        vm.store(
            address(registry), bytes32(uint256(vaultBase) + 1), bytes32(uint256(1) | (uint256(unpaid ? 1 : 0) << 64))
        );
        vm.store(address(registry), bytes32(PENDING_CLAIM_ZAT_SLOT), bytes32(uint256(unpaid ? AMOUNT : 0)));
        vm.store(address(registry), keccak256(abi.encode(VAULT, UNPROVEN_SLOT)), bytes32(uint256(open ? 1 : 0)));
    }

    function _claimState() internal view returns (uint8 status, uint40 paidAt, uint40 dueAt) {
        uint256 word =
            uint256(vm.load(address(registry), bytes32(uint256(keccak256(abi.encode(CLAIM, CLAIMS_SLOT))) + 2)));
        status = uint8(word >> 72);
        paidAt = uint40(word >> 128);
        dueAt = uint40(word >> 168);
    }

    function _unpaidClaims() internal view returns (uint64) {
        bytes32 vaultBase = keccak256(abi.encode(VAULT, VAULTS_SLOT));
        return uint64(uint256(vm.load(address(registry), bytes32(uint256(vaultBase) + 1))) >> 64);
    }
}
