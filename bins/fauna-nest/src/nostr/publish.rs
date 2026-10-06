//! The create-side nostr arm: a new local post, derived into a nostr event and
//! pushed to the author's relays when it references a nostr-origin note or the
//! author turned on `auto_publish` (`docs/goal/ui/nostr.md` § Replying to and
//! quoting a nostr note → *When the nest publishes*).
//!
//! Rides `routes::spawn_post_bridge_fanout` beside the AP push and the bluesky
//! write-through. Fire-and-forget + non-fatal: a publish hiccup never fails the
//! local create. The derived event is stored and mapped `outbound` exactly as
//! `store::materialize_account` does, so the materialization sweep dedupes it
//! and `fauna.posts.delete`'s kind-5 leg retracts it.

use std::sync::Arc;

use fauna_bridge_nostr::nip10;
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::translate::{ResolvedReference, fauna_post_to_nostr};
use fauna_bridge_nostr::types::{Event, Filter};
use fauna_core::data::{Post, Reference};
use rusqlite::Connection;

use crate::nostr::relays::resolve_relay_urls;
use crate::nostr::{db, key_crypto, relay_endpoint, store, sync_worker};
use crate::routes::AppState;

/// Spawn the create-side publish for a freshly stored local post. A clean
/// no-op for an author with no deposited nsec, or whose post neither
/// references a nostr note nor falls under `auto_publish`.
pub fn spawn_create_publish(
    state: Arc<AppState>,
    author: [u8; 32],
    post_id: [u8; 32],
    body: Vec<u8>,
) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        if let Err(e) = create_publish_inner(&state, author, post_id, &body).await {
            tracing::warn!("nostr create-publish: {e:#}");
        }
    });
}

/// The publish itself, awaited — [`spawn_create_publish`]'s body, public for
/// the real-client relay harness (`tests/nostr_relay_interop.rs`).
pub async fn create_publish_inner(
    state: &Arc<AppState>,
    author: [u8; 32],
    post_id: [u8; 32],
    body: &[u8],
) -> anyhow::Result<()> {
    let actor_hex = hex::encode(author);
    let post_id_hex = hex::encode(post_id);
    // The off-box servability rule every produce sink applies: a gated or
    // archive-imported post never leaves the box.
    let Some(post) = crate::db::posts::decode_stored_post(body) else {
        return Ok(());
    };
    if !crate::db::public_servability::publishable_off_box_at_create(&post) {
        return Ok(());
    }

    let conn = state.db.conn().await;
    let Some(account) = db::get_account(&conn, &actor_hex)? else {
        return Ok(());
    };
    // Only a deposited nsec signs on the nest (`remote`/`nip07` cannot).
    let Some(sealed) = account.encrypted_privkey.clone() else {
        return Ok(());
    };
    // Already derived (the materialization sweep got here first): one post,
    // one derived event.
    if db::get_event_by_fauna_id(&conn, &post_id_hex)?.is_some() {
        return Ok(());
    }
    let resolved = resolve_nostr_references(&conn, &post)?;
    drop(conn);

    // The reference is the intent — a reply or quote of a nostr note is
    // delivered whatever `auto_publish` says; a reply only with
    // `publish_replies` on.
    if resolved.is_empty() && !account.auto_publish {
        return Ok(());
    }
    if post.is_reply() && !account.publish_replies {
        return Ok(());
    }
    // An explicitly empty relay list publishes nowhere — never the defaults.
    let relay_urls = resolve_relay_urls(account.relay_list.as_deref());
    if relay_urls.is_empty() {
        return Ok(());
    }

    let Some(nest_key) = state.nest_signing_key.as_ref() else {
        anyhow::bail!("nest signing key not configured");
    };
    let secret = key_crypto::decrypt_nostr_privkey(&nest_key.to_bytes(), &sealed)?;
    let keypair = Keypair::from_secret_bytes(secret)?;

    let unsigned = fauna_post_to_nostr(&post, &keypair.public_key_bytes(), &resolved)?;
    let event = keypair.sign_event(unsigned);
    let conn = state.db.conn().await;
    store::store_event(&conn, &event, true)?;
    db::insert_event_map(&conn, &post_id_hex, &event.id, &event.pubkey, "outbound")?;
    drop(conn);

    // Live subscribers of this nest's relay, then the author's own relays.
    let _ = state.nostr.relay_tx.send(relay_endpoint::NostrRelayEvent {
        event_json: serde_json::to_string(&event)?,
        author_pubkey: event.pubkey.clone(),
    });
    tracing::info!(post = %post_id_hex, event = %event.id, relays = relay_urls.len(), "nostr: publishing derived post");
    state
        .nostr
        .sync_tx
        .send(sync_worker::OutboundEvent { event, relay_urls })
        .await
        .map_err(|e| anyhow::anyhow!("outbound enqueue: {e}"))?;
    Ok(())
}

