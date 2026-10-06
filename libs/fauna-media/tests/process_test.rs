//! Tests for the real `process_media` (lifted from
//! `bins/fauna-nest/src/storage/plaintext.rs::process_plaintext_blob`).
//!
//! Fixtures are built programmatically with `image` + `img_parts` so the
//! repo carries no opaque binary blobs. Each test names what it asserts in
//! its body; helpers are at the bottom.
//!
//! Gated on the `process_media` feature — when off, `process_media` is the
//! identity stub and these behavioral assertions don't apply.

#![cfg(feature = "process_media")]

use fauna_media::process::{
    ProcessedMedia, StripCoverage, detect_c2pa, detect_c2pa_in_bytes, process_media,
    strip_coverage, strip_metadata, strip_metadata_with_coverage,
};
use fauna_media::test_fixtures::{build_png, build_rgb_image};

const EXIF_CANARY: &[u8] = b"canary-exif-data-do-not-leak";
const IPTC_CANARY: &[u8] = b"canary-iptc-data-do-not-leak";

// ── MIME sniff ──────────────────────────────────────────────────────────────

#[test]
fn sniff_mime_png() {
    let png = build_png(10, 10);
    let ProcessedMedia { mime, .. } = process_media(&png);
    assert_eq!(mime, "image/png");
}

#[test]
fn sniff_mime_jpeg() {
    let jpeg = build_jpeg(10, 10);
    let ProcessedMedia { mime, .. } = process_media(&jpeg);
    assert_eq!(mime, "image/jpeg");
}

#[test]
fn sniff_mime_webp() {
    let webp = build_webp(10, 10);
    let ProcessedMedia { mime, .. } = process_media(&webp);
    assert_eq!(mime, "image/webp");
}

#[test]
fn sniff_mime_gif() {
    // Minimal valid GIF87a header — sniff only inspects the first 4 bytes.
    let gif: Vec<u8> = b"GIF87a".to_vec();
    let ProcessedMedia { mime, .. } = process_media(&gif);
    assert_eq!(mime, "image/gif");
}

#[test]
fn sniff_mime_pdf() {
    let pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let ProcessedMedia { mime, .. } = process_media(&pdf);
    assert_eq!(mime, "application/pdf");
}

#[test]
fn sniff_mime_mp4() {
    // Minimal ISO-BMFF `ftyp` box header — sniff only inspects bytes 4..8 (the
    // 4-byte box-size field before it is never checked), matching every other
    // format-family video container (mov/m4a/3gp all share this box).
    let mp4: Vec<u8> = b"\x00\x00\x00\x18ftypisom".to_vec();
    let ProcessedMedia { mime, .. } = process_media(&mp4);
    assert_eq!(mime, "video/mp4");
}

#[test]
fn sniff_mime_heic_is_an_image_not_a_video() {
    // HEIC shares the ISO-BMFF `ftyp` box with mp4/mov, and until 2026-08-01
    // every ftyp container was reported as `video/mp4` — so an iPhone photo (the
    // camera default) went on the wire labelled a video, and was skipped by
    // every `mime.starts_with("image/")` branch here. The 4-byte major brand at
    // 8..12 is what separates them.
    for brand in [&b"heic"[..], &b"heix"[..], &b"mif1"[..], &b"msf1"[..]] {
        let mut heic: Vec<u8> = b"\x00\x00\x00\x18ftyp".to_vec();
        heic.extend_from_slice(brand);
        let ProcessedMedia { mime, .. } = process_media(&heic);
        assert_eq!(
            mime,
            "image/heic",
            "ISO-BMFF brand {} is a still image, not a video",
            String::from_utf8_lossy(brand)
        );
    }
}

#[test]
fn sniff_mime_avif_is_image_avif_not_heic() {
    // AVIF shares HEIF's item structure but not its codec, and the MIME is
    // load-bearing: browsers render `image/avif` but not `image/heic`. Until
    // 2026-08-01 the `avif`/`avis` brands were folded into the Heif variant and
    // went on the wire as `image/heic` — stored *and* served under a type no
    // browser can render, strictly worse than either rejecting or labelling
    // them right.
    for brand in [&b"avif"[..], &b"avis"[..]] {
        let mut avif: Vec<u8> = b"\x00\x00\x00\x18ftyp".to_vec();
        avif.extend_from_slice(brand);
        let ProcessedMedia { mime, .. } = process_media(&avif);
        assert_eq!(
            mime,
            "image/avif",
            "ISO-BMFF brand {} is AVIF, not HEIC",
            String::from_utf8_lossy(brand)
        );
    }
}

#[test]
fn sniff_mime_webm() {
    // WebM/Matroska EBML header signature.
    let webm: Vec<u8> = vec![0x1A, 0x45, 0xDF, 0xA3];
    let ProcessedMedia { mime, .. } = process_media(&webm);
    assert_eq!(mime, "video/webm");
}

#[test]
fn sniff_mime_unknown_falls_back_to_octet_stream() {
    let bytes: Vec<u8> = b"not a recognized format".to_vec();
    let ProcessedMedia { mime, .. } = process_media(&bytes);
    assert_eq!(mime, "application/octet-stream");
}

// ── EXIF / IPTC strip ───────────────────────────────────────────────────────

#[test]
fn strip_exif_removes_canary_from_png() {
    let png = build_png(10, 10);
    let with_exif = inject_png_exif_chunk(&png, EXIF_CANARY);
    assert!(
        contains(&with_exif, EXIF_CANARY),
        "fixture builder must embed the canary; otherwise the strip assertion is vacuous"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&with_exif);
    assert_eq!(mime, "image/png");
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must strip eXIf chunk contents from PNG"
    );
}

#[test]
fn strip_text_chunk_removes_canary_from_png() {
    // PNGs also carry tEXt/iTXt/zTXt — strip should remove those too.
    let png = build_png(10, 10);
    let with_text = inject_png_text_chunk(&png, EXIF_CANARY);
    assert!(
        contains(&with_text, EXIF_CANARY),
        "fixture must contain canary"
    );

    let ProcessedMedia { stripped_bytes, .. } = process_media(&with_text);
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must strip tEXt chunk from PNG"
    );
}

#[test]
fn strip_iptc_removes_canary_from_jpeg() {
    let jpeg = build_jpeg(10, 10);
    let with_iptc = inject_jpeg_app13(&jpeg, IPTC_CANARY);
    assert!(
        contains(&with_iptc, IPTC_CANARY),
        "fixture must contain canary"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&with_iptc);
    assert_eq!(mime, "image/jpeg");
    assert!(
        !contains(&stripped_bytes, IPTC_CANARY),
        "process_media must strip APP13 (IPTC) from JPEG"
    );
}

#[test]
fn strip_exif_removes_canary_from_jpeg() {
    let jpeg = build_jpeg(10, 10);
    let with_exif = inject_jpeg_app1(&jpeg, EXIF_CANARY);
    assert!(
        contains(&with_exif, EXIF_CANARY),
        "fixture must contain canary"
    );

    let ProcessedMedia { stripped_bytes, .. } = process_media(&with_exif);
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must strip APP1 (Exif) from JPEG"
    );
}

#[test]
fn strip_exif_removes_canary_from_webp() {
    // WebP was sniffed from the very first lift but never stripped — the strip's
    // `_ => body` catch-all swallowed it silently. A phone-shot WebP writes GPS
    // into exactly this RIFF `EXIF` chunk.
    let webp = build_webp(10, 10);
    let with_exif = inject_webp_chunk(&webp, *b"EXIF", EXIF_CANARY);
    assert!(
        contains(&with_exif, EXIF_CANARY),
        "fixture builder must embed the canary; otherwise the strip assertion is vacuous"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&with_exif);
    assert_eq!(mime, "image/webp");
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must strip the EXIF chunk from WebP"
    );
}

#[test]
fn strip_xmp_removes_canary_from_webp() {
    // The same coordinates ride WebP's `XMP ` chunk as XML — note the
    // significant trailing space, RIFF ids are always four bytes.
    let webp = build_webp(10, 10);
    let with_xmp = inject_webp_chunk(&webp, *b"XMP ", IPTC_CANARY);
    let ProcessedMedia { stripped_bytes, .. } = process_media(&with_xmp);
    assert!(
        !contains(&stripped_bytes, IPTC_CANARY),
        "process_media must strip the `XMP ` chunk from WebP"
    );
}

#[test]
fn strip_keeps_webp_image_payload() {
    // Removing metadata chunks must not disturb the compressed picture: the
    // stripped file still decodes, at its original dimensions.
    let webp = build_webp(64, 48);
    let stripped = strip_metadata(&inject_webp_chunk(&webp, *b"EXIF", EXIF_CANARY));
    let decoded = image::load_from_memory(&stripped).expect("stripped WebP must still decode");
    assert_eq!(
        image::GenericImageView::dimensions(&decoded),
        (64, 48),
        "strip must not resize or re-encode the WebP payload"
    );
}

#[test]
fn strip_removes_comment_extension_from_gif() {
    // GIF's Comment Extension (0x21 0xFE) is a free-text metadata carrier.
    let gif = build_gif(16, 16);
    let with_comment = inject_gif_extension(&gif, 0xFE, EXIF_CANARY);
    assert!(
        contains(&with_comment, EXIF_CANARY),
        "fixture builder must embed the canary; otherwise the strip assertion is vacuous"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&with_comment);
    assert_eq!(mime, "image/gif");
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must strip the Comment extension from GIF"
    );
}

