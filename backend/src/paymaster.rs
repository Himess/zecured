//! RQ-05: our own paymaster. Pimlico's SingletonPaymasterV7 in verifying mode, deployed with
//! our own signer by contracts/script/DeployPaymaster.s.sol, answers for any UserOp carrying a
//! valid signature of that signer. The backend gives that signature only to UserOps that passed
//! the proxy's checks (paymaster allowlist, EntryPoint v0.7, a Zecured account, rate limits):
//! it answers ERC-7677's two methods itself instead of forwarding them to a hosted paymaster,
//! so no third party sees a UserOp or decides what is sponsored.
//!
//! `paymasterData` (verifying mode) = `uint8 (mode 0 << 1 | allowAllBundlers 1)` ‖ `uint48
//! validUntil` ‖ `uint48 validAfter` ‖ 65-byte signature, where the signature is the EIP-191
//! signature of the paymaster's own `getHash(0, packedUserOp)` (read with `eth_call`, so this code
//! never re-implements the contract's hash), with `packedUserOp.paymasterAndData` holding the
//! data without the signature. The format is the one Pimlico's local mock paymaster signs, which
//! the local fork tests already run against.

use std::time::Duration;

use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::{SignerSync, local::PrivateKeySigner},
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::allowlist::UserOpView;

/// Mode byte: verifying mode (0) shifted left, any bundler allowed (1).
const MODE_BYTE: u8 = 0b01;
/// Gas the paymaster's validation may use, and its post-op limit (unused in verifying mode, but
/// part of the UserOp): the values Pimlico's mock paymaster quotes.
pub const VALIDATION_GAS: u64 = 50_000;
pub const POST_OP_GAS: u64 = 100_000;
/// How long a signed sponsorship stays valid.
pub const VALID_FOR: Duration = Duration::from_secs(600);
/// A well-formed placeholder signature for gas estimation (Pimlico's).
const DUMMY_SIGNATURE: [u8; 65] = hex_literal(
    b"cd91f19f0f19ce862d7bec7b7d9b95457145afc6f639c28fd0360f488937bfa41e6eedcd3a46054fd95fcd0e3ef6b0bc0a615c4d975eef55c8a3517257904d5b1c",
);

const fn hex_literal(s: &[u8; 130]) -> [u8; 65] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("not lowercase hex"),
        }
    }
    let mut out = [0u8; 65];
    let mut i = 0;
    while i < 65 {
        out[i] = nibble(s[2 * i]) << 4 | nibble(s[2 * i + 1]);
        i += 1;
    }
    out
}

sol! {
    struct PackedUserOperation {
        address sender;
        uint256 nonce;
        bytes initCode;
        bytes callData;
        bytes32 accountGasLimits;
        uint256 preVerificationGas;
        bytes32 gasFees;
        bytes paymasterAndData;
        bytes signature;
    }

    function getHash(uint8 mode, PackedUserOperation userOp) external view returns (bytes32);
    function validatePaymasterUserOp(PackedUserOperation userOp, bytes32 userOpHash, uint256 maxCost)
        external returns (bytes context, uint256 validationData);
}

/// The paymaster's data without its signature.
pub fn unsigned_data(valid_until: u64, valid_after: u64) -> Vec<u8> {
    let mut out = vec![MODE_BYTE];
    out.extend_from_slice(&valid_until.to_be_bytes()[2..]); // uint48
    out.extend_from_slice(&valid_after.to_be_bytes()[2..]); // uint48
    out
}

fn u128_word(high: U256, low: U256) -> Result<B256> {
    let (high, low): (u128, u128) = (high.try_into()?, low.try_into()?);
    let mut w = [0u8; 32];
    w[..16].copy_from_slice(&high.to_be_bytes());
    w[16..].copy_from_slice(&low.to_be_bytes());
    Ok(B256::from(w))
}

