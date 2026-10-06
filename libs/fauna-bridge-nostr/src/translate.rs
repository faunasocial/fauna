use fauna_core::data::{Post, PostBody, Profile, Reference, StructuredField, Timestamp};
use fauna_core::identity::ActorId;
use fauna_core::structured::{
    self, SCHEMA_ARTICLE, SCHEMA_CLASSIFIED, SCHEMA_COMMUNITY, SCHEMA_LIVE_ACTIVITY,
};

/// Extract the 32-byte BLAKE3 digest from a Fauna CID — the part Nostr
/// event tags carry as a 64-char hex "id". Strips the 4-byte
/// `v1 + dag-cbor + blake3-256 + len 32` prefix.
fn cid_digest_hex(cid: &fauna_cbor::Cid) -> String {
    hex::encode(cid.digest())
}

/// Build a synthetic Fauna CID from a 32-byte external hash (Nostr event
/// ID, ActivityPub digest, …). The CID is syntactically valid but does
/// NOT match any real Fauna post — bridges use this for cross-protocol
/// references where the original payload doesn't exist as canonical
/// dag-cbor.
fn synthetic_cid(digest: [u8; 32]) -> fauna_cbor::Cid {
    fauna_cbor::Cid::from_digest_dag_cbor(digest)
}

use crate::nip10;
use crate::nip18;
use crate::nip25;
use crate::types::{Event, Tag, UnsignedEvent, kind};

/// A post's reply or quote target, resolved by the caller to the nostr event
/// it names (`nostr.md` § Replying to and quoting a nostr note → *Reference
/// resolution*). This crate is DB-free: the nest maps the `Reference`'s local
/// post id through `nostr_event_map` and reads the parent's stored event for
/// its thread root and `p` tags. Every id here is lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedReference {
    Reply {
        event_id: String,
        pubkey: String,
        /// The parent's own NIP-10 root; `None` when the parent is a root.
        root_id: Option<String>,
        /// The parent's own `p` tags — NIP-10's thread participants.
        parent_p_tags: Vec<String>,
    },
    Quote {
        event_id: String,
        pubkey: String,
    },
}

/// Convert a Fauna Post to an unsigned Nostr event.
///
/// Truncates Fauna microsecond timestamp to Nostr seconds. A kind-1 note's
/// threading comes from `resolved` ONLY, never from the post's own
/// `Reply`/`Quote` references: those name local post ids no relay knows, so a
/// reference the caller could not resolve derives a plain note (never the
/// Fauna digest as a tag id).
pub fn fauna_post_to_nostr(
    post: &Post,
    nostr_pubkey: &[u8; 32],
    resolved: &[ResolvedReference],
) -> anyhow::Result<UnsignedEvent> {
    let created_at = post.created_at.0 / 1_000_000; // microseconds → seconds

    // Determine kind and content based on references
    for reference in &post.references {
        match reference {
            Reference::React { post_id, emoji } => {
                let target_id = cid_digest_hex(post_id);
                return Ok(crate::nip25::build_reaction_event(
                    &target_id,
                    &hex::encode(nostr_pubkey), // self as target pubkey placeholder
                    emoji,
                    nostr_pubkey,
                    created_at,
                ));
            }
            Reference::Repost { post_id } => {
                // Build a minimal kind-6 repost
                let target_id = cid_digest_hex(post_id);
                return Ok(UnsignedEvent {
                    pubkey: *nostr_pubkey,
                    created_at,
                    kind: kind::REPOST,
                    tags: vec![Tag::new(vec!["e".into(), target_id])],
                    content: String::new(),
                });
            }
            _ => {}
        }
    }

    // Check for a Nostr article (PostBody::Structured with schema "nostr/article")
    if let PostBody::Structured {
        schema,
        fields,
        content: body_content,
        ..
    } = &post.body
        && schema == SCHEMA_ARTICLE
    {
        let title = structured::field(fields, "title");
        let summary_val = structured::field(fields, "summary");
        let image_val = structured::field(fields, "image");

        let article = crate::nip23::Article {
            title,
            summary: if summary_val.is_empty() {
                None
            } else {
                Some(summary_val)
            },
            image: if image_val.is_empty() {
                None
            } else {
                Some(image_val)
            },
            published_at: None,
            content_markdown: body_content.clone().unwrap_or_default(),
            hashtags: vec![],
            identifier: String::new(),
        };
        return Ok(crate::nip23::build_article_event(
            &article,
            nostr_pubkey,
            created_at,
        ));
    }

    let mut content = post.body_text();
    let mut tags = Vec::new();

    for reference in resolved {
        match reference {
            ResolvedReference::Reply {
                event_id,
                pubkey,
                root_id,
                parent_p_tags,
            } => {
                // NIP-10 marked form: the parent's root as `root` + the parent
                // as `reply`, or the parent alone as `root` when it is one.
                let root = root_id.as_deref().unwrap_or(event_id);
                tags.extend(
                    nip10::build_reply_tags(Some(root), event_id, pubkey)
                        .into_iter()
                        .filter(|t| t.name() == Some("e")),
                );
                let mut participants = vec![pubkey];
                for p in parent_p_tags {
                    if !participants.contains(&p) {
                        participants.push(p);
                    }
                }
                tags.extend(
                    participants
                        .into_iter()
                        .map(|p| Tag::new(vec!["p".into(), p.clone()])),
                );
            }
            ResolvedReference::Quote { event_id, pubkey } => {
                tags.extend(nip18::build_quote_tags(event_id, pubkey));
                // The NIP-21 reference every client embeds; the `q` tag alone
                // renders as nothing in most of them.
                let mut id = [0u8; 32];
                let mut author = [0u8; 32];
                hex::decode_to_slice(event_id, &mut id)
                    .map_err(|e| anyhow::anyhow!("quoted event id is not 32-byte hex: {e}"))?;
                let author = hex::decode_to_slice(pubkey, &mut author)
                    .is_ok()
                    .then_some(&author);
                let nevent = crate::nip19::encode_nevent(&id, author);
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str("nostr:");
                content.push_str(&nevent);
            }
        }
    }

    Ok(UnsignedEvent {
        pubkey: *nostr_pubkey,
        created_at,
        kind: kind::TEXT_NOTE,
        tags,
        content,
    })
}

