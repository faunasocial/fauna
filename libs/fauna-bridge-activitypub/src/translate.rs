// Fauna <-> AP translation.

use anyhow::Result;
use fauna_core::data::{ContentHash, MediaItem, Post, PostBody, Profile, Reference, Timestamp};
use fauna_core::identity::ActorId;

use crate::types::{
    AP_PUBLIC, ApActivity, ApEndpoints, ApMediaAttachment, ApNote, ApPerson, ApPropertyValue,
    ApPublicKey, ApTag, default_context,
};

// ---------------------------------------------------------------------------
// Context for outbound (Fauna → AP) translation
// ---------------------------------------------------------------------------

/// Context needed to build outbound AP objects for a Fauna post.
pub struct OutboundContext {
    pub domain: String,
    pub username: String,
    pub post_id_hex: String,
    /// The fediverse object the post's `Reference::Reply` names, resolved by
    /// the caller through `ap_post_map` + the cached remote actor; `None` for
    /// a top-level post or a target that was never a fediverse object.
    pub in_reply_to: Option<ReferencedApObject>,
    /// Likewise for the post's `Reference::Quote`.
    pub quote_of: Option<ReferencedApObject>,
}

/// A remote AP object a post replies to or quotes (`activitypub.md` § Reply
/// and quote). This crate is DB-free: the nest resolves every field from its
/// own tables, never from anything a client supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferencedApObject {
    /// The object's id — the note's `inReplyTo` / quote target.
    pub ap_url: String,
    /// Its author's actor URI — the `Mention` href and the `to` entry.
    pub author_uri: String,
    /// `@preferredUsername@host` — the `Mention` name.
    pub author_handle: String,
}

/// The FEP-e232 media type of an object `Link` naming an AS2 object.
const AS2_OBJECT_LINK_MEDIA_TYPE: &str =
    "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"";

// ---------------------------------------------------------------------------
// Outbound audience (the account's `default_visibility` setting)
// ---------------------------------------------------------------------------

/// Audience addressing for an outbound Note, from the AP account's
/// `default_visibility` setting. Mastodon-conventional mapping:
/// public → `to=[Public], cc=[followers]`; unlisted → `to=[followers],
/// cc=[Public]`; followers_only → `to=[followers]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteVisibility {
    Public,
    Unlisted,
    FollowersOnly,
}

impl NoteVisibility {
    /// Parse the stored `default_visibility` setting; unknown values fall
    /// back to `Public` (the column default).
    pub fn from_setting(s: &str) -> Self {
        match s {
            "unlisted" => NoteVisibility::Unlisted,
            "followers_only" => NoteVisibility::FollowersOnly,
            _ => NoteVisibility::Public,
        }
    }

    /// The `(to, cc)` audience fields for a note by the given actor.
    fn addressing(self, followers_url: String) -> (Vec<String>, Vec<String>) {
        match self {
            NoteVisibility::Public => (vec![AP_PUBLIC.into()], vec![followers_url]),
            NoteVisibility::Unlisted => (vec![followers_url], vec![AP_PUBLIC.into()]),
            NoteVisibility::FollowersOnly => (vec![followers_url], vec![]),
        }
    }
}

// ---------------------------------------------------------------------------
// Fauna Post → AP Note
// ---------------------------------------------------------------------------

/// Translate a Fauna `Post` to an ActivityPub `Note`.
pub fn fauna_post_to_ap_note(
    post: &Post,
    ctx: &OutboundContext,
    visibility: NoteVisibility,
) -> ApNote {
    let actor_url = format!("https://{}/ap/users/{}", ctx.domain, ctx.username);
    let note_id = format!("{}/notes/{}", actor_url, ctx.post_id_hex);
    let followers_url = format!("{}/followers", actor_url);
    let published = timestamp_to_iso8601(post.created_at);

    let (text, attachments) = extract_text_and_media(post, &actor_url, &ctx.domain);

    let mut content = format!("<p>{}</p>", html_escape(&text));
    let sensitive = post.content_warning.is_some();
    let (mut to, cc) = visibility.addressing(followers_url);
    let mut tag = vec![];

    if let Some(parent) = &ctx.in_reply_to {
        tag.push(ApTag {
            r#type: "Mention".into(),
            href: Some(parent.author_uri.clone()),
            name: Some(parent.author_handle.clone()),
            media_type: None,
        });
        if !to.contains(&parent.author_uri) {
            to.push(parent.author_uri.clone());
        }
    }
    let quote = ctx.quote_of.as_ref().map(|quoted| {
        let url = &quoted.ap_url;
        tag.push(ApTag {
            r#type: "Link".into(),
            href: Some(url.clone()),
            name: Some(format!("RE: {url}")),
            media_type: Some(AS2_OBJECT_LINK_MEDIA_TYPE.into()),
        });
        // Mastodon's own fallback form, for a server that reads no spelling.
        let url = html_escape(url);
        content.push_str(&format!(
            "<p class=\"quote-inline\">RE: <a href=\"{url}\">{url}</a></p>"
        ));
        quoted.ap_url.clone()
    });

    ApNote {
        context: Some(default_context()),
        r#type: "Note".into(),
        id: note_id,
        attributed_to: actor_url.clone(),
        content,
        published,
        to,
        cc,
        in_reply_to: ctx.in_reply_to.as_ref().map(|p| p.ap_url.clone()),
        quote_uri: quote.clone(),
        misskey_quote: quote.clone(),
        quote,
        url: None,
        sensitive: Some(sensitive),
        content_warning: post.content_warning.clone(),
        attachment: attachments,
        tag,
    }
}

/// Extract plain text and media attachments from a PostBody. The text half is
/// [`Post::body_text`] verbatim (this used to hand-derive it, and drifted from
/// that method's own variant coverage before being pointed at it); the
/// attachment half stays local — it needs `actor_url`/`domain` to build AP
/// blob URLs, which `body_text` has no reason to know about.
fn extract_text_and_media(
    post: &Post,
    actor_url: &str,
    domain: &str,
) -> (String, Vec<ApMediaAttachment>) {
    let text = post.body_text();
    let items = post.body.media_items();
    let attachments = items
        .iter()
        .map(|item| media_item_to_attachment(item, actor_url, domain))
        .collect();
    (text, attachments)
}

