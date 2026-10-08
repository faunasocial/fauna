//! The PQ-2 parser-hardening surface — one check body per fuzz target, shared
//! verbatim by the cargo-fuzz targets (`fuzz/fuzz_targets/*`) and the
//! merge-gate bounded smoke (`tests/fuzz_smoke.rs`), so a crash found by
//! fuzzing is replayable by the smoke and every corpus entry exercises exactly
//! the fuzzed code path.
//!
//! Contract (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
//! Open questions PQ-2): the peer channel is a remotely-reachable pre-auth
//! parser on consumer devices, and the § Wormability posture's rule 2 names
//! the memory-safe stack necessary but not sufficient — these checks are the
//! parser-hardening sufficiency half, over the three decode links a hostile
//! peer's bytes cross: L2 framing ([`PeerStreamAdapter`]), the dispatcher's
//! wire decode ([`fauna_protocol::envelope::decode_frame`]), and the peer-leg
//! kinds' payload structs.
//!
//! Each function takes arbitrary attacker-controlled bytes and MUST return
//! without panicking — structured errors are the only acceptable failure.
//! The returned outcome values exist for the smoke's corpus-replay assertions
//! (a pristine captured frame must still decode — the additive-everywhere
//! wire-compat canary); fuzz targets ignore them. Nothing here is production
//! API.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{MAX_FRAME_LEN, PeerStreamAdapter};

/// What [`check_framing_decode`] observed while draining the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramingOutcome {
    /// Complete frames the adapter yielded before EOF or the first error.
    pub frames: usize,
    /// `true` iff the stream ended in a clean EOF (no error, no partial frame).
    pub clean_eof: bool,
}

/// An in-memory [`fauna_transport::ByteStream`]: reads serve `data` then EOF;
/// writes are accepted and discarded. Never returns `Pending`, so draining the
/// adapter over it terminates by construction.
struct ReplayStream {
    data: Vec<u8>,
    pos: usize,
}

impl AsyncRead for ReplayStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let remaining = &this.data[this.pos..];
        if remaining.is_empty() {
            return Poll::Ready(Ok(())); // no bytes put = EOF
        }
        let n = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..n]);
        this.pos += n;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ReplayStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Target 1 — the L2 framing path: arbitrary bytes as a peer byte stream into
/// [`PeerStreamAdapter`]'s `LengthDelimitedCodec` decode. Must never panic;
/// every yielded frame is asserted ≤ [`MAX_FRAME_LEN`] (the codec's
/// no-allocation-amplification bound holds at the yield surface — an inbound
/// length prefix past the cap errors instead of buffering).
pub fn check_framing_decode(data: &[u8]) -> FramingOutcome {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    rt.block_on(async {
        let stream = ReplayStream {
            data: data.to_vec(),
            pos: 0,
        };
        let mut adapter = PeerStreamAdapter::new(Box::pin(stream));
        let mut frames = 0usize;
        loop {
            match adapter.next().await {
                Some(Ok(frame)) => {
                    assert!(
                        frame.len() <= MAX_FRAME_LEN,
                        "adapter yielded a frame past MAX_FRAME_LEN ({} > {})",
                        frame.len(),
                        MAX_FRAME_LEN
                    );
                    frames += 1;
                }
                Some(Err(_)) => {
                    return FramingOutcome {
                        frames,
                        clean_eof: false,
                    };
                }
                None => {
                    return FramingOutcome {
                        frames,
                        clean_eof: true,
                    };
                }
            }
        }
    })
}

/// Target 2 — the dispatcher's wire decode: arbitrary bytes through
/// [`fauna_protocol::envelope::decode_frame`] (the discriminant route +
/// `Request`/`Reply`/`Push`/`Cancel` strict decode — exactly what the
/// dispatcher driver runs on every inbound frame) plus the bare
/// `Request`/`Reply` shapes. Must error on garbage, never panic. Returns
/// whether `decode_frame` accepted the bytes (the smoke's replay assertion).
pub fn check_frame_decode(data: &[u8]) -> bool {
    use fauna_protocol::envelope::{Cancel, Push, decode_frame};
    use fauna_protocol::{Reply, Request, decode_strict};

    let frame_ok = decode_frame(data).is_ok();
    let _ = decode_strict::<Request>(data);
    let _ = decode_strict::<Reply>(data);
    let _ = decode_strict::<Push>(data);
    let _ = decode_strict::<Cancel>(data);
    frame_ok
}