/// Convert a Nostr event to a Fauna Post.
///
/// Kind 1 → PostBody::Text (or TextWithMedia if image URLs detected).
/// Kind 5 → returns Err (use nostr_deletion_to_fauna instead).
/// Kind 7 → produces a Post with Reference::React.
/// Kind 6 → produces a Post with Reference::Repost.
/// Unknown kinds → PostBody::Structured { schema: "nostr/kind-N" }.
///
/// `resolve_local` maps a kind-1 note's NIP-10 parent (`e`) or NIP-18 quote
/// (`q`) event id to the local post id this nest holds for it (the nest reads
/// `nostr_event_map`); only a resolved target becomes a `Reference::Reply` /
/// `Reference::Quote`, so an unmapped one leaves the note top-level rather
/// than threading it under an id no row carries (`nostr.md` § Replying to and
/// quoting a nostr note → *Reference resolution*).
pub fn nostr_event_to_fauna(
    event: &Event,
    resolve_local: &dyn Fn(&str) -> Option<fauna_cbor::Cid>,
) -> anyhow::Result<(Post, Vec<Reference>)> {
    let author = synthetic_actor_id(&pubkey_bytes_from_hex(&event.pubkey)?);
    let created_at = Timestamp(event.created_at * 1_000_000); // seconds → microseconds

    let mut references = Vec::new();

    let body = match event.kind {
        kind::TEXT_NOTE => {
            // NIP-10 threading: the `reply`-marked parent, or — a direct reply
            // to a thread root carries the `root` marker alone — the root.
            let thread = nip10::parse_thread_tags(&event.tags);
            if let Some(post_id) = thread
                .reply
                .as_deref()
                .or(thread.root.as_deref())
                .and_then(resolve_local)
            {
                references.push(Reference::Reply { post_id });
            }

            if let Some(post_id) = nip18::parse_quote_target(event)
                .as_deref()
                .and_then(resolve_local)
            {
                references.push(Reference::Quote { post_id });
            }

            text_to_post_body(&event.content)
        }
        kind::REACTION => {
            if let Some((target_id, emoji)) = nip25::parse_reaction(event)
                && let Ok(bytes) = hex::decode(&target_id)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                references.push(Reference::React {
                    post_id: synthetic_cid(arr),
                    emoji,
                });
            }
            PostBody::Text {
                content: event.content.clone(),
                facets: vec![],
            }
        }
        kind::REPOST => {
            if let Some(target_id) = nip18::parse_repost_target(event)
                && let Ok(bytes) = hex::decode(&target_id)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                references.push(Reference::Repost {
                    post_id: synthetic_cid(arr),
                });
            }
            PostBody::Text {
                content: event.content.clone(),
                facets: vec![],
            }
        }
        kind::DELETION => {
            anyhow::bail!("kind 5 deletion events should use nostr_deletion_to_fauna()");
        }
        kind::LONG_FORM => {
            let article = crate::nip23::parse_article(event)?;
            PostBody::Structured {
                schema: SCHEMA_ARTICLE.to_string(),
                fields: vec![
                    StructuredField {
                        key: "title".into(),
                        value: article.title,
                    },
                    StructuredField {
                        key: "summary".into(),
                        value: article.summary.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "image".into(),
                        value: article.image.unwrap_or_default(),
                    },
                ],
                content: Some(article.content_markdown),
                facets: vec![],
                items: vec![],
            }
        }
        kind::COMMUNITY_DEFINITION => {
            let community = crate::nip72::parse_community(event)?;
            PostBody::Structured {
                schema: SCHEMA_COMMUNITY.to_string(),
                fields: vec![
                    StructuredField {
                        key: "name".into(),
                        value: community.name.unwrap_or(community.identifier.clone()),
                    },
                    StructuredField {
                        key: "description".into(),
                        value: community.description.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "identifier".into(),
                        value: community.identifier,
                    },
                ],
                content: community.rules,
                facets: vec![],
                items: vec![],
            }
        }
        kind::CLASSIFIED => {
            let listing = crate::nip99::parse_classified(event)?;
            PostBody::Structured {
                schema: SCHEMA_CLASSIFIED.to_string(),
                fields: vec![
                    StructuredField {
                        key: "title".into(),
                        value: listing.title,
                    },
                    StructuredField {
                        key: "price".into(),
                        value: listing.price.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "location".into(),
                        value: listing.location.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "condition".into(),
                        value: listing.condition.unwrap_or_default(),
                    },
                ],
                content: Some(listing.content),
                facets: vec![],
                items: vec![],
            }
        }
        kind::LIVE_ACTIVITY => {
            let activity = crate::nip53::parse_live_activity(event)?;
            PostBody::Structured {
                schema: SCHEMA_LIVE_ACTIVITY.to_string(),
                fields: vec![
                    StructuredField {
                        key: "title".into(),
                        value: activity.title.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "status".into(),
                        value: activity.status,
                    },
                    StructuredField {
                        key: "streaming_url".into(),
                        value: activity.streaming_url.unwrap_or_default(),
                    },
                    StructuredField {
                        key: "participants".into(),
                        value: activity
                            .current_participants
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                    },
                ],
                content: activity.summary,
                facets: vec![],
                items: vec![],
            }
        }
        other => {
            // Unknown kind → Structured fallback
            PostBody::Structured {
                schema: format!("nostr/kind-{other}"),
                fields: vec![StructuredField {
                    key: "content".into(),
                    value: event.content.clone(),
                }],
                content: Some(event.content.clone()),
                facets: vec![],
                items: vec![],
            }
        }
    };

    let post = Post {
        author,
        created_at,
        body,
        references: references.clone(),
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    Ok((post, references))
}

