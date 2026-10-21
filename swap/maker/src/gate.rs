//! What the maker checks before it quotes or pays: the invitation (mainnet beta) and the taker's
//! destination. Nothing here logs an invite code, a receiver or a salt.

use alloy::primitives::{B256, keccak256};
use anyhow::{Context, Result, anyhow, ensure};
use zcash_protocol::consensus::Network;
use zecured_zcash_ops::keys::ironwood_receiver;

use crate::config::MakerConfig;

/// The HTTP header that carries an invite code (`GET /swap/quote`). A header, never a query
/// parameter: URLs end up in logs.
pub const INVITE_HEADER: &str = "x-zecured-invite";

/// Mainnet beta (docs/MAINNET.md § 5): whether `code` admits its holder. With `invite_only` off (the
/// judge deployment) everyone is admitted; on, the code's keccak256 must be one of the configured hashes.
pub fn admitted(cfg: &MakerConfig, code: Option<&str>) -> bool {
    if !cfg.invite_only {
        return true;
    }
    let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
        return false;
    };
    let hash = keccak256(code.as_bytes());
    cfg.invite_code_hashes.contains(&hash)
}

/// A taker's destination, checked: the raw 43-byte Ironwood receiver and the salt, whose
/// `keccak256(salt || receiver)` is the order's commitment.
pub struct Destination {
    pub receiver: [u8; 43],
    pub salt: [u8; 32],
}

/// Checks `receiver` (an Ironwood unified address of `network`, or the raw receiver as 86 hex
/// digits) and `salt` (64 hex digits) against the order's `commitment`. The commitment is on Base,
/// so no authentication is needed: a receiver that does not open it is refused.
pub fn check_destination(network: &Network, receiver: &str, salt: &str, commitment: B256) -> Result<Destination> {
    let receiver = receiver.trim();
    let raw: [u8; 43] = if let Some(hex) = receiver.strip_prefix("0x") {
        let bytes: [u8; 43] = hex::decode(hex)
            .context("receiver hex")?
            .try_into()
            .map_err(|_| anyhow!("a raw receiver is 43 bytes"))?;
        let address = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&bytes))
            .context("not a valid Ironwood receiver")?;
        address.to_raw_address_bytes()
    } else {
        ironwood_receiver(network, receiver)?.to_raw_address_bytes()
    };
    let salt: [u8; 32] = hex::decode(salt.trim().trim_start_matches("0x"))
        .context("salt hex")?
        .try_into()
        .map_err(|_| anyhow!("the salt is 32 bytes"))?;
    ensure!(
        zecured_swap::receiver_commitment(&salt, &raw) == commitment,
        "the destination does not open the order's commitment"
    );
    Ok(Destination { receiver: raw, salt })
}

#[cfg(test)]
mod tests {
    use orchard::keys::{FullViewingKey, Scope, SpendingKey};
    use zecured_zcash_ops::keys::encode_ironwood_ua;

    use super::*;
    use crate::config::tests::judge_like;

    #[test]
    fn invitations() {
        let mut c = judge_like();
        assert!(admitted(&c, None), "no invitation needed on the judge deployment");
        c.invite_only = true;
        c.invite_code_hashes = vec![keccak256("zecured-beta-0001".as_bytes())];
        assert!(admitted(&c, Some("zecured-beta-0001")));
        assert!(admitted(&c, Some(" zecured-beta-0001 ")), "whitespace around the code");
        assert!(!admitted(&c, Some("zecured-beta-0002")));
        assert!(!admitted(&c, Some("")));
        assert!(!admitted(&c, None));
    }

    fn some_address() -> (String, [u8; 43]) {
        let sk = (0u8..=255)
            .find_map(|i| Option::<SpendingKey>::from(SpendingKey::from_bytes([i; 32])))
            .expect("a spending key");
        let addr = FullViewingKey::from(&sk).address_at(3u32, Scope::External);
        (encode_ironwood_ua(&Network::TestNetwork, addr).unwrap(), addr.to_raw_address_bytes())
    }

    #[test]
    fn destinations_open_the_commitment() {
        let (ua, raw) = some_address();
        let salt = [0x5a; 32];
        let c = zecured_swap::receiver_commitment(&salt, &raw);
        let d = check_destination(&Network::TestNetwork, &ua, &hex::encode(salt), c).unwrap();
        assert_eq!(d.receiver, raw);
        // The raw receiver works too.
        let d = check_destination(&Network::TestNetwork, &format!("0x{}", hex::encode(raw)), &hex::encode(salt), c);
        assert_eq!(d.unwrap().salt, salt);
        // Another salt, another commitment, another network, garbage: refused.
        assert!(check_destination(&Network::TestNetwork, &ua, &hex::encode([0x5b; 32]), c).is_err());
        assert!(check_destination(&Network::TestNetwork, &ua, &hex::encode(salt), B256::ZERO).is_err());
        assert!(check_destination(&Network::MainNetwork, &ua, &hex::encode(salt), c).is_err());
        assert!(check_destination(&Network::TestNetwork, "utest1nope", &hex::encode(salt), c).is_err());
        assert!(check_destination(&Network::TestNetwork, "0x1234", &hex::encode(salt), c).is_err());
        assert!(check_destination(&Network::TestNetwork, &ua, "00", c).is_err());
    }
}
