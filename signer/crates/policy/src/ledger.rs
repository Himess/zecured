//! Vault accounting and payout planning (policies P2, P3, P6; refunds by proof, T-02).
//!
//! A vault's ZEC sits in the notes Base knows by their tags (docs/TIER3.md § 2), its lineage
//! notes:
//! - its **main lineage**: the deposit Base activated the vault with and the change notes of the
//!   vault's own payouts. Its value always equals `outstanding` plus every claim it backs that is
//!   not paid yet (I10).
//! - one **refund note** per deposit that landed on Base as a refund (Registry v3, T-02): the
//!   deposit's own note, which pays its Refund claim alone and without change. A refund is never
//!   mixed into the main lineage (I30).
//!
//! Anything else the vault receives is outside the books: a deposit that has not landed on Base
//! (it leaves the vault only once a proof lands it, as collateral or as a refund: an off-chain
//! refund of a note a deployed guest can make a lineage note would later look like a theft, I30),
//! and **foreign** notes sent to the vault's internal address by anyone else (ignored for I10 and
//! fees, see [`lineage_members`]).
//!
//! Every Base event that burns zZEC creates claims (one per recipient); so do a refund (T-02) and
//! the opening fee (T-06). Claims are paid in Base order, one payout transaction at a time. A
//! payout of the main lineage settles up to [`MAX_CLAIMS_PER_PAYOUT`] of them (review H-06), its
//! first claim bearing the ZIP-317 fee out of its recipient's output, except that a Fee claim
//! riding along bears exactly its own output's share ([`ZIP317_MARGINAL_FEE`]). It spends the
//! whole main lineage and returns the rest (the still-unpaid claims plus `outstanding`) to the
//! vault's internal address, so the zZEC on Base and the ZEC in the vault never diverge.

use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use zecured_zcash_ops::spend::{OutputPrivacy, zip317_fee};

/// Identifier of a claim on Base (`keccak256(abi.encode(vaultId, sequence))`).
pub type ClaimId = [u8; 32];
/// `keccak256(salt32 || receiver43)`.
pub type Commitment = [u8; 32];

/// Seconds a close-from-collateral payout waits for the market maker's recipient so both
/// outputs can share one transaction (docs/ARCHITECTURE.md, payout queue).
pub const MARKET_MAKER_GRACE_SECS: u64 = 120;

/// Most claims one payout settles: the Registry's `MAX_CLAIMS_PER_PAYOUT` (a commitment names 1
/// to 8 claims). Review H-06: a vault's claims would otherwise be paid one payout at a
/// time, each waiting for the previous change to be mined, so splitting one liquidation into many
/// small ones could hold every later claim of the vault back for hours.
pub const MAX_CLAIMS_PER_PAYOUT: usize = 8;

/// ZIP 317's marginal fee per logical action: what a Fee claim riding in a payout bears of the
/// payout's fee, its own output's share (the Registry's `ZIP317_MARGINAL_FEE`, T-06).
pub const ZIP317_MARGINAL_FEE: u64 = 5_000;

/// What created a claim (the Registry's `ClaimKind`, in its order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClaimKind {
    /// `ReleaseRequested`: the borrower takes the remaining collateral (P2).
    Release,
    /// `LiquidationRelease`: the liquidator receives the seized ZEC (P3).
    Liquidation,
    /// `CloseRelease`, borrower's part (P6).
    CloseUser,
    /// `CloseRelease`, market maker's part (P6).
    CloseMarketMaker,
    /// `RefundOpened` (T-02): a deposit that landed on Base as a refund, paid back from its own
    /// note to the destination the vault's account gives (`RefundDestinationSet`).
    Refund,
    /// `FeeCharged` (T-06): the opening fee, owed to the signers' treasury; it rides with the
    /// vault's first exit.
    Fee,
    /// `VaultRetired` (T-04): bookkeeping only, settled in USDC, never paid on Zcash.
    Retired,
}

impl ClaimKind {
    /// Third parties' outputs are encrypted without an outgoing viewing key, so the vault
    /// UFVK given to the borrower reveals nothing about them.
    pub fn privacy(self) -> OutputPrivacy {
        match self {
            ClaimKind::Release | ClaimKind::CloseUser | ClaimKind::Refund => OutputPrivacy::VaultOvk,
            ClaimKind::Liquidation | ClaimKind::CloseMarketMaker | ClaimKind::Fee | ClaimKind::Retired => {
                OutputPrivacy::NoOvk
            }
        }
    }

    /// Backed by, and paid from, the vault's main lineage (a Refund claim has its own note; a
    /// Retired one is never paid on Zcash).
    pub fn main_lineage(self) -> bool {
        !matches!(self, ClaimKind::Refund | ClaimKind::Retired)
    }

    /// T-05: the signers' bonds pay the claimant of such a claim that stays unpaid too long.
    pub fn liveness_covered(self) -> bool {
        !matches!(self, ClaimKind::Fee | ClaimKind::Retired)
    }
}

/// Payment state of a claim, as seen by a signer node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimState {
    /// Not paid on Zcash yet (`Pending` on Base).
    Unpaid,
    /// Paid by a mined Zcash transaction (it may still await `confirmPayout` on Base).
    Paid,
    /// Written off on Base: a dust claim of a closed vault (RT-8) or an abandoned refund (T-02).
    /// Its value stays in the vault; an abandoned refund is pending again once its account gives
    /// a destination.
    WrittenOff,
    /// T-04: its vault was compromised; the compensator pays it in USDC.
    Compensated,
    /// T-05 (a): its claimant disputes a confirmation that this node did not see mined itself.
    /// Unpaid until the payout is proven (then paid) or the claim is reopened (then pending).
    Disputed,
}

/// One claim, in the order its Base event was emitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub kind: ClaimKind,
    pub amount: u64,
    /// Zero while the claim has no destination: a refund before its account gives one, a
    /// bond-paid claim before three signers give its Reimburse destination (T-02, T-05).
    pub commitment: Commitment,
    /// Base `(block number, log index)` of the emitting event; the two claims of one
    /// `CloseRelease` share it.
    pub order: (u64, u64),
    /// When the claim became payable (Base's `createdAt`): the creating block's timestamp, or for
    /// a Refund claim the time its destination was given. The bonds' liveness clock (T-05).
    pub created_at: u64,
    /// For close claims: the other claim of the same `CloseRelease`.
    pub partner: Option<ClaimId>,
    pub state: ClaimState,
    /// Refund claims: the tag of the refunded deposit's note, which pays the claim (I30).
    #[serde(default)]
    pub refund_tag: Option<[u8; 32]>,
    /// T-05: the bonds paid the claimant; the claim's ZEC now reimburses them, at the destination
    /// three signers give (`ReimburseDestinationSet`).
    #[serde(default)]
    pub bond_paid: bool,
}

impl Claim {
    /// Counted in the vault's books (I10): not paid on Zcash and not settled in USDC. A
    /// written-off claim's value stays in the vault, and a disputed one is unpaid again.
    pub fn in_books(&self) -> bool {
        matches!(self.state, ClaimState::Unpaid | ClaimState::WrittenOff | ClaimState::Disputed)
    }

    /// Whether Base lets a payout pay it: pending, with a destination.
    pub fn payable(&self) -> bool {
        self.state == ClaimState::Unpaid && self.commitment != [0; 32]
    }

    /// The outgoing-viewing-key policy of its output: a bond-paid claim now reimburses the bonds,
    /// a third party for the vault's viewing key.
    pub fn privacy(&self) -> OutputPrivacy {
        if self.bond_paid {
            OutputPrivacy::NoOvk
        } else {
            self.kind.privacy()
        }
    }
}

/// An off-chain recipient revealed to the signers: raw Orchard-protocol receiver + salt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    #[serde(with = "hex43")]
    pub receiver: [u8; 43],
    pub salt: [u8; 32],
}

impl Recipient {
    /// The commitment that appears on Base for this recipient.
    pub fn commitment(&self) -> Commitment {
        commitment(&self.salt, &self.receiver)
    }
}

/// `keccak256(salt32 || receiver43)`.
pub fn commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> Commitment {
    let mut h = Keccak256::new();
    h.update(salt);
    h.update(receiver);
    h.finalize().into()
}

/// Tier 3 `claimRef` (docs/TIER3.md § 2): `keccak256(salt32 || txid32)`, the txid in internal
/// byte order. Committed next to the claim it pays, it lets the claimant, who knows its salt but
/// not the vault's key, find the payout and prove a misdirected payment (F3).
pub fn claim_ref(salt: &[u8; 32], txid: &[u8; 32]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(salt);
    h.update(txid);
    h.finalize().into()
}

/// RT-4: `keccak256(abi.encodePacked(claimIds))`, the Registry's `inputClaimSet` of a note once a
/// payout of these claims (in this order) is committed from it.
pub fn claim_set_hash(claims: &[ClaimId]) -> [u8; 32] {
    let mut h = Keccak256::new();
    for id in claims {
        h.update(id);
    }
    h.finalize().into()
}

/// One output of a planned payout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedPayment {
    pub claim: ClaimId,
    pub receiver: [u8; 43],
    /// Value received, after the fee this payment bears.
    pub value: u64,
    pub privacy: OutputPrivacy,
}

/// What a payout spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayoutInput {
    /// The vault's main lineage (its deposit, or the change of its last payout).
    Main,
    /// The note of a deposit Base refunds, by its tag: it pays its own Refund claim (I30).
    Refund([u8; 32]),
    /// P5: a deposit no guest of the deployment can land on Base, by its note id, refunded
    /// off-chain to the destination its owner asked for ([`refund_plan`], I30).
    Unlanded([u8; 32]),
}

/// A payout transaction every signer can reproduce and check a PCZT against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutPlan {
    /// Claims this transaction settles, in the order its commitment lists them.
    pub claims: Vec<ClaimId>,
    pub payments: Vec<PlannedPayment>,
    /// Number of notes spent (all of the input's).
    pub inputs: usize,
    /// Value returned to the vault's internal address.
    pub change: u64,
    pub fee: u64,
    pub input: PayoutInput,
}

/// What to do next for a vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextPayout {
    Pay(PayoutPlan),
    /// A close payout is waiting for the market maker's recipient until this Unix time.
    WaitUntil(u64),
    Nothing,
}

/// Errors that mean the vault's books do not add up; signers refuse to sign.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("claims to pay ({claims}) exceed the main lineage ({lineage})")]
    Overdrawn { claims: u64, lineage: u64 },
    #[error("no main-lineage notes to spend")]
    NoInputs,
    #[error("I10 violated: lineage {lineage} != outstanding {outstanding} + unpaid claims {unpaid}")]
    InvariantI10 {
        lineage: u64,
        outstanding: u64,
        unpaid: u64,
    },
    #[error("refund of {value} zat does not cover the fee {fee}")]
    RefundIsDust { value: u64, fee: u64 },
}