/// Build a native kind-5 deletion event naming the given **derived Nostr
/// event id(s)** — the outbound half of Fauna post deletion (`feed.md`
/// § State & data shape → *Post deletion*; `nostr.md` § The relay event
/// store, NIP-09). The caller resolves the real target ids from
/// `nostr_event_map` and signs the result; nothing is computed from the
/// Fauna post id here. (The predecessor `fauna_tombstone_to_nostr` derived
/// its `e`-tag from `cid_digest_hex(post_id)` — neither a real Nostr event
/// id nor the map's key convention, so it named nothing lookup-able; it was
/// deleted with the wiring that replaced it.)
pub fn kind5_deletion(
    nostr_pubkey: &[u8; 32],
    created_at_secs: u64,
    target_event_ids: &[String],
) -> UnsignedEvent {
    UnsignedEvent {
        pubkey: *nostr_pubkey,
        created_at: created_at_secs,
        kind: kind::DELETION,
        tags: target_event_ids
            .iter()
            .map(|id| Tag::new(vec!["e".into(), id.clone()]))
            .collect(),
        content: String::new(),
    }
}

/// The NIP-01 / NIP-24 metadata a kind-0 event's `content` carries — the
/// consume-side twin of [`fauna_profile_to_nostr`]: what a *remote* author
/// published about themselves, read at the inbound sweep's transit point
/// (`docs/goal/behavior/bridges.md` § Unified feed ingestion → *Bridged
/// authors*). Only the display fields are lifted; every value is trimmed and
/// an empty one reads as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NostrMetadata {
    /// The username-shaped `name` (NIP-01).
    pub name: Option<String>,
    /// The `display_name` (NIP-24) — the human name, over `name`.
    pub display_name: Option<String>,
    /// The `picture` URL (NIP-01) — the remote origin, un-proxied.
    pub picture: Option<String>,
    /// The `nip05` address (NIP-05), `user@domain` — the handle when present.
    pub nip05: Option<String>,
}

impl NostrMetadata {
    /// The bridge's user-facing handle: the NIP-05 address when the author
    /// published one, else the `name`.
    pub fn handle(&self) -> Option<&str> {
        self.nip05.as_deref().or(self.name.as_deref())
    }
}

