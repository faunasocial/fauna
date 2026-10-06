//! The PQ-2 bounded smoke — the merge-gate half of the peer-channel fuzz
//! hardening (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
//! Open questions PQ-2).
//!
//! Replays the checked-in corpus (`fuzz/corpus/<target>/`, captured from a
//! real two-`PeerChannel` exchange by [`regenerate_corpus`]) plus a **fixed,
//! deterministic** mutation + random sweep through the exact check functions
//! the cargo-fuzz targets call (`fauna_peer_channel::hardening`). Seconds, not
//! a fuzz farm — coverage-guided long runs stay manual (`fuzz/README.md`).
//!
//! The pristine-corpus replay doubles as a **wire-compat canary**: a captured
//! frame from an older build must keep decoding (additive-everywhere,
//! `docs/goal/architecture/version-compatibility.md`), so an incompatible
//! wire-struct change goes red here within minutes of landing.
//!
//! Any crash cargo-fuzz ever finds lands as a fixed bug + a file under the
//! target's corpus dir — this smoke then replays it forever as a regression.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fauna_peer_channel::MAX_FRAME_LEN;
use fauna_peer_channel::hardening::{
    check_frame_decode, check_framing_decode, check_kind_payload_decode, split_frames,
};

// ── Corpus access ────────────────────────────────────────────────────────────

fn corpus_dir(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/corpus")
        .join(target)
}

/// All corpus entries for `target`, sorted by filename (deterministic order).
/// A missing or empty corpus is a loud red — an unseeded smoke would pass
/// vacuously while reading as coverage.
fn corpus_entries(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = corpus_dir(target);
    let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("corpus dir {} unreadable ({e}) — run `cargo test -p fauna-peer-channel --test fuzz_smoke -- --ignored regenerate_corpus` and commit the output", dir.display()))
        .map(|entry| {
            let entry = entry.expect("corpus dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = std::fs::read(entry.path()).expect("corpus entry readable");
            (name, bytes)
        })
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "corpus dir {} is empty — the smoke would pass vacuously",
        dir.display()
    );
    entries
}

// ── The deterministic sweep ──────────────────────────────────────────────────

/// xorshift64* — a tiny fixed-seed PRNG so the sweep is identical on every
/// run and machine (no wall-clock, no `getrandom`; convention 14 trivially).
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Fixed mutation set for one corpus entry: truncations, single-bit flips,
/// and splices with a sibling entry. Counts are fixed so the smoke's cost is
/// bounded by corpus size alone.
fn mutations(entry: &[u8], sibling: &[u8], rng: &mut XorShift) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    // Truncations: short prefixes + proportional cuts.
    let mut cuts: Vec<usize> = vec![0, 1, 2, 3, 4, 5];
    for frac in [4, 2] {
        cuts.push(entry.len() / frac);
    }
    cuts.push((entry.len() * 3) / 4);
    cuts.push(entry.len().saturating_sub(1));
    cuts.retain(|&c| c < entry.len());
    cuts.dedup();
    for c in cuts {
        out.push(entry[..c].to_vec());
    }
    // Single-bit flips at 32 PRNG positions.
    if !entry.is_empty() {
        for _ in 0..32 {
            let mut m = entry.to_vec();
            let pos = (rng.next() as usize) % m.len();
            let bit = (rng.next() as u8) % 8;
            m[pos] ^= 1 << bit;
            out.push(m);
        }
    }
    // 8 splices: entry's head + sibling's tail at PRNG cut points.
    if !entry.is_empty() && !sibling.is_empty() {
        for _ in 0..8 {
            let a = (rng.next() as usize) % entry.len();
            let b = (rng.next() as usize) % sibling.len();
            let mut m = entry[..a].to_vec();
            m.extend_from_slice(&sibling[b..]);
            out.push(m);
        }
    }
    out
}

/// 512 PRNG buffers, lengths 0..4096 — the "arbitrary bytes" half of the sweep.
fn random_buffers(rng: &mut XorShift) -> Vec<Vec<u8>> {
    (0..512)
        .map(|_| {
            let len = (rng.next() as usize) % 4096;
            let mut buf = vec![0u8; len];
            for chunk in buf.chunks_mut(8) {
                let bytes = rng.next().to_le_bytes();
                let n = chunk.len();
                chunk.copy_from_slice(&bytes[..n]);
            }
            buf
        })
        .collect()
}

/// Run `check` over every corpus entry's mutations + the random sweep.
fn sweep(target: &str, check: impl Fn(&[u8])) {
    let entries = corpus_entries(target);
    let mut rng = XorShift(0xA5A5_5A5A_DEAD_BEEF); // fixed seed — deterministic sweep
    for (i, (_, entry)) in entries.iter().enumerate() {
        let sibling = &entries[(i + 1) % entries.len()].1;
        for m in mutations(entry, sibling, &mut rng) {
            check(&m);
        }
    }
    for buf in random_buffers(&mut rng) {
        check(&buf);
    }
}

// ── Target 1: L2 framing ─────────────────────────────────────────────────────

#[test]
fn framing_corpus_replays_clean_and_sweep_never_panics() {
    // Pristine captures must decode: ≥1 frame, clean EOF (wire-compat canary).
    for (name, entry) in corpus_entries("framing_decode") {
        let outcome = check_framing_decode(&entry);
        assert!(
            outcome.clean_eof && outcome.frames >= 1,
            "pristine framing capture {name} no longer decodes cleanly \
             (frames={}, clean_eof={}) — a wire-compat break, not a smoke flake",
            outcome.frames,
            outcome.clean_eof
        );
    }
    // The no-amplification bound: a prefix past MAX_FRAME_LEN errors with no
    // frame yielded (the codec refuses to buffer it).
    let oversize = ((MAX_FRAME_LEN as u32) + 1).to_be_bytes().to_vec();
    let outcome = check_framing_decode(&oversize);
    assert_eq!(outcome.frames, 0);
    assert!(!outcome.clean_eof, "oversize length prefix must error");
    // The fixed sweep: must not panic (the check asserts the frame bound).
    sweep("framing_decode", |data| {
        let _ = check_framing_decode(data);
    });
}

