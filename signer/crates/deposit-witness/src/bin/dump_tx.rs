//! Writes a mined transaction's raw bytes (hex) and its height, from the light wallet server.
//!
//!   cargo run -p zecured-deposit-witness --bin dump-tx -- <txid> > tx.json

use anyhow::{Context, Result};
use zcash_protocol::TxId;
use zecured_zcash_ops::lwd;

#[tokio::main]
async fn main() -> Result<()> {
    let txid = std::env::args().nth(1).context("usage: dump-tx <txid>")?;
    let txid = TxId::from_hex(&txid).context("txid hex (display order)")?;
    let mut client = lwd::connect("https://testnet.zec.rocks:443").await?;
    let (raw, height) = lwd::raw_transaction(&mut client, &txid).await?;
    println!("{}", serde_json::json!({ "txid": txid.to_string(), "height": height, "raw": hex::encode(raw) }));
    Ok(())
}