/// **Which serve-surface kind each corpus family covers** — the completeness
/// pin's declaration, and the reason a new allowlisted kind cannot ship with
/// an unsmoked pre-auth parser.
///
/// Target 3's struct list and the `kind_payloads/` corpus were hand-written
/// snapshots of the serve surface, and nothing tied them to it. So a fifth
/// entry in `fauna_peer_sync::server::allowlisted_kinds()` re-fired the
/// `peer-channel-hardening-check` gate and passed **green** with a stale
/// struct list and a stale corpus — while the new kind's payload struct
/// became a remotely-reachable pre-auth parser with zero coverage, and the
/// gate's green *read as* "the peer parser surface is hardened". The
/// silent-coverage-cap shape: the number was never wrong, it just answered a
/// question nobody had asked it.
///
/// This table is what the smoke checks the serve surface against, in both
/// directions (`tests/fuzz_smoke.rs`): every allowlisted kind appears here,
/// every row's stems have corpus entries, and every corpus entry belongs to a
/// row. Growing the allowlist without growing the coverage is now a RED.
///
/// Stems are corpus filename prefixes (`<stem>-<n>.bin`), which is also how
/// the smoke maps an entry back to the struct it must decode as.
pub const KIND_PAYLOAD_COVERAGE: &[(&str, &[&str])] = &[
    (
        fauna_protocol::peer::KIND_PEER_NODE_INFO,
        &["peer_node_info_request", "peer_node_info_reply"],
    ),
    // Not in the peer-sync allowlist: the other shipped base kind, served by
    // richer peer nodes. Covered here because target 3 decodes its structs.
    (
        fauna_protocol::peer::KIND_PEER_EXCHANGE,
        &["peer_exchange_request", "peer_exchange_reply"],
    ),
    (
        fauna_protocol::peer_sync::KIND_PEER_SYNC_ADMIT,
        &[
            "peer_sync_admit_request",
            "peer_sync_admit_reply",
            // The inline witness payloads: each rides `EmbedAsBytes` inside
            // the admit exchange, and its inner bytes are strict-decoded
            // pre-auth by the witness verifier — so the inner parsers join
            // the surface beside the envelope structs (T13's ruling names
            // the custody parser explicitly).
            "device_authorization",
            "custody_grant",
        ],
    ),
    (
        "fauna.sync.changes.list",
        &["sync_changes_list_request", "sync_changes_list_reply"],
    ),
    (
        fauna_protocol::peer_sync::KIND_PEER_SYNC_BLOCKS_PULL,
        &[
            "peer_sync_blocks_pull_request",
            "peer_sync_blocks_pull_reply",
            // The pull reply's element type: it crosses the wire inside the
            // reply, and it is decoded on its own by the block path.
            "peer_sync_block",
        ],
    ),
    (
        fauna_protocol::peer_sync::KIND_PEER_SYNC_CHUNKS_PULL,
        &[
            "peer_sync_chunks_pull_request",
            "peer_sync_chunks_pull_reply",
        ],
    ),
];

