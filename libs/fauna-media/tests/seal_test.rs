use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::subscription::types::MlsGroupId;
use fauna_media::audience::{Audience, AudienceClass, RestrictedPostAudience};
use fauna_mls::types::ChannelId;
use zeroize::Zeroizing;

fn library_audience() -> Audience {
    Audience::Library {
        backup_key: BackupKey::from_bytes([0x11; 32]),
    }
}

fn conversation_audience() -> Audience {
    Audience::Conversation {
        channel_id: ChannelId([0x22; 32]),
        epoch_secret: Zeroizing::new([0x33; 32]),
    }
}

fn group_restricted_post_audience() -> Audience {
    Audience::RestrictedPost {
        post_id: ContentHash::from_digest_raw([0x44; 32]),
        audience: RestrictedPostAudience::Group {
            group_id: MlsGroupId(vec![0x55; 32]),
            epoch_secret: Zeroizing::new([0x66; 32]),
        },
    }
}

fn period_restricted_post_audience() -> Audience {
    Audience::RestrictedPost {
        post_id: ContentHash::from_digest_raw([0x77; 32]),
        audience: RestrictedPostAudience::Period {
            tier: "Pro".to_string(),
            period_epoch: 42,
            period_key: Zeroizing::new([0x88; 32]),
        },
    }
}

fn public_post_audience() -> Audience {
    Audience::PublicPost {
        post_id: ContentHash::from_digest_raw([0x99; 32]),
    }
}

#[test]
fn audience_class_tag_matches_variant() {
    assert_eq!(library_audience().class(), AudienceClass::Library);
    assert_eq!(conversation_audience().class(), AudienceClass::Conversation);
    assert_eq!(
        group_restricted_post_audience().class(),
        AudienceClass::GroupRestrictedPost
    );
    assert_eq!(
        period_restricted_post_audience().class(),
        AudienceClass::PeriodRestrictedPost
    );
    assert_eq!(public_post_audience().class(), AudienceClass::PublicPost);
}

#[test]
fn audience_debug_does_not_leak_key_material() {
    // Use a recognizable canary byte pattern; assert it does not appear in
    // the Debug output of any variant carrying key material.
    let canary: [u8; 32] = [0xCD; 32];

    // Library (BackupKey constructed from the canary).
    let lib = Audience::Library {
        backup_key: BackupKey::from_bytes(canary),
    };
    let lib_dbg = format!("{:?}", lib);
    assert!(
        !lib_dbg.contains("205, 205"),
        "Library Debug leaked key bytes: {lib_dbg}"
    );
    assert!(
        !lib_dbg.contains("0xCD"),
        "Library Debug leaked key bytes: {lib_dbg}"
    );

    // Conversation
    let conv = Audience::Conversation {
        channel_id: ChannelId([0x22; 32]),
        epoch_secret: Zeroizing::new(canary),
    };
    let conv_dbg = format!("{:?}", conv);
    assert!(
        !conv_dbg.contains("205, 205"),
        "Conversation Debug leaked key bytes: {conv_dbg}"
    );

    // RestrictedPost::Group
    let grp = Audience::RestrictedPost {
        post_id: ContentHash::from_digest_raw([0x44; 32]),
        audience: RestrictedPostAudience::Group {
            group_id: MlsGroupId(vec![0x55; 32]),
            epoch_secret: Zeroizing::new(canary),
        },
    };
    let grp_dbg = format!("{:?}", grp);
    assert!(
        !grp_dbg.contains("205, 205"),
        "RestrictedPost::Group Debug leaked key bytes: {grp_dbg}"
    );

    // RestrictedPost::Period
    let per = Audience::RestrictedPost {
        post_id: ContentHash::from_digest_raw([0x77; 32]),
        audience: RestrictedPostAudience::Period {
            tier: "Pro".to_string(),
            period_epoch: 42,
            period_key: Zeroizing::new(canary),
        },
    };
    let per_dbg = format!("{:?}", per);
    assert!(
        !per_dbg.contains("205, 205"),
        "RestrictedPost::Period Debug leaked key bytes: {per_dbg}"
    );
}

