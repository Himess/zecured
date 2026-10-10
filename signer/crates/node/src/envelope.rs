//! Authenticated encryption between two signer nodes.
//!
//! DKG round-2 packages and the ZIP 2005 `sk` contributions are secret to their recipient,
//! but they travel through the coordinator. Each node has a static X25519 key; a message
//! from A to B is encrypted with ChaCha20-Poly1305 under a key derived from the static-static
//! Diffie-Hellman secret, both public keys and a context string (vault id + purpose). Only A
//! and B can derive the key, so the coordinator can neither read nor forge these messages.

use anyhow::{Result, anyhow};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use rand_core_06::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

const KEY_PERSONALIZATION: &[u8; 16] = b"Zecured_P2P_Key_";
const NONCE_LEN: usize = 12;

/// A sealed message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sealed {
    #[serde(with = "crate::b64")]
    pub nonce: Vec<u8>,
    #[serde(with = "crate::b64")]
    pub ciphertext: Vec<u8>,
}

fn key(dh: &[u8; 32], sender: &PublicKey, recipient: &PublicKey, context: &[u8]) -> Key {
    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(KEY_PERSONALIZATION)
        .to_state()
        .update(dh)
        .update(sender.as_bytes())
        .update(recipient.as_bytes())
        .update(context)
        .finalize();
    *Key::from_slice(hash.as_bytes())
}

/// Encrypts `plaintext` from `me` to `them`, bound to `context`.
pub fn seal(me: &StaticSecret, them: &PublicKey, context: &[u8], plaintext: &[u8]) -> Result<Sealed> {
    let my_pub = PublicKey::from(me);
    let dh = me.diffie_hellman(them);
    let cipher = ChaCha20Poly1305::new(&key(dh.as_bytes(), &my_pub, them, context));
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: context,
            },
        )
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok(Sealed {
        nonce: nonce.to_vec(),
        ciphertext,
    })
}

/// Decrypts a message `sender` sealed to `me` for `context`.
pub fn open(me: &StaticSecret, sender: &PublicKey, context: &[u8], sealed: &Sealed) -> Result<Vec<u8>> {
    if sealed.nonce.len() != NONCE_LEN {
        return Err(anyhow!("bad nonce length"));
    }
    let my_pub = PublicKey::from(me);
    let dh = me.diffie_hellman(sender);
    let cipher = ChaCha20Poly1305::new(&key(dh.as_bytes(), sender, &my_pub, context));
    cipher
        .decrypt(
            Nonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad: context,
            },
        )
        .map_err(|_| anyhow!("message from peer failed authentication"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_authentication() {
        let a = StaticSecret::random_from_rng(OsRng);
        let b = StaticSecret::random_from_rng(OsRng);
        let c = StaticSecret::random_from_rng(OsRng);
        let (pa, pb) = (PublicKey::from(&a), PublicKey::from(&b));

        let sealed = seal(&a, &pb, b"vault-1/dkg2", b"secret share").unwrap();
        assert_eq!(open(&b, &pa, b"vault-1/dkg2", &sealed).unwrap(), b"secret share");
        // Wrong context, wrong recipient, or a third party claiming to be the sender: rejected.
        assert!(open(&b, &pa, b"vault-2/dkg2", &sealed).is_err());
        assert!(open(&c, &pa, b"vault-1/dkg2", &sealed).is_err());
        assert!(open(&b, &PublicKey::from(&c), b"vault-1/dkg2", &sealed).is_err());
        // Tampering is detected.
        let mut bad = sealed.clone();
        bad.ciphertext[0] ^= 1;
        assert!(open(&b, &pa, b"vault-1/dkg2", &bad).is_err());
    }
}
