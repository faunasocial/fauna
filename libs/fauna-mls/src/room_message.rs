//! **A community room's authored message** — the signed authorship half of the
//! community class's content plane (`docs/goal/behavior/conversation-rooms.md`
//! § The three classes → *Community* → *Who wrote it*).
//!
//! # Why a signature at all
//!
//! An end-to-end room's bubbles are attributed by MLS: the framing carries the
//! sender's leaf and the engine authenticates it, which is why
//! [`crate::types::ChannelMessage`] can simply *have* a `sender`. A community
//! room has no MLS group. Its content seals under **one room generation key
//! that every member holds** ([`fauna_core::group_content`]), and a key every
//! member holds authenticates nobody: any member could seal a body and the
//! nest log would carry it indistinguishably from any other member's. The
//! channel log cannot supply the missing half either — a `conv` record
//! persists no sender by design (`moderation.md` § Implementation status
//! today), and a nest-asserted author would be an attribution no member could
//! check.
//!
//! So the author signs and every reader verifies: a bubble's name comes from
//! the author's own actor key, exactly as a room policy's does
//! ([`crate::room_policy::SignedRoomPolicy`]) and a generation mint's does
//! (`fauna_core::group_generation::sign_group_mint_as_minter`).
//!
//! # What the signature covers, and why each part
//!
//! [`RoomMessageCore`], canonical dag-cbor under [`TAG_ROOM_MESSAGE`] in the
//! injective `[tag_len] ‖ tag ‖ canonical` framing the room-policy plane uses,
//! so one plane's signature can never be reframed as another's:
//!
//! - **`room`** — a member of two rooms must not be able to lift a co-member's
//!   bubble out of one and into the other. The AEAD binds only the generation.
//! - **`generation`** — a member *holds* the generation key, so it could
//!   re-seal a co-member's plaintext under a later generation and replay it
//!   past a rotation. Binding it here makes that a refusal; the AEAD's AAD
//!   binds it only against a party that cannot re-seal.
//! - **`author`** — the claim itself, checked as the verifying key.
//! - **`sent_at_ms`** — the author's own stamp, so a bubble renders at the time
//!   its author gave it rather than at the nest's arrival order, and a replay
//!   under a fresh `seq` cannot silently re-date it.
//! - **`body`** — the content.
//!
//! **Not covered: the log `seq`.** The nest allocates it after the author
//! signs, so it cannot be — which is why the reader's dedup key stays
//! `conv:<channel>:<seq>` and this signature is about *who and what*, never
//! *where in the log*.
//!
//! # What a verified signature does NOT establish
//!
//! That the author is (or ever was) a member of the room. The signature says
//! these bytes came from that actor; **membership is the floor roster's
//! answer** (`conversation-rooms.md` § The floor roster), and the reader binds
//! the two. This module deliberately stops at the cryptographic claim — the
//! same split [`crate::room_policy`] draws between "this policy is signed by
//! X" and "X was allowed to sign it".

use ed25519_dalek::Signer;
use fauna_core::crypto::GenerationKey;
use fauna_core::group_content::{
    ROOM_ATTACHMENT_CONTENT_KIND, ROOM_MESSAGE_CONTENT_KIND, open_group_content, seal_group_content,
};
use fauna_core::identity::{ActorId, ActorKeypair, verify_detached};
use serde::{Deserialize, Serialize};

use crate::error::{MlsError, Result};
use crate::types::ChannelMessageBody;

/// Domain tag the author's signature covers.
pub const TAG_ROOM_MESSAGE: &str = "fauna.room.message.v1 2026-09-10";

/// The authored facts of one community-room message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomMessageCore {
    #[serde(with = "serde_bytes")]
    pub room: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub generation: Vec<u8>,
    pub author: ActorId,
    pub sent_at_ms: i64,
    pub body: ChannelMessageBody,
}

