//! The two FROST rounds of a vault payout, with retries (M-1).
//!
//! Round 1 goes to every signer not excluded so far; each verifies the PCZT against its own
//! plan and commits to one nonce pair per vault spend. Three valid committers are chosen for
//! round 2. Tier 3 (docs/TIER3.md § 5): for a payout, each also signs the payout's commitment,
//! which goes to Base between the two rounds; the chosen signers then get every signature the
//! Registry recorded it with, and release their shares only once they see it there. A chosen signer that fails round 2, answers with the wrong number of shares or
//! unparseable ones, or sends a share that FROST's cheater detection rejects is excluded, and
//! both rounds run again with the same PCZT: a node's nonces are single-use, so every attempt
//! is a new session. No answer is indexed before its length is checked.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{B256, Bytes};
use anyhow::{Context, Result, anyhow, ensure};
use reddsa::frost::redpallas::{
    CheaterDetection, Identifier,
    keys::PublicKeyPackage,
    rerandomized::{self, RandomizedParams},
    round1::SigningCommitments,
    round2::SignatureShare,
};
use serde_json::Value;
use tracing::{debug, warn};
use zecured_node::signing::{CommitRequest, CommitResponse, ShareRequest, ShareResponse, SignKind, SpendCommitRecord};
use zecured_threshold::{self as threshold, MIN_SIGNERS, signer_id};
use zecured_zcash_ops::spend::SpendAuthRequest;

use crate::{
    error::{PublicError, scrubbed},
    nodes::{Nodes, shortfall, split},
};

/// Signing sessions tried for one payout before giving up until the next pass.
pub const MAX_ATTEMPTS: usize = 3;

/// What the signers are asked to authorize.
pub struct Payload<'a> {
    pub vault_id: B256,
    pub kind: &'a SignKind,
    /// The proved, unsigned PCZT, as sent to the nodes.
    pub pczt: &'a [u8],
    pub sighash: [u8; 32],
    /// One request (action index and randomizer `alpha`) per vault spend, in action order.
    pub requests: &'a [SpendAuthRequest],
    pub public_key_package: &'a PublicKeyPackage,
}

/// Spend authorization signatures and the signers that produced them.
pub struct Signed {
    pub signatures: Vec<(usize, [u8; 64])>,
    pub signers: Vec<u16>,
    /// Tier 3: the payout's commitment and the signatures it was recorded with, kept so that it
    /// can be submitted again if a Base reorg drops it (RT-5). `None` for refunds and demos.
    pub spend_commit: Option<(SpendCommitRecord, Vec<Bytes>)>,
}

struct Committer {
    index: u16,
    raw: Vec<Value>,
    parsed: Vec<SigningCommitments>,
    /// Tier 3, payouts: the node's commitment of the payout and its signature.
    spend_commit: Option<(SpendCommitRecord, Bytes)>,
}

/// Tier 3: puts a payout's commitment on Base between the two rounds.
pub trait SpendCommitter: Sync {
    /// Records `record` on Base with round-1 signatures `signed` (node index, signature) and
    /// waits until the nodes can see it deep enough; returns the signatures the Registry
    /// recorded, in the order it took them.
    fn commit(
        &self,
        record: &SpendCommitRecord,
        signed: &[(u16, Bytes)],
    ) -> impl std::future::Future<Output = Result<Vec<Bytes>>> + Send;
}

fn parse_commitments(index: u16, c: &CommitResponse, spends: usize) -> Result<Committer> {
    ensure!(c.index == index, "answered as node {}", c.index);
    ensure!(c.commitments.len() == spends, "{} commitments for {spends} spends", c.commitments.len());
    let parsed = c
        .commitments
        .iter()
        .map(|v| serde_json::from_value(v.clone()))
        .collect::<Result<_, _>>()
        .context("malformed commitment")?;
    Ok(Committer {
        index,
        raw: c.commitments.clone(),
        spend_commit: c.spend_commit.as_ref().map(|s| (s.record.clone(), s.signature.clone())),
        parsed,
    })
}

fn parse_shares(index: u16, s: &ShareResponse, spends: usize) -> Result<Vec<SignatureShare>> {
    ensure!(s.index == index, "answered as node {}", s.index);
    ensure!(s.shares.len() == spends, "{} shares for {spends} spends", s.shares.len());
    s.shares.iter().map(|v| serde_json::from_value(v.clone()).context("malformed share")).collect()
}

