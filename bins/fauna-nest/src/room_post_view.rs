//! The home nest's **reception pass over room-restricted posts** — the third
//! purpose of a community room's readable position
//! (`docs/goal/behavior/community-rooms.md` § The three classes → *What the
//! home nest does with its read*, purpose 3), built to the owner's mechanics
//! (`docs/goal/ui/feed.md` § Encryption at rest → *Room-restricted — the
//! ruling*, ruling 7).
//!
//! A room-restricted post is its author's ordinary post: it rests in the
//! author's `__post` plane, fans out to followers, and never enters the room's
//! log. So this pass never *fetches* anything for a room — it runs only on a
//! post this nest is already **storing** (the author, or a follower, homed
//! here), in the act that stores it, and only when this nest is the addressed
//! room's home nest holding the tip's wrap. A home nest that stores no such
//! post derives nothing. That narrowness is the whole of the purpose.
//!
//! What it derives is what the message pass derives, under the same rules
//! (`conversations_handlers::index_room_message`): the typed text into the
//! room's per-room post class (`CacheDb::room_post_view_schema`, the sibling
//! of its message class) and a map naming the post, so
//! `fauna.conversations.room.search` can answer *where* — a post id — and
//! never *what*; and, after the index, the room's named labelers' verdicts
//! (`conversations_handlers::run_room_labelers`, the same set and the same
//! shared scorers as the message pass) onto the bus under the `room_post`
//! kind, keyed `(room, post)`. A post with no typed text — a picture alone —
//! gets no search row and is viewed all the same: the map row and the
//! verdicts, as an attachment-only message is. All of it is purged with the
//! room's other views when the members rotate this nest out, and with the
//! post when its author deletes it — and **withheld, not purged, while a
//! moderation flag stands on the post**: both serve paths below consult
//! `content_meta` through `db::rooms::ROOM_POST_VIEW_MODERATION`, so a legal
//! takedown yields no hit and no verdict for exactly as long as the flag
//! does, and an overturn re-serves the views with nothing re-derived
//! (`moderation.md` § Legal takedown → *The blob-serve door* → *What the
//! withhold binds on owner- and admin-scoped routes*, path 4). The store read
//! this pass makes is not gated: it runs in the act that stores the post, and
//! a post cannot carry a takedown at the instant it is created.
//!
//! **Where the verdicts are served is the part a post does differently.** A
//! message's ride `channel.fetch` beside the envelope; a post is read through
//! `fauna.posts.get`, the feed pages and the deep-link door by every follower
//! of its author, most of whom are not on the room's floor, and none of those
//! reads is floor-gated because the envelope is sealed. So a post's verdicts
//! have a read of their own, `fauna.posts.room_labels`
//! (`posts_handlers::posts_room_labels_handler`), gated by the same two
//! checks as the message read (`conversations_handlers::is_live_floor_member`)
//! — and the envelope reads carry none, on any door, to anyone.

use std::sync::Arc;

use fauna_core::data::{Post, PostBody};
use fauna_core::room_post::{RoomPostSeal, room_post_of};

use crate::routes::AppState;

