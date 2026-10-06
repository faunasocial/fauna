//! The one predicate deciding whether a stored post may be served to an
//! **anonymous, off-box audience** — the ActivityPub actor routes, the ATProto
//! public-projection stream, and the Nostr materializer.
//!
//! ⚠ **This list is the whole class, not the consumers that happened to exist.**
//! Nostr was added 2026-08-04, four months late, purely because the original
//! sweep enumerated the two federation bridges and nobody asked whether the
//! relay was the same kind of surface — it is, and until then a legally
//! compelled takedown was honoured on the federated surfaces and ignored on the
//! public one. **Any new path that serves post content to an unauthenticated,
//! off-box audience belongs here on the day it is written.**
//!
//! Why this is a single named constant rather than a filter each call site
//! spells for itself: the three off-box serve paths are *supposed* to share
//! one servability rule, and until 2026-07-29 they shared a copy of an
//! incomplete one. `atproto_projection`'s own module doc names
//! `activitypub::db_helpers::public_outbox_page` as the filter it follows, and
//! both spelled only `gated_tier IS NULL` — so a post withheld under
//! [`moderation.md`] § Legal takedown was still published to Bluesky and to
//! the fediverse, while `db/feeds.rs` (the site that design cites as *the*
//! withholding point) had honoured all three flags since 2026-07-05. One
//! owner, referenced by every consumer, is the only shape in which those
//! cannot drift apart again.
//!
//! **Two independent failure modes this closes**:
//!
//! 1. The three moderation flags were never consulted, so a legally compelled,
//!    non-discretionary takedown was silently defeated on exactly the surfaces
//!    that publish to a third-party public network.
//! 2. The `LEFT JOIN` made a **missing** `content_meta` row read as *public and
//!    unflagged* — `cm.gated_tier IS NULL` is vacuously true for an unmatched
//!    right side. That state is reachable: every post-write site swallows the
//!    `write_post_index` error that carries `gated_tier` into the index
//!    (`db/moderation.rs:304-307` records the same reachability), so a **gated**
//!    post whose index write failed was projected as public. Hence
//!    `cm.content_id IS NOT NULL` — absence must never read as permission.
//!
//! **A third-party principal reading a public timeline is the same class**
//! (`authorization-server.md` § Scope grammar → *The Fauna family, exactly*):
//! authenticated, but a party off the box, so its two feed reads hold to
//! [`PUBLIC_POST_SERVABLE`] through `db::feeds::FeedAudience::OffBox`.
//!
//! **The moderation half has its own name, [`MODERATION_SERVABLE`], because it
//! binds more surfaces than the off-box ones.** The feed reads, the federation
//! feed query, the Search corpus and the web-site render each serve post
//! content to someone who is not its author, and each once spelled its own
//! copy of the three flags — the federation query and Search had dropped two of
//! them, the render had never had any (all closed 2026-09-10). What
//! `PUBLIC_POST_SERVABLE` adds on top — the `content_meta`-exists, `gated` and
//! archive-platform arms — is specific to the anonymous off-box audience, and
//! is exactly what the on-box surfaces must NOT inherit: Search indexes a gated
//! post's public preview on purpose, the render serves it behind its paywall,
//! and an archive import is served ON Fauna.
//!
//! [`moderation.md`]: ../../../../docs/goal/behavior/moderation.md