/// Convert a Fauna `MediaItem` to an AP `Document` attachment.
fn media_item_to_attachment(item: &MediaItem, _actor_url: &str, domain: &str) -> ApMediaAttachment {
    // A bridge-imported item's `remote_url` is a nest-relative proxied path
    // (bridges.md § Unified feed ingestion ruling 4), meaningless to a remote
    // server — emit the origin it wraps. Otherwise, the blob URL from the hash.
    let url = if let Some(origin) = item
        .remote_url
        .as_deref()
        .and_then(fauna_core::data::proxied_media_origin)
    {
        origin
    } else {
        let hash_hex = hex::encode(item.blob_hash.digest());
        format!("https://{}/blobs/{}", domain, hash_hex)
    };
    ApMediaAttachment {
        r#type: "Document".into(),
        media_type: item.media_type.clone(),
        url: Some(url),
        name: None,
    }
}

// ---------------------------------------------------------------------------
// Audience addressing
// ---------------------------------------------------------------------------

/// The ActivityStreams Public collection, in the three spellings AS2 § 5.6
/// treats as equivalent.
const AS_PUBLIC: [&str; 3] = [
    "https://www.w3.org/ns/activitystreams#Public",
    "as:Public",
    "Public",
];

/// True iff an audience (`to` ∪ `cc`) addresses the AS Public collection.
///
/// This is the inbound audience gate: only a publicly addressed Note may
/// enter a public projection (searchable `post/*`); a remote DM or
/// followers-only Note (Public in neither field) must never acquire a
/// public-schema row. Public-in-`cc` (Mastodon "unlisted") counts as public.
pub fn is_publicly_addressed(to: &[String], cc: &[String]) -> bool {
    to.iter()
        .chain(cc.iter())
        .any(|a| AS_PUBLIC.contains(&a.as_str()))
}

/// True iff an audience (`to` ∪ `cc`) is a **direct message's**: it addresses
/// neither the AS Public collection nor a followers collection, so every entry
/// names an actor. A followers-only Note that also mentions someone is not
/// direct — its `to` is the author's followers collection (a URI ending
/// `/followers` on every mainstream server).
///
/// The two inputs are the counterparty's own words; that is fine here, because
/// the only thing this predicate decides is whether their Note is *eligible*
/// for private delivery to the actors it names — never whose Note it is.
pub fn is_directly_addressed(to: &[String], cc: &[String]) -> bool {
    !is_publicly_addressed(to, cc)
        && !to
            .iter()
            .chain(cc.iter())
            .any(|a| a.trim_end_matches('/').ends_with("/followers"))
}

/// A direct Note's body as plain text — what a conversation row stores.
pub fn direct_note_text(note: &ApNote) -> String {
    strip_html(&note.content).trim().to_string()
}

/// Build the Note of one outbound direct message: addressed to `peer_uri`
/// alone (no Public, no followers collection), mentioning them so the far
/// server notifies, its body `text` as escaped HTML.
pub fn build_direct_note(
    actor_url: &str,
    note_id: &str,
    peer_uri: &str,
    text: &str,
    published: Timestamp,
) -> ApNote {
    let content = format!("<p>{}</p>", html_escape(text).replace('\n', "<br>"));
    ApNote {
        context: Some(default_context()),
        r#type: "Note".into(),
        id: note_id.into(),
        attributed_to: actor_url.into(),
        content,
        published: timestamp_to_iso8601(published),
        to: vec![peer_uri.into()],
        cc: Vec::new(),
        in_reply_to: None,
        quote_uri: None,
        misskey_quote: None,
        quote: None,
        url: None,
        sensitive: None,
        content_warning: None,
        attachment: Vec::new(),
        tag: vec![ApTag {
            r#type: "Mention".into(),
            href: Some(peer_uri.into()),
            name: None,
            media_type: None,
        }],
    }
}

// ---------------------------------------------------------------------------
// AP Note → Fauna Post
// ---------------------------------------------------------------------------

/// Translate an inbound AP `Note` to a Fauna `Post`.
///
/// `fallback_created_at` is used when the Note omits `published` (AS2-optional):
/// the caller passes ingest time, so a timestamp-less Note is dated "first seen"
/// rather than dropped. A *malformed* `published` still errors — that is
/// non-conformant, not a legal omission.
///
/// `references` are the note's threading, resolved by the caller (the nest
/// maps `inReplyTo` through `ap_post_map`); this crate stays DB-free, so an
/// `inReplyTo` the caller could not resolve is simply not passed and the note
/// rests top-level (`activitypub.md` § Reply and quote → *The inbound half*).
pub fn ap_note_to_fauna_post(
    note: &ApNote,
    author: &ActorId,
    fallback_created_at: Timestamp,
    references: Vec<Reference>,
) -> Result<Post> {
    let text = strip_html(&note.content);
    let created_at = if note.published.is_empty() {
        fallback_created_at
    } else {
        parse_iso8601_to_timestamp(&note.published)?
    };

    // Skip attachments with no url — AS2 lets `url` be absent, and one such
    // attachment must not fail the whole Note (it just can't be rendered).
    // The url rests as the shared media proxy's nest-relative path, never the
    // remote origin (bridges.md § Unified feed ingestion ruling 4), and the
    // proxy fetches only `https`, so a non-https attachment yields no item.
    // The attachment's `name` — AS2's alt text — rests as the item's `alt`.
    let attachments: Vec<MediaItem> = note
        .attachment
        .iter()
        .filter_map(|att| {
            let path = fauna_core::data::shared_media_proxy_url(att.url.as_deref()?)?;
            Some(MediaItem {
                blob_hash: ContentHash::from_digest_raw([0u8; 32]),
                media_type: att.media_type.clone(),
                size_bytes: 0,
                dimensions: None,
                thumbnail: None,
                remote_url: Some(path),
                alt: fauna_core::data::media_alt(att.name.as_deref()),
            })
        })
        .collect();

    let body = if attachments.is_empty() {
        PostBody::Text {
            content: text,
            facets: vec![],
        }
    } else {
        PostBody::TextWithMedia {
            content: text,
            facets: vec![],
            items: attachments,
        }
    };

    Ok(Post {
        author: *author,
        created_at,
        body,
        references,
        expires_at: None,
        gated: None,
        content_warning: note.content_warning.clone(),
        origin: None,
    })
}

