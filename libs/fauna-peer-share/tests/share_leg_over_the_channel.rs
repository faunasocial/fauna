//! The share leg driven **over a real peer channel**, two nodes in one process
//! on the in-memory transport seam (`fauna_transport::testing::MemTransport`).
//!
//! The in-crate suites cover each half alone: `server::tests` drives the serve
//! handlers directly, `provenance::tests` drives the policy. Neither can catch
//! the thing that actually breaks a leg — the two halves disagreeing across the
//! wire. So this file exercises the pull side (`client.rs`) against the serve
//! side through `PeerChannel`, which is also the only way `PeerShareBlobFetcher`
//! and `fetch_share_changes` get tested at all: both need a channel.
//!
//! What it pins, in the order a real transfer happens:
//!
//! 1. **Mutual admission** — each side evaluates the other's claim against its
//!    OWN roster, and the reply's `admitted_sets` reports what the responder
//!    admitted.
//! 2. **The byte path end to end** — a real chunked manifest served over the
//!    channel and reassembled by the **existing shared walk**
//!    (`fauna_core::file_download::download_file_bytes_by_manifest`), yielding
//!    the original plaintext. This is the claim "peer-served bytes ride the
//!    existing walk" as a test rather than a comment.
//! 3. **Rule 4 at the transfer boundary** — a peer that serves bytes not
//!    matching the address they were asked for is refused by the fetcher, before
//!    the walk ever sees them.
//! 4. **The provenance ruling across the wire** — the serve filter drops another
//!    writer's rows, and the ingest check independently refuses what a
//!    non-conforming peer would send anyway.
//!
//! No wall-clock waits: the clock is injected, and `await_listening` is the one
//! deadline construct (borrowed from the peer-leg harness).

use std::collections::HashMap;
use std::sync::Arc;

use fauna_core::chunk::ChunkManifest;
use fauna_core::chunker::{chunk_file, extract_chunks};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys, download_file_bytes_by_manifest};
use fauna_core::identity::ActorId;
use fauna_peer_channel::PeerNode;
use fauna_peer_share::provenance::{LocalShareChange, RowRefusal};
use fauna_peer_share::server::{ShareServer, ShareServerConfig, ShareStore};
use fauna_peer_share::{
    PeerShareBlobFetcher, SetMembership, admit_share_over, fetch_share_changes,
};
use fauna_peer_sync::quota::QuotaConfig;
use fauna_protocol::sync::SyncChange;
use fauna_transport::EndpointKey;
use fauna_transport::testing::{Listeners, MemTransport, await_listening};

mod common;
use common::Serving;

/// The two actors. On the contact plane the dialed NodeId *is* the actor key
/// (PT-1b), so one constant serves as both identity and endpoint key.
const SPOUSE_A: [u8; 32] = [0xA1; 32];
const SPOUSE_B: [u8; 32] = [0xB2; 32];
/// A third party neither side shares a set with.
const STRANGER: [u8; 32] = [0xCC; 32];

const HOLIDAY_SET: [u8; 32] = [0x4F; 32];
const OTHER_SET: [u8; 32] = [0x50; 32];

const FILE_PATH: &str = "holiday/clip.mp4";

// ── The two seams, as fixtures ───────────────────────────────────────────────

/// A roster that vouches for exactly the pairs it was built with — the same
/// shape the production impl adapts from `FolderGroupCrypto::contains_member`.
struct Roster(Vec<([u8; 32], [u8; 32])>);

impl SetMembership for Roster {
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
        self.0.contains(&(*channel_id, actor.0))
    }
}

#[derive(Default)]
struct Store {
    rows: Vec<LocalShareChange>,
    manifests: HashMap<[u8; 32], Vec<u8>>,
    chunks: HashMap<[u8; 32], Vec<u8>>,
}

#[async_trait::async_trait]
impl ShareStore for Store {
    async fn changes_since(
        &self,
        _set: &[u8; 32],
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>> {
        Ok(self
            .rows
            .iter()
            .filter(|r| r.change.seq > since)
            .take(max_rows as usize)
            .cloned()
            .collect())
    }

    async fn manifest_bytes(
        &self,
        _set: &[u8; 32],
        manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.manifests.get(manifest_hash).cloned())
    }

    async fn chunk_body(
        &self,
        _set: &[u8; 32],
        store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.chunks.get(store_key).cloned())
    }
}

