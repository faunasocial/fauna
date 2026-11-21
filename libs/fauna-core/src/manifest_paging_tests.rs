//! Tier-1 pins for paged manifests (`file-sync.md` § Paged manifests).

use std::collections::HashMap;

use super::*;
use crate::chunk::{MANIFEST_READER_VERSION, MANIFEST_SEALED_READER, PagedRootError};
use crate::encoding::{canonical_decode, canonical_encode};

const ROOT: [u8; 32] = [7u8; 32];

fn h(tag: &str, i: usize) -> ContentHash {
    ContentHash::of_raw(format!("{tag}-{i}").as_bytes())
}

/// A plaintext-view manifest of `n` chunks; `sealed` gives it store keys
/// distinct from its plaintext hashes (a sealed-chunk upload's shape).
fn view(n: usize, sealed: bool) -> ChunkManifest {
    let sizes: Vec<u64> = (0..n).map(|i| 1000 + i as u64 % 7).collect();
    ChunkManifest {
        file_hash: ContentHash::of_raw(format!("file-{n}").as_bytes()),
        total_size: sizes.iter().sum(),
        chunk_hashes: (0..n).map(|i| h("plain", i)).collect(),
        chunk_sizes: sizes,
        stored_hashes: sealed.then(|| (0..n).map(|i| h("stored", i)).collect()),
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    }
}

/// Small thresholds: inline up to 16 chunks, pages of 4..=32 entries cut
/// on 3 low bits — the paged path without a 64 GB file.
fn small() -> PagingRule {
    PagingRule::with_thresholds(16, 4, 32, 3)
}

fn store(pages: &[Vec<u8>]) -> HashMap<ContentHash, Vec<u8>> {
    pages
        .iter()
        .map(|b| (ContentHash::of_raw(b), b.clone()))
        .collect()
}

fn walk_all(walk: &PageWalk, blobs: &HashMap<ContentHash, Vec<u8>>) -> Result<Vec<WalkedPage>> {
    walk.pages(|hash| {
        blobs
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow!("no blob {}", hex::encode(hash.digest())))
    })
    .collect()
}

#[test]
fn production_constants_are_the_ratified_numbers() {
    assert_eq!(MANIFEST_INLINE_MAX_CHUNKS, 32_768);
    assert_eq!(MANIFEST_PAGE_MIN, 4_096);
    assert_eq!(MANIFEST_PAGE_MAX, 65_536);
    assert_eq!(MANIFEST_PAGE_CUT_BITS, 14);
    assert_eq!(MANIFEST_READER_VERSION, 3);
    assert_eq!(MANIFEST_SEALED_READER, 2);
    assert!(!PagingRule::STANDARD.is_paged(MANIFEST_INLINE_MAX_CHUNKS));
    assert!(PagingRule::STANDARD.is_paged(MANIFEST_INLINE_MAX_CHUNKS + 1));
}

