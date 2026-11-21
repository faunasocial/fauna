//! Paged manifests — a large file's manifest spans blobs
//! (`docs/goal/behavior/file-sync.md` § Paged manifests; the seal:
//! `docs/goal/architecture/mls-group-key-material.md` § M2 *Sealed manifest
//! hashes* → *Paged manifests*).
//!
//! A file of at most [`MANIFEST_INLINE_MAX_CHUNKS`] chunks keeps its single
//! [`ChunkManifest`], byte for byte. A larger one is written as a **root** — a
//! `ChunkManifest` with empty chunk lists, [`ChunkManifest::pages`] set and
//! `min_reader = 3` — and **pages** ([`ManifestPage`]), each its own
//! content-addressed blob holding one contiguous run of the file's chunks. Two
//! levels, never deeper.
//!
//! Three pieces live here, one each:
//! - the paging rule ([`PagingRule`]) — page boundaries from the ordered store
//!   keys alone, so every device sealing a file under one root derives the
//!   same pages and the same root hash;
//! - the writer ([`ChunkManifest::into_wire_parts`]) — the encoded root (or
//!   the single manifest) and its encoded pages;
//! - the walk ([`PageWalk`]) — the one reader of either shape: store keys
//!   (and, given the root secret, plaintext hashes) page by page, every page
//!   verified against the hash, entry count and byte length its root names.

use std::ops::Range;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::chunk::{ChunkManifest, MANIFEST_PAGED_READER, blank_file_hash};
use crate::data::ContentHash;

/// The most chunks a file's manifest holds inline (about 64 GB at the 2 MB
/// average chunk). At or under it the single `ChunkManifest` is written
/// unchanged; above it the manifest is paged.
pub const MANIFEST_INLINE_MAX_CHUNKS: usize = 32_768;

/// A page does not end on a content cut before it holds this many entries.
pub const MANIFEST_PAGE_MIN: usize = 4_096;

/// A page is forced to end at this many entries — the bound every reader and
/// the nest's door hold a page to (under 6 MB sealed).
pub const MANIFEST_PAGE_MAX: usize = 65_536;

/// A page ends after an entry whose store key has this many low bits zero
/// (once it holds [`MANIFEST_PAGE_MIN`] entries): a mean page of about 16,000
/// entries. The bits are read from the little-endian `u64` of the store key's
/// first eight digest bytes.
pub const MANIFEST_PAGE_CUT_BITS: u32 = 14;

/// One page as its root names it: the page blob's content address, how many
/// chunk entries it holds, and how many file bytes those chunks cover (so a
/// ranged read fetches only the pages its range touches). Plaintext on every
/// root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPageRef {
    /// BLAKE3 of the page's canonical encoding — its store key.
    pub hash: ContentHash,
    /// The page's entry count.
    pub chunk_count: u32,
    /// The sum of the page's chunk sizes.
    pub byte_len: u64,
}

/// One page of a paged manifest: a contiguous run of the file's chunk
/// entries, in the fields a single [`ChunkManifest`] holds them. A page of a
/// sealed-chunk upload carries `stored_hashes` and `sealed_hashes` (its
/// `chunk_hashes` sealed under `"fauna.manifest.v1"`, salted by its own
/// `stored_hashes`, with a blank `file_hash`) and an empty `chunk_hashes`; a
/// plaintext page carries `chunk_hashes` and neither of the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPage {
    /// The plaintext chunk hashes — empty on a sealed page.
    pub chunk_hashes: Vec<ContentHash>,
    /// Size of each chunk in bytes, parallel to the entries.
    pub chunk_sizes: Vec<u64>,
    /// The chunks' store keys on a sealed-chunk page; `None` on a plaintext
    /// page, whose store keys are its `chunk_hashes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_hashes: Option<Vec<ContentHash>>,
    /// The sealed `{blank file_hash, chunk_hashes}` of a sealed-chunk page.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub sealed_hashes: Option<Vec<u8>>,
}

impl ManifestPage {
    /// How many chunk entries the page holds.
    pub fn entry_count(&self) -> usize {
        self.stored_hashes
            .as_ref()
            .map_or(self.chunk_hashes.len(), Vec::len)
    }