/// The listener is brought up with `PeerNode::start_with` directly, because the
/// crate deliberately ships **no bind door** (the rule-7 capability brake lands
/// with the nest legs — `server`'s module doc). A test binding its own listener
/// is not that door: nothing here ships, and the brake governs when the plane
/// *lights in production*, not whether a test may drive the handlers.
async fn serving(
    key: [u8; 32],
    listeners: &Listeners,
    roster: Roster,
    store: Store,
    claimed: Vec<[u8; 32]>,
) -> Serving {
    let server = Arc::new(ShareServer::new(
        ShareServerConfig {
            display_name: format!("node-{:02x}", key[0]),
            own_actor: key,
            quotas: QuotaConfig::default(),
            now: Arc::new(|| 1_000),
        },
        Arc::new(roster),
        Arc::new(store),
    ));
    server.set_own_claimed_sets(claimed);
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(key),
        listeners: Arc::clone(listeners),
    });
    let node = PeerNode::start_with(transport.clone(), server.handler_factory()).await;
    await_listening(listeners, &key).await;
    Serving {
        key,
        transport,
        _node: node,
    }
}

// ── A real chunked file, sealed the plaintext way ────────────────────────────

/// The set's M2 content key — generation 1, the shape a set gets at bind time.
const CONTENT_KEY: [u8; 32] = [0x7E; 32];
const CONTENT_KEY_VERSION: u64 = 1;

/// What a member's replica holds for one file: the manifest, its address, and
/// the stored chunk bodies keyed by **store key**.
struct Blob {
    manifest: ChunkManifest,
    manifest_bytes: Vec<u8>,
    manifest_hash: ContentHash,
    /// `(store key, stored body)` — the ciphertext hash and the sealed,
    /// compression-framed body, exactly what any store holds.
    chunks: Vec<(ContentHash, Vec<u8>)>,
}

/// Seal `bytes` the way a shared set's chunks actually rest: chunk → frame →
/// encrypt under the set's content key → re-key by ciphertext hash. This is
/// `fauna_sync_engine::seal::seal_blob`'s pipeline, reproduced here because
/// depending on that crate for a fixture would drag `rusqlite` into this graph
/// (its determinism is pinned where it lives).
///
/// **Sealed, not plaintext, and that is load-bearing.** On the
/// unsealed path (public unsealed content) `stored_hashes` is `None`, so a chunk's store key is its
/// *plaintext* hash while the stored body is compression-framed — the two do not
/// match, and a fetcher cannot verify a body against the address it asked for.
/// Owner content has rested sealed unconditionally since 2026-07-13, and a
/// cross-user shared set is sealed under M2 by definition, so every chunk this
/// leg will ever serve has `store key == hash(stored body)`. Using a plaintext
/// fixture here would have tested a corpus the leg never sees and hidden that
/// distinction (it did, for one iteration of this file).
fn sealed_blob(bytes: &[u8]) -> Blob {
    let mut manifest = chunk_file(bytes);
    let plaintext = extract_chunks(bytes, &manifest);
    // Through the one seal door (`fauna_core::chunk_seal`): frame — the
    // self-describing compression prefix every stored chunk carries, which the
    // walk strips on the way out — then encrypt, then re-key by ciphertext.
    let stored: Vec<(ContentHash, Vec<u8>)> =
        fauna_core::chunk_seal::seal_chunk_bodies(&plaintext, &CONTENT_KEY)
            .expect("seal the chunks under the set's content key");
    manifest.stored_hashes = Some(stored.iter().map(|(k, _)| *k).collect());

    // The manifest's hashes ride sealed under the same key, as every writer
    // emits them (`ChunkManifest::wire_form`).
    let manifest_bytes = fauna_core::encoding::canonical_encode(
        &manifest
            .wire_form(Some(&CONTENT_KEY))
            .expect("seal the manifest's hashes"),
    )
    .expect("encode manifest");
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    Blob {
        manifest,
        manifest_bytes,
        manifest_hash,
        chunks: stored,
    }
}

/// The reader's half: the set's content keys, as a member holding generation 1.
fn member_keys() -> FileDownloadKeys {
    FileDownloadKeys {
        content_keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            CONTENT_KEY,
            1_760_000_000,
        )),
        ..Default::default()
    }
}

