//! Five nodes run the vault DKG through a relaying "coordinator" that only moves JSON
//! messages: all five derive the same vault keys and address and each signs `VaultOpened` for
//! them (C-4), and a tampered contribution is detected, as is a relay that shows signers
//! different round-1 transcripts (M-4).

use std::collections::BTreeMap;

use alloy::{
    primitives::{Address, B256, Bytes},
    signers::local::PrivateKeySigner,
};
use rand_core_06::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};
use zecured_base::{VaultOpened, check_signed_by_all, registry_domain, tag_key_hash, text_hash, vault_opened};
use zecured_node::{
    config::{NetworkConfig, NodeInfo},
    dkg::{self, Round1Out, Round2In, Round3In, Sealed, VaultOpen},
    store::Store,
};

struct TestNode {
    index: u16,
    secret: StaticSecret,
    /// Attestation key (signs `VaultOpened`).
    eth: PrivateKeySigner,
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
        let eth = PrivateKeySigner::random();
        infos.push(NodeInfo {
            index,
            url: format!("http://127.0.0.1:910{index}"),
            x25519: hex::encode(PublicKey::from(&secret).as_bytes()),
            eth: eth.address(),
        });
        let dir = tempdir::Dir::new(&format!("node{index}"));
        nodes.push(TestNode {
            index,
            secret,
            eth,
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
        payout_delay_max_blocks: 0,
        commit_depth_blocks: 3,
        commit_depth_secs: 120,
        local_fork: true,
        open_vault: Default::default(),
        payout_lifetime_secs: None,
        zcash_network: None,
        proof_timing: Default::default(),
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
    run_dkg_for(cfg, nodes, tamper, open())
}

fn run_dkg_for(
    cfg: &NetworkConfig,
    nodes: &mut [TestNode],
    tamper: bool,
    open: VaultOpen,
) -> Vec<anyhow::Result<dkg::Round3Out>> {
    let mut pending = BTreeMap::new();
    let mut r1: Vec<Round1Out> = vec![];
    for n in nodes.iter() {
        let (p, out) = dkg::round1(n.index, open.clone()).unwrap();
        pending.insert(n.index, p);
        // Everything the coordinator relays goes through JSON.
        r1.push(serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap());
    }
    let r2_in = Round2In {
        vault_id: open.vault_id,
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
                vault_id: open.vault_id,
                round1: r1.clone(),
                sealed: to.remove(&n.index).unwrap_or_default(),
            };
            dkg::round3(cfg, &n.secret, &n.eth, n.index, pending.remove(&n.index).unwrap(), &input, &mut n.store)
        })
        .collect()
}

/// The `VaultOpened` the owner checks: the request it signed, the vault id and expiry height
/// the coordinator chose, the address and viewing key it was given.
fn opened_for(address: &str, ufvk: &str) -> VaultOpened {
    let o = open();
    vault_opened(o.vault_id, o.owner, o.account, o.amount, o.expiry_height, address, ufvk)
}

fn signer_set(cfg: &NetworkConfig) -> Vec<(u16, Address)> {
    cfg.nodes.iter().map(|n| (n.index, n.eth)).collect()
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

    // C-4: each node signed `VaultOpened` for that address and the viewing key's tag key with its
    // own attestation key; all five together vouch for them.
    let domain = registry_domain(cfg.chain_id, cfg.registry);
    let signatures: Vec<(u16, Bytes)> = outs.iter().map(|o| (o.index, o.signature.clone())).collect();
    let opened = opened_for(&outs[0].address, &outs[0].ufvk);
    check_signed_by_all(&domain, &opened, &signer_set(&cfg), &signatures).unwrap();
    // They vouch for nothing else: not another address or tag key (a coordinator's own), nor
    // another expiry height or owner than the request each node checked.
    let mut forged = opened_for("utest1attacker", &outs[0].ufvk);
    assert!(check_signed_by_all(&domain, &forged, &signer_set(&cfg), &signatures).is_err());
    forged = opened_for(&outs[0].address, "uviewtest1attacker");
    assert!(check_signed_by_all(&domain, &forged, &signer_set(&cfg), &signatures).is_err());
    forged = opened_for(&outs[0].address, &outs[0].ufvk);
    forged.expiryHeight += 1;
    assert!(check_signed_by_all(&domain, &forged, &signer_set(&cfg), &signatures).is_err());
    forged = opened_for(&outs[0].address, &outs[0].ufvk);
    forged.owner = Address::repeat_byte(0x99);
    assert!(check_signed_by_all(&domain, &forged, &signer_set(&cfg), &signatures).is_err());
    assert_eq!(opened.addressHash, text_hash(&nodes[0].store.vaults[&open().vault_id].address));
    assert_eq!(opened.tagKeyHash, tag_key_hash(&nodes[0].store.vaults[&open().vault_id].ufvk));
    // Four of five signatures are not enough.
    assert!(check_signed_by_all(&domain, &opened, &signer_set(&cfg), &signatures[1..]).is_err());

    // Every node stored the vault with the same sk but its own key share.
    let sks: Vec<String> = nodes.iter().map(|n| n.store.vaults[&open().vault_id].sk.clone()).collect();
    assert!(sks.iter().all(|s| *s == sks[0]));
    let shares: Vec<String> = nodes.iter().map(|n| n.store.vaults[&open().vault_id].key_package.to_string()).collect();
    assert!(shares.windows(2).all(|w| w[0] != w[1]));
}

