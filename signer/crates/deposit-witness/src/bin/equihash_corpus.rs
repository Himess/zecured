//! RQ-10 / K4: a differential corpus for the relay's on-chain Equihash check.
//!
//! Takes the real Testnet headers of the relay test fixtures, changes their solutions (and a few
//! other header bytes) in ways that exercise every rule of the check, and labels every case
//! with the verdict of the `equihash` crate 0.3.0, the reference Zebra calls. The Foundry test
//! `ZcashEquihashCorpus` then requires the Solidity library to agree on every case. The random
//! choices come from a fixed seed: the same command always writes the same corpus.
//!
//!   cargo run -p zecured-deposit-witness --bin equihash-corpus -- \
//!     contracts/test/relay/fixtures/testnet_recent.json \
//!     contracts/test/relay/fixtures/testnet_nu7_activation.json \
//!     contracts/test/relay/fixtures/testnet_deposit_anchor.json \
//!     > contracts/test/relay/fixtures/equihash_corpus.json

use anyhow::{Context, Result, ensure};

const HEADER_LENGTH: usize = 1487;
const PREFIX: usize = 108; // what the reference calls the input: the header before the nonce
const NONCE: std::ops::Range<usize> = 108..140;
const SOLUTION: std::ops::Range<usize> = 143..HEADER_LENGTH;
const INDICES: usize = 512;
const BITS: usize = 21;

/// SplitMix64: a fixed, documented generator, so the corpus is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn indices(solution: &[u8]) -> Vec<u32> {
    (0..INDICES)
        .map(|i| {
            (0..BITS).fold(0u32, |acc, b| {
                let bit = i * BITS + b;
                (acc << 1) | u32::from((solution[bit / 8] >> (7 - bit % 8)) & 1)
            })
        })
        .collect()
}

fn minimal(indices: &[u32]) -> Vec<u8> {
    let mut out = vec![0u8; INDICES * BITS / 8];
    for (i, idx) in indices.iter().enumerate() {
        for b in 0..BITS {
            if (idx >> (BITS - 1 - b)) & 1 == 1 {
                let bit = i * BITS + b;
                out[bit / 8] |= 1 << (7 - bit % 8);
            }
        }
    }
    out
}

fn with_indices(header: &[u8], idx: &[u32]) -> Vec<u8> {
    let mut h = header.to_vec();
    h[SOLUTION].copy_from_slice(&minimal(idx));
    h
}

/// The reference's verdict: "Valid", or the kind of the first defect it reports.
fn reference(header: &[u8]) -> String {
    match equihash::is_valid_solution(200, 9, &header[..PREFIX], &header[NONCE], &header[SOLUTION]) {
        Ok(()) => "Valid".into(),
        // The crate exposes its error kind only through Display (equihash 0.3.0 verify.rs).
        Err(e) => match e.to_string().trim_start_matches("Invalid solution: ") {
            "invalid parameters" => "InvalidEncoding",
            "invalid collision length between StepRows" => "Collision",
            "Index tree incorrectly ordered" => "OutOfOrder",
            "duplicate indices" => "DuplicateIdxs",
            "root hash of tree is non-zero" => "NonZeroRootHash",
            other => panic!("unknown equihash error {other}"),
        }
        .into(),
    }
}

/// Swaps the two halves of the subtree of size 2^level that starts at `start` (an ordering
/// violation at that level, every hash unchanged).
fn swap_subtrees(idx: &mut [u32], start: usize, level: u32) {
    let half = 1usize << (level - 1);
    for i in 0..half {
        idx.swap(start + i, start + half + i);
    }
}

fn main() -> Result<()> {
    let mut headers: Vec<Vec<u8>> = vec![];
    for path in std::env::args().skip(1) {
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).with_context(|| path.clone())?)?;
        for raw in json["header_raws"].as_array().context("header_raws")? {
            let bytes = hex::decode(raw.as_str().context("hex")?.trim_start_matches("0x"))?;
            ensure!(bytes.len() == HEADER_LENGTH, "a header of {} bytes in {path}", bytes.len());
            headers.push(bytes);
        }
    }
    ensure!(!headers.is_empty(), "usage: equihash-corpus <relay fixture.json>...");

    let mut rng = Rng(0x5a65_6375_7265_6400); // "Zecured\0"
    let mut cases: Vec<(String, Vec<u8>)> = vec![];
    for h in &headers {
        let idx = indices(&h[SOLUTION]);
        ensure!(minimal(&idx) == h[SOLUTION], "index decoding does not round-trip");
        cases.push(("real".into(), h.clone()));

        let mut flipped = h.clone();
        let bit = rng.below(SOLUTION.len() * 8);
        flipped[SOLUTION.start + bit / 8] ^= 1 << (bit % 8);
        cases.push(("solution bit flip".into(), flipped));

        let mut swapped = idx.clone();
        let a = rng.below(INDICES);
        swapped.swap(a, (a + 1 + rng.below(INDICES - 1)) % INDICES);
        cases.push(("two indices swapped".into(), with_indices(h, &swapped)));

        let mut duplicated = idx.clone();
        let i = rng.below(INDICES);
        duplicated[(i + 1 + rng.below(INDICES - 1)) % INDICES] = duplicated[i];
        cases.push(("an index duplicated".into(), with_indices(h, &duplicated)));

        let mut bumped = idx.clone();
        let i = rng.below(INDICES);
        bumped[i] = (bumped[i] + 1) % (1 << BITS);
        cases.push(("an index plus one".into(), with_indices(h, &bumped)));

        let mut reordered = idx.clone();
        let level = 1 + rng.below(9) as u32;
        let size = 1usize << level;
        swap_subtrees(&mut reordered, rng.below(INDICES / size) * size, level);
        cases.push((format!("subtrees swapped at level {level}"), with_indices(h, &reordered)));

        let mut nonce = h.clone();
        nonce[NONCE.start + rng.below(32)] ^= 1 << rng.below(8);
        cases.push(("nonce bit flip".into(), nonce));

        let mut prefix = h.clone();
        prefix[rng.below(PREFIX)] ^= 1 << rng.below(8);
        cases.push(("header prefix bit flip".into(), prefix));
    }

    let raws: Vec<String> = cases.iter().map(|(_, h)| format!("0x{}", hex::encode(h))).collect();
    let kinds: Vec<String> = cases.iter().map(|(_, h)| reference(h)).collect();
    let mutations: Vec<&str> = cases.iter().map(|(m, _)| m.as_str()).collect();
    let valid = kinds.iter().filter(|k| *k == "Valid").count();
    let out = serde_json::json!({
        "description": "RQ-10 differential corpus: real Testnet headers of the relay fixtures, changed, each labelled by the equihash crate 0.3.0 (the reference Zebra calls). Generated by signer/crates/deposit-witness/src/bin/equihash_corpus.rs with a fixed seed.",
        "cases": cases.len(),
        "valid": valid,
        "mutations": mutations,
        "kinds": kinds,
        "raws": raws,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    eprintln!("{} cases, {} valid", cases.len(), valid);
    Ok(())
}