/// The v0.7 packed form of `op` with `paymasterAndData = paymaster ‖ validation gas ‖ post-op gas
/// ‖ data`, as the EntryPoint and the paymaster's `getHash` see it.
pub fn packed(op: &UserOpView, nonce: U256, signature: Bytes, paymaster: Address, data: &[u8]) -> Result<PackedUserOperation> {
    let gas = |v: Option<U256>, what: &str| v.with_context(|| format!("the UserOp has no {what}"));
    let init_code = match (op.factory, &op.factory_data) {
        (Some(factory), data) => [factory.as_slice(), data.as_ref().map_or(&[][..], |d| d.as_ref())].concat(),
        (None, _) => vec![],
    };
    let mut paymaster_and_data = paymaster.to_vec();
    paymaster_and_data.extend_from_slice(&(u128::from(VALIDATION_GAS)).to_be_bytes());
    paymaster_and_data.extend_from_slice(&(u128::from(POST_OP_GAS)).to_be_bytes());
    paymaster_and_data.extend_from_slice(data);
    Ok(PackedUserOperation {
        sender: op.sender,
        nonce,
        initCode: init_code.into(),
        callData: op.call_data.clone(),
        accountGasLimits: u128_word(
            gas(op.verification_gas_limit, "verificationGasLimit")?,
            gas(op.call_gas_limit, "callGasLimit")?,
        )?,
        preVerificationGas: gas(op.pre_verification_gas, "preVerificationGas")?,
        gasFees: u128_word(
            gas(op.max_priority_fee_per_gas, "maxPriorityFeePerGas")?,
            gas(op.max_fee_per_gas, "maxFeePerGas")?,
        )?,
        paymasterAndData: paymaster_and_data.into(),
        signature,
    })
}

pub struct OwnPaymaster {
    pub address: Address,
    signer: PrivateKeySigner,
    rpc_url: String,
    http: reqwest::Client,
}

impl OwnPaymaster {
    pub fn new(address: Address, signer: PrivateKeySigner, rpc_url: String) -> Result<Self> {
        Ok(Self {
            address,
            signer,
            rpc_url,
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
        })
    }

    /// `pm_getPaymasterStubData`: placeholder data of the final length, for gas estimation.
    pub fn stub(&self) -> Value {
        let mut data = unsigned_data(0, 0);
        data.extend_from_slice(&DUMMY_SIGNATURE);
        json!({
            "paymaster": self.address,
            "paymasterData": Bytes::from(data),
            "paymasterVerificationGasLimit": format!("{VALIDATION_GAS:#x}"),
            "paymasterPostOpGasLimit": format!("{POST_OP_GAS:#x}"),
            "isFinal": false,
        })
    }

    /// `pm_getPaymasterData`: the signed data for `op` (already checked by the proxy).
    pub async fn data(&self, op_json: &Value, now: u64) -> Result<Value> {
        let op: UserOpView = serde_json::from_value(op_json.clone()).context("UserOp")?;
        let nonce: U256 = serde_json::from_value(op_json.get("nonce").cloned().context("the UserOp has no nonce")?)?;
        let signature: Bytes = op_json
            .get("signature")
            .map(|s| serde_json::from_value(s.clone()))
            .transpose()?
            .unwrap_or_default();
        let unsigned = unsigned_data(now + VALID_FOR.as_secs(), 0);
        let packed = packed(&op, nonce, signature, self.address, &unsigned)?;
        let hash = self.paymaster_hash(packed).await?;
        let sig = self.signer.sign_message_sync(hash.as_slice())?;
        let mut data = unsigned;
        data.extend_from_slice(&sig.as_bytes());
        Ok(json!({ "paymaster": self.address, "paymasterData": Bytes::from(data) }))
    }