// ── Target 2: the dispatcher's wire decode ───────────────────────────────────

#[test]
fn frame_decode_corpus_replays_ok_and_sweep_never_panics() {
    for (name, entry) in corpus_entries("frame_decode") {
        assert!(
            check_frame_decode(&entry),
            "pristine captured frame {name} no longer decode_frame()s — \
             a wire-compat break, not a smoke flake"
        );
    }
    sweep("frame_decode", |data| {
        let _ = check_frame_decode(data);
    });
}

// ── Target 3: kind payload decode ────────────────────────────────────────────

#[test]
fn kind_payload_corpus_replays_ok_and_sweep_never_panics() {
    use fauna_protocol::decode_strict;
    use fauna_protocol::peer::{
        PeerExchangeReply, PeerExchangeRequest, PeerNodeInfoReply, PeerNodeInfoRequest,
    };
    use fauna_protocol::peer_sync::{
        PeerSyncAdmitReply, PeerSyncAdmitRequest, PeerSyncBlock, PeerSyncBlocksPullReply,
        PeerSyncBlocksPullRequest,
    };
    use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};

    // Each pristine entry decodes as its own struct (filename prefix → type).
    for (name, entry) in corpus_entries("kind_payloads") {
        let stem = name.split('-').next().unwrap_or("");
        let ok = match stem {
            "peer_node_info_request" => decode_strict::<PeerNodeInfoRequest>(&entry).is_ok(),
            "peer_node_info_reply" => decode_strict::<PeerNodeInfoReply>(&entry).is_ok(),
            "peer_exchange_request" => decode_strict::<PeerExchangeRequest>(&entry).is_ok(),
            "peer_exchange_reply" => decode_strict::<PeerExchangeReply>(&entry).is_ok(),
            "peer_sync_admit_request" => decode_strict::<PeerSyncAdmitRequest>(&entry).is_ok(),
            "peer_sync_admit_reply" => decode_strict::<PeerSyncAdmitReply>(&entry).is_ok(),
            "device_authorization" => {
                decode_strict::<fauna_core::data::DeviceAuthorization>(&entry).is_ok()
            }
            "custody_grant" => {
                decode_strict::<fauna_core::custody_grant::CustodyGrant>(&entry).is_ok()
            }
            "peer_sync_block" => decode_strict::<PeerSyncBlock>(&entry).is_ok(),
            "peer_sync_blocks_pull_request" => {
                decode_strict::<PeerSyncBlocksPullRequest>(&entry).is_ok()
            }
            "peer_sync_blocks_pull_reply" => {
                decode_strict::<PeerSyncBlocksPullReply>(&entry).is_ok()
            }
            "sync_changes_list_request" => decode_strict::<SyncChangesListRequest>(&entry).is_ok(),
            "sync_changes_list_reply" => decode_strict::<SyncChangesListReply>(&entry).is_ok(),
            other => panic!("kind_payloads corpus entry {name} has unknown type prefix {other}"),
        };
        // ⚠ `peer_exchange_{request,reply}-1.bin` were captured while
        // `wg_public_key` still existed on these structs (removed 2026-08-24).
        // They still decode because the field lands in the `extra` catch-all,
        // and that is precisely the point: they are the standing proof that
        // unknown keys still decode against the field-less
        // struct (the `extra` catch-all). Do NOT regenerate them to remove the stale key — doing so
        // deletes the evidence (`version-compatibility.md` § Dimension 2).
        assert!(
            ok,
            "pristine kind payload {name} no longer decodes as its own struct — \
             a wire-compat break, not a smoke flake"
        );
        assert!(check_kind_payload_decode(&entry) >= 1);
    }
    sweep("kind_payloads", |data| {
        let _ = check_kind_payload_decode(data);
    });
}

/// **The completeness pin (PQ-2).** Target 3's coverage was a hand-written
/// snapshot of a serve surface owned by another crate, with nothing tying the
/// two together — so growing the allowlist re-fired this gate and it passed
/// green over the new kind's un-smoked pre-auth parser. These three assertions
/// are that tie, and they are what makes the gate's green mean what it reads
/// as.
///
/// Red-verify (both directions, done at authoring):
/// - add a fake kind to `allowlisted_kinds()` → the first assertion fires;
/// - delete a `kind_payloads` corpus entry → the second fires.
#[test]
fn the_hardening_surface_covers_the_whole_peer_serve_allowlist() {
    use fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE;
    use fauna_peer_sync::server::allowlisted_kinds;

    // 1. Every kind this peer actually serves is a kind target 3 hardens.
    //    This is the assertion that fires when the allowlist grows.
    for kind in allowlisted_kinds() {
        assert!(
            KIND_PAYLOAD_COVERAGE.iter().any(|(k, _)| *k == kind),
            "served kind {kind} has no entry in `hardening::KIND_PAYLOAD_COVERAGE` — its \
             payload struct is a remotely-reachable pre-auth parser with no corpus and no \
             fuzz target. Add the kind's request/reply stems to the table, generate their \
             corpus entries (`cargo test -p fauna-peer-channel --test fuzz_smoke \
             generate_corpus -- --ignored`), and add the structs to \
             `hardening::check_kind_payload_decode`."
        );
    }

    // 2. Every declared stem really has bytes behind it. A table row with no
    //    corpus entry would claim coverage that does not exist — the same
    //    silent cap one level down.
    let present: std::collections::BTreeSet<String> = corpus_entries("kind_payloads")
        .into_iter()
        .map(|(name, _)| name.split('-').next().unwrap_or("").to_string())
        .collect();
    for (kind, stems) in KIND_PAYLOAD_COVERAGE {
        for stem in *stems {
            assert!(
                present.contains(*stem),
                "coverage table claims {kind} is covered by corpus stem {stem}, but \
                 `kind_payloads/` holds no {stem}-*.bin"
            );
        }
    }

    // 3. And no corpus entry is orphaned — an entry belonging to no kind is
    //    either a stale snapshot or a kind someone forgot to declare.
    let declared: std::collections::BTreeSet<&str> = KIND_PAYLOAD_COVERAGE
        .iter()
        .flat_map(|(_, stems)| stems.iter().copied())
        .collect();
    for stem in &present {
        assert!(
            declared.contains(stem.as_str()),
            "corpus stem {stem} belongs to no kind in `KIND_PAYLOAD_COVERAGE` — declare \
             the kind it hardens, or delete the stale entry"
        );
    }
}

