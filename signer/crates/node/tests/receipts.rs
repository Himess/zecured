//! R-5: a node issues a liveness receipt, `DestinationReceived(claimId, commitment, receivedAt)`
//! under the bond's domain, only for a pending claim whose destination it holds and has validated
//! (a valid Ironwood receiver that is none of the vault's own addresses), at the claim's current
//! commitment; `receivedAt` is when it first held that recipient. R-1: a reopened claim is never
//! confirmed by signature.

use alloy::{
    primitives::{Address, B256},
    signers::local::PrivateKeySigner,
    sol_types::SolStruct,
};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use rand_core_06::{OsRng, RngCore};
use x25519_dalek::StaticSecret;
use zcash_protocol::TxId;
use zecured_base::{
    operator::{bond_domain, destination_received},
    recover_typed,
};
use zecured_node::{
    ConfirmIn, Node, NodeOptions,
    config::{Identity, NetworkConfig},
    store::{ConfirmedPayout, VaultRecord},
};
use zecured_policy::ledger::{Claim, ClaimKind, ClaimState, Recipient};
use zecured_zcash_ops::keys::VaultSecret;

const CHAIN_ID: u64 = 84_532;
const BOND: Address = Address::repeat_byte(0xcc);
const VAULT: B256 = B256::repeat_byte(1);

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn node() -> (Node, TempDir) {
    node_at("http://127.0.0.1:9")
}

/// A node whose Base RPC is `rpc`.
fn node_at(rpc: &str) -> (Node, TempDir) {
    let dir = TempDir(std::env::temp_dir().join(format!("zecured-receipts-{}", OsRng.next_u64())));
    let cfg = NetworkConfig {
        lightwalletd_url: "http://127.0.0.1:9".into(),
        base_rpc_url: rpc.into(),
        chain_id: CHAIN_ID,
        registry: Address::repeat_byte(0xaa),
        adapter: Address::repeat_byte(0xbb),
        registry_from_block: 0,
        confirmations: 3,
        vault_expiry_blocks: 288,
        denominations: vec![1_000_000],
        nodes: vec![],
        coordinator: Some(Address::repeat_byte(0xdd)),
        attest_delay_max_blocks: 0,
        payout_delay_max_blocks: 0,
        commit_depth_blocks: 3,
        commit_depth_secs: 120,
        local_fork: true,
        open_vault: Default::default(),
        payout_lifetime_secs: None,
        zcash_network: None,
        proof_timing: Default::default(),
    };
    let id = Identity {
        index: 2,
        eth: PrivateKeySigner::random(),
        x25519: StaticSecret::random_from_rng(OsRng),
    };
    (Node::open(cfg, id, &dir.0, NodeOptions::default()).unwrap(), dir)
}

fn stranger() -> orchard::Address {
    loop {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
            return FullViewingKey::from(&sk).address_at(0u32, Scope::External);
        }
    }
}

fn claim(id: u8, kind: ClaimKind, commitment: [u8; 32]) -> Claim {
    Claim {
        id: [id; 32],
        kind,
        amount: 300_000,
        commitment,
        order: (10, u64::from(id)),
        created_at: 1_000,
        partner: None,
        state: ClaimState::Unpaid,
        refund_tag: None,
        bond_paid: false,
        proven: false,
        confirmed_at: 0,
    }
}

/// A vault with real keys, whose claims are `claims`.
fn vault(claims: Vec<Claim>) -> (VaultRecord, FullViewingKey) {
    let keys = zecured_threshold::dkg_in_process(OsRng).unwrap();
    let ak = keys.ak_bytes().unwrap();
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    let fvk = VaultSecret::from_bytes(sk).full_viewing_key(&ak).unwrap();
    let record = VaultRecord {
        vault_id: VAULT,
        owner: Address::repeat_byte(2),
        account: Address::repeat_byte(3),
        amount: 1_000_000,
        birthday: 1,
        expiry_height: 289,
        key_package: serde_json::Value::Null,
        public_key_package: serde_json::Value::Null,
        sk: hex::encode(sk),
        ak: hex::encode(ak),
        ufvk: String::new(),
        address: String::new(),
        decisions: vec![],
        activated: true,
        outstanding: 100_000,
        claims,
        refunds: vec![],
        used_nonces: vec![],
        inflight: None,
        confirmed: vec![],
        last_event: None,
        activated_tag: None,
        reopened: vec![],
    };
    (record, fvk)
}