/// Target 3 — the peer-leg kinds' payload structs: arbitrary bytes through
/// strict decode of every wire struct the peer serve/client decodes. The
/// surface is declared by [`KIND_PAYLOAD_COVERAGE`], which the smoke pins
/// against `fauna-peer-sync`'s own allowlist. Must never panic. Returns how
/// many of the structs accepted the bytes (the smoke asserts each pristine
/// corpus entry decodes as at least its own struct).
pub fn check_kind_payload_decode(data: &[u8]) -> usize {
    use fauna_core::custody_grant::CustodyGrant;
    use fauna_core::data::DeviceAuthorization;
    use fauna_protocol::decode_strict;
    use fauna_protocol::peer::{
        PeerExchangeReply, PeerExchangeRequest, PeerNodeInfoReply, PeerNodeInfoRequest,
    };
    use fauna_protocol::peer_sync::{
        PeerSyncAdmitReply, PeerSyncAdmitRequest, PeerSyncBlock, PeerSyncBlocksPullReply,
        PeerSyncBlocksPullRequest, PeerSyncChunksPullReply, PeerSyncChunksPullRequest,
    };
    use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};

    let mut ok = 0usize;
    macro_rules! try_decode {
        ($ty:ty) => {
            if decode_strict::<$ty>(data).is_ok() {
                ok += 1;
            }
        };
    }
    try_decode!(PeerNodeInfoRequest);
    try_decode!(PeerNodeInfoReply);
    try_decode!(PeerExchangeRequest);
    try_decode!(PeerExchangeReply);
    try_decode!(PeerSyncAdmitRequest);
    try_decode!(PeerSyncAdmitReply);
    try_decode!(DeviceAuthorization);
    try_decode!(CustodyGrant);
    try_decode!(PeerSyncBlock);
    try_decode!(PeerSyncBlocksPullRequest);
    try_decode!(PeerSyncBlocksPullReply);
    try_decode!(PeerSyncChunksPullRequest);
    try_decode!(PeerSyncChunksPullReply);
    try_decode!(SyncChangesListRequest);
    try_decode!(SyncChangesListReply);
    ok
}

/// The CROSS-USER SHARE leg's payload-coverage table — the `p2p-share`
/// plane's own surface, a disjoint sibling of [`KIND_PAYLOAD_COVERAGE`]
/// exactly as `fauna.peer.share.*` is a disjoint sibling of
/// `fauna.peer.sync.*` (wormability rule 5's separability). A separate table
/// (and a separate corpus dir, `fuzz/corpus/kind_payloads_p2p_share/`)
/// because the family compiles away with the feature: folding these rows
/// into the base table would either break the default-features smoke (rows
/// naming gated consts) or ship the gated kind strings ungated.
///
/// Same rules as the base table: every kind the share serve set allowlists
/// appears here, every row's stems have corpus entries, every corpus entry
/// belongs to a row. Since row 59 slice B the share allowlist exists
/// (`fauna_peer_share::server::allowlisted_kinds`) and the smoke pins this
/// table against it, exactly as the base table is pinned against
/// `fauna_peer_sync::server::allowlisted_kinds`. ⚠ The hardening gate must run
/// `--features p2p-share` (`justfile::peer-channel-hardening-check`) — a
/// default-features run never compiles these rows (the union-vs-parts trap).
#[cfg(feature = "p2p-share")]
pub const KIND_PAYLOAD_COVERAGE_P2P_SHARE: &[(&str, &[&str])] = &[
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_ADMIT,
        &[
            "peer_share_admit_request",
            "peer_share_admit_reply",
            // The group-membership witness's carriage + its inner certificate
            // (a fauna-core `GroupRosterRecord`, decoded by the evaluator) —
            // the admit exchange's fourth-witness-kind surface.
            "peer_share_group_witness",
            "group_roster_record",
        ],
    ),
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_CHANGES_LIST,
        &[
            "peer_share_changes_list_request",
            "peer_share_changes_list_reply",
            // The reply's element type: it crosses the wire inside the reply,
            // and the row-provenance check decodes it on its own.
            "peer_share_change",
        ],
    ),
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_MANIFESTS_GET,
        &[
            "peer_share_manifests_get_request",
            "peer_share_manifests_get_reply",
            "peer_share_manifest",
        ],
    ),
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_CHUNKS_PULL,
        &[
            "peer_share_chunks_pull_request",
            "peer_share_chunks_pull_reply",
            "peer_share_chunk",
            // The ranged want — its own struct because chunk pulls are ranged
            // (a chunk body outgrows a frame); it decodes inside the request.
            "peer_share_chunk_want",
        ],
    ),
    // The offline share-initiation ceremony's carriage: the
    // wrapper structs, the frame's own enum, and the inner signed payloads —
    // the inner structs decode with fauna-core's canonical decode at
    // ingest/verify, exactly as the base table's inner witnesses do.
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_CEREMONY_OFFER,
        &[
            "peer_share_ceremony_frame_request",
            "peer_share_ceremony_frame_ack",
            "group_ceremony_message",
            "group_share_offer",
        ],
    ),
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL,
        &[
            "peer_share_ceremony_accept_poll_request",
            "peer_share_ceremony_accept_poll_reply",
            "group_share_accept",
        ],
    ),
    (
        fauna_protocol::peer_share::KIND_PEER_SHARE_CEREMONY_DELIVER,
        &[
            "peer_share_ceremony_frame_request",
            "peer_share_ceremony_frame_ack",
            "group_ceremony_message",
            "group_share_deliver",
        ],
    ),
];