/// Resolve the post's first `Reply` and first `Quote` through
/// `nostr_event_map` into the ids the translator writes (`nostr.md` § Replying
/// to and quoting a nostr note → *Reference resolution*). A reply's thread
/// root and participants come from the parent event in the relay store —
/// every mapped event rests there beside its map row. A target with no map
/// row (a native, bluesky or fediverse post) resolves to nothing.
pub(crate) fn resolve_nostr_references(
    conn: &Connection,
    post: &Post,
) -> anyhow::Result<Vec<ResolvedReference>> {
    let mut out = Vec::new();
    let (mut replied, mut quoted) = (false, false);
    for reference in &post.references {
        let (is_reply, post_id) = match reference {
            Reference::Reply { post_id } if !replied => (true, post_id),
            Reference::Quote { post_id } if !quoted => (false, post_id),
            _ => continue,
        };
        let Some(target) = db::get_event_by_fauna_id(conn, &hex::encode(post_id.digest()))? else {
            continue;
        };
        if is_reply {
            replied = true;
            let parent = stored_event(conn, &target.nostr_event_id)?;
            let thread = parent
                .as_ref()
                .map(|p| nip10::parse_thread_tags(&p.tags))
                .unwrap_or_default();
            out.push(ResolvedReference::Reply {
                event_id: target.nostr_event_id,
                pubkey: target.nostr_pubkey,
                root_id: thread.root,
                parent_p_tags: thread.pubkeys,
            });
        } else {
            quoted = true;
            out.push(ResolvedReference::Quote {
                event_id: target.nostr_event_id,
                pubkey: target.nostr_pubkey,
            });
        }
    }
    Ok(out)
}