#[test]
fn strip_removes_application_extension_from_gif() {
    // XMP — and therefore GPS — rides GIF's Application Extension (0x21 0xFF).
    let gif = build_gif(16, 16);
    let with_app = inject_gif_extension(&gif, 0xFF, IPTC_CANARY);
    let ProcessedMedia { stripped_bytes, .. } = process_media(&with_app);
    assert!(
        !contains(&stripped_bytes, IPTC_CANARY),
        "process_media must strip the Application extension from GIF"
    );
}

#[test]
fn strip_keeps_gif_image_payload() {
    // The hand-rolled GIF walker is the one stripped arm with no parser crate
    // behind it, so pin that it copies the picture through intact rather than
    // truncating the block stream.
    let gif = build_gif(32, 24);
    let stripped = strip_metadata(&inject_gif_extension(&gif, 0xFE, EXIF_CANARY));
    let decoded = image::load_from_memory(&stripped).expect("stripped GIF must still decode");
    assert_eq!(
        image::GenericImageView::dimensions(&decoded),
        (32, 24),
        "strip must not disturb the GIF image blocks"
    );
}

#[test]
fn strip_keeps_gif_graphic_control_extension() {
    // Only Comment + Application are metadata. Graphic Control (0xF9) is render
    // state — frame delay, transparency — and dropping it would change how the
    // image animates, which is a content change, not a privacy fix.
    let gif = build_gif(16, 16);
    assert!(
        contains(&gif, &[0x21, 0xF9]),
        "fixture must contain a Graphic Control extension for this to be meaningful"
    );
    let stripped = strip_metadata(&inject_gif_extension(&gif, 0xFE, EXIF_CANARY));
    assert!(
        contains(&stripped, &[0x21, 0xF9]),
        "strip must keep the Graphic Control extension"
    );
}

#[test]
fn strip_leaves_a_malformed_gif_untouched() {
    // Fail-safe direction: the backup ingress stores the copy a restore returns,
    // so a container we cannot walk must come back exactly as the user gave it
    // to us — never truncated at the point the walker gave up.
    let truncated = {
        let gif = build_gif(16, 16);
        gif[..gif.len() / 2].to_vec()
    };
    assert_eq!(
        strip_metadata(&truncated),
        truncated,
        "an unwalkable GIF must pass through byte-identical"
    );
}

#[test]
fn strip_removes_location_atom_from_mp4_udta() {
    // `moov/udta` is where QuickTime writes `©xyz` — the GPS coordinates a
    // phone stamps on every video it records.
    let mp4 = build_mp4_with_udta(EXIF_CANARY);
    assert!(
        contains(&mp4, EXIF_CANARY),
        "fixture builder must embed the canary; otherwise the strip assertion is vacuous"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&mp4);
    assert_eq!(mime, "video/mp4");
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must neutralise the udta location atom in an mp4"
    );
}

#[test]
fn strip_removes_xmp_uuid_box_from_mp4() {
    // The Adobe XMP `uuid` box carries the same coordinates as XML. Only that
    // specific UUID is touched — `uuid` is the generic vendor-extension box.
    let mp4 = build_mp4_with_xmp_uuid(IPTC_CANARY);
    let ProcessedMedia { stripped_bytes, .. } = process_media(&mp4);
    assert!(
        !contains(&stripped_bytes, IPTC_CANARY),
        "process_media must neutralise the XMP uuid box in an mp4"
    );
}

#[test]
fn strip_leaves_a_non_xmp_uuid_box_alone() {
    // A vendor extension we don't recognise may be something the player needs;
    // neutralising every `uuid` box would be the reckless reading of "strip
    // metadata". Only the XMP UUID is ours to remove.
    let mp4 = build_mp4_with_other_uuid(IPTC_CANARY);
    let stripped = strip_metadata(&mp4);
    assert!(
        contains(&stripped, IPTC_CANARY),
        "a non-XMP uuid box must survive the strip"
    );
}

#[test]
fn mp4_strip_preserves_every_byte_offset() {
    // THE load-bearing property, and the reason the box is neutralised in place
    // rather than removed: `stco`/`co64` chunk offsets point at absolute file
    // positions, so if the strip moved a single byte, every one of them would
    // silently address the wrong data. Length identical, `ftyp` untouched, and
    // the `mdat` payload byte-for-byte where it was.
    let mp4 = build_mp4_with_udta(EXIF_CANARY);
    let stripped = strip_metadata(&mp4);

    assert_eq!(
        stripped.len(),
        mp4.len(),
        "neutralisation must not change the file length — offsets depend on it"
    );
    let mdat_start = mp4
        .windows(4)
        .position(|w| w == b"mdat")
        .expect("fixture must contain an mdat box");
    assert_eq!(
        &stripped[mdat_start..],
        &mp4[mdat_start..],
        "the mdat payload must be byte-identical and at the same offset"
    );
    assert_eq!(
        &stripped[..12],
        &mp4[..12],
        "the ftyp box must be untouched"
    );
}

#[test]
fn mp4_strip_rewrites_the_box_to_free_rather_than_deleting_it() {
    // The neutralised box must still be *there*, as ignorable free space —
    // that is what keeps every later offset valid.
    let mp4 = build_mp4_with_udta(EXIF_CANARY);
    let stripped = strip_metadata(&mp4);
    assert!(
        contains(&mp4, b"udta") && !contains(&stripped, b"udta"),
        "the udta box type must be rewritten"
    );
    assert!(
        contains(&stripped, b"free"),
        "it must be rewritten to `free` (ignorable padding), not deleted"
    );
}

#[test]
fn strip_leaves_a_malformed_mp4_untouched() {
    // Fail-safe, same contract as the GIF walker: a box claiming a size past
    // the end of the file means our reading is wrong, so give the user's bytes
    // back rather than a partially-rewritten video.
    let mut mp4 = build_mp4_with_udta(EXIF_CANARY);
    let n = mp4.len();
    // Overstate the second top-level box's size.
    mp4[16..20].copy_from_slice(&((n as u32) * 4).to_be_bytes());
    assert_eq!(
        strip_metadata(&mp4),
        mp4,
        "an unwalkable mp4 must pass through byte-identical"
    );
}

// ── HEIC / HEIF / AVIF item-level strip ─────────────────────────────────────
//
// HEIC is the iPhone camera default, so it dominates a photo-library backup —
// and it is the one ISO-BMFF family the mp4 walker above must NOT be pointed
// at. In a video, a top-level `meta` box is metadata; in a HEIC it is
// *structural* — it holds the `iinf`/`iloc` that locate the picture itself, so
// `free`-ing it would destroy the image. Exif lives instead as an **item**
// whose bytes sit in `mdat`, reachable only by resolving `iinf` (which item is
// Exif) against `iloc` (where that item's bytes are). These tests pin the
// resolution, and — more importantly — pin that the *primary image* item comes
// back byte-identical.

#[test]
fn strip_removes_exif_item_from_heic() {
    let heic = build_heic_with_exif_item(EXIF_CANARY);
    assert!(
        contains(&heic, EXIF_CANARY),
        "fixture builder must embed the canary; otherwise the strip assertion is vacuous"
    );

    let ProcessedMedia {
        stripped_bytes,
        mime,
        ..
    } = process_media(&heic);
    assert_eq!(mime, "image/heic");
    assert!(
        !contains(&stripped_bytes, EXIF_CANARY),
        "process_media must zero the Exif item's extent in a HEIC"
    );
}

#[test]
fn strip_removes_xmp_item_from_heic() {
    // HEIF carries XMP as an item of type `mime` whose content_type is
    // `application/rdf+xml` — not as the `uuid` box mp4 uses. Same GPS, second
    // carrier; reaching it means parsing `infe`'s trailing strings.
    let heic = build_heic_with_xmp_item(IPTC_CANARY);
    let stripped = strip_metadata(&heic);
    assert!(
        !contains(&stripped, IPTC_CANARY),
        "the XMP item's extent must be zeroed too"
    );
}

#[test]
fn heic_strip_preserves_the_primary_image_and_every_byte_offset() {
    // THE corruption guard, and the reason this is item-level surgery rather
    // than the mp4 arm's box neutralisation. The assertion deliberately reads
    // the file a *second, independent way*: the fixture computed the primary
    // item's absolute offset itself, so finding those bytes unchanged at that
    // exact position does not depend on the walker having parsed `iloc`
    // correctly — which is the one mistake a hand-written `iloc` walker and a
    // hand-built fixture could otherwise share and go confidently green on.
    let heic = build_heic_with_exif_item(EXIF_CANARY);
    let stripped = strip_metadata(&heic);

    assert_eq!(
        stripped.len(),
        heic.len(),
        "zeroing an extent must not change the file length — every iloc offset depends on it"
    );
    let image_at = heic
        .windows(HEIC_PRIMARY_IMAGE.len())
        .position(|w| w == HEIC_PRIMARY_IMAGE)
        .expect("fixture must contain the primary image payload");
    assert_eq!(
        &stripped[image_at..image_at + HEIC_PRIMARY_IMAGE.len()],
        HEIC_PRIMARY_IMAGE,
        "the primary image item's extent must be byte-identical and at the same offset"
    );
    assert_eq!(
        &stripped[..12],
        &heic[..12],
        "the ftyp box must be untouched"
    );
    for structural in [&b"meta"[..], &b"iinf"[..], &b"iloc"[..], &b"pitm"[..]] {
        assert!(
            contains(&stripped, structural),
            "the {} box locates the image — it must survive intact",
            String::from_utf8_lossy(structural)
        );
    }
}

