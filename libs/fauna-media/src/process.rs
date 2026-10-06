//! ProcessedMedia + process_media.
//!
//! Real impl (sniff MIME → strip EXIF/IPTC → detect C2PA → maybe-thumbnail)
//! is the lift of `process_plaintext_blob` from
//! `bins/fauna-nest/src/storage/plaintext.rs`. Two Cargo features carve it:
//! `process_media` (the curated baseline — MIME sniff + EXIF/IPTC strip +
//! JPEG thumbnail, pulling `image` + `img-parts`) and the additive
//! `c2pa-detect` (populates `has_c2pa`, pulling the heavy `c2pa` tree). All of
//! `image`/`img-parts`/`c2pa` cross-compile everywhere (`c2pa` via
//! `rust_native_crypto`), so the split is driven by wasm bundle weight, not
//! feasibility: a lean web build may take `process_media` alone. With neither
//! feature, `process_media` is the identity-passthrough stub at the bottom.

/// What `process_media` produces from raw uploader-supplied bytes.
///
/// `stripped_bytes` is what gets sealed and uploaded; `mime` /
/// `thumbnail_bytes` / `has_c2pa` populate the `UploadSidecar`.
#[derive(Debug, Clone)]
pub struct ProcessedMedia {
    pub stripped_bytes: Vec<u8>,
    pub mime: String,
    pub thumbnail_bytes: Option<Vec<u8>>,
    pub has_c2pa: bool,
}

/// What [`strip_metadata`] removes from a given container — the honest
/// coverage map, as a value.
///
/// Returned by [`strip_coverage`] and asserted format-by-format in
/// `tests/process_test.rs`, so the goal doc's coverage claim
/// (`docs/goal/behavior/file-sync.md` § Ingress metadata-strip convergence) has
/// a single executable source of truth rather than a prose summary that drifts
/// — which is precisely how the JPEG/PNG-only coverage came to be described as
/// "EXIF/GPS is stripped on the ingress" for a year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripCoverage {
    /// Every metadata carrier this container defines is removed, losslessly —
    /// the compressed media payload is never decoded or re-encoded.
    Stripped,
    /// Metadata this build did not remove may still be present. The
    /// `&'static str` is the reason, and it is what `file-sync.md` publishes as
    /// a declared residual — never a silent pass-through.
    ///
    /// **There is deliberately no third "nothing to worry about" answer.** A
    /// `NoCarrier` variant existed until 2026-08-15 and `Container::Unknown`
    /// mapped to it, reasoning "we could not identify a container, so there is
    /// no metadata layout to remove". That inference does not follow, and the
    /// format family it is most wrong about is the one that *defines* Exif: the
    /// sniff has no TIFF branch, so every DNG — Apple ProRAW, Android RAW —
    /// sniffed as `Unknown` and was reported as carrying nothing while keeping
    /// its full Exif block. Failing to parse bytes is not evidence about what
    /// they contain.
    Residual(&'static str),
}

/// A container family recognized from a byte prefix alone — pure comparison,
/// no decoding. Shared between [`Container::sniff`]'s MIME classification
/// (feature-gated on `process_media`, since only that build needs the full
/// `mime()`/`strip_coverage()` treatment) and the storage layer's AEAD-shape
/// sanity check
/// (`bins/fauna-nest/src/storage/sealed.rs::has_plaintext_magic_prefix`,
/// which must reject a sealed blob that looks like a known plaintext format
/// and stays unconditionally compiled — a sealed-storage security gate can
/// never depend on a media-processing feature flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownPrefix {
    Png,
    Jpeg,
    Webp,
    Gif,
    /// ISO-BMFF with a HEVC-coded image brand.
    Heif,
    /// ISO-BMFF with an AV1-coded image brand.
    Avif,
    /// ISO-BMFF with any other brand — mp4/mov/m4a/3gp.
    IsoBmffVideo,
    Matroska,
    Pdf,
}

/// Sniff `body`'s leading bytes against every plaintext container this crate
/// recognizes. `None` means unrecognized, not "not plaintext" — an unrecognized
/// prefix (a TIFF/DNG, arbitrary binary, or genuine AEAD-sealed ciphertext) is
/// exactly the case both callers treat differently: the MIME sniffer reports
/// `Container::Unknown` and passes the bytes through untouched, while the
/// storage-layer gate treats "no known plaintext prefix" as consistent with a
/// sealed blob.
pub fn sniff_known_prefix(body: &[u8]) -> Option<KnownPrefix> {
    if body.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some(KnownPrefix::Png)
    } else if body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(KnownPrefix::Jpeg)
    } else if body.starts_with(b"RIFF") && body.len() >= 12 && &body[8..12] == b"WEBP" {
        Some(KnownPrefix::Webp)
    } else if body.starts_with(b"GIF8") {
        Some(KnownPrefix::Gif)
    } else if body.starts_with(b"%PDF") {
        Some(KnownPrefix::Pdf)
    } else if body.len() >= 12 && &body[4..8] == b"ftyp" {
        // ISO-BMFF. The 4-byte major brand at 8..12 separates the still-image
        // brands from video — see `Container`'s doc comment for why HEIC/AVIF
        // must not collapse into `video/mp4`.
        match &body[8..12] {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => Some(KnownPrefix::Heif),
            b"avif" | b"avis" => Some(KnownPrefix::Avif),
            _ => Some(KnownPrefix::IsoBmffVideo),
        }
    } else if body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some(KnownPrefix::Matroska)
    } else {
        None
    }
}

/// Whether this build compiled the real pipeline (`true`) or the
/// identity-passthrough stub (`false`).
///
/// Exposed so a consumer whose correctness *depends* on the real pipeline can
/// make that a compile-time assertion rather than a silent runtime degradation
/// — `process_media` is frequently enabled by **feature unification** from a
/// sibling crate rather than requested directly, so it can be switched off by
/// an edit far away from the code that needs it. The web bundle is the standing
/// example: it inherits the feature through `fauna-conversations`' wasm32
/// block, and without it the feed producer stamps no `thumbnail_hash` at all.
pub const PROCESS_MEDIA_ENABLED: bool = cfg!(feature = "process_media");

#[cfg(feature = "process_media")]
pub(crate) use real::render_thumbnail_from_path;
#[cfg(feature = "process_media")]
pub use real::{
    detect_c2pa, process_media, strip_coverage, strip_metadata, strip_metadata_with_coverage,
};

#[cfg(not(feature = "process_media"))]
pub(crate) use stub::render_thumbnail_from_path;
#[cfg(not(feature = "process_media"))]
pub use stub::{
    detect_c2pa, process_media, strip_coverage, strip_metadata, strip_metadata_with_coverage,
};

