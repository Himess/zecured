//! Five nodes run the vault DKG through a relaying "coordinator" that only moves JSON
//! messages: all five derive the same vault keys and address, and a tampered contribution
//! is detected.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, Bytes};
use rand_core_06::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};
use zecured_node::{
    config::{NetworkConfig, NodeInfo},
    dkg::{self, Round1Out, Round2In, Round3In, VaultOpen},
    store::Store,
};

struct TestNode {
    index: u16,
    secret: StaticSecret,
    store: Store,
    _dir: tempdir::Dir,
}

mod tempdir {
    use std::path::PathBuf;
    pub struct Dir(pub PathBuf);
    impl Dir {
        pub fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("zecured-test-{tag}-{}", rand_suffix()));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn rand_suffix() -> u64 {
        use rand_core_06::RngCore;
        rand_core_06::OsRng.next_u64()
    }
}

fn setup() -> (NetworkConfig, Vec<TestNode>) {
    let mut nodes = vec![];
    let mut infos = vec![];
    for index in 1..=5u16 {
        let secret = StaticSecret::random_from_rng(OsRng);
        infos.push(NodeInfo {
            index,
            url: format!("http://127.0.0.1:910{index}"),
            x25519: hex::encode(PublicKey::from(&secret).as_bytes()),
            eth: Address::repeat_byte(index as u8),
        });
        let dir = tempdir::Dir::new(&format!("node{index}"));
        nodes.push(TestNode {
            index,
            secret,
            store: Store::open(&dir.0).unwrap(),
            _dir: dir,
        });
    }
    let cfg = NetworkConfig {
        lightwalletd_url: String::new(),
        base_rpc_url: String::new(),
        chain_id: 84_532,
        registry: Address::repeat_byte(0xaa),
        adapter: Address::repeat_byte(0xbb),
        registry_from_block: 0,
        confirmations: 3,
        vault_expiry_blocks: 288,
        denominations: vec![1_000_000, 5_000_000],
        nodes: infos,
        coordinator: None,
        attest_delay_max_blocks: 0,
    };
    (cfg, nodes)
}

fn open() -> VaultOpen {
    VaultOpen {
        vault_id: B256::repeat_byte(7),
        owner: Address::repeat_byte(1),
        account: Address::repeat_byte(2),
        amount: 1_000_000,
        request_id: B256::repeat_byte(3),
        deadline: u64::MAX,
        signature: Bytes::new(),
        birthday: 4_486_000,
        expiry_height: 4_486_288,
    }
}

fn run_dkg(cfg: &NetworkConfig, nodes: &mut [TestNode], tamper: bool) -> Vec<anyhow::Result<dkg::Round3Out>> {
    let mut pending = BTreeMap::new();
    let mut r1: Vec<Round1Out> = vec![];
    for n in nodes.iter() {
        let (p, out) = dkg::round1(n.index, open()).unwrap();
        pending.insert(n.index, p);
        // Everything the coordinator relays goes through JSON.
        r1.push(serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap());
    }
    let r2_in = Round2In {
        vault_id: open().vault_id,
        round1: r1.clone(),
    };
    let mut to: BTreeMap<u16, BTreeMap<u16, dkg::Sealed>> = BTreeMap::new();
    for n in nodes.iter() {
        let out = dkg::round2(cfg, &n.secret, n.index, pending.get_mut(&n.index).unwrap(), &r2_in).unwrap();
        for (recipient, sealed) in out.sealed {
            to.entry(recipient).or_default().insert(n.index, sealed);
        }
    }
    if tamper {
        // A malicious relay flips a byte of node 2's message to node 1.
        to.get_mut(&1).unwrap().get_mut(&2).unwrap().ciphertext[3] ^= 0x40;
    }
    nodes
        .iter_mut()
        .map(|n| {
            let input = Round3In {
                vault_id: open().vault_id,
                round1: r1.clone(),
                sealed: to.remove(&n.index).unwrap_or_default(),
            };
            dkg::round3(cfg, &n.secret, n.index, pending.remove(&n.index).unwrap(), &input, &mut n.store)
        })
        .collect()
}

#[test]
fn five_nodes_agree_on_the_vault() {
    let (cfg, mut nodes) = setup();
    let outs: Vec<dkg::Round3Out> = run_dkg(&cfg, &mut nodes, false).into_iter().map(|r| r.unwrap()).collect();
    for o in &outs[1..] {
        assert_eq!(o.ak, outs[0].ak);
        assert_eq!(o.ufvk, outs[0].ufvk);
        assert_eq!(o.address, outs[0].address);
        assert_eq!(o.public_key_package, outs[0].public_key_package);
    }
    assert!(outs[0].address.starts_with("utest1"));
    // Every node stored the vault with the same sk but its own key share.
    let sks: Vec<String> = nodes.iter().map(|n| n.store.vaults[&open().vault_id].sk.clone()).collect();
    assert!(sks.iter().all(|s| *s == sks[0]));
    let shares: Vec<String> = nodes.iter().map(|n| n.store.vaults[&open().vault_id].key_package.to_string()).collect();
    assert!(shares.windows(2).all(|w| w[0] != w[1]));
}

#[test]
fn tampered_relay_is_detected() {
    let (cfg, mut nodes) = setup();
    let outs = run_dkg(&cfg, &mut nodes, true);
    assert!(outs[0].is_err(), "node 1 must reject the altered message");
    assert!(nodes[0].store.vaults.is_empty());
}
