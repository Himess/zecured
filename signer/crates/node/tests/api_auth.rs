//! The node API over real HTTP: only the health probe is open, everything else (the demo mode
//! switch included) needs the coordinator's signature and is accepted once (security review
//! C-3, L-1); "compromised" mode needs a demo build and `--demo-allow-compromise`; a payout
//! confirmation can be given again (H-5).

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use alloy::{
    primitives::{Address, B256},
    signers::local::PrivateKeySigner,
};
use rand_core_06::{OsRng, RngCore};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use x25519_dalek::StaticSecret;
use zcash_protocol::TxId;
use zecured_base::{payout_confirmation, recover_typed, registry_domain};
use zecured_node::{
    ConfirmIn, Node, NodeOptions, api, auth,
    config::{Identity, NetworkConfig, NodeInfo},
    now,
    store::{ConfirmedPayout, VaultRecord},
};
use zecured_policy::derive::payout_ref;

const TOKEN: &str = "demo-admin-token";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("zecured-api-{}", OsRng.next_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(coordinator: Option<Address>) -> NetworkConfig {
    NetworkConfig {
        // Nothing here is ever contacted by these tests.
        lightwalletd_url: "http://127.0.0.1:9".into(),
        base_rpc_url: "http://127.0.0.1:9".into(),
        chain_id: 84_532,
        registry: Address::repeat_byte(0xaa),
        adapter: Address::repeat_byte(0xbb),
        registry_from_block: 0,
        confirmations: 3,
        vault_expiry_blocks: 288,
        denominations: vec![1_000_000],
        nodes: (1..=5u8)
            .map(|i| NodeInfo {
                index: u16::from(i),
                url: format!("http://127.0.0.1:910{i}"),
                x25519: hex::encode([i; 32]),
                eth: Address::repeat_byte(i),
            })
            .collect(),
        coordinator,
        attest_delay_max_blocks: 0,
        payout_delay_max_blocks: 0,
        commit_depth_blocks: 3,
        commit_depth_secs: 120,
        local_fork: true,
        open_vault: Default::default(),
        payout_lifetime_secs: None,
        zcash_network: None,
        proof_timing: Default::default(),
    }
}

fn node(dir: &TempDir, coordinator: Option<Address>, opts: NodeOptions) -> anyhow::Result<Node> {
    node_at(dir, coordinator, opts, 1)
}

fn node_at(dir: &TempDir, coordinator: Option<Address>, opts: NodeOptions, index: u16) -> anyhow::Result<Node> {
    let id = Identity {
        index,
        eth: PrivateKeySigner::random(),
        x25519: StaticSecret::random_from_rng(OsRng),
    };
    Node::open(config(coordinator), id, &dir.0, opts)
}

async fn serve(node: Node) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api::router(Arc::new(node));
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
}

