//! The owed-nests projection Settings renders (`succession-linked-nests.md`
//! § The road → *What the dial does with an entry it cannot land*, *the user
//! ends what the runtime cannot*), driven over scripted fakes.

use fauna_client_core::succession_delivery::{Delivery, OwedReason};
use std::sync::{Arc, Mutex};

use fauna_client_core::linked_nests::{LinkedConnection, LinkedNestTarget};
use fauna_client_recovery::RecoveryClient;
use fauna_client_recovery::linked_fanout::LinkedNestDial;
use fauna_client_recovery::owed_nests::{
    OwedNestsView, OwedOutcome, dismiss_owed_nest, read_owed_nests,
};
use fauna_client_testkit::{RejectingRequester, ScriptedRequester, block_on};
use fauna_protocol::ByteBuf;
use fauna_protocol::pair::{ForwardQueue, PairListReply, PairingRow};
use fauna_protocol::recovery::{
    OwedNest, SUCCESSION_OWED_SETTLE_KIND, SuccessionOwedSettleReply, SuccessionOwedSettleRequest,
    SuccessionStatusReply,
};

fn owed(old: u8, nest: u8, url: Option<&str>) -> OwedNest {
    OwedNest {
        old_actor_id: ByteBuf::from(vec![old; 32]),
        nest_id: ByteBuf::from(vec![nest; 32]),
        nest_url: url.map(str::to_string),
        ..Default::default()
    }
}

fn encoded<T: serde::Serialize>(reply: &T) -> Vec<u8> {
    fauna_protocol::encode_canonical(reply).unwrap().to_vec()
}

#[test]
fn nothing_owed_renders_nothing() {
    assert_eq!(OwedNestsView::project(&[], None, &[]), None);
    // A delivery the keeping nest no longer serves is no entry: presence is
    // the keeping nest's word.
    let stale = [(owed(1, 2, Some("wss://a")), Delivery::NoAccount)];
    assert_eq!(OwedNestsView::project(&[], Some(&stale), &[]), None);
}

#[test]
fn each_served_entry_carries_this_devices_last_outcome() {
    let served = [
        owed(1, 2, Some("wss://unreachable.example")),
        owed(1, 3, Some("wss://impostor.example")),
        owed(1, 4, None),
        owed(1, 5, Some("wss://later.example")),
    ];
    let deliveries = [
        (
            served[0].clone(),
            Delivery::Owed(OwedReason::Unreachable("refused".into())),
        ),
        (
            served[1].clone(),
            Delivery::Owed(OwedReason::Parked { presented: [9; 32] }),
        ),
        (served[2].clone(), Delivery::Owed(OwedReason::NoAddress)),
    ];
    let view = OwedNestsView::project(&served, Some(&deliveries), &[]).expect("four owed");
    let kinds: Vec<_> = view.entries.iter().map(|e| e.outcome.kind()).collect();
    // The fourth was not delivered by this device: never painted as a failure.
    assert_eq!(
        kinds,
        ["unreachable", "other-nest", "no-address", "not-tried"]
    );
    assert_eq!(view.status().args["count"], "4");
    assert_eq!(
        view.entries[0].address_copy().args["address"],
        "wss://unreachable.example"
    );
    assert_eq!(
        view.entries[2].address_copy().key,
        "settings.recovery_kit.owed_nest_address_none"
    );
}

#[test]
fn a_runtime_that_delivered_nothing_reads_not_tried() {
    let served = [owed(1, 2, Some("wss://a"))];
    let view = OwedNestsView::project(&served, None, &[]).unwrap();
    assert_eq!(view.entries[0].outcome, OwedOutcome::NotTried);
}

#[test]
fn the_refusal_code_reaches_the_line() {
    let served = [owed(1, 2, Some("wss://a"))];
    let deliveries = [(
        served[0].clone(),
        Delivery::Owed(OwedReason::Refused(
            "fauna.recovery.signature_failed".into(),
        )),
    )];
    let view = OwedNestsView::project(&served, Some(&deliveries), &[]).unwrap();
    let line = view.entries[0].outcome_copy();
    assert_eq!(line.key, "settings.recovery_kit.owed_nest_refused");
    assert_eq!(line.args["code"], "fauna.recovery.signature_failed");
}

