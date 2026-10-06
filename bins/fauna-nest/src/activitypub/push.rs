//! The ActivityPub produce direction: the `Create`-push of local posts to
//! accepted fediverse followers, and its `Delete` twin (Leg D of the
//! post-delete propagation set).
//!
//! Owner doc: `docs/goal/behavior/activitypub.md` §§ The produce direction /
//! Post deletion. Both entry points are fire-and-forget + non-fatal: a push
//! hiccup must never fail the local create/delete. Delivery is durable —
//! jobs go straight into `ap_delivery_queue` and the sync worker's poll
//! delivers them (retries, backoff, dead-inbox skip ride unchanged).

use std::sync::Arc;

use fauna_bridge_activitypub::translate::{
    NoteVisibility, OutboundContext, ReferencedApObject, build_create_activity,
    build_delete_activity, fauna_post_to_ap_note,
};
use fauna_core::data::{Post, Reference};

use crate::routes::AppState;

/// Spawn the `Create`-push for a freshly ingested local post. Clean no-op
/// when the author has no enabled AP account or no accepted followers.
pub fn spawn_create_push(state: Arc<AppState>, author: [u8; 32], post_id: [u8; 32], body: Vec<u8>) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        if let Err(e) = create_push_inner(&state, author, post_id, &body).await {
            tracing::warn!("ap create-push: {e:#}");
        }
    });
}

/// Spawn the `Delete`-push for a deleted local post. Pushes iff a `Create`
/// was pushed (the `ap_post_map` row is the witness); clean no-op otherwise.
pub fn spawn_delete_push(state: Arc<AppState>, actor: [u8; 32], post_id: [u8; 32]) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        if let Err(e) = delete_push_inner(&state, actor, post_id).await {
            tracing::warn!("ap delete-push: {e:#}");
        }
    });
}

pub(crate) async fn create_push_inner(
    state: &Arc<AppState>,
    author: [u8; 32],
    post_id: [u8; 32],
    body: &[u8],
) -> anyhow::Result<()> {
    let actor_hex = hex::encode(author);
    let account = {
        let conn = state.db.conn().await;
        super::db_helpers::get_account(&conn, &actor_hex)?
    };
    // Enabling federation is the opt-in; no account or disabled → no push.
    let Some(account) = account else {
        return Ok(());
    };
    if !account.enabled {
        return Ok(());
    }

    // Native signed posts are embed-as-bytes; bridge-ingested posts are bare —
    // `decode_stored_post` handles both (the same decode the pull outbox uses).
    let Some(post) = crate::db::posts::decode_stored_post(body) else {
        anyhow::bail!("undecodable post body for {}", hex::encode(post_id));
    };

    // The create-time half of the one off-box servability rule
    // (`db::public_servability::publishable_off_box_at_create`): a gated
    // (monetized/paywalled) post is never world-broadcast — pushing it would
    // republish the teaser as an ordinary free Note without the paywall
    // context the web render carries — and an archive-imported post is served
    // on Fauna, never re-broadcast (`archive-import.md` § Compatibility →
    // *Slice-3 rulings*, ruling 1). Both live in the public `post/*`
    // projection and decode fine, so nothing upstream stops them; the pull
    // outbox applies the SQL predicate, and this leg sees the decoded post at
    // the instant it lands, before any row exists to filter.
    if !crate::db::public_servability::publishable_off_box_at_create(&post) {
        return Ok(());
    }

    let domain = state.handle_domain();
    let post_id_hex = hex::encode(post_id);
    let refs = {
        let conn = state.db.conn().await;
        resolve_ap_references(&conn, &post)?
    };
    let ctx = OutboundContext {
        domain,
        username: account.username.clone(),
        post_id_hex: post_id_hex.clone(),
        in_reply_to: refs.in_reply_to,
        quote_of: refs.quote_of,
    };
    // Push/pull-symmetric object identity: same note id + activity id the
    // pull outbox derives, so a pushed and a pulled copy are the same object.
    let visibility = NoteVisibility::from_setting(&account.default_visibility);
    let note = fauna_post_to_ap_note(&post, &ctx, visibility);
    let activity_id = format!("{}/activities/{}", account.actor_url, post_id_hex);
    let create = build_create_activity(&account.actor_url, &activity_id, &note);
    let activity_json = serde_json::to_string(&create)?;

    let conn = state.db.conn().await;
    // The map row makes the push round-trip coherent: inbound interactions on
    // the note URL resolve, and the Delete leg gets its pushed-witness.
    // Written even with zero followers — the pull outbox serves the same URL.
    super::db_helpers::insert_post_map(&conn, &post_id_hex, &note.id, &actor_hex, None)?;
    let mut inboxes = follower_inboxes(&conn, &actor_hex)?;
    // A reply or quote also reaches the referenced author — the addressee is
    // the intent, whether or not they follow us.
    for inbox in refs.author_inboxes {
        if !inboxes.contains(&inbox) {
            inboxes.push(inbox);
        }
    }
    for inbox in &inboxes {
        super::db_helpers::enqueue_delivery(&conn, &activity_json, inbox)?;
    }
    drop(conn);

    if !inboxes.is_empty() {
        // Deliver now rather than at the worker's next 30s tick: a post
        // reaching its followers is the latency-sensitive path.
        state.activitypub.delivery_nudge.notify_one();
        tracing::info!(
            post = %post_id_hex,
            inboxes = inboxes.len(),
            "ap: enqueued Create push"
        );
    }
    Ok(())
}

