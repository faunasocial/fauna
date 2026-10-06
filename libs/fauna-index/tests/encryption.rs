//! Integration tests for the segment-AEAD wire format and the master-direct
//! AEAD wire format. Lives separate from `round_trip.rs` so the encryption
//! cases can be filtered (`cargo test -p fauna-index --test encryption`).

use fauna_index::{
    ContentId, ContentKind, FieldKind, Index, IndexMasterKey, IndexedDoc, IndexedField,
    open_segment_bytes, open_under_master, rewrap_segment_master_key, seal_segment_bytes,
    seal_under_master,
};

fn key(seed: u8) -> IndexMasterKey {
    IndexMasterKey::from_bytes([seed; 32])
}

#[test]
fn segment_seal_open_round_trip() {
    let master = key(7);
    let plaintext = b"the quick brown fox jumps over the lazy dog".to_vec();
    let sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    let opened = open_segment_bytes(&sealed, &master).expect("open");
    assert_eq!(opened, plaintext);
}

#[test]
fn segment_seal_round_trips_empty_plaintext() {
    let master = key(13);
    let sealed = seal_segment_bytes(&[], &master).expect("seal");
    let opened = open_segment_bytes(&sealed, &master).expect("open");
    assert!(opened.is_empty());
}

#[test]
fn segment_seal_produces_distinct_ciphertexts_for_same_input() {
    // Random nonces (header + body + per-segment data key) mean two seals of
    // the same plaintext under the same master key produce different bytes.
    let master = key(21);
    let plaintext = b"deterministic input".to_vec();
    let a = seal_segment_bytes(&plaintext, &master).expect("seal a");
    let b = seal_segment_bytes(&plaintext, &master).expect("seal b");
    assert_ne!(a, b, "two seals of identical plaintext must differ");
    assert_eq!(open_segment_bytes(&a, &master).expect("open a"), plaintext);
    assert_eq!(open_segment_bytes(&b, &master).expect("open b"), plaintext);
}

const OFF_HEADER_NONCE: usize = 8;
const OFF_WRAPPED_DK: usize = 32;
const OFF_BODY_NONCE: usize = 80;
const OFF_BODY: usize = 104;

