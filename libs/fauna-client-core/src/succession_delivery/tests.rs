//! The delivery's arms over scripted fake nests: what is sent, in what order,
//! and when an entry is settled. What a real nest accepts is
//! `bins/fauna-nest/tests/conformance_succession.rs`'s, over the real
//! handlers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{IdentitySuccession, RecoveryKey, RecoveryKeyRegistration};
use fauna_protocol::recovery as wire;
use fauna_protocol::{ByteBuf, RpcError, RpcErrorClass, RpcRequester};

use super::*;
use crate::recovery_chain::{REGISTRATION_CHAIN_KIND, REGISTRATION_SUBMIT_KIND};

const NEST: [u8; 32] = [0xbb; 32];
const URL: &str = "https://owed.test";

#[derive(Debug)]
enum FakeErr {
    Refused(RpcError),
    Dropped,
}

impl core::fmt::Display for FakeErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(e) => f.write_str(&e.code),
            Self::Dropped => f.write_str("dropped"),
        }
    }
}

impl RpcErrorClass for FakeErr {
    fn is_rejection(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Refused(e) => Some(e),
            Self::Dropped => None,
        }
    }
}

fn refused(code: &str) -> FakeErr {
    FakeErr::Refused(RpcError::new(code, "test"))
}

/// One fake nest's state.
#[derive(Default)]
struct State {
    /// The owed list it keeps, and what was settled out of it.
    owed: Vec<wire::OwedNest>,
    settled: Vec<(Vec<u8>, Vec<u8>)>,
    settle_refused: bool,
    /// `succession.lookup`'s answer per retired id.
    paths: BTreeMap<[u8; 32], Vec<Vec<u8>>>,
    /// Each account's registration chain; a submit appends for the caller.
    chains: BTreeMap<[u8; 32], Vec<Vec<u8>>>,
    /// `succession.submit`'s answers, in order (`Ok` when exhausted).
    submits: VecDeque<Result<(), &'static str>>,
    /// What was asked, in order.
    calls: Vec<&'static str>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<State>>);

impl Fake {
    fn with(f: impl FnOnce(&mut State)) -> Self {
        let fake = Self::default();
        f(&mut fake.0.lock().unwrap());
        fake
    }
    fn calls(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().calls.clone()
    }
    fn settled(&self) -> usize {
        self.0.lock().unwrap().settled.len()
    }
    fn chain(&self, actor: &[u8; 32]) -> Vec<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .chains
            .get(actor)
            .cloned()
            .unwrap_or_default()
    }
}

/// A connection to a [`Fake`] — anonymous, or as `actor`.
struct Conn {
    nest: Fake,
    actor: Option<[u8; 32]>,
}

fn roundtrip<A: serde::Serialize, B: serde::de::DeserializeOwned>(value: A) -> B {
    fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&value).unwrap()).unwrap()
}

impl RpcRequester for Conn {
    type Error = FakeErr;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, FakeErr>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let mut s = self.nest.0.lock().unwrap();
        s.calls.push(kind);
        match kind {
            SUCCESSION_STATUS_KIND => Ok(roundtrip(wire::SuccessionStatusReply {
                owed_nests: s.owed.clone(),
                ..Default::default()
            })),
            SUCCESSION_LOOKUP_KIND => {
                let req: wire::SuccessionLookupRequest = roundtrip(payload);
                let id: [u8; 32] = req.actor_id.as_ref().try_into().unwrap();
                let statements = s.paths.get(&id).cloned().unwrap_or_default();
                Ok(roundtrip(wire::SuccessionLookupReply {
                    statements: statements.into_iter().map(ByteBuf::from).collect(),
                    ..Default::default()
                }))
            }
            SUCCESSION_SUBMIT_KIND => match s.submits.pop_front().unwrap_or(Ok(())) {
                Ok(()) => Ok(roundtrip(wire::SuccessionSubmitReply::default())),
                Err(code) => Err(refused(code)),
            },
            SUCCESSION_OWED_SETTLE_KIND => {
                if s.settle_refused {
                    return Err(FakeErr::Dropped);
                }
                let req: wire::SuccessionOwedSettleRequest = roundtrip(payload);
                s.settled
                    .push((req.old_actor_id.into_vec(), req.nest_id.into_vec()));
                Ok(roundtrip(wire::SuccessionOwedSettleReply::default()))
            }
            REGISTRATION_CHAIN_KIND => {
                let req: wire::RegistrationChainRequest = roundtrip(payload);
                let id: [u8; 32] = req.actor_id.as_ref().try_into().unwrap();
                let registrations = s.chains.get(&id).cloned().unwrap_or_default();
                Ok(roundtrip(wire::RegistrationChainReply {
                    registrations: registrations.into_iter().map(ByteBuf::from).collect(),
                    ..Default::default()
                }))
            }
            REGISTRATION_SUBMIT_KIND => {
                let actor = self.actor.expect("a registration is submitted signed in");
                let req: wire::RegistrationSubmitRequest = roundtrip(payload);
                s.chains
                    .entry(actor)
                    .or_default()
                    .push(req.registration.into_vec());
                Ok(roundtrip(wire::RegistrationSubmitReply::default()))
            }
            other => panic!("unexpected kind {other}"),
        }
    }
}

