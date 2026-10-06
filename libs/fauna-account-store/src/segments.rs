//! Verbatim segment adoption — the bulk bootstrap path (charter:
//! `account-data-plane.md` § Store logical schema → *How nest CARv2 segments
//! map onto the local log (the bootstrap contract)*).
//!
//! A fresh replica does not issue ~50 per-domain queries to learn a scope's
//! class-1 truth. It pulls the scope's **whole CARv2 segment files** (the
//! custodian-pull mechanics of `message-segment-store.md` § Client-device
//! custodian (pull), with a plaintext-adoption sink in place of the sealed
//! custody sink), drops them into the store's segment area **byte-for-byte**,
//! and rebuilds its local record index from each segment's sidecar
//! `record_order`.
//!
//! This module is the *logical* half: it takes an offered `(.dat, .meta)` pair
//! as bytes, verifies it, and reports what a backend must file. It is
//! deliberately free of I/O and of native-only types — the same admission runs
//! on web's OPFS backend (W6 (account-data-plane.md § Workstreams)) as on the native SQLite one.
//!
//! # Why the reader-side sidecar view is its own type
//!
//! `fauna-segment-store` owns the *writer* of the `.meta` sidecar and holds a
//! strict, canonical-encoding struct for it. This crate cannot depend on that
//! crate (its dependency floor excludes the engine graph — see the crate
//! docs), but more importantly it must not share that struct even if it could:
//! a **reader** of an at-rest format owes forward tolerance
//! (`version-compatibility.md` § I2 — a newer nest's additively-grown sidecar
//! must still be adoptable by an older app), while the writer owes exactness.
//! One shared strict type would reject exactly the additive growth the compat
//! rule requires. So both implement one documented format —
//! `message-segment-store.md` § Segment file format is the owner — and
//! `the_writers_real_sidecar_is_adoptable` pins them together against the real
//! writer, which is a stronger check than any struct-shape assertion.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use anyhow::{Context, Result, bail};
use fauna_core::data::ContentHash;
use fauna_core::format::hex_full;
use serde::Deserialize;

/// The sidecar `format_version` this reader implements. A segment whose
/// `min_reader_format_version` exceeds this cannot be adopted; a merely newer
/// `format_version` is tolerated (its extra fields are ignored).
pub const SIDECAR_READER_VERSION: u16 = 1;

/// A segment cannot be adopted by this binary: its sidecar declares a reader
/// floor newer than what this binary implements. Typed for the same reason
/// [`crate::store::StoreIncompatible`] is — the honest rendering is "this app
/// is too old", never "corrupt". Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentIncompatible {
    pub format_version: u16,
    pub min_reader: u16,
    pub binary: u16,
}

impl std::fmt::Display for SegmentIncompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "segment sidecar is newer than this binary: format_version {}, \
             min_reader_format_version {}, binary reads {}",
            self.format_version, self.min_reader, self.binary
        )
    }
}

impl std::error::Error for SegmentIncompatible {}

/// A segment's sidecar names a kind that does not match the scope it is filed
/// under. Typed so [`crate::store::AccountStore::bootstrap_scope_segments`]
/// can tell this refusal apart from any other adoption failure and count it
/// instead of aborting the whole scope's pull on it. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentKindMismatch {
    pub sidecar_kind: String,
    pub scope_kind: String,
}

impl std::fmt::Display for SegmentKindMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "segment kind does not match the scope it is filed under — refusing to adopt \
             (sidecar kind {}, scope kind {})",
            self.sidecar_kind, self.scope_kind
        )
    }
}

impl std::error::Error for SegmentKindMismatch {}

/// The sidecar fields an adopting replica reads, decoded from the `.meta`
/// file's canonical dag-cbor (`message-segment-store.md` § Segment file
/// format). Unknown fields are ignored by construction — see the module docs
/// on why this is a view rather than the writer's struct.
///
/// `created_at_secs` and `floor_metadata` are deliberately absent: the account
/// store's index carries neither, and a reader that decodes only what it uses
/// cannot be broken by a change to what it doesn't.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SegmentSidecarView {
    pub format_version: u16,
    /// `#[serde(default)]` mirrors the writer: sidecars written before the
    /// field existed read as the baseline `1` rather than failing.
    #[serde(default = "baseline_min_reader")]
    pub min_reader_format_version: u16,
    /// The segment-store kind tag ("mail" | "conv" | "calendar" | "card" |
    /// "post"). This is also the axis hydration policy is expressed on, so an
    /// adopted record's index row takes its kind from here.
    pub kind: String,
    #[serde(with = "serde_bytes")]
    pub actor_id: [u8; 32],
    pub segment_id: u32,
    pub bucket: String,
    /// Record CIDs in append order — the rebuild source. CARv2's own index is
    /// digest-sorted, so it cannot reconstruct this order.
    pub record_order: Vec<ContentHash>,
}