#[tokio::test]
async fn a_receipt_needs_the_claims_destination_held_and_valid() {
    let (node, _dir) = node();
    let held = Recipient {
        receiver: stranger().to_raw_address_bytes(),
        salt: [5; 32],
    };
    let unknown = Recipient {
        receiver: stranger().to_raw_address_bytes(),
        salt: [6; 32],
    };
    let claims = vec![
        claim(1, ClaimKind::Liquidation, held.commitment()),
        claim(2, ClaimKind::Release, unknown.commitment()),
        claim(3, ClaimKind::Refund, [0; 32]),
        Claim {
            state: ClaimState::Paid,
            ..claim(4, ClaimKind::Liquidation, held.commitment())
        },
        Claim {
            bond_paid: true,
            ..claim(5, ClaimKind::Release, held.commitment())
        },
        claim(6, ClaimKind::Fee, held.commitment()),
    ];
    let (record, fvk) = vault(claims);
    let own = Recipient {
        receiver: fvk.address_at(0u32, Scope::Internal).to_raw_address_bytes(),
        salt: [7; 32],
    };
    let mut record = record;
    record.claims.push(claim(7, ClaimKind::Release, own.commitment()));
    let mut store = node.store.lock().await;
    store.save_vault(record).unwrap();
    store.add_recipient(held).unwrap();
    // A recipient that is the vault's own address (stored behind the node API's back).
    store.add_recipient(own).unwrap();
    let first_held = store.held_since(&held.commitment(), u64::MAX).unwrap();

    let receipt = node.issue_receipt(&mut store, B256::repeat_byte(1), BOND).unwrap();
    assert_eq!((receipt.commitment, receipt.received_at), (B256::from(held.commitment()), first_held));
    let msg = destination_received(B256::repeat_byte(1), receipt.commitment, receipt.received_at);
    let signer = recover_typed(&bond_domain(CHAIN_ID, BOND), &msg, &receipt.signature).unwrap();
    assert_eq!(signer, node.id.eth.address(), "the node's attestation key, under the bond's domain");
    // Asked again: the same receipt (the time it first held the recipient).
    let again = node.issue_receipt(&mut store, B256::repeat_byte(1), BOND).unwrap();
    assert_eq!(again.received_at, receipt.received_at);
    // Under another bond's domain the digest differs.
    let other_bond = msg.eip712_signing_hash(&bond_domain(CHAIN_ID, Address::repeat_byte(0xce)));
    assert_ne!(other_bond, msg.eip712_signing_hash(&bond_domain(CHAIN_ID, BOND)));

    for (id, why) in [
        (2, "this node does not hold"),
        (3, "no destination"),
        (4, "not pending"),
        (5, "not pending"),
        (6, "do not cover"),
        (7, "own addresses"),
        (9, "unknown claim"),
    ] {
        let e = node.issue_receipt(&mut store, B256::repeat_byte(id), BOND).unwrap_err();
        assert!(format!("{e:#}").contains(why), "claim {id}: {e:#}");
    }
}