#[test]
fn seal_for_audience_library_roundtrip() {
    let audience = library_audience();
    let plaintext = b"library blob bytes";
    let fauna_media::seal::SealedBlob { bytes, class } =
        fauna_media::seal::seal_for_audience(&audience, plaintext);
    assert_eq!(class, AudienceClass::Library);

    // Recover the BackupKey directly so the test asserts the seal is exactly
    // encrypt_backup_chunk under the variant's key.
    let key = BackupKey::from_bytes([0x11; 32]);
    let recovered = fauna_core::crypto::decrypt_backup_chunk(&key, &bytes).expect("decrypts");
    assert_eq!(recovered, plaintext);
}

#[test]
fn seal_for_audience_conversation_roundtrip() {
    let audience = conversation_audience();
    let plaintext = b"conversation blob bytes";
    let fauna_media::seal::SealedBlob { bytes, class } =
        fauna_media::seal::seal_for_audience(&audience, plaintext);
    assert_eq!(class, AudienceClass::Conversation);

    // Recover with decrypt_blob — the spec's dispatch table
    // says this is the conversation primitive.
    let epoch_secret = [0x33u8; 32];
    let recovered = fauna_mls::blob_crypto::decrypt_blob(&epoch_secret, &bytes).expect("decrypts");
    assert_eq!(recovered, plaintext);
}

#[test]
fn seal_for_audience_restricted_post_group_roundtrip() {
    use fauna_core::data::ContentHash;
    use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
    use fauna_media::seal::SealedBlob;

    let audience = group_restricted_post_audience();
    let plaintext = b"group-restricted post-attached blob";
    let SealedBlob { bytes, class } = fauna_media::seal::seal_for_audience(&audience, plaintext);
    assert_eq!(class, AudienceClass::GroupRestrictedPost);

    // Reproduce derive_post_key(epoch_secret, post_id), then decrypt.
    let epoch_secret = [0x66u8; 32];
    let post_id = ContentHash::from_digest_raw([0x44; 32]);
    let per_post_key = derive_post_key(&epoch_secret, &post_id);
    let recovered = decrypt_content(&per_post_key, &bytes).expect("decrypts");
    assert_eq!(recovered, plaintext);
}

#[test]
fn seal_for_audience_restricted_post_period_roundtrip() {
    use fauna_core::data::ContentHash;
    use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
    use fauna_media::seal::SealedBlob;

    let audience = period_restricted_post_audience();
    let plaintext = b"period-restricted (subscription) post-attached blob";
    let SealedBlob { bytes, class } = fauna_media::seal::seal_for_audience(&audience, plaintext);
    assert_eq!(class, AudienceClass::PeriodRestrictedPost);

    let period_key = [0x88u8; 32];
    let post_id = ContentHash::from_digest_raw([0x77; 32]);
    let per_post_key = derive_post_key(&period_key, &post_id);
    let recovered = decrypt_content(&per_post_key, &bytes).expect("decrypts");
    assert_eq!(recovered, plaintext);
}

#[test]
fn seal_for_audience_public_post_passes_through() {
    let audience = public_post_audience();
    let plaintext = b"public post attachment bytes";
    let fauna_media::seal::SealedBlob { bytes, class } =
        fauna_media::seal::seal_for_audience(&audience, plaintext);
    assert_eq!(class, AudienceClass::PublicPost);
    assert_eq!(
        bytes, plaintext,
        "PublicPost passes bytes through unchanged"
    );
}