enum AggregateError {
    /// FROST identified these signers' shares as invalid.
    Culprits(BTreeSet<u16>),
    Failed(anyhow::Error),
}

/// Aggregates every spend's shares; on an invalid share, reports who sent it.
fn aggregate(
    chosen: &[Committer],
    shares: &BTreeMap<u16, Vec<SignatureShare>>,
    p: &Payload<'_>,
) -> Result<Vec<(usize, [u8; 64])>, AggregateError> {
    let ids = chosen
        .iter()
        .map(|c| Ok((c.index, signer_id(c.index)?)))
        .collect::<Result<Vec<(u16, Identifier)>>>()
        .map_err(AggregateError::Failed)?;
    let mut out = Vec::with_capacity(p.requests.len());
    for (k, req) in p.requests.iter().enumerate() {
        let commitments = chosen.iter().zip(&ids).map(|(c, (_, id))| (*id, c.parsed[k])).collect();
        let package = threshold::signing_package(commitments, &p.sighash).map_err(AggregateError::Failed)?;
        let mut share_map = BTreeMap::new();
        for (index, id) in &ids {
            let share = shares.get(index).and_then(|s| s.get(k)).copied();
            let share = share.ok_or_else(|| AggregateError::Failed(anyhow!("missing share of node {index}")))?;
            share_map.insert(*id, share);
        }
        let randomizer = threshold::randomizer_from_alpha(&req.alpha).map_err(AggregateError::Failed)?;
        let params = RandomizedParams::from_randomizer(p.public_key_package.verifying_key(), randomizer);
        let aggregated = rerandomized::aggregate_custom(
            &package,
            &share_map,
            p.public_key_package,
            CheaterDetection::AllCheaters,
            &params,
        );
        match aggregated {
            Ok(signature) => {
                let bytes = signature.serialize().map_err(|e| AggregateError::Failed(anyhow!("signature: {e}")))?;
                let bytes: [u8; 64] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| AggregateError::Failed(anyhow!("signature is not 64 bytes")))?;
                out.push((req.action_index, bytes));
            }
            Err(e) => {
                let culprits: BTreeSet<u16> = e
                    .culprits()
                    .iter()
                    .filter_map(|c| ids.iter().find(|(_, id)| id == c).map(|(index, _)| *index))
                    .collect();
                return Err(if culprits.is_empty() {
                    AggregateError::Failed(anyhow!("frost aggregate: {e}"))
                } else {
                    AggregateError::Culprits(culprits)
                });
            }
        }
    }
    Ok(out)
}

/// The commitment record that at least `need` committers signed, if any (with five signers and a
/// threshold of three, at most one can be).
fn agreed_record(committers: &[Committer], need: usize) -> Option<SpendCommitRecord> {
    let records: Vec<&SpendCommitRecord> =
        committers.iter().filter_map(|c| c.spend_commit.as_ref().map(|(r, _)| r)).collect();
    records.iter().find(|r| records.iter().filter(|o| *o == *r).count() >= need).map(|r| (*r).clone())
}

fn session_id() -> String {
    let mut sid = [0u8; 16];
    rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut sid);
    hex::encode(sid)
}

