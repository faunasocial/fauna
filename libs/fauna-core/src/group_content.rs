//! **Per-kind content keys under a storage-group generation key** — the
//! content half of the recipient-set scheme
//! (`key-material-hierarchy.md` § Audience: a storage group, the *Per-kind
//! content keys* bullet: "derived from the generation key per kind context
//! (`BLAKE3::derive_key`, the T14 stratification pattern) — content seals
//! under the tip").
//!
//! The scheme's other three halves already exist: the generation keys and
//! their roster/mint machinery ([`crate::group_generation`]), the X-Wing
//! wraps that carry a generation key to a member
//! (`fauna_mls::wrapped_blob::group_generation_wraps`), and the machinery
//! root that seals the group's own bookkeeping
//! ([`crate::crypto::GroupMachinerySchedule`]). This module is what a group
//! *content* kind seals with, and its first consumer is a **community
//! conversation room's message log**
//! (`conversation-rooms.md` § The three classes → *Community*): the room's
//! members and its home nest each hold a wrap of the tip generation, so each
//! derives the same content key and opens the same envelope — "differing only
//! in which key the reader holds" is literal here, because there is only one
//! key.
//!
//! **Why a separate branch from the machinery schedule.** Machinery keys are
//! derived from the scope's immutable machinery root, deliberately *never*
//! rotated ([`crate::crypto::GroupMachinerySchedule`]); content keys are
//! derived from the *generation* key, which rotates on every removal. Folding
//! them into one branch would make a rotation either re-seal the whole
//! machinery DAG or sever nothing — the split is what lets a removal sever
//! future content while every ever-admitted member keeps reading the roster
//! history it already holds.
//!
//! **The seal shape is `fauna-mls::blob_crypto::encrypt_blob`'s**, not a new
//! one: ChaCha20-Poly1305 under a two-step derived key, with a *keyed*
//! deterministic nonce so identical plaintext under one generation converges
//! (the content-addressed store dedups by construction) while the 12-byte
//! cleartext prefix stays computable only by holders of the generation's key
//! material — the ratified no-confirmation-oracle / no-correlation property
//! (`owner-key-material.md`; keyed 2026-08-31). What is added over
//! `encrypt_blob` is an **AAD binding the generation id**, because a group
//! content envelope carries that id in cleartext so a reader knows which wrap
//! to open: without the binding, relabelling an envelope to another
//! generation would surface as an opaque AEAD failure instead of a refusal,
//! and a reader that holds both generations could be walked between them.

use crate::crypto::GenerationKey;
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
};

/// Domain-separation context for a group content kind's **AEAD key**.
///
/// Frozen: every group content envelope ever sealed opens through this
/// string, so editing it would make every stored community-room message
/// unopenable by every member and by the home nest alike.
const GROUP_CONTENT_SEAL_CONTEXT: &str = "fauna.group.content.seal.v1 2026-09-09";

/// Domain-separation context for a group content kind's **nonce-derivation
/// key**, separate from the AEAD key's so neither derivation constrains the
/// other. Frozen for the same reason.
const GROUP_CONTENT_NONCE_CONTEXT: &str = "fauna.group.content.nonce.v1 2026-09-09";

/// The content kind a community conversation room's message log seals under —
/// the scheme's first content kind. Kept beside the derivation rather than in
/// `fauna-protocol` so the nest and the apps read one spelling and this crate
/// stays the bottom of the dependency edge.
pub const ROOM_MESSAGE_CONTENT_KIND: &str = "fauna.conversations.room.message";

/// The content kind a community conversation room's **attachment blobs** seal
/// under — the scheme's second content kind, off the same generation as the
/// message that names the blob (`conversation-rooms.md` § The three classes →
/// *Community* → *Attachments — the second content kind*, ratified
/// 2026-09-10). A second kind rather than the message key because an
/// attachment is a different record — raw bytes with no signed core of their
/// own; the authorship rides in the message whose signature covers the blob's
/// `sealed_cid` — and because keeping the kinds apart is what leaves a
/// narrower reader position representable later. Every holder of the
/// generation's wrap derives it, the room's home nest included: a kind cannot
/// narrow the audience, only a roster can (`key-material-hierarchy.md`
/// § Audience: a storage group → *Per-kind content keys*).
pub const ROOM_ATTACHMENT_CONTENT_KIND: &str = "fauna.conversations.room.attachment";

/// The content kind a **room-restricted post** addressed to a community room
/// derives its base key under — the scheme's third content kind, off the
/// generation the post names (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the ruling*, ruling 4, 2026-09-10).
///
/// Unlike the other two, this kind does not seal the record itself: a post is
/// its author's ordinary post and rests in its author's `__post` plane, never
/// in the room's log, so it keeps the Posts row's own seal
/// (`derive_post_key(base, seal_id)` + `encrypt_content`). This kind names only
/// the **base** that seal starts from ([`room_post_base_key`]) — kept apart from
/// the message kind so a room's posts and messages never share a key.
pub const ROOM_POST_CONTENT_KIND: &str = "fauna.conversations.room.post";