pub(crate) async fn delete_push_inner(
    state: &Arc<AppState>,
    actor: [u8; 32],
    post_id: [u8; 32],
) -> anyhow::Result<()> {
    let actor_hex = hex::encode(actor);
    let post_id_hex = hex::encode(post_id);

    let conn = state.db.conn().await;
    // The account is needed for the actor URL + signing key; deliberately NOT
    // gated on `enabled` — a Create pushed before the user disabled federation
    // must still be chased by its Delete.
    let Some(account) = super::db_helpers::get_account(&conn, &actor_hex)? else {
        return Ok(());
    };
    // Pushed-witness: only a post whose Create was pushed gets a Delete.
    // Tombstoned rows included — a delete retry (AlreadyGone) must still find
    // the URL of a Delete a first attempt failed to enqueue/deliver.
    let Some(note_url) = super::db_helpers::get_local_note_url(&conn, &post_id_hex, &actor_hex)?
    else {
        return Ok(());
    };

    let activity_id = format!("{}/activities/{}#delete", account.actor_url, post_id_hex);
    let delete = build_delete_activity(&account.actor_url, &activity_id, &note_url);
    let activity_json = serde_json::to_string(&delete)?;

    let inboxes = follower_inboxes(&conn, &actor_hex)?;
    for inbox in &inboxes {
        super::db_helpers::enqueue_delivery(&conn, &activity_json, inbox)?;
    }
    // Tombstone regardless of follower count — the local delete stands and the
    // mapping must stop resolving for inbound interactions.
    super::db_helpers::tombstone_post_map(&conn, &post_id_hex)?;
    drop(conn);

    if !inboxes.is_empty() {
        // Chase the pushed copy now — a deleted post outliving its derivation
        // for 30s is the visible half of the propagation invariant.
        state.activitypub.delivery_nudge.notify_one();
    }

    tracing::info!(
        post = %post_id_hex,
        inboxes = inboxes.len(),
        "ap: enqueued Delete push + tombstoned map row"
    );
    Ok(())
}

/// A post's fediverse reply/quote targets, resolved for the translator, plus
/// the referenced authors' delivery inboxes (`activitypub.md` § Reply and
/// quote).
#[derive(Debug, Default)]
pub(crate) struct ResolvedApReferences {
    pub in_reply_to: Option<ReferencedApObject>,
    pub quote_of: Option<ReferencedApObject>,
    pub author_inboxes: Vec<String>,
}

/// Resolve the post's first `Reply` and first `Quote` through `ap_post_map`.
/// Shared by the `Create`-push and the pull outbox so a pushed and a pulled
/// copy stay the same AP object. Every field comes from this nest's own
/// tables — an ingested note's cached remote actor, or, for a reply to one of
/// our own pushed notes, the local account — never from the client. A target
/// with no live map row (a native, bluesky or nostr post) resolves to nothing
/// and the post federates as a plain top-level note.
pub(crate) fn resolve_ap_references(
    conn: &rusqlite::Connection,
    post: &Post,
) -> anyhow::Result<ResolvedApReferences> {
    let mut out = ResolvedApReferences::default();
    for reference in &post.references {
        let (slot, post_id) = match reference {
            Reference::Reply { post_id } if out.in_reply_to.is_none() => (0, post_id),
            Reference::Quote { post_id } if out.quote_of.is_none() => (1, post_id),
            _ => continue,
        };
        let Some((object, inbox)) = resolve_ap_reference(conn, &hex::encode(post_id.digest()))?
        else {
            continue;
        };
        if let Some(inbox) = inbox
            && !out.author_inboxes.contains(&inbox)
        {
            out.author_inboxes.push(inbox);
        }
        if slot == 0 {
            out.in_reply_to = Some(object);
        } else {
            out.quote_of = Some(object);
        }
    }
    Ok(out)
}

