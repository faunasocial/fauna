//! Email payload building and decoding.
//!
//! Email wire shape: canonical dag-cbor-encoded tuple `(EmbedAsBytes-cr, EmbedAsBytes-post)`,
//! where both halves carry the signed kind per the embed-as-bytes contract
//! (`docs/goal/architecture/transport.md` § Embed-as-bytes for signed
//! payloads). Sign-over-CID migration landed in Tasks 2.5–2.6 of the
//! CBOR-DAG-everywhere Layer 2 plan.

use fauna_core::data::{
    ContactRequest, ContentHash, MediaItem, Post, PostBody, StructuredField, Timestamp,
};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, canonical_encode, compute_post_id, decode_signed_bytes,
    sign_envelope,
};
use fauna_core::identity::{ActorId, ActorKeypair};

use crate::ClientError;

const EMAIL_SCHEMA: &str = "email/v1";

// ── Knock wire sentinel ───────────────────────────────────────

/// Wire **subject** of a knock (contact-request) payload. A protocol sentinel
/// the recipient's nest/client matches to recognize an inbound message as a
/// knock — it travels on the wire, so it must **never** be localized (doing so
/// would diverge the wire and break knock detection). Distinct from the
/// `contacts.knock` i18n *button label*, which merely shares the English word.
/// Single source of truth so no client inlines the literal (priority #2).
pub const KNOCK_SUBJECT: &str = "Knock";

/// Wire **body** of a knock (contact-request) payload. Protocol sentinel — see
/// [`KNOCK_SUBJECT`].
pub const KNOCK_BODY: &str = "Contact request";

// ── Public types ──────────────────────────────────────────────

/// A media attachment carried by an email.
pub struct Attachment {
    pub hash: [u8; 32],
    pub media_type: String,
    pub size_bytes: u64,
}

/// A decoded email payload.
pub struct DecodedEmail {
    pub from: [u8; 32],
    pub subject: String,
    pub body: String,
    pub timestamp: u64,
    pub valid: bool,
    pub encrypted: bool,
    pub post_id: Vec<u8>,
    pub sender_node: String,
    pub attachments: Vec<Attachment>,
}

// ── Builder functions ─────────────────────────────────────────

/// Build a signed email payload (ContactRequest + Post) for the inbox.
/// Returns canonical dag-cbor-encoded bytes ready to POST to `/api/v1/inbox/{actor_id}`.
pub fn build_signed_email(
    kp: &ActorKeypair,
    to: &[u8; 32],
    subject: &str,
    body: &str,
    node_url: &str,
) -> Result<Vec<u8>, ClientError> {
    let author = kp.actor_id();
    let recipient = ActorId(*to);
    build_email_post(
        kp,
        author,
        recipient,
        subject,
        body,
        vec![],
        false,
        node_url,
    )
}

/// Build a signed **knock** (contact-request) payload — a [`build_signed_email`]
/// with the canonical knock subject/body ([`KNOCK_SUBJECT`] / [`KNOCK_BODY`])
/// baked in. Single source of truth for the knock wire sentinel so it can't
/// drift per client: every app builds its outbound knock through this one
/// builder (Rust direct, or the `fauna-ffi` / `fauna-wasm` wrappers) rather
/// than passing its own copy of the literal strings (priority #2).
pub fn build_knock_payload(
    kp: &ActorKeypair,
    to: &[u8; 32],
    node_url: &str,
) -> Result<Vec<u8>, ClientError> {
    build_signed_email(kp, to, KNOCK_SUBJECT, KNOCK_BODY, node_url)
}

/// Build a signed email payload with media attachments.
pub fn build_signed_email_with_media(
    kp: &ActorKeypair,
    to: &[u8; 32],
    subject: &str,
    body: &str,
    media: Vec<Attachment>,
    encrypted: bool,
    node_url: &str,
) -> Result<Vec<u8>, ClientError> {
    let author = kp.actor_id();
    let recipient = ActorId(*to);
    let items: Vec<MediaItem> = media
        .into_iter()
        .map(|a| MediaItem {
            blob_hash: ContentHash::from_digest_raw(a.hash),
            media_type: a.media_type,
            size_bytes: a.size_bytes,
            dimensions: None,
            thumbnail: None,
            remote_url: None,
            alt: None,
        })
        .collect();
    build_email_post(
        kp, author, recipient, subject, body, items, encrypted, node_url,
    )
}