/// [`RoomMessageCore`] plus its author's detached signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedRoomMessage {
    pub core: RoomMessageCore,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl RoomMessageCore {
    /// Sign these facts as `signer`.
    ///
    /// # Errors
    /// [`MlsError::Encoding`] if the core does not canonically encode.
    pub fn sign(mut self, signer: &ActorKeypair) -> Result<SignedRoomMessage> {
        self.author = signer.actor_id();
        let canonical = canonical(&self)?;
        let signature = signer
            .signing_key()
            .sign(&signing_input(TAG_ROOM_MESSAGE, &canonical))
            .to_bytes()
            .to_vec();
        Ok(SignedRoomMessage {
            core: self,
            signature,
        })
    }
}

impl SignedRoomMessage {
    /// Check the author's signature, and that the core was authored for
    /// **this** room and **this** generation.
    ///
    /// # Errors
    /// [`MlsError::PolicyViolation`] for a room/generation the core does not
    /// name, or a signature that does not verify under its claimed author.
    pub fn verify(&self, room: &[u8; 32], generation: &[u8; 32]) -> Result<()> {
        if self.core.room.as_slice() != room.as_slice() {
            return Err(MlsError::PolicyViolation(
                "room message was authored for another room".into(),
            ));
        }
        if self.core.generation.as_slice() != generation.as_slice() {
            return Err(MlsError::PolicyViolation(
                "room message was authored under another generation".into(),
            ));
        }
        let canonical = canonical(&self.core)?;
        if !verify_detached(
            &self.core.author.0,
            &signing_input(TAG_ROOM_MESSAGE, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "room message signature does not verify under its claimed author".into(),
            ));
        }
        Ok(())
    }
}

/// Author one community-room message and seal it under the room's generation
/// key.
///
/// # Errors
/// [`MlsError::Encoding`] on an encode or seal failure.
pub fn seal_room_message(
    gen_key: &GenerationKey,
    room: &[u8; 32],
    generation: &[u8; 32],
    author: &ActorKeypair,
    sent_at_ms: i64,
    body: ChannelMessageBody,
) -> Result<Vec<u8>> {
    let signed = RoomMessageCore {
        room: room.to_vec(),
        generation: generation.to_vec(),
        author: author.actor_id(),
        sent_at_ms,
        body,
    }
    .sign(author)?;
    let plaintext = canonical(&signed)?;
    seal_group_content(gen_key, ROOM_MESSAGE_CONTENT_KIND, generation, &plaintext)
        .map_err(MlsError::Encoding)
}

/// Open one community-room message and verify its authorship.
///
/// # Errors
/// [`MlsError::Encoding`] when the envelope does not open or decode;
/// [`MlsError::PolicyViolation`] from [`SignedRoomMessage::verify`].
pub fn open_room_message(
    gen_key: &GenerationKey,
    room: &[u8; 32],
    generation: &[u8; 32],
    sealed: &[u8],
) -> Result<SignedRoomMessage> {
    let plaintext = open_group_content(gen_key, ROOM_MESSAGE_CONTENT_KIND, generation, sealed)
        .map_err(MlsError::Encoding)?;
    let signed = fauna_protocol::codec::decode_strict::<SignedRoomMessage>(&plaintext)
        .map_err(|e| MlsError::Encoding(format!("room message did not decode: {e}")))?;
    signed.verify(room, generation)?;
    Ok(signed)
}

/// Seal one community-room **attachment** under the room's attachment content
/// kind, bound to the generation of the message that will name it
/// (`conversation-rooms.md` § The three classes → *Community* → *Attachments —
/// the second content kind*). The end-to-end class's twin is
/// `MlsEngine::seal_conversation_blob`; here there is no epoch — the reader
/// takes the generation from the `RoomSealed` envelope, which the author's
/// signature binds, so a `ChannelAttachment::epoch` on this class is written
/// `0` and never read.
///
/// Returns the sealed bytes; the caller content-addresses them (the BLAKE3 of
/// the sealed bytes is the blob's `sealed_cid`) and uploads them to the room's
/// home nest. Convergent by construction — two members attaching one file
/// under one generation store one blob — and openable by every holder of the
/// generation's wrap, the home nest included: the class, not the kind, decides
/// who reads.
///
/// # Errors
/// [`MlsError::Encoding`] on a seal failure.
pub fn seal_room_attachment(
    gen_key: &GenerationKey,
    generation: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    seal_group_content(gen_key, ROOM_ATTACHMENT_CONTENT_KIND, generation, plaintext)
        .map_err(MlsError::Encoding)
}