// ---------------------------------------------------------------------------
// Fauna Profile → AP Person
// ---------------------------------------------------------------------------

/// Build the fields every `Person` actor document shares regardless of
/// whether a Fauna profile backs it — id/inbox/outbox/followers/following/url
/// (all derived from the **live** `domain`, never a cached snapshot),
/// public_key, endpoints, context, type, preferred_username — defaulted to the
/// no-profile shape (`name = username`, no summary, no attachments).
/// `manually_approves_followers` is the caller's: it describes the account's
/// follow path, which no profile decides (see [`fauna_profile_to_ap_person`]). [`fauna_profile_to_ap_person`] overrides the
/// profile-derived fields on top; nest's `minimal_person`
/// (`actor_routes.rs`) uses the defaults as-is. The two were independently
/// duplicated before this and drifted once: one used a cached
/// `ap_accounts.actor_url` snapshot while this skeleton (and every sibling
/// route) always derives from the current domain, producing a
/// self-contradictory actor document on a nest enabled before its domain was
/// registered.
pub fn ap_person_skeleton(
    username: &str,
    domain: &str,
    public_key_pem: String,
    manually_approves_followers: bool,
) -> ApPerson {
    let actor_url = format!("https://{}/ap/users/{}", domain, username);
    ApPerson {
        context: default_context(),
        r#type: "Person".into(),
        id: actor_url.clone(),
        preferred_username: username.into(),
        name: username.into(),
        summary: None,
        inbox: format!("{}/inbox", actor_url),
        outbox: format!("{}/outbox", actor_url),
        followers: Some(format!("{}/followers", actor_url)),
        following: Some(format!("{}/following", actor_url)),
        url: Some(actor_url.clone()),
        public_key: ApPublicKey {
            id: format!("{}#main-key", actor_url),
            owner: actor_url.clone(),
            public_key_pem,
        },
        icon: None,
        image: None,
        manually_approves_followers,
        endpoints: Some(ApEndpoints {
            shared_inbox: Some(format!("https://{}/ap/inbox", domain)),
        }),
        attachment: vec![],
    }
}

/// Translate a Fauna `Profile` to an ActivityPub `Person` actor.
///
/// `manually_approves_followers` is the negation of the account's *accept
/// follows by itself* setting (`activitypub.md` § Follow requests) and comes
/// from the caller, never from the profile: the profile's conversation inbox
/// mode says who may message the account, a different question, and peers
/// render the lock and their "request sent" state from this field.
pub fn fauna_profile_to_ap_person(
    profile: &Profile,
    username: &str,
    domain: &str,
    public_key_pem: String,
    manually_approves_followers: bool,
) -> ApPerson {
    let display_name = profile.display_name.clone().unwrap_or_default();
    let summary = profile
        .bio
        .as_ref()
        .map(|bio| format!("<p>{}</p>", html_escape(bio)));

    let attachment: Vec<ApPropertyValue> = profile
        .links
        .iter()
        .map(|link| ApPropertyValue {
            r#type: "PropertyValue".into(),
            name: link.label.clone(),
            value: link.uri.clone(),
        })
        .collect();

    ApPerson {
        name: display_name,
        summary,
        attachment,
        ..ap_person_skeleton(
            username,
            domain,
            public_key_pem,
            manually_approves_followers,
        )
    }
}

// ---------------------------------------------------------------------------
// The instance actor
// ---------------------------------------------------------------------------

/// The nest-level instance actor's URL.
///
/// One home for the shape, because three things must agree on it byte for byte:
/// the served document's `id`, the `keyId` our signed requests advertise, and
/// the route that serves the document. It sits under `/ap/` with every other AP
/// surface — `api-layers.md` classifies exactly that prefix as permanent Layer-6
/// residue — rather than at Mastodon's root-level `/actor`.
pub fn instance_actor_url(domain: &str) -> String {
    format!("https://{domain}/ap/instance")
}

/// The `keyId` a remote dereferences to verify one of our signed requests.
pub fn instance_key_id(domain: &str) -> String {
    format!("{}#main-key", instance_actor_url(domain))
}

/// Build the nest's instance-actor document.
///
/// `Application`, not `Person`: it represents the server itself, authors no
/// content and has no follower graph, and a peer that mistook it for a user
/// would offer people a profile to follow.
///
/// `preferredUsername` is the bare domain, which is Mastodon's own instance-actor
/// shape and what makes `acct:<domain>@<domain>` resolve to it. That matters
/// beyond cosmetics: a peer handed an unfamiliar `keyId` may fetch the document
/// and then WebFinger the `preferredUsername@host` it names back to check the
/// two agree before trusting the key.
pub fn instance_actor_person(domain: &str, public_key_pem: String) -> ApPerson {
    let actor_url = instance_actor_url(domain);
    let shared_inbox = format!("https://{domain}/ap/inbox");

    ApPerson {
        context: default_context(),
        r#type: "Application".into(),
        id: actor_url.clone(),
        preferred_username: domain.into(),
        name: domain.into(),
        summary: None,
        // The nest's existing shared inbox. An actor document must name an
        // inbox, and minting a second one that no route serves would be a
        // dangling link of exactly the kind the `/.well-known/nodeinfo` href
        // became for six weeks (`activitypub.md` § Implementation status).
        inbox: shared_inbox.clone(),
        outbox: format!("{actor_url}/outbox"),
        // Omitted rather than served as empty collections — the instance actor
        // is a signing identity, not a profile, and an advertised link we do
        // not route is the failure mode named above.
        followers: None,
        following: None,
        url: Some(actor_url.clone()),
        public_key: ApPublicKey {
            id: instance_key_id(domain),
            owner: actor_url,
            public_key_pem,
        },
        icon: None,
        image: None,
        // It never accepts a follow, so manual approval is the honest signal.
        manually_approves_followers: true,
        endpoints: Some(ApEndpoints {
            shared_inbox: Some(shared_inbox),
        }),
        attachment: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Activity builders
// ---------------------------------------------------------------------------

/// Build a `Create` activity wrapping a Note.
pub fn build_create_activity(actor_url: &str, activity_id: &str, note: &ApNote) -> ApActivity {
    let published = Some(note.published.clone());
    let to = note.to.clone();
    let cc = note.cc.clone();
    ApActivity {
        context: Some(default_context()),
        r#type: "Create".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: serde_json::to_value(note).unwrap_or(serde_json::Value::Null),
        to,
        cc,
        published,
    }
}

/// Build an `Announce` (boost) activity.
pub fn build_announce_activity(
    actor_url: &str,
    activity_id: &str,
    object_url: &str,
    published: &str,
) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Announce".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: serde_json::Value::String(object_url.into()),
        to: vec![AP_PUBLIC.into()],
        cc: vec![],
        published: Some(published.into()),
    }
}

