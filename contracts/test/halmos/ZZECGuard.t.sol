// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";

/// @notice Symbolic checks of the zZEC transfer guard with Halmos (docs/TESTING.md § 1b). Callers, recipients, amounts
///         and the function called are symbolic, so each `check_` covers every value of them, not samples. Each one
///         starts from ANY state of the token: its storage (every balance and allowance, the supply) is symbolic, and
///         the expectation, which lives in transient storage, is armed by the Registry, the Adapter or no one. A check
///         is thus a property of every single call, wherever it comes in a sequence.
/// @dev The Registry, the Adapter and Morpho are plain addresses here; RegistryRules.t.sol runs the real Registry
///      against this token. The properties are the guard's rules (src/ZZEC.sol, invariant I2 on the token side):
///      G1  mint: only by the Registry, only to the Registry;
///      G2  burn: only by the Registry, only from the Registry or the Adapter;
///      G3  the expectation is only ever cleared, or armed by the Registry or the Adapter for itself;
///      G4  into Morpho: only the Registry's zZEC, pulled by Morpho;
///      G5  out of Morpho: only to the armed recipient, at most the armed amount, once; only that recipient settles it,
///          and it learns exactly the amount received;
///      G6  no address other than the Registry, the Adapter and Morpho ever holds zZEC (an induction step);
///      G7  the supply changes only through G1 and G2, by exactly the amount.
contract ZZECGuardCheck is Test {
    address internal constant REGISTRY = address(0x1000);
    address internal constant ADAPTER = address(0x2000);
    address internal constant MORPHO = address(0x3000);

    /// Any single zZEC call: `op` selects the function, `caller` sends it, `a`, `b` and `amount` are its arguments.
    struct Call {
        uint8 op;
        address caller;
        address a;
        address b;
        uint256 amount;
    }

    ZZEC internal zzec;

    function setUp() public {
        zzec = new ZZEC(REGISTRY, ADAPTER, MORPHO);
    }

    /// Any state: symbolic storage, and an expectation armed by the Registry, the Adapter or no one (`armedBy` any
    /// other address). Transient storage starts empty in every transaction, so only those two can have armed it.
    function _anyState(address armedBy, uint256 armedMax) internal {
        vm.setArbitraryStorage(address(zzec));
        if (armedBy == REGISTRY || armedBy == ADAPTER) {
            vm.prank(armedBy);
            zzec.expectFromMorpho(armedMax);
        }
    }

    function _call(Call memory c) internal returns (bool ok) {
        return _call(c.op, c.caller, c.a, c.b, c.amount);
    }

    function _call(uint8 op, address caller, address a, address b, uint256 amount) internal returns (bool ok) {
        bytes memory data;
        if (op == 0) data = abi.encodeCall(IZZEC.mint, (a, amount));
        else if (op == 1) data = abi.encodeCall(IZZEC.burn, (a, amount));
        else if (op == 2) data = abi.encodeCall(IERC20.transfer, (a, amount));
        else if (op == 3) data = abi.encodeCall(IERC20.transferFrom, (a, b, amount));
        else if (op == 4) data = abi.encodeCall(IERC20.approve, (a, amount));
        else if (op == 5) data = abi.encodeCall(IZZEC.expectFromMorpho, (amount));
        else data = abi.encodeCall(IZZEC.settleExpectation, ());
        vm.prank(caller);
        (ok,) = address(zzec).call(data);
    }

    /// G1, G2, G7.
    function check_theSupplyChangesOnlyByTheRegistrysMintOrBurn(address armedBy, uint256 armedMax, Call memory c)
        public
    {
        _anyState(armedBy, armedMax);
        uint256 before = zzec.totalSupply();
        bool ok = _call(c);
        uint256 afterwards = zzec.totalSupply();
        if (!ok || c.op > 1) {
            assert(afterwards == before);
        } else if (c.op == 0) {
            assert(c.caller == REGISTRY && c.a == REGISTRY && afterwards == before + c.amount);
        } else {
            assert(c.caller == REGISTRY && (c.a == REGISTRY || c.a == ADAPTER) && afterwards == before - c.amount);
        }
    }

    /// G3.
    function check_theExpectationIsOnlyClearedOrArmedByItsRecipient(address armedBy, uint256 armedMax, Call memory c)
        public
    {
        _anyState(armedBy, armedMax);
        (address recipient0, uint256 max0) = zzec.pendingExpectation();
        bool ok = _call(c);
        (address recipient1, uint256 max1) = zzec.pendingExpectation();
        if (recipient1 != recipient0 || max1 != max0) {
            bool cleared = recipient1 == address(0) && max1 == 0;
            bool armedForItself = ok && c.op == 5 && (c.caller == REGISTRY || c.caller == ADAPTER)
                && recipient1 == c.caller && max1 == c.amount;
            assert(cleared || armedForItself);
        }
    }

    /// G4: a transfer whose source is not Morpho succeeds only as Morpho pulling the Registry's zZEC, whatever
    ///     allowance the source gave.
    function check_intoMorphoOnlyTheRegistrysZzecPulledByMorpho(
        address armedBy,
        uint256 armedMax,
        bool useTransferFrom,
        address caller,
        address from,
        address to,
        uint256 amount
    ) public {
        _anyState(armedBy, armedMax);
        address source = useTransferFrom ? from : caller;
        vm.assume(source != MORPHO);
        bool ok;
        if (useTransferFrom) {
            vm.prank(from);
            (bool approved,) = address(zzec).call(abi.encodeCall(IERC20.approve, (caller, type(uint256).max)));
            approved;
            ok = _call(3, caller, from, to, amount);
        } else {
            ok = _call(2, caller, to, address(0), amount);
        }
        if (ok) assert(source == REGISTRY && to == MORPHO && caller == MORPHO);
    }

    /// G5.
    function check_outOfMorphoOnlyToTheArmedRecipientOnce(
        address armedBy,
        uint256 armedMax,
        bool useTransferFrom,
        address caller,
        address to,
        uint256 amount,
        address settler,
        address to2,
        uint256 amount2
    ) public {
        _anyState(armedBy, armedMax);
        (address armed, uint256 max) = zzec.pendingExpectation();
        bool ok;
        if (useTransferFrom) {
            // Whatever allowance Morpho may have given (it gives none).
            vm.prank(MORPHO);
            zzec.approve(caller, type(uint256).max);
            ok = _call(3, caller, MORPHO, to, amount);
        } else {
            ok = _call(2, MORPHO, to, address(0), amount);
        }
        if (!ok) return;
        assert(armed != address(0) && to == armed && amount <= max);
        (address left, uint256 leftMax) = zzec.pendingExpectation();
        assert(left == address(0) && leftMax == 0);
        // Once: a second transfer out of Morpho, before a new expectation, always reverts.
        bool again = _call(2, MORPHO, to2, address(0), amount2);
        assert(!again);
        // Only the recipient settles, and it learns exactly what it received.
        vm.prank(settler);
        (bool settled, bytes memory ret) = address(zzec).call(abi.encodeCall(IZZEC.settleExpectation, ()));
        if (settled) assert(settler == to && abi.decode(ret, (uint256)) == amount);
        else assert(settler != to);
    }

    /// G6, as an induction step: from any state in which `other` holds nothing and the Registry, the Adapter and Morpho
    /// hold the whole supply, any call by anyone keeps both. Both hold at deployment, and G3 keeps the expectation's
    /// recipient the Registry or the Adapter, so they hold after every sequence of calls: no account ever holds zZEC
    /// (I2). Its hardest queries take minutes on a CI runner, more than the default solver timeout: none here.
    /// @custom:halmos --solver-timeout-assertion 0
    function check_noOtherAddressEverHoldsZzec(address armedBy, uint256 armedMax, Call memory c, address other) public {
        vm.assume(other != REGISTRY && other != ADAPTER && other != MORPHO);
        _anyState(armedBy, armedMax);
        vm.assume(zzec.balanceOf(other) == 0);
        vm.assume(_threeHoldTheSupply());
        _call(c);
        assert(zzec.balanceOf(other) == 0);
        assert(_threeHoldTheSupply());
    }

    /// Registry + Adapter + Morpho == totalSupply, without overflow.
    function _threeHoldTheSupply() internal view returns (bool) {
        uint256 supply = zzec.totalSupply();
        uint256 r = zzec.balanceOf(REGISTRY);
        uint256 a = zzec.balanceOf(ADAPTER);
        return r <= supply && a <= supply - r && zzec.balanceOf(MORPHO) == supply - r - a;
    }
}