#[test]
fn heic_strip_reads_every_iloc_version() {
    // `iloc`'s field layout is version-dependent: v0 has no construction-method
    // word and a 16-bit item_ID, v1 adds the construction method, v2 widens
    // item_ID to 32 bits. Getting one right and the others wrong is the classic
    // way this walker silently zeroes the wrong bytes — so exercise all three,
    // asserting BOTH halves (canary gone, image intact) each time.
    for version in [0u8, 1, 2] {
        let heic = build_heic_with_exif_item_iloc_version(EXIF_CANARY, version);
        let stripped = strip_metadata(&heic);
        assert!(
            !contains(&stripped, EXIF_CANARY),
            "iloc v{version}: the Exif extent must be zeroed"
        );
        assert!(
            contains(&stripped, HEIC_PRIMARY_IMAGE),
            "iloc v{version}: the primary image must survive"
        );
        assert_eq!(
            stripped.len(),
            heic.len(),
            "iloc v{version}: length must hold"
        );
    }
}

#[test]
fn heic_strip_honours_a_base_offset() {
    // `base_offset` is added to every extent offset of its item. A walker that
    // ignores it reads (and zeroes) the wrong range — which in a real photo is
    // a hole punched in the picture, not a missed strip.
    let heic = build_heic_with_exif_item_base_offset(EXIF_CANARY);
    let stripped = strip_metadata(&heic);
    assert!(
        !contains(&stripped, EXIF_CANARY),
        "the base_offset must be added to the extent offset"
    );
    assert!(
        contains(&stripped, HEIC_PRIMARY_IMAGE),
        "the primary image must survive a base_offset file"
    );
}

#[test]
fn heic_strip_leaves_a_non_metadata_item_alone() {
    // Only `Exif` and the XMP `mime` item are ours. An item of some other type
    // is content — a thumbnail, an alpha plane, a derived image — and zeroing
    // it destroys part of the user's photo.
    let heic = build_heic_with_unknown_item(IPTC_CANARY);
    let stripped = strip_metadata(&heic);
    assert!(
        contains(&stripped, IPTC_CANARY),
        "an item whose type is neither Exif nor XMP must survive untouched"
    );
}

#[test]
fn heic_strip_refuses_a_metadata_extent_that_overlaps_the_primary_image() {
    // Every other fail-safe in the HEIF arm guards against
    // the walker MISREADING the file; this one guards against a file that is
    // read CORRECTLY and declares something destructive — a well-formed `Exif`
    // item whose extent covers the picture. The whitelist cannot help: the item
    // really is type `Exif`. The realistic writer is not an attacker but a buggy
    // camera app or transcoder, and HEIF's extent model makes that more
    // plausible than it sounds. What raises it above a curiosity is where the
    // output goes: a photo-library backup stores the zeroed copy, and that is
    // the only copy a restore returns.
    let heic = build_heic_with_overlapping_exif_extent(EXIF_CANARY);
    // Located by content, independently of the walker's own `iloc` parse — the
    // same technique `heic_strip_preserves_the_primary_image_and_every_byte_offset`
    // uses, and for the same reason.
    let image_at = heic
        .windows(HEIC_PRIMARY_IMAGE.len())
        .position(|w| w == HEIC_PRIMARY_IMAGE)
        .expect("fixture must contain the primary image payload");

    let (out, coverage) = strip_metadata_with_coverage(&heic);

    assert_eq!(
        &out[image_at..image_at + HEIC_PRIMARY_IMAGE.len()],
        HEIC_PRIMARY_IMAGE,
        "the primary image was ZEROED. The Exif item's extent overlapped it, every \
         structural check passed (lengths valid, offsets in range, construction_method 0), \
         and the whitelist did not help because the item really is type Exif"
    );
    assert_eq!(
        out, heic,
        "an overlapping extent must fail safe like every other surprise — the user's \
         bytes back byte-identical, nothing guessed at"
    );
    match coverage {
        StripCoverage::Residual(reason) => assert!(
            !reason.is_empty(),
            "a bail must state that nothing was removed"
        ),
        other => panic!(
            "the walker declined to strip, but the OUTCOME reported {other:?}. \
             A caller cannot distinguish 'stripped' from 'declined to touch'."
        ),
    }

    // Positive control, same builder: without the overlap the strip must still
    // happen. Without this the guard could be satisfied by refusing every HEIC.
    let ordinary = build_heic_with_exif_item(EXIF_CANARY);
    let (stripped, coverage) = strip_metadata_with_coverage(&ordinary);
    assert!(
        !contains(&stripped, EXIF_CANARY),
        "the ordinary non-overlapping Exif item must still be zeroed — a guard that \
         refuses everything is not a guard"
    );
    assert_eq!(
        coverage,
        StripCoverage::Stripped,
        "and the ordinary file must still report a real strip"
    );
    assert!(
        contains(&stripped, HEIC_PRIMARY_IMAGE),
        "the primary image must survive the ordinary strip"
    );
}

#[test]
fn heic_strip_does_not_refuse_an_empty_metadata_extent() {
    // The narrow half of the guard, pinned because it is a branch and
    // an unpinned branch is how a fail-safe quietly becomes a refuse-everything.
    // A zero-length extent occupies no bytes, so it can destroy nothing — but
    // half-open interval arithmetic on its own would still place `(s, s)` inside
    // an enclosing content range and refuse the file. Refusing here would cost a
    // real strip on a file we understand perfectly well, so the guard skips it.
    let heic = build_heic_with_empty_exif_extent(EXIF_CANARY);
    let (out, coverage) = strip_metadata_with_coverage(&heic);

    assert_eq!(
        coverage,
        StripCoverage::Stripped,
        "an empty extent overlaps nothing — refusing here is the guard over-firing, \
         not failing safe"
    );
    assert!(
        contains(&out, HEIC_PRIMARY_IMAGE),
        "and the primary image must survive regardless"
    );
}

#[test]
fn heic_strip_refuses_a_metadata_extent_declared_over_the_files_own_boxes() {
    // The overlap guard above compares a metadata extent with
    // other ITEMS' extents only — so an `Exif` extent declared over the file's
    // own structure passed it and was zeroed: over `ftyp` the file no longer
    // sniffs as an image, over the head of `meta` the item table is gone. Both
    // leave a photo the backup stores and a restore returns undecodable. The
    // guard is therefore stated positively — a metadata extent must lie wholly
    // inside an item-data payload (`mdat`, or `idat`) — rather than as a list of
    // boxes to avoid, which the next box nobody listed would slip past.
    let over_ftyp = build_heic_inner(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(EXIF_CANARY),
        1,
        None,
        // `ftyp` is the file's first 24 bytes in this builder.
        Some(|_| (0, 24)),
    );
    let over_meta_head = build_heic_inner(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(EXIF_CANARY),
        1,
        None,
        // `meta` follows `ftyp`: its header, FullBox word, and `hdlr`'s header.
        Some(|_| (24, 16)),
    );
    assert_eq!(
        &over_ftyp[4..8],
        b"ftyp",
        "fixture: ftyp must open the file"
    );
    assert_eq!(
        &over_meta_head[28..32],
        b"meta",
        "fixture: meta must follow ftyp"
    );

    for (label, heic) in [("ftyp", over_ftyp), ("the head of meta", over_meta_head)] {
        let (out, coverage) = strip_metadata_with_coverage(&heic);
        assert_eq!(
            out, heic,
            "an Exif extent declared over {label} was acted on — the file's own \
             structure was zeroed, leaving a photo no reader can open"
        );
        match coverage {
            StripCoverage::Residual(reason) => assert!(
                !reason.is_empty(),
                "a bail must state that nothing was removed"
            ),
            other => {
                panic!("{label}: the walker declined to strip, but the OUTCOME reported {other:?}")
            }
        }
    }
}

#[test]
fn heic_strip_zeroes_an_exif_item_stored_in_idat() {
    // The positive control for the containment guard's second arm: an
    // item stored with `construction_method` 1 lives in `meta/idat`, not `mdat`,
    // and its extent is measured from the `idat` payload. A guard that accepted
    // only `mdat` would refuse every such file — a fail-safe quietly turned
    // refuse-everything, reporting a residual on a file we understand.
    let heic = build_heic_with_exif_item_in_idat(EXIF_CANARY);
    assert!(
        contains(&heic, EXIF_CANARY),
        "fixture builder must embed the canary"
    );
    let (out, coverage) = strip_metadata_with_coverage(&heic);
    assert_eq!(
        coverage,
        StripCoverage::Stripped,
        "an Exif item inside idat is a file we understand — it must be stripped"
    );
    assert!(
        !contains(&out, EXIF_CANARY),
        "the idat-hosted Exif item's bytes must be zeroed"
    );
    assert!(
        contains(&out, HEIC_PRIMARY_IMAGE),
        "the primary image must survive"
    );
    assert_eq!(out.len(), heic.len(), "zeroing must not change the length");
    assert!(contains(&out, b"idat"), "the idat box header must survive");
}

#[test]
fn strip_leaves_a_malformed_heic_untouched() {
    // Fail-safe, the same contract as the GIF and mp4 walkers — and it matters
    // most here: for a photo backup the stored copy is the only copy a restore
    // returns, so a half-understood file is given back exactly as received.
    let mut heic = build_heic_with_exif_item(EXIF_CANARY);
    let n = heic.len();
    // Overstate the `meta` box's size. Its 4-byte size field is the word
    // immediately before the fourcc — derived, not hardcoded, because this
    // fixture's `ftyp` is a different length from the mp4 one and an offset
    // guessed from that sibling test lands in the brand list instead, leaving
    // the file perfectly walkable and the assertion vacuous.
    let meta_at = heic
        .windows(4)
        .position(|w| w == b"meta")
        .expect("fixture must contain a meta box");
    heic[meta_at - 4..meta_at].copy_from_slice(&((n as u32) * 4).to_be_bytes());
    assert_eq!(
        strip_metadata(&heic),
        heic,
        "an unwalkable HEIC must pass through byte-identical"
    );
}