/// One target: the object + its author, and the author's inbox when remote.
fn resolve_ap_reference(
    conn: &rusqlite::Connection,
    target_hex: &str,
) -> anyhow::Result<Option<(ReferencedApObject, Option<String>)>> {
    let Some(target) = super::db_helpers::get_ap_reference_target(conn, target_hex)? else {
        return Ok(None);
    };
    if let Some(uri) = target.remote_actor_uri {
        let cached = super::db_helpers::get_remote_actor(conn, &uri)?;
        let username = cached
            .and_then(|a| a.preferred_username)
            .filter(|u| !u.is_empty())
            .or_else(|| uri.rsplit('/').next().map(str::to_string))
            .unwrap_or_default();
        let inbox = super::db_helpers::resolve_delivery_inbox(conn, Some(&uri), &target.ap_url);
        let object = ReferencedApObject {
            author_handle: format!("@{username}@{}", url_host(&uri)),
            author_uri: uri,
            ap_url: target.ap_url,
        };
        return Ok(Some((object, Some(inbox))));
    }
    // A `Create`-push row: our own note, authored by the row's local actor —
    // the self-thread. Nothing to deliver to ourselves.
    let Some(account) = super::db_helpers::get_account(conn, &target.actor_id)? else {
        return Ok(None);
    };
    if !target.ap_url.starts_with(&account.actor_url) {
        return Ok(None);
    }
    let object = ReferencedApObject {
        author_handle: format!("@{}@{}", account.username, url_host(&account.actor_url)),
        author_uri: account.actor_url,
        ap_url: target.ap_url,
    };
    Ok(Some((object, None)))
}

