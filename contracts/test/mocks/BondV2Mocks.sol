// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ClaimDetails, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {IZecuredPriceFeed} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";

/// @notice MOCK (tests only): when armed, calls `target` with `data` from inside a hook and records the outcome, so that
///         a test can show that the bond refuses to be re-entered from that point.
abstract contract MockReentryProbe {
    address public probeTarget;
    bytes public probeData;
    bool public probeTried;
    bool public probeSucceeded;
    bytes public probeRevertData;

    /// Arms (or, with a zero target, disarms) the probe and forgets the last outcome.
    function armProbe(address target, bytes calldata data) external {
        probeTarget = target;
        probeData = data;
        probeTried = false;
        probeSucceeded = false;
        delete probeRevertData;
    }

    function _probe() internal {
        if (probeTarget == address(0)) return;
        probeTried = true;
        (probeSucceeded, probeRevertData) = probeTarget.call(probeData);
    }
}

/// @notice MOCK (tests only): what SignerBondV2 reads and calls on ZecuredRegistry v3 (`signers`, `claimDetails`,
///         `markBondPaid`), on its IZecuredSpendCommits part (`openVaultCount`, `lastVaultSettledAt`, `claimInput`,
///         `lastCommitAt`) and on its dispute path (`disputeWindow`, `inclusionWindow`, read by the constructor, R-18).
///         The test sets every value. `markBondPaid` does what the real hook does (the bond only, a
///         pending claim not yet paid from the bonds: `bondPaid = true`, commitment zero), and records the bond's state
///         at that moment.
contract MockBondRegistry is MockReentryProbe {
    address[5] internal _signers;
    mapping(bytes32 claimId => ClaimDetails) internal _claims;

    uint256 public openVaultCount;
    uint64 public lastVaultSettledAt;
    uint64 public disputeWindow = 1 days;
    uint64 public inclusionWindow = 6 hours;
    mapping(bytes32 claimId => bytes32) public claimInput;
    mapping(bytes32 inputTag => uint40) public lastCommitAt;

    address public signerBond;
    IERC20 public usdc;
    uint256 public markBondPaidCalls;
    bytes32 public lastMarked;
    /// The bond's `totalSlashable` and the claimant's balance when `markBondPaid` ran.
    uint256 public slashableAtMark;
    uint256 public claimantBalanceAtMark;

    constructor(address[5] memory signers_) {
        _signers = signers_;
    }

    function setSignerBond(address bond, IERC20 token) external {
        signerBond = bond;
        usdc = token;
    }

    function setSigners(address[5] memory signers_) external {
        _signers = signers_;
    }

    function setDisputeWindows(uint64 dispute, uint64 inclusion) external {
        disputeWindow = dispute;
        inclusionWindow = inclusion;
    }

    function setOpenVaultCount(uint256 count) external {
        openVaultCount = count;
    }

    function setLastVaultSettledAt(uint64 at) external {
        lastVaultSettledAt = at;
    }

    function setClaim(bytes32 claimId, ClaimDetails memory c) external {
        _claims[claimId] = c;
    }

    function setClaimInput(bytes32 claimId, bytes32 inputTag) external {
        claimInput[claimId] = inputTag;
    }

    function setLastCommitAt(bytes32 inputTag, uint40 at) external {
        lastCommitAt[inputTag] = at;
    }

    function signers() external view returns (address[5] memory) {
        return _signers;
    }

    function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory) {
        return _claims[claimId];
    }

    function markBondPaid(bytes32 claimId) external {
        require(msg.sender == signerBond, "MOCK registry: only the bond");
        ClaimDetails storage c = _claims[claimId];
        require(c.status == ClaimStatus.Pending && !c.bondPaid, "MOCK registry: not payable");
        c.bondPaid = true;
        c.commitment = bytes32(0);
        markBondPaidCalls += 1;
        lastMarked = claimId;
        slashableAtMark = ISignerBond(signerBond).totalSlashable();
        claimantBalanceAtMark = usdc.balanceOf(c.claimant);
        _probe();
    }
}

