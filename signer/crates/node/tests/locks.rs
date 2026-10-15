//! M-7 (security review 2026-10-11): a hanging light wallet server must not freeze the node.
//! Scans used to run with the global store lock held, so every API call (health included)
//! waited for the slowest server; now the store is free while a scan is in progress.

use std::{sync::Arc, time::Duration};

use alloy::{
    primitives::{Address, B256},
    signers::local::PrivateKeySigner,
};
use rand_core_06::{OsRng, RngCore};
use tokio::net::TcpListener;
use x25519_dalek::StaticSecret;
use zecured_node::{
    Node, NodeOptions,
    config::{Identity, NetworkConfig},
    store::VaultRecord,
};
use zecured_zcash_ops::keys::VaultSecret;

const VAULT: B256 = B256::repeat_byte(1);

/// A server that accepts connections and never answers.
async fn silent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut open = vec![];
        while let Ok((stream, _)) = listener.accept().await {
            open.push(stream);
        }
    });
    url
}

fn vault() -> VaultRecord {
    let keys = zecured_threshold::dkg_in_process(OsRng).unwrap();
    let ak = keys.ak_bytes().unwrap();
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    VaultSecret::from_bytes(sk).full_viewing_key(&ak).unwrap();
    VaultRecord {
        vault_id: VAULT,
        owner: Address::repeat_byte(2),
        account: Address::repeat_byte(3),
        amount: 1_000_000,
        birthday: 1_000,
        expiry_height: 1_288,
        key_package: serde_json::Value::Null,
        public_key_package: serde_json::Value::Null,
        sk: hex::encode(sk),
        ak: hex::encode(ak),
        ufvk: String::new(),
        address: String::new(),
        decisions: vec![],
        attestation: None,
        activated: false,
        outstanding: 0,
        claims: vec![],
        refunds: vec![],
        used_nonces: vec![],
        inflight: None,
        confirmed: vec![],
        last_event: None,
        cancelled: false,
        activated_tag: None,
    }
}

#[tokio::test]
async fn a_hanging_light_wallet_server_does_not_lock_the_store() {
    let dir = std::env::temp_dir().join(format!("zecured-locks-{}", OsRng.next_u64()));
    let cfg = NetworkConfig {
        lightwalletd_url: silent_server().await,
        base_rpc_url: "http://127.0.0.1:9".into(),
        chain_id: 84_532,
        registry: Address::repeat_byte(0xaa),
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
    let node = Arc::new(Node::open(cfg, id, &dir, NodeOptions::default()).unwrap());
    node.store.lock().await.save_vault(vault()).unwrap();

    // An attestation request scans the vault through the silent server: it hangs.
    let scanning = tokio::spawn({
        let node = node.clone();
        async move { node.attest(VAULT).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!scanning.is_finished(), "the scan should still be waiting for the server");
    // Meanwhile the store (health, owner requests, Base sync) is available.
    let store = tokio::time::timeout(Duration::from_millis(200), node.store.lock()).await;
    assert!(store.is_ok(), "the store lock is held across a network scan");
    drop(store);
    scanning.abort();
    std::fs::remove_dir_all(&dir).unwrap();
}
