//! The unknown arms of the enums inside the two records every device of an
//! account merges and rewrites — the channel history slice (`history/<ch>`)
//! and the drafts blob (`__drafts`) — pinned the way
//! `transport.md` § Schema and forward-compat discipline → *Rule 3 in full*
//! asks: a newer writer is modelled (a test-only twin enum with one extra
//! variant, or the newer record's own bytes), this build decodes it, gives the
//! unknown value the restrictive behaviour its ledger line names, and — for a
//! carrying arm — writes back the exact bytes it read.

use fauna_conversations::ConversationsManager;
use fauna_conversations::address::{Rail, TypedAddress, UNKNOWN_ADDRESS_DISPLAY};
use fauna_conversations::capabilities::derive_capabilities;
use fauna_conversations::compose::{ComposeState, RecipientPickerState, ResolveState, SendState};
use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
use fauna_conversations::reactions::StampedReactionEvent;
use fauna_conversations::store::attachments::{AttachmentOpeningKey, SealedBlobCoordinates};
use fauna_conversations::store::{ChannelHistorySlice, DraftStore, merge_history_slices};
use fauna_conversations::thread::{ThreadFlavor, ThreadId};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorId;
use fauna_core::render::{RenderBlock, RenderDocument};
use fauna_mls::types::ReactionOp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type Value = fauna_cbor::Value;

// ---- one enum at a time: the twin writes, the real type reads ----

/// The newer writer's `TypedAddress`: the real variants' shape plus a rail
/// this build lacks.
#[derive(Serialize)]
enum NewerTypedAddress {
    Email { email_address: String },
    Matrix { mxid: String, server: String },
}

#[test]
fn an_unknown_address_decodes_is_inert_and_re_encodes_byte_identically() {
    let bytes = canonical_encode(&NewerTypedAddress::Matrix {
        mxid: "@ada:example.test".into(),
        server: "example.test".into(),
    })
    .unwrap();
    let addr: TypedAddress = canonical_decode(&bytes).unwrap();
    assert!(matches!(addr, TypedAddress::Unknown { .. }), "{addr:?}");
    // Restrictive: no rail (nothing sends to it), no person, a neutral label.
    assert_eq!(addr.rail(), None);
    assert_eq!(addr.person_actor_id(), None);
    assert_eq!(addr.person_handle(), None);
    assert_eq!(addr.display(), UNKNOWN_ADDRESS_DISPLAY);
    assert_eq!(canonical_encode(&addr).unwrap(), bytes);

    // A known variant still decodes as itself.
    let known = canonical_encode(&NewerTypedAddress::Email {
        email_address: "a@example.test".into(),
    })
    .unwrap();
    assert_eq!(
        canonical_decode::<TypedAddress>(&known).unwrap(),
        TypedAddress::Email {
            email_address: "a@example.test".into()
        }
    );
}

#[derive(Serialize)]
enum NewerThreadFlavor {
    #[allow(dead_code)]
    OneToOne,
    Broadcast,
}

#[test]
fn an_unknown_flavor_decodes_withholds_every_affordance_and_re_encodes() {
    let bytes = canonical_encode(&NewerThreadFlavor::Broadcast).unwrap();
    let flavor: ThreadFlavor = canonical_decode(&bytes).unwrap();
    assert!(matches!(flavor, ThreadFlavor::Unknown { .. }), "{flavor:?}");
    assert_eq!(canonical_encode(&flavor).unwrap(), bytes);

    let caps = derive_capabilities(Rail::FaunaMls, flavor.clone());
    assert!(!caps.supports_attachments);
    assert!(!caps.supports_reactions);
    assert!(!caps.supports_message_delete);
    assert!(!caps.supports_per_message_reply);
    assert!(!caps.supports_membership_change);
    assert!(!caps.supports_rename);
    assert!(!caps.can_invite);
    assert!(!caps.can_remove_members);
    assert!(!caps.can_leave_room);
    assert!(!caps.can_set_policy);
    assert!(!fauna_conversations::capabilities::is_in_place_mls_group(
        Rail::FaunaMls,
        flavor
    ));
}

#[derive(Serialize)]
enum NewerOpeningKey {
    SealedSender {
        suite: u16,
        #[serde(with = "serde_bytes")]
        hint: Vec<u8>,
    },
}

#[test]
fn an_unknown_attachment_key_is_carried_byte_identically() {
    let bytes = canonical_encode(&NewerOpeningKey::SealedSender {
        suite: 7,
        hint: vec![1, 2, 3],
    })
    .unwrap();
    let key: AttachmentOpeningKey = canonical_decode(&bytes).unwrap();
    assert!(matches!(key, AttachmentOpeningKey::Unknown(_)), "{key:?}");
    assert_eq!(canonical_encode(&key).unwrap(), bytes);
}