#[test]
fn segment_open_rejects_flipped_body_byte() {
    let master = key(33);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    // Flip a bit in the body region.
    let body_byte = OFF_BODY + 2;
    sealed[body_byte] ^= 0x01;
    let err =
        open_segment_bytes(&sealed, &master).expect_err("flipped body byte must fail to decrypt");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt body failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn segment_open_rejects_flipped_header_nonce() {
    let master = key(34);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    sealed[OFF_HEADER_NONCE] ^= 0x01;
    let err = open_segment_bytes(&sealed, &master)
        .expect_err("flipped header nonce must fail to unwrap data key");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn segment_open_rejects_flipped_wrapped_data_key() {
    let master = key(35);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    sealed[OFF_WRAPPED_DK + 5] ^= 0x80;
    let err = open_segment_bytes(&sealed, &master)
        .expect_err("flipped wrapped data key must fail AEAD tag check");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn segment_open_rejects_flipped_body_nonce() {
    let master = key(36);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    sealed[OFF_BODY_NONCE + 1] ^= 0x40;
    let err = open_segment_bytes(&sealed, &master)
        .expect_err("flipped body nonce must fail body decrypt");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt body failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn segment_open_rejects_truncated_blob() {
    let master = key(37);
    let plaintext = b"intact body".to_vec();
    let sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    // Drop the last byte (truncates the AEAD tag).
    let truncated = &sealed[..sealed.len() - 1];
    let err = open_segment_bytes(truncated, &master).expect_err("truncated blob must fail");
    let _ = err;
}

#[test]
fn segment_open_rejects_bad_magic() {
    let master = key(38);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    sealed[0] = b'X';
    let err = open_segment_bytes(&sealed, &master).expect_err("bad magic must fail before AEAD");
    let msg = format!("{err}");
    assert!(
        msg.contains("bad segment magic"),
        "unexpected message: {msg}"
    );
}

/// A relabelled version byte is rejected — but by the **AEAD tag**, not by an
/// exact-match string check, and that is the point.
///
/// The version byte alone no longer decides readability: the *reader floor* does
/// (`crate::version`, the § 2.2 two-number scheme), so a genuinely newer-additive
/// segment is meant to open. What must not open is a blob whose prefix someone
/// *rewrote after the fact* — and since the prefix is now the AEAD associated data,
/// the tag catches exactly that. The in-crate tests in `src/seal.rs` pin both sides:
/// a legitimately-sealed v9 segment opens; a relabelled one does not.
#[test]
fn segment_open_rejects_relabelled_version() {
    let master = key(39);
    let plaintext = b"intact body".to_vec();
    let mut sealed = seal_segment_bytes(&plaintext, &master).expect("seal");
    sealed[4] = 0x99;
    let err = open_segment_bytes(&sealed, &master).expect_err("a relabelled version must not open");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "the AAD-bound prefix must catch the relabel: {msg}"
    );
}

#[test]
fn segment_open_with_wrong_master_fails() {
    let alice = key(1);
    let bob = key(2);
    let plaintext = b"alice's data".to_vec();
    let sealed = seal_segment_bytes(&plaintext, &alice).expect("seal");
    let err =
        open_segment_bytes(&sealed, &bob).expect_err("opening with wrong master key must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

fn one_mail_doc(id: u8, title: &str, body: &str) -> IndexedDoc {
    IndexedDoc {
        kind: ContentKind::Mail,
        content_id: ContentId(vec![id]),
        timestamp_ns: 1_000_000_000 * (id as i64 + 1),
        sender_actor_id: None,
        secondary_id: None,
        fields: vec![
            IndexedField {
                kind: FieldKind::Title,
                text: title.into(),
            },
            IndexedField {
                kind: FieldKind::Body,
                text: body.into(),
            },
        ],
    }
}

#[test]
fn index_seal_encrypted_open_encrypted_round_trip_preserves_queryability() {
    let master = key(99);

    let mut writer = Index::create_in_ram().expect("create");
    writer
        .add_doc(one_mail_doc(1, "alpha", "the quick brown fox"))
        .expect("add 1");
    writer
        .add_doc(one_mail_doc(2, "beta", "lazy dog naps"))
        .expect("add 2");
    let sealed = writer.seal_encrypted(&master).expect("seal_encrypted");

    let reader = Index::open_encrypted(&sealed, &master).expect("open_encrypted");
    let hits = reader
        .query("fox", &[ContentKind::Mail], None, 10)
        .expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id.0, vec![1]);
}

#[test]
fn index_open_encrypted_with_wrong_master_fails() {
    let alice = key(50);
    let bob = key(51);
    let mut writer = Index::create_in_ram().expect("create");
    writer
        .add_doc(one_mail_doc(1, "secret", "alice only"))
        .expect("add");
    let sealed = writer.seal_encrypted(&alice).expect("seal_encrypted");

    // `.err().expect` rather than `expect_err`: `Index` wraps a live Tantivy
    // index and deliberately implements no `Debug`.
    #[allow(clippy::err_expect)]
    let err = Index::open_encrypted(&sealed, &bob)
        .err()
        .expect("opening with wrong master must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn rewrap_segment_master_key_swaps_only_the_header() {
    let alice = key(70);
    let bob = key(71);
    let plaintext = b"body untouched by rotation".to_vec();
    let sealed_alice = seal_segment_bytes(&plaintext, &alice).expect("seal");
    let sealed_bob = rewrap_segment_master_key(&sealed_alice, &alice, &bob).expect("rewrap");

    // Body region (offset 80 onwards: body_nonce + body) is byte-identical:
    // rotation must not touch the body.
    assert_eq!(
        &sealed_alice[OFF_BODY_NONCE..],
        &sealed_bob[OFF_BODY_NONCE..],
        "rewrap must not touch body_nonce or body"
    );
    // Header region (header_nonce + wrapped_dk) IS different:
    assert_ne!(
        &sealed_alice[OFF_HEADER_NONCE..OFF_BODY_NONCE],
        &sealed_bob[OFF_HEADER_NONCE..OFF_BODY_NONCE],
        "rewrap must regenerate header_nonce + wrapped_dk"
    );

    // The rewrapped blob opens under bob, not under alice.
    assert_eq!(
        open_segment_bytes(&sealed_bob, &bob).expect("open with bob"),
        plaintext
    );
    let err = open_segment_bytes(&sealed_bob, &alice)
        .expect_err("alice should no longer open the rotated blob");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn rewrap_segment_with_wrong_old_master_fails() {
    let alice = key(80);
    let mallory = key(81);
    let bob = key(82);
    let plaintext = b"alice's data".to_vec();
    let sealed = seal_segment_bytes(&plaintext, &alice).expect("seal");
    let err = rewrap_segment_master_key(&sealed, &mallory, &bob)
        .expect_err("rewrap with wrong old master must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("unwrap data key failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn master_direct_round_trip() {
    let master = key(110);
    let plaintext = b"manifest-shaped data".to_vec();
    let sealed = seal_under_master(&plaintext, &master).expect("seal");
    let opened = open_under_master(&sealed, &master).expect("open");
    assert_eq!(opened, plaintext);
}

#[test]
fn master_direct_rejects_segment_format() {
    // Confirm the master-direct opener rejects a segment-format blob (magic
    // mismatch) so callers can't accidentally pass the wrong shape.
    let master = key(111);
    let segment_blob = seal_segment_bytes(b"x", &master).expect("seal segment");
    let err = open_under_master(&segment_blob, &master)
        .expect_err("master-direct opener must reject segment magic");
    let msg = format!("{err}");
    assert!(
        msg.contains("bad master-direct magic"),
        "unexpected message: {msg}"
    );
}

#[test]
fn segment_open_rejects_master_direct_format() {
    // And the reverse — segment opener rejects a master-direct blob.
    let master = key(112);
    let direct_blob = seal_under_master(b"x", &master).expect("seal direct");
    let err = open_segment_bytes(&direct_blob, &master)
        .expect_err("segment opener must reject master-direct magic");
    let msg = format!("{err}");
    assert!(
        msg.contains("bad segment magic"),
        "unexpected message: {msg}"
    );
}

#[test]
fn master_direct_rejects_tampered_body() {
    let master = key(113);
    let plaintext = b"intact".to_vec();
    let mut sealed = seal_under_master(&plaintext, &master).expect("seal");
    let body_byte = 32 + 2; // header is 32 bytes; flip a byte in the body
    sealed[body_byte] ^= 0x01;
    let err = open_under_master(&sealed, &master).expect_err("flipped body byte must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt master-direct body failed"),
        "unexpected message: {msg}"
    );
}

#[test]
fn master_direct_rejects_wrong_master() {
    let alice = key(114);
    let bob = key(115);
    let sealed = seal_under_master(b"alice", &alice).expect("seal");
    let err = open_under_master(&sealed, &bob).expect_err("wrong master must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt master-direct body failed"),
        "unexpected message: {msg}"
    );
}

// ───────────────────────── v2 per-kind split (S1, 2026-08-02) ─────────────────────────

use fauna_index::{
    IndexSegmentKey, open_segment_bytes_mailcal, open_under_mailcal_key,
    rewrap_segment_mailcal_key, seal_segment_bytes_mailcal, seal_under_mailcal_key,
};

fn seg_key(seed: u8) -> IndexSegmentKey {
    IndexSegmentKey::from_bytes([seed; 32])
}

fn master_key(seed: u8) -> fauna_index::IndexMasterKey {
    fauna_index::IndexMasterKey::from_bytes([seed; 32])
}

#[test]
fn mailcal_segment_seal_open_round_trip() {
    let key = seg_key(50);
    let plaintext = b"a mail-kind tantivy segment".to_vec();
    let sealed = seal_segment_bytes_mailcal(&plaintext, &key).expect("seal");
    let opened = open_segment_bytes_mailcal(&sealed, &key).expect("open");
    assert_eq!(opened, plaintext);
}

/// The blast-radius property at the byte level: a segment wrapped under one
/// class's key never opens under the other's — in either direction — even when
/// the raw key bytes differ only by role.
#[test]
fn segment_keys_do_not_cross_open() {
    let plaintext = b"class-bound bytes".to_vec();

    let mailcal_sealed =
        seal_segment_bytes_mailcal(&plaintext, &seg_key(51)).expect("seal mailcal");
    let err = open_segment_bytes(&mailcal_sealed, &master_key(52))
        .expect_err("a mailcal segment must not open under a (different-bytes) master key");
    assert!(format!("{err}").contains("unwrap data key failed"));

    let master_sealed = seal_segment_bytes(&plaintext, &master_key(53)).expect("seal master");
    let err = open_segment_bytes_mailcal(&master_sealed, &seg_key(54))
        .expect_err("a master segment must not open under a (different-bytes) segment key");
    assert!(format!("{err}").contains("unwrap data key failed"));
}

/// The MSEK-rotation rewrap pass (`key-material-hierarchy.md` § Path B-sibling-4):
/// old-generation → new-generation segment-key rewrap keeps the body and the
/// AAD-bound prefix byte-identical, O(header).
#[test]
fn mailcal_rewrap_preserves_body_and_prefix() {
    let old = seg_key(60);
    let new = seg_key(61);
    let plaintext = b"a mail segment surviving an MSEK rotation".to_vec();
    let sealed = seal_segment_bytes_mailcal(&plaintext, &old).expect("seal");

    let rewrapped = rewrap_segment_mailcal_key(&sealed, &old, &new).expect("rewrap");
    assert_eq!(rewrapped[..8], sealed[..8], "prefix verbatim");
    assert_eq!(
        open_segment_bytes_mailcal(&rewrapped, &new).expect("opens under the new generation"),
        plaintext
    );
    assert!(
        open_segment_bytes_mailcal(&rewrapped, &old).is_err(),
        "the old generation no longer opens the rewrapped header"
    );
}

#[test]
fn mailcal_direct_seal_open_round_trip_and_wrong_key_refusal() {
    let key = seg_key(70);
    let plaintext = b"manifest-mailcal.idx bytes".to_vec();
    let sealed = seal_under_mailcal_key(&plaintext, &key).expect("seal");
    assert_eq!(
        open_under_mailcal_key(&sealed, &key).expect("open"),
        plaintext
    );
    let err = open_under_mailcal_key(&sealed, &seg_key(71)).expect_err("wrong key must refuse");
    assert!(format!("{err}").contains("decrypt master-direct body failed"));
}