/// A small body — one chunk well under the serve reply's byte budget. The
/// happy-path fixture for everything that is not about size.
fn holiday_bytes() -> Vec<u8> {
    b"the spouses' holiday clip, frame after frame. "
        .iter()
        .cycle()
        .take(300_000)
        .copied()
        .collect()
}

/// An **ordinary** file: 1.5 MB, which the chunker keeps as ONE chunk (its
/// single-chunk threshold is 8 MB) whose body is far over any one reply's byte
/// budget — and over the peer channel's 1 MiB frame cap. Every real holiday
/// video, photo or document looks like this; the 300 KB fixture above is the
/// unrepresentative case.
fn ordinary_file_bytes() -> Vec<u8> {
    // Genuinely incompressible content (xorshift32 noise), so nothing
    // downstream can shrink it into fitting by accident. The earlier
    // `(i * K) >> 13` ramp LOOKED varied but zstd folded 1.5 MB of it to
    // 171 KB the moment the seal door started compressing (2026-09-03) —
    // exactly the "fits by accident" this fixture exists to rule out; a
    // holiday video does not compress, and neither must this.
    let mut x: u32 = 0x2545_F491;
    (0u32..1_500_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn row(
    seq: i64,
    author: [u8; 32],
    sequenced: bool,
    manifest_hash: &ContentHash,
) -> LocalShareChange {
    LocalShareChange {
        change: SyncChange {
            seq,
            path_hash: hex::encode(
                fauna_core::data::ContentHash::of_raw(FILE_PATH.as_bytes()).digest(),
            ),
            manifest_hash: Some(hex::encode(manifest_hash.digest())),
            size_bytes: 300_000,
            change_type: "create".to_string(),
            created_at: 1_760_000_000,
            path: Some(FILE_PATH.to_string()),
            author_actor_id: Some(hex::encode(author)),
            ..Default::default()
        },
        sequenced,
        locally_authored: true,
        signer_cert: None,
    }
}

// ── The tests ────────────────────────────────────────────────────────────────

/// The whole point of the leg: B holds the holiday clip, A does not, and A ends
/// up with the exact bytes — over the peer channel, through the shared download
/// walk, with no nest in the picture at all.
#[tokio::test]
async fn a_spouse_pulls_the_holiday_clip_byte_for_byte_over_the_peer_channel() {
    let listeners = fauna_transport::testing::listeners();
    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let (manifest_bytes, manifest_hash, chunks) =
        (blob.manifest_bytes, blob.manifest_hash, blob.chunks);

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A), (HOLIDAY_SET, SPOUSE_B)]),
        Store {
            manifests: HashMap::from([(manifest_hash.digest(), manifest_bytes)]),
            chunks: chunks
                .iter()
                .map(|(k, body)| (k.digest(), body.clone()))
                .collect(),
            rows: vec![row(7, SPOUSE_B, true, &manifest_hash)],
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A), (HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    let admission = admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("mutual admission over a shared set");
    assert_eq!(admission.admitted_by_peer, vec![HOLIDAY_SET]);
    assert_eq!(
        admission.we_admitted,
        vec![HOLIDAY_SET],
        "our own evaluation of B's claim is independent of what B admitted"
    );

    // A learns WHAT to pull from the change row...
    let page = fetch_share_changes(Arc::clone(&channel), &HOLIDAY_SET, 0, &SPOUSE_B, true)
        .await
        .expect("changes page");
    assert_eq!(page.accepted.len(), 1, "refused: {:?}", page.refused);
    let wanted = page.accepted[0]
        .change
        .manifest_hash
        .clone()
        .expect("a create row names its manifest");

    // ...and then pulls it through the shared walk, unchanged.
    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let hash_bytes: [u8; 32] = hex::decode(&wanted).unwrap().try_into().unwrap();
    let got = download_file_bytes_by_manifest(
        &fetcher,
        &member_keys(),
        ContentHash::from_digest_raw(hash_bytes),
        Some(CONTENT_KEY_VERSION),
        FILE_PATH,
    )
    .await
    .expect("the shared walk reassembles peer-served bytes");
    assert_eq!(got, bytes, "byte-for-byte, over the peer channel");
}