#[test]
fn the_list_is_read_from_the_status_kind() {
    let rpc = ScriptedRequester::new([encoded(&SuccessionStatusReply {
        owed_nests: vec![owed(1, 2, Some("wss://a"))],
        ..Default::default()
    })]);
    let served = block_on(read_owed_nests(&rpc)).unwrap();
    assert_eq!(served.len(), 1);
    assert_eq!(rpc.kinds(), ["fauna.recovery.succession.status"]);
}

#[test]
fn the_dismiss_settles_exactly_that_entry_and_dials_nothing() {
    let served = [owed(1, 2, Some("wss://a")), owed(1, 3, Some("wss://b"))];
    let mut view = OwedNestsView::project(&served, None, &[]).unwrap();
    let bound = RecoveryClient::new(ScriptedRequester::new([encoded(
        &SuccessionOwedSettleReply::default(),
    )]));
    let dial = FakeDial::default();
    let target = view.entries[1].clone();
    block_on(dismiss_owed_nest(&bound, &dial, &target)).unwrap();
    // One call, the settle — nothing else is sent on a dismiss, and nothing
    // is dialed for a bound entry.
    let rpc = bound.transport();
    assert_eq!(rpc.kinds(), [SUCCESSION_OWED_SETTLE_KIND]);
    assert!(dial.opened().is_empty());
    let sent: SuccessionOwedSettleRequest =
        fauna_protocol::decode_strict(&rpc.payloads()[0]).unwrap();
    assert_eq!(sent.old_actor_id.as_ref(), &[1u8; 32]);
    assert_eq!(sent.nest_id.as_ref(), &[3u8; 32]);
    view.remove(&target.key());
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].nest_id, vec![2u8; 32]);
}

#[test]
fn a_failed_dismiss_keeps_the_entry_and_says_why() {
    let served = [owed(1, 2, Some("wss://a"))];
    let view = OwedNestsView::project(&served, None, &[]).unwrap();
    let bound = RecoveryClient::new(RejectingRequester::new().reject(
        SUCCESSION_OWED_SETTLE_KIND,
        fauna_protocol::RpcError::new("fauna.recovery.not_successor", "offline"),
    ));
    let err = block_on(dismiss_owed_nest(
        &bound,
        &FakeDial::default(),
        &view.entries[0],
    ))
    .unwrap_err();
    assert!(!err.is_empty());
    let line = fauna_client_recovery::owed_nests::dismiss_failed_copy(&err);
    assert_eq!(line.key, "settings.recovery_kit.owed_nest_dismiss_failed");
}

// ── Entries a LINKED nest keeps ─────────────────────────────────────────────

const KEEPER: [u8; 32] = [0x4b; 32];
const KEEPER_URL: &str = "wss://keeper.example";

/// A dial that hands out one scripted connection per address, presenting the
/// identity it was given, and records what it opened.
#[derive(Default)]
struct FakeDial {
    nests: Vec<(String, [u8; 32], Arc<ScriptedRequester>)>,
    opened: Mutex<Vec<String>>,
}

impl FakeDial {
    fn with(mut self, url: &str, presented: [u8; 32], rpc: Arc<ScriptedRequester>) -> Self {
        self.nests.push((url.to_string(), presented, rpc));
        self
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }

    fn reach(
        &self,
        target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Arc<ScriptedRequester>>, String> {
        self.opened.lock().unwrap().push(target.nest_url.clone());
        self.nests
            .iter()
            .find(|(url, _, _)| *url == target.nest_url)
            .map(|(_, presented, rpc)| LinkedConnection {
                rpc: Arc::clone(rpc),
                bound_identity: *presented,
            })
            .ok_or_else(|| format!("{}: unreachable", target.nest_url))
    }
}

#[async_trait::async_trait]
impl LinkedNestDial for FakeDial {
    type Owner = Arc<ScriptedRequester>;
    type Anonymous = Arc<ScriptedRequester>;

