//! H-2 (security review 2026-10-11): a vault never pays one of its own addresses. A borrower
//! who set the release destination to the vault's internal address had the payout land as
//! "change" outside the books and brick the vault; destinations, refund recipients and
//! third-party recipients equal to any vault address (any diversifier, either scope) are now
//! refused.

use alloy::{
    primitives::{Address, B256},
    signers::local::PrivateKeySigner,
};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use rand_core_06::{OsRng, RngCore};
use x25519_dalek::StaticSecret;
use zecured_base::{Destination, RefundRequest, registry_domain, sign_typed};
use zecured_node::{
    DestinationIn, Node, NodeOptions, RecipientIn, RefundIn,
    config::{Identity, NetworkConfig},
    now,
    store::VaultRecord,
};
use zecured_policy::ledger::commitment;
use zecured_zcash_ops::keys::VaultSecret;

const CHAIN_ID: u64 = 84_532;
const REGISTRY: Address = Address::repeat_byte(0xaa);
const VAULT: B256 = B256::repeat_byte(1);

fn node(dir: &std::path::Path) -> Node {
    let cfg = NetworkConfig {
        // Never contacted: every refusal below happens before any scan.
        lightwalletd_url: "http://127.0.0.1:9".into(),
        base_rpc_url: "http://127.0.0.1:9".into(),
        chain_id: CHAIN_ID,
        registry: REGISTRY,
        adapter: Address::repeat_byte(0xbb),
        registry_from_block: 0,
        confirmations: 3,
        vault_expiry_blocks: 288,
        denominations: vec![1_000_000],
        nodes: vec![],
        coordinator: Some(Address::repeat_byte(0xcc)),
        attest_delay_max_blocks: 0,
        payout_delay_max_blocks: 0,
        commit_depth_blocks: 3,
        local_fork: true,
        open_vault: Default::default(),
    };
    let id = Identity {
        index: 1,
        eth: PrivateKeySigner::random(),
        x25519: StaticSecret::random_from_rng(OsRng),
    };
    Node::open(cfg, id, dir, NodeOptions::default()).unwrap()
}

/// A vault with real keys (FROST group key as `ak`, a random ZIP 2005 `sk`).
fn vault(owner: Address) -> (VaultRecord, FullViewingKey) {
    let keys = zecured_threshold::dkg_in_process(OsRng).unwrap();
    let ak = keys.ak_bytes().unwrap();
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    let fvk = VaultSecret::from_bytes(sk).full_viewing_key(&ak).unwrap();
    let record = VaultRecord {
        vault_id: VAULT,
        owner,
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
        attestation: None,
        activated: true,
        outstanding: 1_000_000,
        claims: vec![],
        refunds: vec![],
        used_nonces: vec![],
        inflight: None,
        confirmed: vec![],
        last_event: None,
        cancelled: false,
        activated_tag: None,
    };
    (record, fvk)
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

fn recipient(addr: &orchard::Address, salt: [u8; 32]) -> RecipientIn {
    RecipientIn {
        receiver: hex::encode(addr.to_raw_address_bytes()),
        salt: hex::encode(salt),
    }
}

fn destination(owner: &PrivateKeySigner, addr: &orchard::Address, nonce: u64) -> DestinationIn {
    let salt = [nonce as u8; 32];
    let deadline = now() + 600;
    let msg = Destination {
        vaultId: VAULT,
        commitment: B256::from(commitment(&salt, &addr.to_raw_address_bytes())),
        nonce,
        deadline,
    };
    DestinationIn {
        recipient: recipient(addr, salt),
        nonce,
        deadline,
        signature: sign_typed(owner, &registry_domain(CHAIN_ID, REGISTRY), &msg).unwrap(),
    }
}

#[tokio::test]
async fn a_vault_never_pays_its_own_addresses() {
    let dir = std::env::temp_dir().join(format!("zecured-recipients-{}", OsRng.next_u64()));
    let node = node(&dir);
    let owner = PrivateKeySigner::random();
    let (record, fvk) = vault(owner.address());
    node.store.lock().await.save_vault(record).unwrap();

    let own = [
        fvk.address_at(0u32, Scope::Internal),
        fvk.address_at(0u32, Scope::External),
        fvk.address_at(7u32, Scope::External),
        fvk.address_at(3u32, Scope::Internal),
    ];
    for (i, addr) in own.iter().enumerate() {
        let nonce = u64::try_from(i).unwrap();
        // The owner's payout destination.
        let err = node.destination(VAULT, destination(&owner, addr, nonce)).await.unwrap_err();
        assert!(err.to_string().contains("own addresses"), "{err}");
        // A third party's recipient (liquidator, market maker).
        assert!(node.add_recipient(recipient(addr, [9; 32])).await.is_err());
        // A refund of a non-conforming deposit.
        let salt = [8; 32];
        let deadline = now() + 600;
        let msg = RefundRequest {
            vaultId: VAULT,
            noteId: B256::repeat_byte(4),
            commitment: B256::from(commitment(&salt, &addr.to_raw_address_bytes())),
            nonce: 100 + nonce,
            deadline,
        };
        let refund = RefundIn {
            note_id: B256::repeat_byte(4),
            recipient: recipient(addr, salt),
            nonce: 100 + nonce,
            deadline,
            signature: sign_typed(&owner, &registry_domain(CHAIN_ID, REGISTRY), &msg).unwrap(),
        };
        let err = node.refund_request(VAULT, refund).await.unwrap_err();
        assert!(err.to_string().contains("own addresses"), "{err}");
    }
    // Nothing was recorded, not even the nonces.
    assert!(node.store.lock().await.vaults[&VAULT].used_nonces.is_empty());

    // Anyone else's address is fine.
    let other = stranger();
    node.destination(VAULT, destination(&owner, &other, 50)).await.unwrap();
    node.add_recipient(recipient(&other, [9; 32])).await.unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
