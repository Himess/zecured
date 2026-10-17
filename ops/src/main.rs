//! `zecured-ops`: prepare, sign, collect and submit the signers' operator messages (O-3).
//!
//! ```text
//! zecured-ops prepare pause --until +6h --out pause.json        # an operator, with Base access
//! zecured-ops sign pause.json --identity signer/data/node-1/identity.json --out pause.1.json --yes
//! zecured-ops collect pause.json pause.1.json pause.2.json pause.3.json --out pause.bundle.json
//! zecured-ops submit pause.bundle.json --dry-run                 # simulate on Base
//! zecured-ops submit pause.bundle.json --sender-key-env ZECURED_OPS_SENDER_KEY
//! ```
//!
//! Every file holds public data only (messages, digests, addresses, signatures). Keys come from a
//! node's identity file or an environment variable and are never printed; nor is the RPC URL,
//! which may carry an API key.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, B256, FixedBytes},
    providers::ProviderBuilder,
};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Serialize, de::DeserializeOwned};
use zecured_base::{events::version_id, operator::VersionKind};
use zecured_ops::{
    Network, Request, SignatureFile, Spec, check_submission, collect, load_key, parse_branch, parse_time, prepare,
    scrub, send, sign, simulate,
};

#[derive(Parser)]
#[command(about = "Zecured operator messages: prepare, sign, collect and submit (O-3)")]
struct Cli {
    /// The network config: the Registry, the chain id, the Base RPC and the signer set.
    #[arg(
        long,
        env = "ZECURED_NETWORK_CONFIG",
        default_value = "signer/data/network.json",
        global = true
    )]
    config: PathBuf,
    /// Base RPC to use instead of the network config's.
    #[arg(long, env = "ZECURED_OPS_RPC", hide_env_values = true, global = true)]
    rpc: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a request file for the signers (reads the contract and its nonce from Base).
    Prepare {
        #[command(subcommand)]
        what: Prepare,
    },
    /// Show what a request or bundle asks to sign, and the signatures it holds.
    Show { file: PathBuf },
    /// Sign a request with one signer's attestation key (no Base access needed).
    Sign {
        file: PathBuf,
        /// The signer node's identity file (`eth_private_key`).
        #[arg(long, conflicts_with = "key_env")]
        identity: Option<PathBuf>,
        /// An environment variable holding the key instead.
        #[arg(long)]
        key_env: Option<String>,
        #[arg(long)]
        out: PathBuf,
        /// Sign: without it, only show what would be signed.
        #[arg(long)]
        yes: bool,
    },
    /// Merge signature files into a bundle (or into an earlier bundle).
    Collect {
        file: PathBuf,
        #[arg(required = true)]
        signatures: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a bundle against Base, simulate it, and send it.
    Submit {
        file: PathBuf,
        /// Simulate only.
        #[arg(long)]
        dry_run: bool,
        /// The sending key's identity file (any funded key; the signatures authorize the call).
        #[arg(long, conflicts_with = "sender_key_env")]
        sender_identity: Option<PathBuf>,
        /// An environment variable holding the sending key instead.
        #[arg(long)]
        sender_key_env: Option<String>,
    },
}

#[derive(Args)]
struct Common {
    /// Where to write the request.
    #[arg(long)]
    out: PathBuf,
    /// Seconds the signatures stay valid (the message's deadline).
    #[arg(long, default_value_t = 21_600)]
    ttl: u64,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Deposit,
    Violation,
    Swap,
}

impl From<Kind> for VersionKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Deposit => VersionKind::Deposit,
            Kind::Violation => VersionKind::Violation,
            Kind::Swap => VersionKind::Swap,
        }
    }
}