/// Build a `Like` activity.
pub fn build_like_activity(actor_url: &str, activity_id: &str, object_url: &str) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Like".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: serde_json::Value::String(object_url.into()),
        to: vec![],
        cc: vec![],
        published: None,
    }
}

/// Build a `Follow` activity.
pub fn build_follow_activity(
    actor_url: &str,
    activity_id: &str,
    target_actor_url: &str,
) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Follow".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: serde_json::Value::String(target_actor_url.into()),
        to: vec![target_actor_url.into()],
        cc: vec![],
        published: None,
    }
}

/// Build an `Accept` activity (e.g. accepting a Follow).
pub fn build_accept_activity(
    actor_url: &str,
    activity_id: &str,
    original_activity: serde_json::Value,
) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Accept".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: original_activity,
        to: vec![],
        cc: vec![],
        published: None,
    }
}

/// Build a `Reject` activity (refusing a Follow) — the `Accept` builder's twin.
pub fn build_reject_activity(
    actor_url: &str,
    activity_id: &str,
    original_activity: serde_json::Value,
) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Reject".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: original_activity,
        to: vec![],
        cc: vec![],
        published: None,
    }
}

/// The `Follow` an `Accept` or a `Reject` answers, rebuilt from what the nest
/// keeps of it: the activity id, the requester and the local actor it asked to
/// follow. Both answers embed this rather than the inbound activity's own
/// JSON, so an answer sent at once and one sent later from the stored row are
/// the same object, and nothing a remote wrote is echoed back beyond its id.
pub fn follow_object_for_answer(
    follow_activity_id: &str,
    requester_actor_url: &str,
    local_actor_url: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "Follow",
        "id": follow_activity_id,
        "actor": requester_actor_url,
        "object": local_actor_url,
    })
}

/// Build an `Undo` activity (e.g. undo a Follow or Like).
pub fn build_undo_activity(
    actor_url: &str,
    activity_id: &str,
    original_activity: serde_json::Value,
) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Undo".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: original_activity,
        to: vec![],
        cc: vec![],
        published: None,
    }
}