#[test]
fn seal_envelope_shape_is_class_specific() {
    let plaintext = b"shape probe";

    // Library: encrypt_backup_chunk emits version-byte 0x01 + 12-byte nonce + ct+tag.
    let lib = fauna_media::seal::seal_for_audience(&library_audience(), plaintext);
    assert_eq!(lib.bytes[0], 0x01, "Library version byte");
    assert!(
        lib.bytes.len() > 1 + 12 + 16,
        "Library: version + nonce + ct + tag"
    );

    // Conversation: 12-byte nonce + ct+tag (no version byte).
    let conv = fauna_media::seal::seal_for_audience(&conversation_audience(), plaintext);
    assert!(conv.bytes.len() > 12 + 16, "Conversation: nonce + ct + tag");

    // GroupRestrictedPost: 12-byte nonce + ct+tag.
    let grp = fauna_media::seal::seal_for_audience(&group_restricted_post_audience(), plaintext);
    assert!(
        grp.bytes.len() > 12 + 16,
        "GroupRestrictedPost: nonce + ct + tag"
    );

    // PeriodRestrictedPost: 12-byte nonce + ct+tag.
    let per = fauna_media::seal::seal_for_audience(&period_restricted_post_audience(), plaintext);
    assert!(
        per.bytes.len() > 12 + 16,
        "PeriodRestrictedPost: nonce + ct + tag"
    );

    // PublicPost: byte-identical to input.
    let pub_ = fauna_media::seal::seal_for_audience(&public_post_audience(), plaintext);
    assert_eq!(pub_.bytes, plaintext, "PublicPost: byte-identical");
}

use fauna_media::pipeline::process_and_seal;
use fauna_media::sidecar::UploadSidecar;

#[test]
fn process_and_seal_passes_non_image_bytes_unchanged() {
    // Cover every Audience variant so the composer's dispatch surface is
    // verified end-to-end. Input bytes don't match any sniff signature, so
    // MIME falls back to octet-stream and there's no thumbnail / no C2PA
    // detection — exercises the non-image path of the real `process_media`
    // (and the identity-passthrough stub when `process_media` is off).
    let plaintext = b"raw uploader-supplied bytes";

    // Library
    {
        let audience = library_audience();
        let payload = process_and_seal(plaintext, &audience);
        assert_eq!(payload.primary.class, AudienceClass::Library);
        // Stub passes raw bytes straight through to the seal layer: decrypt
        // and verify we recover plaintext (uses known key bytes [0x11; 32]).
        let key = BackupKey::from_bytes([0x11; 32]);
        let recovered = fauna_core::crypto::decrypt_backup_chunk(&key, &payload.primary.bytes)
            .expect("decrypts");
        assert_eq!(recovered, plaintext);
        assert_eq!(payload.primary_sidecar.class, AudienceClass::Library);
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }

    // Conversation
    {
        let audience = conversation_audience();
        let payload = process_and_seal(plaintext, &audience);
        assert_eq!(payload.primary.class, AudienceClass::Conversation);
        let epoch_secret = [0x33u8; 32];
        let recovered = fauna_mls::blob_crypto::decrypt_blob(&epoch_secret, &payload.primary.bytes)
            .expect("decrypts");
        assert_eq!(recovered, plaintext);
        assert_eq!(payload.primary_sidecar.class, AudienceClass::Conversation);
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }

    // GroupRestrictedPost
    {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};

        let audience = group_restricted_post_audience();
        let payload = process_and_seal(plaintext, &audience);
        assert_eq!(payload.primary.class, AudienceClass::GroupRestrictedPost);
        let epoch_secret = [0x66u8; 32];
        let post_id = ContentHash::from_digest_raw([0x44; 32]);
        let per_post_key = derive_post_key(&epoch_secret, &post_id);
        let recovered = decrypt_content(&per_post_key, &payload.primary.bytes).expect("decrypts");
        assert_eq!(recovered, plaintext);
        assert_eq!(
            payload.primary_sidecar.class,
            AudienceClass::GroupRestrictedPost
        );
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }

    // PeriodRestrictedPost
    {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};

        let audience = period_restricted_post_audience();
        let payload = process_and_seal(plaintext, &audience);
        assert_eq!(payload.primary.class, AudienceClass::PeriodRestrictedPost);
        let period_key = [0x88u8; 32];
        let post_id = ContentHash::from_digest_raw([0x77; 32]);
        let per_post_key = derive_post_key(&period_key, &post_id);
        let recovered = decrypt_content(&per_post_key, &payload.primary.bytes).expect("decrypts");
        assert_eq!(recovered, plaintext);
        assert_eq!(
            payload.primary_sidecar.class,
            AudienceClass::PeriodRestrictedPost
        );
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }

    // PublicPost — pass-through, bytes are identical to plaintext.
    {
        let audience = public_post_audience();
        let payload = process_and_seal(plaintext, &audience);
        assert_eq!(payload.primary.class, AudienceClass::PublicPost);
        assert_eq!(payload.primary.bytes, plaintext);
        assert_eq!(payload.primary_sidecar.class, AudienceClass::PublicPost);
        assert_eq!(payload.primary_sidecar.mime, "application/octet-stream");
        assert!(!payload.primary_sidecar.has_c2pa);
        assert_eq!(payload.primary_sidecar.thumbnail_hash, None);
        assert!(payload.thumbnail.is_none());
    }
}