    /// The page's store keys, in order.
    pub fn store_keys(&self) -> &[ContentHash] {
        self.stored_hashes.as_deref().unwrap_or(&self.chunk_hashes)
    }

    /// The page twin of [`ChunkManifest::check_hash_shape`] +
    /// [`ChunkManifest::check_sizes`]: between one and [`MANIFEST_PAGE_MAX`]
    /// entries, one size per entry and none above the chunk maximum, and the
    /// seal's pairing — a sealed page carries its store keys and sealed blob
    /// and no plaintext hash, a plaintext page neither.
    pub fn check_shape(&self) -> Result<()> {
        match (&self.stored_hashes, &self.sealed_hashes) {
            (None, None) => {}
            (Some(_), Some(_)) => {
                if !self.chunk_hashes.is_empty() {
                    bail!("sealed manifest page carries plaintext hashes beside sealed_hashes");
                }
            }
            (None, Some(_)) => bail!("sealed manifest page without stored_hashes"),
            (Some(_), None) => {
                bail!("sealed-chunk manifest page carries plaintext hashes and no sealed_hashes")
            }
        }
        let n = self.entry_count();
        if n == 0 || n > MANIFEST_PAGE_MAX {
            bail!("manifest page of {n} entries (1..={MANIFEST_PAGE_MAX} allowed)");
        }
        if self.chunk_sizes.len() != n {
            bail!(
                "manifest page lists {n} chunks but {} chunk sizes",
                self.chunk_sizes.len()
            );
        }
        let max = u64::from(crate::chunker::MAX_CHUNK);
        if let Some(size) = self.chunk_sizes.iter().find(|&&s| s > max) {
            bail!("manifest page chunk size {size} exceeds the chunk maximum");
        }
        Ok(())
    }

    /// Decode a page blob's plaintext bytes and check it is a page — the one
    /// check the nest's page door and the walk share: a canonical
    /// [`ManifestPage`], not a manifest (a body carrying `pages` or
    /// `file_hash` is a single manifest or a root, and manifests are two
    /// levels, never deeper), within [`Self::check_shape`]'s bounds.
    pub fn decode_checked(bytes: &[u8]) -> Result<Self> {
        let probe: RootProbe =
            crate::encoding::canonical_decode(bytes).context("decoding manifest page")?;
        if probe.pages.is_some() || probe.file_hash.is_some() {
            bail!("a manifest page body is a manifest, not a page — a root never names a root");
        }
        let page: Self =
            crate::encoding::canonical_decode(bytes).context("decoding manifest page")?;
        page.check_shape()?;
        Ok(page)
    }

    /// The sum of the page's chunk sizes (each bounded by the chunk maximum
    /// and the count by [`MANIFEST_PAGE_MAX`], so it cannot overflow once
    /// [`Self::check_shape`] passed).
    pub fn byte_len(&self) -> u64 {
        self.chunk_sizes.iter().sum()
    }
}

/// The page boundary rule: four hard-coded numbers in production
/// ([`Self::STANDARD`]); tests reach the paged path through
/// [`Self::with_thresholds`] instead of building a 64 GB file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagingRule {
    inline_max: usize,
    page_min: usize,
    page_max: usize,
    cut_mask: u64,
}

impl PagingRule {
    /// The rule every production writer uses.
    pub const STANDARD: Self = Self {
        inline_max: MANIFEST_INLINE_MAX_CHUNKS,
        page_min: MANIFEST_PAGE_MIN,
        page_max: MANIFEST_PAGE_MAX,
        cut_mask: (1u64 << MANIFEST_PAGE_CUT_BITS) - 1,
    };

    /// A rule with small thresholds, for tests only (`test-util`, or a debug
    /// build — the e2e agent's compile-gated `FAUNA_E2E_PAGING_RULE` seam): the
    /// inline bound, the page minimum and maximum, and the cut's low-bit count.
    #[cfg(any(test, debug_assertions, feature = "test-util"))]
    pub fn with_thresholds(
        inline_max: usize,
        page_min: usize,
        page_max: usize,
        cut_bits: u32,
    ) -> Self {
        assert!(page_min >= 1 && page_min <= page_max && page_max <= MANIFEST_PAGE_MAX);
        assert!(cut_bits < 64);
        Self {
            inline_max,
            page_min,
            page_max,
            cut_mask: (1u64 << cut_bits) - 1,
        }
    }