/// Golden bytes: an inline-size manifest — plaintext and sealed — encodes to
/// the bytes the pre-paging field set ([`PrePagingManifest`]) encodes, i.e.
/// exactly as it did before the `pages` field existed, and `into_wire_parts`
/// writes that encoding and no pages.
#[test]
fn an_inline_manifest_encodes_byte_identically() {
    let plain = ChunkManifest {
        file_hash: ContentHash::from_digest_raw([0xAA; 32]),
        total_size: 3,
        chunk_hashes: vec![ContentHash::from_digest_raw([1; 32])],
        chunk_sizes: vec![3],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let bytes = canonical_encode(&plain).unwrap();
    assert_eq!(
        bytes,
        canonical_encode(&PrePagingManifest::from(&plain)).unwrap()
    );
    let (root, pages) = plain.clone().into_wire_parts(None).unwrap();
    assert_eq!(root, bytes);
    assert!(pages.is_empty());

    let sealed_view = ChunkManifest {
        stored_hashes: Some(vec![ContentHash::from_digest_raw([2; 32])]),
        ..plain
    };
    let wire = sealed_view.wire_form(Some(&ROOT)).unwrap();
    assert_eq!(wire.min_reader, Some(MANIFEST_SEALED_READER));
    let bytes = canonical_encode(&wire).unwrap();
    assert_eq!(
        bytes,
        canonical_encode(&PrePagingManifest::from(&wire)).unwrap()
    );
    let (root, pages) = sealed_view.into_wire_parts(Some(&ROOT)).unwrap();
    assert_eq!(root, bytes);
    assert!(pages.is_empty());
    assert!(!bytes.windows(5).any(|w| w == b"pages"));
}

/// The `ChunkManifest` field set as it stood before `pages` existed, verbatim
/// (serde attributes included) — the encoder every manifest at rest was
/// written by.
#[derive(Serialize)]
struct PrePagingManifest {
    file_hash: ContentHash,
    total_size: u64,
    chunk_hashes: Vec<ContentHash>,
    chunk_sizes: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stored_hashes: Option<Vec<ContentHash>>,
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    sealed_hashes: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_reader: Option<u32>,
}

impl From<&ChunkManifest> for PrePagingManifest {
    fn from(m: &ChunkManifest) -> Self {
        assert!(m.pages.is_none());
        Self {
            file_hash: m.file_hash,
            total_size: m.total_size,
            chunk_hashes: m.chunk_hashes.clone(),
            chunk_sizes: m.chunk_sizes.clone(),
            stored_hashes: m.stored_hashes.clone(),
            sealed_hashes: m.sealed_hashes.clone(),
            min_reader: m.min_reader,
        }
    }
}

#[test]
fn every_single_manifest_encodes_as_the_pre_paging_encoder_did() {
    for n in [0, 1, 10, 200] {
        for sealed in [false, true] {
            let m = view(n, sealed);
            let seal = sealed.then_some(&ROOT);
            let wire = m.wire_form(seal).unwrap();
            let old = canonical_encode(&PrePagingManifest::from(&wire)).unwrap();
            assert_eq!(
                canonical_encode(&wire).unwrap(),
                old,
                "n={n} sealed={sealed}"
            );
            // At the production bound none of these is paged.
            let (bytes, pages) = m.into_wire_parts(seal).unwrap();
            assert_eq!(bytes, old);
            assert!(pages.is_empty());
        }
    }
}

#[test]
fn the_paging_rule_is_deterministic_bounded_and_covers_the_file() {
    let keys: Vec<ContentHash> = (0..2_000).map(|i| h("k", i)).collect();
    let rule = small();
    let ranges = rule.page_ranges(&keys);
    assert_eq!(ranges, rule.page_ranges(&keys), "deterministic");
    assert_eq!(ranges.first().unwrap().start, 0);
    assert_eq!(ranges.last().unwrap().end, keys.len());
    for w in ranges.windows(2) {
        assert_eq!(w[0].end, w[1].start, "contiguous");
    }
    for (i, r) in ranges.iter().enumerate() {
        assert!(r.len() <= 32, "page over the maximum");
        if i + 1 < ranges.len() {
            assert!(r.len() >= 4, "an inner page under the minimum");
            let last = &keys[r.end - 1];
            assert!(
                r.len() == 32 || rule.is_cut(last),
                "a page ends on a cut or the maximum"
            );
        }
    }
    // Every cut is taken once the minimum is reached.
    assert!(ranges.len() > 2_000 / 32);
}

#[test]
fn an_inserted_entry_changes_only_the_pages_around_it() {
    let keys: Vec<ContentHash> = (0..2_000).map(|i| h("k", i)).collect();
    let mut edited = keys.clone();
    edited.insert(1_000, h("inserted", 0));
    let rule = small();
    let pages = |ks: &[ContentHash]| -> Vec<Vec<ContentHash>> {
        rule.page_ranges(ks)
            .into_iter()
            .map(|r| ks[r].to_vec())
            .collect()
    };
    let before = pages(&keys);
    let after = pages(&edited);
    let changed_before = before.iter().filter(|p| !after.contains(p)).count();
    let changed_after = after.iter().filter(|p| !before.contains(p)).count();
    assert!(changed_before <= 3, "{changed_before} old pages changed");
    assert!(changed_after <= 3, "{changed_after} new pages changed");
    // The pages wholly before the insertion are untouched.
    let head = before
        .iter()
        .take_while(|p| p.iter().all(|k| keys[..1_000].contains(k)));
    for p in head {
        assert!(after.contains(p));
    }
}

#[test]
fn a_paged_manifest_is_a_small_root_and_pages() {
    for sealed in [false, true] {
        let m = view(200, sealed);
        let seal = sealed.then_some(&ROOT);
        let (root_bytes, pages) = m.clone().into_wire_parts_with(&small(), seal).unwrap();
        assert!(pages.len() > 1);
        let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
        assert!(root.is_paged());
        assert_eq!(root.min_reader, Some(3));
        assert!(root.chunk_hashes.is_empty() && root.chunk_sizes.is_empty());
        assert!(root.stored_hashes.is_none());
        assert_eq!(root.is_sealed(), sealed);
        assert_eq!(
            root.file_hash == m.file_hash,
            !sealed,
            "a sealed root blanks file_hash"
        );
        root.check_hash_shape().unwrap();
        root.check_sizes().unwrap();
        let refs = root.pages.as_ref().unwrap();
        assert_eq!(refs.len(), pages.len());
        for (r, b) in refs.iter().zip(&pages) {
            assert_eq!(r.hash, ContentHash::of_raw(b));
            let page: ManifestPage = canonical_decode(b).unwrap();
            page.check_shape().unwrap();
            assert_eq!(page.sealed_hashes.is_some(), sealed);
            assert_eq!(page.chunk_hashes.is_empty(), sealed);
        }
        // Convergent: the same view under the same root writes the same bytes.
        let again = m.into_wire_parts_with(&small(), seal).unwrap();
        assert_eq!(again, (root_bytes, pages));
    }
}

#[test]
fn the_walk_reassembles_a_paged_manifest() {
    for sealed in [false, true] {
        let m = view(200, sealed);
        let seal = sealed.then_some(&ROOT);
        let (root_bytes, pages) = m.clone().into_wire_parts_with(&small(), seal).unwrap();
        let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
        let blobs = store(&pages);

        // With the root secret (or plaintext): every hash, key and size, in order.
        let walk = PageWalk::open(root.clone(), seal).unwrap();
        assert_eq!(walk.file_hash(), Some(m.file_hash));
        assert_eq!(walk.page_count(), pages.len());
        let walked = walk_all(&walk, &blobs).unwrap();
        let keys: Vec<_> = walked.iter().flat_map(|p| p.store_keys.clone()).collect();
        let hashes: Vec<_> = walked
            .iter()
            .flat_map(|p| p.chunk_hashes.clone().unwrap())
            .collect();
        let sizes: Vec<_> = walked.iter().flat_map(|p| p.chunk_sizes.clone()).collect();
        assert_eq!(keys, m.store_keys().unwrap());
        assert_eq!(hashes, m.chunk_hashes);
        assert_eq!(sizes, m.chunk_sizes);
        let mut chunk = 0;
        let mut byte = 0;
        for p in &walked {
            assert_eq!((p.first_chunk, p.byte_offset), (chunk, byte));
            chunk += p.store_keys.len() as u64;
            byte += p.chunk_sizes.iter().sum::<u64>();
        }

        // Without the secret (a nest): store keys only.
        let walk = PageWalk::open(root, None).unwrap();
        let walked = walk_all(&walk, &blobs).unwrap();
        let keys: Vec<_> = walked.iter().flat_map(|p| p.store_keys.clone()).collect();
        assert_eq!(keys, m.store_keys().unwrap());
        assert_eq!(walked[0].chunk_hashes.is_none(), sealed);
        assert_eq!(walk.file_hash().is_none(), sealed);
    }
}

#[test]
fn the_walk_reads_a_single_manifest_as_one_page() {
    for sealed in [false, true] {
        let m = view(10, sealed);
        let seal = sealed.then_some(&ROOT);
        let (bytes, pages) = m.clone().into_wire_parts_with(&small(), seal).unwrap();
        assert!(pages.is_empty());
        let fetched: ChunkManifest = canonical_decode(&bytes).unwrap();
        let walk = PageWalk::open(fetched, seal).unwrap();
        assert!(!walk.is_paged());
        assert_eq!(walk.page_to_fetch(0), None);
        let walked = walk_all(&walk, &HashMap::new()).unwrap();
        assert_eq!(walked.len(), 1);
        assert_eq!(walked[0].store_keys, m.store_keys().unwrap());
        assert_eq!(walked[0].chunk_hashes.as_ref(), Some(&m.chunk_hashes));
        assert_eq!(walk.file_hash(), Some(m.file_hash));
        assert_eq!(walk.pages_for_range(0, 5), 0..1);
    }
}

#[test]
fn a_ranged_read_names_only_the_pages_it_covers() {
    let m = view(200, false);
    let (root_bytes, _) = m.into_wire_parts_with(&small(), None).unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    let refs = root.pages.clone().unwrap();
    let walk = PageWalk::open(root, None).unwrap();
    let first = refs[0].byte_len;
    assert_eq!(walk.pages_for_range(0, 1), 0..1);
    assert_eq!(walk.pages_for_range(first - 1, 1), 0..1);
    assert_eq!(walk.pages_for_range(first - 1, 2), 0..2);
    assert_eq!(walk.pages_for_range(first, 1), 1..2);
    assert_eq!(walk.pages_for_range(0, walk.total_size()), 0..refs.len());
    assert_eq!(
        walk.pages_for_range(walk.total_size() - 1, 100),
        refs.len() - 1..refs.len()
    );
    assert_eq!(walk.pages_for_range(walk.total_size(), 1), 0..0);
    assert_eq!(walk.pages_for_range(3, 0), 0..0);
}

#[test]
fn the_walk_refuses_a_page_that_is_not_the_one_its_root_names() {
    let m = view(200, true);
    let (root_bytes, pages) = m.into_wire_parts_with(&small(), Some(&ROOT)).unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    let walk = PageWalk::open(root.clone(), Some(&ROOT)).unwrap();

    // Wrong bytes for the address (a tampered or swapped page).
    let err = walk.open_page(0, Some(&pages[1])).unwrap_err();
    assert!(err.to_string().contains("hashes to"), "{err}");

    // A root whose page entry count lies (its byte length still sums).
    let mut lying = root.clone();
    lying.pages.as_mut().unwrap()[0].chunk_count += 1;
    let walk = PageWalk::open(lying, None).unwrap();
    let err = walk.open_page(0, Some(&pages[0])).unwrap_err();
    assert!(err.to_string().contains("entries"), "{err}");

    // A tampered page list fails the root seal: reordered pages.
    let mut reordered = root.clone();
    reordered.pages.as_mut().unwrap().swap(0, 1);
    assert!(PageWalk::open(reordered, Some(&ROOT)).is_err());

    // A page whose sealed hashes were sealed under another root fails closed.
    let walk = PageWalk::open(root, None).unwrap();
    walk.open_page(0, Some(&pages[0])).unwrap();
    let other = PageWalk {
        root_secret: Some([9u8; 32]),
        ..walk
    };
    assert!(other.open_page(0, Some(&pages[0])).is_err());
}

#[test]
fn a_page_sealed_unlike_its_root_is_refused() {
    let (_, plain_pages) = view(200, false)
        .into_wire_parts_with(&small(), None)
        .unwrap();
    let (root_bytes, _) = view(200, true)
        .into_wire_parts_with(&small(), Some(&ROOT))
        .unwrap();
    let mut root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    // Point the sealed root at a plaintext page of the right count and size.
    let page: ManifestPage = canonical_decode(&plain_pages[0]).unwrap();
    root.pages.as_mut().unwrap()[0] = ManifestPageRef {
        hash: ContentHash::of_raw(&plain_pages[0]),
        chunk_count: page.entry_count() as u32,
        byte_len: page.byte_len(),
    };
    root.total_size = root
        .pages
        .as_ref()
        .unwrap()
        .iter()
        .map(|p| p.byte_len)
        .sum();
    let walk = PageWalk::open(root, None).unwrap();
    let err = walk.open_page(0, Some(&plain_pages[0])).unwrap_err();
    assert!(err.to_string().contains("not sealed the way"), "{err}");
}

#[test]
fn a_root_naming_a_root_is_refused() {
    let (inner, _) = view(200, false)
        .into_wire_parts_with(&small(), None)
        .unwrap();
    let inner_root: ChunkManifest = canonical_decode(&inner).unwrap();
    let outer = ChunkManifest {
        file_hash: inner_root.file_hash,
        total_size: 10,
        chunk_hashes: vec![],
        chunk_sizes: vec![],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: Some(3),
        pages: Some(vec![ManifestPageRef {
            hash: ContentHash::of_raw(&inner),
            chunk_count: 1,
            byte_len: 10,
        }]),
    };
    let walk = PageWalk::open(outer, None).unwrap();
    let err = walk.open_page(0, Some(&inner)).unwrap_err();
    assert!(
        err.to_string().contains("a root never names a root"),
        "{err}"
    );

    // A single manifest named as a page is refused the same way.
    let (single, _) = view(3, false).into_wire_parts_with(&small(), None).unwrap();
    let outer = ChunkManifest {
        pages: Some(vec![ManifestPageRef {
            hash: ContentHash::of_raw(&single),
            chunk_count: 1,
            byte_len: 10,
        }]),
        ..walk.manifest.clone()
    };
    let walk = PageWalk::open(outer, None).unwrap();
    assert!(walk.open_page(0, Some(&single)).is_err());
}

#[test]
fn a_malformed_root_is_refused_before_any_page() {
    let (root_bytes, _) = view(200, true)
        .into_wire_parts_with(&small(), Some(&ROOT))
        .unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    let refused = |m: ChunkManifest, why: &str| {
        assert!(m.check_hash_shape().is_err(), "{why}: shape");
        assert!(PageWalk::open(m, Some(&ROOT)).is_err(), "{why}: walk");
    };

    let mut m = root.clone();
    m.chunk_sizes = vec![5];
    refused(m, "a root with chunk sizes");
    let mut m = root.clone();
    m.chunk_hashes = vec![h("x", 0)];
    refused(m, "a root with chunk hashes");
    let mut m = root.clone();
    m.stored_hashes = Some(vec![h("x", 0)]);
    refused(m, "a root with store keys");
    let mut m = root.clone();
    m.pages = Some(vec![]);
    refused(m, "a root of no pages");
    let mut m = root.clone();
    m.total_size += 1;
    refused(m, "page lengths that disagree with total_size");
    let mut m = root.clone();
    m.pages.as_mut().unwrap()[0].chunk_count = 0;
    refused(m, "an empty page");
    let mut m = root.clone();
    m.pages.as_mut().unwrap()[0].chunk_count = MANIFEST_PAGE_MAX as u32 + 1;
    refused(m, "a page over the entry bound");
    let mut m = root.clone();
    let max = u64::from(crate::chunker::MAX_CHUNK);
    let extra = 2 * max;
    m.pages.as_mut().unwrap()[0].chunk_count = 1;
    m.pages.as_mut().unwrap()[0].byte_len += extra;
    m.total_size += extra;
    refused(m, "more bytes than the page's chunks hold");
    let mut m = root.clone();
    m.min_reader = Some(2);
    refused(m, "a root that does not stamp min_reader 3");
    let mut m = root.clone();
    m.file_hash = h("leak", 0);
    refused(m, "a sealed root naming its file hash");

    root.check_hash_shape().unwrap();
}

#[test]
fn a_paged_root_is_refused_by_a_version_two_reader() {
    let (root_bytes, _) = view(200, false)
        .into_wire_parts_with(&small(), None)
        .unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    root.check_min_reader().unwrap();
    let err = root.check_min_reader_for(2).unwrap_err();
    assert!(err.to_string().contains("update this client"), "{err}");
    // A sealed single manifest stays readable by a version-two reader.
    let single = view(3, true).wire_form(Some(&ROOT)).unwrap();
    single.check_min_reader_for(2).unwrap();
}

#[test]
fn a_root_has_no_store_keys_of_its_own() {
    let (root_bytes, _) = view(200, true)
        .into_wire_parts_with(&small(), Some(&ROOT))
        .unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    assert_eq!(root.store_keys(), Err(PagedRootError));
    assert_eq!(root.store_key(0), Err(PagedRootError));
    assert!(root.clone().unseal_hashes(&ROOT).is_err());
    assert!(root.seal_hashes(&ROOT).is_err());
}

#[test]
fn the_writer_refuses_a_root_or_a_sealed_form() {
    let (root_bytes, _) = view(200, false)
        .into_wire_parts_with(&small(), None)
        .unwrap();
    let root: ChunkManifest = canonical_decode(&root_bytes).unwrap();
    assert!(root.into_wire_parts_with(&small(), None).is_err());
    let sealed = view(3, true).wire_form(Some(&ROOT)).unwrap();
    assert!(sealed.into_wire_parts_with(&small(), Some(&ROOT)).is_err());
    assert!(
        view(200, true)
            .into_wire_parts_with(&small(), None)
            .is_err()
    );
    assert!(
        view(200, false)
            .into_wire_parts_with(&small(), Some(&ROOT))
            .is_err()
    );
}

#[test]
fn the_walk_flattens_either_shape_to_the_plaintext_view() {
    for (n, sealed) in [(200, false), (200, true), (10, false), (10, true)] {
        let m = view(n, sealed);
        let seal = sealed.then_some(&ROOT);
        let (bytes, pages) = m.clone().into_wire_parts_with(&small(), seal).unwrap();
        assert_eq!(pages.is_empty(), n <= 16);
        let fetched: ChunkManifest = canonical_decode(&bytes).unwrap();
        assert_eq!(
            fetched.entry_count(),
            n,
            "the entry count off the wire, either shape"
        );
        let blobs = store(&pages);
        let fetch = |hash: &ContentHash| blobs.get(hash).cloned().ok_or_else(|| anyhow!("no page"));
        let flat = PageWalk::open(fetched.clone(), seal)
            .unwrap()
            .flatten(fetch)
            .unwrap();
        assert_eq!(flat.file_hash, m.file_hash);
        assert_eq!(flat.total_size, m.total_size);
        assert_eq!(flat.chunk_hashes, m.chunk_hashes);
        assert_eq!(flat.chunk_sizes, m.chunk_sizes);
        assert_eq!(flat.store_keys().unwrap(), m.store_keys().unwrap());
        assert!(!flat.is_paged() && flat.sealed_hashes.is_none());

        // A sealed manifest walked without its secret has no plaintext view.
        if sealed {
            let keyless = PageWalk::open(fetched, None).unwrap();
            assert!(keyless.flatten(fetch).is_err());
        }
        // A page the fetch cannot supply refuses the whole view.
        if !pages.is_empty() {
            let walk = PageWalk::open(canonical_decode(&bytes).unwrap(), seal).unwrap();
            assert!(walk.flatten(|_| Err(anyhow!("gone"))).is_err());
        }
    }
}
