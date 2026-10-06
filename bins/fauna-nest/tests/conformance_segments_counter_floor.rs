//! **tier_3** — `fauna.segments.counter_floor`, the counter floor at
//! acceptance (`segment-backup-protocol.md` § Client-device custodian (pull) →
//! *Restore* → *Recovery into the lived-in nest that regressed*, part (0)).
//!
//! Spoken the way the owner's device speaks it — the production kind handler,
//! dispatched as the owner's own connection — against a real nest's real
//! segment stores: the floor raises the saved counter, never lowers it, is
//! idempotent, takes each family on its own key, is the owner's alone, and is
//! refused beyond the id space's headroom.

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::segments::{SegmentsCounterFloorReply, SegmentsCounterFloorRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

const OWNER: [u8; 32] = [0x31; 32];
const STRANGER: [u8; 32] = [0x32; 32];

fn nest() -> (Arc<AppState>, RpcRouter) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    fauna_nest::segments::register_segments_handlers(&mut b);
    (state, b.build())
}

async fn floor(
    (state, router): &(Arc<AppState>, RpcRouter),
    caller: [u8; 32],
    kind: &str,
    scope: [u8; 32],
    floor: u32,
) -> Result<u32, RpcError> {
    let meta = router
        .kind_meta("fauna.segments.counter_floor")
        .expect("registered");
    let req = SegmentsCounterFloorRequest {
        kind: kind.to_string(),
        actor_id: hex::encode(scope),
        floor,
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let out = (meta.handler)(state.clone(), caller, payload).await?;
    let reply: SegmentsCounterFloorReply = decode(&out).unwrap();
    Ok(reply.next_segment_id)
}

async fn append_mail(state: &Arc<AppState>, tag: u8, received_at_ms: i64) -> u32 {
    fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        &OWNER,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            format!("sealed-body-{tag}").into_bytes(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(b"hint".to_vec()),
        fauna_mail::segments::MailFloorMetadata {
            received_at: received_at_ms,
            ..fauna_mail::segments::MailFloorMetadata::default()
        },
    )
    .await
    .expect("append")
    .seg_id
}

/// **The floor raises the counter, never lowers it, and a repeat is a no-op —
/// and the next segment the owner's mail opens takes it.**
#[tokio::test]
async fn the_owner_floors_their_mail_counter_and_the_next_segment_takes_it() {
    let nest = nest();
    assert_eq!(append_mail(&nest.0, 1, 1_715_000_000_000).await, 1);

    assert_eq!(floor(&nest, OWNER, "mail", OWNER, 9).await.unwrap(), 9);
    assert_eq!(
        floor(&nest, OWNER, "mail", OWNER, 4).await.unwrap(),
        9,
        "a lower floor lowers nothing"
    );
    assert_eq!(floor(&nest, OWNER, "mail", OWNER, 9).await.unwrap(), 9);

    // Another month rotates: the new segment is numbered at the floor, so
    // every id below it — the ones a lost copy numbered — stays spent.
    assert_eq!(append_mail(&nest.0, 2, 1_717_700_000_000).await, 9);
}

/// **Each family floors on its own key**: the journal's tag floors the
/// journal, and leaves the content counter where it was.
#[tokio::test]
async fn the_journal_floors_on_its_own_key() {
    let nest = nest();
    assert_eq!(
        floor(&nest, OWNER, "mail-placement", OWNER, 6)
            .await
            .unwrap(),
        6
    );
    let journal = nest.0.mail_placement.load_manifest(&OWNER).await.unwrap();
    assert_eq!(journal.kind_manifest.next_seg_id, 6);
    let content = nest.0.mail_segments.load_manifest(&OWNER).await.unwrap();
    assert!(
        content.kind_manifest.next_seg_id < 6,
        "the content family was not floored"
    );
}

/// **The floor is the owner's alone**, a tag the plane does not serve is
/// refused, and so is a floor beyond the id space's headroom — each before
/// anything is touched.
#[tokio::test]
async fn a_stranger_an_unknown_kind_and_an_unhonourable_floor_are_refused() {
    let nest = nest();
    let err = floor(&nest, STRANGER, "mail", OWNER, 9).await.unwrap_err();
    assert_eq!(err.code, "fauna.segments.not_owner");
    let err = floor(&nest, OWNER, "nope", OWNER, 9).await.unwrap_err();
    assert_eq!(err.code, "fauna.segments.unknown_kind");
    let err = floor(
        &nest,
        OWNER,
        "mail",
        OWNER,
        fauna_segment_store::MAX_COUNTER_FLOOR + 1,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.segments.invalid_params");

    let km = nest.0.mail_segments.load_manifest(&OWNER).await.unwrap();
    assert!(
        km.kind_manifest.next_seg_id < 9,
        "no refusal moved the counter"
    );
}