    /// Whether a file of `chunks` chunks is written paged.
    pub fn is_paged(&self, chunks: usize) -> bool {
        chunks > self.inline_max
    }

    fn is_cut(&self, key: &ContentHash) -> bool {
        let d = key.digest();
        let mut low = [0u8; 8];
        low.copy_from_slice(&d[..8]);
        u64::from_le_bytes(low) & self.cut_mask == 0
    }

    /// The page boundaries over a file's ordered store keys: a page ends after
    /// the entry whose key has the cut's low bits zero once it holds at least
    /// the page minimum, is forced to end at the page maximum, and the last
    /// page ends with the file. Reads nothing but the keys, so it is
    /// deterministic, and an inserted entry moves only the boundaries around
    /// it (the next content cut re-aligns the rest).
    pub fn page_ranges(&self, store_keys: &[ContentHash]) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0;
        for (i, key) in store_keys.iter().enumerate() {
            let len = i + 1 - start;
            if len >= self.page_max || (len >= self.page_min && self.is_cut(key)) {
                ranges.push(start..i + 1);
                start = i + 1;
            }
        }
        if start < store_keys.len() {
            ranges.push(start..store_keys.len());
        }
        ranges
    }
}

impl ChunkManifest {
    /// The file's chunk count, read off either shape without a page: a root's
    /// is the sum of the entry counts its page list names, a single
    /// manifest's its own list's length.
    pub fn entry_count(&self) -> usize {
        match &self.pages {
            Some(pages) => pages.iter().map(|p| p.chunk_count as usize).sum(),
            None => self.chunk_sizes.len(),
        }
    }

    /// The bytes a writer uploads for this (plaintext-view) manifest under
    /// [`PagingRule::STANDARD`]: the encoded single manifest and no pages at
    /// or under the inline bound — exactly [`ChunkManifest::wire_form`]'s
    /// encoding — else the encoded root and its encoded pages, in file order.
    /// `seal_root` is the root that sealed the chunks (`None` for a plaintext
    /// manifest), as for `wire_form`. Pages go up before the root.
    pub fn into_wire_parts(self, seal_root: Option<&[u8; 32]>) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
        self.into_wire_parts_with(&PagingRule::STANDARD, seal_root)
    }

    /// [`Self::into_wire_parts`] under an explicit rule (tests' small one).
    pub fn into_wire_parts_with(
        self,
        rule: &PagingRule,
        seal_root: Option<&[u8; 32]>,
    ) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
        if self.is_paged() || self.is_sealed() {
            bail!("into_wire_parts takes a manifest's plaintext view, not a root or a sealed form");
        }
        self.check_sizes()?;
        let n = self.chunk_hashes.len();
        if let Some(stored) = &self.stored_hashes
            && stored.len() != n
        {
            bail!(
                "manifest lists {} store keys for {n} chunk hashes",
                stored.len()
            );
        }
        if !rule.is_paged(n) {
            let wire = self.wire_form(seal_root)?;
            let bytes = crate::encoding::canonical_encode(&wire).context("encoding manifest")?;
            return Ok((bytes, Vec::new()));
        }
        let sealed = match (seal_root, &self.stored_hashes) {
            (Some(root), Some(_)) => Some(root),
            (None, None) => None,
            (Some(_), None) => bail!("a sealing root for a plaintext manifest"),
            (None, Some(_)) => bail!("a sealed-chunk manifest without its root"),
        };
        let keys = self.store_keys()?;
        let mut refs = Vec::new();
        let mut page_bytes = Vec::new();
        for range in rule.page_ranges(&keys) {
            let hashes = &self.chunk_hashes[range.clone()];
            let sizes = self.chunk_sizes[range.clone()].to_vec();
            let page = match sealed {
                Some(root) => {
                    let stored = &keys[range.clone()];
                    let seal = crate::manifest_crypto::seal_manifest_hashes(
                        root,
                        &blank_file_hash(),
                        hashes,
                        stored,
                    )?;
                    ManifestPage {
                        chunk_hashes: Vec::new(),
                        chunk_sizes: sizes,
                        stored_hashes: Some(stored.to_vec()),
                        sealed_hashes: Some(seal),
                    }
                }
                None => ManifestPage {
                    chunk_hashes: hashes.to_vec(),
                    chunk_sizes: sizes,
                    stored_hashes: None,
                    sealed_hashes: None,
                },
            };
            let bytes = crate::encoding::canonical_encode(&page).context("encoding page")?;
            refs.push(ManifestPageRef {
                hash: ContentHash::of_raw(&bytes),
                chunk_count: u32::try_from(range.len()).expect("a page is at most 65,536 entries"),
                byte_len: page.byte_len(),
            });
            page_bytes.push(bytes);
        }
        let (file_hash, sealed_hashes) = match sealed {
            Some(root) => {
                let page_hashes: Vec<ContentHash> = refs.iter().map(|r| r.hash).collect();
                let seal = crate::manifest_crypto::seal_root_file_hash(
                    root,
                    &self.file_hash,
                    &page_hashes,
                )?;
                (blank_file_hash(), Some(seal))
            }
            None => (self.file_hash, None),
        };
        let root = ChunkManifest {
            file_hash,
            total_size: self.total_size,
            chunk_hashes: Vec::new(),
            chunk_sizes: Vec::new(),
            stored_hashes: None,
            sealed_hashes,
            min_reader: Some(MANIFEST_PAGED_READER),
            pages: Some(refs),
        };
        root.check_root_shape()?;
        let bytes = crate::encoding::canonical_encode(&root).context("encoding manifest root")?;
        Ok((bytes, page_bytes))
    }
}