/// Runs both FROST rounds for `p`, retrying without misbehaving signers; a payout's commitment
/// goes to Base through `committer` between the rounds (once, whatever the retries).
pub async fn sign<C: SpendCommitter>(nodes: &Nodes, p: &Payload<'_>, committer: &C) -> Result<Signed> {
    let need = usize::from(MIN_SIGNERS);
    let spends = p.requests.len();
    let vault = hex::encode(&p.vault_id[..4]);
    let payout = matches!(p.kind, SignKind::Payout { .. });
    // The commitment on Base and the signatures it was recorded with (Tier 3).
    let mut committed: Option<(SpendCommitRecord, Vec<Bytes>)> = None;
    let mut excluded: BTreeSet<u16> = BTreeSet::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let candidates: Vec<u16> = nodes.indices().into_iter().filter(|i| !excluded.contains(i)).collect();
        if candidates.len() < need {
            return Err(anyhow!("too few signers left to retry").context(PublicError::SignersUnavailable));
        }

        // Round 1: every remaining signer verifies the PCZT and commits.
        let session = session_id();
        let commit = CommitRequest {
            session_id: session.clone(),
            vault_id: p.vault_id,
            kind: p.kind.clone(),
            pczt: p.pczt.to_vec(),
        };
        let (answers, failures) =
            split(nodes.post_many::<_, CommitResponse>(&candidates, "/v1/sign/commit", &commit).await);
        let mut committers = vec![];
        for (index, c) in &answers {
            match parse_commitments(*index, c, spends) {
                Ok(committer) => committers.push(committer),
                Err(e) => {
                    debug!(vault = %vault, index, error = %scrubbed(&e), "malformed round-1 answer");
                    excluded.insert(*index);
                }
            }
        }
        if payout {
            // Every honest node computes the same commitment from its own view of the same payout:
            // the record at least `need` committers agree on (the one already on Base after a
            // retry). A node with another record, or none, disagrees about the payout.
            let record = committed.as_ref().map(|(r, _)| r.clone()).or_else(|| agreed_record(&committers, need));
            committers.retain(|c| {
                let agrees = record.is_some() && c.spend_commit.as_ref().map(|(r, _)| r) == record.as_ref();
                if !agrees {
                    debug!(vault = %vault, index = c.index, "round-1 commitment differs or is missing");
                    excluded.insert(c.index);
                }
                agrees
            });
        }
        if committers.len() < need {
            return Err(shortfall("signing round 1", committers.len(), need, &failures));
        }
        committers.truncate(need);

        // Tier 3: the payout's commitment goes to Base (once) before any share is asked for.
        let spend_commit_sigs = match (&committed, payout) {
            (Some((_, sigs)), _) => sigs.clone(),
            (None, true) => {
                let (record, _) = committers[0].spend_commit.clone().context("payout without a commitment")?;
                let signed: Vec<(u16, Bytes)> = committers
                    .iter()
                    .filter_map(|c| c.spend_commit.as_ref().map(|(_, sig)| (c.index, sig.clone())))
                    .collect();
                let sigs = committer.commit(&record, &signed).await.context(PublicError::Internal)?;
                committed = Some((record, sigs.clone()));
                sigs
            }
            (None, false) => vec![],
        };

        // Round 2 on the chosen signers only.
        let chosen: Vec<u16> = committers.iter().map(|c| c.index).collect();
        let share = ShareRequest {
            session_id: session,
            commitments: (0..spends)
                .map(|k| committers.iter().map(|c| (c.index, c.raw[k].clone())).collect())
                .collect(),
            spend_commit_sigs,
        };
        let mut culprits = BTreeSet::new();
        let mut shares = BTreeMap::new();
        for (index, answer) in nodes.post_many::<_, ShareResponse>(&chosen, "/v1/sign/share", &share).await {
            match answer.and_then(|s| parse_shares(index, &s, spends)) {
                Ok(s) => {
                    shares.insert(index, s);
                }
                Err(e) => {
                    debug!(vault = %vault, index, error = %scrubbed(&e), "round 2 failed");
                    culprits.insert(index);
                }
            }
        }
        if culprits.is_empty() {
            match aggregate(&committers, &shares, p) {
                Ok(signatures) => {
                    return Ok(Signed {
                        signatures,
                        signers: chosen,
                        spend_commit: committed,
                    });
                }
                Err(AggregateError::Culprits(c)) => culprits = c,
                Err(AggregateError::Failed(e)) => return Err(e.context(PublicError::Internal)),
            }
        }
        warn!(vault = %vault, attempt, signers = ?chosen, excluded = ?culprits, "signing round 2 failed; retrying without them");
        excluded.extend(culprits);
    }
    Err(anyhow!("no valid signature after {MAX_ATTEMPTS} sessions").context(PublicError::SignersUnavailable))
}

