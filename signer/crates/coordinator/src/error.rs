//! Errors shown to API callers, and scrubbing of diagnostics before they are logged (L-2).
//!
//! Callers only ever see a fixed message and a short code ([`PublicError`]). Internal error
//! chains can embed node URLs, RPC URLs (with API keys), note positions or heights; they are
//! logged at debug level only, after [`scrub`].

use std::fmt;

/// A failure as reported to API callers: a fixed message and a short code, nothing else.
///
/// Attach it to an internal error with `.context(PublicError::…)` (or return it directly);
/// [`public_error`] finds it anywhere in the chain. Anything without one is `Internal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicError {
    BadRequest,
    UnknownVault,
    NotDenomination,
    AlreadyBound,
    /// Too many vaults are waiting for their deposit (M-8).
    Busy,
    /// The open-vault request's hashcash stamp is missing or too weak (H-01).
    BadStamp,
    InvalidReceiver,
    NotOwner,
    Expired,
    NonceUsed,
    /// I30: an off-chain refund of a deposit that a guest of the deployment can land on Base.
    RefundOnBase,
    /// Fewer than three signers accepted, and at least one refused under its policy.
    SignersRefused,
    /// Fewer than three signers answered (unreachable, offline or broken).
    SignersUnavailable,
    /// The Zcash light wallet server or the Base RPC failed.
    ChainUnavailable,
    /// Mainnet beta (section 6): openings are invite-only and the request has no valid code.
    InviteRequired,
    /// Mainnet beta: the invite code's uses are all taken.
    InviteUsed,
    /// Mainnet beta: the operator closed vault openings (exits and payouts go on).
    OpeningsClosed,
    /// R-11.1: the vault's activation is not blocked (the coverage cap has room): nothing to refund.
    NotBlocked,
    /// R-11.1: no deposit of the vault's amount has been seen with its confirmations yet.
    NoDeposit,
    /// R-11.1: the deposit is on Base already (or the vault no longer waits for it).
    AlreadyLanded,
    /// R-11.1: this deployment's guest has no Refund mode (journal v1).
    RefundUnavailable,
    /// The coordinator is not set up for this (e.g. it makes no proofs, or the deployment has no
    /// bonds).
    NotConfigured,
    /// R-5: no signer gave a valid liveness receipt for the claim.
    NoReceipt,
    /// R-11.1: the coverage cap has no room for this vault's activation now.
    CapFull,
    Internal,
}

impl PublicError {
    /// Short machine-readable code.
    pub fn code(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnknownVault => "unknown_vault",
            Self::NotDenomination => "not_a_denomination",
            Self::AlreadyBound => "already_bound",
            Self::Busy => "busy",
            Self::BadStamp => "bad_stamp",
            Self::InvalidReceiver => "invalid_receiver",
            Self::NotOwner => "not_owner",
            Self::Expired => "expired",
            Self::NonceUsed => "nonce_used",
            Self::RefundOnBase => "refund_on_base",
            Self::SignersRefused => "signers_refused",
            Self::SignersUnavailable => "signers_unavailable",
            Self::ChainUnavailable => "chain_unavailable",
            Self::InviteRequired => "invite_required",
            Self::InviteUsed => "invite_used",
            Self::OpeningsClosed => "openings_closed",
            Self::NotBlocked => "not_blocked",
            Self::NoDeposit => "no_deposit",
            Self::AlreadyLanded => "already_landed",
            Self::RefundUnavailable => "refund_unavailable",
            Self::NotConfigured => "not_configured",
            Self::NoReceipt => "no_receipt",
            Self::CapFull => "cap_full",
            Self::Internal => "internal",
        }
    }

    /// Fixed, user-facing message.
    pub fn message(self) -> &'static str {
        match self {
            Self::BadRequest => "Invalid request.",
            Self::UnknownVault => "Unknown vault.",
            Self::NotDenomination => "Not a standard collateral denomination.",
            Self::AlreadyBound => "This account or owner key already has a vault.",
            Self::Busy => "Too many vaults are waiting for a deposit. Try again later.",
            Self::BadStamp => "The request's proof of work is missing or too weak.",
            Self::InvalidReceiver => "Not a valid unified address with an Ironwood (Orchard-protocol) receiver.",
            Self::NotOwner => "Not signed by the vault owner.",
            Self::Expired => "The request has expired.",
            Self::NonceUsed => "This request nonce was already used.",
            Self::RefundOnBase => {
                "This deposit can land on Base: it is refunded there, as a Refund claim, once a proof lands it."
            }
            Self::SignersRefused => "The signers refused the request.",
            Self::SignersUnavailable => "Not enough signers are available. Try again later.",
            Self::ChainUnavailable => "A blockchain service is unavailable. Try again later.",
            Self::InviteRequired => "Opening a vault needs a valid invite code.",
            Self::InviteUsed => "This invite code has been used.",
            Self::OpeningsClosed => "New vaults cannot be opened right now. Existing vaults are not affected.",
            Self::NotBlocked => "The deposit can still activate the vault: there is nothing to refund.",
            Self::NoDeposit => "No confirmed deposit of the vault's amount has been seen yet.",
            Self::AlreadyLanded => "The deposit is already on Base.",
            Self::RefundUnavailable => "This deployment cannot prove a deposit as a refund.",
            Self::NotConfigured => "Not configured on this backend.",
            Self::NoReceipt => "No signer holds a validated destination for this claim yet.",
            Self::CapFull => {
                "The coverage cap has no room for this vault now: try again later, or ask for the deposit back."
            }
            Self::Internal => "Internal error.",
        }
    }

    /// HTTP status for the API.
    pub fn status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::NotOwner | Self::InviteRequired | Self::OpeningsClosed => 403,
            Self::UnknownVault | Self::NoReceipt => 404,
            Self::AlreadyBound
            | Self::NonceUsed
            | Self::RefundOnBase
            | Self::InviteUsed
            | Self::NotBlocked
            | Self::AlreadyLanded
            | Self::RefundUnavailable
            | Self::CapFull => 409,
            Self::NotDenomination | Self::InvalidReceiver | Self::Expired | Self::SignersRefused | Self::NoDeposit => {
                422
            }
            Self::BadStamp => 422,
            Self::Busy => 429,
            Self::Internal => 500,
            Self::SignersUnavailable | Self::ChainUnavailable | Self::NotConfigured => 503,
        }
    }
}