/// Open one community-room attachment sealed by [`seal_room_attachment`],
/// under the generation the naming message was sealed under.
///
/// # Errors
/// [`MlsError::Encoding`] when the blob does not open — another generation's
/// key, the message kind's key, or tampered bytes, deliberately
/// indistinguishable.
pub fn open_room_attachment(
    gen_key: &GenerationKey,
    generation: &[u8; 32],
    sealed: &[u8],
) -> Result<Vec<u8>> {
    open_group_content(gen_key, ROOM_ATTACHMENT_CONTENT_KIND, generation, sealed)
        .map_err(MlsError::Encoding)
}

/// What a published labeler reads of one **verified** community-room message
/// — the `label()` ABI's post-shaped [`LabelerPostInput`]: the community
/// room's content-kind mapping, as `fauna_ffi::labeler::mail_to_labeler_input_bare`
/// is mail's (`conversation-rooms.md` § The three classes → *What the home
/// nest does with its read*, purpose 2).
///
/// `text` is what the member typed — the message, or an attachment's caption
/// — and `None` rather than empty when there is none. The attachments ride as
/// their **declared metadata** only (`has_media`, and the first one's MIME
/// type — the ABI carries one): the input has no bytes facet, so an image
/// classifier cannot see an image through it yet, which § *What the read
/// covers* records as the label build's open half. `author` is the signed
/// author, never a placeholder: a room message, unlike an external mail
/// sender, always has one. No hashtag facet exists on a message, so a labeler
/// that asks for hashtags gets none.
///
/// `None` for control traffic, which is not content and never reaches a
/// derived view.
pub fn labeler_input(signed: &SignedRoomMessage) -> Option<fauna_core::scoring::LabelerPostInput> {
    let (text, attachments) = match &signed.core.body {
        ChannelMessageBody::Text(text) => (text.as_str(), &[][..]),
        ChannelMessageBody::Attachments { body, attachments } => {
            (body.as_str(), attachments.as_slice())
        }
        _ => return None,
    };
    Some(fauna_core::scoring::LabelerPostInput {
        text: (!text.is_empty()).then(|| text.to_string()),
        hashtags: Vec::new(),
        has_media: !attachments.is_empty(),
        media_type: attachments.first().map(|a| a.mime_type.clone()),
        duration_ms: None,
        author: signed.core.author,
    })
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fauna_cbor::encode_canonical(value)
        .map_err(|e| MlsError::Encoding(format!("room message canonical encode: {e}")))
        .map(|b| b.to_vec())
}