/// Decode a canonical dag-cbor-encoded email payload `(EmbedAsBytes-cr, EmbedAsBytes-post)`.
pub fn decode_email(payload: &[u8]) -> Result<DecodedEmail, ClientError> {
    let (post, cr, valid, post_id) = crate::cr_post::decode_verified_cr_post_pair(payload)?;

    let (subject, body) = extract_email_fields(&post);

    let encrypted = match &post.body {
        PostBody::Structured { fields, .. } => fields
            .iter()
            .any(|f| f.key == "encrypted" && f.value == "true"),
        _ => false,
    };

    let sender_node = String::from_utf8(cr.sender_node.clone()).unwrap_or_default();

    let attachments = extract_attachments(&post);

    Ok(DecodedEmail {
        from: cr.sender.0,
        subject,
        body,
        timestamp: post.created_at.0,
        valid,
        encrypted,
        post_id,
        sender_node,
        attachments,
    })
}

/// Decode an email payload and project it to the canonical JSON string the
/// thin client bindings expose (FFI `decode_email`, wasm `decodeEmail`).
///
/// Single source of truth for the wire-facing JSON shape so a field added to
/// [`DecodedEmail`] can't silently drift between the two bindings — both
/// previously hand-built this identical object. Fields: `from`, `subject`,
/// `body`, `timestamp`, `valid`, `encrypted`, `post_id`, `sender_node`,
/// `attachments` (each `{hash, media_type, size_bytes}`); all byte IDs are
/// hex-encoded.
pub fn decode_email_json(payload: &[u8]) -> Result<String, ClientError> {
    let decoded = decode_email(payload)?;

    let attachments_json: Vec<serde_json::Value> = decoded
        .attachments
        .iter()
        .map(|a| {
            serde_json::json!({
                "hash": hex::encode(a.hash),
                "media_type": a.media_type,
                "size_bytes": a.size_bytes,
            })
        })
        .collect();

    let result = serde_json::json!({
        "from": hex::encode(decoded.from),
        "subject": decoded.subject,
        "body": decoded.body,
        "timestamp": decoded.timestamp,
        "valid": decoded.valid,
        "encrypted": decoded.encrypted,
        "post_id": hex::encode(&decoded.post_id),
        "sender_node": decoded.sender_node,
        "attachments": attachments_json,
    });

    serde_json::to_string(&result).map_err(|e| ClientError(format!("json serialize: {e}")))
}

/// Extract the "to" field from a canonical dag-cbor-encoded email payload.
/// Returns a list of 32-byte actor IDs decoded from hex.
pub fn get_recipients(payload: &[u8]) -> Result<Vec<[u8; 32]>, ClientError> {
    let (_cr_wire, post_wire): (EmbedAsBytes, EmbedAsBytes) =
        canonical_decode(payload).map_err(|e| ClientError(format!("decode: {e}")))?;
    let post: Post = decode_signed_bytes(&post_wire.bytes)
        .map_err(|e| ClientError(format!("decode post: {e}")))?;

    let recipients = match &post.body {
        PostBody::Structured { fields, .. } => {
            let to_value = fields
                .iter()
                .find(|f| f.key == "to")
                .map(|f| f.value.as_str())
                .unwrap_or("");
            to_value
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let bytes = hex::decode(s.trim())
                        .map_err(|e| ClientError(format!("bad hex in 'to' field: {e}")))?;
                    bytes
                        .try_into()
                        .map_err(|_| ClientError("actor ID must be 32 bytes".into()))
                })
                .collect::<Result<Vec<[u8; 32]>, ClientError>>()?
        }
        _ => vec![],
    };

    Ok(recipients)
}

// ── Internal helpers ──────────────────────────────────────────

