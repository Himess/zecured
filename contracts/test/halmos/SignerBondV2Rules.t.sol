// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {SignerBondV2} from "../../src/SignerBondV2.sol";
import {BondConfig} from "../../src/interfaces/v3/ISignerBond.sol";
import {ClaimDetails, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";

/// @notice MOCK (tests only): the Registry's sorted signer set, the only part of it a deposit reads, the dispute
///         windows the bond's constructor reads (R-18), and one claim whose details the check sets (R-5).
contract SignerSetForHalmos {
    ClaimDetails internal _claim;

    function setClaim(ClaimDetails memory c) external {
        _claim = c;
    }

    function claimDetails(bytes32) external view returns (ClaimDetails memory) {
        return _claim;
    }

    function claimInput(bytes32) external pure returns (bytes32) {
        return bytes32(0);
    }

    function lastCommitAt(bytes32) external pure returns (uint40) {
        return 0;
    }

    function markBondPaid(bytes32) external pure {}

    function disputeWindow() external pure returns (uint64) {
        return 1 days;
    }

    function inclusionWindow() external pure returns (uint64) {
        return 6 hours;
    }

    function signers() external pure returns (address[5] memory s) {
        s[0] = address(0x5101);
        s[1] = address(0x5102);
        s[2] = address(0x5103);
        s[3] = address(0x5104);
        s[4] = address(0x5105);
    }
}

/// @notice MOCK (tests only): the slasher, the compensator and the oracle a liveness payment reads: no vault frozen, no
///         entitlement, a fixed price ($1,200 per ZEC) and no feed state.
contract BondPeersForHalmos {
    function vaultFrozen(bytes32) external pure returns (bool) {
        return false;
    }

    function entitlement(bytes32) external pure returns (Entitlement memory e) {}

    function price() external pure returns (uint256) {
        return 1_200e34;
    }
}

/// @notice Test harness: the real bond, with the first step of its effective bond callable on its own, and any arming
///         settable (any state of the receipts' map).
contract SignerBondV2HalmosHarness is SignerBondV2 {
    constructor(BondConfig memory cfg) SignerBondV2(cfg) {}

    function sumAndCheapestThree(uint256[5] memory bonds) external pure returns (uint256, uint256) {
        return _sumAndCheapestThree(bonds);
    }

    function setArming(bytes32 claimId, bytes32 commitment, uint64 payableAt) external {
        _armings[claimId] = Arming({commitment: commitment, payableAt: payableAt});
    }
}

/// @notice Symbolic checks of SignerBondV2's effective bond with Halmos (docs/TESTING.md § 1b; design notes § 2.9), on
///         the real bond. Bonds, amounts and the signer are symbolic, so each check covers every value, not samples
///         (bonds and amounts below 2^64 USDC units, 18 trillion USDC, so that no sum overflows):
///         B1  the effective bond never exceeds the sum of the bonds, from ANY state of the bond (symbolic storage);
///         B2  S3, the effective bond's second term, is the sum of the three smallest bonds: of three bonds none of
///             which exceeds either of the other two. Any other three signers hold at least as much (they swap some of
///             these for bonds at least as large), so three colluders lose at least 3/5 of the effective bond (T-01);
///         B3  a deposit adds exactly its amount to its signer's bond and changes no other bond and no unbonding, from
///             ANY state;
///         B4  (R-5) whatever the claim and whatever its arming: no liveness is due, nor paid, unless the claim is armed
///             for its current commitment; and when it is due, it is a liveness period after the later of the claim's
///             creation and the receipt, at the earliest.
///         Together: a deposit never lowers the effective bond. It raises one bond (B3). Raising one bond raises the
///         sum by as much and, S3 being the sum of the three smallest (B2), never lowers S3; and `min(sum, 5 * S3 / 3)`
///         is monotone in both.
/// @dev Those two monotonicity steps are argued, not checked: neither solver that Halmos ships with (yices, z3) proves
///      inequalities between sums of different bonds, or the monotonicity of a 256-bit `5 * x / 3`, in useful time. B2
///      is therefore stated with equalities and pairwise comparisons only. test/unit/SignerBondV2.t.sol fuzzes the whole
///      property (`testFuzz_deposit_neverLowersTheEffectiveBond`) and the effective bond against a brute force over
///      every triple. The slasher, the Intake, the compensator and the oracle are plain addresses: neither the effective
///      bond nor a deposit calls them. The token is MockUSDC.
contract SignerBondV2RulesCheck is Test {
    uint256 internal constant LIMIT = 2 ** 64;

    SignerSetForHalmos internal registry;
    MockUSDC internal usdc;
    SignerBondV2HalmosHarness internal bond;
    BondPeersForHalmos internal peers;

    bytes32 internal constant CLAIM = keccak256("halmos.claim");
    address internal constant CLAIMANT = address(0xC1A1);

    function setUp() public {
        registry = new SignerSetForHalmos();
        usdc = new MockUSDC(address(this));
        peers = new BondPeersForHalmos();
        bond = new SignerBondV2HalmosHarness(
            BondConfig({
                registry: address(registry),
                slasher: address(peers),
                intake: address(0x6002),
                compensator: address(peers),
                usdc: IERC20(address(usdc)),
                oracle: IOracle(address(peers)),
                minBond: 100e6,
                unbondingPeriod: 2 days + 6 hours,
                challengeWindow: 1 days,
                bountyBps: 1_000,
                bountyCap: 50e6,
                livenessPeriod: 1 days,
                commitGrace: 2 hours,
                livenessPremiumBps: 500
            })
        );
    }

    // Branch-free comparisons (1 or 0), so that the checks add no paths of their own.
    function _le(uint256 a, uint256 b) internal pure returns (uint256 r) {
        assembly ("memory-safe") {
            r := iszero(gt(a, b))
        }
    }

    function _eq(uint256 a, uint256 b) internal pure returns (uint256 r) {
        assembly ("memory-safe") {
            r := eq(a, b)
        }
    }

    function _bounded(uint256[5] memory b) internal pure {
        for (uint256 i; i < 5; ++i) {
            vm.assume(b[i] < LIMIT);
        }
    }

    /// Any state of the bond, and its five bonds.
    function _anyState() internal returns (uint256[5] memory b) {
        vm.setArbitraryStorage(address(bond));
        for (uint8 i; i < 5; ++i) {
            b[i] = bond.bondOf(i);
        }
        _bounded(b);
    }

    /// B1.
    function check_effectiveBondNeverExceedsTheSum() public {
        uint256[5] memory b = _anyState();
        uint256 sum;
        unchecked {
            sum = b[0] + b[1] + b[2] + b[3] + b[4];
        }
        assert(bond.effectiveBond() <= sum);
    }

    /// B2. With z3: unlike yices (which times out here), it chains the loop's comparisons (b <= c and c <= d, so b <= d)
    /// at once.
    /// @custom:halmos --solver z3
    function check_cheapestThreeAreTheThreeSmallest(uint256[5] memory b) public view {
        _bounded(b);
        (uint256 sum, uint256 s3) = bond.sumAndCheapestThree(b);
        // Some two signers u and v are left out: S3 is what the other three hold, and none of them holds more than u
        // or v. The ten pairs, from (3, 4) to (0, 1).
        uint256 found = _theOthers(b, s3, 3, 4) | _theOthers(b, s3, 2, 4) | _theOthers(b, s3, 2, 3)
            | _theOthers(b, s3, 1, 4) | _theOthers(b, s3, 1, 3) | _theOthers(b, s3, 1, 2) | _theOthers(b, s3, 0, 4)
            | _theOthers(b, s3, 0, 3) | _theOthers(b, s3, 0, 2) | _theOthers(b, s3, 0, 1);
        uint256 ok;
        unchecked {
            ok = _eq(sum, b[0] + b[1] + b[2] + b[3] + b[4]) & found;
        }
        assert(ok == 1);
    }

    /// 1 if `s3` is the sum of the bonds other than `u`'s and `v`'s, and none of those exceeds `b[u]` or `b[v]`.
    function _theOthers(uint256[5] memory b, uint256 s3, uint256 u, uint256 v) internal pure returns (uint256 ok) {
        uint256 rest;
        ok = 1;
        unchecked {
            for (uint256 i; i < 5; ++i) {
                if (i == u || i == v) continue;
                rest += b[i];
                ok &= _le(b[i], b[u]) & _le(b[i], b[v]);
            }
        }
        ok &= _eq(s3, rest);
    }

    /// B3, on the real deposit path.
    function check_depositOnlyAddsItsAmountToItsSignersBond(uint8 signer, uint256 amount) public {
        uint256[5] memory b = _anyState();
        uint256[5] memory unbonding;
        uint64[5] memory readyAt;
        for (uint8 i; i < 5; ++i) {
            (unbonding[i], readyAt[i]) = bond.unbondingOf(i);
        }
        vm.assume(!bond.halted());
        vm.assume(amount != 0 && amount < LIMIT);
        // One path per signer, each with a constant index.
        uint8 index;
        address who;
        if (signer == 0) (index, who) = (0, address(0x5101));
        else if (signer == 1) (index, who) = (1, address(0x5102));
        else if (signer == 2) (index, who) = (2, address(0x5103));
        else if (signer == 3) (index, who) = (3, address(0x5104));
        else (index, who) = (4, address(0x5105));
        usdc.mint(who, amount);
        vm.startPrank(who);
        usdc.approve(address(bond), amount);
        bond.deposit(amount);
        vm.stopPrank();
        uint256 ok = 1;
        unchecked {
            for (uint8 i; i < 5; ++i) {
                ok &= _eq(bond.bondOf(i), i == index ? b[i] + amount : b[i]);
                (uint256 u, uint64 r) = bond.unbondingOf(i);
                ok &= _eq(u, unbonding[i]) & _eq(r, readyAt[i]);
            }
        }
        assert(ok == 1);
    }

    /// The claim and its arming, symbolic; the bonds hold enough for any claim below 2^64 zatoshi at $1,200.
    function _armedClaim(
        uint8 kind,
        bytes32 commitment,
        uint40 createdAt,
        uint64 amount,
        bytes32 armedFor,
        uint64 armedAt
    ) internal {
        vm.assume(kind <= uint8(type(ClaimKind).max));
        registry.setClaim(
            ClaimDetails({
                vaultId: keccak256("halmos.vault"),
                kind: ClaimKind(kind),
                status: ClaimStatus.Pending,
                amount: amount,
                commitment: commitment,
                createdAt: createdAt,
                bondPaid: false,
                claimant: CLAIMANT
            })
        );
        bond.setArming(CLAIM, armedFor, armedAt);
    }

    /// B4, the view.
    function check_noLivenessDueWithoutAMatchingReceipt(
        uint8 kind,
        bytes32 commitment,
        uint40 createdAt,
        uint64 amount,
        bytes32 armedFor,
        uint64 armedAt
    ) public {
        _armedClaim(kind, commitment, createdAt, amount, armedFor, armedAt);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        if (armedAt == 0 || armedFor != commitment) {
            assert(dueAt == 0);
        } else if (dueAt != 0) {
            uint256 from = armedAt > createdAt ? armedAt : createdAt;
            assert(uint256(dueAt) >= from + 1 days); // the livenessPeriod of setUp
        }
    }

    /// Funds every bond with 10^30 USDC units: no claim below 2^64 zatoshi outgrows them.
    function _fundBonds() internal {
        address[5] memory s = registry.signers();
        for (uint256 i; i < 5; ++i) {
            usdc.mint(s[i], 1e30);
            vm.startPrank(s[i]);
            usdc.approve(address(bond), 1e30);
            bond.deposit(1e30);
            vm.stopPrank();
        }
    }

    /// B4, the payment, unarmed: a claim the bonds could pay (an eligible kind, a destination, not dust), with the
    /// bonds funded and any time past, is refused unless armed for its current commitment.
    function check_noLivenessPaidWithoutAMatchingReceipt(
        uint8 kind,
        bytes32 commitment,
        uint40 createdAt,
        uint64 amount,
        bytes32 armedFor,
        uint64 armedAt,
        uint40 late
    ) public {
        vm.assume(kind <= uint8(ClaimKind.Refund) && commitment != bytes32(0) && amount >= 100_000);
        vm.assume(armedAt == 0 || armedFor != commitment);
        vm.assume(armedAt < 2 ** 40);
        _armedClaim(kind, commitment, createdAt, amount, armedFor, armedAt);
        _fundBonds();
        uint256 from = armedAt > createdAt ? armedAt : createdAt;
        vm.warp(from + 1 days + late);
        vm.prank(CLAIMANT);
        (bool ok,) = address(bond).call(abi.encodeCall(SignerBondV2.slashLiveness, (CLAIM)));
        assert(!ok);
    }

    /// B4, the payment, armed: the same claim (0.01 ZEC: a concrete amount keeps the payment's 512-bit arithmetic out
    /// of the solver), armed for its current commitment at any time, is paid once due, whatever the kind, the commitment,
    /// the creation, the receipt's time and the delay.
    function check_livenessPaidWithAMatchingReceipt(
        uint8 kind,
        bytes32 commitment,
        uint40 createdAt,
        uint64 armedAt,
        uint40 late
    ) public {
        vm.assume(kind <= uint8(ClaimKind.Refund) && commitment != bytes32(0));
        vm.assume(armedAt != 0 && armedAt < 2 ** 40);
        _armedClaim(kind, commitment, createdAt, 1_000_000, commitment, armedAt);
        _fundBonds();
        uint256 from = armedAt > createdAt ? armedAt : createdAt;
        vm.warp(from + 1 days + late);
        vm.prank(CLAIMANT);
        (bool ok,) = address(bond).call(abi.encodeCall(SignerBondV2.slashLiveness, (CLAIM)));
        assert(ok);
    }
}