/// Target 3's share-plane twin: arbitrary bytes through strict decode of
/// every `fauna.peer.share.*` wire struct. The surface is declared by
/// [`KIND_PAYLOAD_COVERAGE_P2P_SHARE`]. Must never panic; returns how many
/// structs accepted the bytes.
#[cfg(feature = "p2p-share")]
pub fn check_p2p_share_payload_decode(data: &[u8]) -> usize {
    use fauna_protocol::decode_strict;
    use fauna_protocol::peer_share::{
        PeerShareAdmitReply, PeerShareAdmitRequest, PeerShareCeremonyAcceptPollReply,
        PeerShareCeremonyAcceptPollRequest, PeerShareCeremonyFrameAck,
        PeerShareCeremonyFrameRequest, PeerShareChange, PeerShareChangesListReply,
        PeerShareChangesListRequest, PeerShareChunk, PeerShareChunkWant, PeerShareChunksPullReply,
        PeerShareChunksPullRequest, PeerShareManifest, PeerShareManifestsGetReply,
        PeerShareManifestsGetRequest,
    };

    let mut ok = 0usize;
    macro_rules! try_decode {
        ($ty:ty) => {
            if decode_strict::<$ty>(data).is_ok() {
                ok += 1;
            }
        };
    }
    try_decode!(PeerShareAdmitRequest);
    try_decode!(PeerShareAdmitReply);
    try_decode!(PeerShareChangesListRequest);
    try_decode!(PeerShareChangesListReply);
    try_decode!(PeerShareChange);
    try_decode!(PeerShareManifestsGetRequest);
    try_decode!(PeerShareManifestsGetReply);
    try_decode!(PeerShareManifest);
    try_decode!(PeerShareChunksPullRequest);
    try_decode!(PeerShareChunksPullReply);
    try_decode!(PeerShareChunk);
    try_decode!(PeerShareChunkWant);
    try_decode!(PeerShareCeremonyFrameRequest);
    try_decode!(PeerShareCeremonyFrameAck);
    try_decode!(PeerShareCeremonyAcceptPollRequest);
    try_decode!(PeerShareCeremonyAcceptPollReply);
    try_decode!(fauna_protocol::peer_share::PeerShareGroupWitness);
    // The carried certificate's own decode — fauna-core's canonical decode,
    // the evaluator's production parse path.
    if fauna_core::encoding::canonical_decode::<fauna_core::group_scope::GroupRosterRecord>(data)
        .is_ok()
    {
        ok += 1;
    }
    // The ceremony frame's inner decode surface, through fauna-core's OWN
    // canonical decode — the production parse path at ingest/verify (the
    // same inner-witness discipline as the base table's DeviceAuthorization
    // and CustodyGrant arms).
    if fauna_core::group_ceremony::decode_group_ceremony_message(data).is_ok() {
        ok += 1;
    }
    if fauna_core::encoding::canonical_decode::<fauna_core::group_ceremony::GroupShareOffer>(data)
        .is_ok()
    {
        ok += 1;
    }
    if fauna_core::encoding::canonical_decode::<fauna_core::group_ceremony::GroupShareAccept>(data)
        .is_ok()
    {
        ok += 1;
    }
    if fauna_core::encoding::canonical_decode::<fauna_core::group_ceremony::GroupShareDeliver>(data)
        .is_ok()
    {
        ok += 1;
    }
    ok
}

/// The frame payloads (`Bytes`) a pristine multi-frame capture splits into —
/// used by the corpus generator and the smoke to cross-seed target 2 from
/// target 1's captures. Returns `None` if `data` is not a clean sequence of
/// well-formed `[u32 BE len][payload]` frames.
pub fn split_frames(data: &[u8]) -> Option<Vec<Bytes>> {
    let mut frames = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if len > MAX_FRAME_LEN || rest.len() < 4 + len {
            return None;
        }
        frames.push(Bytes::copy_from_slice(&rest[4..4 + len]));
        rest = &rest[4 + len..];
    }
    Some(frames)
}