/// One page as the walk yields it: where it sits in the file, its store keys
/// and sizes, and — when the walk holds the root secret, or the manifest is
/// plaintext — its plaintext chunk hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedPage {
    /// The page's position in the root (0 for a single manifest).
    pub index: usize,
    /// The file-wide index of the page's first chunk.
    pub first_chunk: u64,
    /// The file offset of the page's first byte.
    pub byte_offset: u64,
    /// The page's store keys, in order.
    pub store_keys: Vec<ContentHash>,
    /// The page's plaintext chunk hashes; `None` for a sealed page walked
    /// without the root secret (a nest walks store keys only).
    pub chunk_hashes: Option<Vec<ContentHash>>,
    /// The page's chunk sizes, parallel to `store_keys`.
    pub chunk_sizes: Vec<u64>,
}

/// The one manifest walk (`file-sync.md` § Paged manifests → *One walk, used
/// by every reader*): opened over a fetched manifest of either shape, it
/// yields the file's chunk entries page by page — a single manifest as one
/// page, a root as its pages, each fetched page verified against the hash,
/// entry count and byte length the root names before anything in it is used.
///
/// Fetching is the caller's: a synchronous reader hands [`Self::pages`] a
/// `fetch(hash) -> bytes` closure; an asynchronous one fetches
/// [`Self::page_to_fetch`] itself and opens the bytes with
/// [`Self::open_page`]. Either way only one page is held at a time.
#[derive(Debug, Clone)]
pub struct PageWalk {
    /// The checked manifest — a single manifest already unsealed when the
    /// walk holds the root secret.
    manifest: ChunkManifest,
    root_secret: Option<[u8; 32]>,
    /// The file's whole-content hash, when known (plaintext, or opened).
    file_hash: Option<ContentHash>,
    /// Per page, its first chunk index and first byte offset.
    starts: Vec<(u64, u64)>,
}

impl PageWalk {
    /// Open a fetched manifest for walking: the reader-version gate, the
    /// shape and size checks of its shape, and — given the root secret — the
    /// sealed hashes (a single manifest's) or `file_hash` (a root's). Every
    /// refusal that needs no page happens here.
    pub fn open(manifest: ChunkManifest, root_secret: Option<&[u8; 32]>) -> Result<Self> {
        manifest.check_min_reader()?;
        manifest.check_hash_shape()?;
        manifest.check_sizes()?;
        let Some(pages) = manifest.pages.clone() else {
            let manifest = match root_secret {
                Some(root) if manifest.is_sealed() => manifest.unseal_hashes(root)?,
                _ => manifest,
            };
            let file_hash = (!manifest.is_sealed()).then_some(manifest.file_hash);
            return Ok(Self {
                manifest,
                root_secret: root_secret.copied(),
                file_hash,
                starts: vec![(0, 0)],
            });
        };
        let file_hash = match (&manifest.sealed_hashes, root_secret) {
            (None, _) => Some(manifest.file_hash),
            (Some(sealed), Some(root)) => {
                let hashes: Vec<ContentHash> = pages.iter().map(|p| p.hash).collect();
                Some(crate::manifest_crypto::open_root_file_hash(
                    root, &hashes, sealed,
                )?)
            }
            (Some(_), None) => None,
        };
        let mut starts = Vec::with_capacity(pages.len());
        let (mut chunk, mut byte) = (0u64, 0u64);
        for p in &pages {
            starts.push((chunk, byte));
            chunk += u64::from(p.chunk_count);
            byte += p.byte_len;
        }
        Ok(Self {
            manifest,
            root_secret: root_secret.copied(),
            file_hash,
            starts,
        })
    }