    async fn owner(
        &self,
        target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Self::Owner>, String> {
        self.reach(target)
    }

    async fn anonymous(
        &self,
        _target: &LinkedNestTarget,
    ) -> Result<LinkedConnection<Self::Anonymous>, String> {
        panic!("a dismiss never opens an anonymous connection")
    }
}

/// The bound nest's `fauna.pair.list` naming the keeper.
fn pairings_naming_the_keeper() -> Vec<u8> {
    encoded(&PairListReply {
        pairings: vec![PairingRow {
            private_nest_id: ByteBuf::from(KEEPER.to_vec()),
            capabilities: Vec::new(),
            expires_at: None,
            created_at: 0,
            label: None,
            nest_url: Some(KEEPER_URL.to_string()),
            extra: Default::default(),
        }],
        forward_queue: ForwardQueue::default(),
        extra: Default::default(),
    })
}

#[test]
fn a_linked_keepers_entries_are_listed_beside_the_bound_nests() {
    let served = [owed(1, 2, Some("wss://a"))];
    let at_keeper = [
        (
            owed(7, 8, Some("wss://far.example")),
            Delivery::Owed(OwedReason::Unreachable("timed out".into())),
        ),
        // Landed at the keeper: the runtime settled it there in that pass.
        (owed(7, 9, Some("wss://done.example")), Delivery::NoAccount),
    ];
    let view = OwedNestsView::project(&served, None, &[(KEEPER, &at_keeper)]).unwrap();
    assert_eq!(view.entries.len(), 2);
    assert_eq!(view.entries[0].keeper, None);
    assert_eq!(view.entries[1].keeper, Some(KEEPER));
    assert_eq!(view.entries[1].nest_id, vec![8u8; 32]);
    assert_eq!(view.entries[1].outcome.kind(), "unreachable");
    assert_eq!(view.status().args["count"], "2");
    // A linked keeper's entries alone still render the list.
    let only_linked = OwedNestsView::project(&[], None, &[(KEEPER, &at_keeper)]).unwrap();
    assert_eq!(only_linked.entries.len(), 1);
}

#[test]
fn a_linked_entry_is_dismissed_at_its_keeper_not_the_bound_nest() {
    let at_keeper = [(
        owed(7, 8, Some("wss://far.example")),
        Delivery::Owed(OwedReason::Parked { presented: [9; 32] }),
    )];
    let mut view = OwedNestsView::project(&[], None, &[(KEEPER, &at_keeper)]).unwrap();
    let bound = RecoveryClient::new(ScriptedRequester::new([pairings_naming_the_keeper()]));
    let keeper = Arc::new(ScriptedRequester::new([encoded(
        &SuccessionOwedSettleReply::default(),
    )]));
    let dial = FakeDial::default().with(KEEPER_URL, KEEPER, Arc::clone(&keeper));
    let entry = view.entries[0].clone();
    block_on(dismiss_owed_nest(&bound, &dial, &entry)).unwrap();
    // The bound session only lists the pairings; the settle goes to the
    // keeper over the one owner connection, and the owed nest is not dialed.
    assert_eq!(bound.transport().kinds(), ["fauna.pair.list"]);
    assert_eq!(dial.opened(), [KEEPER_URL]);
    assert_eq!(keeper.kinds(), [SUCCESSION_OWED_SETTLE_KIND]);
    let sent: SuccessionOwedSettleRequest =
        fauna_protocol::decode_strict(&keeper.payloads()[0]).unwrap();
    assert_eq!(sent.old_actor_id.as_ref(), &[7u8; 32]);
    assert_eq!(sent.nest_id.as_ref(), &[8u8; 32]);
    view.remove(&entry.key());
    assert!(view.entries.is_empty());
}

#[test]
fn a_keeper_answering_as_another_nest_is_sent_nothing() {
    let at_keeper = [(
        owed(7, 8, Some("wss://far.example")),
        Delivery::Owed(OwedReason::NoAddress),
    )];
    let view = OwedNestsView::project(&[], None, &[(KEEPER, &at_keeper)]).unwrap();
    let bound = RecoveryClient::new(ScriptedRequester::new([pairings_naming_the_keeper()]));
    let impostor = Arc::new(ScriptedRequester::new([]));
    let dial = FakeDial::default().with(KEEPER_URL, [0x66; 32], Arc::clone(&impostor));
    let err = block_on(dismiss_owed_nest(&bound, &dial, &view.entries[0])).unwrap_err();
    assert!(err.contains("another nest"), "{err}");
    assert!(impostor.kinds().is_empty());
}

#[test]
fn a_keeper_no_pairing_names_is_not_dialed() {
    let other_keeper = [0x55; 32];
    let at_keeper = [(
        owed(7, 8, Some("wss://far.example")),
        Delivery::Owed(OwedReason::NoAddress),
    )];
    let view = OwedNestsView::project(&[], None, &[(other_keeper, &at_keeper)]).unwrap();
    let bound = RecoveryClient::new(ScriptedRequester::new([pairings_naming_the_keeper()]));
    let dial = FakeDial::default();
    assert!(block_on(dismiss_owed_nest(&bound, &dial, &view.entries[0])).is_err());
    assert!(dial.opened().is_empty());
}
