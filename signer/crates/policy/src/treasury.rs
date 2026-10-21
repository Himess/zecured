//! W-6: the destination that reimburses the bonds for a claim they paid, one per claim. Its receiver
//! is a diversified Ironwood receiver of the signers' treasury incoming viewing key, at an index
//! derived from the claim id; its salt is derived from the claim id under a key only the signers
//! hold. Every signer derives the same commitment, `keccak256(salt || receiver)`, and signs
//! `ReimburseDestination` only for it (`zecured-ops sign`); the coordinator registers the recipient
//! with the nodes so that the payout can be verified like any other.
//!
//! The receiver differs per claim: one payout may reimburse several claims of a bound claim set, and
//! the Registry refuses one commitment for two claims of a payout (`DuplicateClaimCommitment`). It
//! also differs from the Fee claim's receiver, which the treasury's address gives (a small index):
//! the derived index has its top bit set. The keys are never logged.

use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use orchard::keys::{DiversifierIndex, IncomingViewingKey};
use serde::Deserialize;

use crate::{derive::blake2b_256, ledger::Recipient};

const REIMBURSE_INDEX: &[u8; 16] = b"Zecured_ReimbIdx";
const REIMBURSE_SALT: &[u8; 16] = b"Zecured_ReimbSlt";

/// The signers' treasury keys: its Ironwood (Orchard-protocol) incoming viewing key and the key the
/// reimbursement salts are derived under. Never printed or logged (no `Debug`).
#[derive(Clone)]
pub struct TreasuryKeys {
    ivk: IncomingViewingKey,
    salt_key: [u8; 32],
}

impl TreasuryKeys {
    /// From the raw incoming viewing key (64 bytes: `dk || ivk`) and the salt key.
    pub fn from_bytes(ivk: &[u8; 64], salt_key: [u8; 32]) -> Result<Self> {
        let ivk = Option::<IncomingViewingKey>::from(IncomingViewingKey::from_bytes(ivk))
            .ok_or_else(|| anyhow!("not an Ironwood incoming viewing key"))?;
        ensure!(salt_key != [0; 32], "the reimbursement salt key is zero");
        Ok(Self { ivk, salt_key })
    }

    /// From the treasury keys file, `{"ivk": <128 hex>, "saltKey": <64 hex>}`, private to the signers
    /// and their coordinator. N-12: on Unix, refused while its group or anyone else has access to it
    /// (`zecured-ops treasury-keys` writes it with mode 0600).
    pub fn read_file(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct KeysFile {
            ivk: String,
            salt_key: String,
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                std::fs::metadata(path).with_context(|| format!("read {}", path.display()))?.permissions().mode();
            ensure!(
                mode & 0o077 == 0,
                "the treasury keys file {} is open to others (mode {:o}): make it the operator's alone (chmod 600)",
                path.display(),
                mode & 0o777
            );
        }
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let f: KeysFile = serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
        Self::from_hex(&f.ivk, &f.salt_key)
    }

    /// From their hex forms (as in a keys file): 128 and 64 hex characters.
    pub fn from_hex(ivk: &str, salt_key: &str) -> Result<Self> {
        let ivk: [u8; 64] = hex::decode(ivk.trim_start_matches("0x"))
            .ok()
            .and_then(|b| b.try_into().ok())
            .context("the treasury incoming viewing key is not 64 bytes of hex")?;
        let salt_key: [u8; 32] = hex::decode(salt_key.trim_start_matches("0x"))
            .ok()
            .and_then(|b| b.try_into().ok())
            .context("the reimbursement salt key is not 32 bytes of hex")?;
        Self::from_bytes(&ivk, salt_key)
    }

    /// The diversifier index of `claim_id`'s receiver: BLAKE2b of the claim id, top bit set.
    pub fn index_of(claim_id: &[u8; 32]) -> [u8; 11] {
        let h = blake2b_256(REIMBURSE_INDEX, None, &[claim_id]);
        let mut index = [0u8; 11];
        index.copy_from_slice(&h[..11]);
        index[10] |= 0x80; // little-endian: at least 2^87, never a wallet's ordinary small index
        index
    }

    /// The recipient that reimburses the bonds for `claim_id`.
    pub fn reimbursement(&self, claim_id: &[u8; 32]) -> Recipient {
        let index = DiversifierIndex::from(Self::index_of(claim_id));
        Recipient {
            receiver: self.ivk.address_at(index).to_raw_address_bytes(),
            salt: blake2b_256(REIMBURSE_SALT, Some(&self.salt_key), &[claim_id]),
        }
    }
}

#[cfg(test)]
mod tests {
    use orchard::keys::{FullViewingKey, Scope, SpendingKey};

    use super::*;

    fn keys() -> TreasuryKeys {
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes([7; 32])).unwrap();
        let ivk = FullViewingKey::from(&sk).to_ivk(Scope::External).to_bytes();
        TreasuryKeys::from_bytes(&ivk, [9; 32]).unwrap()
    }

    /// W-6: each claim has its own receiver and salt, the same for every signer; the receiver is the
    /// treasury's (its viewing key finds the index again), never at the treasury address's own index.
    #[test]
    fn w6_each_claim_has_its_own_treasury_destination() {
        let k = keys();
        let (a, b) = (k.reimbursement(&[1; 32]), k.reimbursement(&[2; 32]));
        assert_ne!(a.receiver, b.receiver);
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.commitment(), b.commitment());
        assert_eq!(keys().reimbursement(&[1; 32]).commitment(), a.commitment(), "deterministic");
        let address = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&a.receiver)).unwrap();
        let found = k.ivk.diversifier_index(&address).expect("the treasury's own receiver");
        assert_eq!(found, DiversifierIndex::from(TreasuryKeys::index_of(&[1; 32])));
        assert_ne!(
            a.receiver,
            k.ivk.address_at(DiversifierIndex::from(0u32)).to_raw_address_bytes(),
            "not the treasury address's index 0 (the Fee claim's receiver)"
        );
        assert!(TreasuryKeys::index_of(&[3; 32])[10] & 0x80 != 0);
        // Another salt key gives other salts, the same receivers.
        let other = TreasuryKeys::from_bytes(&k.ivk.to_bytes(), [8; 32]).unwrap().reimbursement(&[1; 32]);
        assert_eq!(other.receiver, a.receiver);
        assert_ne!(other.salt, a.salt);
    }

    #[test]
    fn w6_keys_are_checked() {
        assert!(TreasuryKeys::from_hex("00", "11").is_err());
        let ivk = hex::encode(keys().ivk.to_bytes());
        assert!(TreasuryKeys::from_hex(&ivk, &"00".repeat(32)).is_err(), "a zero salt key");
        assert!(TreasuryKeys::from_hex(&ivk, &"09".repeat(32)).is_ok());
    }

    /// N-12: a keys file its group or anyone else may read is refused; the operator's alone (0600,
    /// as `zecured-ops treasury-keys` writes it) is read.
    #[cfg(unix)]
    #[test]
    fn n12_a_keys_file_open_to_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zecured-n12-treasury-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("treasury-keys.json");
        let ivk = hex::encode(keys().ivk.to_bytes());
        let body = serde_json::json!({ "ivk": ivk, "saltKey": "09".repeat(32) });
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        for open in [0o644, 0o640, 0o604, 0o660] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(open)).unwrap();
            let e = TreasuryKeys::read_file(&path).err().expect("refused");
            assert!(e.to_string().contains("open to others"), "{e}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(TreasuryKeys::read_file(&path).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