#[derive(Serialize)]
enum NewerResolveState {
    Probing,
}

#[derive(Serialize)]
enum NewerSendState {
    Retrying { attempt: u32 },
    Queued,
}

#[test]
fn an_unknown_transient_state_reads_as_idle() {
    let probing = canonical_encode(&NewerResolveState::Probing).unwrap();
    assert_eq!(
        canonical_decode::<ResolveState>(&probing).unwrap(),
        ResolveState::Idle
    );
    for newer in [
        NewerSendState::Retrying { attempt: 2 },
        NewerSendState::Queued,
    ] {
        let bytes = canonical_encode(&newer).unwrap();
        assert!(matches!(
            canonical_decode::<SendState>(&bytes).unwrap(),
            SendState::Idle
        ));
    }
    // The known forms decode as they always did.
    for state in [
        ResolveState::Idle,
        ResolveState::Resolving,
        ResolveState::Resolved,
        ResolveState::NotFound,
        ResolveState::Error,
    ] {
        let bytes = canonical_encode(&state).unwrap();
        assert_eq!(canonical_decode::<ResolveState>(&bytes).unwrap(), state);
    }
    let failed = SendState::failed("relay down");
    let bytes = canonical_encode(&failed).unwrap();
    let back: SendState = canonical_decode(&bytes).unwrap();
    assert_eq!(canonical_encode(&back).unwrap(), bytes);
    assert!(matches!(back, SendState::Failed { .. }));
    let sending = canonical_encode(&SendState::Sending).unwrap();
    assert!(matches!(
        canonical_decode::<SendState>(&sending).unwrap(),
        SendState::Sending
    ));
}

// ---- the history slice a newer device wrote ----

const CH: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
const BLOB: &str = "b10b";

fn email(name: &str) -> TypedAddress {
    TypedAddress::Email {
        email_address: format!("{name}@example.com"),
    }
}

fn message(id: &str, ts: i64) -> MessageSnapshot {
    MessageSnapshot {
        message_id: MessageId(id.to_string()),
        sender: email("peer"),
        sender_display: String::new(),
        body: "hello".to_string(),
        document: RenderDocument {
            blocks: vec![RenderBlock::Attachment {
                blob_hash: BLOB.to_string(),
                filename: "a.bin".to_string(),
                mime_type: "application/octet-stream".to_string(),
                size_bytes: 3,
                is_image: false,
                c2pa: false,
            }],
        },
        timestamp_ms: ts,
        subject_line: None,
        badges: MessageBadges::default(),
        reply_to: None,
        reactions: vec![],
        deleted: false,
        is_own: false,
        legal_takedown_ref: None,
        labels: vec![],
        plane_ref: None,
        can_delete: false,
    }
}

fn msg_id() -> String {
    format!("conv:{CH}:1")
}

/// What this build would write for the channel — every enum at a known value.
fn known_slice(watermark: i64) -> ChannelHistorySlice {
    ChannelHistorySlice {
        channel_id_hex: CH.to_string(),
        label: "peer".to_string(),
        flavor: ThreadFlavor::MlsGroup,
        participants: vec![email("me"), email("peer")],
        messages: vec![message(&msg_id(), 10)],
        watermark,
        attachment_coordinates: [(
            BLOB.to_string(),
            SealedBlobCoordinates {
                sealed_cid_hex: "cafe".to_string(),
                size_bytes: 3,
                key: AttachmentOpeningKey::MlsEpoch { epoch: 1 },
            },
        )]
        .into(),
        reaction_log: [(
            MessageId(msg_id()),
            vec![StampedReactionEvent::new(
                ActorId([2; 32]),
                "👍".to_string(),
                ReactionOp::Add,
                20,
            )],
        )]
        .into(),
        ..Default::default()
    }
}

fn map(value: &mut Value) -> &mut BTreeMap<String, Value> {
    match value {
        Value::Map(m) => m,
        other => panic!("not a map: {other:?}"),
    }
}

fn list(value: &mut Value) -> &mut Vec<Value> {
    match value {
        Value::List(l) => l,
        other => panic!("not a list: {other:?}"),
    }
}

fn newer_address() -> Value {
    Value::Map(
        [(
            "Matrix".to_string(),
            Value::Map(
                [(
                    "mxid".to_string(),
                    Value::String("@ada:example.test".into()),
                )]
                .into(),
            ),
        )]
        .into(),
    )
}