/// The base key a room-restricted post addressed to a community room seals
/// under: the generation key it names, domain-separated for posts. The
/// per-post key is `derive_post_key(this, seal_id)`.
///
/// One spelling for every reader — the author's device sealing, a member's
/// device opening, and the room's home nest opening at reception — so none of
/// them can pick the wrong kind.
#[must_use]
pub fn room_post_base_key(gen_key: &GenerationKey) -> [u8; 32] {
    group_content_key(gen_key, ROOM_POST_CONTENT_KIND)
}

/// Derive one kind's AEAD key from a generation key.
///
/// The ratified two-step ([`crate::domain_key`]): `derive_key` isolates the
/// plane, `keyed_hash` folds in the kind, so two content kinds of one group
/// never share a key and a group's content branch is unrelated to its
/// machinery branch or to any account-plane branch on the same 32 bytes.
#[must_use]
pub fn group_content_key(gen_key: &GenerationKey, kind: &str) -> [u8; 32] {
    crate::domain_key::derive_domain_key(
        GROUP_CONTENT_SEAL_CONTEXT,
        gen_key.as_bytes(),
        kind.as_bytes(),
    )
}

/// The nonce-derivation key for one kind, domain-separated from
/// [`group_content_key`].
fn group_content_nonce_key(gen_key: &GenerationKey, kind: &str) -> [u8; 32] {
    crate::domain_key::derive_domain_key(
        GROUP_CONTENT_NONCE_CONTEXT,
        gen_key.as_bytes(),
        kind.as_bytes(),
    )
}

/// Seal one group content record under `gen_key`, bound to the generation
/// that minted it.
///
/// Returns `nonce (12 bytes) || ciphertext`. `generation_id` is **not** stored
/// here — the carrier records it in cleartext beside the bytes (for a room
/// message, `ChannelEnvelope::RoomSealed { generation, .. }`) — it is bound as
/// AAD so a relabelled envelope is a crisp refusal.
///
/// # Errors
/// Only an AEAD failure, which is unreachable for a 32-byte key and a
/// 12-byte nonce; never swallowed.
pub fn seal_group_content(
    gen_key: &GenerationKey,
    kind: &str,
    generation_id: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let key = group_content_key(gen_key, kind);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce_key = group_content_nonce_key(gen_key, kind);
    let nonce_bytes = blake3::keyed_hash(&nonce_key, plaintext);
    let nonce = Nonce::from_slice(&nonce_bytes.as_bytes()[..12]);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: generation_id,
            },
        )
        .map_err(|e| format!("seal group content: {e}"))?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes.as_bytes()[..12]);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open one group content record. Input: `nonce (12 bytes) || ciphertext`.