#[test]
fn heic_without_a_meta_box_passes_through_untouched() {
    // A bare `ftyp` stub has no item structure at all — nothing to locate, so
    // nothing to zero, and certainly nothing to guess at.
    let stub = heic_fixture();
    assert_eq!(strip_metadata(&stub), stub);
}

// ── Declared coverage ───────────────────────────────────────────────────────

#[test]
fn strip_coverage_reports_the_exact_stripped_set() {
    // The executable half of `file-sync.md` § Ingress metadata-strip
    // convergence. If an arm is added or lost, this fails and the goal doc's
    // table is updated in the same change — the drift that produced the
    // 2026-07-23 finding is what this exists to prevent.
    for (label, bytes) in [
        ("jpeg", build_jpeg(16, 16)),
        ("png", build_png(16, 16)),
        ("webp", build_webp(16, 16)),
        ("gif", build_gif(16, 16)),
        ("mp4", build_mp4_with_udta(EXIF_CANARY)),
        ("heic", build_heic_with_exif_item(EXIF_CANARY)),
    ] {
        assert_eq!(
            strip_coverage(&bytes),
            StripCoverage::Stripped,
            "{label} is documented as stripped"
        );
    }
}

#[test]
fn declared_residual_formats_pass_through_carrying_their_metadata() {
    // These are the honest half: formats we recognise, do NOT strip, and say so.
    // The assertion is deliberately two-sided — the bytes survive untouched
    // (never mangled) *and* coverage reports a residual with a stated reason, so
    // no caller can read a successful return as "the metadata is gone".
    // HEIC left this list 2026-08-01 — it is stripped now, asserted above.
    for (label, bytes) in [
        ("webm", vec![0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x02]),
        ("pdf", b"%PDF-1.4\ntrailer".to_vec()),
    ] {
        match strip_coverage(&bytes) {
            StripCoverage::Residual(reason) => assert!(
                !reason.is_empty(),
                "{label}: a residual must state why it is not stripped"
            ),
            other => panic!("{label} is a declared residual, got {other:?}"),
        }
        assert_eq!(
            strip_metadata(&bytes),
            bytes,
            "{label}: a residual format must pass through byte-identical, not be mangled"
        );
    }
}

#[test]
fn unrecognised_bytes_report_a_residual_never_an_absence_of_metadata() {
    // This replaced `unidentified_bytes_report_no_carrier_rather_than_a_residual`,
    // whose input was a genuinely carrier-free string — which made it vacuous
    // as a claim about *unrecognised* bytes. Failing to parse bytes is not
    // evidence about what they contain, and the format family the old claim was
    // most wrong about is the one that defines Exif.
    //
    // A DNG is a TIFF (`II*\0`, little-endian magic 42). The sniff has no TIFF
    // branch, so Apple ProRAW and Android RAW land in `Unknown` carrying a full
    // Exif block — and the old mapping affirmatively reported they carried none.
    let mut dng = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
    dng.extend_from_slice(EXIF_CANARY);
    match strip_coverage(&dng) {
        StripCoverage::Residual(reason) => assert!(
            !reason.is_empty(),
            "an unrecognised container must state why its coverage is unknown"
        ),
        other => panic!(
            "a TIFF/DNG carrying Exif must not be reported as covered; got {other:?}. \
             The sniff has no TIFF branch, so this is the DEFAULT answer for every \
             unrecognised metadata-bearing format, not a special case."
        ),
    }
    // The bytes themselves are still returned untouched — the fail-safe
    // direction is unchanged, only the claim about them.
    assert_eq!(
        strip_metadata(&dng),
        dng,
        "unrecognised bytes must pass through byte-identical"
    );
}

// ── Composite files: a JPEG is not always only a JPEG ───────────────────────

#[test]
fn control_standalone_motion_video_is_stripped() {
    // The control that makes the next test's failure diagnostic: the SAME video,
    // standalone, is stripped correctly. Without it, a red there could mean
    // "the mp4 arm is broken" rather than "the composite was never walked".
    let video = build_mp4_with_udta(EXIF_CANARY);
    assert!(
        contains(&video, EXIF_CANARY),
        "fixture must embed the canary"
    );

    let (out, coverage) = strip_metadata_with_coverage(&video);
    assert!(
        !contains(&out, EXIF_CANARY),
        "the standalone video's udta location must be neutralised"
    );
    assert_eq!(coverage, StripCoverage::Stripped);
}

#[test]
fn a_motion_photo_loses_the_location_in_its_appended_video_too() {
    // Google/Samsung "Motion Photo" is a single .jpg with a complete mp4
    // appended after EOI; MediaStore reports it as image/jpeg, so android's
    // PhotoBackupEngine hands the whole file to the strip. Dispatching on the
    // OUTER container alone stripped the still and walked past the video's
    // moov/udta/(c)xyz — while coverage reported `Stripped`. That is the default
    // camera output of a large share of Android devices, on the ingress the user
    // guides describe as removing location.
    //
    // Two canaries, so a half-fix cannot pass: one in the still's Exif, a
    // different one in the appended video's location atom.
    let still = inject_jpeg_app1(&build_jpeg(10, 10), EXIF_CANARY);
    let video = build_mp4_with_udta(IPTC_CANARY);
    let mut motion_photo = still.clone();
    motion_photo.extend_from_slice(&video);

    assert!(contains(&motion_photo, EXIF_CANARY), "still canary missing");
    assert!(contains(&motion_photo, IPTC_CANARY), "video canary missing");

    let (out, coverage) = strip_metadata_with_coverage(&motion_photo);
    assert!(
        !contains(&out, EXIF_CANARY),
        "the still half's Exif must still be stripped"
    );
    assert!(
        !contains(&out, IPTC_CANARY),
        "the APPENDED VIDEO kept its location through the strip — the composite \
         was not walked. This is the finding: a silent success on a file that \
         still carries its GPS."
    );
    assert_eq!(
        coverage,
        StripCoverage::Stripped,
        "both halves were stripped, so the composite is stripped"
    );
}

#[test]
fn a_fill_byte_before_eoi_does_not_hide_the_appended_video() {
    // JPEG allows any number of `0xFF` fill bytes
    // before a marker, so a still may end `… FF FF D9`. The end-of-still walk
    // stepped over `FF FF` two bytes at a time, landing past the EOI's `FF` and
    // running on into the appended mp4 — so the split never happened, the
    // video's location atom was never walked, and coverage said `Stripped`.
    let still = inject_jpeg_app1(&build_jpeg(10, 10), EXIF_CANARY);
    assert_eq!(
        &still[still.len() - 2..],
        [0xFF, 0xD9],
        "fixture must end in EOI"
    );
    let mut motion_photo = still[..still.len() - 2].to_vec();
    motion_photo.extend_from_slice(&[0xFF, 0xFF, 0xD9]); // one fill byte before EOI
    motion_photo.extend_from_slice(&build_mp4_with_udta(IPTC_CANARY));

    let (out, coverage) = strip_metadata_with_coverage(&motion_photo);
    assert!(
        !contains(&out, IPTC_CANARY),
        "the APPENDED VIDEO kept its location — a fill byte before EOI hid the \
         composite from the split"
    );
    assert!(
        !contains(&out, EXIF_CANARY),
        "the still half's Exif must be stripped too"
    );
    assert_eq!(coverage, StripCoverage::Stripped);
}

#[test]
fn a_composite_whose_appended_half_is_a_residual_says_so() {
    // The other side of the contract: when the appended container is one we
    // do NOT strip, the answer must be a residual — never a silent `Stripped` on
    // a file that kept its metadata. The still half is still stripped.
    let still = inject_jpeg_app1(&build_jpeg(10, 10), EXIF_CANARY);
    let mut composite = still.clone();
    composite.extend_from_slice(b"%PDF-1.4\ntrailer"); // a declared residual

    let (out, coverage) = strip_metadata_with_coverage(&composite);
    assert!(
        !contains(&out, EXIF_CANARY),
        "the still half is stripped even when the appended half is not"
    );
    match coverage {
        StripCoverage::Residual(reason) => assert!(
            !reason.is_empty(),
            "a composite with an unstripped half must state why"
        ),
        other => panic!("expected a residual for an appended PDF, got {other:?}"),
    }
}

#[test]
fn an_ordinary_jpeg_is_unaffected_by_the_composite_split() {
    // The split must not perturb the overwhelmingly common case. A plain JPEG
    // has no trailer, so it takes exactly the path it always did.
    let jpeg = inject_jpeg_app1(&build_jpeg(24, 24), EXIF_CANARY);
    let (out, coverage) = strip_metadata_with_coverage(&jpeg);
    assert_eq!(coverage, StripCoverage::Stripped);
    assert!(!contains(&out, EXIF_CANARY));
    assert_eq!(
        out,
        strip_metadata(&jpeg),
        "the two entry points must not diverge"
    );
}

// ── Outcome vs. capability ──────────────────────────────────────────────────