fn baseline_min_reader() -> u16 {
    1
}

impl SegmentSidecarView {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let view: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| anyhow::anyhow!("segment sidecar: {e}"))?;
        if view.min_reader_format_version > SIDECAR_READER_VERSION {
            return Err(SegmentIncompatible {
                format_version: view.format_version,
                min_reader: view.min_reader_format_version,
                binary: SIDECAR_READER_VERSION,
            }
            .into());
        }
        Ok(view)
    }
}

/// One record inside an adopted segment: its identity and its payload length.
///
/// No offset: a block is read back through the segment's **own** CARv2
/// `MultihashIndexSorted` index (charter: segments are "indexed by their own
/// CARv2 index"). Caching offsets here would mint a second lookup structure
/// that could disagree with the file it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptedBlock {
    pub cid: ContentHash,
    pub len: u64,
}

/// A segment's coordinates within one replica's store. `(scope, kind,
/// segment_id)` is the identity: a scope's segments are per-kind numbered by
/// the nest that wrote them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SegmentKey {
    pub scope: String,
    pub kind: String,
    pub segment_id: u32,
}

/// A verified segment pair, ready to file. Produced only by [`admit`] — a
/// backend never sees unverified segment bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentAdmission {
    pub sidecar: SegmentSidecarView,
    /// Every record the segment carries, in the sidecar's append order.
    pub blocks: Vec<AdoptedBlock>,
}

/// The buffer a staged segment half is written and read back through — the
/// memory one transfer holds on the store's side, whatever the segment's size
/// (`message-segment-store.md` § Segment size: a segment has no size ceiling,
/// so a transfer's memory is this constant, never its body). A buffer size,
/// not a choice anyone makes, so a constant (`principles.md` § One
/// configuration surface).
pub const SEGMENT_TRANSFER_CHUNK: usize = 64 * 1024;

/// Which file of a segment pair a transfer is writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentHalf {
    /// The `.dat` — the CARv2 container.
    Dat,
    /// The `.meta` sidecar.
    Meta,
}

/// Where a transfer writes an offered segment pair as its bytes arrive — the
/// door every [`crate::store::BootstrapSource`] fetches through. A chunk goes
/// straight to the store's staging area
/// ([`crate::backend::StoreBackend::segment_stage`]), so a transfer holds one
/// chunk, never a half.
#[allow(async_fn_in_trait)] // static dispatch only, as StoreBackend
pub trait SegmentSink {
    /// Append `chunk` to `half`.
    async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()>;
}

/// The segment-store kinds whose records are filed under a **hash of their own
/// bytes** — the only kinds [`admit`] can accept.
///
/// [`admit`]'s check 4 re-hashes every block against the CID the container
/// files it under, so a kind is adoptable exactly when its record id IS that
/// hash. `post` qualifies from birth (`Cid::of_dag_cbor(body)` — the nest's
/// `segments::post::append_body`); **`mail` qualifies since its 2026-08-17
/// cutover** (`segments::mail::append_record` derives
/// `Cid::of_dag_cbor(envelope_bytes)`, and the boot-time cutover reconcile
/// tombstones every record filed under the retired sequenced id, so no store
/// running that code holds mail under an upstream digest — a *pre-cutover
/// source* nest's mail segments simply fail [`admit`]'s re-hash: inert, never
/// dangerous). **`calendar` and `card` qualify since their same-day cutover
/// legs** (`segments::cal`/`card::append_record` derive
/// `Cid::of_dag_cbor(envelope_bytes)`, the cid is stored on the DAV row, and
/// the boot reset wiped the pre-cutover corpus). **`conv` qualifies since its
/// 2026-08-17 leg — the last kind to converge** (`segments::conv::append` files
/// under the shared `fauna_mls::segments::encode_record` mint, the retired
/// `derive_record_id(channel_id, seq, body)` is deleted, and `seq` survives as
/// the cross-member coordinate without being the record's name).
///
/// This is deliberately **not** "the kinds the nest serves" (that list is the
/// nest's own `segments::list_handler::segment_manager_for_kind` — since
/// 2026-08-18 the two coincide at all five kinds, `conv` the last to join
/// under the member-mint rule of `message-segment-store.md` § *Which kinds the
/// two planes serve*: no channel-enumeration door, explicit-list member-mint
/// authorization, home-nest coverage bound). Serving and adopting are still
/// different questions with different answers, and a consumer that needs both
/// — the custodian's nest leg is the first — wants the intersection. Keeping
/// the two lists apart is what stops a kind becoming adoptable by the accident
/// of a nest learning to serve it.
///
/// A kind joins this list when its record identity becomes its content hash —
/// every kind's ratified end state (`message-segment-store.md` § Record
/// identity per kind, ruled 2026-08-17; the transition is the user-approved
/// alpha reset, not a migration).
pub const ADOPTABLE_KINDS: &[&str] = &["post", "mail", "calendar", "card", "conv"];