/// SQL fragment: true iff no **moderation flag** withholds the joined post —
/// it is not taken down under a legal obligation, not quarantined, and not
/// suppressed.
///
/// The gate every surface serving post content to a **non-author** audience
/// shares, whatever else it adds: the feed reads (`db/feeds.rs`), the
/// federation feed query (`query_feed_for_authors`), the Search corpus
/// (`db/fts.rs`), the web-site render (`list_web_published_servable_capped`),
/// and — composed — [`PUBLIC_POST_SERVABLE`]. A surface whose audience is the
/// post's **author** binds only the legal-takedown arm (quarantine is
/// author-visible by design; `moderation.md` § Legal takedown → *Posts*), so it
/// spells `cm.legal_takedown_ref IS NULL` on its own rather than reaching for
/// this.
///
/// **Contract for callers** — the query binds `content_meta` as `cm`. A `LEFT
/// JOIN` is fine here, unlike for the off-box predicate: an unmatched row carries
/// no flag to honour, and a takedown cannot land on one (its write asserts it
/// matched exactly one row — `post_legal_takedown_txn`).
pub const MODERATION_SERVABLE: &str = "cm.legal_takedown_ref IS NULL \
     AND COALESCE(cm.quarantined, 0) = 0 \
     AND COALESCE(cm.suppressed, 0) = 0";

/// SQL fragment: true iff the joined row is a post this nest may publish to an
/// unauthenticated, off-box audience.
///
/// **Contract for callers** — the query must `LEFT JOIN content_meta cm ON
/// cm.content_id = c.id` against `content c`. The join stays `LEFT` on purpose:
/// the ATProto stream also selects `tombstone/post` journal rows, which carry
/// no `content_meta` row by construction, so an `INNER` join would drop the
/// retractions. That is precisely why the post arm must assert
/// `cm.content_id IS NOT NULL` for *itself* instead of leaning on the join
/// shape to do it.
///
/// The **moderation flags** are [`MODERATION_SERVABLE`], composed rather than
/// restated, so the feed read's gate (`db/feeds.rs`, the already-ratified
/// withholding site) and this predicate cannot disagree about which flags
/// exist. The archive-platform
/// arm is the one deliberate asymmetry: an import is served ON Fauna (the feed
/// read keeps it) and never OFF it (`archive-import.md` § Compatibility →
/// *Slice-3 rulings*, ruling 1 — a backdated flood into other networks' home
/// timelines is the one irreversible outcome, and re-publication outward is
/// elsewhere a deliberate author act). The token list is built from
/// `fauna_core::source::ARCHIVE_PLATFORMS`, the one vocabulary, so a platform
/// added there is excluded here with no second edit. The source token itself
/// lives on `content.source` (`c.source`) — the column `extract_post_metadata`
/// / `put_post` / `put_post_with_source` write and `get_post_source` reads
/// back (`db/posts.rs`) — never on `content_meta`, which carries no `source`
/// column.
pub static PUBLIC_POST_SERVABLE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let archive = fauna_core::source::ARCHIVE_PLATFORMS
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "c.schema LIKE 'post/%'
             AND cm.content_id IS NOT NULL
             AND cm.gated_tier IS NULL
             AND {MODERATION_SERVABLE}
             AND COALESCE(c.source, '{native}') NOT IN ({archive})",
        native = fauna_core::source::NATIVE,
    )
});

/// The **create-time half** of [`PUBLIC_POST_SERVABLE`], for the two *push*
/// legs that publish a post the instant it lands — the ActivityPub
/// `Create`-push (`activitypub::push::create_push_inner`) and the Bluesky
/// write-through (`bluesky::write_through_create_inner`), both fanned out by
/// `routes::spawn_post_bridge_fanout` — which hold the decoded post, not a
/// `content`/`content_meta` row the SQL can filter.
///
/// Of the predicate's arms, exactly two are decidable from the post itself,
/// and this answers those: **`gated`** (a paywalled post's plaintext body is
/// its teaser; pushing it would republish the teaser as an ordinary free post
/// without the paywall context the web render carries) and the
/// **archive-platform `source`** (ruling 1 — an import is served on Fauna and
/// never re-broadcast; a backdated flood into other networks' home timelines
/// is the one irreversible outcome). The moderation arms
/// (`legal_takedown_ref`, `quarantined`, `suppressed`) are row state a post
/// cannot carry at the instant it is created, and the `content_meta`-exists
/// arm guards the pull side against a failed index write — neither has a
/// create-time meaning. Kept beside the SQL so the two halves cannot drift:
/// an arm added there that *has* a create-time meaning is added here in the
/// same change (`moderation.md` § Legal takedown, the off-box bullet).
///
/// Why this exists at all: the SQL predicate covers every *pull* surface (outbox, note
/// dereference, projection stream, materializer), and for four months the two
/// push legs each carried their own one-arm copy (`gated` only) — so ruling 1,
/// landed in the predicate alone, would have stopped an imported public post
/// from being *served* off-box while the create-time push still delivered it
/// to every fediverse follower's inbox and cross-posted it to Bluesky.
pub fn publishable_off_box_at_create(post: &fauna_core::data::Post) -> bool {
    post.gated.is_none()
        && !fauna_core::source::ARCHIVE_PLATFORMS.contains(&post.source_token().as_str())
}

