//! Tier 2 (docs/TIER2.md): the borrower proves their own deposit and activates the vault with
//! the proof. Nothing here needs the signers or the coordinator to cooperate: the vault's
//! `VaultOpened` signatures were kept when the vault opened, the viewing key was checked against
//! them, the chain data is public, and the Intake trusts only the proof.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use alloy::{
    eips::BlockNumberOrTag,
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use zcash_protocol::TxId;
use zecured_base::{
    events::{IZcashHeaderRelay, IZecuredIntake, IZecuredRegistry, IZecuredVersions, VERSION_DEPOSIT, version_id},
    gas_with_headroom,
    relay::deposit_anchor,
};
use zecured_deposit_witness::{
    p2p,
    witness::{self, Mode},
};
use zecured_zcash_ops::lwd;

use crate::{LoanFile, NetworkParams, lwd_connect, zcash_network};

pub struct Paths {
    pub host: PathBuf,
    pub headers: PathBuf,
    pub summaries: PathBuf,
    /// Owner-only directory for the witness (it holds the vault's tag key) and the seal.
    pub work: PathBuf,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SealFile {
    pub image_id: B256,
    pub journal: Bytes,
    pub seal: Bytes,
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

/// The Intake the Registry names (Registry v3: deposit proofs go there).
pub async fn intake_of<P: Provider>(provider: &P, registry: Address) -> Result<Address> {
    IZecuredRegistry::new(registry, provider).intake().call().await.context("Registry.intake")
}

/// Builds the witness on this machine and proves it (or makes a MOCK seal): the seal file the
/// Intake takes, for the deposit `txid` of the loan `l`.
#[allow(clippy::too_many_arguments)]
pub async fn make_seal(
    l: &LoanFile,
    ufvk: &str,
    net: &NetworkParams,
    paths: &Paths,
    mock: bool,
    txid: &str,
    lwd_url: &str,
    rpc: &str,
) -> Result<SealFile> {
    let expiry_height = l.expiry_height.context("this loan file has no expiry height (opened before Tier 2)")?;
    ensure!(l.opened_signatures.len() == 5, "this loan file has no VaultOpened signatures (opened before C-4)");
    // The deposit guest computes VaultOpened v2 (`tagKeyHash`); v1 signatures (`ufvkHash`) never match it.
    ensure!(
        l.tag_key_hash.is_some(),
        "this loan's vault was opened under VaultOpened v1, which the deposit guest no longer proves"
    );
    let txid = TxId::from_hex(txid).context("deposit txid")?;
    let mut client = lwd_connect(lwd_url).await?;
    let (_, height) = lwd::raw_transaction(&mut client, &txid).await?;
    let height = u32::try_from(height)?;

    let provider = ProviderBuilder::new().connect_http(rpc.parse()?);
    let intake = intake_of(&provider, net.registry).await?;
    let anchor = deposit_anchor(&provider, intake, height, l.amount)
        .await?
        .context("the header relay has not reached the deposit with enough work on top yet; try again later")?;
    let (headers, network) = (paths.headers.clone(), zcash_network());
    let tip =
        tokio::task::spawn_blocking(move || p2p::extend_file(network, &headers, &p2p::seed_peers(network))).await??;
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
    let built = witness::build(
        &mut client,
        network,
        &paths.headers,
        &paths.summaries,
        txid,
        &vault,
        Mode::Activate,
        None,
        anchor,
    )
    .await?;
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
    let _ = fs::remove_file(&seal_path);
    Ok(s)
}

/// TEST ONLY: submits the proof with the owner EOA, which pays the Base gas, and finalizes the
/// activation itself (anyone may do both: the proof is all the Intake trusts). Without
/// `--self-submit`, the CLI hands the seal to the backend instead (Y10).
pub async fn submit_with_owner(l: &LoanFile, net: &NetworkParams, s: SealFile, mock: bool, rpc: &str) -> Result<()> {
    let key: PrivateKeySigner = l.owner_key.parse()?;
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    let intake = IZecuredIntake::new(intake_of(&p, net.registry).await?, &p);
    // T-07a: the Intake takes a seal only from an active version (guest image id and verifier).
    let versions = IZecuredVersions::new(intake.versions().call().await.context("Intake.versions")?, &p);
    let selector: [u8; 4] = s.seal.get(..4).and_then(|b| b.try_into().ok()).context("seal without a selector")?;
    ensure!(
        versions.isActive(version_id(VERSION_DEPOSIT, selector, s.image_id)).call().await?,
        "Base takes no deposit proof of this guest and verifier (not an active version, T-07a)"
    );
    let mut sigs: Vec<(Address, Bytes)> = l
        .opened_signatures
        .iter()
        .map(|(index, sig)| {
            let eth = net.nodes.iter().find(|n| n.index == *index).context("signer not in the network config")?.eth;
            Ok((eth, sig.clone()))
        })
        .collect::<Result<_>>()?;
    sigs.sort_by_key(|(a, _)| *a);
    let call = intake.attestDepositWithProof(s.journal, s.seal, s.image_id, sigs.into_iter().map(|(_, s)| s).collect());
    let gas = gas_with_headroom(call.estimate_gas().await.context("estimate attestDepositWithProof")?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "attestDepositWithProof reverted");
    println!(
        "deposit proof accepted{} (tx {}); the activation waits for the relay to move on (K3)",
        if mock { " (MOCK seal)" } else { "" },
        r.transaction_hash
    );

    // K3: anyone may finalize once the Intake allows it; the borrower does not need the
    // coordinator for that either.
    let relay = IZcashHeaderRelay::new(intake.relay().call().await?, &p);
    loop {
        let pending = intake.pendingActivation(l.vault_id).call().await?;
        if pending.account == Address::ZERO {
            println!("the activation is no longer pending (finalized or dropped by someone else)");
            return Ok(());
        }
        ensure!(
            relay.isInBestChain(pending.anchor).call().await?,
            "the proof's anchor left the relay's best chain: drop the pending activation and prove again"
        );
        let tip = relay.bestChainTip().call().await?;
        let now = p
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("no latest Base block")?
            .header
            .timestamp;
        if tip.height >= pending.readyHeight && U256::from(now) >= pending.readyTime {
            break;
        }
        eprintln!("relay at {} of {}; waiting", tip.height, pending.readyHeight);
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
    let call = intake.finalizeActivation(l.vault_id);
    let gas = gas_with_headroom(call.estimate_gas().await.context("estimate finalizeActivation")?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "finalizeActivation reverted");
    println!("vault activated on its deposit proof (tx {})", r.transaction_hash);
    Ok(())
}