// ── Corpus generation (manual; output is committed) ─────────────────────────

/// A tee over one end of a `tokio::io::duplex`: records every byte this end
/// **reads** (= the other end's outbound wire direction, length prefixes and
/// all). Writes pass through untouched.
struct CaptureStream {
    inner: tokio::io::DuplexStream,
    seen: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl tokio::io::AsyncRead for CaptureStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &poll {
            let new = &buf.filled()[before..];
            self.seen.lock().unwrap().extend_from_slice(new);
        }
        poll
    }
}

impl tokio::io::AsyncWrite for CaptureStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Regenerates the checked-in corpus from a REAL captured exchange: two
/// `PeerChannel`s over a duplex, serving the peer-sync allowlist's four kinds
/// (`fauna-peer-sync/src/server.rs::allowlisted_kinds`) plus
/// `fauna.peer.exchange`, with realistic payload fixtures. Run manually, then
/// commit the output:
///
/// ```text
/// cargo test -p fauna-peer-channel --test fuzz_smoke -- --ignored regenerate_corpus
/// ```
///
/// Idempotency keys are freshly random per run, so regenerated bytes differ
/// harmlessly run-to-run; the committed corpus is the stable artifact.
#[tokio::test]
#[ignore = "writes into fuzz/corpus/ — run manually, commit the output"]
async fn regenerate_corpus() {
    use fauna_core::encoding::EmbedAsBytes;
    use fauna_peer_channel::{PeerChannel, PeerHandlers};
    use fauna_protocol::envelope::{Cancel, Frame, Push, encode_frame};
    use fauna_protocol::peer::{
        KIND_PEER_EXCHANGE, KIND_PEER_NODE_INFO, PEER_PROTOCOL_VERSION, PeerExchangeReply,
        PeerExchangeRequest, PeerNodeInfoReply, PeerNodeInfoRequest,
    };
    use fauna_protocol::peer_sync::{
        KIND_PEER_SYNC_ADMIT, KIND_PEER_SYNC_BLOCKS_PULL, PeerSyncAdmitReply, PeerSyncAdmitRequest,
        PeerSyncBlock, PeerSyncBlocksPullReply, PeerSyncBlocksPullRequest,
        WITNESS_DEVICE_AUTHORIZATION,
    };
    use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
    use fauna_protocol::{Value, decode_strict, encode_canonical};
    use fauna_transport::{EndpointKey, PathKind};
    use serde_bytes::ByteBuf;
    use std::sync::{Arc, Mutex};

    fn to_value<T: serde::Serialize>(t: &T) -> Value {
        decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
    }

    fn witness() -> EmbedAsBytes {
        EmbedAsBytes {
            envelope: vec![0xEE; 100],
            bytes: vec![0xBB; 40],
            signer_auth: None,
        }
    }

    // ── The real exchange, captured at the wire ────────────────────────────
    let (a, b) = tokio::io::duplex(256 * 1024);
    let a_seen = Arc::new(Mutex::new(Vec::new())); // bytes A reads = B→A
    let b_seen = Arc::new(Mutex::new(Vec::new())); // bytes B reads = A→B
    let alice = PeerChannel::over_stream(
        Box::pin(CaptureStream {
            inner: a,
            seen: Arc::clone(&a_seen),
        }),
        EndpointKey::from_bytes([2u8; 32]),
        PathKind::Lan,
    );
    let bob = PeerChannel::over_stream(
        Box::pin(CaptureStream {
            inner: b,
            seen: Arc::clone(&b_seen),
        }),
        EndpointKey::from_bytes([1u8; 32]),
        PathKind::Lan,
    );

    let node_info_reply = PeerNodeInfoReply {
        protocol_version: PEER_PROTOCOL_VERSION,
        display_name: "corpus-node".into(),
        ..Default::default()
    };
    let exchange_reply = PeerExchangeReply {
        actor_id: "ef".repeat(32),
        display_name: "corpus-bob".into(),
        ..Default::default()
    };
    let admit_reply = PeerSyncAdmitReply {
        witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
        witness: witness(),
        endpoints: None,
        extra: BTreeMap::new(),
    };
    let changes_reply = SyncChangesListReply {
        changes: vec![SyncChange {
            seq: 1,
            path_hash: "ab".repeat(32),
            manifest_hash: Some("cd".repeat(32)),
            size_bytes: 512,
            change_type: "create".into(),
            created_at: 1_700_000_000,
            path: Some("corpus/state-entry".into()),
            device_id: Some("12".repeat(16)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let pull_reply = PeerSyncBlocksPullReply {
        blocks: vec![PeerSyncBlock {
            cid: ByteBuf::from(vec![0x11u8; 36]),
            bytes: ByteBuf::from(b"corpus block bytes".to_vec()),
            extra: BTreeMap::new(),
        }],
        missing: vec![ByteBuf::from(vec![0x22u8; 36])],
        deferred: vec![ByteBuf::from(vec![0x33u8; 36])],
        extra: BTreeMap::new(),
    };

    let (ni, ex, ad, ch, pu) = (
        node_info_reply.clone(),
        exchange_reply.clone(),
        admit_reply.clone(),
        changes_reply.clone(),
        pull_reply.clone(),
    );
    let _serve = bob.serve(
        PeerHandlers::new()
            .on(KIND_PEER_NODE_INFO, move |_req| {
                let r = ni.clone();
                async move { Ok(to_value(&r)) }
            })
            .on(KIND_PEER_EXCHANGE, move |_req| {
                let r = ex.clone();
                async move { Ok(to_value(&r)) }
            })
            .on(KIND_PEER_SYNC_ADMIT, move |_req| {
                let r = ad.clone();
                async move { Ok(to_value(&r)) }
            })
            .on("fauna.sync.changes.list", move |_req| {
                let r = ch.clone();
                async move { Ok(to_value(&r)) }
            })
            .on(KIND_PEER_SYNC_BLOCKS_PULL, move |_req| {
                let r = pu.clone();
                async move { Ok(to_value(&r)) }
            }),
    );

    let node_info_req = PeerNodeInfoRequest::default();
    let exchange_req = PeerExchangeRequest {
        actor_id: "ab".repeat(32),
        display_name: "corpus-alice".into(),
        nonce_signature: "cd".repeat(64),
        ..Default::default()
    };
    let admit_req = PeerSyncAdmitRequest {
        witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
        witness: witness(),
        endpoints: None,
        extra: BTreeMap::new(),
    };
    let changes_req = SyncChangesListRequest {
        scope: Some(fauna_protocol::account_state::ACCOUNT_STATE_SCOPE.to_string()),
        item_class: Some(
            fauna_protocol::account_state::ItemClass::StateEntry
                .as_wire()
                .to_string(),
        ),
        since: 0,
        frontier: Some(BTreeMap::from([("34".repeat(16), 3i64)])),
        ..Default::default()
    };
    let pull_req = PeerSyncBlocksPullRequest {
        cids: vec![
            ByteBuf::from(vec![0x11u8; 36]),
            ByteBuf::from(vec![0x22u8; 36]),
            ByteBuf::from(vec![0x33u8; 36]),
        ],
        extra: BTreeMap::new(),
    };

    alice
        .request(KIND_PEER_NODE_INFO, to_value(&node_info_req))
        .await
        .expect("node_info");
    alice
        .request(KIND_PEER_EXCHANGE, to_value(&exchange_req))
        .await
        .expect("exchange");
    alice
        .request(KIND_PEER_SYNC_ADMIT, to_value(&admit_req))
        .await
        .expect("admit");
    alice
        .request("fauna.sync.changes.list", to_value(&changes_req))
        .await
        .expect("changes.list");
    alice
        .request(KIND_PEER_SYNC_BLOCKS_PULL, to_value(&pull_req))
        .await
        .expect("blocks.pull");

    drop(alice);
    drop(bob);

    let a_to_b = b_seen.lock().unwrap().clone();
    let b_to_a = a_seen.lock().unwrap().clone();
    assert!(!a_to_b.is_empty() && !b_to_a.is_empty(), "capture empty");

    // ── Write the three corpora ─────────────────────────────────────────────
    let write = |target: &str, name: &str, bytes: &[u8]| {
        let dir = corpus_dir(target);
        std::fs::create_dir_all(&dir).expect("create corpus dir");
        std::fs::write(dir.join(name), bytes).expect("write corpus entry");
    };

    // framing_decode: the full multi-frame streams + each prefixed frame.
    write("framing_decode", "stream-a2b.bin", &a_to_b);
    write("framing_decode", "stream-b2a.bin", &b_to_a);
    for (dir_name, capture) in [("a2b", &a_to_b), ("b2a", &b_to_a)] {
        let frames = split_frames(capture).expect("captured stream splits into clean frames");
        for (i, frame) in frames.iter().enumerate() {
            let mut framed = ((frame.len() as u32).to_be_bytes()).to_vec();
            framed.extend_from_slice(frame);
            write(
                "framing_decode",
                &format!("framed-{dir_name}-{i}.bin"),
                &framed,
            );
        }
    }

    // frame_decode: each captured frame's CBOR payload, plus real encoder
    // output for the two frame types a request/reply exchange never carries.
    for (dir_name, capture) in [("a2b", &a_to_b), ("b2a", &b_to_a)] {
        let frames = split_frames(capture).expect("clean frames");
        for (i, frame) in frames.iter().enumerate() {
            write(
                "frame_decode",
                &format!("payload-{dir_name}-{i}.bin"),
                frame,
            );
        }
    }
    let push = encode_frame(&Frame::Push(Push {
        ty: Push::TYPE,
        kind: "fauna.sync.changed".into(),
        payload: Value::String("corpus".into()),
        seq: 7,
    }))
    .expect("encode push");
    write("frame_decode", "push-1.bin", &push);
    let cancel = encode_frame(&Frame::Cancel(Cancel {
        ty: Cancel::TYPE,
        correlation_id: 42,
    }))
    .expect("encode cancel");
    write("frame_decode", "cancel-1.bin", &cancel);

    // kind_payloads: the canonical encoding of every wire struct exchanged
    // above (filename prefix = struct, consumed by the replay assertions).
    let payloads: Vec<(&str, Vec<u8>)> = vec![
        (
            "peer_node_info_request-1.bin",
            encode_canonical(&node_info_req).unwrap().to_vec(),
        ),
        (
            "peer_node_info_reply-1.bin",
            encode_canonical(&node_info_reply).unwrap().to_vec(),
        ),
        (
            "peer_exchange_request-1.bin",
            encode_canonical(&exchange_req).unwrap().to_vec(),
        ),
        (
            "peer_exchange_reply-1.bin",
            encode_canonical(&exchange_reply).unwrap().to_vec(),
        ),
        (
            "peer_sync_admit_request-1.bin",
            encode_canonical(&admit_req).unwrap().to_vec(),
        ),
        (
            "peer_sync_admit_reply-1.bin",
            encode_canonical(&admit_reply).unwrap().to_vec(),
        ),
        (
            "peer_sync_block-1.bin",
            encode_canonical(&pull_reply.blocks[0]).unwrap().to_vec(),
        ),
        (
            "peer_sync_blocks_pull_request-1.bin",
            encode_canonical(&pull_req).unwrap().to_vec(),
        ),
        (
            "peer_sync_blocks_pull_reply-1.bin",
            encode_canonical(&pull_reply).unwrap().to_vec(),
        ),
        (
            "sync_changes_list_request-1.bin",
            encode_canonical(&changes_req).unwrap().to_vec(),
        ),
        (
            "sync_changes_list_reply-1.bin",
            encode_canonical(&changes_reply).unwrap().to_vec(),
        ),
    ];
    for (name, bytes) in payloads {
        write("kind_payloads", name, &bytes);
    }

    // The inline witness payloads (the admit exchange's inner parsers) — no
    // signature needed: the corpus exercises decode, not verification.
    let device_auth = fauna_core::data::DeviceAuthorization {
        actor_id: fauna_core::identity::ActorId([0xAB; 32]),
        device_key: [0xD1; 32],
        capabilities: vec![fauna_core::data::Capability::RenewBearer],
        created_at: fauna_core::data::Timestamp(1_700_000_000),
        expires_at: Some(fauna_core::data::Timestamp(1_700_100_000)),
    };
    write(
        "kind_payloads",
        "device_authorization-1.bin",
        &encode_canonical(&device_auth).unwrap(),
    );
    // One entry per `CustodyScopeSet` arm, so the fuzzer seeds both shapes.
    let custody_account = fauna_core::custody_grant::CustodyGrant {
        grant_id: vec![0x1D; 16],
        owner: fauna_core::identity::ActorId([0xAB; 32]),
        custodian_key: [0xC5; 32],
        scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
        minted_at: fauna_core::data::Timestamp(1_700_000_000),
        expires_at: fauna_core::data::Timestamp(1_700_100_000),
        removed_devices: Vec::new(),
    };
    write(
        "kind_payloads",
        "custody_grant-1.bin",
        &encode_canonical(&custody_account).unwrap(),
    );
    let custody_scoped = fauna_core::custody_grant::CustodyGrant {
        scopes: fauna_core::custody_grant::CustodyScopeSet::Scopes(vec![
            "state".into(),
            format!("content:conv:{}", "2b".repeat(32)),
        ]),
        ..custody_account
    };
    write(
        "kind_payloads",
        "custody_grant-2.bin",
        &encode_canonical(&custody_scoped).unwrap(),
    );
}

// ── The p2p-share plane's own targets (feature-gated — see
// `hardening::KIND_PAYLOAD_COVERAGE_P2P_SHARE`'s docs: separate table,
// separate corpus dir, and the gate runs `--features p2p-share`) ────────────
#[cfg(feature = "p2p-share")]
mod p2p_share {
    use super::{corpus_dir, corpus_entries, sweep};
    use fauna_core::group_ceremony::{
        GroupShareAccept, GroupShareDeliver, GroupShareOffer, decode_group_ceremony_message,
    };
    use fauna_peer_channel::hardening::{
        KIND_PAYLOAD_COVERAGE_P2P_SHARE, check_p2p_share_payload_decode,
    };
    use fauna_protocol::peer_share::{
        PeerShareAdmitReply, PeerShareAdmitRequest, PeerShareCeremonyAcceptPollReply,
        PeerShareCeremonyAcceptPollRequest, PeerShareCeremonyFrameAck,
        PeerShareCeremonyFrameRequest, PeerShareChange, PeerShareChangesListReply,
        PeerShareChangesListRequest, PeerShareChunk, PeerShareChunkWant, PeerShareChunksPullReply,
        PeerShareChunksPullRequest, PeerShareManifest, PeerShareManifestsGetReply,
        PeerShareManifestsGetRequest,
    };
    use fauna_protocol::{decode_strict, encode_canonical};

    const TARGET: &str = "kind_payloads_p2p_share";

    #[test]
    fn share_kind_payload_corpus_replays_ok_and_sweep_never_panics() {
        for (name, entry) in corpus_entries(TARGET) {
            let stem = name.split('-').next().unwrap_or("");
            let ok = match stem {
                "peer_share_admit_request" => {
                    decode_strict::<PeerShareAdmitRequest>(&entry).is_ok()
                }
                "peer_share_admit_reply" => decode_strict::<PeerShareAdmitReply>(&entry).is_ok(),
                "peer_share_changes_list_request" => {
                    decode_strict::<PeerShareChangesListRequest>(&entry).is_ok()
                }
                "peer_share_changes_list_reply" => {
                    decode_strict::<PeerShareChangesListReply>(&entry).is_ok()
                }
                "peer_share_change" => decode_strict::<PeerShareChange>(&entry).is_ok(),
                "peer_share_manifests_get_request" => {
                    decode_strict::<PeerShareManifestsGetRequest>(&entry).is_ok()
                }
                "peer_share_manifests_get_reply" => {
                    decode_strict::<PeerShareManifestsGetReply>(&entry).is_ok()
                }
                "peer_share_manifest" => decode_strict::<PeerShareManifest>(&entry).is_ok(),
                "peer_share_chunks_pull_request" => {
                    decode_strict::<PeerShareChunksPullRequest>(&entry).is_ok()
                }
                "peer_share_chunks_pull_reply" => {
                    decode_strict::<PeerShareChunksPullReply>(&entry).is_ok()
                }
                "peer_share_chunk" => decode_strict::<PeerShareChunk>(&entry).is_ok(),
                "peer_share_chunk_want" => decode_strict::<PeerShareChunkWant>(&entry).is_ok(),
                "peer_share_ceremony_frame_request" => {
                    decode_strict::<PeerShareCeremonyFrameRequest>(&entry).is_ok()
                }
                "peer_share_ceremony_frame_ack" => {
                    decode_strict::<PeerShareCeremonyFrameAck>(&entry).is_ok()
                }
                "peer_share_ceremony_accept_poll_request" => {
                    decode_strict::<PeerShareCeremonyAcceptPollRequest>(&entry).is_ok()
                }
                "peer_share_ceremony_accept_poll_reply" => {
                    decode_strict::<PeerShareCeremonyAcceptPollReply>(&entry).is_ok()
                }
                "group_ceremony_message" => decode_group_ceremony_message(&entry).is_ok(),
                "peer_share_group_witness" => {
                    decode_strict::<fauna_protocol::peer_share::PeerShareGroupWitness>(&entry)
                        .is_ok()
                }
                "group_roster_record" => fauna_core::encoding::canonical_decode::<
                    fauna_core::group_scope::GroupRosterRecord,
                >(&entry)
                .is_ok(),
                "group_share_offer" => {
                    fauna_core::encoding::canonical_decode::<GroupShareOffer>(&entry).is_ok()
                }
                "group_share_accept" => {
                    fauna_core::encoding::canonical_decode::<GroupShareAccept>(&entry).is_ok()
                }
                "group_share_deliver" => {
                    fauna_core::encoding::canonical_decode::<GroupShareDeliver>(&entry).is_ok()
                }
                other => panic!("{TARGET} corpus entry {name} has unknown type prefix {other}"),
            };
            assert!(
                ok,
                "pristine share kind payload {name} no longer decodes as its own struct — \
                 a wire-compat break, not a smoke flake"
            );
            assert!(check_p2p_share_payload_decode(&entry) >= 1);
        }
        sweep(TARGET, |data| {
            let _ = check_p2p_share_payload_decode(data);
        });
    }

    /// The share table's own completeness pin — same three-way tie as the
    /// base table's (stems ↔ corpus, both directions). The allowlist half
    /// landed with the serve core as
    /// [`share_allowlist_and_hardening_table_stay_tied`] below.
    #[test]
    fn the_share_hardening_surface_and_corpus_tie_both_ways() {
        let present: std::collections::BTreeSet<String> = corpus_entries(TARGET)
            .into_iter()
            .map(|(name, _)| name.split('-').next().unwrap_or("").to_string())
            .collect();
        for (kind, stems) in KIND_PAYLOAD_COVERAGE_P2P_SHARE {
            for stem in *stems {
                assert!(
                    present.contains(*stem),
                    "coverage table claims {kind} is covered by corpus stem {stem}, but \
                     {TARGET}/ holds no {stem}-*.bin"
                );
            }
        }
        let declared: std::collections::BTreeSet<&str> = KIND_PAYLOAD_COVERAGE_P2P_SHARE
            .iter()
            .flat_map(|(_, stems)| stems.iter().copied())
            .collect();
        for stem in &present {
            assert!(
                declared.contains(stem.as_str()),
                "corpus stem {stem} belongs to no kind in \
                 `KIND_PAYLOAD_COVERAGE_P2P_SHARE` — declare the kind it hardens, or \
                 delete the stale entry"
            );
        }
    }

    /// The allowlist half of the tie (walk rule 2, the PQ-2 pinned-to-the-
    /// serve-surface rule extended to the second allowlist): **every kind the
    /// share serve set actually serves has a coverage row here.** Growing the
    /// allowlist without coverage is a RED — which is the whole point of pinning
    /// against the real `const fn` rather than a copied list.
    ///
    /// `fauna.peer.node_info` is deliberately exempt: it is a `fauna.peer.*`
    /// kind, hardened by the BASE table (`peer_node_info_*` stems), and the
    /// share serve set only re-serves it as the pre-witness probe.
    #[test]
    fn share_allowlist_and_hardening_table_stay_tied() {
        let declared: std::collections::BTreeSet<&str> = KIND_PAYLOAD_COVERAGE_P2P_SHARE
            .iter()
            .map(|(kind, _)| *kind)
            .collect();
        for kind in fauna_peer_share::server::allowlisted_kinds() {
            if !kind.starts_with("fauna.peer.share.") {
                assert_eq!(
                    kind,
                    fauna_protocol::peer::KIND_PEER_NODE_INFO,
                    "the share serve set grew a non-share kind ({kind}) — either it \
                     belongs to the base table, or rule 3's least-kind dispatcher \
                     just widened"
                );
                continue;
            }
            assert!(
                declared.contains(kind),
                "the share serve set serves {kind}, but \
                 `KIND_PAYLOAD_COVERAGE_P2P_SHARE` declares no coverage for it — \
                 add its payload structs + corpus entries (walk rule 2)"
            );
        }
        // And nothing in the table claims to harden a kind the serve set does
        // not serve: a stale row would read as coverage of a live surface.
        let served: std::collections::BTreeSet<&str> =
            fauna_peer_share::server::allowlisted_kinds()
                .into_iter()
                .collect();
        for kind in declared {
            assert!(
                served.contains(kind),
                "the coverage table hardens {kind}, which the share serve set no \
                 longer serves — delete the stale row"
            );
        }
    }

    /// Regenerates the share plane's corpus (plain canonical encodings — the
    /// admit exchange has no live serve yet to capture from; when slice B
    /// lands the serve core, this can graduate to a captured exchange like
    /// the base generator). Run manually, then commit the output:
    ///
    /// ```text
    /// cargo test -p fauna-peer-channel --test fuzz_smoke --features p2p-share \
    ///   -- --ignored regenerate_p2p_share_corpus
    /// ```
    #[test]
    #[ignore = "writes into fuzz/corpus/ — run manually, commit the output"]
    fn regenerate_p2p_share_corpus() {
        use fauna_protocol::peer_share::WITNESS_M2_MEMBERSHIP;
        use serde_bytes::ByteBuf;
        use std::collections::BTreeMap;

        let dir = corpus_dir(TARGET);
        std::fs::create_dir_all(&dir).expect("create corpus dir");
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![ByteBuf::from(vec![0x4F; 32]), ByteBuf::from(vec![0x50; 32])],
            group_witnesses: Vec::new(),
            extra: BTreeMap::new(),
        };
        std::fs::write(
            dir.join("peer_share_admit_request-1.bin"),
            encode_canonical(&req).unwrap(),
        )
        .expect("write");
        let reply = PeerShareAdmitReply {
            admitted_sets: vec![ByteBuf::from(vec![0x4F; 32])],
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![ByteBuf::from(vec![0x51; 32])],
            extra: BTreeMap::new(),
        };
        std::fs::write(
            dir.join("peer_share_admit_reply-1.bin"),
            encode_canonical(&reply).unwrap(),
        )
        .expect("write");

        // ── The three data kinds ───────────────────────────
        let write = |name: &str, bytes: bytes::Bytes| {
            std::fs::write(dir.join(name), bytes).expect("write");
        };
        let set = || ByteBuf::from(vec![0x4F; 32]);

        write(
            "peer_share_changes_list_request-1.bin",
            encode_canonical(&PeerShareChangesListRequest {
                set: set(),
                since: 42,
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );
        // A realistic row: the sealed-path + causal-stamp fields a live set
        // carries, so the fuzzer's mutations exercise the nested optionals
        // rather than only the flat ones.
        let row = fauna_protocol::sync::SyncChange {
            seq: 7,
            path_hash: "aa".repeat(32),
            manifest_hash: Some("bb".repeat(32)),
            size_bytes: 1_048_576,
            change_type: "modify".to_string(),
            created_at: 1_760_000_000,
            path: Some("holiday/clip.mp4".to_string()),
            content_key_version: Some(3),
            author_actor_id: Some("c1".repeat(32)),
            path_sealed: Some(ByteBuf::from(vec![0x5E; 48])),
            derived_through: Some(6),
            ..Default::default()
        };
        let sequenced = PeerShareChange {
            change: row.clone(),
            sequenced: true,
            signer_cert: None,
            extra: BTreeMap::new(),
        };
        // The own-pending arm: no author stamp, not sequenced — the shape the
        // provenance ruling admits on channel proof alone.
        let pending = PeerShareChange {
            change: fauna_protocol::sync::SyncChange {
                seq: 0,
                author_actor_id: None,
                ..row
            },
            sequenced: false,
            signer_cert: None,
            extra: BTreeMap::new(),
        };
        write(
            "peer_share_change-1.bin",
            encode_canonical(&sequenced).unwrap(),
        );
        write(
            "peer_share_change-2.bin",
            encode_canonical(&pending).unwrap(),
        );
        write(
            "peer_share_changes_list_reply-1.bin",
            encode_canonical(&PeerShareChangesListReply {
                changes: vec![sequenced, pending],
                more: true,
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );

        let manifest = PeerShareManifest {
            hash: ByteBuf::from(vec![0xA1; 32]),
            bytes: ByteBuf::from(vec![0xCB; 96]),
            extra: BTreeMap::new(),
        };
        write(
            "peer_share_manifest-1.bin",
            encode_canonical(&manifest).unwrap(),
        );
        write(
            "peer_share_manifests_get_request-1.bin",
            encode_canonical(&PeerShareManifestsGetRequest {
                set: set(),
                manifest_hashes: vec![ByteBuf::from(vec![0xA1; 32])],
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );
        // All three outcome vectors non-empty, so a mutation can reach each.
        write(
            "peer_share_manifests_get_reply-1.bin",
            encode_canonical(&PeerShareManifestsGetReply {
                manifests: vec![manifest],
                missing: vec![ByteBuf::from(vec![0xB2; 32])],
                deferred: vec![ByteBuf::from(vec![0xC3; 32])],
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );

        let chunk = PeerShareChunk {
            store_key: ByteBuf::from(vec![0xD4; 32]),
            offset: 716_800,
            bytes: ByteBuf::from(vec![0xE5; 256]),
            total_len: 1_500_000,
            extra: BTreeMap::new(),
        };
        write("peer_share_chunk-1.bin", encode_canonical(&chunk).unwrap());
        // A fresh want and a RESUMED one (offset past zero) — the two shapes a
        // ranged pull actually sends.
        let fresh_want = PeerShareChunkWant {
            store_key: ByteBuf::from(vec![0xD4; 32]),
            offset: 0,
            extra: BTreeMap::new(),
        };
        let resumed_want = PeerShareChunkWant {
            store_key: ByteBuf::from(vec![0xD5; 32]),
            offset: 716_800,
            extra: BTreeMap::new(),
        };
        write(
            "peer_share_chunk_want-1.bin",
            encode_canonical(&fresh_want).unwrap(),
        );
        write(
            "peer_share_chunk_want-2.bin",
            encode_canonical(&resumed_want).unwrap(),
        );
        write(
            "peer_share_chunks_pull_request-1.bin",
            encode_canonical(&PeerShareChunksPullRequest {
                set: set(),
                wants: vec![fresh_want, resumed_want],
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );
        write(
            "peer_share_chunks_pull_reply-1.bin",
            encode_canonical(&PeerShareChunksPullReply {
                chunks: vec![chunk],
                missing: vec![ByteBuf::from(vec![0xE5; 32])],
                deferred: vec![ByteBuf::from(vec![0xF6; 32])],
                extra: BTreeMap::new(),
            })
            .unwrap(),
        );

        // ── The ceremony carriage — REAL signed frames ────
        // Built with fauna-core's own constructors so every signature and
        // content-derived id is genuine; only the deliver's admission wrap is
        // fabricated bytes (it is opaque ciphertext to every parser on this
        // surface). Reception keys are freshly minted per run — regenerated
        // bytes differ harmlessly; the committed corpus is the stable
        // artifact (same note as the base generator's idempotency keys).
        {
            use fauna_core::data::Timestamp;
            use fauna_core::encoding::canonical_encode;
            use fauna_core::group_ceremony::{
                GroupCeremonyMessage, GroupPlaneRow, GroupShareAccept, GroupShareDeliver,
                GroupShareOffer, encode_group_ceremony_message, group_offer_digest,
                sign_group_share_accept, sign_group_share_deliver, sign_group_share_offer,
            };
            use fauna_core::group_generation::{
                GroupReceptionKeyRecord, sign_group_reception_published,
            };
            use fauna_core::group_scope::{GroupBirthRecord, group_scope_id};
            use fauna_core::identity::ActorKeypair;

            let initiator = ActorKeypair::from_secret([0x21; 32]);
            let recipient = ActorKeypair::from_secret([0x31; 32]);
            let birth = GroupBirthRecord {
                authority_actor: initiator.actor_id(),
                salt: [0xB1; 32],
                machinery_root_commit: fauna_core::crypto::GroupMachineryRoot::from_bytes(
                    [0xD7; 32],
                )
                .commitment(),
                created_at_ms: 1_700_000_000_000,
            };
            let scope_id = group_scope_id(&birth).unwrap();
            let offer = GroupShareOffer {
                scope_id,
                initiator: initiator.actor_id(),
                recipient: recipient.actor_id(),
                birth: canonical_encode(&birth).unwrap(),
                offered_at: Timestamp(1_000),
            };
            let offer_env = sign_group_share_offer(&initiator, &offer).unwrap();
            write(
                "group_share_offer-1.bin",
                canonical_encode(&offer).unwrap().to_vec().into(),
            );
            let offer_frame =
                encode_group_ceremony_message(&GroupCeremonyMessage::Offer(offer_env.clone()))
                    .unwrap();
            write("group_ceremony_message-1.bin", offer_frame.clone().into());

            let reception = GroupReceptionKeyRecord::mint(1_000);
            let accept = GroupShareAccept {
                scope_id,
                offer_digest: group_offer_digest(&offer_env).unwrap(),
                recipient: recipient.actor_id(),
                reception_published: sign_group_reception_published(
                    &recipient,
                    reception.reception_pubkey().unwrap(),
                    2_000,
                )
                .unwrap(),
                accepted_at: Timestamp(2_000),
            };
            let accept_env = sign_group_share_accept(&recipient, &accept).unwrap();
            write(
                "group_share_accept-1.bin",
                canonical_encode(&accept).unwrap().to_vec().into(),
            );
            let accept_frame =
                encode_group_ceremony_message(&GroupCeremonyMessage::Accept(accept_env)).unwrap();
            write("group_ceremony_message-2.bin", accept_frame.clone().into());

            let deliver = GroupShareDeliver {
                scope_id,
                initiator: initiator.actor_id(),
                roster_entry_id: [0x7A; 32],
                admission_wrap: vec![0x9C; 180],
                machinery_snapshot: vec![GroupPlaneRow {
                    kind: "fauna.group.birth".into(),
                    key: "birth".into(),
                    value: canonical_encode(&birth).unwrap().to_vec(),
                }],
                delivered_at: Timestamp(3_000),
            };
            let deliver_env = sign_group_share_deliver(&initiator, &deliver).unwrap();
            write(
                "group_share_deliver-1.bin",
                canonical_encode(&deliver).unwrap().to_vec().into(),
            );
            let deliver_frame =
                encode_group_ceremony_message(&GroupCeremonyMessage::Deliver(deliver_env)).unwrap();
            write("group_ceremony_message-3.bin", deliver_frame.clone().into());

            use fauna_protocol::peer_share::{
                PeerShareCeremonyAcceptPollReply, PeerShareCeremonyAcceptPollRequest,
                PeerShareCeremonyFrameAck, PeerShareCeremonyFrameRequest,
            };
            write(
                "peer_share_ceremony_frame_request-1.bin",
                encode_canonical(&PeerShareCeremonyFrameRequest {
                    frame: ByteBuf::from(offer_frame),
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );
            write(
                "peer_share_ceremony_frame_request-2.bin",
                encode_canonical(&PeerShareCeremonyFrameRequest {
                    frame: ByteBuf::from(deliver_frame),
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );
            write(
                "peer_share_ceremony_frame_ack-1.bin",
                encode_canonical(&PeerShareCeremonyFrameAck::default()).unwrap(),
            );
            write(
                "peer_share_ceremony_accept_poll_request-1.bin",
                encode_canonical(&PeerShareCeremonyAcceptPollRequest {
                    scope_id: ByteBuf::from(scope_id.to_vec()),
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );
            // Both poll answers: consent pending, and the owed frame.
            write(
                "peer_share_ceremony_accept_poll_reply-1.bin",
                encode_canonical(&PeerShareCeremonyAcceptPollReply {
                    frame: None,
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );
            write(
                "peer_share_ceremony_accept_poll_reply-2.bin",
                encode_canonical(&PeerShareCeremonyAcceptPollReply {
                    frame: Some(ByteBuf::from(accept_frame)),
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );

            // The group-membership witness carriage: a REAL signed `Enrolled`
            // roster record (the certificate the admit exchange's fourth
            // witness kind carries) + its wire wrapper.
            use fauna_core::data::{Capability, DeviceAuthorization};
            use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
            use fauna_core::group_scope::RosterEntryCore;
            use fauna_protocol::peer_share::PeerShareGroupWitness;

            let device = ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]);
            let cert = DeviceAuthorization {
                actor_id: initiator.actor_id(),
                device_key: device.verifying_key().to_bytes(),
                capabilities: vec![Capability::RenewBearer],
                created_at: Timestamp(1_000),
                expires_at: None,
            };
            let (cert_bytes, cert_env) = sign_envelope(&initiator, &cert).unwrap();
            let core = RosterEntryCore {
                scope_id,
                member_actor: recipient.actor_id(),
                admission_salt: [0x01; 32],
            };
            let (_, record) = fauna_core::group_scope::sign_roster_enrollment(
                &device,
                core,
                vec![0xE0; 8],
                canonical_encode(&EmbedAsBytes::from_signed(cert_bytes, cert_env)).unwrap(),
                2_000,
            )
            .unwrap();
            let enrolled = canonical_encode(&record).unwrap();
            write("group_roster_record-1.bin", enrolled.clone().into());
            write(
                "peer_share_group_witness-1.bin",
                encode_canonical(&PeerShareGroupWitness {
                    scope_id: ByteBuf::from(scope_id.to_vec()),
                    entry: ByteBuf::from(enrolled),
                    extra: BTreeMap::new(),
                })
                .unwrap(),
            );
        }
    }
}