/// The vault's address and viewing key are encoded for the deployment's Zcash network, which the
/// network config gives or the Base chain implies: Testnet's `utest1`/`uviewtest1` with Base
/// Sepolia, Mainnet's `u1`/`uview1` with Base.
#[test]
fn the_vault_is_encoded_for_the_deployments_zcash_network() {
    use zecured_node::config::ZcashNetwork;
    for (chain_id, configured, address, ufvk) in [
        (84_532, None, "utest1", "uviewtest1"),
        (8_453, None, "u1", "uview1"),
        (8_453, Some(ZcashNetwork::Mainnet), "u1", "uview1"),
        (84_532, Some(ZcashNetwork::Testnet), "utest1", "uviewtest1"),
    ] {
        let (mut cfg, mut nodes) = setup();
        cfg.chain_id = chain_id;
        cfg.zcash_network = configured;
        cfg.local_fork = false;
        cfg.attest_delay_max_blocks = 2;
        cfg.payout_delay_max_blocks = 8;
        cfg.check().unwrap();
        let outs: Vec<dkg::Round3Out> = run_dkg(&cfg, &mut nodes, false).into_iter().map(|r| r.unwrap()).collect();
        assert!(outs[0].address.starts_with(address), "{chain_id}: {}", outs[0].address);
        assert!(outs[0].ufvk.starts_with(ufvk), "{chain_id}: {}", outs[0].ufvk);
        // The address decodes as an Ironwood receiver on that network only.
        let network = cfg.zcash().params();
        assert!(zecured_zcash_ops::keys::ironwood_receiver(&network, &outs[0].address).is_ok());
        let other = match cfg.zcash() {
            ZcashNetwork::Testnet => ZcashNetwork::Mainnet,
            ZcashNetwork::Mainnet => ZcashNetwork::Testnet,
        };
        assert!(zecured_zcash_ops::keys::ironwood_receiver(&other.params(), &outs[0].address).is_err());
    }
}
/// L-9: a second opening for the same account (or owner) that was still running when the
/// first one completed saves no vault.
#[test]
fn a_second_vault_for_one_account_is_refused_before_it_is_saved() {
    let (cfg, mut nodes) = setup();
    assert!(run_dkg(&cfg, &mut nodes, false).iter().all(|r| r.is_ok()));
    let second = VaultOpen {
        vault_id: B256::repeat_byte(8),
        ..open()
    };
    for r in run_dkg_for(&cfg, &mut nodes, false, second) {
        let e = r.expect_err("a second vault for the account");
        assert!(e.to_string().contains("already has a vault"), "{e}");
    }
    for n in &nodes {
        assert!(!n.store.vaults.contains_key(&B256::repeat_byte(8)));
    }
}

#[test]
fn tampered_relay_is_detected() {
    let (cfg, mut nodes) = setup();
    let outs = run_dkg(&cfg, &mut nodes, true);
    assert!(outs[0].is_err(), "node 1 must reject the altered message");
    assert!(nodes[0].store.vaults.is_empty());
}