/// Counted against the main lineage: a main-lineage claim not yet paid on Zcash nor settled in
/// USDC (dust and set-aside claims included).
fn backed_by_main_lineage(c: &Claim) -> bool {
    c.kind.main_lineage() && c.in_books()
}

/// I10 for the main lineage: its value equals `outstanding` plus every main-lineage claim not yet
/// paid on Zcash (dust and set-aside claims included). Refund claims are backed by their own
/// notes, never by the main lineage.
pub fn check_i10(lineage_value: u64, outstanding: u64, claims: &[Claim]) -> Result<(), LedgerError> {
    let unpaid: u64 = claims.iter().filter(|c| backed_by_main_lineage(c)).map(|c| c.amount).sum();
    if lineage_value == outstanding + unpaid {
        Ok(())
    } else {
        Err(LedgerError::InvariantI10 {
            lineage: lineage_value,
            outstanding,
            unpaid,
        })
    }
}

/// One note received by a vault (either scope), reduced to what lineage selection needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultNote {
    pub nullifier: [u8; 32],
    pub value: u64,
    /// Received at the vault's internal (change) address.
    pub internal: bool,
    /// Id of the transaction that created the note.
    pub created_in: [u8; 32],
    /// Id of the transaction that spent the note, if it was spent.
    pub spent_in: Option<[u8; 32]>,
}

/// Which of a vault's notes belong to its main lineage, spent or not (H-2).
///
/// The lineage starts at the deposit Base activated the vault with (`root`, by nullifier). An
/// internal note joins it only if it carries value and the transaction that created it spends a
/// lineage note: only the vault (three signers) can spend a vault note, and every signer checked
/// that such a transaction pays the plan exactly, change included. Every other internal note is
/// foreign (sent by whoever knows the internal address) and is ignored for I10 and for fees, so it
/// can neither break the books nor inflate the fee of later payouts. The value check against the
/// payout's planned change is [`retain_planned_change`].
///
/// `notes` must be in chain order. Returns one flag per note.
pub fn lineage_members(root: Option<&[u8; 32]>, notes: &[VaultNote]) -> Vec<bool> {
    let mut member: Vec<bool> = notes.iter().map(|n| Some(&n.nullifier) == root).collect();
    for (i, n) in notes.iter().enumerate() {
        if member[i] || !n.internal || n.value == 0 {
            continue;
        }
        // The creating transaction must spend a note already in the lineage (always earlier in
        // chain order: a note is spent after it is created).
        let joins = notes.iter().zip(&member).any(|(m, &is)| is && m.spent_in == Some(n.created_in));
        member[i] = joins;
    }
    member
}

/// Keeps, among the unspent main-lineage notes, the deposit and the change notes worth exactly
/// `book_value` (H-2).
///
/// Every payout spends the whole lineage and returns its planned change to the internal
/// address; once its claims are marked paid, I10 makes that change equal to the vault's book
/// value `outstanding + unpaid claims`. Any other internal output of a vault transaction (for
/// example a payout to the vault's own address, which nodes refuse to register) is not change.
/// Returns the notes removed from the lineage.
pub fn retain_planned_change<N>(
    lineage: &mut Vec<N>,
    book_value: u64,
    is_internal: impl Fn(&N) -> bool,
    value: impl Fn(&N) -> u64,
) -> Vec<N> {
    let (keep, drop): (Vec<N>, Vec<N>) =
        std::mem::take(lineage).into_iter().partition(|n| !is_internal(n) || value(n) == book_value);
    *lineage = keep;
    drop
}

/// The main lineage's book value: `outstanding` plus every main-lineage claim not yet paid on
/// Zcash. I10 requires the main lineage to hold exactly this much.
pub fn book_value(outstanding: u64, claims: &[Claim]) -> u64 {
    claims
        .iter()
        .filter(|c| backed_by_main_lineage(c))
        .fold(outstanding, |sum, c| sum.saturating_add(c.amount))
}

/// The order claims are paid in: Base order, a close pair's borrower before its market maker.
fn queue_key(c: &Claim) -> ((u64, u64), bool) {
    (c.order, c.kind == ClaimKind::CloseMarketMaker)
}

/// The order a payout lists its claims in (and its commitment on Base, RT-4): the queue's order,
/// Fee claims last (T-06: a Fee claim never bears the payout's fee for the others). A payout
/// rebuilt from the claims a commitment binds (RT-3) lists them the same way.
fn payout_order(group: &mut [(&Claim, Recipient)]) {
    group.sort_by_key(|(c, _)| (c.kind == ClaimKind::Fee, queue_key(c)));
}

/// The recipient a claim pays: none without a destination.
fn payee(claim: &Claim, recipient: &impl Fn(&Commitment) -> Option<Recipient>) -> Option<Recipient> {
    if claim.commitment == [0; 32] {
        return None;
    }
    recipient(&claim.commitment)
}

/// Decides the next payout of a vault's main lineage.
///
/// `lineage_value` / `lineage_notes`: the unspent main-lineage notes (all are spent).
/// `recipient(commitment)`: the recipient revealed off-chain for a commitment, if any.
/// `now`: Unix time, used only for the market maker grace period.
/// `bound`: the unpaid claims a commitment on Base already binds to the lineage note (Tier 3).
///
/// Only pending main-lineage claims with a destination are paid (a refund is paid from its own
/// note, [`next_vault_payout`]). T-06: a Fee claim rides with the vault's first exit, after its
/// first claim, bearing only its own output's share of the fee; it heads a payout only once no
/// other main-lineage claim of the vault is left to pay.
pub fn next_payout(
    claims: &[Claim],
    recipient: impl Fn(&Commitment) -> Option<Recipient>,
    lineage_value: u64,
    lineage_notes: usize,
    now: u64,
    bound: &[ClaimId],
) -> Result<NextPayout, LedgerError> {
    let mut unpaid: Vec<&Claim> =
        claims.iter().filter(|c| c.state == ClaimState::Unpaid && c.kind.main_lineage()).collect();
    unpaid.sort_by_key(|c| queue_key(c));
    let by_id = |id: &ClaimId| unpaid.iter().copied().find(|c| &c.id == id);
    let recipient = |c: &Claim| payee(c, &recipient);

    // RT-3: claims a commitment on Base already binds to this lineage note are its payout, all of
    // them and nothing else (the Registry accepts no other claim list from this note, RT-4), in
    // payout order, before any other claim. They were payable when they were committed.
    if !bound.is_empty() {
        let mut group: Vec<(&Claim, Recipient)> = vec![];
        for claim in unpaid.iter().filter(|c| bound.contains(&c.id)) {
            let Some(rcpt) = recipient(claim) else {
                return Ok(NextPayout::Nothing);
            };
            group.push((claim, rcpt));
        }
        if group.len() != bound.len() {
            return Ok(NextPayout::Nothing); // a bound claim this signer does not know as unpaid
        }
        payout_order(&mut group);
        return Ok(plan(&group, lineage_value, lineage_notes)?.map_or(NextPayout::Nothing, NextPayout::Pay));
    }

    let others_to_pay = unpaid.iter().any(|c| c.kind != ClaimKind::Fee);
    for (i, &claim) in unpaid.iter().enumerate() {
        if claim.kind == ClaimKind::Fee && others_to_pay {
            continue; // T-06: it rides with an exit
        }
        let group = match unit(claim, &by_id, &recipient, lineage_value, lineage_notes, now)? {
            Unit::Pay(group) => group,
            Unit::WaitUntil(t) => return Ok(NextPayout::WaitUntil(t)),
            Unit::Skip => continue,
        };
        // Dust: the claim cannot pay its own fee; it stays in the vault.
        if plan(&group, lineage_value, lineage_notes)?.is_none() {
            continue;
        }
        // T-06 and H-06: the vault's Fee claim rides first, then the later payable claims, in
        // queue order.
        let riders: Vec<&Claim> = unpaid
            .iter()
            .copied()
            .filter(|c| c.kind == ClaimKind::Fee && c.id != claim.id)
            .chain(unpaid[i + 1..].iter().copied().filter(|c| c.kind != ClaimKind::Fee))
            .collect();
        let mut group = with_followers(group, &riders, &by_id, &recipient, lineage_value, lineage_notes, now);
        payout_order(&mut group);
        return Ok(plan(&group, lineage_value, lineage_notes)?.map_or(NextPayout::Nothing, NextPayout::Pay));
    }
    Ok(NextPayout::Nothing)
}

/// The vault's main lineage, as [`next_vault_payout`] plans from it.
#[derive(Clone, Copy, Debug)]
pub struct MainLineage<'a> {
    /// Value of the unspent main-lineage notes.
    pub value: u64,
    /// Number of unspent main-lineage notes.
    pub notes: usize,
    /// The unpaid claims a commitment on Base already binds to the lineage note (RT-3).
    pub bound: &'a [ClaimId],
}

/// The vault's next payout: from its main lineage when one is payable there ([`next_payout`]),
/// else the first payable refund in Base order, from its own note (I30).
///
/// `main` is `None` when nothing may be paid from the main lineage (the vault was never activated,
/// or I10 does not hold for it); `refund_notes` lists the unspent notes of the vault's refunded
/// deposits, by tag and value. A refund is paid alone and without change, so its note's value must
/// be its amount (conservation, as the Registry checks it); a note of another value, or none, leaves
/// the refund unpaid.
pub fn next_vault_payout(
    claims: &[Claim],
    recipient: impl Fn(&Commitment) -> Option<Recipient>,
    main: Option<MainLineage<'_>>,
    refund_notes: &[([u8; 32], u64)],
    now: u64,
) -> Result<NextPayout, LedgerError> {
    let from_main = main.map(|m| next_payout(claims, &recipient, m.value, m.notes, now, m.bound));
    if matches!(from_main, Some(Ok(NextPayout::Pay(_)))) {
        return from_main.unwrap_or(Ok(NextPayout::Nothing));
    }
    let mut refunds: Vec<&Claim> = claims.iter().filter(|c| c.kind == ClaimKind::Refund && c.payable()).collect();
    refunds.sort_by_key(|c| queue_key(c));
    for claim in refunds {
        let Some(tag) = claim.refund_tag else { continue };
        let Some(&(_, value)) = refund_notes.iter().find(|(t, _)| *t == tag) else {
            continue;
        };
        let Some(rcpt) = payee(claim, &recipient) else { continue };
        if value != claim.amount {
            continue;
        }
        if let Some(mut plan) = plan(&[(claim, rcpt)], value, 1)? {
            plan.input = PayoutInput::Refund(tag);
            return Ok(NextPayout::Pay(plan));
        }
    }
    from_main.unwrap_or(Ok(NextPayout::Nothing))
}