/// @notice MOCK (tests only): the slasher's views the bond reads. Tests call `slash` as this address (`vm.prank`).
contract MockBondSlasher {
    uint256 public openCaseCount;
    mapping(bytes32 vaultId => bool) public vaultFrozen;

    function setOpenCaseCount(uint256 count) external {
        openCaseCount = count;
    }

    function setVaultFrozen(bytes32 vaultId, bool frozen) external {
        vaultFrozen[vaultId] = frozen;
    }
}

/// @notice MOCK (tests only): the Intake's coverage check as a threshold: it holds with an effective bond of at least
///         `requiredBond`. Moving the threshold by one unit shows the exact value the bond asks about.
contract MockBondIntake {
    uint256 public requiredBond;

    function setRequiredBond(uint256 bond) external {
        requiredBond = bond;
    }

    function coverageHoldsWith(uint256 effectiveBond) external view returns (bool) {
        return effectiveBond >= requiredBond;
    }
}

/// @notice MOCK (tests only): records what the bond hands over (`onFunds`), with its own USDC balance and the bond's
///         state at that moment; answers `entitlement` with what the test set (R-7).
contract MockBondCompensator is MockReentryProbe {
    IERC20 public immutable usdc;
    address public signerBond;
    mapping(bytes32 claimId => Entitlement) internal _entitlements;
    uint256 public onFundsCalls;
    bytes32 public lastCaseId;
    uint256 public lastAmount;
    uint256 public balanceAtCall;
    bool public haltedAtCall;
    uint256 public slashableAtCall;

    constructor(IERC20 usdc_) {
        usdc = usdc_;
    }

    function setSignerBond(address bond) external {
        signerBond = bond;
    }

    function setEntitlement(bytes32 claimId, Entitlement memory e) external {
        _entitlements[claimId] = e;
    }

    function entitlement(bytes32 claimId) external view returns (Entitlement memory) {
        return _entitlements[claimId];
    }

    function onFunds(bytes32 caseId, uint256 amount) external {
        require(msg.sender == signerBond, "MOCK compensator: only the bond");
        onFundsCalls += 1;
        lastCaseId = caseId;
        lastAmount = amount;
        balanceAtCall = usdc.balanceOf(address(this));
        haltedAtCall = ISignerBond(signerBond).halted();
        slashableAtCall = ISignerBond(signerBond).totalSlashable();
        _probe();
    }
}

/// @notice MOCK (tests only): an oracle whose Morpho-scaled price the test sets, zero included (`MockZecOracle` refuses
///         zero), and that reports a feed state like `ZecuredPriceFeed` (R-9; `BothFresh` unless the test says
///         otherwise).
contract MockBondOracle {
    uint256 public price;
    IZecuredPriceFeed.FeedState public state;

    constructor(uint256 price_) {
        price = price_;
    }

    function setPrice(uint256 price_) external {
        price = price_;
    }

    function setState(IZecuredPriceFeed.FeedState state_) external {
        state = state_;
    }
}

/// @notice MOCK (tests only): an oracle with a price and no feed state (the testnets' demo oracles).
contract MockBondPlainOracle {
    uint256 public price;

    constructor(uint256 price_) {
        price = price_;
    }
}

/// @notice MOCK (tests only): a 6-decimal ERC-20, mintable by anyone, that calls a registered recipient back on every
///         transfer (ERC-777 style), to test the bond against reentrancy through its token. Not USDC: USDC has no hooks.
contract MockHookToken is ERC20 {
    mapping(address account => bool) public hooked;

    constructor() ERC20("MOCK hook token", "mHOOK") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function setHooked(address account, bool on) external {
        hooked[account] = on;
    }

    function _update(address from, address to, uint256 value) internal override {
        super._update(from, to, value);
        if (hooked[to]) MockReentrantAccount(to).onTokenReceived();
    }
}

/// @notice MOCK (tests only): a contract account (a signer, a claimant or a bounty beneficiary) that runs whatever the
///         test tells it to, and probes the bond when the token calls it back.
contract MockReentrantAccount is MockReentryProbe {
    function exec(address target, bytes calldata data) external returns (bytes memory) {
        (bool ok, bytes memory ret) = target.call(data);
        if (!ok) {
            assembly ("memory-safe") {
                revert(add(ret, 0x20), mload(ret))
            }
        }
        return ret;
    }

    function onTokenReceived() external {
        _probe();
    }
}
