//! P-6, a leak scanner. Base must hold no Zcash identifier of a user: no transaction id, note commitment, nullifier,
//! address or viewing key; recipients appear only as salted commitments (CLAUDE.md). This tool gathers, in memory
//! only, the identifiers of our own runs — every note of the test wallets and of the coordinator's vaults (transaction
//! ids in both byte orders, the spending transactions' ids, note commitments, nullifiers, raw Ironwood receivers), and
//! each vault's viewing key, tag key and address — then searches every event, every calldata and, on a local fork,
//! every storage word of the deployment's contracts since their deployment block.
//!
//!   zecured-leak-scan --deployment contracts/deployments/anvil.json --rpc http://127.0.0.1:8545 \
//!       --wallet data/wallet/wallet.json --coordinator signer/data/coordinator --anvil-state --blocks
//!
//! It prints counts only. A match is reported by the identifier's kind and where it was found, never by its value,
//! and the tool exits with status 1. Errors never echo the RPC URL (it may carry a provider key).

use std::{collections::BTreeSet, fs, io::Read, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use serde_json::{Value, json};
use zcash_keys::keys::UnifiedFullViewingKey;
use zecured_policy::derive::tag_key;
use zecured_zcash_ops::{
    keys::ironwood_receiver,
    lwd,
    network::ZcashNetwork,
    scan::{ReceivedNote, ScanState},
};

#[derive(Parser)]
#[command(about = "Searches a deployment's events, calldata and storage for the Zcash identifiers of our own runs")]
struct Args {
    /// Deployment record(s) (`contracts/deployments/<name>.json`): the contracts and their deployment block.
    #[arg(long, required = true)]
    deployment: Vec<PathBuf>,
    /// Base JSON-RPC endpoint.
    #[arg(long, env = "BASE_SEPOLIA_RPC_URL", hide_env_values = true)]
    rpc: String,
    /// Light wallet server for scanning the notes.
    #[arg(long, env = "LIGHTWALLETD_URL", default_value = "https://testnet.zec.rocks:443")]
    lwd: String,
    /// CLI wallet file(s) (`data/wallet/wallet.json`).
    #[arg(long)]
    wallet: Vec<PathBuf>,
    /// The coordinator's data directory: every vault in `vaults/*.json`.
    #[arg(long)]
    coordinator: Option<PathBuf>,
    /// Zcash Mainnet keys and addresses (default Testnet).
    #[arg(long)]
    mainnet: bool,
    /// Blocks per `eth_getLogs` request.
    #[arg(long, default_value_t = 2_000)]
    chunk: u64,
    /// Also read every block for transactions to the contracts that emitted no event.
    #[arg(long)]
    blocks: bool,
    /// Also search the contracts' storage (`anvil_dumpState`: a local fork only).
    #[arg(long)]
    anvil_state: bool,
}

/// The identifiers searched for: (kind, bytes). Never printed.
#[derive(Default)]
struct Needles(Vec<(&'static str, Vec<u8>)>);

impl Needles {
    fn add(&mut self, kind: &'static str, bytes: &[u8]) {
        if bytes.len() >= 16 && !self.0.iter().any(|(_, b)| b.as_slice() == bytes) {
            self.0.push((kind, bytes.to_vec()));
        }
    }

    /// A 32-byte value in both byte orders (Zcash shows transaction ids reversed).
    fn add_both(&mut self, kind: &'static str, bytes: &[u8; 32]) {
        self.add(kind, bytes);
        let mut reversed = *bytes;
        reversed.reverse();
        self.add(kind, &reversed);
    }

    fn notes(&mut self, notes: &[ReceivedNote]) {
        for n in notes {
            self.add_both("transaction id", n.txid.as_ref());
            if let Some(spent) = &n.spent_in {
                self.add_both("transaction id", spent.as_ref());
            }
            let cmx = orchard::note::ExtractedNoteCommitment::from(n.note.commitment()).to_bytes();
            self.add_both("note commitment", &cmx);
            self.add_both("nullifier", &n.nullifier.to_bytes());
            self.add("receiver", &n.recipient.to_raw_address_bytes());
        }
    }

    /// The first needle found in `hay`, by kind.
    fn find(&self, hay: &[u8]) -> Option<&'static str> {
        self.0
            .iter()
            .find(|(_, n)| hay.len() >= n.len() && hay.windows(n.len()).any(|w| w == n.as_slice()))
            .map(|(k, _)| *k)
    }
}

/// The notes of `fvk` from `birthday` on, from a light wallet server that follows `network` (a
/// server of the other network would find none, and the scan would miss every note value).
async fn scan_notes(
    lwd_url: &str,
    network: ZcashNetwork,
    fvk: FullViewingKey,
    birthday: u32,
) -> Result<Vec<ReceivedNote>> {
    let mut client = lwd::connect(lwd_url).await?;
    lwd::check_network(&mut client, network).await?;
    let tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
    let mut state = ScanState::from_birthday(&mut client, fvk, birthday).await?;
    state.scan_to(&mut client, tip).await?;
    Ok(state.notes)
}

/// A minimal JSON-RPC client; its errors name the method only.
struct Rpc {
    url: String,
    http: reqwest::Client,
}

impl Rpc {
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let reply: Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|_| anyhow!("{method}: the RPC endpoint did not answer"))?
            .json()
            .await
            .map_err(|_| anyhow!("{method}: not a JSON-RPC answer"))?;
        if let Some(e) = reply.get("error") {
            bail!("{method}: {}", e.get("message").and_then(Value::as_str).unwrap_or("error"));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn quantity(v: &Value) -> Result<u64> {
    let s = v.as_str().context("not a quantity")?;
    Ok(u64::from_str_radix(s.trim_start_matches("0x"), 16)?)
}

/// Hex data or a quantity (`anvil_dumpState` writes slots and values as quantities: `0x2`).
fn hex_bytes(v: &Value) -> Vec<u8> {
    let Some(s) = v.as_str().map(|s| s.trim_start_matches("0x")) else {
        return Vec::new();
    };
    let padded = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_owned()
    };
    hex::decode(padded).unwrap_or_default()
}

/// An event as one byte string: its topics, then its data.
fn log_blob(log: &Value) -> Vec<u8> {
    let mut blob = Vec::new();
    for t in log["topics"].as_array().into_iter().flatten() {
        blob.extend(hex_bytes(t));
    }
    blob.extend(hex_bytes(&log["data"]));
    blob
}

/// An account's storage from `anvil_dumpState` as (slot, value) words of 32 bytes each, in slot order, so that a
/// value spread over consecutive slots stays contiguous when the words are concatenated.
fn storage_words(storage: &Value) -> Vec<([u8; 32], [u8; 32])> {
    let word = |v: &Value| {
        let raw = hex_bytes(v);
        let mut w = [0u8; 32];
        let n = raw.len().min(32);
        w[32 - n..].copy_from_slice(&raw[raw.len() - n..]);
        w
    };
    let mut words: Vec<_> = storage
        .as_object()
        .into_iter()
        .flatten()
        .map(|(slot, value)| (word(&Value::String(slot.clone())), word(value)))
        .collect();
    words.sort();
    words
}

#[derive(Default)]
struct Report {
    logs: usize,
    transactions: usize,
    storage_words: usize,
    matches: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let zcash = if args.mainnet {
        ZcashNetwork::Mainnet
    } else {
        ZcashNetwork::Testnet
    };
    let network = zcash.params();
    let mut needles = Needles::default();
    let (mut wallets, mut vaults) = (0, 0);

    for path in &args.wallet {
        let file: Value = serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
        let key: [u8; 32] = hex::decode(file["spending_key"].as_str().context("wallet spending_key")?)?
            .try_into()
            .map_err(|_| anyhow!("wallet spending_key is not 32 bytes"))?;
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes(key)).context("invalid spending key")?;
        let fvk = FullViewingKey::from(&sk);
        needles.add("receiver", &fvk.address_at(0u32, Scope::External).to_raw_address_bytes());
        let birthday = u32::try_from(file["birthday"].as_u64().context("wallet birthday")?)?;
        needles.notes(&scan_notes(&args.lwd, zcash, fvk, birthday).await?);
        wallets += 1;
    }

    if let Some(dir) = &args.coordinator {
        for entry in fs::read_dir(dir.join("vaults")).context("the coordinator's vaults directory")? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let v: Value = serde_json::from_slice(&fs::read(&path)?)?;
            let Some(ufvk) = v["ufvk"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            needles.add("viewing key", ufvk.as_bytes());
            needles.add_both("tag key", &tag_key(ufvk));
            if let Some(address) = v["address"].as_str().filter(|s| !s.is_empty()) {
                needles.add("vault address", address.as_bytes());
                needles.add("receiver", &ironwood_receiver(&network, address)?.to_raw_address_bytes());
            }
            let fvk = UnifiedFullViewingKey::decode(&network, ufvk)
                .map_err(|_| anyhow!("a vault's viewing key does not decode"))?
                .orchard()
                .cloned()
                .context("a vault's viewing key has no Orchard-protocol key")?;
            let birthday = u32::try_from(v["birthday"].as_u64().context("vault birthday")?)?;
            needles.notes(&scan_notes(&args.lwd, zcash, fvk, birthday).await?);
            vaults += 1;
        }
    }
    println!(
        "identifiers: {} (from {wallets} wallet(s) and {vaults} vault(s)), held in memory only",
        needles.0.len()
    );

    let rpc = Rpc {
        url: args.rpc.clone(),
        http: reqwest::Client::new(),
    };
    let latest = quantity(&rpc.call("eth_blockNumber", json!([])).await?)?;
    let mut report = Report::default();
    for path in &args.deployment {
        let d: Value = serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
        let addresses: BTreeSet<String> = d["contracts"]
            .as_object()
            .context("deployment record without contracts")?
            .values()
            .filter_map(|a| a.as_str())
            .filter(|a| a.len() == 42 && a.starts_with("0x"))
            .map(str::to_lowercase)
            .collect();
        if addresses.is_empty() {
            bail!("{}: no contract addresses", path.display());
        }
        let from = d["block"].as_u64().context("deployment record without its block")?;
        let address_list: Vec<&String> = addresses.iter().collect();
        let mut transactions = BTreeSet::new();

        let mut start = from;
        while start <= latest {
            let end = (start + args.chunk - 1).min(latest);
            let logs = rpc
                .call(
                    "eth_getLogs",
                    json!([{ "fromBlock": format!("0x{start:x}"), "toBlock": format!("0x{end:x}"), "address": address_list }]),
                )
                .await?;
            for log in logs.as_array().into_iter().flatten() {
                let tx = log["transactionHash"].as_str().unwrap_or_default().to_owned();
                if let Some(kind) = needles.find(&log_blob(log)) {
                    report.matches.push(format!("{kind} in an event of transaction {tx}"));
                }
                transactions.insert(tx);
                report.logs += 1;
            }
            if args.blocks {
                for b in start..=end {
                    let block = rpc.call("eth_getBlockByNumber", json!([format!("0x{b:x}"), true])).await?;
                    for tx in block["transactions"].as_array().into_iter().flatten() {
                        let to = tx["to"].as_str().map(str::to_lowercase).unwrap_or_default();
                        if addresses.contains(&to) {
                            transactions.insert(tx["hash"].as_str().unwrap_or_default().to_owned());
                        }
                    }
                }
            }
            start = end + 1;
        }

        for hash in &transactions {
            let tx = rpc.call("eth_getTransactionByHash", json!([hash])).await?;
            if let Some(kind) = needles.find(&hex_bytes(&tx["input"])) {
                report.matches.push(format!("{kind} in the calldata of transaction {hash}"));
            }
            report.transactions += 1;
        }

        if args.anvil_state {
            let dump = hex_bytes(&rpc.call("anvil_dumpState", json!([])).await?);
            let mut text = String::new();
            flate2::read::GzDecoder::new(dump.as_slice())
                .read_to_string(&mut text)
                .map_err(|_| anyhow!("anvil_dumpState: not a gzip state dump"))?;
            let state: Value = serde_json::from_str(&text)?;
            for (address, account) in state["accounts"].as_object().into_iter().flatten() {
                if !addresses.contains(&address.to_lowercase()) {
                    continue;
                }
                let mut blob = Vec::new();
                for (slot, value) in storage_words(&account["storage"]) {
                    if let Some(kind) = needles.find(&slot) {
                        report.matches.push(format!("{kind} in a storage slot key of {address}"));
                    }
                    blob.extend(value);
                    report.storage_words += 1;
                }
                if let Some(kind) = needles.find(&blob) {
                    report.matches.push(format!("{kind} in the storage of {address}"));
                }
            }
        }
    }

    println!(
        "scanned: {} events, {} transactions' calldata, {} storage words",
        report.logs, report.transactions, report.storage_words
    );
    if report.matches.is_empty() {
        println!("matches: 0");
        Ok(())
    } else {
        println!("matches: {}", report.matches.len());
        for m in &report.matches {
            println!("  {m}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes32(seed: u8) -> [u8; 32] {
        std::array::from_fn(|i| seed.wrapping_add(i as u8))
    }

    #[test]
    fn a_value_is_found_in_either_byte_order() {
        let mut needles = Needles::default();
        let id = bytes32(7);
        needles.add_both("transaction id", &id);
        let mut reversed = id;
        reversed.reverse();
        for value in [id, reversed] {
            let mut hay = vec![0xaa; 5];
            hay.extend(value);
            hay.extend([0xbb; 9]);
            assert_eq!(needles.find(&hay), Some("transaction id"));
        }
        assert_eq!(needles.find(&id[..31]), None);
        assert_eq!(needles.find(&[0u8; 64]), None);
    }

    #[test]
    fn short_or_repeated_values_are_not_searched() {
        let mut needles = Needles::default();
        needles.add("receiver", &[1u8; 15]);
        assert!(needles.0.is_empty());
        needles.add("receiver", &[1u8; 43]);
        needles.add("vault address", &[1u8; 43]);
        assert_eq!(needles.0.len(), 1);
    }

    #[test]
    fn an_event_is_its_topics_then_its_data() {
        let (topic, data) = (bytes32(1), bytes32(100));
        let log =
            json!({ "topics": [format!("0x{}", hex::encode(topic))], "data": format!("0x{}", hex::encode(data)) });
        assert_eq!(log_blob(&log), [topic, data].concat());
    }

    #[test]
    fn storage_words_are_padded_and_in_slot_order() {
        // Slots 1 and 2 hold a 43-byte receiver across their boundary; listed out of order and with a short key.
        let value = [bytes32(50), bytes32(82)].concat();
        let storage = json!({
            "0x10": "0x01",
            "0x2": format!("0x{}", hex::encode(&value[32..])),
            "0x0000000000000000000000000000000000000000000000000000000000000001": format!("0x{}", hex::encode(&value[..32])),
        });
        let words = storage_words(&storage);
        assert_eq!(words.iter().map(|(slot, _)| slot[31]).collect::<Vec<_>>(), vec![1, 2, 16]);
        assert_eq!(words[2].1[31], 1);
        assert!(words[2].1[..31].iter().all(|b| *b == 0));

        let mut needles = Needles::default();
        needles.add("receiver", &value[20..63]);
        let blob: Vec<u8> = words.iter().flat_map(|(_, v)| v.to_vec()).collect();
        assert_eq!(needles.find(&blob), Some("receiver"));
    }
}