/// A Base RPC stub: the Registry names `BOND`, whose `domainSeparator()` is `separator`.
async fn base_stub(separator: B256) -> String {
    use alloy::sol_types::SolCall;
    use axum::{Json, Router, routing::post};
    use zecured_base::events::{ISignerBond, IZecuredRegistry};
    let app = Router::new().route(
        "/",
        post(move |Json(req): Json<serde_json::Value>| async move {
            let result = match req["method"].as_str() {
                Some("eth_call") => {
                    let call = &req["params"][0];
                    let data = call["input"].as_str().or(call["data"].as_str()).unwrap_or_default();
                    let selector = hex::decode(data.trim_start_matches("0x").get(..8).unwrap_or_default()).unwrap();
                    if selector == IZecuredRegistry::signerBondCall::SELECTOR {
                        let mut word = vec![0u8; 12];
                        word.extend_from_slice(BOND.as_slice());
                        serde_json::json!(format!("0x{}", hex::encode(word)))
                    } else if selector == ISignerBond::domainSeparatorCall::SELECTOR {
                        serde_json::json!(separator)
                    } else {
                        return Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"],
                            "error": { "code": 3, "message": "execution reverted" } }));
                    }
                }
                _ => serde_json::json!("0x14a34"),
            };
            Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// R-5: a node serves receipts only while the bond's own EIP-712 domain is the receipts' (a bond
/// with another domain would never take them).
#[tokio::test]
async fn receipts_need_the_bonds_own_domain() {
    for (separator, served) in [
        (bond_domain(CHAIN_ID, BOND).separator(), true),
        (B256::repeat_byte(9), false),
    ] {
        let rpc = base_stub(separator).await;
        let (node, _dir) = node_at(&rpc);
        let held = Recipient {
            receiver: stranger().to_raw_address_bytes(),
            salt: [5; 32],
        };
        let (record, _) = vault(vec![claim(1, ClaimKind::Liquidation, held.commitment())]);
        {
            let mut store = node.store.lock().await;
            store.save_vault(record).unwrap();
            store.add_recipient(held).unwrap();
        }
        let r = node.receipt(B256::repeat_byte(1)).await;
        assert_eq!(r.is_ok(), served, "{:?}", r.as_ref().err());
        if let Err(e) = r {
            assert!(format!("{e:#}").contains("domain"), "{e:#}");
        }
    }
}

/// R-5: a changed destination (a refund's, given again) gets a receipt for the new commitment only.
#[tokio::test]
async fn a_changed_destination_needs_a_new_receipt() {
    let (node, _dir) = node();
    let first = Recipient {
        receiver: stranger().to_raw_address_bytes(),
        salt: [1; 32],
    };
    let second = Recipient {
        receiver: stranger().to_raw_address_bytes(),
        salt: [2; 32],
    };
    let (record, _) = vault(vec![claim(1, ClaimKind::Refund, first.commitment())]);
    let mut store = node.store.lock().await;
    store.save_vault(record).unwrap();
    store.add_recipient(first).unwrap();
    store.add_recipient(second).unwrap();
    let before = node.issue_receipt(&mut store, B256::repeat_byte(1), BOND).unwrap();
    let mut v = store.vaults[&VAULT].clone();
    v.claims[0].commitment = second.commitment();
    store.save_vault(v).unwrap();
    let after = node.issue_receipt(&mut store, B256::repeat_byte(1), BOND).unwrap();
    assert_eq!(after.commitment, B256::from(second.commitment()));
    assert_ne!(after.signature, before.signature);
}

/// R-1: a reopened claim is settled only by a proof: the node refuses its confirmation, even of a
/// payout it confirmed before.
#[tokio::test]
async fn a_reopened_claim_is_never_confirmed_by_signature() {
    let (node, _dir) = node();
    let txid: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
    let (mut record, _) = vault(vec![claim(1, ClaimKind::Liquidation, [9; 32])]);
    record.confirmed = vec![ConfirmedPayout {
        claim_ids: vec![[1; 32]],
        txid,
    }];
    record.reopened = vec![[1; 32]];
    node.store.lock().await.save_vault(record).unwrap();
    let request = ConfirmIn {
        claim_ids: vec![B256::repeat_byte(1)],
        txid: TxId::from_bytes(txid).to_string(),
    };
    let e = node.confirm(VAULT, request).await.unwrap_err();
    assert!(format!("{e:#}").contains("reopened"), "{e:#}");
}