#[derive(Subcommand)]
enum Prepare {
    /// T-03: pause new minting (3 of 5).
    Pause {
        /// When the pause ends: Unix seconds or +N[s|m|h|d].
        #[arg(long)]
        until: String,
        #[command(flatten)]
        common: Common,
    },
    /// T-03: lift the pause (3 of 5).
    Unpause {
        #[command(flatten)]
        common: Common,
    },
    /// T-07a: schedule a verifier version (5 of 5).
    ScheduleVersion {
        #[arg(long, value_enum)]
        kind: Kind,
        /// The RISC Zero router selector (4 bytes, hex).
        #[arg(long)]
        selector: FixedBytes<4>,
        #[arg(long)]
        image_id: B256,
        /// Consensus branch ids the version accepts (deposits; up to 4), e.g. 0x77190ad9.
        #[arg(long = "branch")]
        branches: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// T-07a: remove a verifier version (3 of 5; 5 for the last working deposit version, R-23).
    RemoveVersion {
        #[arg(long, conflicts_with_all = ["kind", "selector", "image_id"])]
        version_id: Option<B256>,
        /// Or the version's kind, selector and image id.
        #[arg(long, value_enum, requires_all = ["selector", "image_id"])]
        kind: Option<Kind>,
        #[arg(long)]
        selector: Option<FixedBytes<4>>,
        #[arg(long)]
        image_id: Option<B256>,
        #[command(flatten)]
        common: Common,
    },
    /// T-05, R-6: the destination that reimburses the bonds for a claim they paid (5 of 5).
    ReimburseDestination {
        #[arg(long)]
        claim: B256,
        /// keccak256(salt || Ironwood receiver) of the destination, revealed to the signers first.
        #[arg(long)]
        commitment: B256,
        #[command(flatten)]
        common: Common,
    },
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?).with_context(|| format!("write {}", path.display()))
}

fn spec_of(what: &Prepare, now: u64) -> Result<(Spec, &Common)> {
    Ok(match what {
        Prepare::Pause { until, common } => (
            Spec::PauseMinting {
                until: parse_time(until, now)?,
            },
            common,
        ),
        Prepare::Unpause { common } => (Spec::UnpauseMinting, common),
        Prepare::ScheduleVersion {
            kind,
            selector,
            image_id,
            branches,
            common,
        } => {
            ensure!(branches.len() <= 4, "at most 4 branch ids");
            let mut ids = [0u32; 4];
            for (slot, b) in ids.iter_mut().zip(branches) {
                *slot = parse_branch(b)?;
            }
            (
                Spec::ScheduleVersion {
                    kind: (*kind).into(),
                    selector: *selector,
                    image_id: *image_id,
                    branches: ids,
                },
                common,
            )
        }
        Prepare::RemoveVersion {
            version_id: id,
            kind,
            selector,
            image_id,
            common,
        } => {
            let version_id = match (id, kind, selector, image_id) {
                (Some(id), ..) => *id,
                (None, Some(kind), Some(selector), Some(image_id)) => {
                    version_id(VersionKind::from(*kind).code(), selector.0, *image_id)
                }
                _ => bail!("give --version-id, or --kind, --selector and --image-id"),
            };
            (Spec::RemoveVersion { version_id }, common)
        }
        Prepare::ReimburseDestination {
            claim,
            commitment,
            common,
        } => (
            Spec::ReimburseDestination {
                claim_id: *claim,
                commitment: *commitment,
            },
            common,
        ),
    })
}

fn show(request: &Request, signers: &[Address]) {
    println!("{}", request.describe());
    println!(
        "  {} signature(s); the contract needs {} at least{}",
        request.signatures.len(),
        request.message.threshold(),
        if matches!(request.message, zecured_base::operator::OperatorMessage::RemoveVersion { .. }) {
            " (5 for the last working deposit version)"
        } else {
            ""
        }
    );
    for s in &request.signatures {
        let known = if signers.contains(&s.signer) {
            ""
        } else {
            "  (not in the network config's signer set)"
        };
        println!("    {}{known}", s.signer);
    }
}

