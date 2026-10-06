//! `GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}` — the `.dat` — and
//! `.../{segment_id}/meta` — its `.meta` sidecar — HTTP handlers.
//!
//! Sibling to `chunk_routes::download_chunk`. Returns the framed
//! segment file as opaque bytes (header + payload + footer + trailer
//! per docs/goal/architecture/message-segment-store.md § Segment file
//! format). HTTP/1.1 `Range` supported for resumable fetches (`crate::http_range`).
//!
//! Auth: BearerAuth, owner-or-custodian — the path's `actor_hex` must equal
//! the Bearer's actor, or the Bearer must hold a live custody row covering
//! that owner's `content:<kind>:<actor_hex>` plane
//! (`crate::custody_admission`, row 148 slice 2); otherwise 403. Both refusals
//! are the same `403 non-owner`, so the door is no oracle for someone else's
//! grants.
//!
//! Open-segment safety: calls `SegmentManager::finalize_open`
//! before reading the file so the returned bytes are a self-consistent
//! framed prefix per Plan 5 spec § D1 open-segment behavior — the
//! source's record_count matches the bytes returned. It is also what makes
//! the sidecar servable at all: `.meta` is written only at `finalize()`.
//!
//! # Why the sidecar has a door (2026-08-11)
//!
//! An **adopting replica** needs it: `record_order` lives only in the
//! sidecar, and it is what rebuilds the local record index in append order
//! (`account-data-plane.md` § the bootstrap contract;
//! `fauna_account_store::segments::admit` refuses a pair it cannot check).
//! Since 2026-08-29 the **client-device custodian's pull** fetches it too —
//! opaque, sealed beside the `.dat`, never opened — because a backup corpus
//! holding the `.dat` alone could not be reopened by anything
//! (`message-segment-store.md` § Cross-location backup protocol → *The backup
//! corpus carries the pair*). The nest's own coordinator reads the same pair
//! off disk (`SegmentManager::read_segment_pair`) rather than through here.
//! So the pair is served as two parts of one route family rather than as a
//! second plane — they are one file pair, they share the owner-only auth and
//! the finalize-on-read, and they retire together if the by-CID byte plane
//! (`message-segment-store.md` § Client-device custodian (pull) → *Pull
//! plane*) ever replaces this transitional route.
//!
//! An older nest has no `/meta` and answers 404, which is a clean "this nest
//! predates the sidecar door" for a newer client (§ I2 additive-everywhere) —
//! never a misparse, which is what a query-parameter part would have risked.
//!
//! Plan 5 of the message-segment-store track (design tracked internally).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Which half of the segment pair a request wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The CARv2 container.
    Dat,
    /// The dag-cbor sidecar carrying `record_order`.
    Meta,
}

pub async fn get_segment(
    state: State<Arc<AppState>>,
    bearer: crate::auth::BearerAuth,
    headers: HeaderMap,
    path: Path<(String, String, u32)>,
) -> axum::response::Response {
    serve_part(state, bearer, headers, path, Part::Dat).await
}

/// `GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}/meta` — the sidecar.
pub async fn get_segment_meta(
    state: State<Arc<AppState>>,
    bearer: crate::auth::BearerAuth,
    headers: HeaderMap,
    path: Path<(String, String, u32)>,
) -> axum::response::Response {
    serve_part(state, bearer, headers, path, Part::Meta).await
}