/// The host's reach to the one owed nest.
struct Reach {
    owed: Fake,
    /// The identity the address presents.
    presented: [u8; 32],
    reachable: bool,
    /// Retired identities this device holds the seed of.
    seeds: BTreeSet<[u8; 32]>,
    /// Identities the owed nest holds an account for.
    accounts: BTreeSet<[u8; 32]>,
}

impl Reach {
    fn to(owed: &Fake) -> Self {
        Self {
            owed: owed.clone(),
            presented: NEST,
            reachable: true,
            seeds: BTreeSet::new(),
            accounts: BTreeSet::new(),
        }
    }
}

impl OwedNestReach for Reach {
    type Anon = Conn;
    type Signed = Conn;

    async fn anonymous(&self, url: &str) -> Result<(Conn, [u8; 32]), String> {
        assert_eq!(url, URL);
        if !self.reachable {
            return Err("unreachable".into());
        }
        let conn = Conn {
            nest: self.owed.clone(),
            actor: None,
        };
        Ok((conn, self.presented))
    }

    async fn sign_in_as(&self, url: &str, actor_id: &[u8; 32]) -> SignIn<Conn> {
        assert_eq!(url, URL);
        if !self.seeds.contains(actor_id) {
            return SignIn::NoSeed;
        }
        if !self.accounts.contains(actor_id) {
            return SignIn::from_error(&FakeErr::Refused(RpcError::not_registered()));
        }
        SignIn::Connected(Conn {
            nest: self.owed.clone(),
            actor: Some(*actor_id),
        })
    }
}

// ── The identities ───────────────────────────────────────────────────────────

fn keypair(tag: u8) -> ActorKeypair {
    ActorKeypair::from_secret([tag; 32])
}

fn id(tag: u8) -> [u8; 32] {
    keypair(tag).actor_id().0
}

fn kit(tag: u8) -> RecoveryKey {
    RecoveryKey::from_bytes([tag; 32])
}

/// `old`'s first registration under `kit(key)`.
fn registration(old: u8, key: u8) -> Vec<u8> {
    let signed = RecoveryKeyRegistration {
        actor_id: ActorId(id(old)),
        recovery_pubkey: kit(key).public(),
        seq: 1,
        created_at: Timestamp(1_700_000_001),
    }
    .sign(keypair(old).signing_key(), &kit(key), None)
    .unwrap();
    canonical_encode(&signed).unwrap()
}

/// The statement that succeeded `old` by `new`.
fn statement(old: u8, new: u8) -> Vec<u8> {
    let signed = IdentitySuccession {
        old_actor_id: ActorId(id(old)),
        new_actor_id: ActorId(id(new)),
        recovery_pubkey: kit(old).public(),
        seq: 2,
        created_at: Timestamp(1_700_000_002),
    }
    .sign(&kit(old), keypair(new).signing_key(), None)
    .unwrap();
    canonical_encode(&signed).unwrap()
}

fn entry(old: u8, url: Option<&str>) -> wire::OwedNest {
    wire::OwedNest {
        old_actor_id: ByteBuf::from(id(old).to_vec()),
        nest_id: ByteBuf::from(NEST.to_vec()),
        nest_url: url.map(str::to_string),
        ..Default::default()
    }
}

/// The keeping nest: it applied `old → new` and keeps the owed entry, and it
/// holds `old`'s chain.
fn keeper(old: u8, new: u8) -> Fake {
    Fake::with(|s| {
        s.owed = vec![entry(old, Some(URL))];
        s.paths.insert(id(old), vec![statement(old, new)]);
        s.chains.insert(id(old), vec![registration(old, old)]);
    })
}

