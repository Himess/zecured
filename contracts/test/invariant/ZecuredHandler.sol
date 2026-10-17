// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredIntake} from "../../src/ZecuredIntake.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake, PendingActivation, PendingRefund} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredDisputes, IZecuredDisputesEvents} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {IZecuredRefunds} from "../../src/interfaces/v3/IZecuredRefunds.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {CloseRequest, SpendCommit, DeleverageRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {PreLiquidationTerms} from "../../src/interfaces/ISettlementAdapter.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {MockCapPrice} from "../mocks/MockCapPrice.sol";

/// @notice What the handler drives: the invariant suite's deployment.
/// @param gates MOCK named as both the bond and the slasher: the handler halts, unbonds, opens cases, freezes.
struct HandlerSystem {
    IMorpho morpho;
    ZecuredRegistry registry;
    SettlementAdapter adapter;
    ZecuredIntake intake;
    MockUSDC usdc;
    MockZecOracle oracle;
    MockHeaderRelay relay;
    MockRiscZeroVerifier verifier;
    bytes32 imageId;
    MockSignerBond gates;
    MockCapPrice capPrice;
}

/// @notice Invariant-test handler (v3): deposits through the Intake with deposit proofs (MOCK relay and verifier), their
///         pending activations, reorganisations and refunds; the gates of minting (pause by signatures, halt, bonds,
///         coverage cap, open cases) and the freeze of a vault; vault lifecycles (borrow, repay, releases, price moves,
///         liquidations, closes, deleverages), payouts (commitments, confirmations, the opening fee), disputes of
///         confirmations (upheld by an inclusion proof, or reopened), refund destinations and abandonment, time; and attacks that must always fail. All against the real Morpho Blue on a
///         Base Sepolia fork. Ghost state mirrors what Base should hold; flags record a violation when it happens.
/// @dev A fixed set of actors keeps the set of Morpho storage slots read from the fork small and cacheable.
contract ZecuredHandler is Test {
    using MorphoBalancesLib for IMorpho;

    IMorpho internal immutable morpho;
    ZecuredRegistry internal immutable registry;
    SettlementAdapter internal immutable adapter;
    ZecuredIntake internal immutable intake;
    ZZEC internal immutable zzec;
    MockUSDC internal immutable usdc;
    MockZecOracle internal immutable oracle;
    MockHeaderRelay internal immutable relay;
    MockRiscZeroVerifier internal immutable verifier;
    bytes32 internal immutable imageId;
    MockSignerBond public immutable gates;
    MockCapPrice internal immutable capPrice;
    MarketParams internal params;
    Id internal marketId;
    uint256[5] internal signerKeys;

    uint256 internal constant ACCOUNT_COUNT = 8;
    uint64 internal constant DUST = 10_000;
    /// A payout's first claim bears at most 15,000 zatoshi of fee here (two claims and change).
    uint64 internal constant BEARER_MIN = 15_000;
    address[ACCOUNT_COUNT] public accounts;
    uint256[ACCOUNT_COUNT] internal accountKeys;
    address public immutable liquidator = makeAddr("zecured.inv.liquidator");
    address public immutable marketMaker = makeAddr("zecured.inv.marketMaker");
    address public immutable stranger = makeAddr("zecured.inv.stranger");

    // ------------------------------------------------------------------ ghost state

    /// A deposit the handler proves.
    struct Deposit {
        bytes32 vaultId;
        uint256 accountIndex;
        uint64 amount;
        bytes32 tag;
    }

    uint8 public constant TAG_PENDING = 1;
    uint8 public constant TAG_ACTIVATED = 2;
    uint8 public constant TAG_REFUND = 3;
    uint8 public constant TAG_DROPPED = 4;
    /// R-12: a refund whose proof waits for the relay; its claim is not open yet.
    uint8 public constant TAG_REFUND_PENDING = 5;

    /// Every vault a deposit landed on (reserved, activated or refunded).
    bytes32[] public vaultIds;
    mapping(bytes32 vaultId => bool) public known;
    mapping(bytes32 vaultId => uint256) public accountIndexOf;
    /// The deposit that activated the vault: its zZEC plus the opening fee.
    mapping(bytes32 vaultId => uint64) public activated;
    /// The deposits that landed on the vault as refunds (proofs, and converted pending activations).
    mapping(bytes32 vaultId => uint64) public refunded;
    /// The vault's lineage notes that no committed payout has spent: its deposit (pending or activated) or the change of
    /// its last payout, and one per refunded deposit until a payout spends it.
    mapping(bytes32 vaultId => bytes32[]) internal liveNotes;
    /// The note a refund claim pays back (its account's deposit).
    mapping(bytes32 claimId => bytes32) public refundNoteOf;
    /// Claims reopened after a dispute: a later proof of their payout still upholds them (D57).
    mapping(bytes32 claimId => bool) public reopened;
    /// The vault's latest committed payout (v4: confirmations name it).
    mapping(bytes32 vaultId => bytes32) public lastPayoutRef;
    /// Every tag the Intake accepted, and what it is now.
    bytes32[] public tags;
    mapping(bytes32 tag => uint8) public tagState;
    mapping(bytes32 tag => bytes32) public tagVault;
    bytes32[] public pendingIds;
    /// R-12: the tags of the pending refunds.
    bytes32[] public pendingRefundTags;
    /// R-12: the sum of a vault's pending refunds.
    mapping(bytes32 vaultId => uint64) public pendingRefundOf;
    /// R-11: Refund claims marked dormant and still so.
    mapping(bytes32 claimId => bool) public dormant;
    Deposit[] internal dropped;
    uint256 internal vaultNonce;
    uint256 internal tagNonce;
    uint256 internal anchorNonce;
    uint256 internal nextNonce;

    bool public attackSucceeded;
    bool public i7Violated;
    bool public i9Violated;
    bool public d14Violated;
    /// Any other violation found while acting (I18, I21 to I24, an unexpected revert); `violation` says which.
    bool public violated;
    string public violation;
    mapping(string => uint256) public calls;

    constructor(HandlerSystem memory s, uint256[5] memory signerKeys_) {
        morpho = s.morpho;
        registry = s.registry;
        adapter = s.adapter;
        intake = s.intake;
        zzec = ZZEC(address(s.registry.zzec()));
        usdc = s.usdc;
        oracle = s.oracle;
        relay = s.relay;
        verifier = s.verifier;
        imageId = s.imageId;
        gates = s.gates;
        capPrice = s.capPrice;
        params = s.registry.marketParams();
        marketId = s.registry.marketId();
        signerKeys = signerKeys_;
        for (uint256 i; i < ACCOUNT_COUNT; ++i) {
            (accounts[i], accountKeys[i]) = makeAddrAndKey(string.concat("zecured.inv.account", vm.toString(i)));
        }
    }

    // ------------------------------------------------------------------ views for the invariants

    function vaultCount() external view returns (uint256) {
        return vaultIds.length;
    }

    function tagCount() external view returns (uint256) {
        return tags.length;
    }

    function pendingCount() external view returns (uint256) {
        return pendingIds.length;
    }

    function pendingRefundCount() external view returns (uint256) {
        return pendingRefundTags.length;
    }

    function liveNotesOf(bytes32 vaultId) external view returns (bytes32[] memory) {
        return liveNotes[vaultId];
    }

    function accountOf(bytes32 vaultId) public view returns (address) {
        return accounts[accountIndexOf[vaultId]];
    }

    // ------------------------------------------------------------------ helpers

    /// The fuzzer favours values like 0 and type(uint256).max; decisions that must stay rare use a hash of the seed.
    function _mix(uint256 seed) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(seed)));
    }

    function _flag(string memory what) internal {
        if (!violated) violation = what;
        violated = true;
    }

    function _sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    /// 3 distinct signers in increasing order, chosen by `seed` (indices i < j < k of the sorted set).
    function _threeSigs(bytes32 digest, uint256 seed) internal view returns (bytes[] memory sigs) {
        uint256[3][10] memory subsets = [
            [uint256(0), 1, 2],
            [uint256(0), 1, 3],
            [uint256(0), 1, 4],
            [uint256(0), 2, 3],
            [uint256(0), 2, 4],
            [uint256(0), 3, 4],
            [uint256(1), 2, 3],
            [uint256(1), 2, 4],
            [uint256(1), 3, 4],
            [uint256(2), 3, 4]
        ];
        uint256[3] memory pick = subsets[seed % 10];
        sigs = new bytes[](3);
        for (uint256 i; i < 3; ++i) {
            sigs[i] = _sign(signerKeys[pick[i]], digest);
        }
    }

    function _one(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    /// A known vault picked by `seed`, or nothing.
    function _vault(uint256 seed) internal view returns (bool found, bytes32 vaultId, address account) {
        if (vaultIds.length == 0) return (false, bytes32(0), address(0));
        vaultId = vaultIds[seed % vaultIds.length];
        return (true, vaultId, accountOf(vaultId));
    }

    function _active(bytes32 vaultId) internal view returns (bool active, uint64 outstanding) {
        VaultStatus status;
        (, outstanding, status) = registry.vault(vaultId);
        active = status == VaultStatus.Active;
    }

    function _status(bytes32 vaultId) internal view returns (VaultStatus status) {
        (,, status) = registry.vault(vaultId);
    }

    function _authorize(address account) internal {
        vm.startPrank(account);
        if (!morpho.isAuthorized(account, address(registry))) morpho.setAuthorization(address(registry), true);
        if (!morpho.isAuthorized(account, address(adapter))) morpho.setAuthorization(address(adapter), true);
        vm.stopPrank();
    }

    function _fund(address who, address spender, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(spender, amount);
    }

    function _removePending(uint256 i) internal {
        pendingIds[i] = pendingIds[pendingIds.length - 1];
        pendingIds.pop();
    }

    function _removePendingRefund(uint256 i) internal {
        pendingRefundTags[i] = pendingRefundTags[pendingRefundTags.length - 1];
        pendingRefundTags.pop();
    }

    function _removeNote(bytes32 vaultId, bytes32 tag) internal {
        bytes32[] storage n = liveNotes[vaultId];
        for (uint256 i; i < n.length; ++i) {
            if (n[i] == tag) {
                n[i] = n[n.length - 1];
                n.pop();
                return;
            }
        }
    }

    function _markTag(bytes32 tag, bytes32 vaultId, uint8 state) internal {
        if (tagState[tag] == 0) tags.push(tag);
        tagState[tag] = state;
        tagVault[tag] = vaultId;
    }

    // ------------------------------------------------------------------ deposits (T-01, T-02, K3)

    /// A deposit proof through the Intake: a new vault's deposit, a note proven again after a reorganisation dropped its
    /// reservation, or (`second`) another note to a vault Base already knows, which lands as a refund. `keepPrice`
    /// leaves the cap price as it is (it may have gone stale): an activatable proof then waits (`CapPriceStale`), a
    /// refund never reads the price.
    function prove(uint256 seed, bool second, bool keepPrice) external {
        // Mostly new vaults with a fresh cap price: second deposits and stale prices are the exception. Decisions use
        // a hash of the seed: the fuzzer favours values like 0, which would make every exception frequent.
        seed = _mix(seed);
        _prove(seed, second && seed % 3 == 0, keepPrice && seed % 4 == 0);
    }

    /// R-12: another note to a vault Base knows, which lands as a pending refund (the refund lifecycle's traffic).
    function proveSecond(uint256 seed) external {
        if (vaultIds.length == 0) return;
        _prove(_mix(seed), true, false);
    }

    function _prove(uint256 seed, bool second, bool keepPrice) internal {
        (Deposit memory d, bool fromDropped) = _nextDeposit(seed, second);
        if (!keepPrice) capPrice.set(oracle.price());
        bool stale = block.timestamp > capPrice.updatedAt() + intake.capPriceMaxAge();
        bool capHeld = _capHolds(d.amount);
        string memory expected = _expectedLanding(d, capHeld);
        uint256 supply = zzec.totalSupply();
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proof(d);
        bool capWait = keccak256(bytes(expected)) == keccak256("prove: cap full (waits)");
        try intake.attestDepositWithProof(journal, seal, imageId, sigs) {
            if (zzec.totalSupply() != supply) _flag("a deposit proof minted");
            if (capWait) _flag("R-11: a new vault's proof landed past the coverage cap");
            _recordLanding(d, capHeld, expected);
        } catch (bytes memory reason) {
            bytes4 selector = bytes4(reason);
            if (stale && selector == IZecuredIntake.CapPriceStale.selector) {
                calls["prove (stale cap price)"]++;
            } else if (selector == IZecuredRegistry.VaultCompromised.selector && gates.isCompromised(d.vaultId)) {
                calls["prove (compromised vault)"]++;
            } else if (capWait && selector == IZecuredIntake.CapExceeded.selector) {
                calls[expected]++; // R-11: the vault stays new, the proof is sent again later
            } else {
                _flag("a deposit proof reverted");
            }
            if (fromDropped) dropped.push(d);
        }
    }

    function _nextDeposit(uint256 seed, bool second) internal returns (Deposit memory d, bool fromDropped) {
        if (!second && dropped.length != 0 && seed % 3 == 0) {
            d = dropped[dropped.length - 1];
            dropped.pop();
            return (d, true);
        }
        if (second && vaultIds.length != 0) {
            d.vaultId = vaultIds[(seed >> 8) % vaultIds.length];
            d.accountIndex = accountIndexOf[d.vaultId];
        } else {
            d.vaultId = keccak256(abi.encode("zecured.inv.vault", vaultNonce++));
            uint256 start = (seed >> 8) % ACCOUNT_COUNT;
            d.accountIndex = start;
            // Mostly an account without a vault: a new vault of a bound account lands as a refund (unreachable with an
            // honest VaultOpened), kept as the exception.
            if (seed % 5 != 0) {
                for (uint256 k; k < ACCOUNT_COUNT; ++k) {
                    uint256 i = (start + k) % ACCOUNT_COUNT;
                    if (registry.vaultOf(accounts[i]) == bytes32(0)) {
                        d.accountIndex = i;
                        break;
                    }
                }
            }
        }
        d.amount = seed % 2 == 0 ? 1_000_000 : 5_000_000; // D7 denominations
        d.tag = keccak256(abi.encode("zecured.inv.tag", tagNonce++));
    }

    /// The proof of `d`: a fresh anchor with the work its amount needs, the journal, a MOCK seal, and the five signers'
    /// signatures over the vault's (stand-in) `VaultOpened`.
    function _proof(Deposit memory d) internal returns (bytes memory journal, bytes memory seal, bytes[] memory sigs) {
        bytes32 anchor = keccak256(abi.encode("zecured.inv.anchor", anchorNonce++));
        relay.setBlock(anchor, true, intake.minConfirmationWorkFor(d.amount));
        bytes32 opened = keccak256(abi.encode("zecured.inv.opened", d.vaultId));
        journal = abi.encode(anchor, d.vaultId, accounts[d.accountIndex], d.amount, d.tag, opened);
        seal = verifier.mockSeal(imageId, sha256(journal));
        bytes32 digest = MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened);
        sigs = new bytes[](5);
        for (uint256 i; i < 5; ++i) {
            sigs[i] = _sign(signerKeys[i], digest);
        }
    }

    /// T-01, computed independently: locked and pending zatoshi with `amount` within the absolute cap and, valued at
    /// the cap price (rounded up), within `coverageBps` of the effective bond.
    function _capHolds(uint64 amount) internal view returns (bool) {
        uint256 locked = registry.lockedZatoshi() + intake.pendingTotal() + intake.pendingRefundTotal() + amount;
        if (locked > intake.maxTotalZat()) return false;
        uint256 value = Math.mulDiv(locked, capPrice.price(), ORACLE_PRICE_SCALE, Math.Rounding.Ceil);
        return value * 10_000 <= gates.effectiveBond() * intake.coverageBps();
    }

    /// T-02, computed independently: where a proven deposit lands (the order of the Intake's reasons does not matter
    /// here, only whether it is a refund).
    function _expectedLanding(Deposit memory d, bool capHeld) internal view returns (string memory) {
        if (_status(d.vaultId) != VaultStatus.None) return "prove: refund (known vault)";
        if (registry.vaultOf(accounts[d.accountIndex]) != bytes32(0)) return "prove: refund (account bound)";
        if (gates.halted()) return "prove: refund (halted)";
        if (!capHeld) return "prove: cap full (waits)";
        return "prove: pending";
    }

    function _recordLanding(Deposit memory d, bool capHeld, string memory expected) internal {
        bool expectPending = keccak256(bytes(expected)) == keccak256("prove: pending");
        if (!known[d.vaultId]) {
            known[d.vaultId] = true;
            vaultIds.push(d.vaultId);
            accountIndexOf[d.vaultId] = d.accountIndex;
        }
        liveNotes[d.vaultId].push(d.tag);
        calls[expected]++;
        if (intake.pendingActivation(d.vaultId).depositTag == d.tag) {
            // I18: a reservation only within the cap; I23: never while halted.
            if (!capHeld) _flag("I18: a reservation past the coverage cap");
            if (gates.halted()) _flag("I23: a reservation while halted");
            if (!expectPending) _flag("T-02: a deposit that should have been refunded was reserved");
            _markTag(d.tag, d.vaultId, TAG_PENDING);
            pendingIds.push(d.vaultId);
        } else {
            if (expectPending) _flag("T-02: a deposit that should have been reserved was refunded");
            // R-12: a refund waits for the relay like an activation; no claim opens in the proof's transaction.
            if (intake.pendingRefund(d.tag).vaultId != d.vaultId) _flag("R-12: a refund landed without pending");
            _markTag(d.tag, d.vaultId, TAG_REFUND_PENDING);
            pendingRefundTags.push(d.tag);
            pendingRefundOf[d.vaultId] += d.amount;
        }
    }

    /// K3: the relay moves on, the delay passes, and anyone finalizes; refused while a gate is closed (I23).
    function finalize(uint256 seed) external {
        if (pendingIds.length == 0) return;
        uint256 i = seed % pendingIds.length;
        bytes32 vaultId = pendingIds[i];
        PendingActivation memory p = intake.pendingActivation(vaultId);
        if (relay.tipHeight() < p.readyHeight) relay.setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
        // Half the time the finalizer waits a pause out.
        if (seed % 2 == 0 && block.timestamp < intake.pausedUntil()) vm.warp(intake.pausedUntil());
        bool closed = gates.halted() || !gates.fullyBonded() || intake.mintingPaused()
            || gates.openCasesOf(vaultId) != 0 || !relay.isInBestChain(p.anchor);
        uint256 supply = zzec.totalSupply();
        try intake.finalizeActivation(vaultId) {
            if (closed) _flag("I23: minted through a closed gate");
            _removePending(i);
            activated[vaultId] = p.amount;
            _markTag(p.depositTag, vaultId, TAG_ACTIVATED);
            (, uint64 outstanding,) = registry.vault(vaultId);
            if (zzec.totalSupply() != supply + outstanding) _flag("I1: the mint differs from the outstanding");
            calls["finalize"]++;
        } catch {
            if (!closed) _flag("a finalization reverted with every gate open");
            calls["finalize (gated)"]++;
        }
    }

    /// K3: a reorganisation takes the anchor off the best chain; anyone drops the reservation, and the note may be proven
    /// again later.
    function drop(uint256 seed) external {
        if (pendingIds.length == 0) return;
        uint256 i = seed % pendingIds.length;
        bytes32 vaultId = pendingIds[i];
        PendingActivation memory p = intake.pendingActivation(vaultId);
        relay.setBlock(p.anchor, false, 0);
        bool caseOpen = gates.openCasesOf(vaultId) != 0;
        try intake.dropPendingActivation(vaultId) {
            if (caseOpen) _flag("R-17: a pending activation dropped while a case was open");
            _removePending(i);
            _removeNote(vaultId, p.depositTag);
            _markTag(p.depositTag, vaultId, TAG_DROPPED);
            dropped.push(
                Deposit({vaultId: vaultId, accountIndex: accountIndexOf[vaultId], amount: p.amount, tag: p.depositTag})
            );
            calls["drop"]++;
        } catch {
            if (!caseOpen) _flag("a reservation off the best chain was not dropped");
            calls["drop (case open)"]++;
        }
    }

    /// T-02, T-03: a pending activation becomes a refund: anyone after a halt, its account once a pause has lasted
    /// `pendingRefundDelay`; nobody otherwise.
    function refundPending(uint256 seed, bool byAccount) external {
        if (pendingIds.length == 0) return;
        uint256 i = seed % pendingIds.length;
        bytes32 vaultId = pendingIds[i];
        PendingActivation memory p = intake.pendingActivation(vaultId);
        // Mostly ready (K3): the relay moved on and the delay passed; half of those wait out the account's delay too.
        if (seed % 4 != 0) {
            if (relay.tipHeight() < p.readyHeight) relay.setTipHeight(p.readyHeight);
            if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
            if (seed % 2 == 0 && block.timestamp < p.readyTime + intake.pendingRefundDelay()) {
                vm.warp(p.readyTime + intake.pendingRefundDelay());
            }
        }
        bool ready =
            relay.tipHeight() >= p.readyHeight && block.timestamp >= p.readyTime && relay.isInBestChain(p.anchor);
        bool allowed =
            ready && (gates.halted() || (byAccount && block.timestamp >= p.readyTime + intake.pendingRefundDelay()));
        vm.prank(byAccount ? p.account : stranger);
        try intake.refundPending(vaultId) returns (bytes32 claimId) {
            if (!allowed) _flag("a pending activation refunded without a halt or a long pause");
            _removePending(i);
            _markTag(p.depositTag, vaultId, TAG_REFUND);
            refunded[vaultId] += p.amount;
            refundNoteOf[claimId] = p.depositTag;
            calls["refundPending"]++;
        } catch {
            if (allowed) _flag("a pending activation could not be refunded");
            calls["refundPending (refused)"]++;
        }
    }

    // ------------------------------------------------------------------ gates (T-01, T-03, T-04, E2)

    /// T-03: three signers pause new minting, for up to `maxPause` (renewable).
    function pause(uint256 seed, uint256 durationSeed) external {
        // Up to three days here (the contract allows `maxPause`), so that the campaign also mints.
        uint64 until = uint64(block.timestamp + bound(durationSeed, 1 hours, 3 days));
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes[] memory sigs = _threeSigs(intake.hashPauseMinting(until, nonce, deadline), seed);
        intake.pauseMinting(until, nonce, deadline, sigs);
        calls["pause"]++;
    }

    function unpause(uint256 seed) external {
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        intake.unpauseMinting(nonce, deadline, _threeSigs(intake.hashUnpauseMinting(nonce, deadline), seed));
        calls["unpause"]++;
    }

    /// The bonds and the slasher (MOCK): a slash halts for good (rarely), a bond leaves or returns, the effective bond
    /// and the cap price move, a case opens or closes on a vault (E2), a vault is frozen or compromised (T-04).
    function setGates(uint256 seed) external {
        seed = _mix(seed);
        uint256 op = seed % 8;
        bytes32 v = vaultIds.length == 0 ? bytes32(0) : vaultIds[(seed >> 8) % vaultIds.length];
        if (op == 0) {
            if (seed % 512 == 0) gates.setHalted(true); // rare: a halt is for good
        } else if (op == 1) {
            gates.setFullyBonded((seed >> 8) % 4 != 0); // a bond missing a quarter of the time
        } else if (op == 2) {
            gates.setEffectiveBond(bound(seed >> 16, 5e6, 60e6)); // 5 to 60 USDC: 0.125 to 1.5 ZEC at $40
        } else if (op == 3 && v != bytes32(0)) {
            gates.setOpenCases(v, gates.openCasesOf(v) == 0 ? 1 : 0);
        } else if (op == 4 && v != bytes32(0) && !gates.isCompromised(v)) {
            gates.setFrozen(v, !gates.vaultFrozen(v));
        } else if (op == 5 && v != bytes32(0) && seed % 256 == 5) {
            // An executed F1/F2 case: the vault compromised and frozen, minting halted, for good (no compensator in
            // this suite).
            gates.setCompromised(v, true);
            gates.setFrozen(v, true);
            gates.setHalted(true);
        } else {
            capPrice.set(bound(seed >> 16, 2_000, 8_000) * 1e32); // $20 .. $80
        }
        calls["gates"]++;
    }

    // ------------------------------------------------------------------ refunds (T-02)

    /// A Refund claim without a destination (pending or abandoned) and not bound to a payout: the first of the vaults
    /// from the one `seed` picks on.
    function _refundWithoutDestination(uint256 seed) internal view returns (bool, bytes32 vaultId, bytes32 claimId) {
        uint256 n = vaultIds.length;
        for (uint256 k; k < n; ++k) {
            vaultId = vaultIds[(seed % n + k) % n];
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                claimId = registry.claimIdOf(vaultId, j);
                ClaimDetails memory c = registry.claimDetails(claimId);
                if (
                    c.kind == ClaimKind.Refund && c.commitment == bytes32(0)
                        && registry.claimInput(claimId) == bytes32(0)
                        && (c.status == ClaimStatus.Pending || c.status == ClaimStatus.WrittenOff)
                ) return (true, vaultId, claimId);
            }
        }
        return (false, bytes32(0), bytes32(0));
    }

    /// The vault's account names where its refund goes (reviving it if it was abandoned).
    function giveRefundDestination(uint256 seed) external {
        (bool found, bytes32 vaultId, bytes32 claimId) = _refundWithoutDestination(seed);
        if (!found) return;
        // A compromised vault's abandoned refund stays written off: its claims are the compensator's.
        (,,,, ClaimStatus status) = registry.claim(claimId);
        if (status == ClaimStatus.WrittenOff && gates.isCompromised(vaultId)) return;
        vm.prank(accountOf(vaultId));
        registry.setRefundDestination(claimId, keccak256(abi.encode("zecured.inv.refundDest", claimId)));
        dormant[claimId] = false;
        calls["refund destination"]++;
    }

    /// T-02: a refund nobody claims is written off after `refundAbandonPeriod`, by anyone.
    function abandon(uint256 seed) external {
        (bool found, bytes32 vaultId, bytes32 claimId) = _refundWithoutDestination(seed);
        if (!found || gates.isCompromised(vaultId)) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (c.status != ClaimStatus.Pending) return;
        uint256 at = uint256(c.createdAt) + registry.refundAbandonPeriod();
        if (block.timestamp < at) vm.warp(at);
        vm.prank(stranger);
        registry.abandonRefund(claimId);
        dormant[claimId] = false;
        calls["abandon"]++;
    }

    /// R-11: a refund nobody gave a destination leaves `lockedZatoshi` after `refundDormancy`, by anyone.
    function markDormant(uint256 seed) external {
        (bool found,, bytes32 claimId) = _refundWithoutDestination(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        if (c.status != ClaimStatus.Pending || c.bondPaid || refunds.isRefundDormant(claimId)) return;
        uint256 at = uint256(c.createdAt) + refunds.refundDormancy();
        if (block.timestamp < at) vm.warp(at);
        uint256 locked = registry.lockedZatoshi();
        vm.prank(stranger);
        try refunds.markRefundDormant(claimId) {
            dormant[claimId] = true;
            if (registry.lockedZatoshi() != locked - c.amount) _flag("R-11: a dormant refund still counts");
            calls["mark dormant"]++;
        } catch {
            _flag("R-11: a refund without a destination could not be marked dormant");
        }
    }

    // ------------------------------------------------------------------ pending refunds (R-8, R-12)

    /// R-12: the relay moves on, the delay passes, and anyone opens the refund's claim; it waits while the vault's
    /// activation pends (R-8). On a compromised vault it opens all the same (it was reserved before the compromise),
    /// for the compensator to settle.
    function finalizeRefund(uint256 seed) external {
        if (pendingRefundTags.length == 0) return;
        uint256 i = seed % pendingRefundTags.length;
        bytes32 tag = pendingRefundTags[i];
        PendingRefund memory p = intake.pendingRefund(tag);
        if (relay.tipHeight() < p.readyHeight) relay.setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
        bool vaultPending = _status(p.vaultId) == VaultStatus.Pending;
        bool compromised = gates.isCompromised(p.vaultId);
        bool anchorGone = !relay.isInBestChain(p.anchor); // a drop R-17 refused
        try intake.finalizeRefund(tag) returns (bytes32 claimId) {
            if (anchorGone) _flag("R-12: a refund opened off the best chain");
            if (vaultPending) _flag("R-8: a refund opened on a pending vault");
            _removePendingRefund(i);
            pendingRefundOf[p.vaultId] -= p.amount;
            _markTag(tag, p.vaultId, TAG_REFUND);
            refunded[p.vaultId] += p.amount;
            refundNoteOf[claimId] = tag;
            calls[compromised ? "finalize refund (compromised)" : "finalize refund"]++;
        } catch (bytes memory reason) {
            bytes4 selector = bytes4(reason);
            if (anchorGone && selector == IZecuredIntake.AnchorNotConfirmed.selector) {
                calls["finalize refund (anchor gone)"]++;
            } else if (vaultPending && selector == IZecuredSpendCommitsEvents.VaultPending.selector) {
                calls["finalize refund (vault pending)"]++;
            } else {
                _flag("a ready refund could not be opened");
            }
        }
    }

    /// R-12: a reorganisation takes a pending refund's anchor off the best chain; anyone drops it (not while a case is
    /// open on the vault, R-17), and the note may be proven again.
    function dropRefund(uint256 seed) external {
        if (pendingRefundTags.length == 0) return;
        uint256 i = seed % pendingRefundTags.length;
        bytes32 tag = pendingRefundTags[i];
        PendingRefund memory p = intake.pendingRefund(tag);
        relay.setBlock(p.anchor, false, 0);
        bool caseOpen = gates.openCasesOf(p.vaultId) != 0;
        try intake.dropPendingRefund(tag) {
            if (caseOpen) _flag("R-17: a pending refund dropped while a case was open");
            _removePendingRefund(i);
            pendingRefundOf[p.vaultId] -= p.amount;
            _removeNote(p.vaultId, tag);
            _markTag(tag, p.vaultId, TAG_DROPPED);
            dropped.push(
                Deposit({vaultId: p.vaultId, accountIndex: accountIndexOf[p.vaultId], amount: p.amount, tag: tag})
            );
            calls["drop refund"]++;
        } catch {
            if (!caseOpen) _flag("a pending refund off the best chain was not dropped");
            calls["drop refund (case open)"]++;
        }
    }

    /// R-12: no payout spends a pending refund's note, even with valid signatures. Must always fail.
    function attackSpendPendingRefund(uint256 seed) external {
        if (pendingRefundTags.length == 0) return;
        bytes32 tag = pendingRefundTags[seed % pendingRefundTags.length];
        bytes32 vaultId = tagVault[tag];
        (bool found, bytes32 claimId) = _payableClaim(vaultId, false);
        if (!found) return;
        (bool possible, SpendCommit memory c) = _reuseCommit(vaultId, tag, claimId);
        if (!possible) return;
        try registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c), seed)) {
            attackSucceeded = true;
        } catch {}
        calls["attack: spend pending refund"]++;
    }

    /// RT-8: a closed vault's claim too small for any payout's fee is written off, by anyone.
    function writeOffDust(uint256 seed) external {
        (bool found, bytes32 vaultId,) = _vault(seed);
        if (!found || _status(vaultId) != VaultStatus.Closed || gates.isCompromised(vaultId)) return;
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (,, uint64 amount,, ClaimStatus status) = registry.claim(claimId);
            if (status != ClaimStatus.Pending || amount > DUST || registry.claimInput(claimId) != bytes32(0)) continue;
            registry.writeOffDust(claimId);
            calls["write off dust"]++;
            return;
        }
    }

    // ------------------------------------------------------------------ vault lifecycles

    function borrow(uint256 vaultSeed, uint256 ltvBps) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstanding) = _active(vaultId);
        if (!active) return;
        _authorize(account);
        ltvBps = bound(ltvBps, 500, 5_500);
        uint256 target = uint256(outstanding) * oracle.price() / ORACLE_PRICE_SCALE * ltvBps / 10_000;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        if (target <= debt + 1) return;
        vm.prank(account);
        try morpho.borrow(params, target - debt, 0, account, stranger) {
            calls["borrow"]++;
        } catch {}
    }

    function repay(uint256 vaultSeed) external {
        (bool found,, address account) = _vault(vaultSeed);
        if (!found) return;
        uint256 shares = morpho.position(marketId, account).borrowShares;
        if (shares == 0) return;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        _fund(account, address(morpho), debt);
        vm.prank(account);
        morpho.repay(params, 0, shares, account, "");
        calls["repay"]++;
    }

    /// I22: no gate (pause, cap, halt, bonds) stops a release; only the vault's own state (here, the freeze) does.
    function release(uint256 vaultSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active,) = _active(vaultId);
        if (!active || morpho.position(marketId, account).borrowShares != 0) return;
        _authorize(account);
        bool frozen = gates.vaultFrozen(vaultId);
        vm.prank(account);
        try registry.requestRelease(vaultId, keccak256(abi.encode("zecured.inv.release", vaultId))) {
            if (frozen) _flag("I24: a frozen vault was released");
            calls["release"]++;
        } catch {
            if (!frozen) _flag("I22: a release reverted");
            calls["release (frozen)"]++;
        }
    }

    /// D14: the account withdraws part of its collateral (debt or not); refused when the LTV would exceed 50 %.
    function partialRelease(uint256 vaultSeed, uint256 amountSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        uint64 minRelease = registry.MIN_RELEASE();
        if (!active || outstandingBefore < 2 * minRelease) return;
        _authorize(account);
        uint64 amount = uint64(bound(amountSeed, minRelease, outstandingBefore - minRelease));
        uint64 claimsBefore = registry.claimCount(vaultId);
        bool frozen = gates.vaultFrozen(vaultId);

        vm.prank(account);
        try registry.requestPartialRelease(
            vaultId, amount, keccak256(abi.encode("zecured.inv.partial", vaultId, claimsBefore))
        ) {
            if (frozen) _flag("I24: a frozen vault was partially released");
            calls["partialRelease"]++;
            _checkPartialRelease(vaultId, account, claimsBefore, outstandingBefore, amount);
        } catch {
            calls["partialRelease (reverted)"]++;
        }
    }

    /// D14 at the moment of a successful partial release, with the LTV recomputed independently (Morpho's expected
    /// debt, rounded up; collateral value rounded down).
    function _checkPartialRelease(
        bytes32 vaultId,
        address account,
        uint64 claimsBefore,
        uint64 outstandingBefore,
        uint64 amount
    ) internal {
        (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
        (,, uint64 claimed,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        uint256 value = uint256(outstandingAfter) * oracle.price() / ORACLE_PRICE_SCALE;
        if (
            !stillActive || outstandingAfter != outstandingBefore - amount || claimed != amount
                || outstandingAfter < registry.MIN_RELEASE() || debt * 10_000 > value * registry.MAX_WITHDRAW_LTV_BPS()
        ) d14Violated = true;
    }

    function setPrice(uint256 cents) external {
        oracle.setPrice(bound(cents, 1_500, 6_000) * 1e32); // $15 .. $60
        calls["setPrice"]++;
    }

    function warp(uint256 secondsToSkip) external {
        skip(bound(secondsToSkip, 1, 30 days));
        calls["warp"]++;
    }

    /// Opens a loan of `ltvBps` (10 %..55 %) of the vault's value at the current price if it has none. Never reverts.
    function _ensureDebt(address account, uint64 outstanding, uint256 ltvBps) internal returns (bool) {
        if (morpho.position(marketId, account).borrowShares != 0) return true;
        _authorize(account);
        uint256 assets =
            uint256(outstanding) * oracle.price() / ORACLE_PRICE_SCALE * bound(ltvBps, 1_000, 5_500) / 10_000;
        if (assets == 0) return false;
        vm.prank(account);
        try morpho.borrow(params, assets, 0, account, stranger) {
            calls["borrow"]++;
            return true;
        } catch {
            return false;
        }
    }

    /// Goal-directed: makes sure the vault has a loan, moves the price so that its LTV is 63 %..95 %, then
    /// liquidates through the Adapter (either Morpho path; the amount may be below the D11 floor on purpose).
    function liquidate(uint256 vaultSeed, bool bySeize, uint256 amount, uint256 ltvSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, 5_000)) return;
        if (!_makeUnhealthy(account, outstandingBefore, ltvSeed)) return;
        (uint256 seizedAssets, uint256 repaidShares) = _liquidationAmounts(account, outstandingBefore, bySeize, amount);
        uint64 claimsBefore = registry.claimCount(vaultId);
        bool frozen = gates.vaultFrozen(vaultId);

        vm.prank(liquidator);
        try adapter.liquidate(account, seizedAssets, repaidShares, keccak256("zecured.inv.liquidator")) returns (
            uint256 seized, uint256
        ) {
            if (frozen) _flag("I24: a frozen vault was liquidated");
            calls["liquidate"]++;
            _checkLiquidation(account, vaultId, claimsBefore, outstandingBefore, seized);
        } catch {
            calls["liquidate (reverted)"]++;
        }
    }

    /// Sets the price so that the account's LTV is 63 %..95 % and funds the liquidator.
    function _makeUnhealthy(address account, uint64 outstanding, uint256 ltvSeed) internal returns (bool) {
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        uint256 price = debt * 10_000 * ORACLE_PRICE_SCALE / (bound(ltvSeed, 6_300, 9_500) * outstanding);
        if (price == 0) return false;
        oracle.setPrice(price);
        _fund(liquidator, address(adapter), debt + 1e6);
        return true;
    }

    function _liquidationAmounts(address account, uint64 outstanding, bool bySeize, uint256 amount)
        internal
        view
        returns (uint256 seizedAssets, uint256 repaidShares)
    {
        // Mostly around the 100,000 zat floor; sub-floor remainders (allowed by D11) can be smaller than that.
        if (bySeize) return (bound(amount, outstanding < 50_000 ? 1 : 50_000, outstanding), 0);
        uint256 shares = morpho.position(marketId, account).borrowShares;
        return (0, bound(amount, 1, shares + shares / 4)); // above the position: capped by the Adapter
    }

    /// I9 (and D11) at the moment of a successful liquidation; the liquidator is the claim's claimant (T-04, T-05).
    function _checkLiquidation(
        address account,
        bytes32 vaultId,
        uint64 claimsBefore,
        uint64 outstandingBefore,
        uint256 seized
    ) internal {
        (, uint64 outstandingAfter,) = registry.vault(vaultId);
        bytes32 claimId = registry.claimIdOf(vaultId, claimsBefore);
        ClaimDetails memory c = registry.claimDetails(claimId);
        // D11: at least MIN_SEIZE, or the whole remaining outstanding, or a seizure that clears the whole debt (L-1).
        bool debtCleared = morpho.position(marketId, account).borrowShares == 0;
        bool belowFloor = seized < adapter.minSeize(outstandingBefore) && seized != outstandingBefore && !debtCleared;
        if (
            seized > outstandingBefore || c.amount != seized || outstandingAfter != outstandingBefore - seized
                || belowFloor || c.kind != ClaimKind.Liquidation || c.claimant != liquidator
        ) i9Violated = true;
    }

    /// Goal-directed: makes sure the vault has a loan, then the market maker closes it with a signed request.
    function close(uint256 vaultSeed, uint256 marginBps, uint256 ltvSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, ltvSeed)) return;
        _authorize(account);
        CloseRequest memory req = CloseRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: uint16(bound(marginBps, 0, 500)),
            minUserAmount: 0,
            userCommitment: keccak256(abi.encode("zecured.inv.user", vaultId)),
            nonce: nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashCloseRequest(req));
        _fund(marketMaker, address(adapter), morpho.expectedBorrowAssets(params, account));
        uint64 claimsBefore = registry.claimCount(vaultId);
        uint256 supplyBefore = zzec.totalSupply();
        bool frozen = gates.vaultFrozen(vaultId);

        vm.prank(marketMaker);
        try adapter.closeWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), type(uint256).max, 0) {
            if (frozen) _flag("I24: a frozen vault was closed");
            calls["close"]++;
            _checkClose(vaultId, claimsBefore, outstandingBefore, supplyBefore);
        } catch {
            calls["close (reverted)"]++;
        }
    }

    /// I7 at the moment of a close; the executor is the market maker claim's claimant.
    function _checkClose(bytes32 vaultId, uint64 claimsBefore, uint64 outstandingBefore, uint256 supplyBefore)
        internal
    {
        (,, uint64 userAmount,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
        ClaimDetails memory mm = registry.claimDetails(registry.claimIdOf(vaultId, claimsBefore + 1));
        (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
        if (
            uint256(userAmount) + mm.amount != outstandingBefore
                || supplyBefore - zzec.totalSupply() != outstandingBefore || stillActive || outstandingAfter != 0
                || registry.claimCount(vaultId) != claimsBefore + 2 || mm.claimant != marketMaker
        ) i7Violated = true;
    }

    /// S18: the market maker repays part of the debt and takes the value in collateral; the vault stays open.
    function deleverage(uint256 vaultSeed, uint256 repaySeed, uint256 marginBps, uint256 ltvSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, ltvSeed)) return;
        _authorize(account);
        DeleverageRequest memory req = _deleverageRequest(vaultId, account, repaySeed, marginBps, outstandingBefore);
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashDeleverageRequest(req));
        _fund(marketMaker, address(adapter), req.repayAssets);
        uint64 claimsBefore = registry.claimCount(vaultId);
        uint256 supplyBefore = zzec.totalSupply();
        bool frozen = gates.vaultFrozen(vaultId);

        vm.prank(marketMaker);
        try adapter.deleverageWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), 0) returns (uint64 mm) {
            if (frozen) _flag("I24: a frozen vault was deleveraged");
            calls["deleverage"]++;
            _checkDeleverage(vaultId, outstandingBefore, claimsBefore, supplyBefore, mm);
        } catch {
            calls["deleverage (reverted)"]++;
        }
    }

    function _deleverageRequest(
        bytes32 vaultId,
        address account,
        uint256 repaySeed,
        uint256 marginBps,
        uint64 outstanding
    ) internal returns (DeleverageRequest memory) {
        return DeleverageRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: uint16(bound(marginBps, 0, 500)),
            repayAssets: bound(repaySeed, 1, morpho.expectedBorrowAssets(params, account)),
            maxCollateral: outstanding,
            nonce: nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
    }

    /// I9 and conservation for a deleverage: one claim of exactly what left the vault, which stays open.
    function _checkDeleverage(
        bytes32 vaultId,
        uint64 outstandingBefore,
        uint64 claimsBefore,
        uint256 supplyBefore,
        uint64 mm
    ) internal {
        (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
        (,, uint64 claimed,,) = registry.claim(registry.claimIdOf(vaultId, claimsBefore));
        if (
            !stillActive || outstandingAfter + mm != outstandingBefore || claimed != mm
                || supplyBefore - zzec.totalSupply() != mm || registry.claimCount(vaultId) != claimsBefore + 1
        ) i9Violated = true;
    }

    // ------------------------------------------------------------------ payouts (Tier 3, T-06)

    /// A vault whose claims the signers can pay now: not pending (K3) and not compromised (T-04).
    function _payable(bytes32 vaultId) internal view returns (bool) {
        VaultStatus status = _status(vaultId);
        return status != VaultStatus.None && status != VaultStatus.Pending && !gates.isCompromised(vaultId);
    }

    /// The vault's first claim the signers can pay: pending, with a destination, more than dust, `committed` or not.
    function _payableClaim(bytes32 vaultId, bool committed) internal view returns (bool, bytes32) {
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status) = registry.claim(claimId);
            if (status != ClaimStatus.Pending || commitment == bytes32(0) || amount <= BEARER_MIN) continue;
            bool isCommitted = registry.claimInput(claimId) != bytes32(0);
            // An uncommitted Fee claim is paid along with another claim, never first (T-06).
            if (isCommitted == committed && (committed || kind != ClaimKind.Fee)) return (true, claimId);
        }
        return (false, bytes32(0));
    }

    /// The vault's uncommitted Fee claim (T-06), if any.
    function _feeClaim(bytes32 vaultId) internal view returns (bool, bytes32) {
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (, ClaimKind kind,,, ClaimStatus status) = registry.claim(claimId);
            if (kind == ClaimKind.Fee && status == ClaimStatus.Pending && registry.claimInput(claimId) == bytes32(0)) {
                return (true, claimId);
            }
        }
        return (false, bytes32(0));
    }

    /// The signers pay one of the vault's claims (with its Fee claim at index 1 when `withFee` and one is due): commit,
    /// then confirm the payout, naming it, with every claim of it still pending (v4, R-1: each payout is confirmed once;
    /// a reopened claim only by a proof). I22: neither step reverts for any gate; R-3: the confirmation, and only it, is
    /// refused while the vault is frozen (its claims are then proven instead).
    function confirm(uint256 vaultSeed, uint256 signerSeed, bool withFee) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found || !_payable(vaultId)) return;
        bytes32[] memory ids;
        (bool committed, bytes32 claimId) = _payableClaim(vaultId, true);
        if (committed) {
            // Committed (and mined) earlier: only confirmed now, unless its confirmation was recorded already.
            if (_payouts().payoutConfirmed(_payoutRefOf(claimId))) return;
            ids = _pendingClaimsOfInput(vaultId, registry.claimInput(claimId));
        } else {
            (bool ok, bytes32 next) = _payableClaim(vaultId, false);
            if (!ok) return;
            ids = _withFee(vaultId, next, withFee);
            if (!_commit(vaultId, ids, signerSeed)) return;
        }
        bytes32 payoutRef = _payoutRefOf(ids[0]);
        bool frozen = gates.vaultFrozen(vaultId);
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, payoutRef, ids), signerSeed);
        try _payouts().confirmPayout(vaultId, payoutRef, ids, sigs) {
            if (frozen) _flag("R-3: a frozen vault's payout was confirmed by signatures");
            calls["confirm"]++;
        } catch {
            if (!frozen) _flag("I22: a committed payout could not be confirmed");
            calls["confirm (frozen)"]++;
        }
    }

    function _payouts() internal view returns (IZecuredPayouts) {
        return IZecuredPayouts(address(registry));
    }

    /// The handler's payout of a claim's input (`_commit` derives its reference from the input).
    function _payoutRefOf(bytes32 claimId) internal view returns (bytes32) {
        return keccak256(abi.encode("zecured.inv.payoutRef", registry.claimInput(claimId)));
    }

    /// The vault's claims bound to `input` that are pending and were never reopened: what a confirmation names.
    function _pendingClaimsOfInput(bytes32 vaultId, bytes32 input) internal view returns (bytes32[] memory ids) {
        uint64 count = registry.claimCount(vaultId);
        bytes32[] memory found = new bytes32[](count);
        uint256 n;
        for (uint64 j; j < count; ++j) {
            bytes32 id = registry.claimIdOf(vaultId, j);
            (,,,, ClaimStatus status) = registry.claim(id);
            if (status == ClaimStatus.Pending && !reopened[id] && registry.claimInput(id) == input) found[n++] = id;
        }
        ids = new bytes32[](n);
        for (uint256 i; i < n; ++i) {
            ids[i] = found[i];
        }
    }

    /// Proven delivery (v4): an inclusion proof of a claim's committed payout (the MOCK slasher stands in for a verified
    /// `Included` proof) makes its payment final from any state before the compensator: committed and pending (also on
    /// a frozen vault), or confirmed by signatures. Never reverts for such a claim; afterwards `paidAt == 0`.
    function provePayout(uint256 vaultSeed, uint256 howSeed) external {
        if (vaultIds.length == 0) return;
        bool confirmedOnly = _mix(howSeed) % 2 == 0;
        // Goal-directed: the first provable claim of any vault, starting from the seeded one ...
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                if (!_provable(claimId, confirmedOnly)) continue;
                _prove(vaultId, claimId, confirmedOnly);
                return;
            }
        }
        // ... else a payout committed now, then proven before anyone confirms it.
        bytes32 v = vaultIds[vaultSeed % vaultIds.length];
        if (confirmedOnly || !_payable(v)) return;
        (bool ok, bytes32 next) = _payableClaim(v, false);
        if (!ok) return;
        bytes32[] memory ids = _withFee(v, next, howSeed % 3 == 0);
        if (_commit(v, ids, howSeed)) _prove(v, ids[0], false);
    }

    function _provable(bytes32 claimId, bool confirmedOnly) internal view returns (bool) {
        if (registry.claimInput(claimId) == bytes32(0)) return false;
        (,,,, ClaimStatus status) = registry.claim(claimId);
        (uint64 paidAt,) = IZecuredDisputes(address(registry)).payoutDispute(claimId);
        return confirmedOnly ? status == ClaimStatus.Paid && paidAt != 0 : status == ClaimStatus.Pending;
    }

    function _prove(bytes32 vaultId, bytes32 claimId, bool confirmedOnly) internal {
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        uint256 unproven = _payouts().unprovenConfirmations(vaultId);
        bytes32 payoutRef = _payoutRefOf(claimId); // before the prank: it reads the Registry
        vm.prank(address(gates));
        try disputes.upholdPayout(claimId, payoutRef) {
            (,,,, ClaimStatus afterwards) = registry.claim(claimId);
            (uint64 paidAfter, uint64 dueAfter) = disputes.payoutDispute(claimId);
            if (afterwards != ClaimStatus.Paid || paidAfter != 0 || dueAfter != 0) {
                _flag("a proven payout did not leave its claim paid for good");
            }
            if (confirmedOnly && _payouts().unprovenConfirmations(vaultId) != unproven - 1) {
                _flag("R-3: a proven confirmation still counted as unproven");
            }
            calls[confirmedOnly ? "prove confirmed" : "prove pending"]++;
        } catch {
            _flag("a committed payout's proof was refused");
        }
    }

    /// R-3: anyone makes a confirmation final once its dispute window is over, unless the vault froze within that
    /// window (then only a proof or the compensator settles it).
    function expireConfirmation(uint256 vaultSeed) external {
        if (vaultIds.length == 0) return;
        // Goal-directed: the first confirmation still open, of any vault, starting from the seeded one.
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                (,,,, ClaimStatus status) = registry.claim(claimId);
                (uint64 paidAt,) = IZecuredDisputes(address(registry)).payoutDispute(claimId);
                if (status != ClaimStatus.Paid || paidAt == 0) continue;
                _expire(vaultId, claimId, paidAt);
                return;
            }
        }
    }

    function _expire(bytes32 vaultId, bytes32 claimId, uint64 paidAt) internal {
        uint64 window = IZecuredDisputes(address(registry)).disputeWindow();
        if (block.timestamp <= uint256(paidAt) + window) vm.warp(uint256(paidAt) + window + 1);
        uint64 frozenSince = gates.frozenSince(vaultId);
        bool suspect = frozenSince != 0 && paidAt + window > frozenSince;
        vm.prank(stranger);
        try _payouts().expireConfirmation(claimId) {
            if (suspect) _flag("R-3: a confirmation made just before a freeze expired");
            calls["expire"]++;
        } catch {
            if (!suspect) _flag("an old confirmation could not expire");
            calls["expire (suspect)"]++;
        }
    }

    /// Tier 3: a payout committed (and mined) whose claims are not confirmed yet.
    function commitOnly(uint256 vaultSeed, uint256 signerSeed, bool withFee) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found || !_payable(vaultId)) return;
        (bool ok, bytes32 claimId) = _payableClaim(vaultId, false);
        if (!ok) return;
        if (_commit(vaultId, _withFee(vaultId, claimId, withFee), signerSeed)) calls["commit only"]++;
    }

    function _withFee(bytes32 vaultId, bytes32 claimId, bool withFee) internal view returns (bytes32[] memory ids) {
        (bool fee, bytes32 feeId) = _feeClaim(vaultId);
        if (!withFee || !fee) return _one(claimId);
        ids = new bytes32[](2);
        (ids[0], ids[1]) = (claimId, feeId);
    }

    /// A live lineage note of the vault worth at least `amount`: `preferred` if it qualifies (a refund is paid from its
    /// own note, I30), else the first that does.
    function _noteFor(bytes32 vaultId, uint64 amount, bytes32 preferred) internal view returns (bool, uint256) {
        bytes32[] storage n = liveNotes[vaultId];
        for (uint256 i; i < n.length; ++i) {
            if (n[i] == preferred && registry.lineageOf(n[i]).value >= amount && _spendable(n[i])) return (true, i);
        }
        for (uint256 i; i < n.length; ++i) {
            if (registry.lineageOf(n[i]).value >= amount && _spendable(n[i])) return (true, i);
        }
        return (false, 0);
    }

    /// R-12: a pending refund's note is spent by no payout until its claim opens.
    function _spendable(bytes32 tag) internal view returns (bool) {
        return tagState[tag] != TAG_REFUND_PENDING;
    }

    /// Tier 3: the signers commit the payout of `ids` from a lineage note of the vault before they sign it; the rest
    /// returns to the vault as change, a new lineage note. I22: it never reverts for any gate.
    function _commit(bytes32 vaultId, bytes32[] memory ids, uint256 signerSeed) internal returns (bool) {
        uint64 claimed;
        bytes32[] memory refs = new bytes32[](ids.length);
        uint8[] memory outputs = new uint8[](ids.length);
        for (uint256 i; i < ids.length; ++i) {
            (,, uint64 amount,,) = registry.claim(ids[i]);
            claimed += amount;
            refs[i] = keccak256(abi.encode("zecured.inv.claimRef", ids[i]));
            outputs[i] = uint8(i);
        }
        (bool ok, uint256 n) = _noteFor(vaultId, claimed, refundNoteOf[ids[0]]);
        if (!ok) return false;
        bytes32 input = liveNotes[vaultId][n];
        uint64 change = registry.lineageOf(input).value - claimed;
        SpendCommit memory c = SpendCommit({
            vaultId: vaultId,
            inputTag: input,
            claimIds: ids,
            claimRefs: refs,
            claimOutputs: outputs,
            payoutRef: keccak256(abi.encode("zecured.inv.payoutRef", input)),
            changeTag: change == 0 ? bytes32(0) : keccak256(abi.encode("zecured.inv.change", input)),
            changeValue: change
        });
        try registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c), signerSeed)) {
            // The input is spent; its change, if any, is the vault's new note.
            if (change == 0) _removeNote(vaultId, input);
            else liveNotes[vaultId][n] = c.changeTag;
            lastPayoutRef[vaultId] = c.payoutRef;
            calls["commit"]++;
            return true;
        } catch {
            _flag("I22: a valid payout commitment reverted");
            return false;
        }
    }

    // ------------------------------------------------------------------ disputes (T-05 (a))

    /// The claimant of one of the vault's confirmed claims disputes the confirmation within the dispute window.
    function dispute(uint256 vaultSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found || gates.isCompromised(vaultId)) return;
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            ClaimDetails memory d = registry.claimDetails(claimId);
            (uint64 paidAt,) = disputes.payoutDispute(claimId);
            if (d.status != ClaimStatus.Paid || paidAt == 0 || d.bondPaid || d.claimant == address(0)) continue;
            if (block.timestamp > uint256(paidAt) + disputes.disputeWindow()) continue;
            vm.prank(d.claimant);
            try disputes.disputePayout(claimId) {
                calls["dispute"]++;
            } catch {
                _flag("a claimant could not dispute a confirmation within its window");
            }
            return;
        }
    }

    /// The vault's first disputed claim is resolved: upheld by a proof of its payout (the MOCK slasher stands in for
    /// a verified `Included` proof), or, once the inclusion window is over, reopened by anyone. A proof also upholds a
    /// claim reopened earlier (D57), as long as it is still unpaid.
    function resolveDispute(uint256 vaultSeed, uint256 howSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        bool prove = _mix(howSeed) % 2 == 0; // the fuzzer favours false for a plain bool
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (,,,, ClaimStatus status) = registry.claim(claimId);
            bool late = prove && status == ClaimStatus.Pending && reopened[claimId];
            if (status != ClaimStatus.Disputed && !late) continue;
            if (prove) {
                // The handler's payout of an input has a reference derived from it (`_commit`).
                bytes32 payoutRef = keccak256(abi.encode("zecured.inv.payoutRef", registry.claimInput(claimId)));
                vm.prank(address(gates));
                try disputes.upholdPayout(claimId, payoutRef) {
                    calls[late ? "uphold late" : "uphold"]++;
                } catch {
                    _flag("a proven payout could not be upheld");
                }
            } else {
                (, uint64 dueAt) = disputes.payoutDispute(claimId);
                if (block.timestamp <= dueAt) return;
                vm.prank(stranger);
                try disputes.reopenDisputedClaim(claimId) {
                    reopened[claimId] = true;
                    calls["reopen"]++;
                } catch {
                    _flag("a dispute could not be reopened after its inclusion window");
                }
            }
            return;
        }
    }

    // ------------------------------------------------------------------ attacks (must always fail)

    /// Tier 3 claim reuse: a claim committed from one lineage note, mined but not confirmed, is committed again from
    /// another live note of the vault to be paid twice. Must always fail.
    function attackClaimReuse(uint256 vaultSeed, uint256 signerSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found || liveNotes[vaultId].length == 0 || !_payable(vaultId)) return;
        (bool committed, bytes32 claimId) = _payableClaim(vaultId, true);
        if (!committed) return;
        bytes32 head = liveNotes[vaultId][signerSeed % liveNotes[vaultId].length];
        (bool possible, SpendCommit memory c) = _reuseCommit(vaultId, head, claimId);
        if (!possible) return;
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(c), signerSeed);
        try registry.commitSpend(c, sigs) {
            attackSucceeded = true;
        } catch {}
        calls["attack: claim reuse"]++;
    }

    /// The commitment of `claimId` from `head` that would conserve value, if the head can cover the claim.
    function _reuseCommit(bytes32 vaultId, bytes32 head, bytes32 claimId)
        internal
        view
        returns (bool possible, SpendCommit memory c)
    {
        (,, uint64 amount,,) = registry.claim(claimId);
        uint64 lineage = registry.lineageOf(head).value;
        if (amount > lineage) return (false, c);
        bytes32[] memory refs = new bytes32[](1);
        refs[0] = keccak256(abi.encode("zecured.inv.reuseRef", head, claimId));
        c = SpendCommit({
            vaultId: vaultId,
            inputTag: head,
            claimIds: _one(claimId),
            claimRefs: refs,
            claimOutputs: new uint8[](1),
            payoutRef: keccak256(abi.encode("zecured.inv.reusePayout", head, claimId)),
            changeTag: amount == lineage ? bytes32(0) : keccak256(abi.encode("zecured.inv.reuseChange", head)),
            changeValue: lineage - amount
        });
        possible = true;
    }

    /// Tier 3: a claim is never confirmed paid without a payout committed for it, even under the reference of another
    /// payout of its vault (R-1). Must always fail.
    function attackUncommittedConfirm(uint256 vaultSeed, uint256 signerSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        bytes32 payoutRef = lastPayoutRef[vaultId];
        if (payoutRef == bytes32(0)) payoutRef = keccak256(abi.encode("zecured.inv.noPayout", vaultId));
        uint64 count = registry.claimCount(vaultId);
        for (uint64 i; i < count; ++i) {
            bytes32 claimId = registry.claimIdOf(vaultId, i);
            (,,,, ClaimStatus status) = registry.claim(claimId);
            if (status != ClaimStatus.Pending || registry.claimInput(claimId) != bytes32(0)) continue;
            bytes32[] memory ids = _one(claimId);
            bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, payoutRef, ids), signerSeed);
            try _payouts().confirmPayout(vaultId, payoutRef, ids, sigs) {
                attackSucceeded = true;
            } catch {}
            calls["attack: uncommitted confirm"]++;
            return;
        }
    }

    /// R-1: a confirmation is never replayed: the same signatures again, or a reopened claim confirmed under its old
    /// payout. A final claim is never proven, disputed or confirmed again. Must always fail.
    function attackReplayOrFinal(uint256 vaultSeed, uint256 signerSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        IZecuredDisputes disputes = IZecuredDisputes(address(registry));
        uint64 count = registry.claimCount(vaultId);
        for (uint64 i; i < count; ++i) {
            bytes32 claimId = registry.claimIdOf(vaultId, i);
            ClaimDetails memory c = registry.claimDetails(claimId);
            (uint64 paidAt,) = disputes.payoutDispute(claimId);
            bytes32 input = registry.claimInput(claimId);
            if (input == bytes32(0)) continue;
            bytes32 payoutRef = _payoutRefOf(claimId);
            bool reopenedPending = c.status == ClaimStatus.Pending && reopened[claimId];
            bool finalPaid = c.status == ClaimStatus.Paid && paidAt == 0;
            if (!reopenedPending && !finalPaid && !(c.status == ClaimStatus.Paid && paidAt != 0)) continue;
            bytes[] memory sigs =
                _threeSigs(_payouts().hashPayoutConfirmation(vaultId, payoutRef, _one(claimId)), signerSeed);
            try _payouts().confirmPayout(vaultId, payoutRef, _one(claimId), sigs) {
                attackSucceeded = true;
            } catch {}
            if (finalPaid) {
                vm.prank(address(gates));
                try disputes.upholdPayout(claimId, payoutRef) {
                    attackSucceeded = true;
                } catch (bytes memory reason) {
                    if (bytes4(reason) != IZecuredDisputesEvents.AlreadyFinal.selector) {
                        _flag("a final claim's second proof failed for another reason");
                    }
                }
                if (c.claimant != address(0)) {
                    vm.prank(c.claimant);
                    try disputes.disputePayout(claimId) {
                        attackSucceeded = true;
                    } catch {}
                }
            }
            calls["attack: replay or final"]++;
            return;
        }
    }

    /// I21: a refund without a destination is never committed, even by valid signatures. Must always fail.
    function attackUnsetDestination(uint256 seed) external {
        (bool found, bytes32 vaultId, bytes32 claimId) = _refundWithoutDestination(seed);
        if (!found || liveNotes[vaultId].length == 0) return;
        (bool possible, SpendCommit memory c) = _reuseCommit(vaultId, liveNotes[vaultId][0], claimId);
        if (!possible) return;
        try registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c), seed)) {
            attackSucceeded = true;
        } catch {}
        calls["attack: unset destination"]++;
    }

    /// T-02: only the vault's account names where its refund goes. Must always fail.
    function attackRefundDestination(uint256 seed) external {
        (bool found,, bytes32 claimId) = _refundWithoutDestination(seed);
        if (!found) return;
        address[3] memory intruders = [stranger, liquidator, marketMaker];
        vm.prank(intruders[seed % 3]);
        try registry.setRefundDestination(claimId, keccak256("zecured.inv.stolenRefund")) {
            attackSucceeded = true;
        } catch {}
        calls["attack: refund destination"]++;
    }

    /// I20: a note lands once: a tag the Intake accepted (pending, activated or refunded) is never accepted again, for
    /// its own vault or another one. Must always fail.
    function attackDoubleLanding(uint256 seed) external {
        if (tags.length == 0) return;
        bytes32 tag = tags[seed % tags.length];
        if (tagState[tag] == TAG_DROPPED) return;
        Deposit memory d = Deposit({
            vaultId: seed % 2 == 0 ? tagVault[tag] : keccak256(abi.encode("zecured.inv.otherVault", seed)),
            accountIndex: (seed >> 8) % ACCOUNT_COUNT,
            amount: 1_000_000,
            tag: tag
        });
        capPrice.set(oracle.price());
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proof(d);
        try intake.attestDepositWithProof(journal, seal, imageId, sigs) {
            attackSucceeded = true;
        } catch {}
        calls["attack: double landing"]++;
    }

    /// A proof without all five `VaultOpened` signatures never lands: four signers, or one of five replaced by an
    /// outsider. Must always fail.
    function attackWeakOpened(uint256 seed) external {
        Deposit memory d = Deposit({
            vaultId: keccak256(abi.encode("zecured.inv.forgedVault", seed)),
            accountIndex: seed % ACCOUNT_COUNT,
            amount: 1_000_000,
            tag: keccak256(abi.encode("zecured.inv.forgedTag", seed))
        });
        capPrice.set(oracle.price());
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proof(d);
        if (seed % 2 == 0) {
            bytes[] memory four = new bytes[](4);
            for (uint256 i; i < 4; ++i) {
                four[i] = sigs[i];
            }
            sigs = four;
        } else {
            (, uint256 outsiderKey) = makeAddrAndKey("zecured.inv.outsider");
            sigs[seed % 5] = _sign(outsiderKey, keccak256(abi.encode(sigs[0])));
        }
        try intake.attestDepositWithProof(journal, seal, imageId, sigs) {
            attackSucceeded = true;
        } catch {}
        calls["attack: weak VaultOpened"]++;
    }

    /// K3: a pending activation is never finalized early (before its height and delay) or by calling the Registry's
    /// hooks directly. Must always fail.
    function attackActivation(uint256 seed) external {
        if (pendingIds.length == 0) return;
        bytes32 vaultId = pendingIds[seed % pendingIds.length];
        PendingActivation memory p = intake.pendingActivation(vaultId);
        if (seed % 2 == 0) {
            if (relay.tipHeight() >= p.readyHeight && block.timestamp >= p.readyTime) return;
            try intake.finalizeActivation(vaultId) {
                attackSucceeded = true;
            } catch {}
        } else {
            vm.prank(stranger);
            try registry.activate(vaultId, p.account, p.amount, p.depositTag) {
                attackSucceeded = true;
            } catch {}
        }
        calls["attack: activation"]++;
    }

    function attackAccountWithdraw(uint256 vaultSeed, uint256 receiverSeed, uint256 amount) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (, uint64 outstanding) = _active(vaultId);
        if (outstanding == 0) return;
        address[4] memory receivers = [account, stranger, address(registry), address(adapter)];
        vm.prank(account);
        try morpho.withdrawCollateral(params, bound(amount, 1, outstanding), account, receivers[receiverSeed % 4]) {
            attackSucceeded = true;
        } catch {}
        calls["attack: account withdraw"]++;
    }

    function attackDirectLiquidation(uint256 vaultSeed, uint256 amount) external {
        (bool found,, address account) = _vault(vaultSeed);
        if (!found) return;
        uint256 shares = morpho.position(marketId, account).borrowShares;
        if (shares == 0) return;
        _fund(liquidator, address(morpho), 100 * 1e6);
        vm.prank(liquidator);
        try morpho.liquidate(params, account, 0, bound(amount, 1, shares), "") {
            attackSucceeded = true;
        } catch {}
        calls["attack: direct liquidation"]++;
    }

    /// Nobody but the vault's account can partially release its collateral.
    function attackPartialRelease(uint256 vaultSeed, uint256 amountSeed) external {
        (bool found, bytes32 vaultId,) = _vault(vaultSeed);
        if (!found) return;
        (, uint64 outstanding) = _active(vaultId);
        uint64 minRelease = registry.MIN_RELEASE();
        if (outstanding < 2 * minRelease) return;
        address[3] memory intruders = [stranger, liquidator, marketMaker];
        vm.prank(intruders[amountSeed % 3]);
        try registry.requestPartialRelease(
            vaultId, uint64(bound(amountSeed, minRelease, outstanding - minRelease)), keccak256("zecured.inv.x")
        ) {
            attackSucceeded = true;
        } catch {}
        calls["attack: partial release"]++;
    }

    function attackFlashLoan(uint256 amount) external {
        uint256 held = zzec.balanceOf(address(morpho));
        if (held == 0) return;
        vm.prank(stranger);
        try morpho.flashLoan(address(zzec), bound(amount, 1, held), "") {
            attackSucceeded = true;
        } catch {}
        calls["attack: flash loan"]++;
    }

    /// I8: a deleverage request executed by anyone but its executor, or after its deadline, never withdraws.
    function attackDeleverage(uint256 vaultSeed, bool expired) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        DeleverageRequest memory req = DeleverageRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: 100,
            repayAssets: 1e6,
            maxCollateral: type(uint64).max,
            nonce: type(uint256).max - nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashDeleverageRequest(req));
        _fund(stranger, address(adapter), 100 * 1e6);
        address caller = stranger;
        if (expired) {
            vm.warp(uint256(req.deadline) + 1);
            caller = marketMaker;
        }
        vm.prank(caller);
        try adapter.deleverageWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), 0) {
            attackSucceeded = true;
        } catch {}
        calls["attack: deleverage"]++;
    }

    /// I8: a valid signed close request executed by someone else, or after its deadline, never withdraws collateral.
    function attackCloseRequest(uint256 vaultSeed, bool expired) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        CloseRequest memory req = CloseRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: 100,
            minUserAmount: 0,
            userCommitment: keccak256("zecured.inv.user"),
            nonce: type(uint256).max - nextNonce++,
            deadline: uint64(block.timestamp + 10 minutes)
        });
        bytes memory sig = _sign(accountKeys[accountIndexOf[vaultId]], adapter.hashCloseRequest(req));
        _fund(stranger, address(adapter), 100 * 1e6);
        _fund(marketMaker, address(adapter), 100 * 1e6);
        address caller = stranger;
        if (expired) {
            vm.warp(uint256(req.deadline) + 1);
            caller = marketMaker;
        }
        vm.prank(caller);
        try adapter.closeWithCollateral(req, sig, keccak256("zecured.inv.marketMaker"), type(uint256).max, 0) {
            attackSucceeded = true;
        } catch {}
        calls["attack: close request"]++;
    }

    // ------------------------------------------------------------------ pre-liquidation (v4)

    /// Terms within the bounds, drawn from `seed`: preLltv 45 % to 60 %, close factors from 1 % up to 100 %, incentive
    /// factors from 1 up to Morpho's 1.1268 at this LLTV, for 1 to 60 days.
    function _drawTerms(uint256 seed) internal view returns (PreLiquidationTerms memory t) {
        seed = _mix(seed);
        uint256 lif = 1_126_760_563_380_281_690; // min(1.15, 1 / (1 - 0.3 * (1 - 0.625)))
        t.preLltv = bound(seed % 1e6, 450_000, 600_000) * 1e12;
        t.preLcf1 = bound((seed >> 32) % 1e6, 10_000, 500_000) * 1e12;
        t.preLcf2 = bound((seed >> 64) % 1e6, t.preLcf1 / 1e12, 1_000_000) * 1e12;
        t.preLif1 = 1e18 + bound((seed >> 96) % 1e6, 0, 50_000) * 1e12;
        t.preLif2 = bound(t.preLif1 + ((seed >> 128) % 1e17), t.preLif1, lif);
        t.expiry = uint64(block.timestamp + bound((seed >> 192) % 1e9, 1 days, 60 days));
    }

    /// The account of an active vault opts into pre-liquidation (or, now and then, withdraws its terms).
    function setPreLiquidationTerms(uint256 vaultSeed, uint256 termsSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active,) = _active(vaultId);
        if (!active) return;
        if (_mix(termsSeed) % 8 == 0) {
            vm.prank(account);
            adapter.clearPreLiquidation();
            calls["preLiquidation terms cleared"]++;
            return;
        }
        PreLiquidationTerms memory t = _drawTerms(termsSeed);
        vm.prank(account);
        try adapter.setPreLiquidation(t) {
            calls["preLiquidation terms set"]++;
        } catch {
            _flag("an account could not set valid pre-liquidation terms");
        }
    }

    /// Morpho's state after interest about `account`, restated independently: whether the position is in its terms'
    /// band, the most shares one pre-liquidation may repay, and the collateral `repaidShares` buy (Morpho's
    /// PreLiquidation arithmetic, every step rounded down but the LTV).
    struct PreQuote {
        bool inBand;
        uint256 repayable;
        uint256 seized;
        uint256 ltv;
        // the state it is computed from
        uint256 totalBorrowAssets;
        uint256 totalBorrowShares;
        uint256 shares;
        uint256 price;
        uint256 value;
        uint256 borrowed;
        uint256 q;
        uint256 preLif;
    }

    function _preQuote(address account, PreLiquidationTerms memory t, uint256 repaidShares)
        internal
        view
        returns (PreQuote memory r)
    {
        (,, r.totalBorrowAssets, r.totalBorrowShares) = morpho.expectedMarketBalances(params);
        r.shares = morpho.position(marketId, account).borrowShares;
        r.price = oracle.price();
        r.value = uint256(morpho.position(marketId, account).collateral) * r.price / ORACLE_PRICE_SCALE;
        r.borrowed =
            (r.shares * (r.totalBorrowAssets + 1) + (r.totalBorrowShares + 1e6) - 1) / (r.totalBorrowShares + 1e6);
        if (r.value == 0 || r.shares == 0) return r;
        r.inBand = r.borrowed <= r.value * params.lltv / 1e18 && r.borrowed > r.value * t.preLltv / 1e18;
        if (!r.inBand) return r;
        r.ltv = (r.borrowed * 1e18 + r.value - 1) / r.value;
        r.q = (r.ltv - t.preLltv) * 1e18 / (params.lltv - t.preLltv);
        r.repayable = r.shares * (t.preLcf1 + r.q * (t.preLcf2 - t.preLcf1) / 1e18) / 1e18;
        r.preLif = t.preLif1 + r.q * (t.preLif2 - t.preLif1) / 1e18;
        uint256 assets = repaidShares * (r.totalBorrowAssets + 1) / (r.totalBorrowShares + 1e6);
        r.seized = (assets * r.preLif / 1e18) * ORACLE_PRICE_SCALE / r.price;
    }

    /// Goal-directed: makes sure the vault has a loan and terms in force, moves the price so that its LTV is around the
    /// band (mostly inside, sometimes just outside), then the liquidator pre-liquidates up to the close factor's cap
    /// (sometimes past it). Whatever the outcome, it must be the one the terms and the floors say.
    function preLiquidate(uint256 vaultSeed, uint256 ltvSeed, uint256 sharesSeed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstandingBefore) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstandingBefore, 5_000)) return;
        _authorize(account);
        PreLiquidationTerms memory t = adapter.preLiquidationTerms(account);
        if (block.timestamp >= t.expiry) {
            t = _drawTerms(ltvSeed);
            vm.prank(account);
            adapter.setPreLiquidation(t);
        }
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        // An LTV from preLltv - 1 % to LLTV + 1 %.
        uint256 ltv = bound(_mix(ltvSeed) % 1e9, t.preLltv / 1e10 - 1e6, 63_500_000) * 1e10;
        uint256 price = debt * 1e18 * ORACLE_PRICE_SCALE / (ltv * outstandingBefore);
        if (price == 0) return;
        oracle.setPrice(price);
        PreQuote memory r = _preQuote(account, t, 0);
        // Mostly the cap (what a pre-liquidator takes), else anything up to an eighth past it.
        uint256 repaidShares =
            _mix(sharesSeed) % 4 == 0 ? bound(sharesSeed, 1, r.repayable + r.repayable / 8 + 1) : r.repayable;
        if (repaidShares == 0) repaidShares = 1;
        r = _preQuote(account, t, repaidShares);
        _fund(liquidator, address(adapter), debt + 1e6);
        _tryPreLiquidate(vaultId, account, outstandingBefore, repaidShares, r);
    }

    function _tryPreLiquidate(
        bytes32 vaultId,
        address account,
        uint64 outstandingBefore,
        uint256 repaidShares,
        PreQuote memory r
    ) internal {
        uint256 floor = adapter.minSeize(outstandingBefore);
        bool frozen = gates.vaultFrozen(vaultId);
        bool allowed = r.inBand && repaidShares <= r.repayable && r.seized >= floor
            && r.seized + floor <= outstandingBefore && !frozen;
        uint64 claimsBefore = registry.claimCount(vaultId);
        vm.prank(liquidator);
        try adapter.preLiquidate(account, repaidShares, keccak256("zecured.inv.preLiquidator")) returns (
            uint256, uint64 seized
        ) {
            if (!allowed) _flag("a pre-liquidation outside the terms, the floors or a freeze went through");
            calls["preLiquidate"]++;
            _checkPreLiquidation(vaultId, account, claimsBefore, outstandingBefore, seized, r);
        } catch {
            if (allowed) _flag("an honest pre-liquidation reverted");
            calls["preLiquidate (refused)"]++;
        }
    }

    /// I9 and D11 at the moment of a pre-liquidation: one Liquidation claim of exactly what left the vault, the
    /// restated amount, the liquidator its claimant, the vault still open with at least the floor; the LTV fell.
    function _checkPreLiquidation(
        bytes32 vaultId,
        address account,
        uint64 claimsBefore,
        uint64 outstandingBefore,
        uint64 seized,
        PreQuote memory r
    ) internal {
        (bool stillActive, uint64 outstandingAfter) = _active(vaultId);
        ClaimDetails memory c = registry.claimDetails(registry.claimIdOf(vaultId, claimsBefore));
        if (
            !stillActive || seized != r.seized || uint256(seized) + outstandingAfter != outstandingBefore
                || c.amount != seized || c.kind != ClaimKind.Liquidation || c.claimant != liquidator
                || outstandingAfter < adapter.minSeize(outstandingBefore)
                || registry.claimCount(vaultId) != claimsBefore + 1
        ) i9Violated = true;
        uint256 value = uint256(outstandingAfter) * oracle.price() / ORACLE_PRICE_SCALE;
        if (value != 0 && morpho.expectedBorrowAssets(params, account) * 1e18 / value >= r.ltv) {
            _flag("a pre-liquidation did not lower the LTV");
        }
    }

    /// Pre-liquidation without consent: an account without terms in force, or a position below its terms' band, is
    /// never pre-liquidated. Must always fail.
    function attackPreLiquidation(uint256 vaultSeed, uint256 seed) external {
        (bool found, bytes32 vaultId, address account) = _vault(vaultSeed);
        if (!found) return;
        (bool active, uint64 outstanding) = _active(vaultId);
        if (!active || !_ensureDebt(account, outstanding, 5_000)) return;
        PreLiquidationTerms memory t = adapter.preLiquidationTerms(account);
        if (block.timestamp < t.expiry) {
            // Terms in force: the LTV 0.5 % to 5 % below the band.
            uint256 debt = morpho.expectedBorrowAssets(params, account);
            uint256 ltv = t.preLltv - bound(_mix(seed) % 1e9, 5e15, 5e16);
            oracle.setPrice(debt * 1e18 * ORACLE_PRICE_SCALE / (ltv * outstanding) + 1);
        }
        uint256 shares = bound(seed, 1, morpho.position(marketId, account).borrowShares);
        _fund(stranger, address(adapter), 100 * 1e6);
        vm.prank(stranger);
        try adapter.preLiquidate(account, shares, keccak256("zecured.inv.x")) {
            attackSucceeded = true;
        } catch {}
        calls["attack: pre-liquidation"]++;
    }
}