/// Parse a kind-0 event's `content` JSON. `None` when the content is not a
/// JSON object — a malformed kind 0 is dropped, never a face of empty
/// strings. Non-string values for the four fields read as absent.
pub fn parse_metadata(content: &str) -> Option<NostrMetadata> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = v.as_object()?;
    let field = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Some(NostrMetadata {
        name: field("name"),
        display_name: field("display_name"),
        picture: field("picture"),
        nip05: field("nip05"),
    })
}

/// Convert a Fauna Profile to a kind-0 metadata event.
pub fn fauna_profile_to_nostr(profile: &Profile, nostr_pubkey: &[u8; 32]) -> UnsignedEvent {
    let created_at = profile.updated_at.0 / 1_000_000;
    let metadata = serde_json::json!({
        "name": profile.display_name,
        "about": profile.bio,
    });

    UnsignedEvent {
        pubkey: *nostr_pubkey,
        created_at,
        kind: kind::METADATA,
        tags: vec![],
        content: serde_json::to_string(&metadata).unwrap_or_default(),
    }
}

/// Derive a synthetic Fauna ActorId from a Nostr pubkey.
/// Uses BLAKE3("fauna-nostr-synthetic" || nostr_pubkey_bytes).
pub fn synthetic_actor_id(nostr_pubkey: &[u8; 32]) -> ActorId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fauna-nostr-synthetic");
    hasher.update(nostr_pubkey);
    let hash = hasher.finalize();
    ActorId(*hash.as_bytes())
}

// ── Helpers ──────────────────────────────────────────────────────

fn text_to_post_body(content: &str) -> PostBody {
    // Simple heuristic: if content contains image URLs, treat as TextWithMedia
    // For v1, we keep it simple — everything is Text
    PostBody::Text {
        content: content.to_string(),
        facets: vec![],
    }
}

