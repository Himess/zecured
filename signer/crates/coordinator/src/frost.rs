//! The two FROST rounds of a vault payout, with retries (M-1).
//!
//! Round 1 goes to every signer not excluded so far; each verifies the PCZT against its own
//! plan and commits to one nonce pair per vault spend. Three valid committers are chosen for
//! round 2. A chosen signer that fails round 2, answers with the wrong number of shares or
//! unparseable ones, or sends a share that FROST's cheater detection rejects is excluded, and
//! both rounds run again with the same PCZT: a node's nonces are single-use, so every attempt
//! is a new session. No answer is indexed before its length is checked.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::B256;
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
use zecured_node::signing::{CommitRequest, CommitResponse, ShareRequest, ShareResponse, SignKind};
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
}

struct Committer {
    index: u16,
    raw: Vec<Value>,
    parsed: Vec<SigningCommitments>,
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

fn session_id() -> String {
    let mut sid = [0u8; 16];
    rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut sid);
    hex::encode(sid)
}

/// Runs both FROST rounds for `p`, retrying without misbehaving signers.
pub async fn sign(nodes: &Nodes, p: &Payload<'_>) -> Result<Signed> {
    let need = usize::from(MIN_SIGNERS);
    let spends = p.requests.len();
    let vault = hex::encode(&p.vault_id[..4]);
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
        if committers.len() < need {
            return Err(shortfall("signing round 1", committers.len(), need, &failures));
        }
        committers.truncate(need);

        // Round 2 on the chosen signers only.
        let chosen: Vec<u16> = committers.iter().map(|c| c.index).collect();
        let share = ShareRequest {
            session_id: session,
            commitments: (0..spends)
                .map(|k| committers.iter().map(|c| (c.index, c.raw[k].clone())).collect())
                .collect(),
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
        Ok(Json(CommitResponse {
            index: n.index,
            commitments: commitments.iter().map(|c| serde_json::to_value(c).unwrap()).collect(),
        }))
    }

    async fn share(
        State(n): State<Arc<MockNode>>,
        Json(r): Json<ShareRequest>,
    ) -> Result<Json<ShareResponse>, (StatusCode, Json<Value>)> {
        let nonces = n.sessions.lock().unwrap().remove(&r.session_id).expect("known session");
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
        sign(&net.nodes, &payload).await
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