async fn run(cli: Cli, rpc: &str) -> Result<()> {
    let net = || Network::load(&cli.config);
    match cli.cmd {
        Cmd::Prepare { what } => {
            let net = net()?;
            let (spec, common) = spec_of(&what, now())?;
            let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
            let prepared = prepare(&provider, &net, &spec, common.ttl, now()).await?;
            write_json(&common.out, &prepared.request)?;
            println!("{}", prepared.request.describe());
            println!("  needs {} signer signatures; written to {}", prepared.needed, common.out.display());
        }
        Cmd::Show { file } => {
            let request: Request = read_json(&file)?;
            request.checked_digest()?;
            let signers = net().map(|n| n.signers()).unwrap_or_default();
            show(&request, &signers);
        }
        Cmd::Sign {
            file,
            identity,
            key_env,
            out,
            yes,
        } => {
            let request: Request = read_json(&file)?;
            request.checked_digest()?;
            let key = load_key(identity.as_deref(), key_env.as_deref())?;
            println!("{}", request.describe());
            println!("  signer {}", key.address());
            if let Ok(net) = net() {
                ensure!(
                    net.chain_id == request.chain_id,
                    "the request is for chain {}, the network config for chain {}",
                    request.chain_id,
                    net.chain_id
                );
                if !net.signers().contains(&key.address()) {
                    eprintln!("warning: this key is not in the network config's signer set");
                }
            }
            ensure!(yes, "nothing signed: check the message above, then run again with --yes");
            write_json(&out, &sign(&request, &key, now())?)?;
            println!("signed: {}", out.display());
        }
        Cmd::Collect { file, signatures, out } => {
            let request: Request = read_json(&file)?;
            let files: Vec<SignatureFile> = signatures.iter().map(|p| read_json(p)).collect::<Result<_>>()?;
            let net = net()?;
            ensure!(net.nodes.len() == 5, "the network config must list the 5 signer nodes");
            let bundle = collect(&request, &files, &net.signers(), now())?;
            write_json(&out, &bundle)?;
            show(&bundle, &net.signers());
            if bundle.signatures.len() < bundle.message.threshold() {
                println!("  not enough yet: collect more into {} before submitting", out.display());
            }
        }
        Cmd::Submit {
            file,
            dry_run,
            sender_identity,
            sender_key_env,
        } => {
            let bundle: Request = read_json(&file)?;
            let net = net()?;
            let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
            let checked = check_submission(&provider, net.registry, &bundle, now()).await?;
            let sender = match (&sender_identity, &sender_key_env) {
                (None, None) if dry_run => None,
                (identity, env) => Some(load_key(identity.as_deref(), env.as_deref())?),
            };
            let from = sender.as_ref().map(|k| k.address()).unwrap_or_default();
            simulate(&provider, &checked, from).await?;
            println!("{}", bundle.message.describe());
            println!(
                "  {} of the {} signatures needed, checked against the Registry's signers; the simulation passed",
                checked.signatures, checked.needed
            );
            match sender {
                Some(key) if !dry_run => {
                    let tx = send(rpc, &checked, key).await?;
                    println!("sent: transaction {tx}");
                }
                _ => println!("dry run: nothing sent"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// The command line is well formed, and a version is named by its id or by its parts.
    #[test]
    fn the_command_line_parses() {
        Cli::command().debug_assert();
        let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("zecured-ops").chain(args.iter().copied()));
        assert!(parse(&["prepare", "pause", "--until", "+6h", "--out", "p.json"]).is_ok());
        assert!(
            parse(&[
                "prepare",
                "remove-version",
                "--version-id",
                &format!("0x{}", "11".repeat(32)),
                "--out",
                "r.json"
            ])
            .is_ok()
        );
        let by_parts = [
            "prepare",
            "remove-version",
            "--kind",
            "swap",
            "--selector",
            "0x73c457ba",
            "--image-id",
            &format!("0x{}", "22".repeat(32)),
            "--out",
            "r.json",
        ];
        let Cmd::Prepare { what } = parse(&by_parts).unwrap().cmd else {
            panic!()
        };
        let (spec, _) = spec_of(&what, 0).unwrap();
        let id = version_id(VersionKind::Swap.code(), [0x73, 0xc4, 0x57, 0xba], B256::repeat_byte(0x22));
        assert_eq!(spec, Spec::RemoveVersion { version_id: id });
        assert!(
            parse(&["prepare", "remove-version", "--kind", "swap", "--out", "r.json"]).is_err(),
            "needs its parts"
        );
        let schedule = [
            "prepare",
            "schedule-version",
            "--kind",
            "deposit",
            "--selector",
            "0x73c457ba",
            "--image-id",
            &format!("0x{}", "22".repeat(32)),
            "--branch",
            "0x77190ad9",
            "--out",
            "s.json",
        ];
        let Cmd::Prepare { what } = parse(&schedule).unwrap().cmd else {
            panic!()
        };
        let (spec, common) = spec_of(&what, 0).unwrap();
        assert_eq!(common.ttl, 21_600);
        assert!(matches!(
            spec,
            Spec::ScheduleVersion {
                branches: [0x7719_0ad9, 0, 0, 0],
                ..
            }
        ));
        assert!(
            parse(&[
                "sign",
                "r.json",
                "--identity",
                "i.json",
                "--key-env",
                "K",
                "--out",
                "s.json"
            ])
            .is_err()
        );
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let rpc = match &cli.rpc {
        Some(rpc) => rpc.clone(),
        None => Network::load(&cli.config).map(|n| n.base_rpc_url).unwrap_or_default(),
    };
    match run(cli, &rpc).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", scrub(&format!("{e:#}"), &rpc));
            ExitCode::FAILURE
        }
    }
}