#[test]
fn upload_sidecar_dag_cbor_roundtrip() {
    let sidecar = UploadSidecar {
        class: AudienceClass::GroupRestrictedPost,
        mime: "image/png".to_string(),
        has_c2pa: true,
        thumbnail_hash: Some([0xAB; 32]),
    };

    let bytes = sidecar.to_dag_cbor();
    let recovered = UploadSidecar::from_dag_cbor(&bytes).expect("CBOR roundtrips");

    assert_eq!(recovered.class, sidecar.class);
    assert_eq!(recovered.mime, sidecar.mime);
    assert_eq!(recovered.has_c2pa, sidecar.has_c2pa);
    assert_eq!(recovered.thumbnail_hash, sidecar.thumbnail_hash);

    // Defensive: serialized bytes must contain none of the key material an
    // Audience would carry. Encode an Audience with all-0xCD key bytes;
    // assert the encoded UploadSidecar does NOT contain that pattern.
    let needle: [u8; 8] = [0xCD; 8];
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "UploadSidecar must not contain Audience key material"
    );
}

// --- Audience-aware sidecar MIME (the `process_media` producer precondition) ---
//
// These exercise the audience-aware sidecar logic that only manifests once the
// real `process_media` runs (it sniffs a real `image/*` MIME + renders a
// thumbnail), so they are gated on the `process_media` feature. With the stub
// every sniff falls back to octet-stream / no thumbnail, so the assertions
// would not be meaningful.

// A real PNG larger than the 300×300 thumbnail threshold, so the real
// `process_media` both sniffs `image/png` AND renders a JPEG thumbnail.
#[cfg(feature = "process_media")]
use fauna_media::test_fixtures::build_png;

/// An image sealed under an AEAD audience declares an octet-stream sidecar MIME +
/// `has_c2pa = false` even though `process_media` sniffed a real `image/*`
/// type and rendered a thumbnail: the ciphertext blob's real type / C2PA flag
/// ride *inside* the sealed bytes, and the nest's encrypted-mode verifier
/// rejects any other sidecar MIME for a sealed class
/// (`bins/fauna-nest/src/storage/encrypted.rs::classify_per_class_envelope` →
/// `mime_class_mismatch`). The companion thumbnail is still generated, its
/// routing hash rides in the primary sidecar, and the sealed thumbnail's own
/// sidecar is octet-stream too.
#[cfg(feature = "process_media")]
#[test]
fn aead_audience_sidecar_is_octet_stream_for_real_image() {
    let png = build_png(800, 600); // > 300×300 ⇒ a thumbnail is rendered
    for audience in [
        library_audience(),
        conversation_audience(),
        group_restricted_post_audience(),
        period_restricted_post_audience(),
    ] {
        let class = audience.class();
        let payload = process_and_seal(&png, &audience);
        assert_eq!(payload.primary_sidecar.class, class);
        assert_eq!(
            payload.primary_sidecar.mime, "application/octet-stream",
            "{class:?}: an AEAD-sealed blob's sidecar MIME must be octet-stream"
        );
        assert!(
            !payload.primary_sidecar.has_c2pa,
            "{class:?}: a sealed blob's sidecar has_c2pa must be false"
        );
        assert!(
            payload.primary_sidecar.thumbnail_hash.is_some(),
            "{class:?}: a >300px image still yields a thumbnail whose hash rides in the primary sidecar"
        );
        let (_, thumb_sidecar) = payload.thumbnail.expect("thumbnail blob present");
        assert_eq!(thumb_sidecar.class, class);
        assert_eq!(
            thumb_sidecar.mime, "application/octet-stream",
            "{class:?}: the sealed thumbnail blob's sidecar MIME is octet-stream too"
        );
        assert!(!thumb_sidecar.has_c2pa);
    }
}