/// **The size case, which is the ordinary case.** A 1.5 MB file is a single
/// chunk (the chunker's single-chunk threshold is 8 MB), and one chunk body that
/// size exceeds both a reply's byte budget and the peer channel's 1 MiB frame
/// cap — so a whole-body-per-reply pull cannot move it at all. Since "holiday
/// videos between spouses" is the scenario this leg exists for, a leg that only
/// carries sub-budget files carries nothing real.
///
/// Found by this test 2026-08-17, mid-slice-B: the first fixture was 300 KB and
/// passed, which is exactly how a whole-body design survives its own test suite.
#[tokio::test]
async fn an_ordinary_multi_megabyte_file_transfers() {
    let listeners = fauna_transport::testing::listeners();
    let bytes = ordinary_file_bytes();
    let blob = sealed_blob(&bytes);
    let (manifest, manifest_bytes, manifest_hash, chunks) = (
        blob.manifest,
        blob.manifest_bytes,
        blob.manifest_hash,
        blob.chunks,
    );
    assert_eq!(
        manifest.chunk_hashes.len(),
        1,
        "1.5 MB is one chunk — that is the point"
    );
    assert!(
        chunks[0].1.len() > 1024 * 1024,
        "and its body is over the channel's frame cap: {} bytes",
        chunks[0].1.len()
    );

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            manifests: HashMap::from([(manifest_hash.digest(), manifest_bytes)]),
            chunks: chunks
                .iter()
                .map(|(k, body)| (k.digest(), body.clone()))
                .collect(),
            ..Default::default()
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let got = download_file_bytes_by_manifest(
        &fetcher,
        &member_keys(),
        manifest_hash,
        Some(CONTENT_KEY_VERSION),
        FILE_PATH,
    )
    .await
    .expect("an ordinary file must transfer over the share leg");
    assert_eq!(got, bytes, "byte-for-byte");
}

/// Rule 4 at the transfer boundary: a member that serves a body which does not
/// hash to the address it was asked for is refused **by the fetcher**, so the
/// bad bytes never reach the walk's reassembly. Built by poisoning exactly one
/// chunk in B's store, leaving everything else honest.
#[tokio::test]
async fn a_poisoned_chunk_is_refused_at_the_transfer_boundary() {
    let listeners = fauna_transport::testing::listeners();
    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let (manifest_bytes, manifest_hash, chunks) =
        (blob.manifest_bytes, blob.manifest_hash, blob.chunks);

    let mut served: HashMap<[u8; 32], Vec<u8>> = chunks
        .iter()
        .map(|(k, body)| (k.digest(), body.clone()))
        .collect();
    // Same store key, different bytes — the substitution the content address
    // exists to catch.
    let victim = chunks[0].0.digest();
    served.insert(victim, vec![0xDE; 4096]);

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            manifests: HashMap::from([(manifest_hash.digest(), manifest_bytes)]),
            chunks: served,
            ..Default::default()
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let err = fetcher
        .fetch_chunks(&[chunks[0].0], FILE_PATH)
        .await
        .expect_err("a substituted body must be refused");
    // The alternate form: the byte-accounting context wraps the refusal.
    let msg = format!("{err:#}");
    assert!(
        msg.contains("chunk hash mismatch"),
        "the refusal must name the mismatch, not fail vaguely: {msg}"
    );
}

/// The same rule on the manifest arm — a manifest is fetched BY its hash, so a
/// substituted one is caught before it can misdirect the whole download.
#[tokio::test]
async fn a_substituted_manifest_is_refused_before_the_walk_reads_it() {
    let listeners = fauna_transport::testing::listeners();
    let manifest_hash = sealed_blob(&holiday_bytes()).manifest_hash;
    // Ask for the real manifest's hash; B answers with a different manifest's
    // bytes.
    let other_bytes = sealed_blob(b"an entirely different file").manifest_bytes;

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            manifests: HashMap::from([(manifest_hash.digest(), other_bytes)]),
            ..Default::default()
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let err = fetcher
        .fetch_manifest(&manifest_hash)
        .await
        .expect_err("a substituted manifest must be refused");
    assert!(err.to_string().contains("manifest hash mismatch"), "{err}");
}

