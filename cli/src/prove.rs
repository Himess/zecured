//! Tier 2 (docs/TIER2.md): the borrower proves their own deposit and activates the vault with
//! the proof. Nothing here needs the signers or the coordinator to cooperate: the vault's
//! `VaultOpened` signatures were kept when the vault opened, the viewing key was checked against
//! them, the chain data is public, and the Registry trusts only the proof.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use zcash_protocol::TxId;
use zecured_base::{events::IZecuredRegistry, gas_with_headroom, relay::anchor_above};
use zecured_deposit_witness::{p2p, witness};
use zecured_zcash_ops::lwd;

use crate::{LoanFile, NetworkParams};

pub struct Paths {
    pub host: PathBuf,
    pub headers: PathBuf,
    pub summaries: PathBuf,
    /// Owner-only directory for the witness (it holds the viewing key) and the seal.
    pub work: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SealFile {
    image_id: B256,
    journal: Bytes,
    seal: Bytes,
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

fn host(paths: &Paths, command: &str, args: &[&Path]) -> Result<()> {
    let status = Command::new(&paths.host).arg(command).args(args).status().context("run the zkVM host")?;
    ensure!(status.success(), "zkVM host {command} failed");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn prove(
    l: &LoanFile,
    ufvk: &str,
    net: &NetworkParams,
    paths: &Paths,
    mock: bool,
    txid: &str,
    lwd_url: &str,
    rpc: &str,
) -> Result<()> {
    let expiry_height = l.expiry_height.context("this loan file has no expiry height (opened before Tier 2)")?;
    ensure!(l.opened_signatures.len() == 5, "this loan file has no VaultOpened signatures (opened before C-4)");
    let txid = TxId::from_hex(txid).context("deposit txid")?;
    let mut client = lwd::connect(lwd_url).await?;
    let (_, height) = lwd::raw_transaction(&mut client, &txid).await?;
    let height = u32::try_from(height)?;

    let provider = ProviderBuilder::new().connect_http(rpc.parse()?);
    let anchor = anchor_above(&provider, net.registry, height)
        .await?
        .context("the header relay has not reached the deposit with enough work on top yet; try again later")?;
    let headers = paths.headers.clone();
    let tip = tokio::task::spawn_blocking(move || p2p::extend_file(&headers, &p2p::seed_peers())).await??;
    ensure!(tip >= anchor, "the P2P peers are behind the relay");
    eprintln!("deposit at {height}, anchored at {anchor}: building the witness");

    let vault = witness::Vault {
        vault_id: l.vault_id.0,
        owner: l.owner_key.parse::<PrivateKeySigner>()?.address().0.0,
        account: l.account.0.0,
        address: l.address.clone(),
        ufvk: ufvk.to_owned(),
        expiry_height,
        amount: l.amount,
    };
    let built = witness::build(&mut client, &paths.headers, &paths.summaries, txid, &vault, anchor).await?;
    fs::create_dir_all(&paths.work)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&paths.work, fs::Permissions::from_mode(0o700))?;
    }
    let witness_path = paths.work.join("witness.json");
    let seal_path = paths.work.join("seal.json");
    write_private(&witness_path, &serde_json::to_vec(&built.witness)?)?;
    let sealed = if mock {
        host(paths, "mock-seal", &[&witness_path, &seal_path])
    } else {
        let receipt = paths.work.join("receipt.bin");
        host(paths, "prove", &[&witness_path, &receipt]).and_then(|()| host(paths, "groth16", &[&receipt, &seal_path]))
    };
    let _ = fs::remove_file(&witness_path);
    sealed?;
    let s: SealFile = serde_json::from_slice(&fs::read(&seal_path)?)?;

    // TEST ONLY: the owner EOA submits (anyone may: the proof is all the Registry trusts).
    let key: PrivateKeySigner = l.owner_key.parse()?;
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    let registry = IZecuredRegistry::new(net.registry, &p);
    ensure!(registry.depositImageId().call().await? == s.image_id, "the Registry expects another deposit guest");
    let mut sigs: Vec<(Address, Bytes)> = l
        .opened_signatures
        .iter()
        .map(|(index, sig)| {
            let eth = net.nodes.iter().find(|n| n.index == *index).context("signer not in the network config")?.eth;
            Ok((eth, sig.clone()))
        })
        .collect::<Result<_>>()?;
    sigs.sort_by_key(|(a, _)| *a);
    let call = registry.attestDepositWithProof(s.journal, s.seal, sigs.into_iter().map(|(_, s)| s).collect());
    let gas = gas_with_headroom(call.estimate_gas().await.context("estimate attestDepositWithProof")?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "attestDepositWithProof reverted");
    println!(
        "vault activated on a deposit proof{} (tx {})",
        if mock { " (MOCK seal)" } else { "" },
        r.transaction_hash
    );
    Ok(())
}