/// The same slice as a newer device writes it: a flavor, a participant, a
/// sender, an attachment key and a reaction op this build does not name, and
/// one more (an unknown op on a second event) inside the log. Encoded
/// canonically, as that build's own encoder would.
fn newer_slice_bytes(watermark: i64) -> Vec<u8> {
    let mut v: Value =
        canonical_decode(&canonical_encode(&known_slice(watermark)).unwrap()).unwrap();
    let slice = map(&mut v);
    slice.insert("flavor".into(), Value::String("Broadcast".into()));
    list(slice.get_mut("participants").unwrap()).push(newer_address());
    let msg = &mut list(slice.get_mut("messages").unwrap())[0];
    map(msg).insert("sender".into(), newer_address());
    let coords = map(slice.get_mut("attachment_coordinates").unwrap())
        .get_mut(BLOB)
        .unwrap();
    map(coords).insert(
        "key".into(),
        Value::Map(
            [(
                "SealedSender".to_string(),
                Value::Map([("suite".to_string(), Value::Integer(7))].into()),
            )]
            .into(),
        ),
    );
    let log = list(
        map(slice.get_mut("reaction_log").unwrap())
            .get_mut(&msg_id())
            .unwrap(),
    );
    let mut toggle = log[0].clone();
    map(&mut toggle).insert("op".into(), Value::String("Toggle".into()));
    map(&mut toggle).insert("sent_at_ms".into(), Value::Integer(30));
    log.push(toggle);
    canonical_encode(&v).unwrap()
}

fn assert_carries_the_newer_values(slice: &ChannelHistorySlice) {
    assert!(
        matches!(slice.flavor, ThreadFlavor::Unknown { .. }),
        "{:?}",
        slice.flavor
    );
    assert!(
        matches!(
            slice.participants.last(),
            Some(TypedAddress::Unknown { .. })
        ),
        "{:?}",
        slice.participants
    );
    assert!(matches!(
        slice.messages[0].sender,
        TypedAddress::Unknown { .. }
    ));
    assert!(matches!(
        slice.attachment_coordinates[BLOB].key,
        AttachmentOpeningKey::Unknown(_)
    ));
    let log = &slice.reaction_log[&MessageId(msg_id())];
    assert!(
        log.iter()
            .any(|ev| ev.op == ReactionOp::Other("Toggle".into())),
        "{log:?}"
    );
}

#[test]
fn a_newer_history_slice_decodes_and_re_encodes_byte_identically() {
    let bytes = newer_slice_bytes(5);
    let slice = ChannelHistorySlice::from_bytes(&bytes).expect("an older build decodes the slice");
    assert_carries_the_newer_values(&slice);
    assert_eq!(slice.to_bytes().unwrap(), bytes);
}

#[test]
fn merging_a_newer_history_slice_keeps_every_unknown_value() {
    let newer = ChannelHistorySlice::from_bytes(&newer_slice_bytes(5)).unwrap();
    // This device's own copy is older (a lower watermark), so the newer side's
    // scalars and message copies win, as for any merge.
    let mine = known_slice(3);
    let merged = merge_history_slices(&mine, &newer);
    assert_eq!(
        merged,
        merge_history_slices(&newer, &mine),
        "the merge stays symmetric"
    );
    assert_carries_the_newer_values(&merged);
    // A merge with itself writes back exactly what was read.
    assert_eq!(
        merge_history_slices(&newer, &newer).to_bytes().unwrap(),
        newer.to_bytes().unwrap()
    );
}

#[test]
fn a_restored_newer_slice_is_re_uploaded_with_its_unknown_values() {
    let bytes = newer_slice_bytes(5);
    let slice = ChannelHistorySlice::from_bytes(&bytes).unwrap();
    let manager = ConversationsManager::new();
    let id = manager.restore_channel_slice(&slice);

    // Restrictive while held: the thread renders, nothing is offered, and the
    // unknown op never counts as a reaction.
    let detail = manager.thread_detail(id.clone()).unwrap();
    assert!(!detail.capabilities.supports_reactions);
    assert!(!detail.capabilities.supports_membership_change);
    // The newer `Toggle` (stamp 30) outranks the `Add` (stamp 20) for that
    // reactor, and is not an add: no pill.
    assert!(
        detail.messages[0].reactions.is_empty(),
        "{:?}",
        detail.messages[0].reactions
    );

    // The one door both `history/<ch>` writers take: what it writes back
    // still carries every value the newer device wrote.
    let resnapshot = manager.snapshot_channel_slice(&id, CH, 5).unwrap();
    assert_carries_the_newer_values(&resnapshot);
}