/// The **viewer-side** provenance verdict over bytes alone — the badge
/// correction `docs/goal/ui/media.md` § C2PA provenance rules every app must
/// perform before painting `c2pa-badge`.
///
/// Why a byte-only entry point exists beside [`detect_c2pa`]: the uploader
/// knows its file's MIME because it just sniffed it, but a *viewer* holds only
/// a blob hash and the bytes it fetched. The two things a viewer could take a
/// MIME from — the sidecar's `mime` and the `x-c2pa` header derived from the
/// sidecar's `has_c2pa` — are both **uploader-asserted**: the nest never
/// inspects blob bytes (`bins/fauna-nest/src/storage/sealed.rs`'s `PublicPost`
/// ingest arm checks length and MIME shape only), so a modified client can
/// claim provenance for an image that carries no manifest. Taking the
/// container from the bytes is what makes this a ground truth rather than a
/// second reading of the uploader's word.
///
/// This is not a second detector. It sniffs the container exactly as
/// [`process_media`] does before *its* `detect_c2pa` call, and delegates: a
/// viewer recomputes precisely the `has_c2pa` an honest uploader would have
/// stamped on these bytes, so an agreeing pair means the assertion was true
/// and a disagreeing pair means the badge must not paint.
///
/// `false` for every non-image container, and `false` unconditionally in a
/// build without `c2pa-detect` (the delegate's stub) — **a badge gated on this
/// therefore degrades to "never paints", never to "paints on the uploader's
/// say-so"**, which is the safe direction. That a shipped artifact really
/// carries the feature is witnessed per-artifact by a merge
/// gate, not assumed here.
pub fn detect_c2pa_in_bytes(raw: &[u8]) -> bool {
    // Exhaustive on purpose, `Container::mime`-style: the next container this
    // crate learns to sniff cannot join the "no manifest possible" set by
    // silently falling through an `_` arm. The MIME strings are the ones
    // `Container::mime` hands `detect_c2pa` on the upload path — the two must
    // agree or an honest upload's badge would vanish on the viewer's re-check.
    let mime = match sniff_known_prefix(raw) {
        Some(KnownPrefix::Png) => "image/png",
        Some(KnownPrefix::Jpeg) => "image/jpeg",
        Some(KnownPrefix::Webp) => "image/webp",
        Some(KnownPrefix::Gif) => "image/gif",
        Some(KnownPrefix::Heif) => "image/heic",
        Some(KnownPrefix::Avif) => "image/avif",
        // Not still-image containers: `detect_c2pa` refuses these by MIME on
        // the upload path too (`!mime.starts_with("image/")`), so short-cutting
        // here keeps the two paths' verdicts identical.
        Some(KnownPrefix::IsoBmffVideo | KnownPrefix::Matroska | KnownPrefix::Pdf) => return false,
        // Unrecognized bytes — `Container::Unknown`'s `application/octet-stream`
        // on the upload path, same `false`.
        None => return false,
    };
    detect_c2pa(mime, raw)
}

#[cfg(feature = "process_media")]
mod real {
    use super::{ProcessedMedia, StripCoverage};

    /// Per-axis dimension cap when decoding an uploaded image into a thumbnail. 16384 px
    /// comfortably exceeds any real photo or panorama, so a legitimate upload
    /// is never clipped, but a decompression bomb declaring enormous
    /// dimensions is refused at the header — before the pixel buffer is
    /// allocated — rather than relying solely on the byte-allocation backstop.
    const MAX_THUMBNAIL_SOURCE_DIM: u32 = 16_384;

    /// Sniff MIME, strip the metadata carriers [`strip_coverage`] reports as
    /// removable, detect C2PA via `c2pa::Reader`, and render a JPEG thumbnail
    /// when the source is an image larger than 300×300.
    ///
    /// Originally lifted from `process_plaintext_blob` in
    /// `bins/fauna-nest/src/storage/plaintext.rs`. That transitional nest-side
    /// copy is **gone** — `PlaintextStorage` was deleted in Phase 4 (2026-07-12)
    /// and the nest cannot read sealed bytes at all, so this is now the only
    /// implementation and every caller is uploader-side (`ui/media.md` §
    /// Today's reality).
    pub fn process_media(raw: &[u8]) -> ProcessedMedia {
        let container = Container::sniff(raw);
        let mime = container.mime();
        let stripped = strip_exif_iptc(container, bytes::Bytes::copy_from_slice(raw));
        let has_c2pa = detect_c2pa(mime, stripped.as_ref());
        let thumbnail_bytes = if mime.starts_with("image/") {
            render_thumbnail(stripped.as_ref())
        } else {
            None
        };
        ProcessedMedia {
            stripped_bytes: stripped.to_vec(),
            mime: mime.to_string(),
            thumbnail_bytes,
            has_c2pa,
        }
    }