fn pubkey_bytes_from_hex(hex_str: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(hex_str)?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fauna_post(content: &str, refs: Vec<Reference>) -> Post {
        Post {
            author: ActorId([1u8; 32]),
            created_at: Timestamp(1_000_000_000_000), // 1_000_000 seconds in microseconds
            body: PostBody::Text {
                content: content.to_string(),
                facets: vec![],
            },
            references: refs,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    #[test]
    fn text_post_to_nostr_kind_1() {
        let post = make_fauna_post("hello world", vec![]);
        let pubkey = [2u8; 32];
        let event = fauna_post_to_nostr(&post, &pubkey, &[]).unwrap();
        assert_eq!(event.kind, kind::TEXT_NOTE);
        assert_eq!(event.content, "hello world");
        assert_eq!(event.created_at, 1_000_000); // truncated from microseconds
    }

    #[test]
    fn timestamp_truncation() {
        let post = Post {
            author: ActorId([1u8; 32]),
            created_at: Timestamp(1_234_567_890_123_456), // microseconds
            body: PostBody::Text {
                content: "test".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let event = fauna_post_to_nostr(&post, &[2u8; 32], &[]).unwrap();
        assert_eq!(event.created_at, 1_234_567_890); // seconds
    }

    fn no_local_post(_: &str) -> Option<fauna_cbor::Cid> {
        None
    }

    fn tag_rows(event: &UnsignedEvent) -> Vec<Vec<&str>> {
        event
            .tags
            .iter()
            .map(|t| t.0.iter().map(String::as_str).collect())
            .collect()
    }

    /// A Fauna reply post: its `Reference::Reply` names the LOCAL id of a swept
    /// note, which the nest resolves through `nostr_event_map` before calling.
    fn fauna_reply() -> Post {
        make_fauna_post(
            "reply text",
            vec![Reference::Reply {
                post_id: synthetic_cid([3u8; 32]),
            }],
        )
    }

    /// Replying mid-thread: the parent's own root rides as the `root` marker,
    /// the parent as `reply`, and the `p` set is the parent's author plus the
    /// parent's own `p` tags (NIP-10 thread participants), deduplicated — and
    /// no tag anywhere carries the Fauna digest (`nostr.md` § Replying to and
    /// quoting a nostr note).
    #[test]
    fn a_resolved_reply_emits_marked_root_and_reply_tags_and_the_p_set() {
        let (parent, root, author, other) = (
            "c".repeat(64),
            "d".repeat(64),
            "e".repeat(64),
            "f".repeat(64),
        );
        let resolved = [ResolvedReference::Reply {
            event_id: parent.clone(),
            pubkey: author.clone(),
            root_id: Some(root.clone()),
            parent_p_tags: vec![other.clone(), author.clone()],
        }];
        let event = fauna_post_to_nostr(&fauna_reply(), &[2u8; 32], &resolved).unwrap();
        assert_eq!(event.kind, kind::TEXT_NOTE);
        assert_eq!(event.content, "reply text");
        assert_eq!(
            tag_rows(&event),
            vec![
                vec!["e", root.as_str(), "", "root"],
                vec!["e", parent.as_str(), "", "reply"],
                vec!["p", author.as_str()],
                vec!["p", other.as_str()],
            ]
        );
        let digest = hex::encode([3u8; 32]);
        assert!(event.tags.iter().all(|t| !t.0.contains(&digest)));
    }

    /// Replying to a thread's root: the parent alone rides, marked `root`.
    #[test]
    fn a_reply_to_a_root_note_marks_the_parent_as_root_only() {
        let (parent, author) = ("c".repeat(64), "e".repeat(64));
        let resolved = [ResolvedReference::Reply {
            event_id: parent.clone(),
            pubkey: author.clone(),
            root_id: None,
            parent_p_tags: vec![],
        }];
        let event = fauna_post_to_nostr(&fauna_reply(), &[2u8; 32], &resolved).unwrap();
        assert_eq!(
            tag_rows(&event),
            vec![
                vec!["e", parent.as_str(), "", "root"],
                vec!["p", author.as_str()]
            ]
        );
    }

    /// An unresolved reference (a native or foreign-bridge target) derives a
    /// plain note — never a synthetic or digest id.
    #[test]
    fn an_unresolved_reply_or_quote_derives_a_plain_note() {
        let event = fauna_post_to_nostr(&fauna_reply(), &[2u8; 32], &[]).unwrap();
        assert!(event.tags.is_empty());
        assert_eq!(event.content, "reply text");

        let quote = make_fauna_post(
            "quoting",
            vec![Reference::Quote {
                post_id: synthetic_cid([3u8; 32]),
            }],
        );
        let event = fauna_post_to_nostr(&quote, &[2u8; 32], &[]).unwrap();
        assert!(event.tags.is_empty());
        assert_eq!(event.content, "quoting");
    }

    #[test]
    fn react_produces_kind_7() {
        let target = synthetic_cid([3u8; 32]);
        let post = make_fauna_post(
            "",
            vec![Reference::React {
                post_id: target,
                emoji: "🔥".into(),
            }],
        );
        let event = fauna_post_to_nostr(&post, &[2u8; 32], &[]).unwrap();
        assert_eq!(event.kind, kind::REACTION);
        assert_eq!(event.content, "🔥");
    }

    #[test]
    fn repost_produces_kind_6() {
        let target = synthetic_cid([3u8; 32]);
        let post = make_fauna_post("", vec![Reference::Repost { post_id: target }]);
        let event = fauna_post_to_nostr(&post, &[2u8; 32], &[]).unwrap();
        assert_eq!(event.kind, kind::REPOST);
    }

    #[test]
    fn a_resolved_quote_emits_q_and_p_and_a_nostr_nevent_reference() {
        let quoted = [0x0cu8; 32];
        let author = [0x0eu8; 32];
        let post = make_fauna_post(
            "quoting this",
            vec![Reference::Quote {
                post_id: synthetic_cid([3u8; 32]),
            }],
        );
        let resolved = [ResolvedReference::Quote {
            event_id: hex::encode(quoted),
            pubkey: hex::encode(author),
        }];
        let event = fauna_post_to_nostr(&post, &[2u8; 32], &resolved).unwrap();
        assert_eq!(event.kind, kind::TEXT_NOTE);
        assert_eq!(
            tag_rows(&event),
            vec![
                vec!["q", hex::encode(quoted).as_str()],
                vec!["p", hex::encode(author).as_str()],
            ]
        );
        let nevent = crate::nip19::encode_nevent(&quoted, Some(&author));
        assert_eq!(event.content, format!("quoting this\n\nnostr:{nevent}"));
    }

    #[test]
    fn nostr_kind_1_to_fauna() {
        let pubkey = "a".repeat(64);
        let event = Event {
            id: "b".repeat(64),
            pubkey: pubkey.clone(),
            created_at: 1_000_000,
            kind: kind::TEXT_NOTE,
            tags: vec![],
            content: "hello from nostr".into(),
            sig: "s".repeat(128),
        };
        let (post, refs) = nostr_event_to_fauna(&event, &no_local_post).unwrap();
        match &post.body {
            PostBody::Text { content, .. } => assert_eq!(content, "hello from nostr"),
            _ => panic!("expected Text"),
        }
        assert_eq!(post.created_at.0, 1_000_000_000_000); // seconds → microseconds
        assert!(refs.is_empty());
    }

    #[test]
    fn nostr_kind_7_to_fauna_reaction() {
        let event = Event {
            id: "b".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind: kind::REACTION,
            tags: vec![
                Tag::new(vec!["e".into(), "c".repeat(64)]),
                Tag::new(vec!["p".into(), "d".repeat(64)]),
            ],
            content: "❤️".into(),
            sig: "s".repeat(128),
        };
        let (_, refs) = nostr_event_to_fauna(&event, &no_local_post).unwrap();
        assert_eq!(refs.len(), 1);
        match &refs[0] {
            Reference::React { emoji, .. } => assert_eq!(emoji, "❤️"),
            _ => panic!("expected React"),
        }
    }

    #[test]
    fn nostr_kind_6_to_fauna_repost() {
        let event = Event {
            id: "b".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind: kind::REPOST,
            tags: vec![Tag::new(vec!["e".into(), "c".repeat(64)])],
            content: "{}".into(),
            sig: "s".repeat(128),
        };
        let (_, refs) = nostr_event_to_fauna(&event, &no_local_post).unwrap();
        assert_eq!(refs.len(), 1);
        match &refs[0] {
            Reference::Repost { .. } => {}
            _ => panic!("expected Repost"),
        }
    }

    #[test]
    fn nostr_kind_30023_to_structured_article() {
        let event = Event {
            id: "b".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind: kind::LONG_FORM,
            tags: vec![
                Tag::new(vec!["d".into(), "my-article".into()]),
                Tag::new(vec!["title".into(), "My Article".into()]),
                Tag::new(vec!["summary".into(), "A summary".into()]),
            ],
            content: "# Body\n\nContent here.".into(),
            sig: "s".repeat(128),
        };
        let (post, _) = nostr_event_to_fauna(&event, &no_local_post).unwrap();
        match &post.body {
            PostBody::Structured {
                schema,
                fields,
                content,
                ..
            } => {
                assert_eq!(schema, "nostr/article");
                assert!(
                    fields
                        .iter()
                        .any(|f| f.key == "title" && f.value == "My Article")
                );
                assert_eq!(content.as_deref(), Some("# Body\n\nContent here."));
            }
            _ => panic!("expected Structured"),
        }
    }

    /// Anti-drift guard: every structured schema the nostr bridge *writes*
    /// (`nostr_event_to_fauna`) must project cleanly through the shared
    /// `fauna_core::structured::structured_view` *reader* the apps ride
    /// (web over `fauna_wasm::structuredView`, native direct). This couples the
    /// writer's field keys to the reader projection — a drift on either side
    /// breaks this test rather than silently blanking a feed card.
    #[test]
    fn writer_output_projects_through_shared_reader() {
        use fauna_core::structured::{StructuredView, structured_view};

        let ev = |kind: u64, tags: Vec<Tag>, content: &str| Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1000,
            kind,
            tags,
            content: content.to_string(),
            sig: "s".repeat(128),
        };

        // Article (NIP-23 / kind 30023)
        let (post, _) = nostr_event_to_fauna(
            &ev(
                kind::LONG_FORM,
                vec![
                    Tag::new(vec!["d".into(), "my-article".into()]),
                    Tag::new(vec!["title".into(), "My Article".into()]),
                    Tag::new(vec!["summary".into(), "A summary".into()]),
                    Tag::new(vec!["image".into(), "https://img".into()]),
                ],
                "# Body",
            ),
            &no_local_post,
        )
        .unwrap();
        assert_eq!(
            structured_view(&post.body),
            Some(StructuredView::Article {
                title: "My Article".into(),
                summary: "A summary".into(),
                image: "https://img".into(),
                content: "# Body".into(),
            })
        );

        // Community (NIP-72 / kind 34550) — the post's free-text content is the rules.
        let (post, _) = nostr_event_to_fauna(
            &ev(
                kind::COMMUNITY_DEFINITION,
                vec![
                    Tag::new(vec!["d".into(), "rustaceans".into()]),
                    Tag::new(vec!["name".into(), "Rustaceans".into()]),
                    Tag::new(vec!["description".into(), "We like Rust".into()]),
                    Tag::new(vec!["rules".into(), "Be kind.".into()]),
                ],
                "",
            ),
            &no_local_post,
        )
        .unwrap();
        assert_eq!(
            structured_view(&post.body),
            Some(StructuredView::Community {
                name: "Rustaceans".into(),
                description: "We like Rust".into(),
                identifier: "rustaceans".into(),
                rules: "Be kind.".into(),
            })
        );

        // Classified (NIP-99 / kind 30402)
        let (post, _) = nostr_event_to_fauna(
            &ev(
                kind::CLASSIFIED,
                vec![
                    Tag::new(vec!["d".into(), "listing-001".into()]),
                    Tag::new(vec!["title".into(), "Bike".into()]),
                    Tag::new(vec!["price".into(), "250 USD".into()]),
                    Tag::new(vec!["location".into(), "Oslo".into()]),
                    Tag::new(vec!["condition".into(), "used".into()]),
                ],
                "A nice bike.",
            ),
            &no_local_post,
        )
        .unwrap();
        assert_eq!(
            structured_view(&post.body),
            Some(StructuredView::Classified {
                title: "Bike".into(),
                price: "250 USD".into(),
                location: "Oslo".into(),
                condition: "used".into(),
                content: "A nice bike.".into(),
            })
        );

        // Live activity (NIP-53 / kind 30311) — content is the summary.
        let (post, _) = nostr_event_to_fauna(
            &ev(
                kind::LIVE_ACTIVITY,
                vec![
                    Tag::new(vec!["d".into(), "stream-001".into()]),
                    Tag::new(vec!["title".into(), "Live".into()]),
                    Tag::new(vec!["status".into(), "live".into()]),
                    Tag::new(vec!["streaming".into(), "https://watch".into()]),
                    Tag::new(vec!["current_participants".into(), "42".into()]),
                    Tag::new(vec!["summary".into(), "Come watch.".into()]),
                ],
                "",
            ),
            &no_local_post,
        )
        .unwrap();
        assert_eq!(
            structured_view(&post.body),
            Some(StructuredView::LiveActivity {
                title: "Live".into(),
                status: "live".into(),
                streaming_url: "https://watch".into(),
                participants: "42".into(),
                summary: "Come watch.".into(),
            })
        );
    }

    #[test]
    fn fauna_article_to_nostr_kind_30023_roundtrip() {
        // Build a Fauna article post
        let article_post = Post {
            author: ActorId([1u8; 32]),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Structured {
                schema: "nostr/article".to_string(),
                fields: vec![
                    StructuredField {
                        key: "title".into(),
                        value: "Test Article".into(),
                    },
                    StructuredField {
                        key: "summary".into(),
                        value: "A test summary".into(),
                    },
                    StructuredField {
                        key: "image".into(),
                        value: "https://example.com/img.png".into(),
                    },
                ],
                content: Some("# Test\n\nMarkdown body.".into()),
                facets: vec![],
                items: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };

        let pubkey = [9u8; 32];
        let unsigned = fauna_post_to_nostr(&article_post, &pubkey, &[]).unwrap();

        // Should produce a kind-30023 event
        assert_eq!(unsigned.kind, kind::LONG_FORM);
        assert_eq!(unsigned.content, "# Test\n\nMarkdown body.");

        // title tag present
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("title") && t.value() == Some("Test Article"))
        );
        // summary tag present
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("summary") && t.value() == Some("A test summary"))
        );
        // image tag present
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("image")
                    && t.value() == Some("https://example.com/img.png"))
        );

        // Now parse it back to Fauna
        let signed = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags.clone(),
            content: unsigned.content.clone(),
            sig: "s".repeat(128),
        };
        let (post, _) = nostr_event_to_fauna(&signed, &no_local_post).unwrap();
        match &post.body {
            PostBody::Structured {
                schema,
                fields,
                content,
                ..
            } => {
                assert_eq!(schema, "nostr/article");
                assert!(
                    fields
                        .iter()
                        .any(|f| f.key == "title" && f.value == "Test Article")
                );
                assert_eq!(content.as_deref(), Some("# Test\n\nMarkdown body."));
            }
            _ => panic!("expected Structured article"),
        }
    }

    #[test]
    fn nostr_kind_5_errors() {
        let event = Event {
            id: "b".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind: kind::DELETION,
            tags: vec![Tag::new(vec!["e".into(), "c".repeat(64)])],
            content: "".into(),
            sig: "s".repeat(128),
        };
        assert!(nostr_event_to_fauna(&event, &no_local_post).is_err());
    }

    #[test]
    fn kind5_deletion_names_the_given_targets_exactly() {
        // The predecessor's test only asserted the tag NAME existed, which
        // let a never-lookup-able computed target survive unnoticed — this
        // one pins the VALUES.
        let ids = vec!["b".repeat(64), "c".repeat(64)];
        let event = kind5_deletion(&[2u8; 32], 2_000_000, &ids);
        assert_eq!(event.kind, kind::DELETION);
        assert_eq!(event.created_at, 2_000_000);
        let e_values: Vec<&str> = event
            .tags
            .iter()
            .filter(|t| t.name() == Some("e"))
            .filter_map(|t| t.value())
            .collect();
        assert_eq!(e_values, vec![ids[0].as_str(), ids[1].as_str()]);
        assert!(event.content.is_empty());
    }

    #[test]
    fn parse_metadata_lifts_the_display_fields_and_trims() {
        let meta = parse_metadata(
            r#"{"name":" alice ","display_name":"Alice A.","picture":"https://p.example/a.png","nip05":"alice@example.com","about":"ignored","lud16":"x@y"}"#,
        )
        .expect("a JSON object parses");
        assert_eq!(meta.name.as_deref(), Some("alice"));
        assert_eq!(meta.display_name.as_deref(), Some("Alice A."));
        assert_eq!(meta.picture.as_deref(), Some("https://p.example/a.png"));
        assert_eq!(meta.nip05.as_deref(), Some("alice@example.com"));
        assert_eq!(meta.handle(), Some("alice@example.com"));
    }

    #[test]
    fn parse_metadata_treats_empty_and_non_string_values_as_absent() {
        let meta =
            parse_metadata(r#"{"name":"bob","display_name":"   ","picture":42,"nip05":null}"#)
                .unwrap();
        assert_eq!(meta.name.as_deref(), Some("bob"));
        assert!(meta.display_name.is_none());
        assert!(meta.picture.is_none());
        assert!(meta.nip05.is_none());
        assert_eq!(
            meta.handle(),
            Some("bob"),
            "no nip05 → the name is the handle"
        );
        assert_eq!(parse_metadata("{}").unwrap(), NostrMetadata::default());
    }

    #[test]
    fn parse_metadata_refuses_a_non_object() {
        assert!(parse_metadata("not json").is_none());
        assert!(parse_metadata("[1,2]").is_none());
        assert!(parse_metadata("\"alice\"").is_none());
    }

    #[test]
    fn synthetic_actor_id_deterministic() {
        let pk = [42u8; 32];
        let id1 = synthetic_actor_id(&pk);
        let id2 = synthetic_actor_id(&pk);
        assert_eq!(id1, id2);
    }

    #[test]
    fn synthetic_actor_id_different_keys_differ() {
        let id1 = synthetic_actor_id(&[1u8; 32]);
        let id2 = synthetic_actor_id(&[2u8; 32]);
        assert_ne!(id1, id2);
    }

    fn kind_1(tags: Vec<Tag>) -> Event {
        Event {
            id: "b".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind: kind::TEXT_NOTE,
            tags,
            content: "replying".into(),
            sig: "s".repeat(128),
        }
    }

    /// Inbound threading: a swept reply whose parent the nest has mapped
    /// carries `Reference::Reply` to that parent's LOCAL id (the resolver's
    /// answer), so it threads under the swept parent's row.
    #[test]
    fn an_inbound_reply_to_a_resolved_parent_threads_under_its_local_id() {
        let parent = "c".repeat(64);
        let local = fauna_cbor::Cid::from_digest_dag_cbor([7u8; 32]);
        let resolve = |id: &str| (id == parent).then_some(local);
        let event = kind_1(vec![
            Tag::new(vec!["e".into(), "9".repeat(64), "".into(), "root".into()]),
            Tag::new(vec!["e".into(), parent.clone(), "".into(), "reply".into()]),
            Tag::new(vec!["p".into(), "d".repeat(64)]),
        ]);
        let (post, refs) = nostr_event_to_fauna(&event, &resolve).unwrap();
        assert_eq!(refs, vec![Reference::Reply { post_id: local }]);
        assert_eq!(post.references, refs);
    }

    /// A direct reply to a thread root carries only the `root` marker (NIP-10):
    /// the root IS the parent.
    #[test]
    fn an_inbound_root_only_reply_threads_under_the_root() {
        let root = "c".repeat(64);
        let local = fauna_cbor::Cid::from_digest_dag_cbor([7u8; 32]);
        let resolve = |id: &str| (id == root).then_some(local);
        let event = kind_1(vec![Tag::new(vec![
            "e".into(),
            root.clone(),
            "".into(),
            "root".into(),
        ])]);
        let (_, refs) = nostr_event_to_fauna(&event, &resolve).unwrap();
        assert_eq!(refs, vec![Reference::Reply { post_id: local }]);
    }

    /// A quote whose target the nest mapped → `Reference::Quote` to the local id.
    #[test]
    fn an_inbound_quote_of_a_resolved_note_references_its_local_id() {
        let quoted = "c".repeat(64);
        let local = fauna_cbor::Cid::from_digest_dag_cbor([8u8; 32]);
        let resolve = |id: &str| (id == quoted).then_some(local);
        let event = kind_1(vec![Tag::new(vec!["q".into(), quoted.clone()])]);
        let (_, refs) = nostr_event_to_fauna(&event, &resolve).unwrap();
        assert_eq!(refs, vec![Reference::Quote { post_id: local }]);
    }

    /// An `e`/`q` target this nest never mapped threads nothing — the swept
    /// note rests top-level, never under a synthetic id no row carries.
    #[test]
    fn an_inbound_reply_or_quote_to_an_unmapped_note_stays_top_level() {
        let event = kind_1(vec![
            Tag::new(vec!["e".into(), "c".repeat(64), "".into(), "reply".into()]),
            Tag::new(vec!["q".into(), "d".repeat(64)]),
        ]);
        let (post, refs) = nostr_event_to_fauna(&event, &no_local_post).unwrap();
        assert!(refs.is_empty());
        assert!(post.references.is_empty());
    }
}
