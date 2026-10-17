//! Mainnet beta (section 6): vault openings gated by invite codes, and a switch that closes them.
//!
//! The operator's file (`--invites`, `ZECURED_INVITES`) lists the keccak256 hashes of the codes, each
//! with a number of uses, and two switches:
//!
//! ```json
//! { "openings": true, "inviteOnly": true,
//!   "codes": [ { "hash": "0x<keccak256 of the code's UTF-8 bytes>", "uses": 1 } ] }
//! ```
//!
//! - `openings: false` closes every opening (`openings_closed`); exits and payouts never depend on
//!   it.
//! - `inviteOnly: true` needs a code whose hash is listed with a use left (`invite_required` for a
//!   missing or unknown code, `invite_used` once its uses are taken).
//!
//! The file is read again on every opening, so the switches and the list change without a restart.
//! Uses are counted in the coordinator's data directory (`invites-used.json`): a use is taken
//! while its vault opens and kept once it has opened; a failed opening gives it back. No file: no
//! gating (the judge deployment). Codes and their hashes are never logged.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use alloy::primitives::{B256, keccak256};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zecured_node::store::write_atomic;

use crate::error::PublicError;

/// One invite code of the operator's file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteCode {
    /// keccak256 of the code's UTF-8 bytes.
    pub hash: B256,
    /// How many vaults the code opens.
    pub uses: u64,
}

/// The operator's file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteConfig {
    /// False: no vault opens.
    #[serde(default = "yes")]
    pub openings: bool,
    /// True: an opening needs a listed code with a use left.
    #[serde(default)]
    pub invite_only: bool,
    #[serde(default)]
    pub codes: Vec<InviteCode>,
}

fn yes() -> bool {
    true
}

impl Default for InviteConfig {
    fn default() -> Self {
        Self {
            openings: true,
            invite_only: false,
            codes: vec![],
        }
    }
}

/// What `GET /config` says about openings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpeningsStatus {
    pub open: bool,
    pub invite_only: bool,
}

/// The hash an invite code is listed under.
pub fn code_hash(code: &str) -> B256 {
    keccak256(code.trim().as_bytes())
}

struct Uses {
    /// Uses kept (vaults opened), by code hash; on disk.
    kept: BTreeMap<B256, u64>,
    /// Uses taken by openings in progress.
    held: BTreeMap<B256, u64>,
}

/// The gate: the operator's file and the uses counted so far.
pub struct InviteGate {
    config: PathBuf,
    used_path: PathBuf,
    uses: Mutex<Uses>,
}

/// A use taken for one opening. Dropped without [`InviteUse::keep`] (the opening failed), it is
/// given back.
pub struct InviteUse<'a> {
    gate: &'a InviteGate,
    hash: Option<B256>,
    kept: bool,
}

impl InviteUse<'_> {
    /// The vault opened: the use is counted for good.
    pub fn keep(mut self) -> Result<()> {
        self.kept = true;
        let Some(hash) = self.hash else {
            return Ok(());
        };
        let mut uses = self.gate.lock();
        release(&mut uses.held, hash);
        *uses.kept.entry(hash).or_insert(0) += 1;
        write_atomic(&self.gate.used_path, &serde_json::to_vec(&uses.kept)?).context("record an invite's use")
    }
}

impl Drop for InviteUse<'_> {
    fn drop(&mut self) {
        if let (false, Some(hash)) = (self.kept, self.hash) {
            release(&mut self.gate.lock().held, hash);
        }
    }
}

fn release(held: &mut BTreeMap<B256, u64>, hash: B256) {
    if let Some(n) = held.get_mut(&hash) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            held.remove(&hash);
        }
    }
}