/// `[tag_len: u8] ‖ tag ‖ canonical` — the succession/room-policy planes'
/// injective framing, reused verbatim so one plane's signature can never be
/// reframed as a record of another.
fn signing_input(tag: &str, canonical: &[u8]) -> Vec<u8> {
    let tag_len = u8::try_from(tag.len()).expect("room message domain tag is < 256 bytes");
    let mut out = Vec::with_capacity(1 + tag.len() + canonical.len());
    out.push(tag_len);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(canonical);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    #[test]
    fn an_attachment_opens_under_its_generation_and_only_under_the_attachment_kind() {
        // The ruling's two edges in one place: a picture and the message that
        // names it seal under one generation but never under one key, and a
        // later generation — a removal's mint — opens nothing attached before it.
        let gen_key = GenerationKey::mint();
        let generation = [0x11u8; 32];
        let sealed = seal_room_attachment(&gen_key, &generation, b"png bytes").unwrap();
        assert_ne!(&sealed[12..], b"png bytes");
        assert_eq!(
            open_room_attachment(&gen_key, &generation, &sealed).unwrap(),
            b"png bytes"
        );
        assert!(
            open_group_content(&gen_key, ROOM_MESSAGE_CONTENT_KIND, &generation, &sealed).is_err(),
            "the message kind's key does not open an attachment"
        );
        assert!(
            open_room_attachment(&GenerationKey::mint(), &generation, &sealed).is_err(),
            "another generation's key does not open it"
        );
        assert!(
            open_room_attachment(&gen_key, &[0x12u8; 32], &sealed).is_err(),
            "relabelling the generation is a refusal, as for a message"
        );
    }

    fn text(body: &ChannelMessageBody) -> &str {
        match body {
            ChannelMessageBody::Text(t) => t.as_str(),
            other => panic!("expected a text body, got {other:?}"),
        }
    }

    /// Seal arbitrary already-signed bytes the way a *member* could: it holds
    /// the generation key, so nothing stops it sealing whatever it likes. Every
    /// refusal below has to come from the signature, never from the AEAD.
    fn seal_as_member(
        gen_key: &GenerationKey,
        generation: &[u8; 32],
        signed: &SignedRoomMessage,
    ) -> Vec<u8> {
        seal_group_content(
            gen_key,
            ROOM_MESSAGE_CONTENT_KIND,
            generation,
            &canonical(signed).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn an_authored_message_round_trips_attributed() {
        let author = kp(1);
        let key = GenerationKey::mint();
        let room = [7u8; 32];
        let generation = [9u8; 32];

        let sealed = seal_room_message(
            &key,
            &room,
            &generation,
            &author,
            1_700_000_000_000,
            ChannelMessageBody::Text("the square is open".into()),
        )
        .expect("an author seals its own message");

        let opened = open_room_message(&key, &room, &generation, &sealed)
            .expect("a member holding the generation opens it");
        assert_eq!(opened.core.author, author.actor_id());
        assert_eq!(opened.core.sent_at_ms, 1_700_000_000_000);
        assert_eq!(text(&opened.core.body), "the square is open");
    }

    /// The property the whole module exists for: **a co-member cannot put
    /// words in another member's mouth.** Every member of a community room
    /// holds the same generation key, so the attacker here seals a perfectly
    /// well-formed envelope — only the signature can refuse it.
    #[test]
    fn a_co_member_cannot_forge_another_members_bubble() {
        let victim = kp(1);
        let attacker = kp(2);
        let key = GenerationKey::mint();
        let room = [7u8; 32];
        let generation = [9u8; 32];

        // The attacker signs as itself, then relabels the author.
        let mut forged = RoomMessageCore {
            room: room.to_vec(),
            generation: generation.to_vec(),
            author: attacker.actor_id(),
            sent_at_ms: 1_700_000_000_000,
            body: ChannelMessageBody::Text("I resign as owner".into()),
        }
        .sign(&attacker)
        .unwrap();
        forged.core.author = victim.actor_id();

        let sealed = seal_as_member(&key, &generation, &forged);
        let err = open_room_message(&key, &room, &generation, &sealed)
            .expect_err("a relabelled author must be refused");
        assert!(
            matches!(err, MlsError::PolicyViolation(_)),
            "expected a policy refusal, got {err:?}"
        );
    }

    /// A member of two rooms must not be able to lift a co-member's bubble out
    /// of one and into the other: the AEAD binds only the generation, so the
    /// signature has to bind the room.
    #[test]
    fn a_bubble_lifted_into_another_room_is_refused() {
        let author = kp(1);
        let key = GenerationKey::mint();
        let generation = [9u8; 32];
        let square = [7u8; 32];
        let annex = [8u8; 32];

        let signed = RoomMessageCore {
            room: square.to_vec(),
            generation: generation.to_vec(),
            author: author.actor_id(),
            sent_at_ms: 1,
            body: ChannelMessageBody::Text("meet at six".into()),
        }
        .sign(&author)
        .unwrap();

        let sealed = seal_as_member(&key, &generation, &signed);
        let err = open_room_message(&key, &annex, &generation, &sealed)
            .expect_err("a bubble authored for another room must be refused");
        assert!(
            matches!(err, MlsError::PolicyViolation(_)),
            "expected a policy refusal, got {err:?}"
        );
    }

    /// A member *holds* the generation key, so it can re-seal a co-member's
    /// plaintext under a later generation and replay it past a rotation. The
    /// AEAD cannot refuse that — it is a legitimate seal by a key-holder — so
    /// the signature binds the generation too.
    #[test]
    fn a_bubble_replayed_under_a_later_generation_is_refused() {
        let author = kp(1);
        let room = [7u8; 32];
        let old_gen = [9u8; 32];
        let new_gen = [10u8; 32];
        let new_key = GenerationKey::mint();

        let signed = RoomMessageCore {
            room: room.to_vec(),
            generation: old_gen.to_vec(),
            author: author.actor_id(),
            sent_at_ms: 1,
            body: ChannelMessageBody::Text("still here".into()),
        }
        .sign(&author)
        .unwrap();

        let resealed = seal_as_member(&new_key, &new_gen, &signed);
        let err = open_room_message(&new_key, &room, &new_gen, &resealed)
            .expect_err("a bubble replayed under a later generation must be refused");
        assert!(
            matches!(err, MlsError::PolicyViolation(_)),
            "expected a policy refusal, got {err:?}"
        );
    }

    /// Editing a co-member's words is the same act as forging them.
    #[test]
    fn an_edited_body_is_refused() {
        let author = kp(1);
        let key = GenerationKey::mint();
        let room = [7u8; 32];
        let generation = [9u8; 32];

        let mut signed = RoomMessageCore {
            room: room.to_vec(),
            generation: generation.to_vec(),
            author: author.actor_id(),
            sent_at_ms: 1,
            body: ChannelMessageBody::Text("yes".into()),
        }
        .sign(&author)
        .unwrap();
        signed.core.body = ChannelMessageBody::Text("no".into());

        let sealed = seal_as_member(&key, &generation, &signed);
        let err = open_room_message(&key, &room, &generation, &sealed)
            .expect_err("an edited body must be refused");
        assert!(
            matches!(err, MlsError::PolicyViolation(_)),
            "expected a policy refusal, got {err:?}"
        );
    }

    /// `sign` stamps the signer's own actor id over whatever the caller put in
    /// the field, so a core signed as one actor and labelled another is not a
    /// shape this module will produce.
    #[test]
    fn signing_stamps_the_signers_own_actor_id() {
        let signer = kp(1);
        let other = kp(2);
        let signed = RoomMessageCore {
            room: [7u8; 32].to_vec(),
            generation: [9u8; 32].to_vec(),
            author: other.actor_id(),
            sent_at_ms: 1,
            body: ChannelMessageBody::Text("hello".into()),
        }
        .sign(&signer)
        .unwrap();
        assert_eq!(signed.core.author, signer.actor_id());
    }

    fn signed_body(author: &ActorKeypair, body: ChannelMessageBody) -> SignedRoomMessage {
        RoomMessageCore {
            room: [7u8; 32].to_vec(),
            generation: [9u8; 32].to_vec(),
            author: author.actor_id(),
            sent_at_ms: 1,
            body,
        }
        .sign(author)
        .unwrap()
    }

    fn attachment(mime: &str) -> crate::types::ChannelAttachment {
        crate::types::ChannelAttachment {
            blob_hash: fauna_core::encoding::content_hash(mime.as_bytes()),
            sealed_cid: fauna_core::encoding::content_hash(b"sealed"),
            filename: "f".into(),
            mime_type: mime.into(),
            size_bytes: 1,
            is_image: mime.starts_with("image/"),
            epoch: 0,
        }
    }

    #[test]
    fn a_room_message_maps_to_the_labeler_input_its_author_wrote() {
        let author = kp(1);
        let text = labeler_input(&signed_body(&author, ChannelMessageBody::Text("hi".into())))
            .expect("a text message is content");
        assert_eq!(text.text.as_deref(), Some("hi"));
        assert!(!text.has_media);
        assert_eq!(text.media_type, None);
        assert_eq!(
            text.author,
            author.actor_id(),
            "the signed author, never a placeholder"
        );

        let captioned = labeler_input(&signed_body(
            &author,
            ChannelMessageBody::Attachments {
                body: "look".into(),
                attachments: vec![attachment("image/png"), attachment("video/mp4")],
            },
        ))
        .unwrap();
        assert_eq!(captioned.text.as_deref(), Some("look"));
        assert!(captioned.has_media);
        assert_eq!(captioned.media_type.as_deref(), Some("image/png"));

        let bare = labeler_input(&signed_body(
            &author,
            ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![attachment("image/png")],
            },
        ))
        .unwrap();
        assert_eq!(
            bare.text, None,
            "an empty caption is no text, not empty text"
        );
        assert!(bare.has_media);
    }
}