#[allow(clippy::too_many_arguments)] // builds an email-post payload from its fields; a struct would just relocate them
fn build_email_post(
    kp: &ActorKeypair,
    author: ActorId,
    recipient: ActorId,
    subject: &str,
    body: &str,
    items: Vec<MediaItem>,
    encrypted: bool,
    node_url: &str,
) -> Result<Vec<u8>, ClientError> {
    let mut fields = vec![
        StructuredField {
            key: "subject".into(),
            value: subject.into(),
        },
        StructuredField {
            key: "to".into(),
            value: hex::encode(recipient.0),
        },
        StructuredField {
            key: "priority".into(),
            value: "normal".into(),
        },
    ];

    if encrypted {
        fields.push(StructuredField {
            key: "encrypted".into(),
            value: "true".into(),
        });
    }

    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: EMAIL_SCHEMA.into(),
            fields,
            content: Some(body.into()),
            facets: vec![],
            items,
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    let (post_bytes, post_env) =
        sign_envelope(kp, &post).map_err(|e| ClientError(format!("sign post: {e}")))?;
    let post_id = compute_post_id(&post).map_err(|e| ClientError(format!("post id: {e}")))?;
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);

    let summary = crate::truncate(subject, 80);
    let cr = ContactRequest {
        sender: author,
        post_id,
        sender_node: node_url.as_bytes().to_vec(),
        summary,
        created_at: Timestamp::now(),
    };

    let (cr_bytes, cr_env) =
        sign_envelope(kp, &cr).map_err(|e| ClientError(format!("sign CR: {e}")))?;
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);
    canonical_encode(&(&cr_wire, &post_wire))
        .map_err(|e| ClientError(format!("encode payload: {e}")))
}

fn extract_email_fields(post: &Post) -> (String, String) {
    match &post.body {
        PostBody::Structured {
            schema,
            fields,
            content,
            ..
        } if schema == EMAIL_SCHEMA => {
            let subject = fields
                .iter()
                .find(|f| f.key == "subject")
                .map(|f| f.value.clone())
                .unwrap_or_default();
            let body = content.clone().unwrap_or_default();
            (subject, body)
        }
        _ => (String::new(), String::new()),
    }
}