#[test]
fn a_bailed_strip_reports_a_residual_even_though_the_format_is_covered() {
    // Every arm is fail-safe, so for exactly the files the walker
    // declined to touch, the STATIC claim still says `Stripped`. The two values
    // answer different questions and this pins the difference: same bytes,
    // `strip_coverage` says the format is covered, the outcome says nothing was
    // removed.
    // The same input `strip_leaves_a_malformed_mp4_untouched` uses: a real mp4
    // whose second top-level box overstates its size past EOF, so the walker
    // knows its reading is wrong and returns the user's bytes.
    let malformed = {
        let mut mp4 = build_mp4_with_udta(EXIF_CANARY);
        let n = mp4.len();
        mp4[16..20].copy_from_slice(&((n as u32) * 4).to_be_bytes());
        mp4
    };

    assert_eq!(
        strip_coverage(&malformed),
        StripCoverage::Stripped,
        "the CAPABILITY claim is about the format, and mp4 is covered"
    );

    let (out, coverage) = strip_metadata_with_coverage(&malformed);
    assert_eq!(
        out, malformed,
        "a bail must return the user's bytes untouched — that direction is load-bearing"
    );
    match coverage {
        StripCoverage::Residual(reason) => assert!(
            !reason.is_empty(),
            "a bail must state that nothing was removed"
        ),
        other => panic!(
            "the walker bailed and removed nothing, but the OUTCOME reported {other:?}. \
             A caller cannot distinguish 'stripped' from 'declined to touch'."
        ),
    }
}

#[test]
fn a_jpeg_the_parser_rejects_reports_a_residual() {
    // The JPEG arm is the one the bail fix missed: when the
    // parser rejects the still its bytes come back untouched — the ratified
    // fail-safe — but the arm fell through to `Stripped`, so a file that kept
    // its Exif was reported as having none. Every other arm reports a residual.
    let jpeg = inject_jpeg_app1(&build_jpeg(10, 10), EXIF_CANARY);
    // Overstate the length of the segment right AFTER the APP1 (which the
    // injector places straight after SOI) so it runs past the end of the file.
    let app1_len = u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
    let next = 2 + 2 + app1_len;
    assert_eq!(jpeg[next], 0xFF, "fixture: a marker must follow APP1");
    let mut overlong = jpeg.clone();
    overlong[next + 2..next + 4].copy_from_slice(&0xFFF0u16.to_be_bytes());

    let (out, coverage) = strip_metadata_with_coverage(&overlong);
    assert_eq!(
        out, overlong,
        "a JPEG the parser rejects must come back untouched"
    );
    match coverage {
        StripCoverage::Residual(reason) => assert!(
            !reason.is_empty(),
            "a bail must state that nothing was removed"
        ),
        other => panic!(
            "the parser rejected the JPEG and nothing was removed, but the OUTCOME \
             reported {other:?} — the Exif canary is still in the bytes"
        ),
    }
}

// ── strip_metadata (the strip-only entry point) ─────────────────────────────
//
// `strip_metadata` is what a *backup ingress* calls: it strips the same
// segments/chunks as `process_media` but skips the thumbnail render and C2PA
// probe, which an ingress copying a whole photo library doesn't need and
// can't afford per-asset. The losslessness assertions below are the point of
// the whole entry point — every hand-rolled client stripper this replaces
// (apple ImageIO, android Bitmap q95, windows BitmapEncoder, web Canvas)
// decodes and re-encodes, permanently degrading the backed-up copy.

#[test]
fn strip_metadata_removes_exif_from_jpeg() {
    let jpeg = build_jpeg(10, 10);
    let with_exif = inject_jpeg_app1(&jpeg, EXIF_CANARY);
    assert!(
        contains(&with_exif, EXIF_CANARY),
        "fixture must contain canary"
    );

    let stripped = strip_metadata(&with_exif);
    assert!(
        !contains(&stripped, EXIF_CANARY),
        "strip_metadata must strip APP1 (Exif) from JPEG"
    );
}

#[test]
fn strip_metadata_removes_iptc_from_jpeg() {
    let jpeg = build_jpeg(10, 10);
    let with_iptc = inject_jpeg_app13(&jpeg, IPTC_CANARY);
    assert!(
        contains(&with_iptc, IPTC_CANARY),
        "fixture must contain canary"
    );

    let stripped = strip_metadata(&with_iptc);
    assert!(
        !contains(&stripped, IPTC_CANARY),
        "strip_metadata must strip APP13 (IPTC) from JPEG"
    );
}

#[test]
fn strip_metadata_removes_exif_and_text_chunks_from_png() {
    let png = build_png(10, 10);
    let with_exif = inject_png_exif_chunk(&png, EXIF_CANARY);
    let with_text = inject_png_text_chunk(&png, IPTC_CANARY);

    assert!(
        !contains(&strip_metadata(&with_exif), EXIF_CANARY),
        "strip_metadata must strip the eXIf chunk from PNG"
    );
    assert!(
        !contains(&strip_metadata(&with_text), IPTC_CANARY),
        "strip_metadata must strip tEXt/iTXt/zTXt chunks from PNG"
    );
}

#[test]
fn strip_metadata_is_lossless_when_there_is_nothing_to_strip() {
    // The defining property: a metadata-free image comes back byte-identical.
    // A decode/re-encode stripper cannot pass this — which is exactly why the
    // five per-app strippers are being retired in favour of this one.
    for (label, original) in [
        ("jpeg", build_jpeg(64, 48)),
        ("png", build_png(64, 48)),
        ("webp", build_webp(64, 48)),
        ("gif", build_gif(64, 48)),
    ] {
        let stripped = strip_metadata(&original);
        assert_eq!(
            stripped, original,
            "{label}: strip_metadata must not re-encode an image that carries no metadata"
        );
    }
}

#[test]
fn strip_metadata_restores_the_exact_pre_injection_bytes() {
    // Stronger than "the canary is gone": stripping an injected image must
    // yield precisely the bytes we started from, proving the compressed image
    // data survived untouched rather than being decoded and re-compressed.
    let jpeg = build_jpeg(64, 48);
    let stripped = strip_metadata(&inject_jpeg_app1(&jpeg, EXIF_CANARY));
    assert_eq!(
        stripped, jpeg,
        "stripping an APP1-injected JPEG must reproduce the original JPEG byte-for-byte"
    );

    let png = build_png(64, 48);
    let stripped_png = strip_metadata(&inject_png_exif_chunk(&png, EXIF_CANARY));
    assert_eq!(
        stripped_png, png,
        "stripping an eXIf-injected PNG must reproduce the original PNG byte-for-byte"
    );

    let webp = build_webp(64, 48);
    let stripped_webp = strip_metadata(&inject_webp_chunk(&webp, *b"EXIF", EXIF_CANARY));
    assert_eq!(
        stripped_webp, webp,
        "stripping an EXIF-injected WebP must reproduce the original WebP byte-for-byte"
    );

    let gif = build_gif(64, 48);
    let stripped_gif = strip_metadata(&inject_gif_extension(&gif, 0xFE, EXIF_CANARY));
    assert_eq!(
        stripped_gif, gif,
        "stripping a Comment-injected GIF must reproduce the original GIF byte-for-byte"
    );
}

#[test]
fn strip_metadata_passes_through_non_images() {
    // A backup ingress hands us every file type; anything we don't understand
    // must survive untouched rather than being mangled or emptied. `GIF87a` is
    // a header with no block stream behind it — recognised by the sniff, then
    // rejected by the walker, which is the fail-safe path.
    for bytes in [
        b"not a recognized format".to_vec(),
        b"GIF87a".to_vec(),
        Vec::new(),
    ] {
        assert_eq!(
            strip_metadata(&bytes),
            bytes,
            "strip_metadata must pass unparseable bytes through unchanged"
        );
    }
}

#[test]
fn strip_metadata_agrees_with_process_media() {
    // Single-owner guard: `process_media` delegates its strip step to
    // `strip_metadata`, so the two can never drift into stripping different
    // things. If this fails, one of them grew a case the other lacks.
    let cases = vec![
        inject_jpeg_app1(&build_jpeg(32, 32), EXIF_CANARY),
        inject_jpeg_app13(&build_jpeg(32, 32), IPTC_CANARY),
        inject_png_exif_chunk(&build_png(32, 32), EXIF_CANARY),
        inject_png_text_chunk(&build_png(32, 32), IPTC_CANARY),
        inject_webp_chunk(&build_webp(32, 32), *b"EXIF", EXIF_CANARY),
        inject_gif_extension(&build_gif(32, 32), 0xFE, EXIF_CANARY),
        build_webp(32, 32),
        heic_fixture(),
        b"\x00\x00\x00\x18ftypisom".to_vec(),
        b"not an image at all".to_vec(),
    ];
    for raw in cases {
        assert_eq!(
            strip_metadata(&raw),
            process_media(&raw).stripped_bytes,
            "strip_metadata and process_media must strip identically"
        );
    }
}

// ── Thumbnail render ────────────────────────────────────────────────────────

#[test]
fn thumbnail_emitted_for_large_image() {
    let png = build_png(800, 600);
    let ProcessedMedia {
        thumbnail_bytes, ..
    } = process_media(&png);
    let thumb = thumbnail_bytes.expect("large image must produce a thumbnail");

    // Decode and check dimensions don't exceed 300×300 (matches plaintext.rs).
    let decoded = image::load_from_memory(&thumb).expect("thumbnail must be a decodable image");
    let (w, h) = image::GenericImageView::dimensions(&decoded);
    assert!(
        w <= 300 && h <= 300,
        "thumbnail dims {w}×{h} must be ≤ 300×300"
    );
    assert!(w > 0 && h > 0, "thumbnail must have non-zero dims");
}

#[test]
fn no_thumbnail_for_small_image() {
    let png = build_png(200, 200);
    let ProcessedMedia {
        thumbnail_bytes, ..
    } = process_media(&png);
    assert!(
        thumbnail_bytes.is_none(),
        "images ≤ 300×300 must not produce a thumbnail"
    );
}

#[test]
fn no_thumbnail_for_non_image() {
    let bytes: Vec<u8> = b"not an image".to_vec();
    let ProcessedMedia {
        thumbnail_bytes,
        mime,
        ..
    } = process_media(&bytes);
    assert_eq!(mime, "application/octet-stream");
    assert!(
        thumbnail_bytes.is_none(),
        "non-image must not produce a thumbnail"
    );
}