async fn serve_part(
    State(state): State<Arc<AppState>>,
    bearer: crate::auth::BearerAuth,
    headers: HeaderMap,
    Path((kind, actor_hex, segment_id)): Path<(String, String, u32)>,
    part: Part,
) -> axum::response::Response {
    let actor_bytes: [u8; 32] = match fauna_core::hex32::decode(&actor_hex) {
        Ok(b) => b,
        Err(_) => return ApiError::bad_request("invalid actor_hex").into_response(),
    };

    // Owner, or a custodian the owner granted this content plane to (the bulk half of the nest custody door). The custody verdict is
    // the LIVE capability row, re-derived per request by
    // `crate::custody_admission`, so `fauna.capabilities.revoke` severs a live
    // custodian at its very next byte fetch — the same per-dispatch guarantee
    // the WS doors give, on the one surface that is not WS.
    //
    // ⚠ The refusal stays the SAME `403 non-owner` in both directions, so a
    // custodian whose grant does not cover this plane learns exactly what an
    // unrelated stranger learns.
    //
    // This is also why the path's `actor_hex` — not the bearer's actor — is
    // what everything below reads: on the custody path they are deliberately
    // different actors, and the bytes belong to the one in the path.
    if actor_bytes != bearer.0.0
        && crate::custody_admission::admit_segment_plane_for_custody(
            &state,
            &bearer.0.0,
            &actor_bytes,
            &kind,
        )
        .await
        .is_err()
    {
        return ApiError::forbidden("non-owner").into_response();
    }

    // Kinds land on this plane through their own per-kind rollout; the rest
    // 404. One lookup for both the route and the list handler, so the two
    // surfaces cannot disagree about which kinds this nest serves.
    let Some(mgr) = crate::segments::list_handler::served_segments_for_kind(&state, &kind) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Finalize-on-read; idempotent. After this returns, the on-disk
    // segment file is a self-consistent framed prefix that matches the
    // (record_count, size_bytes) the list handler advertises — and the
    // sidecar exists at all, since `finalize()` is what writes it.
    if let Err(e) = mgr.finalize_open(&actor_bytes).await {
        tracing::warn!("finalize_open failed: {e}");
        return ApiError::internal("segment finalize failed").into_response();
    }

    let path = match part {
        Part::Dat => mgr.file_path(&actor_bytes, segment_id),
        Part::Meta => mgr.meta_path(&actor_bytes, segment_id),
    };

    let file_bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            tracing::error!("read segment file: {e}");
            return ApiError::internal("read segment file").into_response();
        }
    };

    // Range support — partial fetch on resume, through the nest's one range
    // parser (`crate::http_range`, shared with the blob route).
    let total = file_bytes.len() as u64;
    match crate::http_range::requested(&headers, total) {
        crate::http_range::ByteRange::Satisfiable { start, end } => {
            let slice = Bytes::copy_from_slice(&file_bytes[start as usize..=end as usize]);
            return crate::http_range::partial_content(Response::builder(), start, end, total)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(axum::body::Body::from(slice))
                .expect("static headers")
                .into_response();
        }
        crate::http_range::ByteRange::Unsatisfiable => {
            return crate::http_range::not_satisfiable(total);
        }
        crate::http_range::ByteRange::Ignore => {}
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::ACCEPT_RANGES, "bytes"),
        ],
        Bytes::from(file_bytes),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::BearerAuth;
    use crate::routes::AppState;
    use crate::segments::test_helpers::{build_state, floor};
    use axum::body::to_bytes;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use fauna_core::identity::ActorId;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Build state, append a single record, finalize, return
    /// (tempdir, state, actor_id, segment_id, on-disk seg-NNNNNNNN.dat bytes).
    async fn build_state_with_one_segment() -> (TempDir, Arc<AppState>, [u8; 32], u32, Vec<u8>) {
        let (tmp, state) = build_state();
        let actor = [0xAAu8; 32];

        let seg_id = crate::segments::mail::append_record(
            &state.mail_segments,
            &state.db,
            &actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed-body-bytes".to_vec(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed-index-hint".to_vec(),
            ),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append")
        .seg_id;

        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("finalize_open");

        let path = state.mail_segments.segment_file_path(&actor, seg_id);
        let bytes = std::fs::read(&path).expect("read segment file");
        (tmp, state, actor, seg_id, bytes)
    }

    #[tokio::test]
    async fn returns_404_for_unknown_segment() {
        let (_tmp, state) = build_state();
        let actor = [0xAAu8; 32];
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), 0u32)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_403_for_non_owner() {
        let (_tmp, state) = build_state();
        let actor = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let bearer = BearerAuth(ActorId(other));
        let resp = get_segment(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), 0u32)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn returns_404_for_unknown_kind() {
        let (_tmp, state) = build_state();
        let actor = [0xAAu8; 32];
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("conv".to_string(), hex::encode(actor), 0u32)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_bytes_for_valid_segment() {
        let (_tmp, state, actor, seg_id, expected_bytes) = build_state_with_one_segment().await;
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), seg_id)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], &expected_bytes[..]);
    }

    /// The placement journal's pair is served through this same route family
    /// under its own tag, verbatim — and a segment id the content family also
    /// holds never answers with the content's bytes.
    #[tokio::test]
    async fn serves_both_halves_of_a_placement_journal_segment_to_its_owner() {
        let (_tmp, state, actor, content_seg, content_bytes) = build_state_with_one_segment().await;
        let journal_seg = state
            .mail_placement
            .append_event(
                &actor,
                &fauna_mail::segments::placement::MailPlacementRecord::Create {
                    mailbox: "INBOX".to_string(),
                    uid_validity: 1,
                    attrs: Vec::new(),
                },
            )
            .await
            .expect("journal a mailbox create");
        assert_eq!(
            journal_seg, content_seg,
            "both families number from 1, which is what makes this a real test"
        );

        let dat = get_segment(
            State(state.clone()),
            BearerAuth(ActorId(actor)),
            HeaderMap::new(),
            Path((
                "mail-placement".to_string(),
                hex::encode(actor),
                journal_seg,
            )),
        )
        .await
        .into_response();
        assert_eq!(dat.status(), StatusCode::OK);
        let dat = to_bytes(dat.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            &dat[..],
            &std::fs::read(state.mail_placement.segment_file_path(&actor, journal_seg))
                .expect("the route finalized the journal segment")[..],
        );
        assert_ne!(
            &dat[..],
            &content_bytes[..],
            "never the content family's file"
        );

        let meta = get_segment_meta(
            State(state.clone()),
            BearerAuth(ActorId(actor)),
            HeaderMap::new(),
            Path((
                "mail-placement".to_string(),
                hex::encode(actor),
                journal_seg,
            )),
        )
        .await
        .into_response();
        assert_eq!(meta.status(), StatusCode::OK);
        let meta = to_bytes(meta.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            &meta[..],
            &std::fs::read(state.mail_placement.segment_meta_path(&actor, journal_seg))
                .expect("read the journal sidecar")[..],
        );

        // A bearer who is not the owner is refused before any file is read.
        let stranger = get_segment(
            State(state),
            BearerAuth(ActorId([0xBBu8; 32])),
            HeaderMap::new(),
            Path((
                "mail-placement".to_string(),
                hex::encode(actor),
                journal_seg,
            )),
        )
        .await
        .into_response();
        assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn honours_range_header() {
        let (_tmp, state, actor, seg_id, expected_bytes) = build_state_with_one_segment().await;
        let bearer = BearerAuth(ActorId(actor));
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=10-".parse().unwrap());
        let resp = get_segment(
            State(state),
            bearer,
            headers,
            Path(("mail".to_string(), hex::encode(actor), seg_id)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        let total = expected_bytes.len();
        assert_eq!(
            resp.headers()[header::CONTENT_RANGE],
            format!("bytes 10-{}/{total}", total - 1).as_str()
        );
        assert_eq!(resp.headers()[header::ACCEPT_RANGES], "bytes");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], &expected_bytes[10..]);
    }

    /// A malformed or multi-range header is ignored and the whole segment
    /// served (RFC 9110 § 14.2) — before the shared parser it answered 416,
    /// failing a fetch over a header the route could simply disregard.
    #[tokio::test]
    async fn malformed_or_multi_range_serves_the_whole_segment() {
        for raw in ["bytes=20-10", "bytes=0-1,5-6"] {
            let (_tmp, state, actor, seg_id, expected_bytes) = build_state_with_one_segment().await;
            let mut headers = HeaderMap::new();
            headers.insert(header::RANGE, raw.parse().unwrap());
            let resp = get_segment(
                State(state),
                BearerAuth(ActorId(actor)),
                headers,
                Path(("mail".to_string(), hex::encode(actor), seg_id)),
            )
            .await
            .into_response();
            assert_eq!(resp.status(), StatusCode::OK, "{raw}");
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], &expected_bytes[..]);
        }
    }

    #[tokio::test]
    async fn range_out_of_bounds_returns_416() {
        let (_tmp, state, actor, seg_id, expected_bytes) = build_state_with_one_segment().await;
        let bearer = BearerAuth(ActorId(actor));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::RANGE,
            format!("bytes={}-", expected_bytes.len() + 100)
                .parse()
                .unwrap(),
        );
        let resp = get_segment(
            State(state),
            bearer,
            headers,
            Path(("mail".to_string(), hex::encode(actor), seg_id)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers()[header::CONTENT_RANGE],
            format!("bytes */{}", expected_bytes.len()).as_str()
        );
    }

    /// The sidecar door serves the `.meta` half of the pair — the bytes an
    /// adopting replica's `record_order` rebuild reads and a backup pass never
    /// touches.
    #[tokio::test]
    async fn meta_returns_the_sidecar_bytes() {
        let (_tmp, state, actor, seg_id, _dat) = build_state_with_one_segment().await;
        let expected = std::fs::read(state.mail_segments.segment_meta_path(&actor, seg_id))
            .expect("read sidecar");
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment_meta(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), seg_id)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], &expected[..]);
        assert_ne!(
            &body[..],
            &_dat[..],
            "the sidecar is the other half of the pair, not the container"
        );
    }

    #[tokio::test]
    async fn meta_returns_404_for_unknown_segment() {
        let (_tmp, state) = build_state();
        let actor = [0xAAu8; 32];
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment_meta(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), 0u32)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Same owner-only gate as the `.dat` half — the sidecar names the actor a
    /// segment belongs to, so it is no less the owner's than the container is.
    #[tokio::test]
    async fn meta_returns_403_for_non_owner() {
        let (_tmp, state, actor, seg_id, _) = build_state_with_one_segment().await;
        let bearer = BearerAuth(ActorId([0xBBu8; 32]));
        let resp = get_segment_meta(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("mail".to_string(), hex::encode(actor), seg_id)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// A kind with no per-kind rollout 404s on both halves — one gate, so the
    /// two can never disagree about what this nest serves.
    #[tokio::test]
    async fn meta_returns_404_for_unknown_kind() {
        let (_tmp, state) = build_state();
        let actor = [0xAAu8; 32];
        let bearer = BearerAuth(ActorId(actor));
        let resp = get_segment_meta(
            State(state),
            bearer,
            HeaderMap::new(),
            Path(("conv".to_string(), hex::encode(actor), 0u32)),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