    /// Whether the manifest is a paged root.
    pub fn is_paged(&self) -> bool {
        self.manifest.is_paged()
    }

    /// How many pages the walk yields (1 for a single manifest).
    pub fn page_count(&self) -> usize {
        self.starts.len()
    }

    /// The root's page list (empty for a single manifest).
    pub fn page_refs(&self) -> &[ManifestPageRef] {
        self.manifest.pages.as_deref().unwrap_or(&[])
    }

    /// The file's size in bytes.
    pub fn total_size(&self) -> u64 {
        self.manifest.total_size
    }

    /// The whole-file content address the reassembled chunks must hash to —
    /// `None` for a sealed manifest walked without the root secret.
    pub fn file_hash(&self) -> Option<ContentHash> {
        self.file_hash
    }

    /// The blob to fetch for page `index`: `Some(hash)` for a root's page,
    /// `None` for a single manifest (its one page is the manifest itself).
    pub fn page_to_fetch(&self, index: usize) -> Option<ContentHash> {
        self.page_refs().get(index).map(|p| p.hash)
    }

    /// The file offset of page `index`'s first byte (0 for a single
    /// manifest's one page); `None` past the last page.
    pub fn page_offset(&self, index: usize) -> Option<u64> {
        self.starts.get(index).map(|&(_, byte)| byte)
    }

    /// The pages a byte range `[offset, offset + len)` touches — what a ranged
    /// read (on-demand hydration, web and share serving) fetches. Empty for an
    /// empty range or one past the end of the file.
    pub fn pages_for_range(&self, offset: u64, len: u64) -> Range<usize> {
        let end = offset.saturating_add(len).min(self.total_size());
        if len == 0 || offset >= end {
            return 0..0;
        }
        let first = self.starts.partition_point(|&(_, b)| b <= offset) - 1;
        let last = self.starts.partition_point(|&(_, b)| b < end);
        first..last
    }