#[test]
fn no_thumbnail_for_oversized_image_dimension_guard() {
    // An image whose declared dimensions exceed the per-axis decode cap is
    // refused (decompression-bomb guard, security review finding F): no
    // thumbnail, and process_media still returns normally — the upload is
    // unaffected. 17000 > the 16384 cap; the source bytes stay tiny (17000×1),
    // so this exercises the dimension limit specifically (not raw allocation).
    let png = build_png(17_000, 1);
    let ProcessedMedia {
        thumbnail_bytes,
        mime,
        ..
    } = process_media(&png);
    assert_eq!(mime, "image/png");
    assert!(
        thumbnail_bytes.is_none(),
        "an image past the decode dimension cap must not produce a thumbnail"
    );
}

// ── C2PA detection ──────────────────────────────────────────────────────────

#[test]
fn c2pa_detection_negative_for_plain_jpeg() {
    let jpeg = build_jpeg(50, 50);
    let ProcessedMedia { has_c2pa, .. } = process_media(&jpeg);
    assert!(!has_c2pa, "plain JPEG must not be flagged as C2PA-bearing");
}

#[test]
fn c2pa_detection_negative_for_plain_png() {
    let png = build_png(50, 50);
    let ProcessedMedia { has_c2pa, .. } = process_media(&png);
    assert!(!has_c2pa, "plain PNG must not be flagged as C2PA-bearing");
}

#[test]
fn c2pa_detection_skipped_for_non_image() {
    let bytes: Vec<u8> = b"not an image".to_vec();
    let ProcessedMedia { has_c2pa, .. } = process_media(&bytes);
    assert!(!has_c2pa, "non-image must not be flagged as C2PA-bearing");
}

// Positive C2PA detection is the additive `c2pa-detect` layer: a curated build
// (`process_media` alone — e.g. a lean web bundle) always reports
// `has_c2pa = false`, so the negatives above still hold but this positive only
// applies when `c2pa-detect` is on.
#[cfg(feature = "c2pa-detect")]
#[test]
fn c2pa_detection_positive_for_signed_png() {
    // Real C2PA-signed PNG fixture (kept in tests/fixtures/ at repo root —
    // generated once with c2patool, then committed).
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    let ProcessedMedia { mime, has_c2pa, .. } = process_media(signed_png);
    assert_eq!(mime, "image/png");
    assert!(has_c2pa, "C2PA-signed PNG must be detected");
}

// `detect_c2pa` is the probe-only half `process_media` now calls internally
// (`attachments_to_inbound`'s receive-time use — a decrypted attachment has
// already been stripped/sealed by its sender, so the receiver probes the raw
// bytes directly rather than re-running the full pipeline). Same three
// readings as the `process_media`-mediated tests above, called directly.

#[test]
fn detect_c2pa_negative_for_plain_jpeg() {
    let jpeg = build_jpeg(50, 50);
    assert!(
        !detect_c2pa("image/jpeg", &jpeg),
        "plain JPEG must not be flagged as C2PA-bearing"
    );
}

#[test]
fn detect_c2pa_skipped_for_non_image_mime() {
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    assert!(
        !detect_c2pa("application/octet-stream", signed_png),
        "a non-`image/*` mime must short-circuit regardless of the bytes"
    );
}

#[cfg(feature = "c2pa-detect")]
#[test]
fn detect_c2pa_positive_for_signed_png() {
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    assert!(
        detect_c2pa("image/png", signed_png),
        "C2PA-signed PNG must be detected directly, without a process_media round trip"
    );
}

// `detect_c2pa_in_bytes` is the VIEWER's entry point: same detector, container
// sniffed from the bytes instead of taken from an uploader-asserted MIME
// (`ui/media.md` § C2PA provenance — the badge-correction rule). The pair that
// matters is the last two tests: the same signed PNG that `detect_c2pa` refuses
// under a lying MIME is detected here, because here the bytes decide.

#[test]
fn detect_c2pa_in_bytes_negative_for_plain_png() {
    let png = build_png(40, 40);
    assert!(
        !detect_c2pa_in_bytes(&png),
        "a plain PNG carries no manifest, so the viewer's verdict must be false — \
         this is the badge that a forged has_c2pa=true would otherwise paint"
    );
}

#[test]
fn detect_c2pa_in_bytes_negative_for_non_image_bytes() {
    assert!(
        !detect_c2pa_in_bytes(b"not a container at all"),
        "unrecognized bytes sniff to no container and must not be probed"
    );
}

#[cfg(feature = "c2pa-detect")]
#[test]
fn detect_c2pa_in_bytes_ignores_a_lying_mime_because_it_has_none() {
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    // The uploader-asserted-MIME entry point short-circuits on this exact
    // input (`detect_c2pa_skipped_for_non_image_mime` above) …
    assert!(!detect_c2pa("application/octet-stream", signed_png));
    // … while the viewer's entry point sniffs PNG off the bytes and detects it.
    assert!(
        detect_c2pa_in_bytes(signed_png),
        "the viewer takes the container from the bytes, so no asserted MIME can \
         suppress a genuine manifest"
    );
}

#[cfg(feature = "c2pa-detect")]
#[test]
fn detect_c2pa_in_bytes_agrees_with_the_uploader_on_honest_bytes() {
    // The correction rule only works if an honest upload's badge SURVIVES the
    // viewer's re-check: `process_media` stamps `has_c2pa` over the stripped
    // bytes, and those stripped bytes are what the viewer fetches.
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    let ProcessedMedia {
        stripped_bytes,
        has_c2pa,
        ..
    } = process_media(signed_png);
    assert!(has_c2pa, "fixture must be detected on the upload path");
    assert!(
        detect_c2pa_in_bytes(&stripped_bytes),
        "the viewer re-checks the bytes the nest actually serves — a disagreement \
         here would make every honest provenance badge vanish"
    );
}

// The strip must not destroy the provenance it deliberately preserves. The PNG
// arm removes only `eXIf`/`tEXt`/`iTXt`/`zTXt` and leaves C2PA's `caBX` in the
// retain set — but it does so through a full `img-parts` decode → re-encode
// round trip, so an unknown ancillary chunk survives only if the encoder
// carries it through. Web depends on exactly that: it detects `has_c2pa` in the
// browser over the RAW bytes, while the nest serves the STRIPPED bytes and the
// viewer re-parses the manifest from those served bytes to correct the badge
// (`media.md` § C2PA provenance). Were the round trip to drop `caBX`, the
// sidecar would honestly declare `true` over an image that no longer carries a
// manifest — a badge that can never paint, and a silent one: every status check
// still passes. Runs on the curated build too (no `c2pa-detect` needed) — this
// asserts the strip, not the detection.
#[test]
fn strip_preserves_the_png_c2pa_chunk() {
    let signed_png = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    assert!(
        png_chunk_kinds(signed_png).iter().any(|k| k == b"caBX"),
        "fixture precondition: c2pa-signed.png must carry a caBX chunk"
    );

    let ProcessedMedia { stripped_bytes, .. } = process_media(signed_png);

    assert!(
        png_chunk_kinds(&stripped_bytes)
            .iter()
            .any(|k| k == b"caBX"),
        "the metadata strip dropped the C2PA `caBX` chunk; surviving chunks: {:?}",
        png_chunk_kinds(&stripped_bytes)
            .iter()
            .map(|k| String::from_utf8_lossy(k).into_owned())
            .collect::<Vec<_>>()
    );
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// The four-byte kind of every chunk in a PNG, in file order.
fn png_chunk_kinds(png: &[u8]) -> Vec<[u8; 4]> {
    let mut kinds = Vec::new();
    let mut off = 8; // past the 8-byte signature
    while off + 8 <= png.len() {
        let len = u32::from_be_bytes([png[off], png[off + 1], png[off + 2], png[off + 3]]) as usize;
        kinds.push([png[off + 4], png[off + 5], png[off + 6], png[off + 7]]);
        off += 12 + len; // length + kind + data + CRC
    }
    kinds
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Build a minimal RGB JPEG of the given dimensions.
fn build_jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = build_rgb_image(w, h);
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Jpeg)
        .expect("JPEG encode");
    buf.into_inner()
}

/// Build a minimal RGB WebP of the given dimensions.
fn build_webp(w: u32, h: u32) -> Vec<u8> {
    let img = build_rgb_image(w, h);
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::WebP)
        .expect("WebP encode");
    buf.into_inner()
}

/// Build a minimal GIF of the given dimensions.
fn build_gif(w: u32, h: u32) -> Vec<u8> {
    let img = build_rgb_image(w, h);
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Gif)
        .expect("GIF encode");
    buf.into_inner()
}

/// A HEIC container header — `ftyp` box with the `heic` major brand. Only the
/// first 12 bytes are ever sniffed, and the strip declines the format outright,
/// so no real image payload is needed to exercise either path.
fn heic_fixture() -> Vec<u8> {
    b"\x00\x00\x00\x18ftypheic".to_vec()
}

/// Build an ISO-BMFF box: 4-byte big-endian total size, 4-byte type, payload.
fn bmff_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// Assemble `ftyp` + `moov`(+ the given moov children) + `mdat` — the minimal
/// shape the walker has to traverse, with a real `mdat` after the metadata so
/// the offset-preservation assertion means something.
fn build_mp4(moov_children: Vec<u8>) -> Vec<u8> {
    let mut out = bmff_box(b"ftyp", b"isom\x00\x00\x02\x00");
    out.extend_from_slice(&bmff_box(b"moov", &moov_children));
    out.extend_from_slice(&bmff_box(b"mdat", b"pretend-compressed-video-frames"));
    out
}