/// Sends a raw HTTP/1.1 request; returns the status code.
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str, headers: &[(&str, String)]) -> u16 {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(format!("{head}\r\n{body}").as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn signed(key: &PrivateKeySigner, path: &str, body: &str) -> Vec<(&'static str, String)> {
    let (ts, sig) = auth::sign(key, "POST", path, body.as_bytes(), now()).unwrap();
    vec![(auth::TS_HEADER, ts), (auth::SIG_HEADER, sig)]
}

fn mode_body(mode: &str, token: &str) -> String {
    format!(r#"{{"mode":"{mode}","token":"{token}"}}"#)
}

#[tokio::test]
async fn a_node_refuses_to_start_without_a_coordinator() {
    let dir = TempDir::new();
    assert!(node(&dir, None, NodeOptions::default()).is_err());
    let dev = NodeOptions {
        insecure_no_auth: true,
        ..Default::default()
    };
    node(&dir, None, dev).unwrap();
}

#[tokio::test]
async fn the_mode_switch_needs_the_coordinators_signature_and_is_not_replayable() {
    let dir = TempDir::new();
    let coordinator = PrivateKeySigner::random();
    let opts = NodeOptions {
        admin_token: Some(TOKEN.into()),
        ..Default::default()
    };
    let addr = serve(node(&dir, Some(coordinator.address()), opts).unwrap()).await;
    let path = "/v1/admin/mode";
    let offline = mode_body("offline", TOKEN);

    // The health probe stays open.
    assert_eq!(request(addr, "GET", "/v1/health", "", &[]).await, 200);
    // Unsigned: refused, even with the right token (it used to be exempt from D16).
    assert_eq!(request(addr, "POST", path, &offline, &[]).await, 401);
    // Signed by anyone but the coordinator: refused.
    let stranger = PrivateKeySigner::random();
    assert_eq!(request(addr, "POST", path, &offline, &signed(&stranger, path, &offline)).await, 401);
    // The coordinator's signature but a wrong token: refused.
    let wrong = mode_body("offline", "guess");
    assert_eq!(request(addr, "POST", path, &wrong, &signed(&coordinator, path, &wrong)).await, 401);
    // "offline" works on any node; the same signed request cannot be replayed.
    let headers = signed(&coordinator, path, &offline);
    assert_eq!(request(addr, "POST", path, &offline, &headers).await, 200);
    assert_eq!(request(addr, "POST", path, &offline, &headers).await, 401);
    // Any other route needs the signature too.
    assert_eq!(request(addr, "POST", "/v1/sync", "{}", &[]).await, 401);

    // "compromised" is refused on a node not started with --demo-allow-compromise.
    let compromised = mode_body("compromised", TOKEN);
    assert_eq!(request(addr, "POST", path, &compromised, &signed(&coordinator, path, &compromised)).await, 403);
}

#[cfg(not(feature = "demo"))]
#[test]
fn compromised_mode_is_not_built_without_the_demo_feature() {
    let dir = TempDir::new();
    let opts = NodeOptions {
        demo_allow_compromise: true,
        ..Default::default()
    };
    assert!(node(&dir, Some(Address::repeat_byte(1)), opts).is_err());
}

/// On a demo build only nodes 4 and 5 may run with `--demo-allow-compromise` (D18); node 1 is
/// refused at start-up, so the test runs node 4.
#[cfg(feature = "demo")]
#[tokio::test]
async fn compromised_mode_needs_the_flag_on_a_demo_build() {
    let dir = TempDir::new();
    let coordinator = PrivateKeySigner::random();
    let opts = || NodeOptions {
        admin_token: Some(TOKEN.into()),
        demo_allow_compromise: true,
        ..Default::default()
    };
    assert!(node_at(&dir, Some(coordinator.address()), opts(), 1).is_err());
    let addr = serve(node_at(&dir, Some(coordinator.address()), opts(), 4).unwrap()).await;
    let path = "/v1/admin/mode";
    let compromised = mode_body("compromised", TOKEN);
    assert_eq!(request(addr, "POST", path, &compromised, &signed(&coordinator, path, &compromised)).await, 200);
}

/// H-5: once a node confirmed a payout and cleared it, a failed `confirmPayout` on Base used to
/// wedge the vault (later calls: "no payout in flight"). The node now signs the same
/// confirmation again, and only for the transaction it confirmed.
#[tokio::test]
async fn a_confirmation_can_be_given_again() {
    let dir = TempDir::new();
    let dev = NodeOptions {
        insecure_no_auth: true,
        ..Default::default()
    };
    let node = node(&dir, None, dev).unwrap();
    let vault_id = B256::repeat_byte(1);
    let claim = [7u8; 32];
    // Not a palindrome, so byte order matters.
    let txid: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
    node.store
        .lock()
        .await
        .save_vault(VaultRecord {
            vault_id,
            owner: Address::repeat_byte(2),
            account: Address::repeat_byte(3),
            amount: 1_000_000,
            birthday: 1,
            expiry_height: 289,
            key_package: serde_json::Value::Null,
            public_key_package: serde_json::Value::Null,
            sk: String::new(),
            ak: String::new(),
            ufvk: String::new(),
            address: String::new(),
            decisions: vec![],
            activated: true,
            outstanding: 0,
            claims: vec![],
            refunds: vec![],
            used_nonces: vec![],
            inflight: None,
            confirmed: vec![ConfirmedPayout {
                claim_ids: vec![claim],
                txid,
            }],
            last_event: None,
            activated_tag: None,
            reopened: vec![],
        })
        .unwrap();

    // The coordinator sends the txid in display form (byte-reversed hex).
    let display = TxId::from_bytes(txid).to_string();
    let again = ConfirmIn {
        claim_ids: vec![B256::from(claim)],
        txid: display,
    };
    let response = node.confirm(vault_id, again.clone()).await.unwrap();
    // R-1: the confirmation names the payout, by the reference the node derives from the
    // transaction it confirmed (the vault's tag key over the txid, internal byte order).
    let msg = payout_confirmation(vault_id, B256::from(payout_ref("", &txid)), &again.claim_ids);
    let domain = registry_domain(84_532, Address::repeat_byte(0xaa));
    let signer = recover_typed(&domain, &msg, &response.signature).unwrap();
    assert_eq!(signer, node.id.eth.address());
    // Another payout's reference: not this signature.
    let other = payout_confirmation(vault_id, B256::repeat_byte(0xab), &again.claim_ids);
    assert_ne!(recover_typed(&domain, &other, &response.signature).unwrap(), node.id.eth.address());

    // Another transaction, or other claims: nothing in flight, nothing confirmed.
    let other_tx = ConfirmIn {
        txid: TxId::from_bytes([6; 32]).to_string(),
        ..again.clone()
    };
    assert!(node.confirm(vault_id, other_tx).await.is_err());
    // The same bytes in internal order are another transaction id.
    let internal_order = ConfirmIn {
        txid: hex::encode(txid),
        ..again.clone()
    };
    assert!(node.confirm(vault_id, internal_order).await.is_err());
    let other_claims = ConfirmIn {
        claim_ids: vec![B256::repeat_byte(8)],
        ..again
    };
    assert!(node.confirm(vault_id, other_claims).await.is_err());
}