fn extract_attachments(post: &Post) -> Vec<Attachment> {
    match &post.body {
        PostBody::Structured { items, .. }
        | PostBody::TextWithMedia { items, .. }
        | PostBody::Media { items, .. } => items
            .iter()
            .map(|item| Attachment {
                hash: item.blob_hash.digest(),
                media_type: item.media_type.clone(),
                size_bytes: item.size_bytes,
            })
            .collect(),
        _ => vec![],
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    fn test_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([42u8; 32])
    }

    fn recipient_key() -> [u8; 32] {
        [7u8; 32]
    }

    #[test]
    fn build_and_decode_roundtrip() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload = build_signed_email(
            &kp,
            &to,
            "Hello World",
            "This is the body.",
            "http://localhost:3000",
        )
        .unwrap();

        let decoded = decode_email(&payload).unwrap();

        assert!(decoded.valid, "signature validation should pass");
        assert_eq!(decoded.subject, "Hello World");
        assert_eq!(decoded.body, "This is the body.");
        assert_eq!(decoded.from, kp.actor_id().0);
        assert!(!decoded.encrypted);
        assert_eq!(decoded.sender_node, "http://localhost:3000");
        assert!(!decoded.post_id.is_empty());
    }

    #[test]
    fn build_knock_payload_uses_canonical_sentinel() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload = build_knock_payload(&kp, &to, "http://localhost:3000").unwrap();

        let decoded = decode_email(&payload).unwrap();
        assert!(decoded.valid, "knock payload must be a valid signed email");
        // The decoded subject/body MUST be the canonical wire sentinel — both
        // the named consts and their literal values are pinned so a future
        // edit to either can't silently change the wire shape (knock detection
        // on the recipient side keys off these exact strings).
        assert_eq!(decoded.subject, KNOCK_SUBJECT);
        assert_eq!(decoded.body, KNOCK_BODY);
        assert_eq!(decoded.subject, "Knock");
        assert_eq!(decoded.body, "Contact request");
        // build_knock_payload is exactly build_signed_email with the sentinel.
        let direct = build_signed_email(
            &kp,
            &to,
            "Knock",
            "Contact request",
            "http://localhost:3000",
        )
        .unwrap();
        let knock_fields = decode_email(&direct).unwrap();
        assert_eq!(knock_fields.subject, decoded.subject);
        assert_eq!(knock_fields.body, decoded.body);
    }

    #[test]
    fn build_and_decode_with_media_roundtrip() {
        let kp = test_keypair();
        let to = recipient_key();
        let attachment = Attachment {
            hash: [0x11u8; 32],
            media_type: "image/png".into(),
            size_bytes: 2048,
        };
        let payload = build_signed_email_with_media(
            &kp,
            &to,
            "Photo email",
            "See attached.",
            vec![attachment],
            false,
            "http://localhost:3000",
        )
        .unwrap();

        let decoded = decode_email(&payload).unwrap();
        assert!(decoded.valid);
        assert_eq!(decoded.subject, "Photo email");
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].hash, [0x11u8; 32]);
        assert_eq!(decoded.attachments[0].media_type, "image/png");
        assert_eq!(decoded.attachments[0].size_bytes, 2048);
    }

    #[test]
    fn build_encrypted_email() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload = build_signed_email_with_media(
            &kp,
            &to,
            "Secret",
            "Encrypted body.",
            vec![],
            true,
            "http://localhost:3000",
        )
        .unwrap();

        let decoded = decode_email(&payload).unwrap();
        assert!(decoded.valid);
        assert!(decoded.encrypted);
    }

    #[test]
    fn get_recipients_extracts_correct_actors() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload =
            build_signed_email(&kp, &to, "Test", "body", "http://localhost:3000").unwrap();

        let recipients = get_recipients(&payload).unwrap();
        assert_eq!(recipients.len(), 1);
        assert_eq!(recipients[0], to);
    }

    #[test]
    fn decode_email_json_projects_all_fields() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload = build_signed_email(
            &kp,
            &to,
            "Hello World",
            "This is the body.",
            "http://localhost:3000",
        )
        .unwrap();

        let json = decode_email_json(&payload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["from"], hex::encode(kp.actor_id().0));
        assert_eq!(v["subject"], "Hello World");
        assert_eq!(v["body"], "This is the body.");
        assert_eq!(v["valid"], true);
        assert_eq!(v["encrypted"], false);
        assert_eq!(v["sender_node"], "http://localhost:3000");
        assert!(v["timestamp"].as_u64().unwrap() > 0);
        assert!(!v["post_id"].as_str().unwrap().is_empty());
        assert_eq!(v["attachments"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn decode_email_json_includes_attachments_and_encrypted() {
        let kp = test_keypair();
        let to = recipient_key();
        let attachment = Attachment {
            hash: [0x11u8; 32],
            media_type: "image/png".into(),
            size_bytes: 2048,
        };
        let payload = build_signed_email_with_media(
            &kp,
            &to,
            "Photo email",
            "See attached.",
            vec![attachment],
            true,
            "http://localhost:3000",
        )
        .unwrap();

        let json = decode_email_json(&payload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["encrypted"], true);
        let atts = v["attachments"].as_array().unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0]["hash"], hex::encode([0x11u8; 32]));
        assert_eq!(atts[0]["media_type"], "image/png");
        assert_eq!(atts[0]["size_bytes"], 2048);
    }

    #[test]
    fn decode_validates_signatures() {
        let kp = test_keypair();
        let to = recipient_key();
        let payload =
            build_signed_email(&kp, &to, "Signed", "body", "http://localhost:3000").unwrap();

        // Corrupt the payload
        let mut bad = payload.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;

        // Either fails to decode or returns valid=false
        if let Ok(d) = decode_email(&bad) {
            assert!(!d.valid, "corrupted payload should not be valid");
        }

        // Original is valid
        let decoded_ok = decode_email(&payload).unwrap();
        assert!(decoded_ok.valid);
    }
}