/// Verify an offered `(dat, meta)` pair on behalf of `expect_actor`, filed
/// under a scope whose kind is `expect_kind` (`None` for a non-content scope,
/// which has no kind to bind).
///
/// Adoption is the one path where a replica takes a *bulk* container of bytes
/// from elsewhere, so it is also the one path where a single unchecked
/// assumption poisons many records at once. Seven checks, each answering a way
/// the pair could lie:
///
/// 1. The sidecar decodes, and its reader floor admits this binary.
/// 2. The sidecar names **this** actor — a segment belonging to someone else
///    is refused outright rather than filed under our scope.
/// 3. The sidecar names **the scope's own kind** — a source cannot relabel a
///    bulky kind the hydration policy dehydrates as one it always fetches and
///    make a dehydrating replica adopt (and be unable to reclaim) the whole
///    container.
/// 4. The `.dat` is a finalized CARv2 (an active segment has no index, so
///    `Reader::new` refuses it — exactly right: adopting a file still being
///    appended to would freeze a prefix and call it the whole segment).
/// 5. **Every** block's bytes hash to the CID the file files them under — the
///    same F9-shaped anti-poisoning predicate
///    [`crate::store::AccountStore::put_block`] applies per block, applied here
///    to the whole container.
/// 6. `record_order` holds no duplicate.
/// 7. `record_order` and the CARv2 index describe the **same set** — no record
///    the container cannot produce, and no block the sidecar does not declare
///    (a smuggled block would be served by `block_get` while appearing in no
///    index rebuild, which is precisely a block nothing accounts for).
///
/// The `.dat` is read through `dat` — the staged file on a backend with one
/// ([`crate::backend::SegmentStaging::dat_reader`]) — one record at a time, so
/// admitting a segment holds one record (at most
/// `fauna_carv2::v1::MAX_RECORD_LEN`) and the index, never the container. The
/// `.meta` is decoded whole: it is the index of the admission itself
/// (`record_order`), the same order of size as the sets this check builds.
pub fn admit<R: Read + Seek>(
    dat: R,
    meta: &[u8],
    expect_actor: &[u8; 32],
    expect_kind: Option<&str>,
) -> Result<SegmentAdmission> {
    let sidecar = SegmentSidecarView::decode(meta)?;
    if &sidecar.actor_id != expect_actor {
        bail!(
            "segment belongs to a different actor — refusing to adopt \
             (sidecar actor {}, this replica {})",
            hex_full(&sidecar.actor_id),
            hex_full(expect_actor)
        );
    }
    if let Some(kind) = expect_kind
        && sidecar.kind != kind
    {
        return Err(SegmentKindMismatch {
            sidecar_kind: sidecar.kind.clone(),
            scope_kind: kind.to_string(),
        }
        .into());
    }

    let mut reader = fauna_carv2::Reader::new(dat)
        .map_err(|e| anyhow::anyhow!("segment .dat is not a finalized CARv2 container: {e}"))?;

    // Keyed by CID: a segment's record count has no ceiling, so the length
    // lookup below must not be a scan per record.
    let mut in_file: BTreeMap<[u8; 36], u64> = BTreeMap::new();
    for block in reader.iter() {
        let (cid, bytes) = block.context("segment .dat: reading an indexed block")?;
        // The reader compares the on-disk CID to its index entry but never
        // re-hashes the body (its own docs say so). Adoption is exactly the
        // caller that must.
        if !cid.matches(&bytes) {
            bail!(
                "segment .dat: block {} does not hash to the CID it is filed under",
                hex_full(cid.as_bytes())
            );
        }
        in_file.insert(*cid.as_bytes(), bytes.len() as u64);
    }

    let mut declared: BTreeSet<[u8; 36]> = BTreeSet::new();
    for cid in &sidecar.record_order {
        if !declared.insert(*cid.as_bytes()) {
            bail!(
                "segment sidecar: record_order lists {} twice",
                hex_full(cid.as_bytes())
            );
        }
    }
    let absent = declared
        .iter()
        .filter(|c| !in_file.contains_key(*c))
        .count();
    let undeclared = in_file.keys().filter(|c| !declared.contains(*c)).count();
    if absent != 0 || undeclared != 0 {
        bail!(
            "segment sidecar and .dat disagree: {absent} record(s) declared but absent from the \
             container, {undeclared} block(s) present but undeclared"
        );
    }

    // Report in the sidecar's append order — the order an index rebuild walks.
    let blocks = sidecar
        .record_order
        .iter()
        .map(|cid| AdoptedBlock {
            cid: *cid,
            len: in_file[cid.as_bytes()],
        })
        .collect();

    Ok(SegmentAdmission { sidecar, blocks })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::io::Cursor;

    /// [`admit`] over an in-memory `.dat`.
    fn admit_bytes(
        dat: &[u8],
        meta: &[u8],
        expect_actor: &[u8; 32],
        expect_kind: Option<&str>,
    ) -> Result<SegmentAdmission> {
        admit(Cursor::new(dat), meta, expect_actor, expect_kind)
    }

    /// A hand-built sidecar so tests can produce shapes the real writer never
    /// would (a newer reader floor, a mismatched record_order). The
    /// writer-parity test in `store.rs` covers the shapes it *does* produce.
    #[derive(Serialize)]
    struct TestSidecar {
        format_version: u16,
        min_reader_format_version: u16,
        kind: String,
        #[serde(with = "serde_bytes")]
        actor_id: [u8; 32],
        segment_id: u32,
        bucket: String,
        created_at_secs: u64,
        record_order: Vec<ContentHash>,
        floor_metadata: Vec<serde_bytes::ByteBuf>,
    }

    fn sidecar_bytes(actor: [u8; 32], order: Vec<ContentHash>, min_reader: u16) -> Vec<u8> {
        let n = order.len();
        fauna_cbor::encode_canonical(&TestSidecar {
            format_version: 1,
            min_reader_format_version: min_reader,
            kind: "post".into(),
            actor_id: actor,
            segment_id: 7,
            bucket: "2026-08".into(),
            created_at_secs: 1_754_000_000,
            record_order: order,
            floor_metadata: vec![serde_bytes::ByteBuf::new(); n],
        })
        .unwrap()
    }

    /// A finalized CARv2 container over `bodies`, CIDs computed the way the
    /// segment store computes them (`of_dag_cbor`).
    fn dat_bytes(bodies: &[&[u8]]) -> Vec<u8> {
        let mut w = fauna_carv2::Writer::new(Cursor::new(Vec::new()), &[]).unwrap();
        for body in bodies {
            w.write_block(&ContentHash::of_dag_cbor(body), body)
                .unwrap();
        }
        w.finalize().unwrap().into_inner()
    }

    const ACTOR: [u8; 32] = [3u8; 32];

    #[test]
    fn a_well_formed_pair_is_admitted_in_append_order() {
        let bodies: &[&[u8]] = &[b"second-by-digest maybe", b"first appended", b"third"];
        let order: Vec<ContentHash> = bodies.iter().map(|b| ContentHash::of_dag_cbor(b)).collect();
        let admission = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes(ACTOR, order.clone(), 1),
            &ACTOR,
            None,
        )
        .expect("admitted");

        assert_eq!(admission.sidecar.kind, "post");
        assert_eq!(admission.sidecar.segment_id, 7);
        // Append order, not the CARv2 index's digest order — the whole reason
        // the sidecar carries record_order at all.
        let got: Vec<ContentHash> = admission.blocks.iter().map(|b| b.cid).collect();
        assert_eq!(got, order);
        let lens: Vec<u64> = admission.blocks.iter().map(|b| b.len).collect();
        assert_eq!(
            lens,
            bodies.iter().map(|b| b.len() as u64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn another_actors_segment_is_refused() {
        let bodies: &[&[u8]] = &[b"someone else's record"];
        let order = vec![ContentHash::of_dag_cbor(bodies[0])];
        let err = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes([9u8; 32], order, 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("different actor"), "got: {err}");
    }

    #[test]
    fn a_newer_reader_floor_is_refused_as_incompatible_not_corrupt() {
        let bodies: &[&[u8]] = &[b"record from a newer nest"];
        let order = vec![ContentHash::of_dag_cbor(bodies[0])];
        let err = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes(ACTOR, order, 2),
            &ACTOR,
            None,
        )
        .unwrap_err();
        let typed = err
            .downcast_ref::<SegmentIncompatible>()
            .expect("typed refusal, so a caller can say 'this app is too old'");
        assert_eq!(typed.min_reader, 2);
    }

    #[test]
    fn a_newer_additive_sidecar_is_tolerated() {
        // I2: a newer nest grows the sidecar additively; an older app must
        // still adopt. Modelled by a format_version ahead of ours whose reader
        // floor still admits us, carrying a field this reader never heard of.
        #[derive(Serialize)]
        struct GrownSidecar {
            format_version: u16,
            min_reader_format_version: u16,
            kind: String,
            #[serde(with = "serde_bytes")]
            actor_id: [u8; 32],
            segment_id: u32,
            bucket: String,
            created_at_secs: u64,
            record_order: Vec<ContentHash>,
            floor_metadata: Vec<serde_bytes::ByteBuf>,
            /// The field from the future.
            zz_compression_profile: String,
        }
        let bodies: &[&[u8]] = &[b"a record a newer nest wrote"];
        let order = vec![ContentHash::of_dag_cbor(bodies[0])];
        let meta = fauna_cbor::encode_canonical(&GrownSidecar {
            format_version: 9,
            min_reader_format_version: 1,
            kind: "mail".into(),
            actor_id: ACTOR,
            segment_id: 12,
            bucket: "2027-01".into(),
            created_at_secs: 1,
            record_order: order.clone(),
            floor_metadata: vec![serde_bytes::ByteBuf::new()],
            zz_compression_profile: "brotli".into(),
        })
        .unwrap();

        let admission = admit_bytes(&dat_bytes(bodies), &meta, &ACTOR, None).expect("tolerated");
        assert_eq!(admission.sidecar.format_version, 9);
        assert_eq!(admission.blocks.len(), 1);
    }

    #[test]
    fn a_block_that_does_not_hash_to_its_cid_poisons_nothing() {
        // Hand-write a container filing bytes under a CID they do not hash to.
        let mut w = fauna_carv2::Writer::new(Cursor::new(Vec::new()), &[]).unwrap();
        let lying_cid = ContentHash::of_dag_cbor(b"what the CID claims");
        w.write_block(&lying_cid, b"what the file actually holds")
            .unwrap();
        let dat = w.finalize().unwrap().into_inner();

        let err = admit_bytes(
            &dat,
            &sidecar_bytes(ACTOR, vec![lying_cid], 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not hash to the CID"),
            "got: {err}"
        );
    }

    #[test]
    fn a_smuggled_block_the_sidecar_does_not_declare_is_refused() {
        let bodies: &[&[u8]] = &[b"declared record", b"a block nobody declared"];
        // record_order names only the first.
        let order = vec![ContentHash::of_dag_cbor(bodies[0])];
        let err = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes(ACTOR, order, 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("present but undeclared"),
            "got: {err}"
        );
    }

    #[test]
    fn a_declared_record_the_container_lacks_is_refused() {
        let bodies: &[&[u8]] = &[b"the one record present"];
        let order = vec![
            ContentHash::of_dag_cbor(bodies[0]),
            ContentHash::of_dag_cbor(b"a record promised but not shipped"),
        ];
        let err = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes(ACTOR, order, 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("declared but absent"),
            "got: {err}"
        );
    }

    #[test]
    fn a_duplicated_record_order_entry_is_refused() {
        let bodies: &[&[u8]] = &[b"one record"];
        let c = ContentHash::of_dag_cbor(bodies[0]);
        let err = admit_bytes(
            &dat_bytes(bodies),
            &sidecar_bytes(ACTOR, vec![c, c], 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("twice"), "got: {err}");
    }

    #[test]
    fn an_unfinalized_container_is_refused() {
        // An **active** segment carries `index_offset = 0` (the CARv2 header's
        // "no index yet" encoding) — adopting one would freeze whatever prefix
        // happened to be flushed and call it the whole segment. Modelled by
        // clearing that field on a finalized file: bytes 43..51, i.e. the
        // 11-byte pragma plus the header's 32-byte offset of `index_offset`.
        let body: &[u8] = b"a record in a segment still being appended to";
        let mut unfinalized = dat_bytes(&[body]);
        unfinalized[43..51].fill(0);

        let err = admit_bytes(
            &unfinalized,
            &sidecar_bytes(ACTOR, vec![ContentHash::of_dag_cbor(body)], 1),
            &ACTOR,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("finalized CARv2"), "got: {err}");
    }
}