impl InviteGate {
    /// The gate of the operator's file `config`, counting uses in `data_dir`.
    pub fn open(config: &Path, data_dir: &Path) -> Result<Self> {
        let used_path = data_dir.join("invites-used.json");
        let kept = match std::fs::read(&used_path) {
            Ok(raw) => serde_json::from_slice(&raw).context("invites-used.json")?,
            Err(_) => BTreeMap::new(),
        };
        let gate = Self {
            config: config.to_owned(),
            used_path,
            uses: Mutex::new(Uses {
                kept,
                held: BTreeMap::new(),
            }),
        };
        gate.config()?; // a broken file is refused at start-up, not at the first opening
        Ok(gate)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Uses> {
        self.uses.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The operator's file as it is now.
    pub fn config(&self) -> Result<InviteConfig> {
        let raw = std::fs::read(&self.config).context("read the invites file")?;
        serde_json::from_slice(&raw).context("parse the invites file")
    }

    /// Whether openings are open, and invite-only.
    pub fn status(&self) -> Result<OpeningsStatus> {
        let c = self.config()?;
        Ok(OpeningsStatus {
            open: c.openings,
            invite_only: c.invite_only,
        })
    }

    /// Takes a use of `code` for one opening, or refuses it with a fixed public error.
    pub fn take(&self, code: Option<&str>) -> Result<InviteUse<'_>> {
        let config = self.config().context(PublicError::Internal)?;
        anyhow::ensure!(config.openings, PublicError::OpeningsClosed);
        if !config.invite_only {
            return Ok(InviteUse {
                gate: self,
                hash: None,
                kept: false,
            });
        }
        let hash = code.filter(|c| !c.trim().is_empty()).map(code_hash).ok_or(PublicError::InviteRequired)?;
        let listed = config.codes.iter().find(|c| c.hash == hash).ok_or(PublicError::InviteRequired)?;
        let mut uses = self.lock();
        let taken = uses.kept.get(&hash).copied().unwrap_or(0) + uses.held.get(&hash).copied().unwrap_or(0);
        anyhow::ensure!(taken < listed.uses, PublicError::InviteUsed);
        *uses.held.entry(hash).or_insert(0) += 1;
        Ok(InviteUse {
            gate: self,
            hash: Some(hash),
            kept: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{error::public_error, store::tests::TempDir};

    fn gate(dir: &TempDir, config: &InviteConfig) -> InviteGate {
        let path = dir.0.join("invites.json");
        std::fs::write(&path, serde_json::to_vec(config).unwrap()).unwrap();
        InviteGate::open(&path, &dir.0).unwrap()
    }

    fn refused(r: Result<InviteUse<'_>>) -> PublicError {
        public_error(&r.err().expect("refused")).unwrap()
    }

    /// Off by default: openings open, no code needed.
    #[test]
    fn without_gating_any_opening_passes() {
        let dir = TempDir::new("invites-off");
        let g = gate(&dir, &InviteConfig::default());
        g.take(None).unwrap().keep().unwrap();
        g.take(Some("anything")).unwrap().keep().unwrap();
        assert_eq!(
            g.status().unwrap(),
            OpeningsStatus {
                open: true,
                invite_only: false
            }
        );
        // A file with only the switch closed: every opening is refused.
        let closed = gate(
            &dir,
            &InviteConfig {
                openings: false,
                ..InviteConfig::default()
            },
        );
        assert_eq!(refused(closed.take(None)), PublicError::OpeningsClosed);
    }

    /// Invite-only: a listed code opens as many vaults as its uses, counted on disk; a failed
    /// opening gives its use back; a missing or unknown code is refused.
    #[test]
    fn a_code_opens_as_many_vaults_as_its_uses() {
        let dir = TempDir::new("invites-on");
        let config = InviteConfig {
            openings: true,
            invite_only: true,
            codes: vec![
                InviteCode {
                    hash: code_hash("beta-alpha-7f3k"),
                    uses: 2,
                },
                InviteCode {
                    hash: code_hash("beta-bravo-2m9q"),
                    uses: 1,
                },
            ],
        };
        let g = gate(&dir, &config);
        assert_eq!(refused(g.take(None)), PublicError::InviteRequired);
        assert_eq!(refused(g.take(Some(" "))), PublicError::InviteRequired);
        assert_eq!(refused(g.take(Some("beta-charlie"))), PublicError::InviteRequired);
        // A failed opening gives the use back.
        drop(g.take(Some("beta-bravo-2m9q")).unwrap());
        // Two openings in progress take both uses; a third waits for neither.
        let first = g.take(Some("beta-alpha-7f3k")).unwrap();
        let second = g.take(Some(" beta-alpha-7f3k ")).unwrap();
        assert_eq!(refused(g.take(Some("beta-alpha-7f3k"))), PublicError::InviteUsed);
        first.keep().unwrap();
        second.keep().unwrap();
        assert_eq!(refused(g.take(Some("beta-alpha-7f3k"))), PublicError::InviteUsed);
        g.take(Some("beta-bravo-2m9q")).unwrap().keep().unwrap();
        // Counted on disk: a restart remembers them.
        let again = InviteGate::open(&dir.0.join("invites.json"), &dir.0).unwrap();
        assert_eq!(refused(again.take(Some("beta-bravo-2m9q"))), PublicError::InviteUsed);
        // The used-count file names hashes only, never a code.
        let used = std::fs::read_to_string(dir.0.join("invites-used.json")).unwrap();
        assert!(!used.contains("beta-"), "{used}");
        // The switch closes openings even with codes left.
        let mut closed = config.clone();
        closed.openings = false;
        closed.codes[0].uses = 10;
        let g = gate(&dir, &closed);
        assert_eq!(refused(g.take(Some("beta-alpha-7f3k"))), PublicError::OpeningsClosed);
        assert_eq!(
            g.status().unwrap(),
            OpeningsStatus {
                open: false,
                invite_only: true
            }
        );
    }

    /// The hash is keccak256 of the code's bytes (`cast keccak <code>`).
    #[test]
    fn codes_are_listed_by_their_keccak_hash() {
        assert_eq!(code_hash("abc"), keccak256(b"abc"));
        assert_eq!(code_hash(" abc\n"), keccak256(b"abc"));
        let parsed: InviteConfig = serde_json::from_str(r#"{ "inviteOnly": true, "codes": [] }"#).unwrap();
        assert!(parsed.openings && parsed.invite_only);
        let dir = TempDir::new("invites-bad");
        let path = dir.0.join("invites.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(InviteGate::open(&path, &dir.0).is_err(), "a broken file is refused at start-up");
    }
}