#[test]
fn an_unknown_reaction_op_ranks_as_a_retraction() {
    let me = ActorId([1; 32]);
    let reactor = ActorId([2; 32]);
    let add = StampedReactionEvent::new(reactor, "👍".into(), ReactionOp::Add, 20);
    let later =
        StampedReactionEvent::new(reactor, "👍".into(), ReactionOp::Other("Toggle".into()), 30);
    let tied =
        StampedReactionEvent::new(reactor, "👍".into(), ReactionOp::Other("Toggle".into()), 20);
    // A later unknown op beats the add, and on an equal stamp it is the
    // retraction's rank: either way, the reaction is not shown.
    for log in [vec![add.clone(), later], vec![add.clone(), tied]] {
        assert!(fauna_conversations::reactions::fold_reactions(&log, me).is_empty());
    }
    // It never counts as an add on its own.
    let alone =
        StampedReactionEvent::new(reactor, "❤️".into(), ReactionOp::Other("Toggle".into()), 5);
    assert!(fauna_conversations::reactions::fold_reactions(&[alone], me).is_empty());
}

// ---- the drafts blob a newer device wrote ----

fn known_drafts_bytes() -> Vec<u8> {
    let store = DraftStore::new();
    store.set(
        ThreadId("t-1".into()),
        ComposeState {
            body_draft: "half a thought".into(),
            reply_recipients: vec![email("peer")],
            ..Default::default()
        },
    );
    store.set_new_thread(Some(ComposeState {
        body_draft: "new".into(),
        recipient_picker: Some(RecipientPickerState {
            raw_input: "ad".into(),
            chips: vec![email("ada")],
            ..Default::default()
        }),
        ..Default::default()
    }));
    store.snapshot_bytes()
}

#[test]
fn a_newer_drafts_blob_restores_and_re_snapshots_with_its_unknown_values() {
    let known = known_drafts_bytes();
    // The newer writer: an address of a kind this build lacks among the
    // recipients and the chips, and transient states this build does not name.
    let mut v: Value = canonical_decode(&known).unwrap();
    let blob = map(&mut v);
    let entry = &mut list(blob.get_mut("threads").unwrap())[0];
    let compose = map(map(entry).get_mut("compose").unwrap());
    list(compose.get_mut("reply_recipients").unwrap()).push(newer_address());
    compose.insert("send_state".into(), Value::String("Queued".into()));
    let new_thread = map(blob.get_mut("new_thread").unwrap());
    let picker = map(new_thread.get_mut("recipient_picker").unwrap());
    list(picker.get_mut("chips").unwrap()).push(newer_address());
    picker.insert("resolve_state".into(), Value::String("Probing".into()));
    let newer = canonical_encode(&v).unwrap();

    let store = DraftStore::new();
    store
        .restore_from_bytes(&newer)
        .expect("an older build restores the blob");
    let restored = store.get(&ThreadId("t-1".into()));
    assert!(matches!(
        restored.reply_recipients.last(),
        Some(TypedAddress::Unknown { .. })
    ));
    assert!(matches!(restored.send_state, SendState::Idle));
    let picker = store.new_thread().unwrap().recipient_picker.unwrap();
    assert!(matches!(
        picker.chips.last(),
        Some(TypedAddress::Unknown { .. })
    ));
    assert_eq!(picker.resolve_state, ResolveState::Idle);

    // What this build writes back: the carried addresses byte-for-byte, the
    // transient states at the Idle every writer stores.
    let mut expected: Value = canonical_decode(&newer).unwrap();
    let blob = map(&mut expected);
    let entry = &mut list(blob.get_mut("threads").unwrap())[0];
    map(map(entry).get_mut("compose").unwrap())
        .insert("send_state".into(), Value::String("Idle".into()));
    let picker = map(map(blob.get_mut("new_thread").unwrap())
        .get_mut("recipient_picker")
        .unwrap());
    picker.insert("resolve_state".into(), Value::String("Idle".into()));
    assert_eq!(store.snapshot_bytes(), canonical_encode(&expected).unwrap());
}

#[derive(Serialize, Deserialize)]
struct Holder {
    addr: TypedAddress,
}

#[test]
fn an_unknown_address_survives_json_as_its_value() {
    // The wasm snapshot is JSON: an unknown address crosses it as the value
    // the newer writer wrote, never as raw bytes.
    let bytes = canonical_encode(&newer_address()).unwrap();
    let addr: TypedAddress = canonical_decode(&bytes).unwrap();
    let json = serde_json::to_string(&Holder { addr }).unwrap();
    assert_eq!(json, r#"{"addr":{"Matrix":{"mxid":"@ada:example.test"}}}"#);
}