#[cfg(test)]
mod tests {
    use super::{PUBLIC_POST_SERVABLE, publishable_off_box_at_create};

    fn post(gated: bool, platform: Option<&str>) -> fauna_core::data::Post {
        fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_710_892_800_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "hello".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: gated.then(|| fauna_core::subscription::types::GatedInfo {
                encrypted_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x33u8; 32]),
                key_access: fauna_core::subscription::types::KeyAccess::Broadcast {
                    key_blob_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x44u8; 32]),
                },
                tier: "premium".into(),
                tier_rank: 1,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: platform.map(|p| fauna_core::data::PostOrigin {
                platform: p.into(),
                url: None,
            }),
        }
    }

    /// The create-time twin answers the two arms the push legs can decide:
    /// a gated post and an archive-imported post are refused; an ordinary
    /// native post passes; and a platform this build does not know is not an
    /// archive platform (`Post::source_token` falls back to `fauna`), so it
    /// is published exactly as an older nest would.
    #[test]
    fn the_create_time_twin_refuses_gated_and_archive_origin_posts() {
        assert!(publishable_off_box_at_create(&post(false, None)));
        assert!(!publishable_off_box_at_create(&post(true, None)));
        for token in fauna_core::source::ARCHIVE_PLATFORMS {
            assert!(
                !publishable_off_box_at_create(&post(false, Some(token))),
                "{token} is never re-broadcast"
            );
        }
        assert!(
            !publishable_off_box_at_create(&post(false, Some(" Facebook "))),
            "the un-normalized spelling is the same token"
        );
        assert!(
            publishable_off_box_at_create(&post(false, Some("myspace"))),
            "an unknown platform indexes as fauna and publishes like one"
        );
        assert!(!publishable_off_box_at_create(&post(
            true,
            Some("facebook")
        )));
    }

    /// Ruling 1 (`archive-import.md` § Compatibility → *Slice-3 rulings*): the
    /// predicate names every archive platform, from the ONE vocabulary
    /// (`fauna_core::source::ARCHIVE_PLATFORMS`), so a platform added there is
    /// excluded here without a second edit — and a token that is not a
    /// normalized source token can never reach the SQL.
    #[test]
    fn the_predicate_refuses_every_archive_platform_from_the_one_vocabulary() {
        let sql = PUBLIC_POST_SERVABLE.as_str();
        for token in fauna_core::source::ARCHIVE_PLATFORMS {
            assert_eq!(
                fauna_core::source::normalize(token).as_deref(),
                Some(*token),
                "{token} is a normalized token (no quoting hazard)"
            );
            assert!(sql.contains(&format!("'{token}'")), "{sql}");
        }
        assert!(
            sql.contains("COALESCE(c.source, 'fauna') NOT IN ("),
            "{sql}"
        );
        // The pre-existing arms are untouched.
        for arm in [
            "c.schema LIKE 'post/%'",
            "cm.content_id IS NOT NULL",
            "cm.gated_tier IS NULL",
            "cm.legal_takedown_ref IS NULL",
            "COALESCE(cm.quarantined, 0) = 0",
            "COALESCE(cm.suppressed, 0) = 0",
        ] {
            assert!(sql.contains(arm), "{arm} missing from {sql}");
        }
    }
}