/// M-4 (security review 2026-10-11): a malicious coordinator and one malicious signer (5) show
/// two groups of honest signers two different round-1 outputs of signer 5. Before sealed
/// messages were bound to the round-1 transcript, each group completed the DKG with its own
/// group key and `sk`: two vault addresses, the user paying one that half the signers never
/// watch. Now honest signers cannot open each other's messages, and all of them refuse.
#[test]
fn equivocated_round1_creates_no_vault() {
    let (cfg, mut nodes) = setup();
    let vault_id = open().vault_id;
    let mut pending = BTreeMap::new();
    let mut honest = vec![];
    for n in nodes.iter().take(4) {
        let (p, out) = dkg::round1(n.index, open()).unwrap();
        pending.insert(n.index, p);
        honest.push(out);
    }
    let (mut p5a, out5a) = dkg::round1(5, open()).unwrap();
    let (mut p5b, out5b) = dkg::round1(5, open()).unwrap();
    let list_a: Vec<Round1Out> = honest.iter().cloned().chain([out5a]).collect();
    let list_b: Vec<Round1Out> = honest.iter().cloned().chain([out5b]).collect();
    let in_group_a = |i: u16| i == 1 || i == 2;
    let view = |i: u16| if in_group_a(i) { list_a.clone() } else { list_b.clone() };

    let mut to: BTreeMap<u16, BTreeMap<u16, Sealed>> = BTreeMap::new();
    for n in nodes.iter().take(4) {
        let input = Round2In {
            vault_id,
            round1: view(n.index),
        };
        let out = dkg::round2(&cfg, &n.secret, n.index, pending.get_mut(&n.index).unwrap(), &input).unwrap();
        for (recipient, sealed) in out.sealed {
            to.entry(recipient).or_default().insert(n.index, sealed);
        }
    }
    let secret5 = &nodes[4].secret;
    for (p, list, group_a) in [(&mut p5a, &list_a, true), (&mut p5b, &list_b, false)] {
        let input = Round2In {
            vault_id,
            round1: list.clone(),
        };
        for (recipient, sealed) in dkg::round2(&cfg, secret5, 5, p, &input).unwrap().sealed {
            if in_group_a(recipient) == group_a {
                to.entry(recipient).or_default().insert(5, sealed);
            }
        }
    }

    for n in nodes.iter_mut().take(4) {
        let input = Round3In {
            vault_id,
            round1: view(n.index),
            sealed: to.remove(&n.index).unwrap_or_default(),
        };
        let res =
            dkg::round3(&cfg, &n.secret, &n.eth, n.index, pending.remove(&n.index).unwrap(), &input, &mut n.store);
        assert!(res.is_err(), "signer {} must refuse", n.index);
        assert!(n.store.vaults.is_empty());
    }
}

/// M-4: the round-1 transcript relayed in round 3 must be the one of round 2, and a signer's
/// own round-1 output must be relayed unchanged.
#[test]
fn round1_transcript_is_fixed_after_round2() {
    let (cfg, mut nodes) = setup();
    let vault_id = open().vault_id;
    let mut pending = BTreeMap::new();
    let mut r1: Vec<Round1Out> = vec![];
    for n in nodes.iter() {
        let (p, out) = dkg::round1(n.index, open()).unwrap();
        pending.insert(n.index, p);
        r1.push(out);
    }
    assert_eq!(r1[0].index, 1);

    // Round 2 with node 1's own output altered: refused (before any state is used up).
    let mut altered = r1.clone();
    altered[0].sk_commitment = B256::repeat_byte(0xee);
    let bad = Round2In {
        vault_id,
        round1: altered,
    };
    let err = dkg::round2(&cfg, &nodes[0].secret, 1, pending.get_mut(&1).unwrap(), &bad).unwrap_err();
    assert!(err.to_string().contains("altered"), "{err}");

    let r2_in = Round2In {
        vault_id,
        round1: r1.clone(),
    };
    let mut to: BTreeMap<u16, BTreeMap<u16, Sealed>> = BTreeMap::new();
    for n in nodes.iter() {
        let out = dkg::round2(&cfg, &n.secret, n.index, pending.get_mut(&n.index).unwrap(), &r2_in).unwrap();
        for (recipient, sealed) in out.sealed {
            to.entry(recipient).or_default().insert(n.index, sealed);
        }
    }
    // Round 3 for node 1 with its own (otherwise unused) round-1 entry altered: the old code
    // ignored it; the transcript no longer matches round 2's.
    let mut changed = r1.clone();
    changed[0].sk_commitment = B256::repeat_byte(0xee);
    let input = Round3In {
        vault_id,
        round1: changed,
        sealed: to.remove(&1).unwrap(),
    };
    let n1 = &mut nodes[0];
    let err =
        dkg::round3(&cfg, &n1.secret, &n1.eth, 1, pending.remove(&1).unwrap(), &input, &mut n1.store).unwrap_err();
    assert!(err.to_string().contains("transcript"), "{err}");
    assert!(n1.store.vaults.is_empty());
}