/// A real image attached to a `PublicPost` (plaintext passthrough) keeps the
/// real sniffed MIME in its sidecar — the Content-Type the nest serves on
/// download, and the non-empty `type/subtype` the PublicPost verifier requires.
/// Its thumbnail (also plaintext) stays `image/jpeg`.
#[cfg(feature = "process_media")]
#[test]
fn public_post_sidecar_keeps_real_image_mime() {
    let png = build_png(800, 600);
    let payload = process_and_seal(&png, &public_post_audience());
    assert_eq!(payload.primary_sidecar.class, AudienceClass::PublicPost);
    assert_eq!(payload.primary_sidecar.mime, "image/png");
    assert!(payload.primary_sidecar.thumbnail_hash.is_some());
    let (_, thumb_sidecar) = payload.thumbnail.expect("thumbnail blob present");
    assert_eq!(thumb_sidecar.mime, "image/jpeg");
    assert!(!thumb_sidecar.has_c2pa);
}

// --- seal_thumbnail_only (the SyncEngine producer entrypoint) ---
//
// `seal_thumbnail_only` is for producers that store the primary by another path
// (the device-sync chunk pipeline) and need ONLY the thumbnail sidecar blob +
// its content hash. It must seal the thumbnail identically to `process_and_seal`
// (same audience, same sidecar shape, same `blake3(sealed bytes)` hash), so a
// client fetches it direct-by-hash and decrypts it the same way.

/// A >300px image yields a sealed thumbnail whose hash matches the one
/// `process_and_seal` would record in the primary sidecar, with the same
/// octet-stream sidecar for an AEAD audience.
#[cfg(feature = "process_media")]
#[test]
fn seal_thumbnail_only_matches_process_and_seal_for_aead_image() {
    use fauna_media::pipeline::seal_thumbnail_only;
    let png = build_png(800, 600); // > 300×300 ⇒ a thumbnail is rendered
    let audience = library_audience();
    let class = audience.class();

    let only = seal_thumbnail_only(&png, &audience).expect("a >300px image yields a thumbnail");
    assert!(!only.bytes.is_empty(), "sealed thumbnail bytes present");
    assert_eq!(
        only.hash,
        *blake3::hash(&only.bytes).as_bytes(),
        "recorded hash is blake3 of the sealed bytes"
    );
    assert_eq!(only.sidecar.class, class);
    assert_eq!(
        only.sidecar.mime, "application/octet-stream",
        "an AEAD-sealed thumbnail's sidecar MIME is octet-stream"
    );
    assert!(!only.sidecar.has_c2pa);
    assert_eq!(
        only.sidecar.thumbnail_hash, None,
        "thumbnails don't have thumbnails"
    );

    // The hash must equal what process_and_seal records in the primary sidecar
    // (modulo per-call AEAD nonce randomness, the SHAPE is identical) — assert it
    // is a real recorded routing hash, not absent.
    let payload = process_and_seal(&png, &audience);
    assert!(
        payload.primary_sidecar.thumbnail_hash.is_some(),
        "process_and_seal also records a thumbnail hash for the same image"
    );
}