/// `missing` and `deferred` mean different things, and a **single-hash** fetch is
/// where conflating them does real damage: there is no smaller want list to
/// retry with, so `deferred` says "the manifest body itself is over budget", not
/// "ask someone else". A puller told the wrong one walks the whole membership
/// chasing a manifest every member holds.
///
/// Reachable only for an enormous file (~87 GB), so the point of the test is the
/// **diagnosis**, which is what tells a future slice to make manifest fetches
/// ranged the way chunk pulls already are.
#[tokio::test]
async fn an_over_budget_manifest_is_not_reported_as_missing() {
    let listeners = fauna_transport::testing::listeners();
    // A manifest body over one reply's budget. Its bytes need not be a real
    // manifest: the serve side never parses one, and the fetcher must refuse
    // before any parse anyway.
    let oversized = vec![0xAB; 900 * 1024];
    let hash = ContentHash::of_raw(&oversized);

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            manifests: HashMap::from([(hash.digest(), oversized)]),
            ..Default::default()
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let err = fetcher
        .fetch_manifest(&hash)
        .await
        .expect_err("over budget, so nothing is served");
    let msg = err.to_string();
    assert!(
        msg.contains("exceeds one reply's byte budget"),
        "the peer HOLDS it — the error must say so: {msg}"
    );
    assert!(
        !msg.contains("does not hold"),
        "reporting a held-but-oversized manifest as absent sends the puller \
         round the whole membership for nothing: {msg}"
    );
}

/// A stranger neither side shares a set with gets nothing: its claim is refused
/// at admit, so it never reaches a data kind. The refusal crosses the wire as
/// an `ok = false` reply, which the client half surfaces as an error rather than
/// an empty result.
#[tokio::test]
async fn a_stranger_is_refused_at_the_admit_exchange() {
    let listeners = fauna_transport::testing::listeners();
    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;
    let stranger = serving(
        STRANGER,
        &listeners,
        Roster(vec![]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = stranger.dial(&b).await;
    let err = admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![]),
        &SPOUSE_B,
    )
    .await
    .expect_err("B's roster holds no membership for the stranger");
    assert!(
        err.to_string().contains("admit"),
        "the failure names the exchange it happened in: {err}"
    );
}

/// An admitted member asking for a set it was NOT admitted to is refused per
/// request — the scope check that makes one connection's verdict a set of sets
/// rather than a blanket pass.
#[tokio::test]
async fn an_admitted_member_cannot_reach_a_set_outside_its_verdict() {
    let listeners = fauna_transport::testing::listeners();
    let b = serving(
        SPOUSE_B,
        &listeners,
        // A shares HOLIDAY_SET with B, but not OTHER_SET.
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted for HOLIDAY_SET");

    let err = fetch_share_changes(Arc::clone(&channel), &OTHER_SET, 0, &SPOUSE_B, true)
        .await
        .expect_err("OTHER_SET was never admitted");
    assert!(err.to_string().contains("changes.list"), "{err}");
}

/// The provenance ruling across the wire, lifted (writer-signed change
/// records, ruling (3)) — from both directions at once, over one N-member set:
///
/// * B's store holds two rows of a THIRD writer: one unsigned, one signed. The
///   serve filter drops the unsigned one (nothing could vouch for it) and
///   relays the signed one — A receives a member's row B did not write.
/// * A's ingest judges independently: the relayed signed row verifies through
///   the shared reader as the writer's own; the unsigned one, handed to the
///   judge under B's proven identity, is a refused unsigned relay; and a row a
///   read-only member fabricates with its own key fails the roster, not the
///   serve filter.
///
/// Testing both against the same rows is the point: either mechanism alone
/// would look sufficient, and the leg is only safe because neither trusts the
/// other.
#[tokio::test]
async fn a_third_writers_row_relays_only_signed_and_verifies_on_ingest() {
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::sync_row_verify::{ReaderBinding, RowReader};
    use fauna_protocol::sync_writer_sig::{ChangeSigner, ChangeVerifyError};

    const NONCE: [u8; 32] = [0x4E; 32];
    let writer = ActorKeypair::from_secret([0x55; 32]);
    let read_only = ActorKeypair::from_secret([0x66; 32]);
    let manifest_hash = sealed_blob(b"whatever").manifest_hash;
    let signed_by = |k: &ActorKeypair, seq: i64| {
        let mut r = row(seq, k.actor_id().0, true, &manifest_hash);
        r.change.device_id = Some("de".repeat(32));
        r.locally_authored = false;
        ChangeSigner::direct(k)
            .sign_row(&mut r.change, NONCE)
            .expect("sign");
        r
    };

    let listeners = fauna_transport::testing::listeners();
    let unsigned_third_party = LocalShareChange {
        locally_authored: false,
        ..row(9, STRANGER, true, &manifest_hash)
    };
    let relayed = signed_by(&writer, 10);

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            rows: vec![
                row(7, SPOUSE_B, true, &manifest_hash),
                // Authored elsewhere, held by B — exactly the shape a
                // multi-writer set produces.
                unsigned_third_party.clone(),
                relayed.clone(),
            ],
            ..Default::default()
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    let page = fetch_share_changes(Arc::clone(&channel), &HOLIDAY_SET, 0, &SPOUSE_B, true)
        .await
        .expect("changes page");
    let seqs: Vec<i64> = page.accepted.iter().map(|c| c.change.seq).collect();
    assert_eq!(
        seqs,
        vec![7, 10],
        "the signed third-writer row relays; the unsigned one never crossed the wire"
    );
    assert!(page.refused.is_empty(), "nothing to refuse — none was sent");

    // And the authoritative ingest half, standing on its own.
    let mut reader = RowReader::new();
    reader.install_binding(ReaderBinding {
        set_nonce: Some(NONCE),
        owner: Some(SPOUSE_A),
        ..Default::default()
    });
    reader.install_roster(
        [SPOUSE_B, writer.actor_id().0]
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
    );
    let b_hex = hex::encode(SPOUSE_B);
    assert_eq!(
        fauna_peer_share::judge_peer_row(&reader, &page.accepted[1].change, true, &b_hex, true),
        Ok(fauna_peer_share::PeerRowAdmission::Verified {
            writer: writer.actor_id().0
        }),
        "a member's row B did not write verifies self-contained"
    );
    assert_eq!(
        fauna_peer_share::judge_peer_row(&reader, &unsigned_third_party.change, true, &b_hex, true),
        Err(RowRefusal::DidNotVerify(
            fauna_protocol::sync_writer_sig::ChangeVerifyError::Unsigned
        )),
        "a relayed-but-unsigned row is refused — as unsigned, under the flipped switch"
    );
    assert_eq!(
        fauna_peer_share::judge_peer_row(
            &reader,
            &signed_by(&read_only, 11).change,
            true,
            &b_hex,
            true
        ),
        Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter)),
        "a read-only member's fabricated row fails the roster on the signed actor"
    );
}