/// The authority of an `https://host[:port]/…` URL.
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// Deduplicated delivery inboxes for the actor's ACCEPTED inbound followers:
/// shared inbox preferred, per-actor inbox fallback, unresolvable followers
/// (no cached remote actor) skipped — delivery is best-effort by contract.
fn follower_inboxes(
    conn: &rusqlite::Connection,
    local_actor_id: &str,
) -> anyhow::Result<Vec<String>> {
    let follows = super::db_helpers::list_followers(conn, local_actor_id)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in follows {
        if f.state != "accepted" {
            continue;
        }
        let Some(actor) = super::db_helpers::get_remote_actor(conn, &f.remote_actor_uri)? else {
            continue;
        };
        let inbox = actor
            .shared_inbox
            .filter(|s| !s.is_empty())
            .unwrap_or(actor.inbox);
        if inbox.is_empty() {
            continue;
        }
        if seen.insert(inbox.clone()) {
            out.push(inbox);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activitypub::db_helpers::{self, RemoteActor};

    async fn state() -> Arc<AppState> {
        Arc::new(crate::activitypub::ap_state_for_test().await)
    }

    fn test_post_body() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_710_892_800_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "hello fediverse".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).expect("encode")
    }

    /// A gated (monetized) post: `body` is the public **preview**, the full
    /// content is ciphertext at `encrypted_ref` (`GatedInfo` doc).
    fn gated_post_body() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_710_892_800_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "subscriber-only teaser".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(fauna_core::subscription::types::GatedInfo {
                encrypted_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x33u8; 32]),
                key_access: fauna_core::subscription::types::KeyAccess::Broadcast {
                    key_blob_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x44u8; 32]),
                },
                tier: "premium".into(),
                tier_rank: 1,
                seal_id: fauna_cbor::Cid::from_digest_dag_cbor([0x55u8; 32]),
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).expect("encode")
    }

    /// A public post re-authored from an export archive: an ordinary signed
    /// post whose envelope carries `origin.platform` (`Post::source_token`
    /// indexes it under that token).
    fn archive_origin_post_body() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_600_000_000_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "a post from 2020, imported today".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: Some(fauna_core::data::PostOrigin {
                platform: fauna_core::source::FACEBOOK.into(),
                url: None,
            }),
        };
        fauna_core::encoding::canonical_encode(&post).expect("encode")
    }

    async fn seed_account(state: &Arc<AppState>, author: [u8; 32], visibility: &str) -> String {
        let actor_hex = hex::encode(author);
        let conn = state.db.conn().await;
        db_helpers::create_account(
            &conn,
            &actor_hex,
            "alice",
            "https://localhost/ap/users/alice",
            b"secret",
            "pem",
        )
        .expect("account");
        db_helpers::update_settings(
            &conn,
            &actor_hex,
            &db_helpers::ApSettings {
                default_visibility: Some(visibility.into()),
                ..Default::default()
            },
        )
        .expect("settings");
        actor_hex
    }

    async fn seed_follower(
        state: &Arc<AppState>,
        actor_hex: &str,
        remote_uri: &str,
        follow_state: &str,
        shared_inbox: Option<&str>,
        cache_actor: bool,
    ) {
        let conn = state.db.conn().await;
        db_helpers::create_follow(&conn, actor_hex, remote_uri, "inbound", None).expect("follow");
        if follow_state == "accepted" {
            db_helpers::accept_follow(&conn, actor_hex, remote_uri, "inbound").expect("accept");
        }
        if cache_actor {
            db_helpers::upsert_remote_actor(
                &conn,
                &RemoteActor {
                    uri: remote_uri.into(),
                    inbox: format!("{remote_uri}/inbox"),
                    shared_inbox: shared_inbox.map(String::from),
                    public_key_pem: "pem".into(),
                    preferred_username: None,
                    display_name: None,
                    avatar_url: None,
                    banner_url: None,
                    summary: None,
                    last_fetched: 1,
                },
            )
            .expect("remote actor");
        }
    }

    async fn pending_jobs(state: &Arc<AppState>) -> Vec<db_helpers::DeliveryJob> {
        let conn = state.db.conn().await;
        db_helpers::get_pending_deliveries(&conn, 100).expect("pending")
    }

    const BOB: &str = "https://masto.example/users/bob";
    const BOB_STATUS: &str = "https://masto.example/users/bob/statuses/9";
    const BOB_INBOX: &str = "https://masto.example/users/bob/inbox";
    const INGESTED: [u8; 32] = [0x99u8; 32];

    fn reply_to(post_id: fauna_core::data::ContentHash) -> Reference {
        Reference::Reply { post_id }
    }

    fn quote_of(post_id: fauna_core::data::ContentHash) -> Reference {
        Reference::Quote { post_id }
    }

    /// A post by the author whose one reference names the local post `target`.
    fn referencing_post(
        target: [u8; 32],
        reference: fn(fauna_core::data::ContentHash) -> Reference,
    ) -> fauna_core::data::Post {
        fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_710_892_900_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "answering".into(),
                facets: vec![],
            },
            references: vec![reference(fauna_core::data::ContentHash::from_digest_raw(
                target,
            ))],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn referencing_post_body(
        target: [u8; 32],
        reference: fn(fauna_core::data::ContentHash) -> Reference,
    ) -> Vec<u8> {
        fauna_core::encoding::canonical_encode(&referencing_post(target, reference))
            .expect("encode")
    }

    /// Bob's status, ingested from the fediverse and mapped at `INGESTED`,
    /// with his actor cached the way the inbound ingest caches it.
    async fn seed_ingested_status(state: &Arc<AppState>, actor_hex: &str) {
        let conn = state.db.conn().await;
        db_helpers::insert_post_map(
            &conn,
            &hex::encode(INGESTED),
            BOB_STATUS,
            actor_hex,
            Some(BOB),
        )
        .expect("map");
        db_helpers::upsert_remote_actor(
            &conn,
            &RemoteActor {
                uri: BOB.into(),
                inbox: BOB_INBOX.into(),
                shared_inbox: None,
                public_key_pem: "pem".into(),
                preferred_username: Some("bob".into()),
                display_name: None,
                avatar_url: None,
                banner_url: None,
                summary: None,
                last_fetched: 1,
            },
        )
        .expect("remote actor");
    }

    fn note_of(job: &db_helpers::DeliveryJob) -> serde_json::Value {
        let activity: serde_json::Value = serde_json::from_str(&job.activity_json).unwrap();
        assert_eq!(activity["type"], "Create");
        activity["object"].clone()
    }

    /// `activitypub.md` § Reply and quote: a reply to an ingested status pushes
    /// `inReplyTo` + a Mention + the author in `to`, and reaches the author's
    /// inbox beside the follower fan-out.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reply_to_an_ingested_status_threads_mentions_and_reaches_its_author() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/carol",
            "accepted",
            None,
            true,
        )
        .await;
        seed_ingested_status(&state, &actor_hex).await;

        create_push_inner(
            &state,
            author,
            [0x21u8; 32],
            &referencing_post_body(INGESTED, reply_to),
        )
        .await
        .expect("push");

        let jobs = pending_jobs(&state).await;
        let mut inboxes: Vec<_> = jobs.iter().map(|j| j.target_inbox.as_str()).collect();
        inboxes.sort();
        assert_eq!(
            inboxes,
            vec![BOB_INBOX, "https://r1.example/users/carol/inbox"]
        );
        let note = note_of(&jobs[0]);
        assert_eq!(note["inReplyTo"], BOB_STATUS);
        assert!(
            note["to"].as_array().unwrap().iter().any(|a| a == BOB),
            "{note}"
        );
        assert_eq!(
            note["tag"],
            serde_json::json!([{"type": "Mention", "href": BOB, "name": "@bob@masto.example"}])
        );
    }

    /// A quote carries every spelling, the FEP-e232 tag and the `RE:` line,
    /// and reaches the quoted author too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quote_of_an_ingested_status_carries_every_spelling_and_reaches_its_author() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_ingested_status(&state, &actor_hex).await;

        create_push_inner(
            &state,
            author,
            [0x22u8; 32],
            &referencing_post_body(INGESTED, quote_of),
        )
        .await
        .expect("push");

        let jobs = pending_jobs(&state).await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].target_inbox, BOB_INBOX);
        let note = note_of(&jobs[0]);
        for key in ["quote", "quoteUri", "_misskey_quote"] {
            assert_eq!(note[key], BOB_STATUS, "{key}");
        }
        assert_eq!(note["tag"][0]["type"], "Link");
        let re_line = format!("RE: <a href=\"{BOB_STATUS}\"");
        assert!(
            note["content"].as_str().unwrap().contains(&re_line),
            "{note}"
        );
        assert!(note.get("inReplyTo").is_none());
    }

    /// A reference with no live map row (a native, bluesky or nostr target)
    /// pushes today's plain note to the followers only.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unmapped_reference_pushes_the_plain_note() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/carol",
            "accepted",
            None,
            true,
        )
        .await;

        create_push_inner(
            &state,
            author,
            [0x23u8; 32],
            &referencing_post_body(INGESTED, reply_to),
        )
        .await
        .expect("push");

        let jobs = pending_jobs(&state).await;
        assert_eq!(jobs.len(), 1);
        let note = note_of(&jobs[0]);
        for key in ["inReplyTo", "tag", "quote", "quoteUri", "_misskey_quote"] {
            assert!(note.get(key).is_none(), "{key}: {note}");
        }
        assert_eq!(note["content"], "<p>answering</p>");
    }

    /// A reply to one of our own pushed notes resolves through the same table
    /// (the self-thread): `inReplyTo` names our note, the Mention names us,
    /// and nothing is delivered to ourselves.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reply_to_our_own_pushed_note_threads_without_self_delivery() {
        let state = state().await;
        let author = [7u8; 32];
        seed_account(&state, author, "public").await;
        let first = [0x24u8; 32];
        create_push_inner(&state, author, first, &test_post_body())
            .await
            .expect("push");
        let first_url = {
            let conn = state.db.conn().await;
            db_helpers::get_ap_url_for_post(&conn, &hex::encode(first))
                .unwrap()
                .expect("mapped")
        };

        create_push_inner(
            &state,
            author,
            [0x25u8; 32],
            &referencing_post_body(first, reply_to),
        )
        .await
        .expect("push");
        assert!(
            pending_jobs(&state).await.is_empty(),
            "no followers, no self-delivery"
        );

        let conn = state.db.conn().await;
        let refs = resolve_ap_references(&conn, &referencing_post(first, reply_to)).unwrap();
        let parent = refs.in_reply_to.expect("our own note resolves");
        assert_eq!(parent.ap_url, first_url);
        assert_eq!(parent.author_uri, "https://localhost/ap/users/alice");
        assert_eq!(parent.author_handle, "@alice@localhost");
        assert!(refs.author_inboxes.is_empty());
    }

    /// The push enqueues one job per deduplicated accepted-follower inbox
    /// (shared inbox preferred), writes the local `ap_post_map` row, and the
    /// activity is a `Create` with the account's addressing.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_enqueues_to_accepted_followers_and_maps() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        // Two accepted followers on the same server → one shared inbox.
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            Some("https://r1.example/inbox"),
            true,
        )
        .await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/carol",
            "accepted",
            Some("https://r1.example/inbox"),
            true,
        )
        .await;
        // A pending follower and an uncached one contribute nothing.
        seed_follower(
            &state,
            &actor_hex,
            "https://r2.example/users/dan",
            "pending",
            None,
            true,
        )
        .await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r3.example/users/eve",
            "accepted",
            None,
            false,
        )
        .await;

        let post_id = [1u8; 32];
        create_push_inner(&state, author, post_id, &test_post_body())
            .await
            .expect("push");

        let jobs = pending_jobs(&state).await;
        assert_eq!(jobs.len(), 1, "deduped shared inbox → exactly one job");
        assert_eq!(jobs[0].target_inbox, "https://r1.example/inbox");
        let activity: serde_json::Value = serde_json::from_str(&jobs[0].activity_json).unwrap();
        assert_eq!(activity["type"], "Create");
        assert_eq!(activity["actor"], "https://localhost/ap/users/alice");
        let note = &activity["object"];
        assert!(
            note["content"]
                .as_str()
                .unwrap()
                .contains("hello fediverse")
        );
        // Public visibility: Public in to, followers in cc.
        assert_eq!(
            activity["to"][0],
            "https://www.w3.org/ns/activitystreams#Public"
        );

        // The local map row exists and resolves both ways.
        let conn = state.db.conn().await;
        let post_id_hex = hex::encode(post_id);
        let url = db_helpers::get_ap_url_for_post(&conn, &post_id_hex)
            .unwrap()
            .expect("map row");
        assert_eq!(
            url,
            format!("https://localhost/ap/users/alice/notes/{post_id_hex}")
        );
        assert_eq!(
            db_helpers::get_post_id_for_ap_url(&conn, &url).unwrap(),
            Some(post_id_hex)
        );
    }

    /// A gated (monetized/paywalled) post is never world-broadcast to the
    /// fediverse — the same stance nostr's `public_post_from_payload` and the
    /// trending plane already enforce for the other `post/*` consumers.
    ///
    /// A gated post's `body` decodes fine (it is the public preview), so
    /// nothing upstream stops it: the exclusion has to be explicit here.
    /// Pushing it would republish a paywalled post's teaser as an ordinary
    /// free Note, stripped of the paywall context the web render carries.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_skips_gated_posts() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            Some("https://r1.example/inbox"),
            true,
        )
        .await;

        let post_id = [2u8; 32];
        create_push_inner(&state, author, post_id, &gated_post_body())
            .await
            .expect("push returns Ok — a gated post is skipped, not an error");

        assert!(
            pending_jobs(&state).await.is_empty(),
            "a gated post enqueues no delivery, even to an accepted follower"
        );

        // No map row either: the note URL must not resolve, or the pull outbox
        // and inbound interactions would still surface the gated post.
        let conn = state.db.conn().await;
        assert_eq!(
            db_helpers::get_ap_url_for_post(&conn, &hex::encode(post_id)).unwrap(),
            None,
            "a gated post gets no ap_post_map row"
        );
    }

    /// Ruling 1 (`archive-import.md` § Compatibility → *Slice-3 rulings*) on
    /// the PUSH leg: an archive-imported public post is served on Fauna and
    /// never re-broadcast. The pull outbox applies `PUBLIC_POST_SERVABLE`;
    /// this leg fires at create time on the decoded post, before any row the
    /// SQL could filter exists, so it must apply the predicate's create-time
    /// twin itself — without it every imported post would be delivered to
    /// every accepted follower's inbox, the backdated flood the ruling exists
    /// to prevent. Same shape as the gated skip: `Ok`, no job, no map row.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_skips_archive_origin_posts() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            Some("https://r1.example/inbox"),
            true,
        )
        .await;

        let post_id = [3u8; 32];
        create_push_inner(&state, author, post_id, &archive_origin_post_body())
            .await
            .expect("push returns Ok — an imported post is skipped, not an error");

        assert!(
            pending_jobs(&state).await.is_empty(),
            "an archive-imported post enqueues no delivery, even to an accepted follower"
        );
        let conn = state.db.conn().await;
        assert_eq!(
            db_helpers::get_ap_url_for_post(&conn, &hex::encode(post_id)).unwrap(),
            None,
            "an archive-imported post gets no ap_post_map row"
        );
        drop(conn);

        // The control: the same author's native post beside it still pushes,
        // so the skip above discriminates on the origin, not on the fixture.
        create_push_inner(&state, author, [4u8; 32], &test_post_body())
            .await
            .expect("push");
        assert_eq!(pending_jobs(&state).await.len(), 1);
    }

    /// No AP account (or a disabled one) → clean no-op: no jobs, no map row.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_noop_without_enabled_account() {
        let state = state().await;
        let author = [7u8; 32];

        // No account at all.
        create_push_inner(&state, author, [1u8; 32], &test_post_body())
            .await
            .expect("no-op");
        assert!(pending_jobs(&state).await.is_empty());

        // Disabled account.
        let actor_hex = seed_account(&state, author, "public").await;
        {
            let conn = state.db.conn().await;
            db_helpers::update_settings(
                &conn,
                &actor_hex,
                &db_helpers::ApSettings {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        create_push_inner(&state, author, [1u8; 32], &test_post_body())
            .await
            .expect("no-op");
        assert!(pending_jobs(&state).await.is_empty());
        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_ap_url_for_post(&conn, &hex::encode([1u8; 32]))
                .unwrap()
                .is_none()
        );
    }

    /// The push nudges the delivery worker instead of leaving the activity for
    /// the next 30s poll — reaching followers is the latency-sensitive path.
    /// (`notify_one` stores a permit when no one is waiting, so `notified()`
    /// completing immediately IS the assertion that the nudge fired.)
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_nudges_the_delivery_worker() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            None,
            true,
        )
        .await;
        let nudge = state.activitypub.delivery_nudge.clone();

        create_push_inner(&state, author, [5u8; 32], &test_post_body())
            .await
            .expect("push");

        tokio::time::timeout(std::time::Duration::from_millis(100), nudge.notified())
            .await
            .expect("the push must nudge the worker, not wait for its poll tick");
    }

    /// Nothing enqueued (no followers) → nothing to nudge about: the worker
    /// keeps sleeping rather than being woken to drain an empty queue.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_without_followers_does_not_nudge() {
        let state = state().await;
        let author = [7u8; 32];
        seed_account(&state, author, "public").await;
        let nudge = state.activitypub.delivery_nudge.clone();

        create_push_inner(&state, author, [6u8; 32], &test_post_body())
            .await
            .expect("push");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), nudge.notified())
                .await
                .is_err(),
            "a push with no delivery jobs must not wake the worker"
        );
    }

    /// followers_only addressing: followers collection in `to`, no Public
    /// anywhere (the outbound mirror of the inbound audience gate).
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_respects_followers_only_visibility() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "followers_only").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            None,
            true,
        )
        .await;

        create_push_inner(&state, author, [2u8; 32], &test_post_body())
            .await
            .expect("push");

        let jobs = pending_jobs(&state).await;
        assert_eq!(jobs.len(), 1);
        // No shared inbox cached → per-actor inbox fallback.
        assert_eq!(jobs[0].target_inbox, "https://r1.example/users/bob/inbox");
        let activity: serde_json::Value = serde_json::from_str(&jobs[0].activity_json).unwrap();
        assert_eq!(
            activity["to"][0],
            "https://localhost/ap/users/alice/followers"
        );
        assert!(!jobs[0].activity_json.contains("activitystreams#Public"));
    }

    /// Zero followers: no delivery jobs, but the map row still lands (the
    /// pull outbox serves the same URL; inbound interactions must resolve).
    #[tokio::test(flavor = "multi_thread")]
    async fn create_push_without_followers_still_maps() {
        let state = state().await;
        let author = [7u8; 32];
        seed_account(&state, author, "public").await;

        create_push_inner(&state, author, [3u8; 32], &test_post_body())
            .await
            .expect("push");

        assert!(pending_jobs(&state).await.is_empty());
        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_ap_url_for_post(&conn, &hex::encode([3u8; 32]))
                .unwrap()
                .is_some()
        );
    }

    /// The pushed-witness rule: no `Create` was pushed (no map row) → the
    /// Delete leg is a clean no-op.
    #[tokio::test(flavor = "multi_thread")]
    async fn delete_push_requires_pushed_witness() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            None,
            true,
        )
        .await;

        delete_push_inner(&state, author, [9u8; 32])
            .await
            .expect("no-op");
        assert!(pending_jobs(&state).await.is_empty());
    }

    /// Seed the state a delete torn after its steps 1–2 leaves: an AP account
    /// with an accepted follower, the pushed `Create`'s `ap_post_map` row, and
    /// the step-1 `tombstone/post` witness naming the author — but no segment
    /// and no `content` row, so `delete_post_core` answers `AlreadyGone`.
    async fn seed_torn_delete(state: &Arc<AppState>, author: [u8; 32], post_id: [u8; 32]) {
        let actor_hex = seed_account(state, author, "public").await;
        seed_follower(
            state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            None,
            true,
        )
        .await;
        create_push_inner(state, author, post_id, &test_post_body())
            .await
            .expect("create push");
        let tombstone = tombstone_of(author, post_id);
        let bytes = fauna_core::encoding::canonical_encode(&tombstone).unwrap();
        state
            .db
            .delete_post_projection_with_witness(
                &post_id,
                Some((&author, bytes.as_slice(), tombstone.created_at.0 as i64)),
            )
            .await
            .expect("witness");
    }

    fn tombstone_of(author: [u8; 32], post_id: [u8; 32]) -> fauna_core::data::Tombstone {
        fauna_core::data::Tombstone {
            author: fauna_core::identity::ActorId(author),
            post_id: fauna_cbor::Cid::from_digest_dag_cbor(post_id),
            created_at: fauna_core::data::Timestamp(1_710_892_900_000_000),
        }
    }

    async fn delete_jobs(state: &Arc<AppState>) -> usize {
        pending_jobs(state)
            .await
            .iter()
            .filter(|j| j.activity_json.contains("\"Delete\""))
            .count()
    }

    /// Finding: a retried delete of a post that is already gone still chases
    /// the outward legs — here the ActivityPub `Delete` a first attempt never
    /// enqueued (a crash after steps 1–2). The settled `AlreadyGone` path used
    /// to return before reaching any of them.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_retried_delete_of_a_gone_post_still_pushes_the_delete() {
        let state = state().await;
        let author = [7u8; 32];
        let post_id = [5u8; 32];
        seed_torn_delete(&state, author, post_id).await;

        let outcome = crate::routes::delete_post_core(
            &state,
            author,
            &tombstone_of(author, post_id),
            post_id,
            crate::routes::RenderSite::Now,
        )
        .await
        .map_err(|_| ())
        .expect("delete retry");
        assert!(matches!(
            outcome,
            crate::routes::PostDeleteOutcome::AlreadyGone
        ));

        // The leg is fire-and-forget: wait for its enqueue.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while delete_jobs(&state).await == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the retried delete never enqueued the ActivityPub Delete"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(delete_jobs(&state).await, 1);
    }

    /// Create-then-delete: the Delete enqueues to the same followers, names
    /// the pushed note URL, and tombstones the map row; a retry (AlreadyGone
    /// semantics) still finds the tombstoned witness and enqueues again.
    #[tokio::test(flavor = "multi_thread")]
    async fn delete_push_enqueues_delete_and_tombstones_map() {
        let state = state().await;
        let author = [7u8; 32];
        let actor_hex = seed_account(&state, author, "public").await;
        seed_follower(
            &state,
            &actor_hex,
            "https://r1.example/users/bob",
            "accepted",
            Some("https://r1.example/inbox"),
            true,
        )
        .await;

        let post_id = [4u8; 32];
        let post_id_hex = hex::encode(post_id);
        create_push_inner(&state, author, post_id, &test_post_body())
            .await
            .expect("create push");

        delete_push_inner(&state, author, post_id)
            .await
            .expect("delete push");

        let jobs = pending_jobs(&state).await;
        assert_eq!(jobs.len(), 2, "the Create job + the Delete job");
        let delete_job = jobs
            .iter()
            .find(|j| j.activity_json.contains("\"Delete\""))
            .expect("a Delete job");
        let activity: serde_json::Value = serde_json::from_str(&delete_job.activity_json).unwrap();
        assert_eq!(activity["type"], "Delete");
        assert_eq!(
            activity["object"],
            format!("https://localhost/ap/users/alice/notes/{post_id_hex}")
        );

        // Map row tombstoned: no longer resolves for inbound interactions.
        {
            let conn = state.db.conn().await;
            assert!(
                db_helpers::get_ap_url_for_post(&conn, &post_id_hex)
                    .unwrap()
                    .is_none()
            );
        }

        // Retry idempotency: a second delete still finds the tombstoned
        // witness and enqueues another Delete (harmless; remote dedupes).
        delete_push_inner(&state, author, post_id)
            .await
            .expect("delete retry");
        let jobs = pending_jobs(&state).await;
        assert_eq!(
            jobs.iter()
                .filter(|j| j.activity_json.contains("\"Delete\""))
                .count(),
            2
        );
    }
}