    /// Open page `index`. A single manifest's one page takes no bytes
    /// (`None`); a root's page takes the fetched blob, which must hash to the
    /// address the root names, decode to a page and not a root, pass the
    /// page's shape check, match the root's seal (sealed pages under a sealed
    /// root only) and hold exactly the entry count and byte length the root
    /// names. Given the root secret, a sealed page's hashes are opened.
    pub fn open_page(&self, index: usize, bytes: Option<&[u8]>) -> Result<WalkedPage> {
        let (first_chunk, byte_offset) = *self
            .starts
            .get(index)
            .ok_or_else(|| anyhow!("manifest walk: page {index} is past the last page"))?;
        let Some(pages) = &self.manifest.pages else {
            if bytes.is_some() {
                bail!("manifest walk: a single manifest has no page blob to open");
            }
            let m = &self.manifest;
            return Ok(WalkedPage {
                index,
                first_chunk,
                byte_offset,
                store_keys: m.store_keys()?,
                chunk_hashes: (!m.is_sealed()).then(|| m.chunk_hashes.clone()),
                chunk_sizes: m.chunk_sizes.clone(),
            });
        };
        let want = &pages[index];
        let bytes = bytes.ok_or_else(|| anyhow!("manifest walk: page {index} needs its blob"))?;
        let actual = ContentHash::of_raw(bytes);
        if actual != want.hash {
            bail!(
                "manifest page {index} hashes to {} but its root names {}",
                hex::encode(actual.digest()),
                hex::encode(want.hash.digest())
            );
        }
        let page = ManifestPage::decode_checked(bytes)
            .map_err(|e| anyhow!("manifest page {index}: {e:#}"))?;
        if page.sealed_hashes.is_some() != self.manifest.is_sealed() {
            bail!("manifest page {index} is not sealed the way its root is");
        }
        if page.entry_count() != want.chunk_count as usize {
            bail!(
                "manifest page {index} holds {} entries but its root names {}",
                page.entry_count(),
                want.chunk_count
            );
        }
        if page.byte_len() != want.byte_len {
            bail!(
                "manifest page {index} covers {} bytes but its root names {}",
                page.byte_len(),
                want.byte_len
            );
        }
        let chunk_hashes = match (&page.sealed_hashes, &self.root_secret) {
            (None, _) => Some(page.chunk_hashes.clone()),
            (Some(sealed), Some(root)) => {
                let stored = page.store_keys();
                let opened = crate::manifest_crypto::open_manifest_hashes(root, stored, sealed)?;
                if opened.file_hash != blank_file_hash()
                    || opened.chunk_hashes.len() != stored.len()
                {
                    bail!("manifest page {index} opens to a payload that is not its own");
                }
                Some(opened.chunk_hashes)
            }
            (Some(_), None) => None,
        };
        Ok(WalkedPage {
            index,
            first_chunk,
            byte_offset,
            store_keys: page.store_keys().to_vec(),
            chunk_hashes,
            chunk_sizes: page.chunk_sizes,
        })
    }

    /// The file's plaintext view as ONE single manifest — every page walked
    /// through `fetch` (and verified as [`Self::open_page`] verifies it) and
    /// its entries put back together in file order: the shape every reader
    /// that needs the plaintext hashes in hand at once takes (a private link's
    /// mint, a share serve's chunk index). A single manifest comes back as
    /// the walk opened it. Refused when the walk holds no plaintext view (a
    /// sealed manifest walked without its root secret) or any page fails.
    pub fn flatten<F>(&self, fetch: F) -> Result<ChunkManifest>
    where
        F: FnMut(&ContentHash) -> Result<Vec<u8>>,
    {
        let file_hash = self
            .file_hash
            .ok_or_else(|| anyhow!("manifest walk: no plaintext view without the root secret"))?;
        if !self.is_paged() {
            return Ok(self.manifest.clone());
        }
        let n = self.manifest.entry_count();
        let (mut chunk_hashes, mut chunk_sizes, mut store_keys) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for page in self.pages(fetch) {
            let page = page?;
            chunk_hashes.extend(page.chunk_hashes.ok_or_else(|| {
                anyhow!("manifest page {} did not open to plaintext", page.index)
            })?);
            chunk_sizes.extend(page.chunk_sizes);
            store_keys.extend(page.store_keys);
        }
        Ok(ChunkManifest {
            file_hash,
            total_size: self.total_size(),
            chunk_hashes,
            chunk_sizes,
            stored_hashes: self.manifest.is_sealed().then_some(store_keys),
            sealed_hashes: None,
            min_reader: None,
            pages: None,
        })
    }

    /// Every page in file order, each fetched through `fetch` only when the
    /// iterator reaches it (a single manifest fetches nothing).
    pub fn pages<'w, F>(&'w self, mut fetch: F) -> impl Iterator<Item = Result<WalkedPage>> + 'w
    where
        F: FnMut(&ContentHash) -> Result<Vec<u8>> + 'w,
    {
        (0..self.page_count()).map(move |i| match self.page_to_fetch(i) {
            None => self.open_page(i, None),
            Some(hash) => {
                let bytes = fetch(&hash)?;
                self.open_page(i, Some(&bytes))
            }
        })
    }
}

/// The keys that mark a manifest (single or root) rather than a page: a page
/// carries neither, so a "page" decoding with either is a root naming a
/// manifest — refused, since manifests are two levels and never deeper.
#[derive(Deserialize)]
struct RootProbe {
    #[serde(default)]
    pages: Option<Vec<ManifestPageRef>>,
    #[serde(default)]
    file_hash: Option<ContentHash>,
}

#[cfg(test)]
#[path = "manifest_paging_tests.rs"]
mod tests;