/// Build a `Delete` activity for a given object URL.
pub fn build_delete_activity(actor_url: &str, activity_id: &str, object_url: &str) -> ApActivity {
    ApActivity {
        context: Some(default_context()),
        r#type: "Delete".into(),
        id: activity_id.into(),
        actor: actor_url.into(),
        object: serde_json::Value::String(object_url.into()),
        to: vec![AP_PUBLIC.into()],
        cc: vec![],
        published: None,
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Escape characters that have special meaning in HTML. Escapes apostrophes
/// too (unlike `fauna_core::markdown`'s own text-node escaper) — `strip_html`
/// below must decode `&#39;` from federated peers regardless, so this crate's
/// own outbound encoder produces the same round-trippable shape.
fn html_escape(s: &str) -> String {
    fauna_core::markdown::escape_html_with(s, true)
}

/// Strip HTML tags from a string, converting block elements to newlines.
fn strip_html(html: &str) -> String {
    // First pass: convert block-level elements and <br> to newlines
    let html = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n")
        .replace("</div>", "\n");
    // Second pass: strip remaining tags
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

/// Format a Fauna `Timestamp` (microseconds since epoch) as an ISO 8601 string.
fn timestamp_to_iso8601(ts: Timestamp) -> String {
    use chrono::{DateTime, Utc};
    let secs = (ts.0 / 1_000_000) as i64;
    let nanos = ((ts.0 % 1_000_000) * 1000) as u32;
    let dt = DateTime::<Utc>::from_timestamp(secs, nanos)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Parse an ISO 8601 / RFC 3339 string into a Fauna `Timestamp` (microseconds).
fn parse_iso8601_to_timestamp(s: &str) -> Result<Timestamp> {
    use chrono::DateTime;
    let dt =
        DateTime::parse_from_rfc3339(s).map_err(|e| anyhow::anyhow!("invalid timestamp: {e}"))?;
    let micros = dt.timestamp() as u64 * 1_000_000 + dt.timestamp_subsec_micros() as u64;
    Ok(Timestamp(micros))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{InboxMode, ProfileLink};
    use fauna_core::identity::ActorId;

    fn test_actor_id() -> ActorId {
        ActorId([1u8; 32])
    }

    /// The instance actor is an `Application`, not a `Person`: it is the server,
    /// not a user, and a peer that took it for a user would offer it a timeline.
    #[test]
    fn instance_actor_is_an_application_named_for_the_domain() {
        let person = instance_actor_person("nest.test", "PEM".into());
        assert_eq!(person.r#type, "Application");
        assert_eq!(person.id, "https://nest.test/ap/instance");
        // Mastodon's own instance-actor shape, and what makes
        // `acct:nest.test@nest.test` resolve to this document.
        assert_eq!(person.preferred_username, "nest.test");
    }

    /// The `keyId` we advertise in a Signature header, the `publicKey.id` in the
    /// document a peer fetches from it, and the document's own `id` must agree —
    /// a peer resolves the first to find the second and checks it owns the third.
    /// One function owns the shape precisely so these cannot drift apart.
    #[test]
    fn instance_key_id_agrees_with_the_served_document() {
        let person = instance_actor_person("nest.test", "PEM".into());
        assert_eq!(person.public_key.id, instance_key_id("nest.test"));
        assert_eq!(person.public_key.owner, person.id);
        assert_eq!(person.public_key.id, format!("{}#main-key", person.id));
        assert_eq!(person.public_key.public_key_pem, "PEM");
    }

    /// Every link the document advertises must be one we actually serve. The
    /// inbox is the shared inbox that already exists; the follower collections
    /// are omitted rather than pointed at routes that do not exist (the
    /// `/.well-known/nodeinfo` href dangled for six weeks on exactly this
    /// mistake).
    #[test]
    fn instance_actor_advertises_only_routes_that_exist() {
        let person = instance_actor_person("nest.test", "PEM".into());
        assert_eq!(person.inbox, "https://nest.test/ap/inbox");
        assert_eq!(
            person
                .endpoints
                .as_ref()
                .and_then(|e| e.shared_inbox.as_deref()),
            Some("https://nest.test/ap/inbox"),
        );
        assert_eq!(person.outbox, "https://nest.test/ap/instance/outbox");
        assert!(
            person.followers.is_none(),
            "the instance actor has no follower graph"
        );
        assert!(
            person.following.is_none(),
            "the instance actor follows nobody"
        );
    }

    /// It signs; it is not a profile. Serialised, it must carry a `publicKey`
    /// and no user-content-bearing fields.
    #[test]
    fn instance_actor_document_carries_no_user_content() {
        let json = serde_json::to_value(instance_actor_person("nest.test", "PEM".into())).unwrap();
        assert!(json.get("publicKey").is_some());
        assert!(json.get("@context").is_some());
        assert!(json.get("summary").is_none());
        assert!(json.get("icon").is_none());
        assert!(json.get("image").is_none());
        assert_eq!(
            json.get("attachment"),
            None,
            "no profile metadata fields on a server identity",
        );
    }

    /// The shared skeleton `fauna_profile_to_ap_person` and nest's
    /// `minimal_person` both build from — every link derives from the live
    /// `domain`, never a cached snapshot, which is the exact drift the two
    /// once had before they shared this function.
    #[test]
    fn person_skeleton_derives_every_link_from_the_live_domain() {
        let person = ap_person_skeleton("alice", "nest.test", "PEM".into(), false);
        assert_eq!(person.id, "https://nest.test/ap/users/alice");
        assert_eq!(person.inbox, "https://nest.test/ap/users/alice/inbox");
        assert_eq!(person.outbox, "https://nest.test/ap/users/alice/outbox");
        assert_eq!(
            person.followers.as_deref(),
            Some("https://nest.test/ap/users/alice/followers")
        );
        assert_eq!(
            person.following.as_deref(),
            Some("https://nest.test/ap/users/alice/following")
        );
        assert_eq!(person.url.as_deref(), Some(person.id.as_str()));
        assert_eq!(person.public_key.id, format!("{}#main-key", person.id));
        assert_eq!(person.public_key.owner, person.id);
        assert_eq!(
            person
                .endpoints
                .as_ref()
                .and_then(|e| e.shared_inbox.as_deref()),
            Some("https://nest.test/ap/inbox")
        );
        // The no-profile-override defaults `minimal_person` relies on as-is.
        assert_eq!(person.name, "alice");
        assert!(person.summary.is_none());
        assert!(!person.manually_approves_followers);
        assert!(person.attachment.is_empty());
    }

    /// `manuallyApprovesFollowers` is the caller's word on both builders — the
    /// account's follow setting — and the profile's conversation inbox mode
    /// has no say in it (`activitypub.md` § Follow requests).
    #[test]
    fn manually_approves_followers_is_the_callers_and_ignores_the_inbox_mode() {
        for manual in [false, true] {
            assert_eq!(
                ap_person_skeleton("alice", "nest.test", "PEM".into(), manual)
                    .manually_approves_followers,
                manual,
                "the no-profile document"
            );
            for inbox_mode in [
                InboxMode::Open,
                InboxMode::AllowKnock,
                InboxMode::ContactsOnly,
                InboxMode::Closed,
            ] {
                let profile = Profile {
                    actor_id: test_actor_id(),
                    display_name: None,
                    bio: None,
                    avatar: None,
                    banner: None,
                    links: vec![],
                    nests: vec![],
                    admin_nests: vec![],
                    load_hint: None,
                    inbox_mode: inbox_mode.clone(),
                    recovery_head: None,
                    updated_at: Timestamp(0),
                };
                let person = fauna_profile_to_ap_person(
                    &profile,
                    "alice",
                    "nest.test",
                    "PEM".into(),
                    manual,
                );
                assert_eq!(
                    person.manually_approves_followers, manual,
                    "inbox mode {inbox_mode:?} must not decide the follow path"
                );
                let wire = serde_json::to_value(&person).unwrap();
                assert_eq!(wire["manuallyApprovesFollowers"], manual);
            }
        }
    }

    #[test]
    fn public_addressing_recognized_in_all_spellings_and_both_fields() {
        let followers = vec!["https://r.example/users/a/followers".to_string()];
        for spelling in [
            "https://www.w3.org/ns/activitystreams#Public",
            "as:Public",
            "Public",
        ] {
            let public = vec![spelling.to_string()];
            assert!(is_publicly_addressed(&public, &[]), "to: {spelling}");
            assert!(
                is_publicly_addressed(&followers, &public),
                "cc (unlisted): {spelling}"
            );
        }
    }

    #[test]
    fn non_public_addressing_is_not_public() {
        let dm_to = vec!["https://local.example/ap/users/alice".to_string()];
        assert!(!is_publicly_addressed(&dm_to, &[]), "DM");
        let followers = vec!["https://r.example/users/a/followers".to_string()];
        assert!(!is_publicly_addressed(&followers, &[]), "followers-only");
        assert!(!is_publicly_addressed(&[], &[]), "unaddressed");
        // A URI merely *containing* "Public" is not the collection.
        let tricky = vec!["https://r.example/NotThePublicCollection".to_string()];
        assert!(!is_publicly_addressed(&tricky, &[]));
    }

    #[test]
    fn text_post_to_ap_note() {
        let post = Post {
            author: test_actor_id(),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: "Hello Fediverse!".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let ctx = OutboundContext {
            domain: "nest.fauna.social".into(),
            username: "alice".into(),
            post_id_hex: "abc123".into(),
            in_reply_to: None,
            quote_of: None,
        };
        let note = fauna_post_to_ap_note(&post, &ctx, NoteVisibility::Public);
        assert_eq!(note.r#type, "Note");
        assert!(note.content.contains("Hello Fediverse!"));
        assert_eq!(
            note.attributed_to,
            "https://nest.fauna.social/ap/users/alice"
        );
    }

    /// The visibility → (to, cc) mapping is the Mastodon-conventional one, and
    /// unlisted/followers_only notes must NOT be publicly addressed in `to`.
    #[test]
    fn note_visibility_addressing() {
        let post = Post {
            author: test_actor_id(),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: "audience test".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let ctx = OutboundContext {
            domain: "nest.example".into(),
            username: "alice".into(),
            post_id_hex: "abc".into(),
            in_reply_to: None,
            quote_of: None,
        };
        let followers = "https://nest.example/ap/users/alice/followers".to_string();

        let public = fauna_post_to_ap_note(&post, &ctx, NoteVisibility::Public);
        assert_eq!(public.to, vec![AP_PUBLIC.to_string()]);
        assert_eq!(public.cc, vec![followers.clone()]);

        let unlisted = fauna_post_to_ap_note(&post, &ctx, NoteVisibility::Unlisted);
        assert_eq!(unlisted.to, vec![followers.clone()]);
        assert_eq!(unlisted.cc, vec![AP_PUBLIC.to_string()]);
        // Unlisted still counts as publicly addressed (Public in cc).
        assert!(is_publicly_addressed(&unlisted.to, &unlisted.cc));

        let followers_only = fauna_post_to_ap_note(&post, &ctx, NoteVisibility::FollowersOnly);
        assert_eq!(followers_only.to, vec![followers]);
        assert!(followers_only.cc.is_empty());
        assert!(!is_publicly_addressed(
            &followers_only.to,
            &followers_only.cc
        ));
    }

    #[test]
    fn note_visibility_from_setting() {
        assert_eq!(
            NoteVisibility::from_setting("public"),
            NoteVisibility::Public
        );
        assert_eq!(
            NoteVisibility::from_setting("unlisted"),
            NoteVisibility::Unlisted
        );
        assert_eq!(
            NoteVisibility::from_setting("followers_only"),
            NoteVisibility::FollowersOnly
        );
        // Unknown/legacy values fall back to the column default.
        assert_eq!(
            NoteVisibility::from_setting("banana"),
            NoteVisibility::Public
        );
    }

    #[test]
    fn post_with_content_warning() {
        let post = Post {
            author: test_actor_id(),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: "Spoiler content".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: Some("CW: spoilers".into()),
            origin: None,
        };
        let ctx = OutboundContext {
            domain: "nest.fauna.social".into(),
            username: "alice".into(),
            post_id_hex: "abc123".into(),
            in_reply_to: None,
            quote_of: None,
        };
        let note = fauna_post_to_ap_note(&post, &ctx, NoteVisibility::Public);
        assert_eq!(note.content_warning.as_deref(), Some("CW: spoilers"));
        assert_eq!(note.sensitive, Some(true));
    }

    fn plain_post(content: &str) -> Post {
        Post {
            author: test_actor_id(),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: content.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn ctx_with(
        in_reply_to: Option<ReferencedApObject>,
        quote_of: Option<ReferencedApObject>,
    ) -> OutboundContext {
        OutboundContext {
            domain: "nest.example".into(),
            username: "alice".into(),
            post_id_hex: "abc".into(),
            in_reply_to,
            quote_of,
        }
    }

    fn bobs_status() -> ReferencedApObject {
        ReferencedApObject {
            ap_url: "https://masto.example/users/bob/statuses/9".into(),
            author_uri: "https://masto.example/users/bob".into(),
            author_handle: "@bob@masto.example".into(),
        }
    }

    /// A note built with no reference is byte-identical to the pre-reply shape:
    /// none of the reply/quote fields or tags appear on the wire.
    #[test]
    fn a_note_with_no_reference_serializes_exactly_as_before() {
        let note = fauna_post_to_ap_note(
            &plain_post("plain"),
            &ctx_with(None, None),
            NoteVisibility::Public,
        );
        let want = serde_json::json!({
            "@context": crate::types::default_context(),
            "type": "Note",
            "id": "https://nest.example/ap/users/alice/notes/abc",
            "attributedTo": "https://nest.example/ap/users/alice",
            "content": "<p>plain</p>",
            "published": note.published,
            "to": [AP_PUBLIC],
            "cc": ["https://nest.example/ap/users/alice/followers"],
            "sensitive": false,
        });
        assert_eq!(serde_json::to_value(&note).unwrap(), want);
    }

    /// A reply to a fediverse status: `inReplyTo` = its `ap_url`, a `Mention`
    /// of its author, and the author's actor URI in `to` beside the
    /// visibility's own addressing (`activitypub.md` § Reply and quote).
    #[test]
    fn a_reply_note_carries_in_reply_to_a_mention_and_the_author_in_to() {
        let bob = bobs_status();
        let note = fauna_post_to_ap_note(
            &plain_post("hi bob"),
            &ctx_with(Some(bob.clone()), None),
            NoteVisibility::Public,
        );
        assert_eq!(note.in_reply_to.as_deref(), Some(bob.ap_url.as_str()));
        assert_eq!(note.to, vec![AP_PUBLIC.to_string(), bob.author_uri.clone()]);
        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(
            json["tag"],
            serde_json::json!([{
                "type": "Mention",
                "href": "https://masto.example/users/bob",
                "name": "@bob@masto.example",
            }])
        );
        assert_eq!(note.content, "<p>hi bob</p>");
        assert!(json.get("quote").is_none());
    }

    /// A quote: the FEP-e232 Link tag, `quote` + `quoteUri` + `_misskey_quote`,
    /// and the `RE: <ap_url>` line appended to `content` for a server that
    /// reads none of them.
    #[test]
    fn a_quote_note_carries_every_quote_spelling_and_the_re_line() {
        let bob = bobs_status();
        let url = bob.ap_url.clone();
        let note = fauna_post_to_ap_note(
            &plain_post("look"),
            &ctx_with(None, Some(bob)),
            NoteVisibility::Public,
        );
        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(json["quote"], url);
        assert_eq!(json["quoteUri"], url);
        assert_eq!(json["_misskey_quote"], url);
        assert_eq!(
            json["tag"],
            serde_json::json!([{
                "type": "Link",
                "mediaType": "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"",
                "href": url,
                "name": format!("RE: {url}"),
            }])
        );
        assert_eq!(
            note.content,
            format!("<p>look</p><p class=\"quote-inline\">RE: <a href=\"{url}\">{url}</a></p>")
        );
        assert!(note.in_reply_to.is_none());
    }

    /// Inbound threading: the caller resolves `inReplyTo` to a local post and
    /// the translation carries the reference it was handed.
    #[test]
    fn an_inbound_note_carries_the_references_its_caller_resolved() {
        let note: ApNote = serde_json::from_value(serde_json::json!({
            "type": "Note",
            "id": "https://masto.example/users/bob/statuses/10",
            "attributedTo": "https://masto.example/users/bob",
            "content": "<p>re</p>",
            "inReplyTo": "https://nest.example/ap/users/alice/notes/abc",
        }))
        .unwrap();
        let target = ContentHash::from_digest_raw([5u8; 32]);
        let reply = Reference::Reply { post_id: target };
        let author = crate::identity::synthetic_actor_id(&note.attributed_to);
        let post =
            ap_note_to_fauna_post(&note, &author, Timestamp(0), vec![reply.clone()]).unwrap();
        assert_eq!(post.references, vec![reply]);
    }

    #[test]
    fn ap_note_to_fauna_post_basic() {
        let note = crate::types::ApNote {
            context: Some(crate::types::default_context()),
            r#type: "Note".into(),
            id: "https://mastodon.social/users/bob/statuses/123".into(),
            attributed_to: "https://mastodon.social/users/bob".into(),
            content: "<p>Hello from Mastodon!</p>".into(),
            published: "2026-03-20T12:00:00Z".into(),
            to: vec![crate::types::AP_PUBLIC.into()],
            cc: vec![],
            in_reply_to: None,
            quote: None,
            quote_uri: None,
            misskey_quote: None,
            attachment: vec![],
            tag: vec![],
            sensitive: None,
            content_warning: None,
            url: None,
        };
        let actor_id = crate::identity::synthetic_actor_id(&note.attributed_to);
        let post = ap_note_to_fauna_post(&note, &actor_id, Timestamp(0), vec![]).unwrap();
        if let PostBody::Text { content, .. } = &post.body {
            assert_eq!(content, "Hello from Mastodon!");
        } else {
            panic!("expected Text body");
        }
    }

    /// bridges.md § Unified feed ingestion ruling 4: an inbound attachment
    /// rests as the shared proxy's nest-relative path, never the origin; a
    /// non-https one yields no item; an outbound re-emit recovers the origin.
    #[test]
    fn ap_note_attachments_rest_as_proxied_paths() {
        let att = |url: &str| crate::types::ApMediaAttachment {
            r#type: "Document".into(),
            media_type: "image/png".into(),
            url: Some(url.into()),
            name: None,
        };
        let note = crate::types::ApNote {
            context: Some(crate::types::default_context()),
            r#type: "Note".into(),
            id: "https://mastodon.social/users/bob/statuses/124".into(),
            attributed_to: "https://mastodon.social/users/bob".into(),
            content: "<p>pic</p>".into(),
            published: "2026-03-20T12:00:00Z".into(),
            to: vec![crate::types::AP_PUBLIC.into()],
            cc: vec![],
            in_reply_to: None,
            quote: None,
            quote_uri: None,
            misskey_quote: None,
            attachment: vec![
                att("https://files.mastodon.social/a.png"),
                att("http://files.mastodon.social/b.png"),
            ],
            tag: vec![],
            sensitive: None,
            content_warning: None,
            url: None,
        };
        let actor_id = crate::identity::synthetic_actor_id(&note.attributed_to);
        let post = ap_note_to_fauna_post(&note, &actor_id, Timestamp(0), vec![]).unwrap();
        let items = post.body.media_items();
        assert_eq!(items.len(), 1, "the http attachment yields no item");
        assert_eq!(
            items[0].remote_url.as_deref(),
            Some("/api/v1/media/proxy?url=https%3A%2F%2Ffiles.mastodon.social%2Fa.png")
        );
        let back = media_item_to_attachment(&items[0], "", "example.org");
        assert_eq!(
            back.url.as_deref(),
            Some("https://files.mastodon.social/a.png")
        );
    }

    /// An attachment's AS2 `name` — the alt text Mastodon sends — rests as the
    /// item's `alt`; an absent or empty one rests as `None`, never `Some("")`.
    #[test]
    fn ap_note_attachment_name_rests_as_the_items_alt() {
        let att = |url: &str, name: Option<&str>| crate::types::ApMediaAttachment {
            r#type: "Document".into(),
            media_type: "image/png".into(),
            url: Some(url.into()),
            name: name.map(Into::into),
        };
        let note = crate::types::ApNote {
            context: Some(crate::types::default_context()),
            r#type: "Note".into(),
            id: "https://mastodon.social/users/bob/statuses/125".into(),
            attributed_to: "https://mastodon.social/users/bob".into(),
            content: "<p>pics</p>".into(),
            published: "2026-03-20T12:00:00Z".into(),
            to: vec![crate::types::AP_PUBLIC.into()],
            cc: vec![],
            in_reply_to: None,
            quote: None,
            quote_uri: None,
            misskey_quote: None,
            attachment: vec![
                att(
                    "https://files.mastodon.social/a.png",
                    Some("a cat on a sofa"),
                ),
                att("https://files.mastodon.social/b.png", None),
                att("https://files.mastodon.social/c.png", Some("  ")),
            ],
            tag: vec![],
            sensitive: None,
            content_warning: None,
            url: None,
        };
        let actor_id = crate::identity::synthetic_actor_id(&note.attributed_to);
        let post = ap_note_to_fauna_post(&note, &actor_id, Timestamp(0), vec![]).unwrap();
        let alts: Vec<Option<&str>> = post
            .body
            .media_items()
            .iter()
            .map(|item| item.alt.as_deref())
            .collect();
        assert_eq!(alts, vec![Some("a cat on a sofa"), None, None]);
    }

    #[test]
    fn profile_to_ap_person() {
        let profile = Profile {
            actor_id: test_actor_id(),
            display_name: Some("Alice".into()),
            bio: Some("Hello".into()),
            avatar: None,
            banner: None,
            links: vec![ProfileLink {
                label: "Website".into(),
                uri: "https://alice.me".into(),
            }],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(1_710_892_800_000_000),
        };
        let person = fauna_profile_to_ap_person(
            &profile,
            "alice",
            "nest.fauna.social",
            "-----BEGIN PUBLIC KEY-----\nMIIB...".into(),
            false,
        );
        assert_eq!(person.preferred_username, "alice");
        assert_eq!(person.name, "Alice");
        assert_eq!(
            person.inbox,
            "https://nest.fauna.social/ap/users/alice/inbox"
        );
    }

    #[test]
    fn html_escape_roundtrip() {
        let raw = "Hello <World> & \"friends\"";
        let escaped = html_escape(raw);
        assert!(escaped.contains("&lt;"));
        assert!(escaped.contains("&gt;"));
        assert!(escaped.contains("&amp;"));
        assert!(escaped.contains("&quot;"));
    }

    /// A direct audience names actors only: `Public` in either field, or a
    /// followers collection in either field, is not direct.
    #[test]
    fn direct_audience_holds_neither_public_nor_a_followers_collection() {
        let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let alice = "https://local.example/ap/users/alice";
        let followers = "https://remote.example/users/bob/followers";
        assert!(is_directly_addressed(&s(&[alice]), &[]));
        assert!(is_directly_addressed(
            &s(&[alice, "https://other.example/users/carol"]),
            &[]
        ));
        assert!(!is_directly_addressed(&s(&[alice, AS_PUBLIC[0]]), &[]));
        assert!(!is_directly_addressed(&s(&[alice]), &s(&[AS_PUBLIC[0]])));
        assert!(!is_directly_addressed(&s(&[followers]), &s(&[alice])));
        assert!(!is_directly_addressed(&s(&[alice]), &s(&[followers])));
        assert!(!is_directly_addressed(
            &s(&["https://remote.example/users/bob/followers/"]),
            &[]
        ));
    }

    /// An outbound direct Note is addressed to the peer alone, mentions them,
    /// and reads back as the text that was sent.
    #[test]
    fn direct_note_is_addressed_to_the_peer_alone_and_round_trips_its_text() {
        let peer = "https://remote.example/users/bob";
        let note = build_direct_note(
            "https://local.example/ap/users/alice",
            "https://local.example/ap/users/alice/dm/7",
            peer,
            "hi <bob> & co\nsecond line",
            Timestamp(1_700_000_000_000_000),
        );
        assert_eq!(note.to, vec![peer.to_string()]);
        assert!(note.cc.is_empty());
        assert!(is_directly_addressed(&note.to, &note.cc));
        assert_eq!(note.tag.len(), 1);
        assert_eq!(note.tag[0].r#type, "Mention");
        assert_eq!(note.tag[0].href.as_deref(), Some(peer));
        assert_eq!(direct_note_text(&note), "hi <bob> & co\nsecond line");
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("<p>Hello!</p>"), "Hello!");
        assert_eq!(strip_html("<p>Line 1</p><p>Line 2</p>"), "Line 1\nLine 2");
        assert_eq!(strip_html("Hello<br>World"), "Hello\nWorld");
    }

    #[test]
    fn strip_html_entities() {
        assert_eq!(strip_html("&lt;b&gt;bold&lt;/b&gt;"), "<b>bold</b>");
        assert_eq!(strip_html("AT&amp;T"), "AT&T");
    }

    #[test]
    fn timestamp_roundtrip() {
        let ts = Timestamp(1_710_892_800_000_000);
        let iso = timestamp_to_iso8601(ts);
        assert_eq!(iso, "2024-03-20T00:00:00Z");
        let parsed = parse_iso8601_to_timestamp(&iso).unwrap();
        assert_eq!(parsed, ts);
    }

    #[test]
    fn activity_builders_produce_correct_types() {
        let actor = "https://nest.fauna.social/ap/users/alice";
        let activity_id = "https://nest.fauna.social/ap/users/alice/activities/1";

        let follow = build_follow_activity(actor, activity_id, "https://mastodon.social/users/bob");
        assert_eq!(follow.r#type, "Follow");
        assert_eq!(follow.actor, actor);

        let like = build_like_activity(
            actor,
            activity_id,
            "https://mastodon.social/users/bob/statuses/1",
        );
        assert_eq!(like.r#type, "Like");

        let announce = build_announce_activity(
            actor,
            activity_id,
            "https://mastodon.social/users/bob/statuses/1",
            "2026-03-20T12:00:00Z",
        );
        assert_eq!(announce.r#type, "Announce");

        let delete = build_delete_activity(
            actor,
            activity_id,
            "https://nest.fauna.social/ap/users/alice/notes/1",
        );
        assert_eq!(delete.r#type, "Delete");

        let accept =
            build_accept_activity(actor, activity_id, serde_json::json!({"type": "Follow"}));
        assert_eq!(accept.r#type, "Accept");

        let undo = build_undo_activity(actor, activity_id, serde_json::json!({"type": "Follow"}));
        assert_eq!(undo.r#type, "Undo");
    }

    /// `Reject` is `Accept`'s twin: the same envelope around the same `Follow`
    /// object, differing in the type alone — so a peer that matches an answer
    /// to its request by the embedded `Follow` id reads both the same way.
    #[test]
    fn reject_is_the_accept_builders_twin() {
        let actor = "https://nest.test/ap/users/alice";
        let follow = follow_object_for_answer(
            "https://remote.example/activities/follow-1",
            "https://remote.example/users/bob",
            actor,
        );
        assert_eq!(follow["type"], "Follow");
        assert_eq!(follow["id"], "https://remote.example/activities/follow-1");
        assert_eq!(follow["actor"], "https://remote.example/users/bob");
        assert_eq!(follow["object"], actor);

        let accept = build_accept_activity(actor, "https://nest.test/a/1", follow.clone());
        let reject = build_reject_activity(actor, "https://nest.test/a/1", follow.clone());
        assert_eq!(reject.r#type, "Reject");
        assert_eq!(reject.object, follow);

        let mut accept = serde_json::to_value(&accept).unwrap();
        let reject = serde_json::to_value(&reject).unwrap();
        accept["type"] = serde_json::json!("Reject");
        assert_eq!(accept, reject, "the two answers differ in their type alone");
    }
}
