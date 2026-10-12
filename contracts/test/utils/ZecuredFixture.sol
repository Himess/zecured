// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {SettlementAdapter} from "../../src/SettlementAdapter.sol";
import {DepositAttestation, CloseRequest, SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {VaultStatus, ClaimKind, ClaimStatus, DepositProofConfig} from "../../src/interfaces/IZecuredRegistry.sol";

/// @notice Shared fixture of the fork tests: a fork of Base Sepolia with the REAL Morpho Blue and AdaptiveCurveIRM, plus
///         freshly deployed MockUSDC + MockZecOracle (MOCK) and the production ZZEC / ZecuredRegistry /
///         SettlementAdapter, wired exactly like script/DeployZecured.s.sol does it (predicted CREATE addresses).
/// @dev RPC: $BASE_SEPOLIA_RPC_URL, or https://sepolia.base.org when unset. Block: $ZECURED_FORK_BLOCK, default
///      47,948,000 (pinned so that Foundry caches the fork state on disk); 0 = latest.
abstract contract ZecuredFixture is Test {
    using MarketParamsLib for MarketParams;
    using MorphoBalancesLib for IMorpho;

    // ------------------------------------------------------------------ Base Sepolia (docs/ref/FACTS.md)

    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    /// @dev Tier 3 bonds disabled (the Tier 3 tests deploy a SignerBond of their own).
    ISignerBond internal constant NO_BOND = ISignerBond(address(0));
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

    // ------------------------------------------------------------------ system

    MockUSDC internal usdc;
    MockZecOracle internal oracle;
    ZZEC internal zzec;
    ZecuredRegistry internal registry;
    SettlementAdapter internal adapter;
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

    /// Tier 2 configuration of the Registry under test: deposit proofs disabled unless a suite overrides this (it may
    /// deploy contracts; it runs before the ZZEC / Registry / Adapter addresses are predicted).
    function _depositProofConfig() internal virtual returns (DepositProofConfig memory) {}

    /// The Registry's bond contract: none by default; Tier 3 tests override it.
    function _signerBond() internal virtual returns (ISignerBond) {
        return NO_BOND;
    }

    /// Deposit proofs disabled (all zero).
    function _noProofs() internal pure returns (DepositProofConfig memory none) {}

    /// Same ordering as script/DeployZecured.s.sol: ZZEC, Registry, Adapter at the deployer's next three nonces.
    function _deploySystem() internal {
        usdc = new MockUSDC(address(this));
        oracle = new MockZecOracle(address(this), PRICE_40);
        DepositProofConfig memory proofs = _depositProofConfig();
        ISignerBond bond = _signerBond();

        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        address predictedAdapter = vm.computeCreateAddress(address(this), nonce + 2);
        zzec = new ZZEC(predictedRegistry, predictedAdapter, address(MORPHO));
        registry = new ZecuredRegistry(
            MORPHO,
            zzec,
            predictedAdapter,
            address(usdc),
            address(oracle),
            ADAPTIVE_CURVE_IRM,
            LLTV,
            signerSet,
            proofs,
            bond
        );
        adapter = new SettlementAdapter(registry);
        assertEq(address(registry), predictedRegistry, "registry address prediction");
        assertEq(address(adapter), predictedAdapter, "adapter address prediction");

        params = registry.marketParams();
        marketId = params.id();

        vm.label(address(MORPHO), "MorphoBlue");
        vm.label(ADAPTIVE_CURVE_IRM, "AdaptiveCurveIRM");
        vm.label(address(usdc), "MockUSDC");
        vm.label(address(oracle), "MockZecOracle");
        vm.label(address(zzec), "zZEC");
        vm.label(address(registry), "ZecuredRegistry");
        vm.label(address(adapter), "SettlementAdapter");
        for (uint256 i; i < 5; ++i) {
            vm.label(signerSet[i], string.concat("signer", vm.toString(i)));
        }
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

    function _attestation(address account, uint64 amount) internal pure returns (DepositAttestation memory) {
        return DepositAttestation({
            vaultId: _vaultIdFor(account), account: account, amount: amount, depositTag: _depositTagFor(account)
        });
    }

    function _attest(address account, uint64 amount) internal returns (bytes32 vaultId) {
        DepositAttestation memory att = _attestation(account, amount);
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att)));
        return att.vaultId;
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
        for (uint256 i; i < claimIds.length; ++i) {
            (,, uint64 amount,,) = registry.claim(claimIds[i]);
            claimed += amount;
            refs[i] = keccak256(abi.encode("zecured.test.claimRef", salt, i));
        }
        uint64 change = registry.lineageOf(inputTag).value - claimed;
        c = SpendCommit({
            vaultId: vaultId,
            inputTag: inputTag,
            claimIds: claimIds,
            claimRefs: refs,
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