/// What one claim brings to a payout, by the queue's rules.
enum Unit<'a> {
    /// Pay these claims (the claim, or a close pair with the borrower first).
    Pay(Vec<(&'a Claim, Recipient)>),
    /// A close pair waits for the market maker's recipient until this Unix time.
    WaitUntil(u64),
    /// Not payable now.
    Skip,
}

/// E1 (and Tier 3): two claims of one payout never share a receiver, nor a salt. Each claim's
/// reference is `keccak256(salt || txid)`, so a shared salt would give two claims one reference
/// and the Registry would refuse the payout's commitment (`ClaimRefAlreadyUsed`) for ever.
fn distinct(a: &Recipient, b: &Recipient) -> bool {
    a.receiver != b.receiver && a.salt != b.salt
}

/// The claims `claim` is paid with if a payout starts with it.
fn unit<'a>(
    claim: &'a Claim,
    by_id: &impl Fn(&ClaimId) -> Option<&'a Claim>,
    recipient: &impl Fn(&Claim) -> Option<Recipient>,
    lineage_value: u64,
    lineage_notes: usize,
    now: u64,
) -> Result<Unit<'a>, LedgerError> {
    // A market maker claim whose borrower claim is still unpaid is paid together with it
    // (the borrower bears the pair's fee); once the borrower is paid it is an ordinary
    // claim. H-3: once the grace period is over, a borrower claim that cannot be paid (its
    // recipient was never revealed, or it is too small to bear the fee) no longer holds the
    // market maker hostage: the market maker is paid alone and bears its own fee.
    if claim.kind == ClaimKind::CloseMarketMaker
        && let Some(user) = claim.partner.as_ref().and_then(by_id)
    {
        let grace_over = now >= user.created_at.saturating_add(MARKET_MAKER_GRACE_SECS);
        if !grace_over || borrower_payable(user, claim, recipient, lineage_value, lineage_notes)? {
            return Ok(Unit::Skip);
        }
    }
    // Without a recipient, a claim is set aside: its value stays in the vault and the
    // queue moves on (docs/ARCHITECTURE.md, payout queue).
    let Some(rcpt) = recipient(claim) else {
        return Ok(Unit::Skip);
    };
    let mut group: Vec<(&Claim, Recipient)> = vec![(claim, rcpt)];
    if claim.kind == ClaimKind::CloseUser
        && let Some(mm) = claim.partner.as_ref().and_then(by_id)
    {
        let grace_end = claim.created_at.saturating_add(MARKET_MAKER_GRACE_SECS);
        match recipient(mm) {
            // E1: one payout never pays the same receiver twice (nor names one salt twice); the
            // market maker then waits for the next payout.
            Some(mm_rcpt) if !distinct(&mm_rcpt, &rcpt) => {}
            Some(mm_rcpt) => group.push((mm, mm_rcpt)),
            None if now < grace_end => return Ok(Unit::WaitUntil(grace_end)),
            None => {} // grace over: pay the borrower alone
        }
    }
    Ok(Unit::Pay(group))
}