/// The fail-closed arm over the wire: with no cached writer role for B, A pulls
/// B's rows and refuses every one of them — while the byte path stays open
/// (nothing in the provenance module gates bytes). "Rows refused, bytes still
/// served" is the ruling's exact posture, and this is where it becomes visible.
#[tokio::test]
async fn with_no_cached_writer_role_rows_are_refused_but_bytes_still_flow() {
    let listeners = fauna_transport::testing::listeners();
    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let (manifest_bytes, manifest_hash, chunks) =
        (blob.manifest_bytes, blob.manifest_hash, blob.chunks);

    let b = serving(
        SPOUSE_B,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_A)]),
        Store {
            rows: vec![row(7, SPOUSE_B, true, &manifest_hash)],
            manifests: HashMap::from([(manifest_hash.digest(), manifest_bytes)]),
            chunks: chunks
                .iter()
                .map(|(k, body)| (k.digest(), body.clone()))
                .collect(),
        },
        vec![HOLIDAY_SET],
    )
    .await;
    let a = serving(
        SPOUSE_A,
        &listeners,
        Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        Store::default(),
        vec![HOLIDAY_SET],
    )
    .await;

    let channel = a.dial(&b).await;
    admit_share_over(
        Arc::clone(&channel),
        &[HOLIDAY_SET],
        &Roster(vec![(HOLIDAY_SET, SPOUSE_B)]),
        &SPOUSE_B,
    )
    .await
    .expect("admitted");

    // peer_is_cached_writer = false — the no-cached-role default.
    let page = fetch_share_changes(Arc::clone(&channel), &HOLIDAY_SET, 0, &SPOUSE_B, false)
        .await
        .expect("the request itself succeeds — refusal is per row, not per page");
    assert!(page.accepted.is_empty());
    assert_eq!(
        page.refused
            .iter()
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>(),
        vec![RowRefusal::NotACachedWriter],
        "and the reason is surfaced, not silently dropped"
    );

    // The byte path is untouched by the roster question.
    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), HOLIDAY_SET);
    let got = download_file_bytes_by_manifest(
        &fetcher,
        &member_keys(),
        manifest_hash,
        Some(CONTENT_KEY_VERSION),
        FILE_PATH,
    )
    .await
    .expect("bytes are multi-source regardless of the row question");
    assert_eq!(got, bytes);
}