/// DEMO ONLY (`demo-tier3`, the separate theft demo, review T3-6): both FROST rounds without a
/// Tier 3 commitment, as three compromised signers would run them to steal. Honest signers refuse
/// round 1 (the PCZT matches no plan of theirs) and are left out; nothing goes to Base, which is
/// what makes the spend provable as F1.
#[cfg(feature = "demo-tier3")]
pub async fn demo_sign_uncommitted(nodes: &Nodes, p: &Payload<'_>) -> Result<Signed> {
    let need = usize::from(MIN_SIGNERS);
    let spends = p.requests.len();
    let session = session_id();
    let commit = CommitRequest {
        session_id: session.clone(),
        vault_id: p.vault_id,
        kind: p.kind.clone(),
        pczt: p.pczt.to_vec(),
    };
    let (answers, failures) =
        split(nodes.post_many::<_, CommitResponse>(&nodes.indices(), "/v1/sign/commit", &commit).await);
    let mut committers: Vec<Committer> =
        answers.iter().filter_map(|(index, c)| parse_commitments(*index, c, spends).ok()).collect();
    if committers.len() < need {
        return Err(shortfall("theft demo, round 1", committers.len(), need, &failures));
    }
    committers.truncate(need);
    let chosen: Vec<u16> = committers.iter().map(|c| c.index).collect();
    let share = ShareRequest {
        session_id: session,
        commitments: (0..spends).map(|k| committers.iter().map(|c| (c.index, c.raw[k].clone())).collect()).collect(),
        spend_commit_sigs: vec![],
    };
    let mut shares = BTreeMap::new();
    for (index, answer) in nodes.post_many::<_, ShareResponse>(&chosen, "/v1/sign/share", &share).await {
        shares.insert(index, answer.and_then(|s| parse_shares(index, &s, spends))?);
    }
    let signatures = aggregate(&committers, &shares, p).map_err(|e| match e {
        AggregateError::Culprits(c) => anyhow!("theft demo: invalid shares from nodes {c:?}"),
        AggregateError::Failed(e) => e,
    })?;
    Ok(Signed {
        signatures,
        signers: chosen,
        spend_commit: None,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use alloy::signers::local::PrivateKeySigner;
    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use rand_core_06::OsRng;
    use reddsa::frost::redpallas::{Field, PallasScalarField, keys::KeyPackage, round1::SigningNonces};

    use super::*;
    use crate::{
        error::public_error,
        nodes::tests::{config, serve},
    };
    use zecured_node::signing::SignedSpendCommit;

    /// How a mock signer node behaves.
    #[derive(Clone, Copy, PartialEq)]
    enum Behaviour {
        Honest,
        /// Refuses round 1 under its "policy".
        RefuseCommit,
        /// Fails round 2.
        FailShare,
        /// Answers round 2 with one share too few (used to panic the coordinator).
        ShortShares,
        /// Signs the wrong randomizer: its share is invalid.
        BadShare,
        /// Tier 3: signs another commitment than the honest nodes (disagrees about the payout).
        OtherCommitment,
        /// Tier 3: releases shares without the commitment's signatures (must never be asked).
        IgnoresCommitment,
    }

    struct MockNode {
        index: u16,
        behaviour: Behaviour,
        key_package: KeyPackage,
        sighash: [u8; 32],
        alphas: Vec<[u8; 32]>,
        sessions: Mutex<HashMap<String, Vec<SigningNonces>>>,
        shares_given: Mutex<usize>,
    }

    async fn commit(
        State(n): State<Arc<MockNode>>,
        Json(r): Json<CommitRequest>,
    ) -> Result<Json<CommitResponse>, (StatusCode, Json<Value>)> {
        if n.behaviour == Behaviour::RefuseCommit {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": "PCZT refused" }))));
        }
        let (nonces, commitments): (Vec<_>, Vec<_>) =
            n.alphas.iter().map(|_| threshold::commit(&n.key_package, &mut OsRng)).unzip();
        n.sessions.lock().unwrap().insert(r.session_id, nonces);
        let mut record = honest_record();
        if n.behaviour == Behaviour::OtherCommitment {
            record.change_value += 1;
        }
        Ok(Json(CommitResponse {
            index: n.index,
            commitments: commitments.iter().map(|c| serde_json::to_value(c).unwrap()).collect(),
            spend_commit: Some(SignedSpendCommit {
                record,
                signature: Bytes::from(vec![u8::try_from(n.index).unwrap(); 65]),
            }),
        }))
    }

    /// The commitment every honest mock node computes.
    fn honest_record() -> SpendCommitRecord {
        SpendCommitRecord {
            vault_id: B256::repeat_byte(1),
            input_tag: B256::repeat_byte(0x66),
            claim_ids: vec![B256::repeat_byte(7)],
            claim_refs: vec![B256::repeat_byte(8)],
            claim_outputs: vec![0],
            payout_ref: B256::repeat_byte(0xaa),
            change_tag: B256::repeat_byte(0xbb),
            change_value: 3_800_000,
        }
    }

    /// Commitments and the (node, signature) pairs they were sent with.
    type Commits = Vec<(SpendCommitRecord, Vec<(u16, Bytes)>)>;

    /// A Registry in memory: records each commitment once, with the signatures it was given.
    #[derive(Default)]
    struct MockRegistry {
        commits: Mutex<Commits>,
    }

    impl SpendCommitter for MockRegistry {
        async fn commit(&self, record: &SpendCommitRecord, signed: &[(u16, Bytes)]) -> Result<Vec<Bytes>> {
            let mut commits = self.commits.lock().unwrap();
            assert!(commits.iter().all(|(r, _)| r.payout_ref != record.payout_ref), "committed twice");
            assert!(signed.len() >= 3);
            commits.push((record.clone(), signed.to_vec()));
            Ok(signed.iter().map(|(_, s)| s.clone()).collect())
        }
    }

    async fn share(
        State(n): State<Arc<MockNode>>,
        Json(r): Json<ShareRequest>,
    ) -> Result<Json<ShareResponse>, (StatusCode, Json<Value>)> {
        let nonces = n.sessions.lock().unwrap().remove(&r.session_id).expect("known session");
        // T3-4: no share without every signature of the commitment on Base.
        assert!(
            r.spend_commit_sigs.len() >= 3 || n.behaviour == Behaviour::IgnoresCommitment,
            "share asked before the commitment"
        );
        if n.behaviour == Behaviour::FailShare {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "crashed" }))));
        }
        let mut shares = vec![];
        for (k, set) in r.commitments.iter().enumerate() {
            let parsed = set
                .iter()
                .map(|(i, c)| (signer_id(*i).unwrap(), serde_json::from_value(c.clone()).unwrap()))
                .collect();
            let package = threshold::signing_package(parsed, &n.sighash).unwrap();
            let alpha = if n.behaviour == Behaviour::BadShare {
                random_alpha()
            } else {
                n.alphas[k]
            };
            let s = threshold::sign_share(&package, &nonces[k], &n.key_package, &alpha).unwrap();
            shares.push(serde_json::to_value(s).unwrap());
        }
        if n.behaviour == Behaviour::ShortShares {
            shares.pop();
        }
        *n.shares_given.lock().unwrap() += 1;
        Ok(Json(ShareResponse { index: n.index, shares }))
    }

    fn random_alpha() -> [u8; 32] {
        PallasScalarField::serialize(&PallasScalarField::random(&mut OsRng))
    }

    struct Net {
        nodes: Nodes,
        registry: MockRegistry,
        mocks: Vec<Arc<MockNode>>,
        pkp: PublicKeyPackage,
        sighash: [u8; 32],
        alphas: Vec<[u8; 32]>,
    }

    async fn network(behaviours: [Behaviour; 5]) -> Net {
        let keys = threshold::dkg_in_process(OsRng).unwrap();
        let sighash = [42u8; 32];
        let alphas = vec![random_alpha(), random_alpha()]; // two vault spends
        let mut urls = vec![];
        let mut mocks = vec![];
        for (k, behaviour) in behaviours.into_iter().enumerate() {
            let index = u16::try_from(k + 1).unwrap();
            let mock = Arc::new(MockNode {
                index,
                behaviour,
                key_package: keys.key_packages[&signer_id(index).unwrap()].clone(),
                sighash,
                alphas: alphas.clone(),
                sessions: Mutex::default(),
                shares_given: Mutex::new(0),
            });
            let app = Router::new()
                .route("/v1/sign/commit", post(commit))
                .route("/v1/sign/share", post(share))
                .with_state(mock.clone());
            urls.push(serve(app).await);
            mocks.push(mock);
        }
        Net {
            nodes: Nodes::new(&config(&urls), PrivateKeySigner::random()).unwrap(),
            registry: MockRegistry::default(),
            mocks,
            pkp: keys.public_key_package,
            sighash,
            alphas,
        }
    }

    async fn run(net: &Net) -> Result<Signed> {
        let requests: Vec<SpendAuthRequest> = net
            .alphas
            .iter()
            .enumerate()
            .map(|(k, alpha)| SpendAuthRequest {
                action_index: 2 * k,
                alpha: *alpha,
            })
            .collect();
        let kind = SignKind::Payout { claim_ids: vec![] };
        let payload = Payload {
            vault_id: B256::repeat_byte(1),
            kind: &kind,
            pczt: b"pczt",
            sighash: net.sighash,
            requests: &requests,
            public_key_package: &net.pkp,
        };
        sign(&net.nodes, &payload, &net.registry).await
    }

    /// Every returned signature is a valid RedPallas signature under rk = ak + [alpha] G.
    fn assert_valid(net: &Net, signed: &Signed) {
        assert_eq!(signed.signatures.len(), net.alphas.len());
        let ak = threshold::ak_bytes(&net.pkp).unwrap();
        let ak = reddsa::VerificationKey::<reddsa::orchard::SpendAuth>::try_from(ak).unwrap();
        for ((action, sig), (k, alpha)) in signed.signatures.iter().zip(net.alphas.iter().enumerate()) {
            assert_eq!(*action, 2 * k);
            let rk = ak.randomize(&PallasScalarField::deserialize(alpha).unwrap());
            rk.verify(&net.sighash, &reddsa::Signature::from(*sig)).unwrap();
        }
    }

    #[tokio::test]
    async fn honest_signers_produce_valid_signatures() {
        let net = network([Behaviour::Honest; 5]).await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert_eq!(signed.signers, vec![1, 2, 3]);
        // Tier 3: committed once, by the chosen signers, before round 2.
        let commits = net.registry.commits.lock().unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].0, honest_record());
        assert_eq!(commits[0].1.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    /// Tier 3: a node that signs another commitment disagrees about the payout and is left out.
    #[tokio::test]
    async fn a_node_with_another_commitment_is_left_out() {
        let net = network([
            Behaviour::OtherCommitment,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert_eq!(signed.signers, vec![2, 3, 4]);
        let commits = net.registry.commits.lock().unwrap();
        assert_eq!(commits[0].0, honest_record());
    }

    /// Tier 3: a retry after a failed round 2 reuses the commitment already on Base, and hands its
    /// signatures to the new signers.
    #[tokio::test]
    async fn a_retry_reuses_the_commitment() {
        let net = network([
            Behaviour::Honest,
            Behaviour::FailShare,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert_eq!(net.registry.commits.lock().unwrap().len(), 1, "committed once");
    }

    /// Tier 3: three nodes that agree on another commitment than the rest cannot be outvoted, and
    /// two that disagree leave too few.
    #[tokio::test]
    async fn without_three_agreeing_nodes_nothing_is_committed() {
        let net = network([
            Behaviour::OtherCommitment,
            Behaviour::OtherCommitment,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::RefuseCommit,
        ])
        .await;
        assert!(run(&net).await.is_err());
        assert!(net.registry.commits.lock().unwrap().is_empty());
        assert!(net.mocks.iter().all(|m| *m.shares_given.lock().unwrap() == 0), "round 2 never started");
    }

    /// M-1: a chosen signer failing round 2 no longer stalls the payout; another subset signs.
    #[tokio::test]
    async fn a_failed_round_two_is_retried_with_other_signers() {
        let net = network([
            Behaviour::Honest,
            Behaviour::FailShare,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert!(!signed.signers.contains(&2));
    }

    /// M-1: a short share list used to index out of bounds and panic the coordinator loop.
    #[tokio::test]
    async fn short_answers_are_rejected_not_indexed() {
        let net = network([
            Behaviour::ShortShares,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert!(!signed.signers.contains(&1));
    }

    /// M-1: FROST's cheater detection names the signer of an invalid share; it is excluded.
    #[tokio::test]
    async fn an_invalid_share_is_attributed_and_excluded() {
        let net = network([
            Behaviour::Honest,
            Behaviour::Honest,
            Behaviour::BadShare,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let signed = run(&net).await.unwrap();
        assert_valid(&net, &signed);
        assert_eq!(signed.signers, vec![1, 2, 4]);
        assert_eq!(*net.mocks[2].shares_given.lock().unwrap(), 1, "node 3 was asked once, then excluded");
    }

    #[tokio::test]
    async fn policy_refusals_below_the_threshold_are_reported() {
        let net = network([
            Behaviour::RefuseCommit,
            Behaviour::RefuseCommit,
            Behaviour::RefuseCommit,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let e = run(&net).await.err().expect("two committers are not enough");
        assert_eq!(public_error(&e), Some(PublicError::SignersRefused));
        assert!(net.mocks.iter().all(|m| *m.shares_given.lock().unwrap() == 0), "round 2 never started");
    }

    #[tokio::test]
    async fn retries_stop_when_too_few_signers_remain() {
        let net = network([
            Behaviour::FailShare,
            Behaviour::FailShare,
            Behaviour::FailShare,
            Behaviour::Honest,
            Behaviour::Honest,
        ])
        .await;
        let e = run(&net).await.err().expect("only two honest signers");
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
    }
}