    async fn paymaster_hash(&self, op: PackedUserOperation) -> Result<B256> {
        let call = getHashCall { mode: 0, userOp: op }.abi_encode();
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "eth_call",
            "params": [{ "to": self.address, "data": Bytes::from(call) }, "latest"],
        });
        let resp: Value = self.http.post(&self.rpc_url).json(&body).send().await?.json().await?;
        let result: Bytes = serde_json::from_value(resp.get("result").cloned().context("paymaster getHash failed")?)?;
        ensure!(result.len() == 32, "unexpected getHash result");
        Ok(B256::from_slice(&result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op() -> UserOpView {
        UserOpView {
            sender: Address::repeat_byte(0x11),
            factory: Some(Address::repeat_byte(0x22)),
            factory_data: Some(Bytes::from(vec![0xaa, 0xbb])),
            call_data: Bytes::from(vec![0xcc]),
            call_gas_limit: Some(U256::from(0x0102)),
            verification_gas_limit: Some(U256::from(0x0304)),
            pre_verification_gas: Some(U256::from(0x0506)),
            max_fee_per_gas: Some(U256::from(0x0708)),
            max_priority_fee_per_gas: Some(U256::from(0x090a)),
            ..Default::default()
        }
    }

    #[test]
    fn the_data_is_mode_then_two_uint48s() {
        let d = unsigned_data(0x0000_1122_3344_5566, 7);
        assert_eq!(d, vec![0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0, 0, 0, 0, 0, 7]);
        assert_eq!(DUMMY_SIGNATURE[0], 0xcd);
        assert_eq!(DUMMY_SIGNATURE[64], 0x1c);
    }

    #[test]
    fn the_packed_user_operation_matches_the_v07_layout() {
        let pm = Address::repeat_byte(0x33);
        let p = packed(&op(), U256::from(9), Bytes::new(), pm, &[0xdd]).unwrap();
        assert_eq!(p.initCode.as_ref(), [Address::repeat_byte(0x22).as_slice(), &[0xaa, 0xbb]].concat().as_slice());
        // accountGasLimits = verificationGasLimit (high 16 bytes) ‖ callGasLimit (low 16 bytes).
        assert_eq!(&p.accountGasLimits[14..16], &[0x03, 0x04]);
        assert_eq!(&p.accountGasLimits[30..32], &[0x01, 0x02]);
        // gasFees = maxPriorityFeePerGas ‖ maxFeePerGas.
        assert_eq!(&p.gasFees[14..16], &[0x09, 0x0a]);
        assert_eq!(&p.gasFees[30..32], &[0x07, 0x08]);
        // paymasterAndData = paymaster ‖ uint128 validation gas ‖ uint128 post-op gas ‖ data.
        let pd = p.paymasterAndData.as_ref();
        assert_eq!(&pd[..20], pm.as_slice());
        assert_eq!(u128::from_be_bytes(pd[20..36].try_into().unwrap()), u128::from(VALIDATION_GAS));
        assert_eq!(u128::from_be_bytes(pd[36..52].try_into().unwrap()), u128::from(POST_OP_GAS));
        assert_eq!(&pd[52..], &[0xdd]);
        // No factory: no init code.
        let mut deployed = op();
        deployed.factory = None;
        assert!(packed(&deployed, U256::ZERO, Bytes::new(), pm, &[]).unwrap().initCode.is_empty());
    }

    #[test]
    fn a_user_operation_without_gas_fields_is_refused() {
        let mut o = op();
        o.max_fee_per_gas = None;
        assert!(packed(&o, U256::ZERO, Bytes::new(), Address::ZERO, &[]).is_err());
    }

    /// Live check (network; `cargo test -p zecured-backend live_ -- --ignored` with PAYMASTER_ADDRESS,
    /// PAYMASTER_SIGNER_PRIVATE_KEY and BASE_SEPOLIA_RPC_URL set): a signature from `data` passes the
    /// deployed paymaster's own `validatePaymasterUserOp`, called as the EntryPoint calls it, and a
    /// tampered UserOp fails it.
    #[tokio::test]
    #[ignore]
    async fn live_our_signature_passes_the_deployed_paymasters_validation() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let own = OwnPaymaster::new(
            env("PAYMASTER_ADDRESS").parse().unwrap(),
            env("PAYMASTER_SIGNER_PRIVATE_KEY").parse().unwrap(),
            env("BASE_SEPOLIA_RPC_URL"),
        )
        .unwrap();
        let op_json = json!({
            "sender": Address::repeat_byte(0x11), "nonce": "0x0", "callData": "0xcc",
            "callGasLimit": "0x10000", "verificationGasLimit": "0x20000", "preVerificationGas": "0x10000",
            "maxFeePerGas": "0x5f5e100", "maxPriorityFeePerGas": "0x0", "signature": "0x",
        });
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let answer = own.data(&op_json, now).await.unwrap();
        let data: Bytes = serde_json::from_value(answer["paymasterData"].clone()).unwrap();
        let op: UserOpView = serde_json::from_value(op_json).unwrap();
        let validate = |op: &UserOpView| {
            let packed = packed(op, U256::ZERO, Bytes::new(), own.address, &data).unwrap();
            validatePaymasterUserOpCall { userOp: packed, userOpHash: B256::ZERO, maxCost: U256::ZERO }.abi_encode()
        };
        let call = |calldata: Vec<u8>| {
            let own = &own;
            async move {
                let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_call", "params": [{
                    "from": crate::allowlist::ENTRY_POINT_V07, "to": own.address, "data": Bytes::from(calldata) }, "latest"] });
                let v: Value = own.http.post(&own.rpc_url).json(&body).send().await.unwrap().json().await.unwrap();
                let out: Bytes = serde_json::from_value(v["result"].clone()).unwrap();
                validatePaymasterUserOpCall::abi_decode_returns(&out).unwrap().validationData
            }
        };
        // validationData: bit 0..160 is 1 when the signature fails; validUntil sits above it.
        let ok = call(validate(&op)).await;
        assert_eq!(ok & ((U256::from(1) << 160) - U256::from(1)), U256::ZERO, "signature accepted");
        let mut tampered = op.clone();
        tampered.call_data = Bytes::from(vec![0xcd]);
        let bad = call(validate(&tampered)).await;
        assert_eq!(bad & U256::from(1), U256::from(1), "a changed UserOp fails the signature");
    }
}