/// An mp4 whose `moov/udta` holds a `©xyz` location atom carrying `canary` —
/// how a phone records where a video was shot.
fn build_mp4_with_udta(canary: &[u8]) -> Vec<u8> {
    // `©xyz` — the © is the 0xA9 byte in QuickTime's atom namespace.
    let xyz = bmff_box(&[0xA9, b'x', b'y', b'z'], canary);
    build_mp4(bmff_box(b"udta", &xyz))
}

/// An mp4 carrying an Adobe XMP `uuid` box.
fn build_mp4_with_xmp_uuid(canary: &[u8]) -> Vec<u8> {
    let mut payload = vec![
        0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF,
        0xAC,
    ];
    payload.extend_from_slice(canary);
    build_mp4(bmff_box(b"uuid", &payload))
}

/// An mp4 carrying a `uuid` box that is *not* XMP — must survive untouched.
fn build_mp4_with_other_uuid(canary: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x11; 16];
    payload.extend_from_slice(canary);
    build_mp4(bmff_box(b"uuid", &payload))
}

// ── HEIC fixture assembly ───────────────────────────────────────────────────
//
// There is no HEIC/HEIF/AVIF *encoder* anywhere in the dependency graph
// (`image` is pinned to jpeg/png/webp/gif, and no libheif/ravif crate is in
// `Cargo.lock`), so — unlike every other format here — a real fixture cannot be
// round-tripped out of an encoder. It is hand-assembled instead, which is
// entirely sufficient: what the walker parses is `meta`/`iinf`/`iloc`
// *structure*, never HEVC pixel data, so a stand-in "image" payload only has to
// be identifiable enough to assert it survived. The one hazard that buys is a
// fixture and a walker sharing the same misunderstanding — which is why the
// offset-preservation test locates the primary image independently, by content,
// instead of trusting either.

/// The stand-in for the primary image item's compressed bytes.
const HEIC_PRIMARY_IMAGE: &[u8] = b"pretend-hevc-primary-image-bytes";

/// One `infe` entry (ItemInfoEntry, version 2 — the version HEIF actually
/// uses): item_ID, protection index, 4-byte item_type, then type-dependent
/// trailing strings (`item_name`, plus `content_type` when the type is `mime`).
fn heic_infe(item_id: u16, item_type: &[u8; 4], trailing: &[u8]) -> Vec<u8> {
    let mut payload = vec![2u8, 0, 0, 0]; // version 2 + flags
    payload.extend_from_slice(&item_id.to_be_bytes());
    payload.extend_from_slice(&0u16.to_be_bytes()); // item_protection_index
    payload.extend_from_slice(item_type);
    payload.extend_from_slice(trailing);
    bmff_box(b"infe", &payload)
}

/// An `iloc` box locating each `(item_id, extent_offset, extent_length)` with
/// one extent apiece. `offset_size`/`length_size` are both 4 and
/// `base_offset_size` is 0 unless `base_offset` is given, so the box's *size*
/// never depends on the offset *values* — which is what lets the assembler
/// below measure the file in one probe pass and then patch in real offsets.
fn heic_iloc(version: u8, base_offset: Option<u32>, items: &[(u16, u32, u32)]) -> Vec<u8> {
    let items: Vec<_> = items
        .iter()
        .map(|&(id, offset, len)| (id, 0u16, offset, len))
        .collect();
    heic_iloc_with_methods(version, base_offset, &items)
}

/// [`heic_iloc`] with a per-item `construction_method` — `(item_id, method,
/// extent_offset, extent_length)`. Method 1 measures the offset from the
/// `idat` payload rather than the file, so it needs `version` 1 or 2.
fn heic_iloc_with_methods(
    version: u8,
    base_offset: Option<u32>,
    items: &[(u16, u16, u32, u32)],
) -> Vec<u8> {
    let base_size = if base_offset.is_some() { 4u8 } else { 0 };
    let mut p = vec![version, 0, 0, 0, 0x44, base_size << 4];
    if version < 2 {
        p.extend_from_slice(&(items.len() as u16).to_be_bytes());
    } else {
        p.extend_from_slice(&(items.len() as u32).to_be_bytes());
    }
    for (id, method, offset, len) in items {
        if version < 2 {
            p.extend_from_slice(&id.to_be_bytes());
        } else {
            p.extend_from_slice(&(*id as u32).to_be_bytes());
        }
        if version == 1 || version == 2 {
            // construction_method in the low nibble.
            p.extend_from_slice(&method.to_be_bytes());
        } else {
            assert_eq!(*method, 0, "iloc v0 has no construction_method field");
        }
        p.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
        if let Some(base) = base_offset {
            p.extend_from_slice(&base.to_be_bytes());
        }
        p.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        p.extend_from_slice(&offset.to_be_bytes());
        p.extend_from_slice(&len.to_be_bytes());
    }
    bmff_box(b"iloc", &p)
}

/// The `meta` box — a **FullBox**, so 4 version/flags bytes precede its
/// children. (Treating it as a plain container is the single most common way an
/// ISO-BMFF image parser goes wrong: every child then reads 4 bytes skewed.)
fn heic_meta(infe_entries: Vec<u8>, entry_count: u16, iloc: Vec<u8>) -> Vec<u8> {
    heic_meta_with_idat(infe_entries, entry_count, iloc, None)
}

/// [`heic_meta`] with an optional `idat` child carrying `idat_payload` — where
/// an item stored with `construction_method` 1 keeps its bytes.
fn heic_meta_with_idat(
    infe_entries: Vec<u8>,
    entry_count: u16,
    iloc: Vec<u8>,
    idat_payload: Option<&[u8]>,
) -> Vec<u8> {
    let mut children = bmff_box(b"hdlr", b"\x00\x00\x00\x00\x00\x00\x00\x00pict\x00\x00\x00");
    children.extend_from_slice(&bmff_box(b"pitm", &[0, 0, 0, 0, 0, 1]));
    let mut iinf_payload = vec![0u8, 0, 0, 0]; // version 0 + flags
    iinf_payload.extend_from_slice(&entry_count.to_be_bytes());
    iinf_payload.extend_from_slice(&infe_entries);
    children.extend_from_slice(&bmff_box(b"iinf", &iinf_payload));
    children.extend_from_slice(&iloc);
    if let Some(idat) = idat_payload {
        children.extend_from_slice(&bmff_box(b"idat", idat));
    }
    let mut payload = vec![0u8, 0, 0, 0]; // meta FullBox version + flags
    payload.extend_from_slice(&children);
    bmff_box(b"meta", &payload)
}

/// Assemble `ftyp` + `meta` + `mdat` where item 1 is the primary image and
/// item 2 is `second_infe`, whose bytes are `second_payload`.
///
/// `iloc` extent offsets are **absolute file offsets**, so they cannot be known
/// until the file is laid out. One probe pass with placeholder offsets measures
/// the layout (whose size is offset-value-independent by construction, see
/// `heic_iloc`), then the real offsets go in.
fn build_heic(
    second_infe: Vec<u8>,
    second_payload: &[u8],
    version: u8,
    base: Option<u32>,
) -> Vec<u8> {
    build_heic_inner(second_infe, second_payload, version, base, None)
}

/// The assembler behind [`build_heic`]. `second_extent` overrides what `iloc`
/// **declares** about item 2 — given the primary image's file offset, it returns
/// the `(offset, length)` to write. The item's payload still sits in `mdat`
/// exactly as usual; only the file's claim about where its bytes are moves,
/// which is how a *structurally flawless* file can still declare something
/// destructive.
fn build_heic_inner(
    second_infe: Vec<u8>,
    second_payload: &[u8],
    version: u8,
    base: Option<u32>,
    second_extent: Option<fn(u32) -> (u32, u32)>,
) -> Vec<u8> {
    let assemble = |image_at: u32, second_at: u32| -> Vec<u8> {
        let mut entries = heic_infe(1, b"hvc1", b"\x00");
        entries.extend_from_slice(&second_infe);
        let base_delta = base.unwrap_or(0);
        // The declared extent for item 2. `heic_iloc`'s offset/length fields are
        // fixed-width, so pointing it elsewhere does not change the box's size —
        // the two-pass layout measurement below stays valid either way.
        let (second_offset, second_len) = match second_extent {
            Some(declare) => declare(image_at),
            None => (second_at, second_payload.len() as u32),
        };
        let iloc = heic_iloc(
            version,
            base,
            &[
                (
                    1,
                    image_at.saturating_sub(base_delta),
                    HEIC_PRIMARY_IMAGE.len() as u32,
                ),
                (2, second_offset.saturating_sub(base_delta), second_len),
            ],
        );
        let mut out = bmff_box(b"ftyp", b"heic\x00\x00\x00\x00mif1heic");
        out.extend_from_slice(&heic_meta(entries, 2, iloc));
        let mut mdat = HEIC_PRIMARY_IMAGE.to_vec();
        mdat.extend_from_slice(second_payload);
        out.extend_from_slice(&bmff_box(b"mdat", &mdat));
        out
    };
    let probe = assemble(0, 0);
    let mdat_payload_start =
        (probe.len() - (HEIC_PRIMARY_IMAGE.len() + second_payload.len())) as u32;
    assemble(
        mdat_payload_start,
        mdat_payload_start + HEIC_PRIMARY_IMAGE.len() as u32,
    )
}

/// The Exif item payload as HEIF stores it: a 4-byte TIFF-header offset, then
/// the Exif blob a camera wrote.
fn heic_exif_payload(canary: &[u8]) -> Vec<u8> {
    let mut exif = b"\x00\x00\x00\x06Exif\x00\x00II*\x00\x08\x00\x00\x00".to_vec();
    exif.extend_from_slice(canary);
    exif
}