/// H-06: `group`, a payable start of a payout, extended with the claims of `riders` (the vault's
/// Fee claim, then the rest of the queue, in order) that the queue would pay on their own, as long
/// as the payout stays within [`MAX_CLAIMS_PER_PAYOUT`], pays every receiver and names every salt
/// once (E1), and its first claim still bears the fee of every output. A claim that does not fit is
/// skipped, not waited for: later claims can still ride along, so claims to one receiver delay only
/// each other. Riders pay no fee, except a Fee claim, which bears its own output's share (T-06).
fn with_followers<'a>(
    mut group: Vec<(&'a Claim, Recipient)>,
    riders: &[&'a Claim],
    by_id: &impl Fn(&ClaimId) -> Option<&'a Claim>,
    recipient: &impl Fn(&Claim) -> Option<Recipient>,
    lineage_value: u64,
    lineage_notes: usize,
    now: u64,
) -> Vec<(&'a Claim, Recipient)> {
    for &claim in riders {
        if group.len() >= MAX_CLAIMS_PER_PAYOUT {
            break;
        }
        // A close pair waiting for its market maker is not split: it waits for a later payout.
        let Ok(Unit::Pay(candidate)) = unit(claim, by_id, recipient, lineage_value, lineage_notes, now) else {
            continue;
        };
        let fits = group.len() + candidate.len() <= MAX_CLAIMS_PER_PAYOUT
            && candidate.iter().all(|(c, r)| group.iter().all(|(g, gr)| g.id != c.id && distinct(r, gr)))
            // Paid on its own, it would bear its fee: no dust rides on another claim's fee.
            && matches!(plan(&candidate, lineage_value, lineage_notes), Ok(Some(_)));
        if !fits {
            continue;
        }
        let mut extended = group.clone();
        extended.extend(candidate);
        if matches!(plan(&extended, lineage_value, lineage_notes), Ok(Some(_))) {
            group = extended;
        }
    }
    group
}

/// Whether the borrower's claim `user` of a close pair can be paid: its recipient is known and
/// it can bear the fee of the transaction that would pay it (with the market maker if known).
fn borrower_payable(
    user: &Claim,
    mm: &Claim,
    recipient: &impl Fn(&Claim) -> Option<Recipient>,
    lineage_value: u64,
    lineage_notes: usize,
) -> Result<bool, LedgerError> {
    let Some(user_rcpt) = recipient(user) else {
        return Ok(false);
    };
    let mut group = vec![(user, user_rcpt)];
    if let Some(mm_rcpt) = recipient(mm).filter(|r| distinct(r, &user_rcpt)) {
        group.push((mm, mm_rcpt));
    }
    Ok(plan(&group, lineage_value, lineage_notes)?.is_some())
}

/// Builds the plan paying `group` from the main lineage, in this order: its first claim (the
/// borrower in a close pair) bears the fee of every output, except that each Fee claim after it
/// bears exactly its own output's share, [`ZIP317_MARGINAL_FEE`] (T-06, as the Registry's
/// `commitSpend` computes each claim's `expected`). Returns `None` if a claim cannot bear its share
/// (dust).
fn plan(
    group: &[(&Claim, Recipient)],
    lineage_value: u64,
    lineage_notes: usize,
) -> Result<Option<PayoutPlan>, LedgerError> {
    if lineage_notes == 0 {
        return Err(LedgerError::NoInputs);
    }
    let claimed: u64 = group.iter().map(|(c, _)| c.amount).sum();
    if claimed > lineage_value {
        return Err(LedgerError::Overdrawn {
            claims: claimed,
            lineage: lineage_value,
        });
    }
    let change = lineage_value - claimed;
    let outputs = group.len() + usize::from(change > 0);
    let fee = zip317_fee(lineage_notes, outputs);
    let riding_fees = group.iter().skip(1).filter(|(c, _)| c.kind == ClaimKind::Fee).count();
    // ZIP 317 counts every output, so the fee always covers the riding Fee claims' shares.
    let head_share = fee - ZIP317_MARGINAL_FEE * riding_fees as u64;

    let mut payments = Vec::with_capacity(group.len());
    for (i, (claim, rcpt)) in group.iter().enumerate() {
        let share = match (i, claim.kind) {
            (0, _) => head_share,
            (_, ClaimKind::Fee) => ZIP317_MARGINAL_FEE,
            _ => 0,
        };
        let value = match claim.amount.checked_sub(share) {
            Some(v) if v > 0 => v,
            _ => return Ok(None),
        };
        payments.push(PlannedPayment {
            claim: claim.id,
            receiver: rcpt.receiver,
            value,
            privacy: claim.privacy(),
        });
    }
    Ok(Some(PayoutPlan {
        claims: group.iter().map(|(c, _)| c.id).collect(),
        payments,
        inputs: lineage_notes,
        change,
        fee,
        input: PayoutInput::Main,
    }))
}

/// T-05: the claims whose claimant the bonds will pay if the signers do not pay them within the
/// bonds' liveness period of their `created_at`: of a kind the bonds cover, pending with a
/// destination, not already paid by the bonds, and older than `age` at `now` (nodes warn at half
/// the period, so that the claims are paid long before the bonds have to).
pub fn overdue(claims: &[Claim], now: u64, age: u64) -> Vec<&Claim> {
    claims
        .iter()
        .filter(|c| c.kind.liveness_covered() && c.payable() && !c.bond_paid && now.saturating_sub(c.created_at) > age)
        .collect()
}

/// P5: refunds one deposit that no guest of the deployment can land on Base (note id `note_id`,
/// value `note_value`) to its depositor, net of the fee; no change. Never for a note a guest can
/// land: that one leaves the vault only as a Refund claim (I30, [`crate::classify::Landing`]).
pub fn refund_plan(note_id: [u8; 32], note_value: u64, receiver: [u8; 43]) -> Result<PayoutPlan, LedgerError> {
    let fee = zip317_fee(1, 1);
    let value = note_value
        .checked_sub(fee)
        .filter(|v| *v > 0)
        .ok_or(LedgerError::RefundIsDust { value: note_value, fee })?;
    Ok(PayoutPlan {
        claims: vec![],
        payments: vec![PlannedPayment {
            claim: [0; 32],
            receiver,
            value,
            privacy: OutputPrivacy::VaultOvk,
        }],
        inputs: 1,
        change: 0,
        fee,
        input: PayoutInput::Unlanded(note_id),
    })
}

mod hex43 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 43], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 43], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s)
            .map_err(serde::de::Error::custom)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("receiver must be 43 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DEPOSIT: u64 = 5_000_000;
    const FEE_1_IN_1_OUT: u64 = 10_000;
    const FEE_1_IN_2_OUT: u64 = 10_000;
    const FEE_1_IN_3_OUT: u64 = 15_000;

    fn rcpt(tag: u8) -> Recipient {
        Recipient {
            receiver: [tag; 43],
            salt: [tag.wrapping_add(100); 32],
        }
    }

    fn claim(id: u8, kind: ClaimKind, amount: u64, order: (u64, u64), partner: Option<u8>) -> Claim {
        Claim {
            id: [id; 32],
            kind,
            amount,
            commitment: rcpt(id).commitment(),
            order,
            created_at: 1_000,
            partner: partner.map(|p| [p; 32]),
            state: ClaimState::Unpaid,
            refund_tag: None,
            bond_paid: false,
        }
    }

    fn known(ids: &[u8]) -> impl Fn(&Commitment) -> Option<Recipient> {
        let map: HashMap<Commitment, Recipient> = ids.iter().map(|&i| (rcpt(i).commitment(), rcpt(i))).collect();
        move |c| map.get(c).copied()
    }

    /// RT-3: a claim that a commitment on Base binds to the lineage note is paid before an earlier
    /// claim whose recipient arrived later, so a vault never deadlocks on a bound claim.
    #[test]
    fn a_claim_bound_on_base_is_planned_before_an_earlier_payable_one() {
        let c0 = claim(5, ClaimKind::Liquidation, 1_000_000, (12, 0), None);
        let c1 = claim(6, ClaimKind::Liquidation, 1_500_000, (13, 0), None);
        let claims = [c0.clone(), c1.clone()];
        // c0's recipient is unknown: c1 is planned alone and committed on Base; its signing fails.
        let NextPayout::Pay(p1) = next_payout(&claims, known(&[6]), DEPOSIT, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p1.claims, vec![c1.id]);
        // c0's recipient arrives: c1, bound to the note, is still the payout.
        let NextPayout::Pay(p) = next_payout(&claims, known(&[5, 6]), DEPOSIT, 1, 3_000, &[c1.id]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![c1.id]);
        // Without the binding the queue order would start with c0 (and c1 would ride along, H-06).
        let NextPayout::Pay(p) = next_payout(&claims, known(&[5, 6]), DEPOSIT, 1, 3_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![c0.id, c1.id]);
        // A bound claim this signer does not know as unpaid, or whose recipient it lost: nothing.
        assert_eq!(next_payout(&claims, known(&[5, 6]), DEPOSIT, 1, 3_000, &[[9; 32]]).unwrap(), NextPayout::Nothing);
        assert_eq!(next_payout(&claims, known(&[5]), DEPOSIT, 1, 3_000, &[c1.id]).unwrap(), NextPayout::Nothing);
    }

    /// RT-3: a close pair bound on Base is paid as the pair, borrower first.
    #[test]
    fn a_bound_close_pair_is_paid_as_committed() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let NextPayout::Pay(p) =
            next_payout(&[mm.clone(), user.clone()], known(&[3, 4]), DEPOSIT, 1, 1_010, &[mm.id, user.id]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id, mm.id]);
    }

    /// `cast keccak` of the 64 bytes `0x11 * 32 || 0x42 * 32`.
    #[test]
    fn claim_ref_vector() {
        assert_eq!(
            hex::encode(claim_ref(&[0x11; 32], &[0x42; 32])),
            "51d4b707b5a706e51817d290a91ae40d0363befaad5013b6658f20cd87ddcdd6"
        );
    }

    #[test]
    fn fees_match_zip317() {
        assert_eq!(zip317_fee(1, 1), FEE_1_IN_1_OUT);
        assert_eq!(zip317_fee(1, 2), FEE_1_IN_2_OUT);
        assert_eq!(zip317_fee(1, 3), FEE_1_IN_3_OUT);
    }

    #[test]
    fn full_release_pays_borrower_minus_fee_without_change() {
        let c = claim(1, ClaimKind::Release, DEPOSIT, (10, 0), None);
        let got = next_payout(std::slice::from_ref(&c), known(&[1]), DEPOSIT, 1, 2_000, &[]).unwrap();
        let NextPayout::Pay(p) = got else { panic!("{got:?}") };
        assert_eq!(p.change, 0);
        assert_eq!(p.fee, FEE_1_IN_1_OUT);
        assert_eq!(p.payments.len(), 1);
        assert_eq!(p.payments[0].value, DEPOSIT - FEE_1_IN_1_OUT);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::VaultOvk);
    }

    #[test]
    fn partial_liquidation_keeps_the_rest_in_the_vault() {
        // ~70% seized at 62.5% LLTV; the rest stays outstanding.
        let seized = 3_520_000;
        let c = claim(2, ClaimKind::Liquidation, seized, (10, 1), None);
        let NextPayout::Pay(p) = next_payout(std::slice::from_ref(&c), known(&[2]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.payments[0].value, seized - FEE_1_IN_2_OUT);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::NoOvk);
        assert_eq!(p.change, DEPOSIT - seized);
        // I10 after the payout: lineage = change, outstanding = DEPOSIT - seized, no unpaid.
        let mut paid = c;
        paid.state = ClaimState::Paid;
        check_i10(p.change, DEPOSIT - seized, &[paid]).unwrap();
    }

    #[test]
    fn close_pays_both_in_one_tx_and_borrower_bears_the_fee() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let NextPayout::Pay(p) =
            next_payout(&[mm.clone(), user.clone()], known(&[3, 4]), DEPOSIT, 1, 1_010, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id, mm.id]);
        assert_eq!(p.change, 0);
        assert_eq!(p.fee, FEE_1_IN_2_OUT);
        assert_eq!(p.payments[0].value, 1_200_000 - FEE_1_IN_2_OUT);
        assert_eq!(p.payments[1].value, 3_800_000);
        assert_eq!(p.payments[1].privacy, OutputPrivacy::NoOvk);
    }

    /// E1: a close whose borrower and market maker share a receiver is paid in two payouts, so
    /// that no payout pays one receiver twice: first the borrower (bearing its own fee), then the
    /// market maker on its own.
    #[test]
    fn a_shared_receiver_is_never_paid_twice_in_one_payout() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mut mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let same = Recipient {
            receiver: [3; 43],
            salt: [9; 32],
        };
        mm.commitment = same.commitment();
        let map: HashMap<Commitment, Recipient> = [(rcpt(3).commitment(), rcpt(3)), (mm.commitment, same)].into();
        let recipient = |c: &Commitment| map.get(c).copied();
        let NextPayout::Pay(p) = next_payout(&[user.clone(), mm.clone()], recipient, DEPOSIT, 1, 1_010, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id], "the borrower alone");
        assert_eq!(p.payments[0].value, 1_200_000 - FEE_1_IN_2_OUT, "with change, two outputs");
        let paid = Claim {
            state: ClaimState::Paid,
            ..user
        };
        let NextPayout::Pay(p) =
            next_payout(&[paid, mm.clone()], recipient, DEPOSIT - 1_200_000, 1, 1_010, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![mm.id], "then the market maker");
    }

    #[test]
    fn close_waits_for_market_maker_then_pays_borrower_alone() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let claims = [user.clone(), mm.clone()];
        assert_eq!(
            next_payout(&claims, known(&[3]), DEPOSIT, 1, 1_050, &[]).unwrap(),
            NextPayout::WaitUntil(1_000 + MARKET_MAKER_GRACE_SECS)
        );
        let NextPayout::Pay(p) = next_payout(&claims, known(&[3]), DEPOSIT, 1, 1_200, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id]);
        // The market maker's share stays in the vault as change.
        assert_eq!(p.change, 3_800_000);
        assert_eq!(p.fee, FEE_1_IN_2_OUT);
    }

    #[test]
    fn late_market_maker_is_paid_alone_and_bears_its_fee() {
        let mut user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        user.state = ClaimState::Paid;
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let NextPayout::Pay(p) = next_payout(&[user, mm.clone()], known(&[4]), 3_800_000, 1, 5_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![mm.id]);
        assert_eq!(p.payments[0].value, 3_800_000 - FEE_1_IN_1_OUT);
        assert_eq!(p.change, 0);
    }

    #[test]
    fn claim_without_recipient_does_not_block_the_queue() {
        let a = claim(5, ClaimKind::Liquidation, 1_000_000, (12, 0), None);
        let b = claim(6, ClaimKind::Liquidation, 1_500_000, (13, 0), None);
        let NextPayout::Pay(p) = next_payout(&[a.clone(), b.clone()], known(&[6]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![b.id]);
        // a's value stays in the vault: change = deposit - b.
        assert_eq!(p.change, DEPOSIT - 1_500_000);
    }

    #[test]
    fn dust_claim_stays_in_the_vault() {
        let dust = claim(7, ClaimKind::Liquidation, FEE_1_IN_2_OUT, (12, 0), None);
        let ok = claim(8, ClaimKind::Liquidation, 2_000_000, (13, 0), None);
        let NextPayout::Pay(p) = next_payout(&[dust, ok.clone()], known(&[7, 8]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![ok.id]);
        assert_eq!(p.change, DEPOSIT - 2_000_000);
    }

    #[test]
    fn overdrawn_ledger_is_refused() {
        let c = claim(9, ClaimKind::Release, DEPOSIT + 1, (10, 0), None);
        assert_eq!(
            next_payout(&[c], known(&[9]), DEPOSIT, 1, 2_000, &[]),
            Err(LedgerError::Overdrawn {
                claims: DEPOSIT + 1,
                lineage: DEPOSIT
            })
        );
    }

    #[test]
    fn i10_detects_mismatch() {
        let c = claim(10, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        check_i10(DEPOSIT, DEPOSIT - 1_000_000, std::slice::from_ref(&c)).unwrap();
        assert!(check_i10(DEPOSIT - 1, DEPOSIT - 1_000_000, &[c]).is_err());
    }

    // ----- Registry v3: refunds by proof (T-02, I30), the opening fee (T-06), bonds (T-05),
    // disputes (T-05 (a)).

    const REFUND_TAG: [u8; 32] = [0xee; 32];

    /// A Refund claim of `amount` paid from the note `REFUND_TAG`, with a destination.
    fn refund(id: u8, amount: u64, order: u64) -> Claim {
        Claim {
            refund_tag: Some(REFUND_TAG),
            ..claim(id, ClaimKind::Refund, amount, (order, 0), None)
        }
    }

    fn main(value: u64, bound: &[ClaimId]) -> Option<MainLineage<'_>> {
        Some(MainLineage { value, notes: 1, bound })
    }

    /// I30: a refund is paid from its own note, alone, without change, bearing the fee of its one
    /// output; it never rides in a payout of the main lineage, nor does a main claim ride with it.
    #[test]
    fn a_refund_is_paid_from_its_own_note_alone() {
        let r = refund(7, 1_234_567, 20);
        let liq = claim(1, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        // The main lineage pays its own claim and nothing of the refund's.
        let claims = [liq.clone(), r.clone()];
        let NextPayout::Pay(p) =
            next_vault_payout(&claims, known(&[1, 7]), main(DEPOSIT, &[]), &[(REFUND_TAG, 1_234_567)], 2_000).unwrap()
        else {
            panic!()
        };
        assert_eq!((p.claims.clone(), p.input), (vec![liq.id], PayoutInput::Main));
        assert_eq!(p.change, DEPOSIT - 1_000_000);
        // Once the main lineage has nothing to pay, the refund is next, from its own note.
        let claims = [
            Claim {
                state: ClaimState::Paid,
                ..liq
            },
            r.clone(),
        ];
        let NextPayout::Pay(p) = next_vault_payout(
            &claims,
            known(&[1, 7]),
            main(DEPOSIT - 1_000_000, &[]),
            &[(REFUND_TAG, 1_234_567)],
            2_000,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!((p.claims.clone(), p.input), (vec![r.id], PayoutInput::Refund(REFUND_TAG)));
        assert_eq!((p.inputs, p.change, p.fee), (1, 0, FEE_1_IN_1_OUT));
        assert_eq!(p.payments[0].value, 1_234_567 - FEE_1_IN_1_OUT);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::VaultOvk, "the borrower's own output");
        // A cancelled vault has no main lineage at all: its refunds are still paid.
        let NextPayout::Pay(p) =
            next_vault_payout(&[r.clone()], known(&[7]), None, &[(REFUND_TAG, 1_234_567)], 2_000).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.input, PayoutInput::Refund(REFUND_TAG));
        // The main planner never pays a refund, even with the lineage's value to spare.
        assert_eq!(next_payout(&[r.clone()], known(&[7]), DEPOSIT, 1, 2_000, &[]).unwrap(), NextPayout::Nothing);
    }

    /// T-02: a refund is not paid without its destination, without its note, from a note of
    /// another value (conservation), when written off (abandoned) or when it cannot bear its fee.
    #[test]
    fn a_refund_waits_for_its_destination_and_its_note() {
        let r = refund(7, 1_234_567, 20);
        let pay = |c: &Claim, notes: &[([u8; 32], u64)]| {
            next_vault_payout(std::slice::from_ref(c), known(&[7]), None, notes, 2_000).unwrap()
        };
        let note = [(REFUND_TAG, 1_234_567)];
        assert!(matches!(pay(&r, &note), NextPayout::Pay(_)));
        let no_destination = Claim {
            commitment: [0; 32],
            ..r.clone()
        };
        assert_eq!(pay(&no_destination, &note), NextPayout::Nothing);
        assert_eq!(pay(&r, &[]), NextPayout::Nothing, "its note is not seen (or spent)");
        assert_eq!(pay(&r, &[(REFUND_TAG, 1_234_566)]), NextPayout::Nothing, "a note of another value");
        assert_eq!(pay(&r, &[([0xef; 32], 1_234_567)]), NextPayout::Nothing, "another refund's note");
        let abandoned = Claim {
            state: ClaimState::WrittenOff,
            ..r.clone()
        };
        assert_eq!(pay(&abandoned, &note), NextPayout::Nothing);
        let dust = refund(7, FEE_1_IN_1_OUT, 20);
        assert_eq!(pay(&dust, &[(REFUND_TAG, FEE_1_IN_1_OUT)]), NextPayout::Nothing);
        // Two refunds: the first in Base order with a destination goes first.
        let other_tag = [0xef; 32];
        let r2 = Claim {
            refund_tag: Some(other_tag),
            ..refund(8, 500_000, 10)
        };
        let NextPayout::Pay(p) = next_vault_payout(
            &[r.clone(), r2.clone()],
            known(&[7, 8]),
            None,
            &[(REFUND_TAG, 1_234_567), (other_tag, 500_000)],
            2_000,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!((p.claims, p.input), (vec![r2.id], PayoutInput::Refund(other_tag)));
    }

    /// The main lineage goes first; when it can only wait (a close pair within its grace period) or
    /// cannot be planned from, a payable refund is paid meanwhile.
    #[test]
    fn a_refund_does_not_wait_for_the_main_lineage() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let r = refund(7, 600_000, 30);
        let claims = [user, mm, r.clone()];
        let notes = [(REFUND_TAG, 600_000)];
        let NextPayout::Pay(p) = next_vault_payout(&claims, known(&[3, 7]), main(DEPOSIT, &[]), &notes, 1_050).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![r.id]);
        // Without the refund, the main lineage still waits.
        assert_eq!(
            next_vault_payout(&claims[..2], known(&[3]), main(DEPOSIT, &[]), &[], 1_050).unwrap(),
            NextPayout::WaitUntil(1_000 + MARKET_MAKER_GRACE_SECS)
        );
        // A main lineage that overdraws is an error, unless a refund can be paid meanwhile.
        let big = claim(5, ClaimKind::Release, DEPOSIT + 1, (40, 0), None);
        assert!(next_vault_payout(std::slice::from_ref(&big), known(&[5]), main(DEPOSIT, &[]), &[], 2_000).is_err());
        let NextPayout::Pay(p) =
            next_vault_payout(&[big, r.clone()], known(&[5, 7]), main(DEPOSIT, &[]), &notes, 2_000).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![r.id]);
    }

    /// The opening fee's claim, owed to the signers' treasury.
    fn fee(id: u8, amount: u64) -> Claim {
        claim(id, ClaimKind::Fee, amount, (5, 1), None)
    }

    /// T-06: the Fee claim rides with the vault's first exit, after its first claim, and bears
    /// exactly its own output's share of the ZIP 317 fee; the first claim bears the rest. The plan
    /// pays exactly what the Registry's `commitSpend` records as each claim's `expected`.
    #[test]
    fn the_fee_rides_with_the_first_exit() {
        let f = fee(9, 25_000);
        let rel = claim(1, ClaimKind::Release, DEPOSIT - 25_000, (20, 0), None);
        let NextPayout::Pay(p) =
            next_payout(&[f.clone(), rel.clone()], known(&[1, 9]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![rel.id, f.id], "the fee after the exit");
        assert_eq!((p.change, p.fee), (0, FEE_1_IN_2_OUT));
        assert_eq!(p.payments[0].value, rel.amount - (FEE_1_IN_2_OUT - ZIP317_MARGINAL_FEE));
        assert_eq!(p.payments[1].value, 25_000 - ZIP317_MARGINAL_FEE, "the fee claim bears its own output");
        assert_eq!(p.payments[1].privacy, OutputPrivacy::NoOvk, "the treasury is a third party");
        assert_eq!(p.payments.iter().map(|x| x.value).sum::<u64>() + p.fee + p.change, DEPOSIT);

        // With change and other riders: three claims and the change are four outputs; the fee claim
        // still bears 5,000 and is listed last.
        let liq = claim(2, ClaimKind::Liquidation, 1_000_000, (21, 0), None);
        let part = claim(3, ClaimKind::Release, 500_000, (22, 0), None);
        let NextPayout::Pay(p) =
            next_payout(&[f.clone(), liq.clone(), part.clone()], known(&[2, 3, 9]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![liq.id, part.id, f.id]);
        assert_eq!(p.fee, zip317_fee(1, 4));
        assert_eq!(p.payments[0].value, 1_000_000 - (p.fee - ZIP317_MARGINAL_FEE));
        assert_eq!(p.payments[1].value, 500_000);
        assert_eq!(p.payments[2].value, 25_000 - ZIP317_MARGINAL_FEE);
        assert_eq!(p.change, DEPOSIT - 1_525_000);

        // Its recipient unknown (the treasury not registered with this signer): the exit is paid
        // without it, and the fee stays in the vault as change.
        let NextPayout::Pay(p) = next_payout(&[f.clone(), liq.clone()], known(&[2]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![liq.id]);
        assert_eq!(p.change, DEPOSIT - 1_000_000);
    }

    /// T-06: the Fee claim never heads a payout while another claim of the vault is left to pay
    /// (it would bear the whole fee); once it is the last one, it is paid alone so that the vault
    /// can settle.
    #[test]
    fn the_fee_is_paid_alone_only_when_nothing_else_is_left() {
        let f = fee(9, 25_000);
        let unknown = claim(1, ClaimKind::Liquidation, 1_000_000, (20, 0), None);
        // The other claim has no recipient yet: nothing is paid, not the fee alone.
        assert_eq!(
            next_payout(&[f.clone(), unknown.clone()], known(&[9]), DEPOSIT, 1, 2_000, &[]).unwrap(),
            NextPayout::Nothing
        );
        // Everything else paid: the fee alone, bearing the whole fee.
        let paid = Claim {
            state: ClaimState::Paid,
            ..unknown
        };
        let NextPayout::Pay(p) = next_payout(&[f.clone(), paid], known(&[9]), 25_000, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![f.id]);
        assert_eq!(p.payments[0].value, 25_000 - FEE_1_IN_1_OUT);
        assert_eq!(p.change, 0);
    }

    /// RT-3 / RT-4: a payout rebuilt from the claims a commitment binds lists them as it was
    /// committed, the fee claim last, whatever order Base lists the binding in.
    #[test]
    fn a_rebuilt_payout_with_the_fee_keeps_its_order() {
        let f = fee(9, 25_000);
        let liq = claim(2, ClaimKind::Liquidation, 1_000_000, (21, 0), None);
        let NextPayout::Pay(first) =
            next_payout(&[f.clone(), liq.clone()], known(&[2, 9]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        for bound in [vec![f.id, liq.id], vec![liq.id, f.id]] {
            let rebuilt = next_payout(&[f.clone(), liq.clone()], known(&[2, 9]), DEPOSIT, 1, 9_000, &bound).unwrap();
            assert_eq!(rebuilt, NextPayout::Pay(first.clone()));
        }
        assert_eq!(claim_set_hash(&first.claims), claim_set_hash(&[liq.id, f.id]));
    }

    /// T-05: a bond-paid claim has no destination until three signers give its Reimburse
    /// destination; then it is paid there, without the vault's outgoing viewing key.
    #[test]
    fn a_bond_paid_claim_is_paid_to_its_reimburse_destination() {
        let release = claim(1, ClaimKind::Release, 1_000_000, (20, 0), None);
        let cleared = Claim {
            bond_paid: true,
            commitment: [0; 32],
            ..release.clone()
        };
        assert_eq!(
            next_payout(std::slice::from_ref(&cleared), known(&[1]), 1_000_000, 1, 2_000, &[]).unwrap(),
            NextPayout::Nothing
        );
        let reimburse = rcpt(42);
        let pointed = Claim {
            commitment: reimburse.commitment(),
            ..cleared
        };
        let NextPayout::Pay(p) = next_payout(&[pointed], known(&[42]), 1_000_000, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.payments[0].receiver, reimburse.receiver);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::NoOvk, "the bonds' destination, not the borrower's");
    }

    /// I10 counts what the main lineage still backs: pending, written-off and disputed main claims;
    /// not refunds (their own notes back them), nor paid, compensated or retired claims.
    #[test]
    fn the_main_lineage_backs_only_its_own_unpaid_claims() {
        let with = |id: u8, kind: ClaimKind, state: ClaimState| Claim {
            state,
            ..claim(id, kind, 100_000, (10, u64::from(id)), None)
        };
        let claims = [
            with(1, ClaimKind::Liquidation, ClaimState::Unpaid),
            with(2, ClaimKind::Liquidation, ClaimState::WrittenOff),
            with(3, ClaimKind::Release, ClaimState::Disputed),
            with(4, ClaimKind::Fee, ClaimState::Unpaid),
            with(5, ClaimKind::Refund, ClaimState::Unpaid),
            with(6, ClaimKind::Liquidation, ClaimState::Paid),
            with(7, ClaimKind::Liquidation, ClaimState::Compensated),
            with(8, ClaimKind::Retired, ClaimState::Compensated),
        ];
        assert_eq!(book_value(1_000_000, &claims), 1_400_000);
        check_i10(1_400_000, 1_000_000, &claims).unwrap();
        assert!(check_i10(1_500_000, 1_000_000, &claims).is_err());
    }

    /// T-05 (a): a disputed claim is unpaid again but not payable until it is reopened; reopened,
    /// it is pending and bound to its input, so it is paid by a payout of that input only.
    #[test]
    fn a_disputed_claim_is_not_payable_until_reopened() {
        let c = claim(1, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        let disputed = Claim {
            state: ClaimState::Disputed,
            ..c.clone()
        };
        assert!(!disputed.payable() && disputed.in_books());
        assert_eq!(
            next_payout(std::slice::from_ref(&disputed), known(&[1]), DEPOSIT, 1, 2_000, &[]).unwrap(),
            NextPayout::Nothing
        );
        let NextPayout::Pay(p) = next_payout(&[c.clone()], known(&[1]), DEPOSIT, 1, 2_000, &[c.id]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![c.id]);
    }

    /// T-05: the payable claims a node warns about, before the bonds would pay their claimants.
    #[test]
    fn overdue_claims_are_the_payable_ones_the_bonds_cover() {
        let at = |id: u8, kind: ClaimKind, created_at: u64| Claim {
            created_at,
            ..claim(id, kind, 100_000, (10, u64::from(id)), None)
        };
        let claims = [
            at(1, ClaimKind::Liquidation, 1_000),
            at(2, ClaimKind::Release, 50_000),
            at(3, ClaimKind::Fee, 1_000),
            Claim {
                commitment: [0; 32],
                ..at(4, ClaimKind::Refund, 1_000)
            },
            Claim {
                bond_paid: true,
                ..at(5, ClaimKind::CloseUser, 1_000)
            },
            Claim {
                state: ClaimState::Paid,
                ..at(6, ClaimKind::CloseMarketMaker, 1_000)
            },
            at(7, ClaimKind::Refund, 1_000),
        ];
        let ids: Vec<u8> = overdue(&claims, 60_000, 43_200).iter().map(|c| c.id[0]).collect();
        assert_eq!(ids, vec![1, 7]);
    }

    /// P5: an off-chain refund is the note less the fee of one input and one output, to one
    /// receiver, without change; a note that cannot pay its own fee stays in the vault.
    #[test]
    fn refund_is_net_of_fee_and_rejects_dust() {
        let p = refund_plan([7; 32], 1_234_567, [1; 43]).unwrap();
        assert_eq!(p.payments[0].value, 1_234_567 - FEE_1_IN_1_OUT);
        assert_eq!((p.change, p.inputs, p.input), (0, 1, PayoutInput::Unlanded([7; 32])));
        assert!(p.claims.is_empty());
        assert!(refund_plan([7; 32], FEE_1_IN_1_OUT, [1; 43]).is_err());
    }

    #[test]
    fn commitment_is_keccak_of_salt_then_receiver() {
        let r = Recipient {
            receiver: [0xab; 43],
            salt: [0x01; 32],
        };
        let mut preimage = vec![0x01; 32];
        preimage.extend([0xab; 43]);
        assert_eq!(r.commitment(), <[u8; 32]>::from(Keccak256::digest(&preimage)));
    }

    // ----- H-3 (security review 2026-10-11): the market maker is paid even if the borrower's
    // close claim is never revealed or is dust. Adapted from the reviewer's PoCs.

    const SMALL_DEPOSIT: u64 = 1_000_000; // 0.01 TAZ vault

    #[test]
    fn market_maker_paid_alone_after_grace_if_user_commitment_never_revealed() {
        let user = claim(3, ClaimKind::CloseUser, 300_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 700_000, (11, 0), Some(3));
        let claims = [user.clone(), mm.clone()];
        // Within the grace period the market maker waits for the borrower's recipient.
        for now in [1_000, 1_050, 1_000 + MARKET_MAKER_GRACE_SECS - 1] {
            assert_eq!(next_payout(&claims, known(&[4]), SMALL_DEPOSIT, 1, now, &[]).unwrap(), NextPayout::Nothing);
        }
        for now in [1_000 + MARKET_MAKER_GRACE_SECS, 1_200, 10_000, 10_000_000] {
            let NextPayout::Pay(p) = next_payout(&claims, known(&[4]), SMALL_DEPOSIT, 1, now, &[]).unwrap() else {
                panic!("market maker not paid at {now}");
            };
            assert_eq!(p.claims, vec![mm.id]);
            // It bears its own fee; the borrower's share stays in the vault as change.
            assert_eq!(p.payments.len(), 1);
            assert_eq!(p.payments[0].value, 700_000 - FEE_1_IN_2_OUT);
            assert_eq!(p.payments[0].privacy, OutputPrivacy::NoOvk);
            assert_eq!(p.change, 300_000);
        }
    }

    #[test]
    fn market_maker_paid_alone_after_grace_if_user_share_is_dust() {
        for user_amount in [0u64, 5_000, FEE_1_IN_2_OUT] {
            let user = claim(3, ClaimKind::CloseUser, user_amount, (11, 0), Some(4));
            let mm = claim(4, ClaimKind::CloseMarketMaker, SMALL_DEPOSIT - user_amount, (11, 0), Some(3));
            let claims = [user.clone(), mm.clone()];
            assert_eq!(
                next_payout(&claims, known(&[3, 4]), SMALL_DEPOSIT, 1, 1_050, &[]).unwrap(),
                NextPayout::Nothing,
                "user_amount = {user_amount}: within the grace period"
            );
            let NextPayout::Pay(p) = next_payout(&claims, known(&[3, 4]), SMALL_DEPOSIT, 1, 10_000, &[]).unwrap()
            else {
                panic!("market maker not paid, user_amount = {user_amount}");
            };
            assert_eq!(p.claims, vec![mm.id]);
            assert_eq!(p.payments[0].value, SMALL_DEPOSIT - user_amount - FEE_1_IN_2_OUT);
            assert_eq!(p.change, user_amount);
            // Once the market maker is paid, I10 still holds with the dust claim unpaid.
            let mut mm_paid = mm.clone();
            mm_paid.state = ClaimState::Paid;
            check_i10(p.change, 0, &[user, mm_paid]).unwrap();
        }
    }

    #[test]
    fn payable_borrower_still_pays_the_close_pair_after_grace() {
        let user = claim(3, ClaimKind::CloseUser, 300_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 700_000, (11, 0), Some(3));
        let NextPayout::Pay(p) =
            next_payout(&[mm.clone(), user.clone()], known(&[3, 4]), SMALL_DEPOSIT, 1, 10_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id, mm.id]);
        assert_eq!(p.payments[0].value, 300_000 - FEE_1_IN_2_OUT);
        assert_eq!(p.payments[1].value, 700_000);
    }

    // ----- H-2: only the vault's own change joins the lineage. Adapted from the reviewer's
    // PoCs `foreign_internal_note_bricks_i10` and
    // `zero_value_internal_notes_inflate_fee_until_claims_are_dust`.

    fn note(tag: u8, value: u64, internal: bool, created_in: u8, spent_in: Option<u8>) -> VaultNote {
        VaultNote {
            nullifier: [tag; 32],
            value,
            internal,
            created_in: [created_in; 32],
            spent_in: spent_in.map(|t| [t; 32]),
        }
    }

    /// The unspent lineage selected from `notes`, then filtered by the planned change.
    fn unspent_lineage(conforming: u8, notes: &[VaultNote], book: u64) -> Vec<VaultNote> {
        let member = lineage_members(Some(&[conforming; 32]), notes);
        let mut lineage: Vec<VaultNote> =
            notes.iter().zip(member).filter(|(n, is)| *is && n.spent_in.is_none()).map(|(n, _)| *n).collect();
        retain_planned_change(&mut lineage, book, |n| n.internal, |n| n.value);
        lineage
    }

    #[test]
    fn foreign_internal_note_does_not_join_the_lineage() {
        // Deposit (tx 1), spent by a liquidation payout (tx 2) whose change is note 11; then
        // the borrower sends 1 zat to the vault's internal address (tx 9).
        let liq = claim(2, ClaimKind::Liquidation, 700_000, (10, 1), None);
        let outstanding = SMALL_DEPOSIT - 700_000;
        let notes = [
            note(10, SMALL_DEPOSIT, false, 1, Some(2)),
            note(11, outstanding, true, 2, None),
            note(12, 1, true, 9, None),
        ];
        let mut liq_paid = liq.clone();
        liq_paid.state = ClaimState::Paid;
        let book = book_value(outstanding, std::slice::from_ref(&liq_paid));
        let lineage = unspent_lineage(10, &notes, book);
        assert_eq!(lineage, vec![notes[1]], "the 1-zat note is foreign");
        let value: u64 = lineage.iter().map(|n| n.value).sum();
        check_i10(value, outstanding, &[liq_paid]).unwrap();
    }

    #[test]
    fn zero_value_internal_notes_do_not_inflate_the_fee() {
        // A first liquidation (tx 2) spent the deposit; its change is note 11. Then 140
        // zero-value notes reach the internal address: half from strangers (tx 9), half as
        // extra zero-value outputs of the vault transaction itself (signers only check outputs
        // that carry value).
        let mut first = claim(1, ClaimKind::Liquidation, 200_000, (9, 0), None);
        first.state = ClaimState::Paid;
        let second = claim(2, ClaimKind::Liquidation, 700_000, (10, 1), None);
        let change = SMALL_DEPOSIT - 200_000;
        let mut notes = vec![
            note(10, SMALL_DEPOSIT, false, 1, Some(2)),
            note(11, change, true, 2, None),
        ];
        notes.extend((0..140u8).map(|i| note(100 + i, 0, true, if i % 2 == 0 { 2 } else { 9 }, None)));
        let claims = [first, second.clone()];
        let outstanding = change - 700_000;

        let lineage = unspent_lineage(10, &notes, book_value(outstanding, &claims));
        assert_eq!(lineage, vec![notes[1]]);
        let value: u64 = lineage.iter().map(|n| n.value).sum();
        check_i10(value, outstanding, &claims).unwrap();
        let NextPayout::Pay(p) = next_payout(&claims, known(&[2]), value, lineage.len(), 2_000, &[]).unwrap() else {
            panic!("the claim must stay payable")
        };
        assert_eq!(p.claims, vec![second.id]);
        assert_eq!(p.fee, FEE_1_IN_2_OUT, "one input, not 141");
    }

    #[test]
    fn only_the_planned_change_of_a_vault_transaction_is_lineage() {
        // A payout (tx 2) spends the deposit and creates two internal notes: the change (worth
        // the book value once its claim is paid) and a payment to the vault's own internal
        // address. Later change notes chain through spends of earlier ones (tx 3).
        let notes = [
            note(10, SMALL_DEPOSIT, false, 1, Some(2)),
            note(11, 400_000, true, 2, Some(3)),
            note(12, 590_000, true, 2, None),
            note(13, 150_000, true, 3, None),
        ];
        let member = lineage_members(Some(&[10; 32]), &notes);
        assert_eq!(member, vec![true, true, true, true]);
        assert_eq!(unspent_lineage(10, &notes, 150_000), vec![notes[3]]);
        // Until the last payout's claims are marked paid, the book value is still the old one:
        // no note qualifies, so I10 fails and signers refuse to plan another payout.
        assert!(unspent_lineage(10, &notes, 400_000).is_empty());
        // No conforming deposit: nothing is lineage.
        assert!(lineage_members(None, &notes[1..]).iter().all(|m| !m));
    }

    // ----- H-06: a payout settles up to eight claims of the vault, the first bearing the fee.

    /// A claim paid to `r` (its commitment is `r`'s).
    fn claim_to(id: u8, kind: ClaimKind, amount: u64, order: u64, r: Recipient) -> Claim {
        Claim {
            commitment: r.commitment(),
            ..claim(id, kind, amount, (order, 0), None)
        }
    }

    /// The recipients `rs`, looked up by commitment.
    fn revealed(rs: &[Recipient]) -> impl Fn(&Commitment) -> Option<Recipient> + Clone + use<> {
        let map: HashMap<Commitment, Recipient> = rs.iter().map(|r| (r.commitment(), *r)).collect();
        move |c| map.get(c).copied()
    }

    fn paid(c: &Claim) -> Claim {
        Claim {
            state: ClaimState::Paid,
            ..c.clone()
        }
    }

    fn ids(claims: &[&Claim]) -> Vec<ClaimId> {
        claims.iter().map(|c| c.id).collect()
    }

    #[test]
    fn many_claims_of_a_vault_are_paid_together() {
        let claims: Vec<Claim> = (1..=10u8)
            .map(|i| claim(i, ClaimKind::Liquidation, 200_000, (10 + u64::from(i), 0), None))
            .collect();
        let all: Vec<u8> = (1..=10).collect();
        let NextPayout::Pay(p) = next_payout(&claims, known(&all), DEPOSIT, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, ids(&claims.iter().take(MAX_CLAIMS_PER_PAYOUT).collect::<Vec<_>>()), "the first eight");
        // ZIP 317 with one input: 8 claims and the change are 9 actions.
        assert_eq!(p.fee, zip317_fee(1, 9));
        assert_eq!(p.fee, 45_000);
        assert_eq!(p.payments[0].value, 200_000 - p.fee, "the first claim bears the fee");
        assert!(p.payments[1..].iter().all(|x| x.value == 200_000), "the others are paid in full");
        assert_eq!(p.change, DEPOSIT - 8 * 200_000);
        assert_eq!(p.payments.iter().map(|x| x.value).sum::<u64>() + p.fee + p.change, DEPOSIT);
        // Once they are paid, the books still add up and the next payout settles the last two.
        let later: Vec<Claim> =
            claims.iter().enumerate().map(|(i, c)| if i < 8 { paid(c) } else { c.clone() }).collect();
        let outstanding = DEPOSIT - 10 * 200_000;
        check_i10(p.change, outstanding, &later).unwrap();
        let NextPayout::Pay(p2) = next_payout(&later, known(&all), p.change, 1, 3_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p2.claims, vec![claims[8].id, claims[9].id]);
        assert_eq!(p2.fee, zip317_fee(1, 3));
        assert_eq!(p2.change, outstanding);
    }

    /// The review's H-06 scenario: a liquidation split into 70 claims to one receiver. They can only
    /// be paid one per payout (E1), but they no longer hold back the claims of others: the real
    /// liquidator and the borrower are paid in the first payout, not after seventy.
    #[test]
    fn a_split_liquidation_to_one_receiver_holds_back_only_itself() {
        const VAULT: u64 = 12_500_000;
        let attacker = rcpt(200);
        let mut claims: Vec<Claim> =
            (1..=70u8).map(|i| claim_to(i, ClaimKind::Liquidation, 100_000, u64::from(i), attacker)).collect();
        let liquidator = claim_to(71, ClaimKind::Liquidation, 3_000_000, 71, rcpt(71));
        let release = claim_to(72, ClaimKind::Release, 2_500_000, 72, rcpt(72));
        claims.extend([liquidator.clone(), release.clone()]);
        let recipient = revealed(&[attacker, rcpt(71), rcpt(72)]);
        let NextPayout::Pay(p) = next_payout(&claims, recipient.clone(), VAULT, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![claims[0].id, liquidator.id, release.id]);
        assert_eq!(p.fee, zip317_fee(1, 4), "three claims and the change");
        assert_eq!(p.payments[0].value, 100_000 - p.fee, "the attacker's first claim bears the fee");
        assert_eq!((p.payments[1].value, p.payments[2].value), (3_000_000, 2_500_000));
        assert_eq!(p.payments[2].privacy, OutputPrivacy::VaultOvk, "each output keeps its claim's privacy");
        assert_eq!(p.payments[1].privacy, OutputPrivacy::NoOvk);
        // The next payout: the attacker's second claim, alone.
        let mut next = claims.clone();
        for c in next.iter_mut().filter(|c| p.claims.contains(&c.id)) {
            c.state = ClaimState::Paid;
        }
        let NextPayout::Pay(p2) = next_payout(&next, recipient, p.change, 1, 3_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p2.claims, vec![claims[1].id]);
    }

    /// E1 for Tier 3: a claim's reference is `keccak256(salt || txid)`, so two claims with one salt
    /// in a payout would share a reference and the Registry would refuse its commitment for ever.
    /// Claims with one salt are paid in different payouts, close pairs included.
    #[test]
    fn claims_sharing_a_salt_are_never_paid_together() {
        let a = Recipient {
            receiver: [1; 43],
            salt: [9; 32],
        };
        let b = Recipient {
            receiver: [2; 43],
            salt: [9; 32],
        };
        let c = Recipient {
            receiver: [3; 43],
            salt: [8; 32],
        };
        let claims = [
            claim_to(1, ClaimKind::Liquidation, 1_000_000, 10, a),
            claim_to(2, ClaimKind::Liquidation, 1_000_000, 11, b),
            claim_to(3, ClaimKind::Liquidation, 1_000_000, 12, c),
        ];
        let recipient = revealed(&[a, b, c]);
        let NextPayout::Pay(p) = next_payout(&claims, recipient.clone(), DEPOSIT, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![claims[0].id, claims[2].id]);
        let rest = [paid(&claims[0]), claims[1].clone(), paid(&claims[2])];
        let NextPayout::Pay(p) = next_payout(&rest, recipient, p.change, 1, 2_000, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![claims[1].id]);

        // A close whose borrower and market maker share a salt: the borrower first, then the market maker.
        let mut user = claim_to(3, ClaimKind::CloseUser, 1_200_000, 11, a);
        let mut mm = claim_to(4, ClaimKind::CloseMarketMaker, 3_800_000, 11, b);
        user.partner = Some(mm.id);
        mm.partner = Some(user.id);
        let recipient = revealed(&[a, b]);
        let NextPayout::Pay(p) =
            next_payout(&[user.clone(), mm.clone()], recipient.clone(), DEPOSIT, 1, 1_010, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id]);
        let NextPayout::Pay(p) =
            next_payout(&[paid(&user), mm.clone()], recipient, DEPOSIT - 1_200_000, 1, 1_010, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![mm.id]);
    }

    /// The first claim bears the fee of every output, so a claim rides along only while the first
    /// one stays above it; and a claim too small to bear its own fee never rides on another's.
    #[test]
    fn riding_along_never_makes_dust_of_the_first_claim_nor_pays_dust() {
        // 12,000 zat bear the 10,000 of a payout alone, not the 15,000 of two claims and change.
        let small = claim(1, ClaimKind::Liquidation, 12_000, (10, 0), None);
        let big = claim(2, ClaimKind::Liquidation, 2_000_000, (11, 0), None);
        let NextPayout::Pay(p) =
            next_payout(&[small.clone(), big.clone()], known(&[1, 2]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![small.id]);
        assert_eq!(p.payments[0].value, 2_000);
        // A dust claim (no more than its own fee) is not paid by the first claim either; later
        // claims still ride along.
        let head = claim(1, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        let dust = claim(2, ClaimKind::Liquidation, FEE_1_IN_2_OUT, (11, 0), None);
        let later = claim(3, ClaimKind::Liquidation, 500_000, (12, 0), None);
        let NextPayout::Pay(p) =
            next_payout(&[head.clone(), dust, later.clone()], known(&[1, 2, 3]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![head.id, later.id]);
        // Whatever rides along, the first claim pays at most the fee of nine outputs.
        assert!(p.fee <= zip317_fee(1, MAX_CLAIMS_PER_PAYOUT + 1));
    }

    /// A close pair rides along whole or waits: within the grace period without the market maker's
    /// recipient it is not split (and, behind another claim, it holds nothing up); after it the
    /// borrower rides along alone; a pair that would pay a receiver of the payout twice waits.
    #[test]
    fn a_close_pair_rides_along_whole_or_waits() {
        let liq = claim(1, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 1_800_000, (11, 0), Some(3));
        let liq2 = claim(5, ClaimKind::Liquidation, 500_000, (12, 0), None);
        let claims = [liq.clone(), user.clone(), mm.clone(), liq2.clone()];
        let pay = |rcpts: &[u8], now: u64| match next_payout(&claims, known(rcpts), DEPOSIT, 1, now, &[]).unwrap() {
            NextPayout::Pay(p) => p.claims,
            other => panic!("{other:?}"),
        };
        // user.created_at = 1,000: the grace period ends at 1,120.
        assert_eq!(pay(&[1, 3, 5], 1_050), vec![liq.id, liq2.id]);
        assert_eq!(pay(&[1, 3, 5], 1_200), vec![liq.id, user.id, liq2.id]);
        assert_eq!(pay(&[1, 3, 4, 5], 1_050), vec![liq.id, user.id, mm.id, liq2.id]);
        // The market maker shares the first claim's receiver: the pair waits for a later payout.
        let mut shared = claims.clone();
        shared[2].commitment = Recipient {
            receiver: [1; 43],
            salt: [77; 32],
        }
        .commitment();
        let mut rcpts: Vec<Recipient> = [1u8, 3, 5].iter().map(|&i| rcpt(i)).collect();
        rcpts.push(Recipient {
            receiver: [1; 43],
            salt: [77; 32],
        });
        let NextPayout::Pay(p) = next_payout(&shared, revealed(&rcpts), DEPOSIT, 1, 1_050, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![liq.id, liq2.id]);
    }

    /// A close pair counts two claims: one that no longer fits is skipped and a later claim still
    /// rides along, up to eight.
    #[test]
    fn a_pair_counts_two_against_the_limit() {
        let mut claims: Vec<Claim> =
            (1..=7u8).map(|i| claim(i, ClaimKind::Liquidation, 100_000, (u64::from(i), 0), None)).collect();
        claims.push(claim(20, ClaimKind::CloseUser, 1_000_000, (20, 0), Some(21)));
        claims.push(claim(21, ClaimKind::CloseMarketMaker, 1_000_000, (20, 0), Some(20)));
        claims.push(claim(30, ClaimKind::Liquidation, 100_000, (30, 0), None));
        let all: Vec<u8> = (1..=7).chain([20, 21, 30]).collect();
        let NextPayout::Pay(p) = next_payout(&claims, known(&all), DEPOSIT, 1, 1_050, &[]).unwrap() else {
            panic!()
        };
        let mut want: Vec<ClaimId> = claims[..7].iter().map(|c| c.id).collect();
        want.push([30; 32]);
        assert_eq!(p.claims, want);
        assert_eq!(p.claims.len(), MAX_CLAIMS_PER_PAYOUT);
    }

    /// RT-3 / T3-5 with H-06: once a commitment on Base binds a payout's claims to the lineage note,
    /// a rebuilt payout is exactly that payout (same claims in the same order, the same fee bearer,
    /// payments and change), whatever became payable since.
    #[test]
    fn a_rebuilt_payout_carries_exactly_its_committed_claims() {
        let claims: Vec<Claim> = (1..=6u8)
            .map(|i| claim(i, ClaimKind::Liquidation, 150_000 + u64::from(i) * 10_000, (u64::from(i), 0), None))
            .collect();
        // Claim 1's recipient is not known yet: claims 2 to 6 are planned (and committed).
        let NextPayout::Pay(first) = next_payout(&claims, known(&[2, 3, 4, 5, 6]), DEPOSIT, 1, 2_000, &[]).unwrap()
        else {
            panic!()
        };
        assert_eq!(first.claims, ids(&claims[1..].iter().collect::<Vec<_>>()));
        // Claim 1's recipient arrives and a new claim 7 appears; the rebuild ignores both.
        let mut more = claims.clone();
        more.push(claim(7, ClaimKind::Liquidation, 300_000, (7, 0), None));
        let rebuilt = next_payout(&more, known(&[1, 2, 3, 4, 5, 6, 7]), DEPOSIT, 1, 9_000, &first.claims).unwrap();
        assert_eq!(rebuilt, NextPayout::Pay(first.clone()));
        // The binding lists them in any order: the payout is the same.
        let mut reversed = first.claims.clone();
        reversed.reverse();
        let rebuilt = next_payout(&more, known(&[1, 2, 3, 4, 5, 6, 7]), DEPOSIT, 1, 9_000, &reversed).unwrap();
        assert_eq!(rebuilt, NextPayout::Pay(first));
    }

    /// A small deterministic generator (xorshift64*), so the property test needs no dependency and
    /// every run checks the same cases.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// The planner before H-06, kept as the reference: the first payable claim (or close pair),
    /// alone. Same queue rules (E1 including salts), no riding along.
    fn single_payout(
        claims: &[Claim],
        recipient: impl Fn(&Commitment) -> Option<Recipient>,
        lineage_value: u64,
        now: u64,
    ) -> Result<NextPayout, LedgerError> {
        let mut unpaid: Vec<&Claim> = claims.iter().filter(|c| c.state == ClaimState::Unpaid).collect();
        unpaid.sort_by_key(|c| (c.order, c.kind == ClaimKind::CloseMarketMaker));
        let by_id = |id: &ClaimId| unpaid.iter().copied().find(|c| &c.id == id);
        let recipient = |c: &Claim| payee(c, &recipient);
        for &claim in &unpaid {
            match unit(claim, &by_id, &recipient, lineage_value, 1, now)? {
                Unit::Pay(group) => {
                    if let Some(p) = plan(&group, lineage_value, 1)? {
                        return Ok(NextPayout::Pay(p));
                    }
                }
                Unit::WaitUntil(t) => return Ok(NextPayout::WaitUntil(t)),
                Unit::Skip => {}
            }
        }
        Ok(NextPayout::Nothing)
    }

    /// Thousands of generated queues (liquidations, releases and close pairs; unknown and shared
    /// recipients; dust; shared salts; inside and after the grace period). Every plan is a
    /// payout the Registry accepts and the nodes reproduce: at most eight distinct claims in queue
    /// order, distinct receivers and salts, the fee of ZIP 317 borne by the first claim, every other
    /// claim paid in full, value conserved; it starts with exactly the payout the single-claim planner
    /// would make (riding along changes who is paid with it, never who is paid first); a rebuild from
    /// its binding is the same payout; and no plain claim that could have ridden along was left out.
    #[test]
    fn generated_queues_give_payouts_the_registry_accepts() {
        let mut rng = Rng(0x5eed_1234_abcd_ef01);
        let (mut aggregated, mut max_seen) = (0usize, 0usize);
        for case in 0..3_000u64 {
            let n = 1 + rng.below(14) as usize;
            let mut claims: Vec<Claim> = vec![];
            let mut rcpts: Vec<Recipient> = vec![];
            let mut id = 0u8;
            while claims.len() < n {
                id += 1;
                let order = (u64::from(id), 0);
                // A few receivers and salts, so that some repeat.
                let mut r = || Recipient {
                    receiver: [1 + rng.below(12) as u8; 43],
                    salt: [1 + rng.below(20) as u8; 32],
                };
                let (r1, r2) = (r(), r());
                let amount = |rng: &mut Rng| match rng.below(5) {
                    0 => 5_000 + rng.below(10_000),
                    _ => 20_000 + rng.below(900_000),
                };
                if rng.below(6) == 0 {
                    let (uid, mid) = (id, id + 1);
                    id += 1;
                    let mut user = claim(uid, ClaimKind::CloseUser, amount(&mut rng), order, Some(mid));
                    let mut mm = claim(mid, ClaimKind::CloseMarketMaker, amount(&mut rng), order, Some(uid));
                    user.commitment = r1.commitment();
                    mm.commitment = r2.commitment();
                    claims.extend([user, mm]);
                    for (r, c) in [(r1, 0u64), (r2, 1)] {
                        if rng.below(5) != c {
                            rcpts.push(r);
                        }
                    }
                } else {
                    let kind = if rng.below(4) == 0 {
                        ClaimKind::Release
                    } else {
                        ClaimKind::Liquidation
                    };
                    let mut c = claim(id, kind, amount(&mut rng), order, None);
                    c.commitment = r1.commitment();
                    if rng.below(6) == 0 {
                        c.state = ClaimState::Paid;
                    }
                    claims.push(c);
                    if rng.below(6) != 0 {
                        rcpts.push(r1);
                    }
                }
            }
            let unpaid: u64 = claims.iter().filter(|c| c.state == ClaimState::Unpaid).map(|c| c.amount).sum();
            let lineage = unpaid + rng.below(3) * 1_000_000;
            let now = if rng.below(2) == 0 { 1_050 } else { 5_000 };
            let recipient = revealed(&rcpts);
            let got = next_payout(&claims, recipient.clone(), lineage, 1, now, &[]).unwrap();
            let single = single_payout(&claims, recipient.clone(), lineage, now).unwrap();
            let NextPayout::Pay(p) = got else {
                assert_eq!(got, single, "case {case}: no payout, so neither before");
                continue;
            };
            let NextPayout::Pay(s) = single else {
                panic!("case {case}: a payout where the single-claim planner had none")
            };
            assert_eq!(p.claims[..s.claims.len()], s.claims[..], "case {case}: the same payout first");
            let planned: Vec<&Claim> = p.claims.iter().map(|id| claims.iter().find(|c| c.id == *id).unwrap()).collect();
            let rs: Vec<Recipient> = planned.iter().map(|c| recipient(&c.commitment).unwrap()).collect();
            assert!((1..=MAX_CLAIMS_PER_PAYOUT).contains(&planned.len()), "case {case}");
            for (i, a) in rs.iter().enumerate() {
                for b in &rs[i + 1..] {
                    assert!(distinct(a, b), "case {case}: E1");
                }
            }
            let key = |c: &Claim| (c.order, c.kind == ClaimKind::CloseMarketMaker);
            assert!(planned.windows(2).all(|w| key(w[0]) < key(w[1])), "case {case}: queue order, no repeats");
            let claimed: u64 = planned.iter().map(|c| c.amount).sum();
            assert_eq!(p.change, lineage - claimed, "case {case}");
            assert_eq!(p.fee, zip317_fee(1, planned.len() + usize::from(p.change > 0)), "case {case}");
            assert_eq!(p.payments[0].value, planned[0].amount - p.fee, "case {case}: the first bears the fee");
            assert!(p.payments[0].value > 0, "case {case}");
            for (x, c) in p.payments.iter().zip(&planned).skip(1) {
                assert_eq!(x.value, c.amount, "case {case}: the others in full");
            }
            assert_eq!(p.payments.iter().map(|x| x.value).sum::<u64>() + p.fee + p.change, lineage, "case {case}");
            // RT-3: the commitment's claims, planned again, are the same payout.
            assert_eq!(
                next_payout(&claims, recipient.clone(), lineage, 1, now, &p.claims).unwrap(),
                NextPayout::Pay(p.clone()),
                "case {case}: rebuild"
            );
            // Maximality for plain claims: a later liquidation or release left out had a reason.
            if planned.len() < MAX_CLAIMS_PER_PAYOUT {
                for c in claims.iter().filter(|c| {
                    c.state == ClaimState::Unpaid
                        && c.partner.is_none()
                        && key(c) > key(planned[0])
                        && !p.claims.contains(&c.id)
                }) {
                    let Some(r) = recipient(&c.commitment) else { continue };
                    let own_fee_ok = c.amount > zip317_fee(1, 2);
                    let compatible = rs.iter().all(|x| distinct(x, &r));
                    let change = p.change - c.amount;
                    let head_bears = planned[0].amount > zip317_fee(1, planned.len() + 1 + usize::from(change > 0));
                    assert!(
                        !(own_fee_ok && compatible && head_bears),
                        "case {case}: claim {:?} could ride along",
                        c.id[0]
                    );
                }
            }
            if planned.len() > s.claims.len() {
                aggregated += 1;
            }
            max_seen = max_seen.max(planned.len());
        }
        // The generator exercises what it should.
        assert!(aggregated > 1_000, "{aggregated} aggregated payouts");
        assert_eq!(max_seen, MAX_CLAIMS_PER_PAYOUT);
    }
}