/// A ≤300px image and a non-image both yield no thumbnail.
#[cfg(feature = "process_media")]
#[test]
fn seal_thumbnail_only_none_for_small_or_nonimage() {
    use fauna_media::pipeline::seal_thumbnail_only;
    let audience = library_audience();

    let small = build_png(100, 100); // ≤ 300×300 ⇒ no thumbnail
    assert!(
        seal_thumbnail_only(&small, &audience).is_none(),
        "a ≤300px image yields no thumbnail"
    );

    let not_an_image = b"this is plainly not an image".to_vec();
    assert!(
        seal_thumbnail_only(&not_an_image, &audience).is_none(),
        "non-image bytes yield no thumbnail"
    );
}

// --- seal_thumbnail_only_from_path (the >64 MiB streaming-upload entrypoint) ---
//
// `seal_thumbnail_only_from_path` reads the source image incrementally from disk
// (`image::ImageReader::open`), so the device-sync streaming-upload path
// (>64 MiB files) stays O(thumbnail) memory instead of loading the whole file.
// It must seal the thumbnail IDENTICALLY to `seal_thumbnail_only` on the same
// image bytes — same sidecar shape and `blake3(sealed bytes)` hash contract — so
// a client fetches + decrypts a streamed-file thumbnail the same way.

/// A >300px image file yields a sealed thumbnail whose sidecar matches what
/// `seal_thumbnail_only` produces for the same bytes (both AEAD-octet-stream).
#[cfg(feature = "process_media")]
#[test]
fn seal_thumbnail_only_from_path_matches_in_memory_for_aead_image() {
    use fauna_media::pipeline::{seal_thumbnail_only, seal_thumbnail_only_from_path};
    let png = build_png(800, 600); // > 300×300 ⇒ a thumbnail is rendered
    let audience = library_audience();
    let class = audience.class();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.png");
    std::fs::write(&path, &png).unwrap();

    let from_path = seal_thumbnail_only_from_path(&path, &audience)
        .expect("a >300px image file yields a thumbnail");
    assert!(
        !from_path.bytes.is_empty(),
        "sealed thumbnail bytes present"
    );
    assert_eq!(
        from_path.hash,
        *blake3::hash(&from_path.bytes).as_bytes(),
        "recorded hash is blake3 of the sealed bytes"
    );
    assert_eq!(from_path.sidecar.class, class);
    assert_eq!(
        from_path.sidecar.mime, "application/octet-stream",
        "an AEAD-sealed thumbnail's sidecar MIME is octet-stream"
    );
    assert!(!from_path.sidecar.has_c2pa);
    assert_eq!(
        from_path.sidecar.thumbnail_hash, None,
        "thumbnails don't have thumbnails"
    );

    // The in-memory entrypoint on the same bytes produces the same sidecar shape
    // (the only difference is per-call AEAD nonce randomness in `bytes`/`hash`).
    let in_memory = seal_thumbnail_only(&png, &audience).expect("in-memory thumbnail");
    assert_eq!(from_path.sidecar.class, in_memory.sidecar.class);
    assert_eq!(from_path.sidecar.mime, in_memory.sidecar.mime);
    assert_eq!(from_path.sidecar.has_c2pa, in_memory.sidecar.has_c2pa);
    assert_eq!(
        from_path.sidecar.thumbnail_hash,
        in_memory.sidecar.thumbnail_hash
    );
}