///
/// # Errors
/// A body under 12 bytes, or an AEAD failure — a wrong generation key, a
/// wrong kind, a `generation_id` that does not match the one sealed in, or
/// tampered bytes. The variants are deliberately not distinguished: telling
/// a caller *which* of those it got is an oracle.
pub fn open_group_content(
    gen_key: &GenerationKey,
    kind: &str,
    generation_id: &[u8; 32],
    sealed: &[u8],
) -> Result<Vec<u8>, String> {
    if sealed.len() < 12 {
        return Err("group content envelope is shorter than its nonce".into());
    }
    let key = group_content_key(gen_key, kind);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = Nonce::from_slice(&sealed[..12]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &sealed[12..],
                aad: generation_id,
            },
        )
        .map_err(|_| "group content envelope did not open".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genk(byte: u8) -> GenerationKey {
        GenerationKey::from_bytes([byte; 32])
    }

    #[test]
    fn a_group_content_record_round_trips_under_its_generation() {
        let key = genk(1);
        let sealed =
            seal_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[9u8; 32], b"hello room").unwrap();
        assert_ne!(&sealed[12..], b"hello room");
        let opened =
            open_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[9u8; 32], &sealed).unwrap();
        assert_eq!(opened, b"hello room");
    }

    #[test]
    fn a_later_generations_key_does_not_open_an_earlier_generations_record() {
        // The severance property the whole scheme exists for: a removal mints
        // a fresh generation, and a member holding only the new key reads
        // nothing sealed under the old one.
        let sealed =
            seal_group_content(&genk(1), ROOM_MESSAGE_CONTENT_KIND, &[9u8; 32], b"before").unwrap();
        assert!(
            open_group_content(&genk(2), ROOM_MESSAGE_CONTENT_KIND, &[9u8; 32], &sealed).is_err()
        );
    }

    #[test]
    fn relabelling_the_generation_id_is_refused_rather_than_silently_failing() {
        // The id rides in cleartext beside the bytes so a reader knows which
        // wrap to open; the AAD is what stops it being edited.
        let key = genk(1);
        let sealed =
            seal_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[9u8; 32], b"hello").unwrap();
        assert!(open_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[10u8; 32], &sealed).is_err());
    }

    #[test]
    fn the_content_kinds_of_one_group_never_share_a_key() {
        // Every shipped kind against every other: a community room's message
        // key must not open its attachments or its posts, nor any of the
        // reverse — the separation each later kind was ruled for.
        const KINDS: [&str; 3] = [
            ROOM_MESSAGE_CONTENT_KIND,
            ROOM_ATTACHMENT_CONTENT_KIND,
            ROOM_POST_CONTENT_KIND,
        ];
        let key = genk(1);
        for (i, sealer) in KINDS.iter().enumerate() {
            let sealed = seal_group_content(&key, sealer, &[9u8; 32], b"hello").unwrap();
            for (j, opener) in KINDS.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    open_group_content(&key, opener, &[9u8; 32], &sealed).is_err(),
                    "{opener} opened what {sealer} sealed"
                );
                assert_ne!(
                    group_content_key(&key, sealer),
                    group_content_key(&key, opener),
                    "{sealer} and {opener} share a key"
                );
            }
        }
    }

    #[test]
    fn the_post_kinds_derivation_is_pinned_by_a_known_answer() {
        // Frozen with the other two: this vector fails the moment the kind
        // string changes, which is exactly when every room-restricted post a
        // community member sealed would become unopenable by every member and
        // by the home nest alike.
        let k = room_post_base_key(&GenerationKey::from_bytes([1u8; 32]));
        assert_eq!(
            k,
            group_content_key(
                &GenerationKey::from_bytes([1u8; 32]),
                ROOM_POST_CONTENT_KIND
            ),
            "the post base key IS the post kind's content key"
        );
        assert_eq!(
            hex::encode(k),
            "136b4143e27a45cbfa2366014f1526e3e95f3f79d0d73175d274821c1b57bffb",
            "the room post key derivation is frozen"
        );
    }

    #[test]
    fn the_attachment_kinds_derivation_is_pinned_by_a_known_answer() {
        // Frozen alongside the message kind's: this vector fails the moment the
        // kind string changes, which is exactly when every stored community-room
        // attachment would become unopenable by every member and by the home
        // nest alike.
        let k = group_content_key(
            &GenerationKey::from_bytes([1u8; 32]),
            ROOM_ATTACHMENT_CONTENT_KIND,
        );
        assert_eq!(
            hex::encode(k),
            "88453a7f15b4fee754ecbe0af61817be66f50a0d2deca9670ff53a414420ab25",
            "the room attachment key derivation is frozen"
        );
    }

    #[test]
    fn the_content_branch_is_independent_of_the_machinery_branch_and_of_every_account_branch() {
        // The plane split's load-bearing property, the sibling of
        // `crypto::tests::the_group_machinery_branch_is_independent_of_every_account_branch`:
        // the same 32 bytes pushed through the content contexts and through
        // the machinery/account contexts must yield unrelated roots, so a
        // reader holding one group's content key learns nothing about its
        // machinery or about any account-state branch.
        let bytes = [7u8; 32];
        let content =
            group_content_key(&GenerationKey::from_bytes(bytes), ROOM_MESSAGE_CONTENT_KIND);
        let nonce =
            group_content_nonce_key(&GenerationKey::from_bytes(bytes), ROOM_MESSAGE_CONTENT_KIND);
        assert_ne!(content, nonce, "the two axes must not collide");

        let machinery = crate::crypto::GroupMachinerySchedule::derive(
            &crate::crypto::GroupMachineryRoot::from_bytes(bytes),
        );
        assert_ne!(content, **machinery.seal_root());
        assert_ne!(content, **machinery.item_blind_root());

        let account = crate::crypto::FleetOnlySchedule::derive_for_generation(
            &GenerationKey::from_bytes(bytes),
        );
        assert_ne!(content, **account.seal_root());
        assert_ne!(content, **account.item_blind_root());
    }

    #[test]
    fn the_derivation_is_pinned_by_a_known_answer() {
        // Frozen contexts: this vector fails the moment either context string
        // or the two-step shape changes, which is exactly when every stored
        // community-room message would become unopenable.
        let k = group_content_key(
            &GenerationKey::from_bytes([1u8; 32]),
            ROOM_MESSAGE_CONTENT_KIND,
        );
        assert_eq!(
            hex::encode(k),
            "3a373cd677e84038a767857d78445e3c1fbbd4d3239f240c3726220274dd69ea",
            "the group content key derivation is frozen"
        );
    }

    #[test]
    fn identical_plaintext_under_one_generation_converges() {
        // Convergent by design (the blob_crypto property): a content-addressed
        // store dedups two identical records without any nonce bookkeeping.
        let key = genk(4);
        let a = seal_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[1u8; 32], b"same").unwrap();
        let b = seal_group_content(&key, ROOM_MESSAGE_CONTENT_KIND, &[1u8; 32], b"same").unwrap();
        assert_eq!(a, b);
        // ...and the nonce is keyed, so a holder of the bytes alone cannot
        // confirm a candidate plaintext by recomputing the prefix.
        let other =
            seal_group_content(&genk(5), ROOM_MESSAGE_CONTENT_KIND, &[1u8; 32], b"same").unwrap();
        assert_ne!(a[..12], other[..12]);
    }
}