/// Build the home nest's derived view of one stored room-restricted post.
///
/// `body` is the post's `EmbedAsBytes` wire exactly as stored — the caller has
/// already verified its signature at ingest, which is what makes `post.author`
/// a fact here rather than a claim.
///
/// Every failure is silent and view-less, for the message pass's reason: the
/// post is its author's act and is stored either way; a view the nest could
/// not build is the nest's own read failing, never a reason to refuse it.
pub(crate) async fn index_room_post(state: &Arc<AppState>, post_id: &[u8; 32], body: &[u8]) {
    let Some(post) = decode_stored_post(body) else {
        return;
    };
    let Some(gated) = &post.gated else {
        return;
    };
    // A community room only: an end-to-end room's post opens under its MLS
    // epoch secret, which no nest ever holds.
    let Some((room, RoomPostSeal::Community { generation })) = room_post_of(&gated.key_access)
    else {
        return;
    };
    // Keyed on the TIP, never on the generation the post names — the same
    // rule as a message: once the members rotate this nest out, a post
    // somebody seals under an older generation it still holds a wrap for
    // must not resurrect a view the revoke deleted. The tip is the end of
    // the parent chain (`db::rooms::order_generations_by_chain`), never the
    // latest stamp.
    let key = match crate::conversations_handlers::nest_room_tip_key(state, &room).await {
        Ok(Some((key, tip))) if tip == generation => key,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!("room post view: {e:?}");
            return;
        }
    };
    // The floor is the membership authority: a post by somebody who is not a
    // live member builds no view, so the derived corpus holds exactly what the
    // room's own members wrote — whoever else happened to hold the key.
    let authored_by_a_member = state
        .db
        .list_floor_roster(&room)
        .await
        .map(|roster| roster.iter().any(|m| m.principal_id == post.author.0))
        .unwrap_or(false);
    if !authored_by_a_member {
        return;
    }
    let seal_id = gated.seal_id;
    let Some(store) = state.backup_service.as_ref().map(|s| s.local_blob_store()) else {
        return;
    };
    // The sealed body rides beside the post (a client uploads it first); a
    // relayed post whose blob has not arrived yet simply gets no view.
    let Ok(Some(sealed)) = store.get(&gated.encrypted_ref).await else {
        return;
    };
    let base = zeroize::Zeroizing::new(fauna_core::group_content::room_post_base_key(&key));
    let per_post = zeroize::Zeroizing::new(fauna_core::subscription::crypto::derive_post_key(
        &base, &seal_id,
    ));
    let Ok(plain) = fauna_core::subscription::crypto::decrypt_content(&per_post, &sealed) else {
        tracing::warn!("room post view: a room post under the tip did not open");
        return;
    };
    let Ok(full) = fauna_core::encoding::canonical_decode::<PostBody>(&plain) else {
        return;
    };
    // The room's POST class — a sibling of its message class, so a caller
    // asking only for messages keeps the page it always got
    // (`CacheDb::room_post_view_schema`).
    let schema = crate::db::CacheDb::room_post_view_schema(&room);
    let doc_id = hex::encode(post_id);
    // Purpose 1, search — only what the member typed, by the one home for
    // which variants carry typed text (`PostBody::text`: a caption, a media
    // post's alt text, a structured post's content; a video's is empty) and
    // never anything derived from attachment bytes, which seal alongside
    // under the same key (`community-rooms.md` § *What the read covers*). A
    // body with none is not indexed — a corpus row holding no token is a hit
    // nobody can produce — and is viewed all the same: the map row and the
    // labels below do not wait on the index, exactly as `index_room_message`
    // labels an attachment-only message. A failed index is likewise this
    // one view failing, never the others'.
    let text = full.text();
    if !text.is_empty()
        && let Err(e) = state
            .db
            .index_document(
                &schema,
                &doc_id,
                "",
                &text,
                "",
                "",
                crate::db::now_epoch_micros(),
            )
            .await
    {
        tracing::warn!("room post view: indexing failed: {e}");
    }
    // The `(room, post)` map row: the record that this nest derived a view of
    // this post for this room — ANY view — and the precondition of every
    // derived view of a post: the search door resolves a hit to a post id
    // through it (`room_post_ids_for_docs`), the floor-gated verdict read
    // finds the rooms whose floors gate a post's verdicts through it
    // (`rooms_indexing_posts`), and the post's delete purges both through it
    // (`purge_room_post_views_for_post`). So it is written for every post
    // that reaches here, indexed or not, keyed under the document key an index writes — a pure function
    // of `(schema, doc id)`, so the key names the FTS row whether or not one
    // exists, and removing a key with no row removes nothing. The invariant
    // the consumers need is "every FTS post row and every `room_post` verdict
    // has a map row", which the order index → map → labels keeps. The message
    // map's "only ever a subset of what is searchable" is that rail's own —
    // its verdicts key on `(room, seq)` and never route through its map — and
    // was never what a post consumer relied on: a hit can only come from an
    // FTS row, so a map row with none is a row the search door never asks
    // about.
    if let Err(e) = state
        .db
        .record_room_post_view(
            &room,
            post_id,
            &crate::db::content_id_for_document(&schema, &doc_id),
            &generation,
        )
        .await
    {
        tracing::warn!("room post view: recording the post map failed: {e}");
        // No map row, no verdict: the post's delete finds its verdicts
        // through the map, and a verdict the delete could not find would
        // outlive the post.
        return;
    }
    // Purpose 3's label half: the same labelers the room names for its
    // messages, over the body the member actually sealed — the one home for
    // the mapping (`LabelerPostInput::from_post`) reads a gated post's public
    // preview, so the opened body is swapped in first. Keyed `(room, post)`
    // under the `room_post` kind, served only by `fauna.posts.room_labels`.
    // The post's own media, for a labeler that declared it reads bytes: sealed
    // beside the body under the SAME per-post key (`../ui/feed.md`
    // § Encryption at rest → *Room-restricted — the ruling*, ruling 4), so the
    // pass opens them with the key it already holds and the facet's ceilings
    // are the message facet's — one loop
    // (`fauna_labeler::build_attachment_facet`), two key models. Which
    // variants carry items is `PostBody::media_items`'s to say (a `Video`
    // body names none and reaches the labelers with its metadata alone, as
    // everywhere).
    let media = full.media_items().to_vec();
    let input = fauna_core::scoring::LabelerPostInput::from_post(&Post { body: full, ..post });
    let Some((labels, scores)) = crate::conversations_handlers::run_room_labelers(
        state,
        &room,
        input,
        crate::conversations_handlers::RoomFacetSource::Post {
            media: &media,
            per_post_key: &per_post,
        },
    )
    .await
    else {
        return;
    };
    if let Err(e) = state
        .db
        .record_room_post_bus(
            &room,
            post_id,
            &labels,
            &scores,
            &state.nest_identity.public_key_bytes(),
        )
        .await
    {
        tracing::warn!("room post view: recording the verdicts failed: {e}");
    }
}

/// The stored post wire decoded to its `Post`, or `None`. No verification:
/// every caller runs after the ingest that verified the same bytes.
fn decode_stored_post(body: &[u8]) -> Option<Post> {
    let wire: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(body).ok()?;
    let (post_bytes, _) = wire.into_signed().ok()?;
    fauna_core::encoding::decode_signed_bytes(&post_bytes).ok()
}
