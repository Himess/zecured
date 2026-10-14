// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test, console} from "forge-std/Test.sol";
import {
    ZecuredEIP712,
    DepositAttestation,
    CloseRequest,
    SpendCommit,
    DeleverageRequest
} from "../../src/libraries/ZecuredEIP712.sol";

/// @notice The EIP-712 type strings of docs/INTERFACES.md § 2 and the struct hashes / digests the contracts compute,
///         cross-checked against Foundry's independent EIP-712 implementation (`vm.eip712HashType`,
///         `vm.eip712HashStruct`, `vm.eip712HashTypedData`, built on alloy) from the same JSON a wallet signs.
///         The golden digests at the bottom are test vectors for the Rust signer and the TypeScript app.
contract ZecuredEIP712Test is Test {
    // ------------------------------------------------------------------ exact type strings (docs/INTERFACES.md § 2)

    string internal constant DEPOSIT_ATTESTATION =
        "DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)";
    string internal constant PAYOUT_CONFIRMATION = "PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)";
    string internal constant CLOSE_REQUEST =
        "CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)";
    string internal constant OPEN_VAULT =
        "OpenVault(address owner,address account,uint64 amount,bytes32 requestId,uint64 deadline)";
    string internal constant DESTINATION =
        "Destination(bytes32 vaultId,bytes32 commitment,uint64 nonce,uint64 deadline)";
    string internal constant REFUND_REQUEST =
        "RefundRequest(bytes32 vaultId,bytes32 noteId,bytes32 commitment,uint64 nonce,uint64 deadline)";
    string internal constant VIEW_KEY_REQUEST = "ViewKeyRequest(bytes32 vaultId,uint64 nonce,uint64 deadline)";
    string internal constant DELEVERAGE_REQUEST =
        "DeleverageRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint256 repayAssets,uint64 maxCollateral,uint256 nonce,uint64 deadline)";
    string internal constant SPEND_COMMIT =
        "SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)";

    // ------------------------------------------------------------------ fixed vector inputs (Base Sepolia)

    uint256 internal constant CHAIN_ID = 84532;
    address internal constant REGISTRY = 0x1111111111111111111111111111111111111111;
    address internal constant ADAPTER = 0x2222222222222222222222222222222222222222;
    address internal constant ACCOUNT = 0x3333333333333333333333333333333333333333;
    address internal constant EXECUTOR = 0x4444444444444444444444444444444444444444;
    bytes32 internal constant VAULT_ID = 0x5555555555555555555555555555555555555555555555555555555555555555;
    bytes32 internal constant DEPOSIT_TAG = 0x6666666666666666666666666666666666666666666666666666666666666666;
    bytes32 internal constant USER_COMMITMENT = 0x7777777777777777777777777777777777777777777777777777777777777777;
    uint64 internal constant AMOUNT = 1_000_000; // 0.01 ZEC
    uint16 internal constant MARGIN_BPS = 100;
    uint64 internal constant MIN_USER_AMOUNT = 500_000;
    uint256 internal constant NONCE = 42;
    uint64 internal constant DEADLINE = 1_800_000_000;
    // SpendCommit (Tier 3): a close pair paid from the deposit, with change.
    bytes32 internal constant CLAIM_REF_0 = 0x8888888888888888888888888888888888888888888888888888888888888888;
    bytes32 internal constant CLAIM_REF_1 = 0x9999999999999999999999999999999999999999999999999999999999999999;
    bytes32 internal constant PAYOUT_REF = 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa;
    bytes32 internal constant CHANGE_TAG = 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb;
    uint64 internal constant CHANGE_VALUE = 3_800_000;

    bytes32 internal constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    // ------------------------------------------------------------------ helpers

    function _domainSeparator(string memory name, address verifyingContract) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(DOMAIN_TYPEHASH, keccak256(bytes(name)), keccak256(bytes("1")), CHAIN_ID, verifyingContract)
        );
    }

    function _digest(bytes32 domainSeparator, bytes32 structHash) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(hex"1901", domainSeparator, structHash));
    }

    function _attestation() internal pure returns (DepositAttestation memory) {
        return DepositAttestation({vaultId: VAULT_ID, account: ACCOUNT, amount: AMOUNT, depositTag: DEPOSIT_TAG});
    }

    function _closeRequest() internal pure returns (CloseRequest memory) {
        return CloseRequest({
            vaultId: VAULT_ID,
            account: ACCOUNT,
            executor: EXECUTOR,
            marginBps: MARGIN_BPS,
            minUserAmount: MIN_USER_AMOUNT,
            userCommitment: USER_COMMITMENT,
            nonce: NONCE,
            deadline: DEADLINE
        });
    }

    function _deleverageRequest() internal pure returns (DeleverageRequest memory) {
        return DeleverageRequest({
            vaultId: VAULT_ID,
            account: ACCOUNT,
            executor: EXECUTOR,
            marginBps: MARGIN_BPS,
            repayAssets: 500_000,
            maxCollateral: 1_262_500,
            nonce: NONCE,
            deadline: DEADLINE
        });
    }

    function _spendCommit() internal pure returns (SpendCommit memory c) {
        bytes32[] memory refs = new bytes32[](2);
        refs[0] = CLAIM_REF_0;
        refs[1] = CLAIM_REF_1;
        uint8[] memory outputs = new uint8[](2);
        (outputs[0], outputs[1]) = (2, 0); // the borrower's output is action 2, the market maker's action 0
        c = SpendCommit({
            vaultId: VAULT_ID,
            inputTag: DEPOSIT_TAG,
            claimIds: _claimIds(),
            claimRefs: refs,
            claimOutputs: outputs,
            payoutRef: PAYOUT_REF,
            changeTag: CHANGE_TAG,
            changeValue: CHANGE_VALUE
        });
    }

    function _claimIds() internal pure returns (bytes32[] memory ids) {
        ids = new bytes32[](2);
        ids[0] = keccak256(abi.encode(VAULT_ID, uint64(0)));
        ids[1] = keccak256(abi.encode(VAULT_ID, uint64(1)));
    }

    function _domainJson(string memory name, address verifyingContract) internal pure returns (string memory) {
        return string.concat(
            '"domain":{"name":"',
            name,
            '","version":"1","chainId":',
            vm.toString(CHAIN_ID),
            ',"verifyingContract":"',
            vm.toString(verifyingContract),
            '"}'
        );
    }

    string internal constant DOMAIN_TYPES_JSON =
        '"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}]';

    // ------------------------------------------------------------------ type hashes

    function test_typeHashesMatchInterfacesMd() public pure {
        assertEq(ZecuredEIP712.DEPOSIT_ATTESTATION_TYPEHASH, keccak256(bytes(DEPOSIT_ATTESTATION)));
        assertEq(ZecuredEIP712.PAYOUT_CONFIRMATION_TYPEHASH, keccak256(bytes(PAYOUT_CONFIRMATION)));
        assertEq(ZecuredEIP712.CLOSE_REQUEST_TYPEHASH, keccak256(bytes(CLOSE_REQUEST)));
        assertEq(ZecuredEIP712.OPEN_VAULT_TYPEHASH, keccak256(bytes(OPEN_VAULT)));
        assertEq(ZecuredEIP712.DESTINATION_TYPEHASH, keccak256(bytes(DESTINATION)));
        assertEq(ZecuredEIP712.REFUND_REQUEST_TYPEHASH, keccak256(bytes(REFUND_REQUEST)));
        assertEq(ZecuredEIP712.VIEW_KEY_REQUEST_TYPEHASH, keccak256(bytes(VIEW_KEY_REQUEST)));
        assertEq(ZecuredEIP712.SPEND_COMMIT_TYPEHASH, keccak256(bytes(SPEND_COMMIT)));
        assertEq(ZecuredEIP712.DELEVERAGE_REQUEST_TYPEHASH, keccak256(bytes(DELEVERAGE_REQUEST)));
    }

    function test_typeHashesMatchIndependentImplementation() public pure {
        assertEq(ZecuredEIP712.DEPOSIT_ATTESTATION_TYPEHASH, vm.eip712HashType(DEPOSIT_ATTESTATION));
        assertEq(ZecuredEIP712.PAYOUT_CONFIRMATION_TYPEHASH, vm.eip712HashType(PAYOUT_CONFIRMATION));
        assertEq(ZecuredEIP712.CLOSE_REQUEST_TYPEHASH, vm.eip712HashType(CLOSE_REQUEST));
        assertEq(ZecuredEIP712.OPEN_VAULT_TYPEHASH, vm.eip712HashType(OPEN_VAULT));
        assertEq(ZecuredEIP712.DESTINATION_TYPEHASH, vm.eip712HashType(DESTINATION));
        assertEq(ZecuredEIP712.REFUND_REQUEST_TYPEHASH, vm.eip712HashType(REFUND_REQUEST));
        assertEq(ZecuredEIP712.VIEW_KEY_REQUEST_TYPEHASH, vm.eip712HashType(VIEW_KEY_REQUEST));
        assertEq(ZecuredEIP712.SPEND_COMMIT_TYPEHASH, vm.eip712HashType(SPEND_COMMIT));
        assertEq(ZecuredEIP712.DELEVERAGE_REQUEST_TYPEHASH, vm.eip712HashType(DELEVERAGE_REQUEST));
    }

    // ------------------------------------------------------------------ struct hashes

    function test_structHashesMatchIndependentImplementation() public pure {
        DepositAttestation memory att = _attestation();
        assertEq(
            ZecuredEIP712.hashDepositAttestation(att),
            vm.eip712HashStruct(DEPOSIT_ATTESTATION, abi.encode(att.vaultId, att.account, att.amount, att.depositTag))
        );

        bytes32[] memory ids = _claimIds();
        assertEq(ZecuredEIP712.claimIdsHash(ids), keccak256(abi.encodePacked(ids[0], ids[1])));
        assertEq(
            ZecuredEIP712.hashPayoutConfirmation(VAULT_ID, ids),
            vm.eip712HashStruct(PAYOUT_CONFIRMATION, abi.encode(VAULT_ID, ZecuredEIP712.claimIdsHash(ids)))
        );

        CloseRequest memory req = _closeRequest();
        assertEq(
            ZecuredEIP712.hashCloseRequest(req),
            vm.eip712HashStruct(
                CLOSE_REQUEST,
                abi.encode(
                    req.vaultId,
                    req.account,
                    req.executor,
                    req.marginBps,
                    req.minUserAmount,
                    req.userCommitment,
                    req.nonce,
                    req.deadline
                )
            )
        );

        SpendCommit memory c = _spendCommit();
        // A struct with dynamic members is ABI-encoded as one dynamic tuple (offset first).
        assertEq(ZecuredEIP712.hashSpendCommit(c), vm.eip712HashStruct(SPEND_COMMIT, abi.encode(c)));
    }

    /// The claim id list is ordered: swapping two ids changes the hash (signers confirm exactly one ordering).
    function test_claimIdsHashIsOrderSensitive() public pure {
        bytes32[] memory ids = _claimIds();
        bytes32[] memory swapped = new bytes32[](2);
        swapped[0] = ids[1];
        swapped[1] = ids[0];
        assertTrue(ZecuredEIP712.claimIdsHash(ids) != ZecuredEIP712.claimIdsHash(swapped));
        assertEq(ZecuredEIP712.claimIdsHash(new bytes32[](0)), keccak256(""));
    }

    // ------------------------------------------------------------------ full digests from wallet JSON

    function test_depositAttestationDigest_matchesTypedDataJson() public pure {
        string memory json = string.concat(
            '{"types":{',
            DOMAIN_TYPES_JSON,
            ',"DepositAttestation":[{"name":"vaultId","type":"bytes32"},{"name":"account","type":"address"},{"name":"amount","type":"uint64"},{"name":"depositTag","type":"bytes32"}]},',
            '"primaryType":"DepositAttestation",',
            _domainJson("ZecuredRegistry", REGISTRY),
            ',"message":{"vaultId":"',
            vm.toString(VAULT_ID),
            '","account":"',
            vm.toString(ACCOUNT),
            '","amount":"',
            vm.toString(AMOUNT),
            '","depositTag":"',
            vm.toString(DEPOSIT_TAG),
            '"}}'
        );
        bytes32 ours = _digest(
            _domainSeparator(ZecuredEIP712.REGISTRY_DOMAIN_NAME, REGISTRY),
            ZecuredEIP712.hashDepositAttestation(_attestation())
        );
        assertEq(ours, vm.eip712HashTypedData(json));
        assertEq(ours, GOLDEN_DEPOSIT_ATTESTATION_DIGEST, "golden vector");
    }

    function test_payoutConfirmationDigest_matchesTypedDataJson() public pure {
        bytes32[] memory ids = _claimIds();
        string memory json = string.concat(
            '{"types":{',
            DOMAIN_TYPES_JSON,
            ',"PayoutConfirmation":[{"name":"vaultId","type":"bytes32"},{"name":"claimIdsHash","type":"bytes32"}]},',
            '"primaryType":"PayoutConfirmation",',
            _domainJson("ZecuredRegistry", REGISTRY),
            ',"message":{"vaultId":"',
            vm.toString(VAULT_ID),
            '","claimIdsHash":"',
            vm.toString(keccak256(abi.encodePacked(ids))),
            '"}}'
        );
        bytes32 ours = _digest(
            _domainSeparator(ZecuredEIP712.REGISTRY_DOMAIN_NAME, REGISTRY),
            ZecuredEIP712.hashPayoutConfirmation(VAULT_ID, ids)
        );
        assertEq(ours, vm.eip712HashTypedData(json));
        assertEq(ours, GOLDEN_PAYOUT_CONFIRMATION_DIGEST, "golden vector");
    }

    function test_closeRequestDigest_matchesTypedDataJson() public pure {
        string memory json = string.concat(
            '{"types":{',
            DOMAIN_TYPES_JSON,
            ',"CloseRequest":[{"name":"vaultId","type":"bytes32"},{"name":"account","type":"address"},{"name":"executor","type":"address"},{"name":"marginBps","type":"uint16"},{"name":"minUserAmount","type":"uint64"},{"name":"userCommitment","type":"bytes32"},{"name":"nonce","type":"uint256"},{"name":"deadline","type":"uint64"}]},',
            '"primaryType":"CloseRequest",',
            _domainJson("ZecuredSettlementAdapter", ADAPTER),
            _closeRequestMessageJson()
        );
        bytes32 ours = _digest(
            _domainSeparator(ZecuredEIP712.ADAPTER_DOMAIN_NAME, ADAPTER),
            ZecuredEIP712.hashCloseRequest(_closeRequest())
        );
        assertEq(ours, vm.eip712HashTypedData(json));
        assertEq(ours, GOLDEN_CLOSE_REQUEST_DIGEST, "golden vector");
    }

    function _closeRequestMessageJson() internal pure returns (string memory) {
        return string.concat(
            ',"message":{"vaultId":"',
            vm.toString(VAULT_ID),
            '","account":"',
            vm.toString(ACCOUNT),
            '","executor":"',
            vm.toString(EXECUTOR),
            '","marginBps":"',
            vm.toString(MARGIN_BPS),
            '","minUserAmount":"',
            vm.toString(MIN_USER_AMOUNT),
            '","userCommitment":"',
            vm.toString(USER_COMMITMENT),
            '","nonce":"',
            vm.toString(NONCE),
            '","deadline":"',
            vm.toString(DEADLINE),
            '"}}'
        );
    }

    /// Every field is covered by the hash: changing any one of them changes the digest.
    function test_closeRequestHash_coversEveryField() public pure {
        bytes32 base = ZecuredEIP712.hashCloseRequest(_closeRequest());
        CloseRequest memory r;
        r = _closeRequest();
        r.vaultId = bytes32(uint256(r.vaultId) ^ 1);
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.account = address(uint160(r.account) ^ 1);
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.executor = address(uint160(r.executor) ^ 1);
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.marginBps ^= 1;
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.minUserAmount ^= 1;
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.userCommitment = bytes32(uint256(r.userCommitment) ^ 1);
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.nonce ^= 1;
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
        r = _closeRequest();
        r.deadline ^= 1;
        assertTrue(ZecuredEIP712.hashCloseRequest(r) != base);
    }

    function test_spendCommitDigest_matchesTypedDataJson() public pure {
        SpendCommit memory c = _spendCommit();
        string memory json = string.concat(
            '{"types":{',
            DOMAIN_TYPES_JSON,
            ',"SpendCommit":[{"name":"vaultId","type":"bytes32"},{"name":"inputTag","type":"bytes32"},{"name":"claimIds","type":"bytes32[]"},{"name":"claimRefs","type":"bytes32[]"},{"name":"claimOutputs","type":"uint8[]"},{"name":"payoutRef","type":"bytes32"},{"name":"changeTag","type":"bytes32"},{"name":"changeValue","type":"uint64"}]},',
            '"primaryType":"SpendCommit",',
            _domainJson("ZecuredRegistry", REGISTRY),
            _spendCommitMessageJson(c)
        );
        bytes32 ours =
            _digest(_domainSeparator(ZecuredEIP712.REGISTRY_DOMAIN_NAME, REGISTRY), ZecuredEIP712.hashSpendCommit(c));
        assertEq(ours, vm.eip712HashTypedData(json));
        assertEq(ours, GOLDEN_SPEND_COMMIT_DIGEST, "golden vector");
    }

    function _spendCommitMessageJson(SpendCommit memory c) internal pure returns (string memory) {
        return string.concat(
            ',"message":{"vaultId":"',
            vm.toString(c.vaultId),
            '","inputTag":"',
            vm.toString(c.inputTag),
            '","claimIds":["',
            vm.toString(c.claimIds[0]),
            '","',
            vm.toString(c.claimIds[1]),
            '"],"claimRefs":["',
            vm.toString(c.claimRefs[0]),
            '","',
            vm.toString(c.claimRefs[1]),
            '"],"claimOutputs":["2","0"],"payoutRef":"',
            vm.toString(c.payoutRef),
            '","changeTag":"',
            vm.toString(c.changeTag),
            '","changeValue":"',
            vm.toString(c.changeValue),
            '"}}'
        );
    }

    /// Every field is covered by the hash, the order of the claims too, and the two arrays cannot be swapped.
    function test_spendCommitHash_coversEveryField() public pure {
        bytes32 base = ZecuredEIP712.hashSpendCommit(_spendCommit());
        SpendCommit memory c;
        c = _spendCommit();
        c.vaultId = bytes32(uint256(c.vaultId) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        c.inputTag = bytes32(uint256(c.inputTag) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        c.claimIds[1] = bytes32(uint256(c.claimIds[1]) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        (c.claimIds[0], c.claimIds[1]) = (c.claimIds[1], c.claimIds[0]);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base, "claim order");
        c = _spendCommit();
        c.claimRefs[0] = bytes32(uint256(c.claimRefs[0]) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        c.claimOutputs[1] = 1;
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base, "an output index");
        c = _spendCommit();
        (c.claimIds, c.claimRefs) = (c.claimRefs, c.claimIds);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base, "arrays swapped");
        c = _spendCommit();
        c.claimRefs = new bytes32[](1);
        c.claimRefs[0] = CLAIM_REF_0;
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base, "a dropped claim ref");
        c = _spendCommit();
        c.payoutRef = bytes32(uint256(c.payoutRef) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        c.changeTag = bytes32(uint256(c.changeTag) ^ 1);
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
        c = _spendCommit();
        c.changeValue ^= 1;
        assertTrue(ZecuredEIP712.hashSpendCommit(c) != base);
    }

    function test_deleverageRequestDigest_matchesTypedDataJson() public pure {
        DeleverageRequest memory req = _deleverageRequest();
        assertEq(
            ZecuredEIP712.hashDeleverageRequest(req),
            vm.eip712HashStruct(
                DELEVERAGE_REQUEST,
                abi.encode(
                    req.vaultId,
                    req.account,
                    req.executor,
                    req.marginBps,
                    req.repayAssets,
                    req.maxCollateral,
                    req.nonce,
                    req.deadline
                )
            )
        );
        string memory json = string.concat(
            '{"types":{',
            DOMAIN_TYPES_JSON,
            ',"DeleverageRequest":[{"name":"vaultId","type":"bytes32"},{"name":"account","type":"address"},{"name":"executor","type":"address"},{"name":"marginBps","type":"uint16"},{"name":"repayAssets","type":"uint256"},{"name":"maxCollateral","type":"uint64"},{"name":"nonce","type":"uint256"},{"name":"deadline","type":"uint64"}]},',
            '"primaryType":"DeleverageRequest",',
            _domainJson("ZecuredSettlementAdapter", ADAPTER),
            string.concat(
                ',"message":{"vaultId":"',
                vm.toString(VAULT_ID),
                '","account":"',
                vm.toString(ACCOUNT),
                '","executor":"',
                vm.toString(EXECUTOR),
                '","marginBps":"100","repayAssets":"500000","maxCollateral":"1262500","nonce":"42","deadline":"',
                vm.toString(DEADLINE),
                '"}}'
            )
        );
        bytes32 ours = _digest(
            _domainSeparator(ZecuredEIP712.ADAPTER_DOMAIN_NAME, ADAPTER), ZecuredEIP712.hashDeleverageRequest(req)
        );
        assertEq(ours, vm.eip712HashTypedData(json));
        assertEq(ours, GOLDEN_DELEVERAGE_REQUEST_DIGEST, "golden vector");
    }

    /// Prints the vectors (run with -vv) so other implementations can copy them.
    function test_printVectors() public pure {
        console.log("chainId 84532, Registry", REGISTRY, "Adapter", ADAPTER);
        console.log("Registry domain separator");
        console.logBytes32(_domainSeparator(ZecuredEIP712.REGISTRY_DOMAIN_NAME, REGISTRY));
        console.log("Adapter domain separator");
        console.logBytes32(_domainSeparator(ZecuredEIP712.ADAPTER_DOMAIN_NAME, ADAPTER));
        console.log("DepositAttestation digest");
        console.logBytes32(GOLDEN_DEPOSIT_ATTESTATION_DIGEST);
        console.log("PayoutConfirmation digest (claimIds = [claimIdOf(vaultId, 0), claimIdOf(vaultId, 1)])");
        console.logBytes32(GOLDEN_PAYOUT_CONFIRMATION_DIGEST);
        console.log("CloseRequest digest");
        console.logBytes32(GOLDEN_CLOSE_REQUEST_DIGEST);
        console.log("DeleverageRequest digest");
        console.logBytes32(GOLDEN_DELEVERAGE_REQUEST_DIGEST);
        console.log("SpendCommit struct hash and digest");
        console.logBytes32(ZecuredEIP712.hashSpendCommit(_spendCommit()));
        console.logBytes32(GOLDEN_SPEND_COMMIT_DIGEST);
    }

    // ------------------------------------------------------------------ golden vectors
    // Inputs: the constants above (chainId 84532; Registry 0x1111..11; Adapter 0x2222..22; account 0x3333..33;
    // executor 0x4444..44; vaultId 0x5555..55; depositTag 0x6666..66; userCommitment 0x7777..77; amount 1,000,000;
    // marginBps 100; minUserAmount 500,000; nonce 42; deadline 1,800,000,000).
    // Each value was produced by this library AND by Foundry's independent EIP-712 implementation; the
    // DepositAttestation digest was also recomputed with `cast keccak` / `cast abi-encode`.
    // Registry domain separator: 0x489271068185e1811817d2dfae78e2be898ee5fc9768bf01d870981f667ef9f8
    // Adapter domain separator:  0xb62551e236af33b3b84da5fe38134bcea7c80b912f8b172bb4020080457a9260

    bytes32 internal constant GOLDEN_DEPOSIT_ATTESTATION_DIGEST =
        0xbe0251d9c909749194da8ebf137e42a33a60d6fba8672a022c4923558fecb90f;
    bytes32 internal constant GOLDEN_PAYOUT_CONFIRMATION_DIGEST =
        0xf166a0191609bbccd2b7aa9e7527b1553a418f7626902cc10099034b838e4973;
    bytes32 internal constant GOLDEN_CLOSE_REQUEST_DIGEST =
        0x2207e7a8cd1b3966d87015d4b554f9271a760f2854c1c9d37ae0391b885ae8f7;
    // DeleverageRequest: marginBps 100, repayAssets 500,000, maxCollateral 1,262,500, nonce 42, deadline as above.
    bytes32 internal constant GOLDEN_DELEVERAGE_REQUEST_DIGEST =
        0x54c59f01064cf8a39e3c1036566f3e83c7cd9c5b21d3256c70f4adc4f1297211;
    // SpendCommit: inputTag 0x6666..66, claimIds as PayoutConfirmation's, claimRefs [0x8888..88, 0x9999..99],
    // claimOutputs [2, 0], payoutRef 0xaaaa..aa, changeTag 0xbbbb..bb, changeValue 3,800,000.
    bytes32 internal constant GOLDEN_SPEND_COMMIT_DIGEST =
        0x3dd9f2e94c96d5aa0a5c016684697f2283f0447f4d0eb81739052c0a07217d7a;
}