fn owed_with(submits: &[Result<(), &'static str>]) -> Fake {
    Fake::with(|s| s.submits = submits.iter().copied().collect())
}

async fn deliver(keeper: &Fake, reach: &Reach) -> Delivery {
    let owed = keeper.0.lock().unwrap().owed[0].clone();
    deliver_owed_nest(
        &Conn {
            nest: keeper.clone(),
            actor: Some(id(2)),
        },
        reach,
        &owed,
    )
    .await
}

// ── The arms ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_landed_statement_settles_the_entry() {
    let (home, owed) = (keeper(1, 2), owed_with(&[]));
    let outcome = deliver(&home, &Reach::to(&owed)).await;
    assert_eq!(
        outcome,
        Delivery::Landed {
            landed: 1,
            replayed: 0
        }
    );
    assert_eq!(owed.calls(), vec![SUCCESSION_SUBMIT_KIND]);
    assert_eq!(
        home.calls(),
        vec![SUCCESSION_LOOKUP_KIND, SUCCESSION_OWED_SETTLE_KIND],
        "the path is read before anything is sent, and settled after"
    );
    assert_eq!(
        home.0.lock().unwrap().settled,
        vec![(id(1).to_vec(), NEST.to_vec())]
    );
}

#[tokio::test]
async fn already_succeeded_settles_without_landing_anything() {
    let (home, owed) = (keeper(1, 2), owed_with(&[Err(codes::ALREADY_SUCCEEDED)]));
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Landed {
            landed: 0,
            replayed: 0
        }
    );
    assert_eq!(home.settled(), 1);
}

#[tokio::test]
async fn an_entry_with_no_address_stays_owed_and_nothing_is_asked() {
    let home = keeper(1, 2);
    home.0.lock().unwrap().owed = vec![entry(1, None)];
    let owed = Fake::default();
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Owed(OwedReason::NoAddress)
    );
    assert!(home.calls().is_empty() && owed.calls().is_empty());
}

#[tokio::test]
async fn an_address_answered_by_another_identity_is_sent_nothing() {
    let (home, owed) = (keeper(1, 2), owed_with(&[]));
    let mut reach = Reach::to(&owed);
    reach.presented = [0xee; 32];
    assert_eq!(
        deliver(&home, &reach).await,
        Delivery::Owed(OwedReason::IdentityMismatch {
            presented: [0xee; 32]
        })
    );
    assert!(owed.calls().is_empty());
    assert_eq!(home.settled(), 0);
}

#[tokio::test]
async fn an_unreachable_nest_stays_owed() {
    let (home, owed) = (keeper(1, 2), owed_with(&[]));
    let mut reach = Reach::to(&owed);
    reach.reachable = false;
    assert!(matches!(
        deliver(&home, &reach).await,
        Delivery::Owed(OwedReason::Unreachable(_))
    ));
    assert_eq!(home.settled(), 0);
}

/// The chain-less arm: the owed nest cannot verify, this device holds the
/// retired seed, so it signs in as the retired identity, replays the keeping
/// nest's chain and submits again.
#[tokio::test]
async fn a_nest_holding_no_chain_is_replayed_the_chain_with_the_held_seed_then_lands() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::NOT_REGISTERED), Ok(())]);
    let mut reach = Reach::to(&owed);
    reach.seeds.insert(id(1));
    reach.accounts.insert(id(1));
    assert_eq!(
        deliver(&home, &reach).await,
        Delivery::Landed {
            landed: 1,
            replayed: 1
        }
    );
    assert_eq!(owed.chain(&id(1)), home.chain(&id(1)));
    assert_eq!(
        owed.calls(),
        vec![
            SUCCESSION_SUBMIT_KIND,
            REGISTRATION_CHAIN_KIND,
            REGISTRATION_SUBMIT_KIND,
            SUCCESSION_SUBMIT_KIND,
        ]
    );
    assert_eq!(home.settled(), 1);
}

/// A chain behind answers `signature_failed`: the same replay.
#[tokio::test]
async fn a_signature_refusal_is_answered_by_the_same_replay() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::SIGNATURE_FAILED), Ok(())]);
    let mut reach = Reach::to(&owed);
    reach.seeds.insert(id(1));
    reach.accounts.insert(id(1));
    assert_eq!(
        deliver(&home, &reach).await,
        Delivery::Landed {
            landed: 1,
            replayed: 1
        }
    );
}