/// A HEIC whose item 2 is an `Exif` item carrying `canary` — how an iPhone
/// stamps GPS onto every photo it takes.
fn build_heic_with_exif_item(canary: &[u8]) -> Vec<u8> {
    build_heic_with_exif_item_iloc_version(canary, 1)
}

fn build_heic_with_exif_item_iloc_version(canary: &[u8], version: u8) -> Vec<u8> {
    build_heic(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(canary),
        version,
        None,
    )
}

/// The same file, but with every extent offset expressed relative to a nonzero
/// per-item `base_offset`.
fn build_heic_with_exif_item_base_offset(canary: &[u8]) -> Vec<u8> {
    build_heic(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(canary),
        1,
        Some(16),
    )
}

/// A HEIC whose `Exif` item is stored in `meta/idat` with `construction_method`
/// 1 — its extent offset is 0, measured from the `idat` payload — while the
/// primary image stays in `mdat` at an absolute offset (the same two-pass
/// layout measurement as [`build_heic_inner`]).
fn build_heic_with_exif_item_in_idat(canary: &[u8]) -> Vec<u8> {
    let exif = heic_exif_payload(canary);
    let assemble = |image_at: u32| -> Vec<u8> {
        let mut entries = heic_infe(1, b"hvc1", b"\x00");
        entries.extend_from_slice(&heic_infe(2, b"Exif", b"\x00"));
        let iloc = heic_iloc_with_methods(
            1,
            None,
            &[
                (1, 0, image_at, HEIC_PRIMARY_IMAGE.len() as u32),
                (2, 1, 0, exif.len() as u32),
            ],
        );
        let mut out = bmff_box(b"ftyp", b"heic\x00\x00\x00\x00mif1heic");
        out.extend_from_slice(&heic_meta_with_idat(entries, 2, iloc, Some(&exif)));
        out.extend_from_slice(&bmff_box(b"mdat", HEIC_PRIMARY_IMAGE));
        out
    };
    let probe = assemble(0);
    assemble((probe.len() - HEIC_PRIMARY_IMAGE.len()) as u32)
}

/// A HEIC carrying XMP the way HEIF specifies it — an item of type `mime`
/// whose `content_type` is `application/rdf+xml`.
fn build_heic_with_xmp_item(canary: &[u8]) -> Vec<u8> {
    let mut xmp = b"<?xpacket begin='' ?><x:xmpmeta>".to_vec();
    xmp.extend_from_slice(canary);
    xmp.extend_from_slice(b"</x:xmpmeta>");
    let mut trailing = b"\x00".to_vec(); // item_name (empty)
    trailing.extend_from_slice(b"application/rdf+xml\x00");
    build_heic(heic_infe(2, b"mime", &trailing), &xmp, 2, None)
}

/// A **structurally flawless** HEIC whose `Exif` item's `iloc` extent points at
/// the primary image's bytes: every box length is right, every offset is inside
/// the file, `construction_method` is 0, and the item genuinely is of type
/// `Exif`. Nothing here is malformed — the file is read correctly and *declares*
/// something destructive, which is the whole point. The Exif
/// payload still sits in `mdat` as usual; only what `iloc` says about it moved.
fn build_heic_with_overlapping_exif_extent(canary: &[u8]) -> Vec<u8> {
    build_heic_inner(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(canary),
        1,
        None,
        Some(|image_at| (image_at, HEIC_PRIMARY_IMAGE.len() as u32)),
    )
}

/// A HEIC whose `Exif` item declares a **zero-length** extent, positioned
/// strictly inside the primary image's range. An empty extent occupies no bytes,
/// so it overlaps nothing and must not trip the guard — half-open
/// interval arithmetic alone would call it "inside the image" and refuse a file
/// the strip could safely have handled.
fn build_heic_with_empty_exif_extent(canary: &[u8]) -> Vec<u8> {
    build_heic_inner(
        heic_infe(2, b"Exif", b"\x00"),
        &heic_exif_payload(canary),
        1,
        None,
        Some(|image_at| (image_at + 4, 0)),
    )
}

/// A HEIC whose second item is ordinary content (a thumbnail image), not
/// metadata — it must come back untouched.
fn build_heic_with_unknown_item(marker: &[u8]) -> Vec<u8> {
    let mut payload = b"THUMB".to_vec();
    payload.extend_from_slice(marker);
    build_heic(heic_infe(2, b"hvc1", b"\x00"), &payload, 1, None)
}

/// Inject an `eXIf` chunk carrying `canary` into a PNG. Mirrors how a camera
/// would write Exif metadata; the strip assertion verifies it's removed.
///
/// **Inserted before IEND, never pushed to the end.** A PNG's chunk stream
/// must end with IEND; a chunk appended after it is a spec violation that
/// `img_parts::png::Png::from_bytes` silently drops on the next parse — which
/// is exactly what `strip_with_coverage` does internally, so a `push`-built
/// fixture never actually reaches the strip code under test. That made this
/// injector's two canary tests pass even with the PNG strip's `strip_kinds`
/// list emptied () — confirmed by mutation: canary preserved end-to-end when
/// correctly positioned, unaffected either way when it was not.
fn inject_png_exif_chunk(png_bytes: &[u8], canary: &[u8]) -> Vec<u8> {
    use img_parts::png::{Png, PngChunk};
    let mut png = Png::from_bytes(bytes::Bytes::copy_from_slice(png_bytes)).expect("valid PNG");
    let chunk = PngChunk::new(*b"eXIf", bytes::Bytes::copy_from_slice(canary));
    let last = png.chunks_mut().len() - 1; // IEND's index — insert just before it.
    png.chunks_mut().insert(last, chunk);
    png.encoder().bytes().to_vec()
}

/// Inject a `tEXt` chunk carrying `canary` into a PNG. Plaintext.rs's strip
/// removes `eXIf`/`tEXt`/`iTXt`/`zTXt`; this asserts the tEXt arm.
///
/// Inserted before IEND — see [`inject_png_exif_chunk`]'s doc comment for why
/// appending after it made the canary assertion vacuous.
fn inject_png_text_chunk(png_bytes: &[u8], canary: &[u8]) -> Vec<u8> {
    use img_parts::png::{Png, PngChunk};
    let mut png = Png::from_bytes(bytes::Bytes::copy_from_slice(png_bytes)).expect("valid PNG");
    // tEXt format: keyword \0 value
    let mut payload = b"Comment\x00".to_vec();
    payload.extend_from_slice(canary);
    let chunk = PngChunk::new(*b"tEXt", bytes::Bytes::from(payload));
    let last = png.chunks_mut().len() - 1;
    png.chunks_mut().insert(last, chunk);
    png.encoder().bytes().to_vec()
}

/// Inject a RIFF chunk carrying `canary` into a WebP — `EXIF` is where a phone
/// camera writes GPS, `XMP ` carries the same data as XML.
fn inject_webp_chunk(webp_bytes: &[u8], id: [u8; 4], canary: &[u8]) -> Vec<u8> {
    use img_parts::{
        riff::{RiffChunk, RiffContent},
        webp::WebP,
    };
    let mut webp = WebP::from_bytes(bytes::Bytes::copy_from_slice(webp_bytes)).expect("valid WebP");
    let chunk = RiffChunk::new(id, RiffContent::Data(bytes::Bytes::copy_from_slice(canary)));
    webp.chunks_mut().push(chunk);
    webp.encoder().bytes().to_vec()
}

/// Inject a GIF extension block carrying `canary`, immediately before the
/// trailer (a valid block position). `label` is 0xFE (Comment) or 0xFF
/// (Application — where XMP rides).
fn inject_gif_extension(gif_bytes: &[u8], label: u8, canary: &[u8]) -> Vec<u8> {
    assert!(canary.len() < 256, "canary must fit one GIF sub-block");
    let mut out = gif_bytes.to_vec();
    let trailer = out.pop().expect("GIF fixture must be non-empty");
    assert_eq!(trailer, 0x3B, "GIF fixture must end with the trailer byte");
    out.extend_from_slice(&[0x21, label, canary.len() as u8]);
    out.extend_from_slice(canary);
    out.extend_from_slice(&[0x00, 0x3B]); // sub-block terminator, then trailer
    out
}

/// Inject an APP1 (Exif) segment carrying `canary` into a JPEG.
fn inject_jpeg_app1(jpeg_bytes: &[u8], canary: &[u8]) -> Vec<u8> {
    use img_parts::jpeg::{Jpeg, JpegSegment, markers};
    let mut jpeg = Jpeg::from_bytes(bytes::Bytes::copy_from_slice(jpeg_bytes)).expect("valid JPEG");
    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(canary);
    let seg = JpegSegment::new_with_contents(markers::APP1, bytes::Bytes::from(payload));
    jpeg.segments_mut().insert(1, seg);
    jpeg.encoder().bytes().to_vec()
}

/// Inject an APP13 (IPTC/Photoshop) segment carrying `canary` into a JPEG.
fn inject_jpeg_app13(jpeg_bytes: &[u8], canary: &[u8]) -> Vec<u8> {
    use img_parts::jpeg::{Jpeg, JpegSegment, markers};
    let mut jpeg = Jpeg::from_bytes(bytes::Bytes::copy_from_slice(jpeg_bytes)).expect("valid JPEG");
    let mut payload = b"Photoshop 3.0\x00".to_vec();
    payload.extend_from_slice(canary);
    let seg = JpegSegment::new_with_contents(markers::APP13, bytes::Bytes::from(payload));
    jpeg.segments_mut().insert(1, seg);
    jpeg.encoder().bytes().to_vec()
}