impl fmt::Display for PublicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PublicError {}

/// The public error attached anywhere in `e`'s chain (the outermost one), if any.
pub fn public_error(e: &anyhow::Error) -> Option<PublicError> {
    e.downcast_ref::<PublicError>().copied()
}

/// Removes identifiers and numbers from a diagnostic before it is logged: every ASCII
/// alphanumeric token of 12 or more characters (txids, vault ids, addresses, keys, API keys in
/// URLs) and every run of 4 or more digits (heights, positions, amounts, ports, timestamps).
pub fn scrub(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.len() >= 12 {
            out.push('…');
        } else if token.len() >= 4 && token.bytes().all(|b| b.is_ascii_digit()) {
            out.push('#');
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for c in message.chars() {
        if c.is_ascii_alphanumeric() {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// `scrub` of the full error chain.
pub fn scrubbed(e: &anyhow::Error) -> String {
    scrub(&format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn public_errors_are_found_anywhere_in_the_chain() {
        let e = anyhow!("node 1 unreachable").context(PublicError::SignersUnavailable).context("DKG round 1");
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
        let e: anyhow::Error = PublicError::NotOwner.into();
        assert_eq!(public_error(&e.context("checking the request")), Some(PublicError::NotOwner));
        assert_eq!(public_error(&anyhow!("disk full")), None);
        // anyhow's `ensure!` with a value keeps the type.
        let check = || -> anyhow::Result<()> {
            anyhow::ensure!(1 + 1 == 3, PublicError::Expired);
            Ok(())
        };
        assert_eq!(public_error(&check().unwrap_err()), Some(PublicError::Expired));
    }

    #[test]
    fn public_messages_are_fixed() {
        for p in [PublicError::SignersRefused, PublicError::Internal, PublicError::Busy] {
            assert!(!p.message().is_empty() && !p.code().contains(' '));
        }
        assert_eq!(PublicError::UnknownVault.status(), 404);
        assert_eq!(PublicError::SignersUnavailable.status(), 503);
    }

    #[test]
    fn scrub_removes_positions_urls_and_ids() {
        let e = anyhow!("P1 decision changed for NotePosition {{ height: 4486950, tx_index: 3, action_index: 1 }}");
        let s = scrubbed(&e.context("vault 8c4a1f2e"));
        assert!(!s.contains("4486950"), "{s}");
        assert!(s.contains("tx_index: 3"), "short numbers stay: {s}");

        let s = scrub("error sending request for url (https://base-sepolia.g.alchemy.com/v2/AbCdEf0123456789xyz)");
        assert!(!s.contains("AbCdEf0123456789xyz"), "{s}");
        let s = scrub("node 2 unreachable (http://127.0.0.1:9102/v1/sign/commit)");
        assert!(!s.contains("9102") && s.contains("node 2"), "{s}");
        let s = scrub(
            "bad txid 9d323f2cbd3ac0fa0397ab4445bd76f54a273cca84de97b50aa92f6fd0f2be82 for utest1qqqqqqqqqqqqqqqqqqqqq",
        );
        assert!(!s.contains("9d323f2c") && !s.contains("utest1qq"), "{s}");
    }
}