    /// The container family we sniffed — the single enumeration that both the
    /// reported MIME and the metadata-strip decision are derived from.
    ///
    /// **Why an enum and not the MIME string it used to be.** The strip used to
    /// `match` on the sniffed `&'static str` with a `_ => body` catch-all, so a
    /// container that [`Container::sniff`] learned to *recognise* silently joined the
    /// set that passes through **unstripped** — no compile error, no test
    /// failure, no doc change. That is exactly how WebP (recognised since the
    /// first lift, never stripped) kept its `EXIF` chunk, and how HEIC — the
    /// iPhone default, and the dominant format in a photo-library backup —
    /// carried GPS straight through the ingress the goal doc described as
    /// stripping it.
    ///
    /// With the family enumerated, [`Container::strip_coverage`] is an
    /// exhaustive `match`: a new variant **fails to compile** until someone
    /// decides, in code, whether it is stripped or a named residual. Silence is
    /// no longer expressible.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Container {
        Jpeg,
        Png,
        Webp,
        Gif,
        /// ISO-BMFF with a HEVC-coded image brand (`heic`/`heix`/`hevc`/`hevx`/`mif1`/`msf1`).
        Heif,
        /// ISO-BMFF with an AV1-coded image brand (`avif`/`avis`). The item
        /// structure (`meta`/`iinf`/`iloc`) is HEIF's exactly — only the codec
        /// and therefore the MIME differ, and the MIME is load-bearing:
        /// browsers render `image/avif` but not `image/heic`, so collapsing
        /// the two variants mislabels one of them on the wire.
        Avif,
        /// ISO-BMFF with any other brand — mp4/mov/m4a/3gp.
        IsoBmffVideo,
        Matroska,
        Pdf,
        Unknown,
    }

    impl Container {
        fn sniff(body: &[u8]) -> Self {
            match super::sniff_known_prefix(body) {
                Some(super::KnownPrefix::Png) => Container::Png,
                Some(super::KnownPrefix::Jpeg) => Container::Jpeg,
                Some(super::KnownPrefix::Webp) => Container::Webp,
                Some(super::KnownPrefix::Gif) => Container::Gif,
                Some(super::KnownPrefix::Heif) => Container::Heif,
                Some(super::KnownPrefix::Avif) => Container::Avif,
                Some(super::KnownPrefix::IsoBmffVideo) => Container::IsoBmffVideo,
                Some(super::KnownPrefix::Matroska) => Container::Matroska,
                Some(super::KnownPrefix::Pdf) => Container::Pdf,
                None => Container::Unknown,
            }
        }

        fn mime(self) -> &'static str {
            match self {
                Container::Jpeg => "image/jpeg",
                Container::Png => "image/png",
                Container::Webp => "image/webp",
                Container::Gif => "image/gif",
                Container::Heif => "image/heic",
                Container::Avif => "image/avif",
                Container::IsoBmffVideo => "video/mp4",
                Container::Matroska => "video/webm",
                Container::Pdf => "application/pdf",
                Container::Unknown => "application/octet-stream",
            }
        }

        /// The exhaustive coverage decision. **Adding a `Container` variant
        /// breaks this `match`** — which is the entire point: the next format
        /// we learn to sniff cannot join the unstripped set by default.
        pub(crate) fn strip_coverage(self) -> StripCoverage {
            match self {
                // Segment/chunk removal, C2PA (JPEG APP11 / PNG `caBX`) preserved.
                Container::Jpeg | Container::Png | Container::Webp => StripCoverage::Stripped,
                // Comment + Application extension blocks removed (GIF's only
                // metadata carriers — XMP rides an Application extension).
                Container::Gif => StripCoverage::Stripped,
                // The `Exif` item and the XMP `mime` item, located through
                // `iinf` → `iloc` and zeroed extent-by-extent. See
                // `strip_heif_item_metadata` for why this is item-level surgery
                // and not the box neutralisation the video arm below uses.
                // AVIF shares HEIF's item structure, so the same surgery covers it.
                Container::Heif | Container::Avif => StripCoverage::Stripped,
                // `udta` (`©xyz`/`loci` location), `meta`, and the Adobe XMP
                // `uuid` box, neutralised in place — never removed. See
                // `neutralise_isobmff_metadata` for why in-place is the whole
                // trick.
                Container::IsoBmffVideo => StripCoverage::Stripped,
                Container::Matroska => StripCoverage::Residual(
                    "Matroska `Tags`/`Attachments` elements use EBML variable-length integers; no \
                     EBML parser is in the dependency graph",
                ),
                Container::Pdf => StripCoverage::Residual(
                    "PDF carries the XMP metadata stream and the Info dictionary; no PDF parser is \
                     in the dependency graph",
                ),
                // We could not identify the container, so we cannot say what it
                // carries — and the honest answer to "what metadata is in bytes
                // I failed to parse" is *unknown*, never *none*. The bytes still
                // pass through untouched (mangling what we do not understand is
                // how a backup ingress destroys a file); only the claim changes.
                Container::Unknown => StripCoverage::Residual(
                    "container not recognised — coverage unknown; a TIFF/DNG (Apple ProRAW, \
                     Android RAW) lands here carrying its full Exif block",
                ),
            }
        }
    }

    /// The **static capability** claim for `raw`'s container: does this build
    /// know how to strip that format at all.
    ///
    /// ⚠ **This is not a statement about these bytes.** It sniffs the container
    /// and reads the coverage table; it never runs the strip. Every arm of the
    /// strip is deliberately fail-safe, so for exactly the files the walker
    /// declined to touch this still answers `Stripped` — which is why an app
    /// deciding whether to warn "this file may still carry its location" must
    /// call [`strip_metadata_with_coverage`] instead.
    ///
    /// Exposed because the alternative is every consumer re-deriving the
    /// capability table from a prose doc, and it stays correct when an arm is
    /// added. Not yet on the UniFFI/wasm faces — no app consumes it today, and
    /// adding it there is additive when one does.
    pub fn strip_coverage(raw: &[u8]) -> StripCoverage {
        Container::sniff(raw).strip_coverage()
    }

    /// Strip privacy-sensitive metadata from raw image bytes — **losslessly**.
    ///
    /// The strip-only half of [`process_media`]: same MIME sniff, same
    /// segment/chunk removal, same C2PA preservation — but no thumbnail render
    /// and no C2PA probe. Both entry points share `strip_exif_iptc`, so they
    /// cannot drift into stripping different things
    /// (`tests/process_test.rs::strip_metadata_agrees_with_process_media`).
    ///
    /// **Coverage is exact, not "images".** [`Container::strip_coverage`] is the
    /// executable source of truth and `file-sync.md` § Ingress metadata-strip
    /// convergence publishes the same table: JPEG (APP1/Exif + APP13/IPTC), PNG
    /// (`eXIf`/`tEXt`/`iTXt`/`zTXt`), WebP (`EXIF`/`XMP `), GIF (Comment +
    /// Application extensions), HEIC/HEIF/AVIF (the `Exif` and XMP items) and
    /// mp4/mov (`udta`/`meta`/XMP `uuid`) are stripped; **WebM and PDF are
    /// declared residuals** that pass through *carrying* their metadata, as is
    /// any container the sniff does not recognise.
    /// Do not read this function as "EXIF/GPS is gone" for an arbitrary file —
    /// and to find out for *specific* bytes, call
    /// [`strip_metadata_with_coverage`] rather than pairing this with
    /// [`strip_coverage`].
    ///
    /// This is the entry point for a **backup ingress** — a photo-library
    /// backup copies whole libraries asset-by-asset, where `process_media`'s
    /// per-asset thumbnail decode is wasted work, and where re-encoding is
    /// actively harmful: the backed-up copy is what a restore returns, so a
    /// lossy stripper permanently degrades the only copy the user gets back.
    /// Metadata segments are removed from the container without touching the
    /// compressed image data, so a file with nothing to strip comes back
    /// byte-identical.
    ///
    /// Non-image and unparseable bytes pass through unchanged.
    pub fn strip_metadata(raw: &[u8]) -> Vec<u8> {
        strip_exif_iptc(Container::sniff(raw), bytes::Bytes::copy_from_slice(raw)).to_vec()
    }

    /// [`strip_metadata`], plus what actually happened to *these* bytes.
    ///
    /// Prefer this over pairing [`strip_metadata`] with [`strip_coverage`]: the
    /// latter answers the **static** question ("does this build know how to
    /// strip this container") without running anything, so for a file the walker
    /// fail-safe-declined it still says `Stripped`. This runs the strip and
    /// reports the outcome, which is what a caller deciding whether to warn
    /// "this file may still carry its location" actually needs.
    ///
    /// Both honest over-claims the 2026-08-01 review graded are covered by the
    /// same weakening: a container that failed to parse, and a composite file
    /// whose appended half is a residual.
    pub fn strip_metadata_with_coverage(raw: &[u8]) -> (Vec<u8>, StripCoverage) {
        let (out, coverage) =
            strip_with_coverage(Container::sniff(raw), bytes::Bytes::copy_from_slice(raw));
        (out.to_vec(), coverage)
    }

    /// Detect whether `raw` carries a parseable C2PA manifest — the probe-only
    /// half of [`process_media`]: same `c2pa::Reader` check, but no strip and
    /// no thumbnail. `mime` is the caller's own MIME for these bytes (e.g. a
    /// received attachment's already-known `mime_type`), so this never
    /// re-sniffs the container.
    ///
    /// C2PA detection is the additive `c2pa-detect` layer (pulls the heavy
    /// `c2pa` tree). A curated build (`process_media` alone — e.g. a lean web
    /// bundle) still compiles this, but reports `false` unconditionally,
    /// mirroring `process_media`'s degraded reading.
    ///
    /// Both parameters are read only inside the `c2pa-detect` arm, so the
    /// curated build the paragraph above describes leaves them unused and
    /// `-D unused-variables` fires — a configuration this doc comment calls
    /// deliberate would not compile. Scoped to exactly that configuration
    /// rather than allowed outright, so with `c2pa-detect` on no attribute is
    /// applied at all and a genuinely unused parameter still goes red.
    #[cfg_attr(not(feature = "c2pa-detect"), allow(unused_variables))]
    pub fn detect_c2pa(mime: &str, raw: &[u8]) -> bool {
        #[cfg(feature = "c2pa-detect")]
        {
            mime.starts_with("image/")
                && c2pa::Reader::from_context(c2pa::Context::default())
                    .with_stream(mime, std::io::Cursor::new(raw))
                    .is_ok()
        }
        #[cfg(not(feature = "c2pa-detect"))]
        {
            false
        }
    }

    /// Remove every metadata carrier [`Container::strip_coverage`] reports as
    /// [`StripCoverage::Stripped`], **without decoding the media payload**.
    ///
    /// Every arm is fail-safe: an unparseable container returns the original
    /// bytes rather than a truncated or half-rewritten file. That direction is
    /// deliberate and load-bearing for the backup ingress — the copy we store is
    /// the copy a restore returns, so "leave it exactly as the user gave it to
    /// us" beats any attempt to salvage a container we could not read.
    fn strip_exif_iptc(container: Container, body: bytes::Bytes) -> bytes::Bytes {
        strip_with_coverage(container, body).0
    }

    /// The strip, plus what actually happened — as opposed to
    /// [`Container::strip_coverage`], which answers the *static* question "does
    /// this build know how to strip this container" without running anything.
    ///
    /// The distinction is load-bearing: every arm below
    /// is deliberately fail-safe, so for exactly the files the walker declined
    /// to touch, the static claim still said `Stripped`. A caller asking "is the
    /// GPS gone from *these* bytes" needs the outcome, not the capability.
    fn strip_with_coverage(
        container: Container,
        body: bytes::Bytes,
    ) -> (bytes::Bytes, StripCoverage) {
        match container.strip_coverage() {
            // Named, published in file-sync.md, and asserted in the tests — the
            // point is that reaching here is a *decision*, not a fall-through.
            residual @ StripCoverage::Residual(_) => return (body, residual),
            StripCoverage::Stripped => {}
        }
        let stripped = match container {
            Container::Jpeg => {
                // ⚠ A JPEG is not always only a JPEG. Google/Samsung "Motion
                // Photo" is a single `.jpg` with a *complete mp4 appended after
                // EOI*, and MediaStore hands it to the ingress as `image/jpeg`
                // — so dispatching on the outer container alone stripped the
                // still half and walked straight past the video's
                // `moov/udta/©xyz` GPS, while coverage reported
                // `Stripped`. That is the default camera output of a large
                // share of Android devices, on the ingress the user guides
                // describe as removing location.
                //
                // So: split the still from whatever follows it *before* handing
                // anything to the JPEG parser, strip each half through this same
                // function, and rejoin. Splitting first also means we never
                // depend on the JPEG encoder's trailer behaviour.
                let still_end = jpeg_still_end(&body).unwrap_or(body.len());
                // A still the parser rejects keeps the whole file untouched —
                // trailer included — and says so.
                let Some(still) = strip_jpeg_segments(body.slice(..still_end)) else {
                    return (body, BAILED);
                };
                if still_end == body.len() {
                    still
                } else {
                    let trailer = body.slice(still_end..);
                    let (trailer, trailer_coverage) =
                        strip_with_coverage(Container::sniff(&trailer), trailer);
                    let mut joined = Vec::with_capacity(still.len() + trailer.len());
                    joined.extend_from_slice(&still);
                    joined.extend_from_slice(&trailer);
                    // A composite is only as stripped as its least-stripped half.
                    if let StripCoverage::Residual(_) = trailer_coverage {
                        return (
                            bytes::Bytes::from(joined),
                            StripCoverage::Residual(
                                "a container is appended after the JPEG and its own metadata is a \
                                 residual — the still half was stripped, the appended half was not",
                            ),
                        );
                    }
                    bytes::Bytes::from(joined)
                }
            }
            Container::Png => match img_parts::png::Png::from_bytes(body.clone()) {
                Ok(mut png) => {
                    let strip_kinds: &[&[u8; 4]] = &[b"eXIf", b"tEXt", b"iTXt", b"zTXt"];
                    png.chunks_mut()
                        .retain(|chunk| !strip_kinds.iter().any(|k| chunk.kind() == **k));
                    bytes::Bytes::from(png.encoder().bytes().to_vec())
                }
                Err(_) => return (body, BAILED),
            },
            Container::Webp => match img_parts::webp::WebP::from_bytes(body.clone()) {
                Ok(mut webp) => {
                    // RIFF chunk removal, the exact analogue of the PNG arm.
                    // `EXIF` is where a phone camera writes GPS; `XMP ` (note
                    // the significant trailing space — RIFF ids are always four
                    // bytes) carries the same coordinates in XML. The image
                    // payload chunks (`VP8 `/`VP8L`/`ALPH`/`ANIM`/`ANMF`) and
                    // the C2PA container are untouched.
                    let strip_ids: &[&[u8; 4]] = &[b"EXIF", b"XMP "];
                    webp.chunks_mut()
                        .retain(|chunk| !strip_ids.iter().any(|id| chunk.id() == **id));
                    bytes::Bytes::from(webp.encoder().bytes().to_vec())
                }
                Err(_) => return (body, BAILED),
            },
            Container::Gif => match strip_gif_extensions(&body) {
                Some(stripped) => bytes::Bytes::from(stripped),
                None => return (body, BAILED),
            },
            Container::IsoBmffVideo => match neutralise_isobmff_metadata(&body) {
                Some(stripped) => bytes::Bytes::from(stripped),
                None => return (body, BAILED),
            },
            Container::Heif | Container::Avif => match strip_heif_item_metadata(&body) {
                Some(stripped) => bytes::Bytes::from(stripped),
                None => return (body, BAILED),
            },
            // Unreachable by the coverage guard above, but spelled out rather
            // than `_ =>` so that adding a variant still breaks this match.
            Container::Matroska | Container::Pdf | Container::Unknown => body,
        };
        (stripped, StripCoverage::Stripped)
    }

    /// What every fail-safe arm reports when it declines to touch a container it
    /// *does* know how to strip. The bytes come back exactly as the user gave
    /// them — that direction is load-bearing for a backup ingress — but the
    /// claim must not stay `Stripped`.
    const BAILED: StripCoverage =
        StripCoverage::Residual("container failed to parse — returned untouched, nothing removed");

    /// Strip a JPEG's APP1 (Exif) + APP13 (IPTC) segments, keeping APP11
    /// (JUMBF/C2PA). Callers pass the **still image alone**; see the Motion
    /// Photo note in [`strip_with_coverage`] for why the split happens first.
    /// `None` when the parser rejects the still — the caller then returns the
    /// user's bytes untouched and reports the bail.
    fn strip_jpeg_segments(still: bytes::Bytes) -> Option<bytes::Bytes> {
        let mut jpeg = img_parts::jpeg::Jpeg::from_bytes(still).ok()?;
        jpeg.segments_mut().retain(|seg| {
            seg.marker() != img_parts::jpeg::markers::APP1
                && seg.marker() != img_parts::jpeg::markers::APP13
        });
        Some(bytes::Bytes::from(jpeg.encoder().bytes().to_vec()))
    }

    /// Byte offset one past the JPEG's terminating `EOI` marker, or `None` if
    /// the bytes do not walk as a JPEG.
    ///
    /// Scanning for a bare `FF D9` is wrong — that pair occurs freely inside
    /// entropy-coded data — so this walks the marker structure: fixed-length
    /// markers (`RSTn`, `TEM`), length-prefixed segments, and after `SOS` the
    /// entropy-coded run, which ends at the first `FF xx` that is neither the
    /// `FF 00` stuffed byte nor a restart marker.
    fn jpeg_still_end(raw: &[u8]) -> Option<usize> {
        if raw.len() < 4 || raw[0] != 0xFF || raw[1] != 0xD8 {
            return None;
        }
        let mut i = 2usize;
        loop {
            // Fill bytes: any number of 0xFF may precede a marker.
            while i < raw.len() && raw[i] == 0xFF && raw.get(i + 1) == Some(&0xFF) {
                i += 1;
            }
            if i + 1 >= raw.len() || raw[i] != 0xFF {
                return None;
            }
            let marker = raw[i + 1];
            match marker {
                0xD9 => return Some(i + 2), // EOI — one past it is the trailer
                // Standalone markers: no length field, no payload.
                0x01 | 0xD0..=0xD8 => {
                    i += 2;
                }
                _ => {
                    let len = u16::from_be_bytes([*raw.get(i + 2)?, *raw.get(i + 3)?]) as usize;
                    if len < 2 {
                        return None;
                    }
                    i = i.checked_add(2)?.checked_add(len)?;
                    if i > raw.len() {
                        return None;
                    }
                    if marker == 0xDA {
                        // Entropy-coded data follows the SOS header.
                        while i < raw.len() {
                            if raw[i] != 0xFF {
                                i += 1;
                                continue;
                            }
                            match raw.get(i + 1) {
                                // Stuffed byte / restart: still data.
                                Some(0x00) | Some(0xD0..=0xD7) => i += 2,
                                // A fill byte: step ONE byte, so the next `FF`
                                // is read as a possible marker prefix. Stepping
                                // the pair walked `FF FF D9` straight past its
                                // EOI.
                                Some(0xFF) => i += 1,
                                Some(_) => break, // a real marker
                                None => return None,
                            }
                        }
                    }
                }
            }
        }
    }

    /// Remove GIF Comment (`0xFE`) and Application (`0xFF`) extension blocks.
    ///
    /// GIF is the one stripped container with no parser in the dependency graph
    /// (`img-parts` covers JPEG/PNG/WebP only), but it is also the one that
    /// needs none: the format is a flat, self-delimiting block stream with **no
    /// offset table anywhere**, so removing a block can never invalidate a
    /// pointer the way it would in ISO-BMFF. The two extensions removed here are
    /// GIF's only metadata carriers — XMP rides an Application extension, whose
    /// conventional 258-byte magic trailer is deliberately shaped to walk
    /// correctly as ordinary data sub-blocks, so the plain walk below consumes
    /// it whole.
    ///
    /// Returns `None` on any structural surprise (truncation, an unknown block
    /// introducer, a sub-block chain running past the end), so the caller keeps
    /// the user's original bytes. Never decodes or re-compresses pixel data:
    /// retained blocks are copied through verbatim.
    fn strip_gif_extensions(body: &[u8]) -> Option<Vec<u8>> {
        // Header (6) + Logical Screen Descriptor (7).
        let mut pos = 13usize;
        if body.len() < pos {
            return None;
        }
        let packed = body[10];
        if packed & 0x80 != 0 {
            // Global Colour Table: 3 * 2^((packed & 7) + 1) bytes.
            pos = pos.checked_add(3 << ((packed & 0x07) + 1))?;
        }
        if pos > body.len() {
            return None;
        }

        let mut out = Vec::with_capacity(body.len());
        out.extend_from_slice(&body[..pos]);

        loop {
            let block_start = pos;
            match *body.get(pos)? {
                // Trailer — everything after it is not ours to interpret.
                0x3B => {
                    out.extend_from_slice(&body[block_start..]);
                    return Some(out);
                }
                // Image Descriptor: 10 header bytes, optional Local Colour
                // Table, then LZW-minimum-code-size + image data sub-blocks.
                0x2C => {
                    pos = pos.checked_add(10)?;
                    let local = *body.get(pos - 1)?;
                    if local & 0x80 != 0 {
                        pos = pos.checked_add(3 << ((local & 0x07) + 1))?;
                    }
                    // LZW minimum code size.
                    pos = pos.checked_add(1)?;
                    pos = skip_gif_sub_blocks(body, pos)?;
                    out.extend_from_slice(body.get(block_start..pos)?);
                }
                // Extension: introducer, label, then sub-blocks.
                0x21 => {
                    let label = *body.get(pos + 1)?;
                    pos = skip_gif_sub_blocks(body, pos.checked_add(2)?)?;
                    // 0xFE Comment, 0xFF Application (XMP lives here). Graphic
                    // Control (0xF9) and Plain Text (0x01) are render state, not
                    // metadata about the photographer — keep them.
                    if label != 0xFE && label != 0xFF {
                        out.extend_from_slice(body.get(block_start..pos)?);
                    }
                }
                // An introducer we do not recognise means our idea of the
                // structure is wrong; stop and keep the original file.
                _ => return None,
            }
        }
    }

    /// The 16-byte UUID that identifies an ISO-BMFF `uuid` box carrying an XMP
    /// packet (Adobe's XMP Specification Part 3). Matching on it rather than
    /// neutralising every `uuid` box matters: `uuid` is the extension mechanism
    /// for *any* vendor payload, and some carry data a player needs.
    const XMP_UUID: [u8; 16] = [
        0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF,
        0xAC,
    ];

    /// Neutralise the metadata boxes of an ISO-BMFF **video** container
    /// (mp4/mov/m4a/3gp) — `udta` (which holds the QuickTime `©xyz` and 3GPP
    /// `loci` location atoms), `meta`, and the Adobe XMP `uuid` box.
    ///
    /// **The trick, and the reason this is safe: nothing is removed and nothing
    /// moves.** A box is neutralised by overwriting its 4-byte *type* field with
    /// `free` — the ignorable free-space box of ISO/IEC 14496-12 §8.1.2 — and
    /// zeroing its payload. The size field is not touched *at all* (so 32-bit,
    /// 64-bit `largesize`, and the size-0-means-to-EOF forms are all preserved
    /// untouched), every following byte keeps its offset, and `mdat` does not
    /// move. That matters more than it sounds: the obvious implementation —
    /// deleting the box — shifts `mdat` and silently invalidates **every**
    /// `stco`/`co64` chunk offset in the file unless each is fixed up, and a
    /// mistake there corrupts a video whose only copy may be the one a backup
    /// restore returns. In-place neutralisation removes that entire failure
    /// class rather than trying to get the fixup right.
    ///
    /// Only descends into the container boxes that hold metadata (`moov` and
    /// its `trak` children) — `mdat` is never walked and never written.
    /// Returns `None` on any structural surprise, so the caller keeps the
    /// user's original bytes.
    ///
    /// ⚠ Video only. `Container::Heif` routes elsewhere precisely because a
    /// HEIC's top-level `meta` box is **structural** (it holds the `iloc`/`iinf`
    /// that locate the image itself), so `free`-ing it would destroy the
    /// picture. For a video brand, a top-level `meta` is metadata.
    fn neutralise_isobmff_metadata(body: &[u8]) -> Option<Vec<u8>> {
        let mut out = body.to_vec();
        let len = out.len();
        neutralise_isobmff_range(&mut out, 0, len, 0)?;
        Some(out)
    }

    /// Walk the boxes in `[start, end)`, neutralising metadata boxes and
    /// recursing into the containers that can hold them. `depth` bounds the
    /// recursion so a malformed file cannot drive it without limit.
    fn neutralise_isobmff_range(buf: &mut [u8], start: usize, end: usize, depth: u8) -> Option<()> {
        // moov → trak → (udta|meta) is the deepest nesting this walk needs.
        if depth > 3 {
            return Some(());
        }
        let mut pos = start;
        while pos < end {
            let BmffBox {
                kind,
                content_start,
                box_end,
            } = read_bmff_box(buf, pos, end)?;

            match &kind {
                b"udta" | b"meta" => neutralise_box(buf, pos, content_start, box_end),
                // Only the XMP uuid — any other vendor extension is left alone.
                b"uuid" => {
                    let uuid = buf.get(content_start..content_start.checked_add(16)?);
                    if uuid == Some(&XMP_UUID[..]) {
                        neutralise_box(buf, pos, content_start, box_end);
                    }
                }
                // Containers that can hold the boxes above. `mdat` is
                // deliberately absent: it is media payload, not a box tree.
                b"moov" | b"trak" => {
                    neutralise_isobmff_range(buf, content_start, box_end, depth + 1)?
                }
                _ => {}
            }
            pos = box_end;
        }
        Some(())
    }

    /// Rewrite one box's type to `free` and zero its payload, leaving every
    /// size byte — and therefore every offset in the file — exactly as it was.
    fn neutralise_box(buf: &mut [u8], box_start: usize, content_start: usize, box_end: usize) {
        buf[box_start + 4..box_start + 8].copy_from_slice(b"free");
        buf[content_start..box_end].fill(0);
    }

    /// One parsed ISO-BMFF box header. `content_start` is the first payload
    /// byte (past the 8- or 16-byte header); `box_end` the first byte after the
    /// box.
    struct BmffBox {
        kind: [u8; 4],
        content_start: usize,
        box_end: usize,
    }

    /// Parse the box header at `pos`, bounded by the enclosing box's `end`.
    ///
    /// Shared by the video neutraliser and the HEIF item walk so the three
    /// size encodings — 32-bit, the `1` escape to a 64-bit `largesize`, and
    /// `0` meaning "to the end of the enclosing box" — are read one way in one
    /// place. Returns `None` on any header that does not fit inside its parent,
    /// which is what makes both callers fail safe onto the original bytes.
    fn read_bmff_box(buf: &[u8], pos: usize, end: usize) -> Option<BmffBox> {
        // A box header is at minimum size(4) + type(4).
        if pos.checked_add(8)? > end {
            return None;
        }
        let size32 = u32::from_be_bytes(buf.get(pos..pos + 4)?.try_into().ok()?) as u64;
        let kind: [u8; 4] = buf.get(pos + 4..pos + 8)?.try_into().ok()?;
        let (box_len, header_len) = match size32 {
            // 0 = "extends to the end of the enclosing box".
            0 => ((end - pos) as u64, 8usize),
            // 1 = the real size is the 64-bit `largesize` that follows.
            1 => {
                if pos.checked_add(16)? > end {
                    return None;
                }
                (
                    u64::from_be_bytes(buf.get(pos + 8..pos + 16)?.try_into().ok()?),
                    16usize,
                )
            }
            n => (n, 8usize),
        };
        let box_len = usize::try_from(box_len).ok()?;
        // A box must contain its own header and must not run past its parent.
        if box_len < header_len || pos.checked_add(box_len)? > end {
            return None;
        }
        Some(BmffBox {
            kind,
            content_start: pos + header_len,
            box_end: pos + box_len,
        })
    }

    /// The `content_type` HEIF uses for an XMP packet stored as a `mime` item
    /// (ISO/IEC 23008-12) — the still-image analogue of the video arm's XMP
    /// `uuid` box, carrying the same coordinates as XML.
    const HEIF_XMP_CONTENT_TYPE: &[u8] = b"application/rdf+xml";

    /// Strip the metadata **items** of a HEIC/HEIF/AVIF still image — the Exif
    /// item and the XMP `mime` item — by zeroing the extent bytes each one
    /// occupies, leaving every box, every length and every offset in place.
    ///
    /// **Why this cannot reuse the video walker, which is the trap here.** In an
    /// mp4 a top-level `meta` box is metadata and neutralising it to `free` is
    /// safe. In a HEIC the top-level `meta` box is **structural**: it holds the
    /// `iinf` (which item is which) and the `iloc` (where each item's bytes
    /// are) that locate the picture itself, so `free`-ing it destroys the image.
    /// That is why [`Container::Heif`] and [`Container::IsoBmffVideo`] are
    /// separate variants routing to separate code — do not "simplify" them
    /// together.
    ///
    /// So the surgery is item-level: resolve `iinf` (item id → item type) against
    /// `iloc` (item id → absolute extents), then zero only the extents belonging
    /// to a metadata item. Nothing is inserted, removed or moved, so — exactly as
    /// in the video arm — every `iloc` offset in the file stays valid by
    /// construction rather than by fixup. A zeroed Exif item is still *declared*,
    /// at its original length; a reader that follows it finds no valid TIFF
    /// header and moves on.
    ///
    /// Fail-safe on **any** structural surprise: a missing `meta`/`iinf`/`iloc`,
    /// a field-size the spec does not permit, an extent running past the end of
    /// the file, or a construction method that would make an offset mean
    /// something other than "position in this file". The caller then keeps the
    /// user's original bytes — which for a photo-library backup is the only copy
    /// a restore returns, so a half-understood file is given back untouched
    /// rather than guessed at.
    ///
    /// And fail-safe on one surprise that is *not* structural: a file we read
    /// correctly that **declares** a metadata extent overlapping another item's
    /// bytes. Every guard above assumes the danger is misreading the layout;
    /// this one exists because a valid-in-every-field file can still ask us to
    /// zero the photo, and the whitelist cannot object because the item really
    /// is Exif. Likewise a metadata extent that is not wholly inside an item-data
    /// payload (a top-level `mdat`, or `meta/idat`) — one declared over the
    /// file's own boxes overlaps no item, yet zeroing it breaks the file.
    fn strip_heif_item_metadata(body: &[u8]) -> Option<Vec<u8>> {
        // Top-level walk: find `meta`. (Unlike a video, we never descend into
        // `moov`/`trak` — a still image has neither.)
        let mut meta: Option<BmffBox> = None;
        // Every top-level `mdat` payload — with `idat`'s below, the only bytes
        // a metadata extent may lie in.
        let mut item_data: Vec<(usize, usize)> = Vec::new();
        let mut pos = 0usize;
        while pos < body.len() {
            let b = read_bmff_box(body, pos, body.len())?;
            pos = b.box_end;
            match &b.kind {
                b"meta" => meta = Some(b),
                b"mdat" => item_data.push((b.content_start, b.box_end)),
                _ => {}
            }
        }
        let meta = meta?;
        // `meta` is a FullBox: 1 version byte + 3 flag bytes precede its
        // children. Reading it as a plain container skews every child by 4.
        let children_start = meta.content_start.checked_add(4)?;
        if children_start > meta.box_end {
            return None;
        }

        let (mut iinf, mut iloc, mut idat) = (None, None, None);
        let mut pos = children_start;
        while pos < meta.box_end {
            let b = read_bmff_box(body, pos, meta.box_end)?;
            pos = b.box_end;
            match &b.kind {
                b"iinf" => iinf = Some(b),
                b"iloc" => iloc = Some(b),
                b"idat" => idat = Some(b),
                _ => {}
            }
        }
        let (iinf, iloc) = (iinf?, iloc?);
        if let Some(idat) = &idat {
            item_data.push((idat.content_start, idat.box_end));
        }

        let metadata_items = heif_metadata_item_ids(body.get(iinf.content_start..iinf.box_end)?)?;
        let extents = heif_item_extents(
            body.get(iloc.content_start..iloc.box_end)?,
            body.len(),
            idat.map(|b| b.content_start),
        )?;

        // Every guard up to here defends against MISREADING the file. This one
        // defends against a file read *correctly* that declares something
        // destructive: a metadata item whose extent covers another item's bytes.
        // Nothing above catches it — the lengths are valid, the offsets are in
        // range, the construction method is resolvable, and the whitelist does
        // not help because the item really is Exif. Zeroing it would punch a
        // hole in the picture, and for a photo-library backup the stored copy is
        // the only copy a restore returns, so an overlapping layout joins the
        // fail-safe set rather than being acted on.
        //
        // A metadata extent overlapping *another metadata* extent is refused for
        // the same reason: real writers do not emit one, so it is evidence the
        // layout was misread, and evidence is all we have. Content-on-content
        // overlap is none of our business — we never write there.
        //
        // And item-on-item is only half of it: an extent declared over
        // the file's OWN boxes — `ftyp`, the head of `meta` — overlaps no item
        // and would sail through. So the guard is also stated positively: a
        // metadata extent must lie wholly inside one item-data payload (an
        // `mdat`, or `idat`). Naming where metadata may live, rather than
        // listing the boxes it must avoid, leaves no unlisted box to slip past.
        let mut metadata_ranges: Vec<(usize, usize)> = Vec::new();
        let mut content_ranges: Vec<(usize, usize)> = Vec::new();
        for (item_id, start, len) in &extents {
            // An empty extent occupies nothing, so it can overlap nothing.
            if *len == 0 {
                continue;
            }
            let range = (*start, start.checked_add(*len)?);
            if metadata_items.contains(item_id) {
                if !item_data
                    .iter()
                    .any(|&(from, to)| from <= range.0 && range.1 <= to)
                {
                    return None;
                }
                metadata_ranges.push(range);
            } else {
                content_ranges.push(range);
            }
        }
        let intersects = |a: (usize, usize), b: (usize, usize)| a.0 < b.1 && b.0 < a.1;
        for (i, &m) in metadata_ranges.iter().enumerate() {
            if content_ranges.iter().any(|&other| intersects(m, other))
                || metadata_ranges[i + 1..]
                    .iter()
                    .any(|&other| intersects(m, other))
            {
                return None;
            }
        }

        let mut out = body.to_vec();
        for (item_id, start, len) in extents {
            if metadata_items.contains(&item_id) {
                out.get_mut(start..start.checked_add(len)?)?.fill(0);
            }
        }
        Some(out)
    }

    /// Read the `iinf` payload and return the ids of the items that are
    /// **metadata** — Exif, or XMP carried as a `mime` item.
    ///
    /// Deliberately a whitelist: every other item is *content* (the primary
    /// image, a thumbnail, an alpha plane, a derived image), and zeroing one
    /// punches a hole in the user's photo.
    fn heif_metadata_item_ids(iinf: &[u8]) -> Option<Vec<u32>> {
        let version = *iinf.first()?;
        let mut pos = 4usize; // version + flags
        let count = if version == 0 {
            let c = u16::from_be_bytes(iinf.get(pos..pos + 2)?.try_into().ok()?) as u32;
            pos += 2;
            c
        } else {
            let c = u32::from_be_bytes(iinf.get(pos..pos + 4)?.try_into().ok()?);
            pos += 4;
            c
        };

        let mut ids = Vec::new();
        for _ in 0..count {
            let b = read_bmff_box(iinf, pos, iinf.len())?;
            if &b.kind == b"infe"
                && let Some(id) = heif_metadata_item_id(iinf.get(b.content_start..b.box_end)?)
            {
                ids.push(id);
            }
            pos = b.box_end;
        }
        Some(ids)
    }

    /// Classify one `infe` (ItemInfoEntry) payload: `Some(item_id)` when it
    /// describes a metadata item, `None` when it is content or a version we
    /// cannot read.
    ///
    /// Versions 0 and 1 predate the `item_type` field entirely, so nothing in
    /// them identifies an Exif item — they yield `None`, which leaves the item's
    /// bytes alone. HEIF itself always writes version ≥ 2.
    fn heif_metadata_item_id(infe: &[u8]) -> Option<u32> {
        let version = *infe.first()?;
        let mut pos = 4usize; // version + flags
        if version < 2 {
            return None;
        }
        let item_id = if version == 2 {
            let v = u16::from_be_bytes(infe.get(pos..pos + 2)?.try_into().ok()?) as u32;
            pos += 2;
            v
        } else {
            let v = u32::from_be_bytes(infe.get(pos..pos + 4)?.try_into().ok()?);
            pos += 4;
            v
        };
        pos = pos.checked_add(2)?; // item_protection_index
        let item_type: [u8; 4] = infe.get(pos..pos + 4)?.try_into().ok()?;
        pos += 4;

        if &item_type == b"Exif" {
            return Some(item_id);
        }
        if &item_type == b"mime" {
            // `item_name` then `content_type`, both NUL-terminated UTF-8.
            let name_end = heif_skip_cstring(infe, pos)?;
            let type_end = heif_skip_cstring(infe, name_end)?;
            if infe.get(name_end..type_end.checked_sub(1)?)? == HEIF_XMP_CONTENT_TYPE {
                return Some(item_id);
            }
        }
        None
    }

    /// Offset just past the NUL terminating the string starting at `pos`.
    fn heif_skip_cstring(buf: &[u8], pos: usize) -> Option<usize> {
        let rel = buf.get(pos..)?.iter().position(|b| *b == 0)?;
        pos.checked_add(rel)?.checked_add(1)
    }

    /// Read the `iloc` payload into `(item_id, absolute_start, length)` triples.
    ///
    /// This is the intricate half, and every bit of the intricacy is
    /// version-dependent field widths — which is exactly how such a walker
    /// silently zeroes the wrong range:
    ///
    /// - `offset_size`/`length_size`/`base_offset_size`/`index_size` are 4-bit
    ///   nibbles naming a **byte width each** (0, 4 or 8 — anything else is
    ///   refused rather than guessed at).
    /// - `item_ID` is 16-bit in versions 0/1 and 32-bit in version 2.
    /// - The construction-method word exists only in versions 1 and 2.
    /// - `index_size` occupies the low nibble of the second byte only in
    ///   versions 1 and 2; in version 0 those bits are reserved.
    ///
    /// Construction method 0 means the offset is a position in this file and 1
    /// means it is relative to the `idat` box's payload — both resolvable, so
    /// both handled. Method 2 (offset into *another item*) is refused: resolving
    /// it means following a second item's extents, and a wrong answer writes
    /// zeroes into whatever that lands on.
    fn heif_item_extents(
        iloc: &[u8],
        file_len: usize,
        idat_start: Option<usize>,
    ) -> Option<Vec<(u32, usize, usize)>> {
        let version = *iloc.first()?;
        let mut pos = 4usize; // version + flags
        let sizes = *iloc.get(pos)?;
        let offset_size = (sizes >> 4) as usize;
        let length_size = (sizes & 0x0F) as usize;
        let next = *iloc.get(pos + 1)?;
        let base_offset_size = (next >> 4) as usize;
        let index_size = if version == 1 || version == 2 {
            (next & 0x0F) as usize
        } else {
            0
        };
        pos += 2;

        let item_count = if version < 2 {
            let c = u16::from_be_bytes(iloc.get(pos..pos + 2)?.try_into().ok()?) as u32;
            pos += 2;
            c
        } else {
            let c = u32::from_be_bytes(iloc.get(pos..pos + 4)?.try_into().ok()?);
            pos += 4;
            c
        };

        let mut out = Vec::new();
        for _ in 0..item_count {
            let item_id = if version < 2 {
                let v = u16::from_be_bytes(iloc.get(pos..pos + 2)?.try_into().ok()?) as u32;
                pos += 2;
                v
            } else {
                let v = u32::from_be_bytes(iloc.get(pos..pos + 4)?.try_into().ok()?);
                pos += 4;
                v
            };
            let construction_method = if version == 1 || version == 2 {
                let word = u16::from_be_bytes(iloc.get(pos..pos + 2)?.try_into().ok()?);
                pos += 2;
                (word & 0x0F) as u8
            } else {
                0
            };
            pos = pos.checked_add(2)?; // data_reference_index
            let base_offset = heif_uint(iloc, pos, base_offset_size)?;
            pos = pos.checked_add(base_offset_size)?;
            let extent_count = u16::from_be_bytes(iloc.get(pos..pos + 2)?.try_into().ok()?);
            pos += 2;

            // Where this item's offsets are measured from.
            let origin = match construction_method {
                0 => 0u64,
                1 => idat_start? as u64,
                // `item_offset` — refusing is the whole fail-safe contract.
                _ => return None,
            };

            for _ in 0..extent_count {
                pos = pos.checked_add(index_size)?;
                let extent_offset = heif_uint(iloc, pos, offset_size)?;
                pos = pos.checked_add(offset_size)?;
                let extent_length = heif_uint(iloc, pos, length_size)?;
                pos = pos.checked_add(length_size)?;

                let start = usize::try_from(
                    origin
                        .checked_add(base_offset)?
                        .checked_add(extent_offset)?,
                )
                .ok()?;
                let len = usize::try_from(extent_length).ok()?;
                // An extent past the end of the file means we misread the
                // layout — never clamp, never write.
                if start.checked_add(len)? > file_len {
                    return None;
                }
                out.push((item_id, start, len));
            }
        }
        Some(out)
    }

    /// Read a big-endian unsigned integer of `size` bytes. ISO/IEC 23008-12
    /// permits only 0 (field absent, value 0), 4 and 8; any other width means we
    /// misread the header, so it is refused rather than approximated.
    fn heif_uint(buf: &[u8], pos: usize, size: usize) -> Option<u64> {
        match size {
            0 => Some(0),
            4 => Some(u32::from_be_bytes(buf.get(pos..pos + 4)?.try_into().ok()?) as u64),
            8 => Some(u64::from_be_bytes(buf.get(pos..pos + 8)?.try_into().ok()?)),
            _ => None,
        }
    }

    /// Walk a GIF sub-block chain (`len` byte, `len` bytes of data, …, `0x00`)
    /// and return the offset just past its terminator.
    fn skip_gif_sub_blocks(body: &[u8], mut pos: usize) -> Option<usize> {
        loop {
            let len = *body.get(pos)? as usize;
            pos = pos.checked_add(1 + len)?;
            if pos > body.len() {
                return None;
            }
            if len == 0 {
                return Some(pos);
            }
        }
    }

    fn render_thumbnail(body: &[u8]) -> Option<Vec<u8>> {
        thumbnail_from_reader(image::ImageReader::new(std::io::Cursor::new(body)))
    }

    /// Render a JPEG thumbnail for the image at `path`, reading it
    /// **incrementally from disk** rather than loading the whole file into
    /// memory.
    ///
    /// `image::ImageReader::open` wraps a `std::io::BufReader<File>`, so the
    /// compressed source is streamed from disk and only the decoded pixel
    /// buffer (bounded by the dimension caps + the 512 MiB alloc cap below) is
    /// held in memory — never the whole file. This is what lets the device-sync
    /// **streaming** upload path (files ≥ 64 MiB) produce a thumbnail while
    /// staying O(decoded-thumbnail) memory instead of O(file). Same caps,
    /// ≤300px decline, and JPEG output as [`render_thumbnail`]; a missing /
    /// unreadable / undecodable file yields `None` (the upload still succeeds).
    pub(crate) fn render_thumbnail_from_path(path: &std::path::Path) -> Option<Vec<u8>> {
        thumbnail_from_reader(image::ImageReader::open(path).ok()?)
    }

    /// Shared thumbnail render over any seekable reader (an in-memory `Cursor`
    /// or a `BufReader<File>`), so the in-memory and from-path entrypoints agree
    /// on caps + output exactly (priority #4).
    ///
    /// Decodes untrusted bytes under explicit limits so a decompression bomb (a
    /// tiny source declaring enormous dimensions) is refused rather than
    /// allocating a giant pixel buffer (2026-06-01 review finding F): the
    /// image-crate default is only a 512 MiB alloc cap with NO dimension cap, so
    /// we add per-axis dimension limits (the cheap early reject) and keep the
    /// alloc cap. A refused / oversized / undecodable source yields `None`,
    /// exactly like any other decode failure — the upload itself still succeeds.
    fn thumbnail_from_reader<R: std::io::BufRead + std::io::Seek>(
        reader: image::ImageReader<R>,
    ) -> Option<Vec<u8>> {
        use image::GenericImageView;
        let mut reader = reader.with_guessed_format().ok()?;
        let mut limits = image::Limits::default(); // max_alloc = 512 MiB
        limits.max_image_width = Some(MAX_THUMBNAIL_SOURCE_DIM);
        limits.max_image_height = Some(MAX_THUMBNAIL_SOURCE_DIM);
        reader.limits(limits);
        let img = reader.decode().ok()?;
        let (w, h) = img.dimensions();
        if w <= 300 && h <= 300 {
            return None;
        }
        let thumb = img.thumbnail(300, 300);
        let mut buf = std::io::Cursor::new(Vec::new());
        thumb.write_to(&mut buf, image::ImageFormat::Jpeg).ok()?;
        Some(buf.into_inner())
    }
}