fn stored_event(conn: &Connection, id: &str) -> anyhow::Result<Option<Event>> {
    let filter = Filter {
        ids: Some(vec![id.to_string()]),
        ..Default::default()
    };
    Ok(store::query_events(conn, std::slice::from_ref(&filter), 1)?
        .into_iter()
        .next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::state::NostrState;
    use fauna_bridge_nostr::types::{Tag, UnsignedEvent};
    use fauna_core::data::{ContentHash, PostBody, Timestamp};
    use fauna_core::identity::ActorId;
    use tokio::sync::mpsc;

    const SEED: [u8; 32] = [42u8; 32];
    const AUTHOR: [u8; 32] = [0x41u8; 32];
    const PARENT_LOCAL: [u8; 32] = [0x77u8; 32];

    struct Fixture {
        state: Arc<AppState>,
        rx: mpsc::Receiver<sync_worker::OutboundEvent>,
        parent: Event,
        root_id: String,
        other_p: String,
    }

    /// An author with a deposited nsec, `relays` and the two toggles, plus a
    /// swept parent note (mid-thread: it carries its own root and a `p`) mapped
    /// at `PARENT_LOCAL` and resting in the relay store.
    async fn fixture(relays: Option<&str>, auto_publish: bool, publish_replies: bool) -> Fixture {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let (tx, rx) = mpsc::channel(8);
        let mut state = AppState {
            nostr: NostrState {
                sync_tx: tx,
                ..NostrState::default()
            },
            ..AppState::for_test(db)
        };
        state.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&SEED));
        let state = Arc::new(state);

        let kp = Keypair::generate();
        let sealed = key_crypto::encrypt_nostr_privkey(&SEED, &kp.secret_bytes()).unwrap();
        let parent_author = Keypair::generate();
        let (root_id, other_p) = ("1".repeat(64), "2".repeat(64));
        let parent = parent_author.sign_event(UnsignedEvent {
            pubkey: parent_author.public_key_bytes(),
            created_at: 1_000,
            kind: 1,
            tags: vec![
                Tag::new(vec!["e".into(), root_id.clone(), "".into(), "root".into()]),
                Tag::new(vec!["p".into(), other_p.clone()]),
            ],
            content: "the parent".into(),
        });
        let conn = state.db.conn().await;
        db::link_account(
            &conn,
            &hex::encode(AUTHOR),
            &kp.public_key_hex(),
            "generate",
            Some(&sealed),
            None,
            relays,
        )
        .unwrap();
        db::update_settings(
            &conn,
            &hex::encode(AUTHOR),
            &db::NostrSettings {
                auto_publish: Some(auto_publish),
                publish_replies: Some(publish_replies),
                ..Default::default()
            },
        )
        .unwrap();
        store::store_event(&conn, &parent, false).unwrap();
        db::insert_event_map(
            &conn,
            &hex::encode(PARENT_LOCAL),
            &parent.id,
            &parent.pubkey,
            "inbound",
        )
        .unwrap();
        drop(conn);
        Fixture {
            state,
            rx,
            parent,
            root_id,
            other_p,
        }
    }

    fn post_body(references: Vec<Reference>) -> Vec<u8> {
        let post = Post {
            author: ActorId(AUTHOR),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: "my words".into(),
                facets: vec![],
            },
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).unwrap()
    }

    fn parent_ref() -> ContentHash {
        ContentHash::from_digest_raw(PARENT_LOCAL)
    }

    async fn publish(f: &Fixture, id: u8, references: Vec<Reference>) {
        create_publish_inner(&f.state, AUTHOR, [id; 32], &post_body(references))
            .await
            .expect("publish");
    }

    fn rows(event: &Event) -> Vec<Vec<&str>> {
        event
            .tags
            .iter()
            .map(|t| t.0.iter().map(String::as_str).collect())
            .collect()
    }

    /// A reply to a swept note: sent, with the parent's root as `root`, the
    /// parent as `reply`, the parent author + the parent's `p` set — and
    /// stored + mapped `outbound` so the sweep dedupes it.
    #[tokio::test]
    async fn a_reply_to_a_swept_note_is_signed_stored_mapped_and_sent_with_marked_tags() {
        let mut f = fixture(None, false, true).await;
        publish(
            &f,
            1,
            vec![Reference::Reply {
                post_id: parent_ref(),
            }],
        )
        .await;

        let sent =
            f.rx.try_recv()
                .expect("a reply to a nostr note is published");
        assert!(!sent.relay_urls.is_empty());
        assert_eq!(
            rows(&sent.event),
            vec![
                vec!["e", f.root_id.as_str(), "", "root"],
                vec!["e", f.parent.id.as_str(), "", "reply"],
                vec!["p", f.parent.pubkey.as_str()],
                vec!["p", f.other_p.as_str()],
            ]
        );
        let conn = f.state.db.conn().await;
        let mapped = db::get_event_by_fauna_id(&conn, &hex::encode([1u8; 32]))
            .unwrap()
            .expect("mapped");
        assert_eq!(mapped.nostr_event_id, sent.event.id);
        assert_eq!(mapped.direction, "outbound");
        assert!(stored_event(&conn, &sent.event.id).unwrap().is_some());
    }

    /// A quote: `q` + `p` and the `nostr:nevent1…` reference in the content.
    #[tokio::test]
    async fn a_quote_of_a_swept_note_is_sent_with_q_p_and_a_nostr_reference() {
        let mut f = fixture(None, false, false).await;
        publish(
            &f,
            2,
            vec![Reference::Quote {
                post_id: parent_ref(),
            }],
        )
        .await;
        let sent = f.rx.try_recv().expect("a quote ignores publish_replies");
        assert_eq!(
            rows(&sent.event),
            vec![
                vec!["q", f.parent.id.as_str()],
                vec!["p", f.parent.pubkey.as_str()]
            ]
        );
        assert!(
            sent.event.content.starts_with("my words\n\nnostr:nevent1"),
            "{}",
            sent.event.content
        );
    }

    /// The two toggles and the relay list given their meaning.
    #[tokio::test]
    async fn the_toggles_and_the_relay_list_decide_what_is_sent() {
        // `publish_replies` off → a reply is not sent.
        let mut f = fixture(None, true, false).await;
        publish(
            &f,
            3,
            vec![Reference::Reply {
                post_id: parent_ref(),
            }],
        )
        .await;
        assert!(f.rx.try_recv().is_err(), "publish_replies off");

        // `auto_publish` on → a plain post is sent.
        let mut f = fixture(None, true, true).await;
        publish(&f, 4, vec![]).await;
        let sent = f.rx.try_recv().expect("auto_publish sends a plain post");
        assert!(sent.event.tags.is_empty());

        // `auto_publish` off and no nostr reference → not sent (a reference to
        // a post that is not a nostr note is no reference here).
        let mut f = fixture(None, false, true).await;
        publish(&f, 5, vec![]).await;
        let elsewhere = ContentHash::from_digest_raw([9u8; 32]);
        publish(&f, 6, vec![Reference::Quote { post_id: elsewhere }]).await;
        assert!(f.rx.try_recv().is_err(), "nothing to publish");

        // An explicitly empty relay list → published nowhere, never the defaults.
        let mut f = fixture(Some("[]"), true, true).await;
        publish(
            &f,
            7,
            vec![Reference::Reply {
                post_id: parent_ref(),
            }],
        )
        .await;
        assert!(f.rx.try_recv().is_err(), "empty relay list");
    }
}