/// A ≤300px image file, a non-image file, and a missing path all yield no
/// thumbnail (a decode/open failure declines gracefully, like the in-memory
/// path).
#[cfg(feature = "process_media")]
#[test]
fn seal_thumbnail_only_from_path_none_for_small_nonimage_or_missing() {
    use fauna_media::pipeline::seal_thumbnail_only_from_path;
    let audience = library_audience();
    let dir = tempfile::tempdir().unwrap();

    let small_path = dir.path().join("tiny.png");
    std::fs::write(&small_path, build_png(100, 100)).unwrap();
    assert!(
        seal_thumbnail_only_from_path(&small_path, &audience).is_none(),
        "a ≤300px image file yields no thumbnail"
    );

    let text_path = dir.path().join("notes.txt");
    std::fs::write(&text_path, b"this is plainly not an image").unwrap();
    assert!(
        seal_thumbnail_only_from_path(&text_path, &audience).is_none(),
        "a non-image file yields no thumbnail"
    );

    let missing_path = dir.path().join("does-not-exist.png");
    assert!(
        seal_thumbnail_only_from_path(&missing_path, &audience).is_none(),
        "a missing file declines gracefully rather than panicking"
    );
}

/// The multipart flattening every app shares: a real >300px `PublicPost`
/// image yields BOTH parts, and the primary sidecar's `thumbnail_hash` routes
/// to exactly the thumbnail bytes handed to the caller.
///
/// This is the contract the nest's `?thumb=1` lookup depends on
/// (`bins/fauna-nest/src/blob_routes.rs` records `thumbnail_hash` into
/// `blob_metadata` on primary ingest, then serves the blob it names). A client
/// that keeps the hash but drops the bytes POSTs a pointer to a blob that was
/// never uploaded — the nest then silently serves the full-size original. Web
/// did exactly that until the `WasmUploadPayload` threading landed.
#[cfg(feature = "process_media")]
#[test]
fn into_multipart_parts_routes_primary_sidecar_to_the_thumbnail_bytes() {
    use fauna_media::pipeline::process_and_seal;
    use fauna_media::sidecar::UploadSidecar;

    let audience = Audience::PublicPost {
        post_id: ContentHash::from_digest_raw([0u8; 32]),
    };
    let payload = process_and_seal(&build_png(800, 600), &audience);
    let (primary, thumbnail) = payload.into_multipart_parts();

    let thumbnail = thumbnail.expect("a >300px image must yield a thumbnail part");
    assert!(!thumbnail.bytes.is_empty());
    assert!(
        thumbnail.bytes.len() < primary.bytes.len(),
        "a thumbnail that is not smaller than its primary defeats the purpose"
    );

    let decoded = UploadSidecar::from_dag_cbor(&primary.sidecar_cbor)
        .expect("primary sidecar decodes as DAG-CBOR");
    assert_eq!(
        decoded.thumbnail_hash,
        Some(*blake3::hash(&thumbnail.bytes).as_bytes()),
        "the primary sidecar must route to the thumbnail bytes we hand the caller"
    );

    // The thumbnail's own sidecar never recurses.
    let thumb_sidecar = UploadSidecar::from_dag_cbor(&thumbnail.sidecar_cbor)
        .expect("thumbnail sidecar decodes as DAG-CBOR");
    assert_eq!(thumb_sidecar.thumbnail_hash, None);
    assert_eq!(thumb_sidecar.class, AudienceClass::PublicPost);
}

/// The `None` arm: non-image bytes produce no thumbnail part, and the primary
/// sidecar must therefore carry no dangling routing hash.
#[test]
fn into_multipart_parts_yields_no_thumbnail_for_a_non_image() {
    use fauna_media::pipeline::process_and_seal;
    use fauna_media::sidecar::UploadSidecar;

    let audience = Audience::PublicPost {
        post_id: ContentHash::from_digest_raw([0u8; 32]),
    };
    let payload = process_and_seal(b"just attachment bytes", &audience);
    let (primary, thumbnail) = payload.into_multipart_parts();

    assert!(thumbnail.is_none());
    let decoded = UploadSidecar::from_dag_cbor(&primary.sidecar_cbor)
        .expect("primary sidecar decodes as DAG-CBOR");
    assert_eq!(decoded.thumbnail_hash, None);
}