#[cfg(not(feature = "process_media"))]
mod stub {
    use super::ProcessedMedia;

    /// Identity-passthrough — see module docs for why this exists.
    pub fn process_media(raw: &[u8]) -> ProcessedMedia {
        ProcessedMedia {
            stripped_bytes: raw.to_vec(),
            mime: "application/octet-stream".to_string(),
            thumbnail_bytes: None,
            has_c2pa: false,
        }
    }

    /// Identity-passthrough counterpart to the real strip-only entry point.
    /// With the curated `process_media` feature off there is no `img-parts`,
    /// so there is nothing to parse the container with.
    pub fn strip_metadata(raw: &[u8]) -> Vec<u8> {
        raw.to_vec()
    }

    /// Stub counterpart to the outcome-carrying face. A stub build strips
    /// nothing, so the honest outcome is always the same residual
    /// [`strip_coverage`] reports — never `Stripped`.
    pub fn strip_metadata_with_coverage(raw: &[u8]) -> (Vec<u8>, super::StripCoverage) {
        (raw.to_vec(), strip_coverage(raw))
    }

    /// Honest coverage for a stub build: **nothing** is stripped, whatever the
    /// format. Reporting this rather than `Stripped` is the whole reason the
    /// coverage map is a value — a caller that trusts the strip can detect a
    /// build which silently does not perform one (the same failure mode
    /// `PROCESS_MEDIA_ENABLED` exists to catch, since this feature is usually
    /// switched on by unification from a sibling crate).
    pub fn strip_coverage(_raw: &[u8]) -> super::StripCoverage {
        super::StripCoverage::Residual(
            "this build compiled fauna-media without the `process_media` feature, so no container \
             is parsed and no metadata is removed",
        )
    }

    /// Stub counterpart to the real C2PA probe — with the curated
    /// `process_media` feature off there is no `c2pa` reader, so nothing is
    /// ever detected.
    pub fn detect_c2pa(_mime: &str, _raw: &[u8]) -> bool {
        false
    }

    /// Stub counterpart to the real from-path thumbnailer — with the curated
    /// `process_media` feature off there is no image codec, so no thumbnail.
    pub(crate) fn render_thumbnail_from_path(_path: &std::path::Path) -> Option<Vec<u8>> {
        None
    }
}