#[tokio::test]
async fn a_nest_holding_no_account_for_the_retired_identity_settles() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::NOT_REGISTERED)]);
    let mut reach = Reach::to(&owed);
    reach.seeds.insert(id(1));
    assert_eq!(deliver(&home, &reach).await, Delivery::NoAccount);
    assert_eq!(home.settled(), 1);
    assert!(owed.chain(&id(1)).is_empty(), "nothing replayed");
}

#[tokio::test]
async fn without_the_retired_seed_a_nest_that_cannot_verify_stays_owed() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::NOT_REGISTERED)]);
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Owed(OwedReason::NoSeed {
            code: codes::NOT_REGISTERED.into()
        })
    );
    assert_eq!(home.settled(), 0);
}

/// Stated bound 3: a key registered there first is never replayed over.
#[tokio::test]
async fn a_chain_the_keeping_nest_does_not_extend_is_never_replayed_over() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::SIGNATURE_FAILED)]);
    owed.0
        .lock()
        .unwrap()
        .chains
        .insert(id(1), vec![registration(1, 9)]);
    let mut reach = Reach::to(&owed);
    reach.seeds.insert(id(1));
    reach.accounts.insert(id(1));
    assert_eq!(
        deliver(&home, &reach).await,
        Delivery::Owed(OwedReason::ChainForked)
    );
    assert_eq!(owed.chain(&id(1)), vec![registration(1, 9)]);
    assert_eq!(home.settled(), 0);
}

/// Stated bound 4: a successor registered there by hand keeps the entry owed.
#[tokio::test]
async fn a_successor_registered_there_by_hand_keeps_the_entry_owed() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err(codes::SUCCESSOR_EXISTS)]);
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Owed(OwedReason::SuccessorExists)
    );
    assert_eq!(home.settled(), 0);
}

#[tokio::test]
async fn any_other_refusal_stays_owed_with_its_code() {
    let home = keeper(1, 2);
    let owed = owed_with(&[Err("fauna.recovery.something_else")]);
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Owed(OwedReason::Refused("fauna.recovery.something_else".into()))
    );
}

/// Several hops are submitted in order; a hop already there moves on.
#[tokio::test]
async fn every_hop_of_the_path_is_submitted_in_order() {
    let home = keeper(1, 2);
    home.0
        .lock()
        .unwrap()
        .paths
        .insert(id(1), vec![statement(1, 2), statement(2, 3)]);
    let owed = owed_with(&[Err(codes::ALREADY_SUCCEEDED), Ok(())]);
    assert_eq!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Landed {
            landed: 1,
            replayed: 0
        }
    );
    assert_eq!(owed.calls().len(), 2);
}

#[tokio::test]
async fn a_settle_the_keeping_nest_does_not_take_leaves_the_entry_owed() {
    let (home, owed) = (keeper(1, 2), owed_with(&[]));
    home.0.lock().unwrap().settle_refused = true;
    assert!(matches!(
        deliver(&home, &Reach::to(&owed)).await,
        Delivery::Owed(OwedReason::Unsettled(_))
    ));
}

#[tokio::test]
async fn the_list_is_delivered_entry_by_entry() {
    let home = keeper(1, 2);
    home.0.lock().unwrap().owed.push(entry(1, None));
    let owed = owed_with(&[]);
    let keeper_conn = Conn {
        nest: home.clone(),
        actor: Some(id(2)),
    };
    let delivered = deliver_owed_nests(&keeper_conn, &Reach::to(&owed))
        .await
        .unwrap();
    let outcomes: Vec<_> = delivered.into_iter().map(|(_, d)| d).collect();
    assert_eq!(
        outcomes,
        vec![
            Delivery::Landed {
                landed: 1,
                replayed: 0
            },
            Delivery::Owed(OwedReason::NoAddress),
        ]
    );
}

/// The link action's half: submits, settles nothing, replays nothing.
#[tokio::test]
async fn the_link_action_submits_the_path_and_counts_what_landed() {
    let owed = owed_with(&[Err(codes::ALREADY_SUCCEEDED), Ok(())]);
    let conn = Conn {
        nest: owed.clone(),
        actor: None,
    };
    assert_eq!(
        submit_statement_path(&conn, &[statement(1, 2), statement(2, 3)]).await,
        Ok(1)
    );
    let owed = owed_with(&[Err(codes::SUCCESSOR_EXISTS)]);
    let conn = Conn {
        nest: owed.clone(),
        actor: None,
    };
    assert_eq!(
        submit_statement_path(&conn, &[statement(1, 2)]).await,
        Err(OwedReason::SuccessorExists)
    );
}
