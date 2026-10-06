//! Where a notification row goes when the user opens it — the one decision
//! every app shares (`behavior/notifications.md` § Deep-link destinations).
//!
//! The shape is [`fauna_client_search::SearchNav`]'s, deliberately: a typed
//! target the producing crate mints and app glue routes into the destination
//! page's own gesture, exactly as a direct click there would. The two enums
//! stay **per-domain** rather than merging into one app-wide `NavTarget`
//! because their variant sets barely overlap — search can reach a file or an
//! unsent draft, a notification can reach a knock, the Family page or a post
//! on a bridged network's own website — and a merged enum would hand each
//! app's glue arms it can never receive.
//!
//! `None` means the row is honestly non-navigable and renders inert. It is
//! never a "we could not decide": each `None` below has a stated reason, and
//! those reasons are what the goal doc's table records.

use fauna_protocol::notifications::{NotifItem, NotifType};

/// Where a notification row navigates when the user opens it — a **typed**
/// target, never a raw id the app parses itself
/// (`behavior/notifications.md` § Deep-link destinations).
/// Serialized for the wasm boundary (`SearchNav`'s derives): the web SPA reads
/// the same decision as a plain tagged object rather than re-deriving it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NotificationDestination {
    /// The liked post's detail view, by its real 64-hex post id.
    ///
    /// Minted from a **native** row's `content_id`, which for the like
    /// producer is the liked post's id verbatim
    /// (`bins/fauna-nest/src/interact_routes.rs` — `insert_notification(…,
    /// "like", "fauna", Some(&liker_id), Some(&post_id_bytes), …)`). That is
    /// the same spelling `SearchNav::Post` carries and the same one
    /// `fauna_feed::FeedManager::resolve_post` takes, so an app hands it
    /// straight to its post-detail seam — including for a post the timeline
    /// never loaded, which `resolve_post` fetches.
    Post { post_id: String },
    /// The pending-knock surface on the Contacts page, by the knocker's
    /// 64-hex actor id.
    ///
    /// The knock row carries no content id at all — a knock *is* its sender
    /// (`bins/fauna-nest/src/routes.rs`) — so the sender is the target. Apps
    /// render pending knocks as a `knock-request-item` block on Contacts
    /// (ui.yaml `contacts`), which is where accept/block/dismiss live; the id
    /// rides along so an app that can highlight the matching row does, and one
    /// that lists them flat simply opens the page.
    Knock { sender_id: String },
    /// The Family page.
    ///
    /// Carries no id on purpose: every `family.*` doorbell's `content_id` is a
    /// **dedup token**, not a navigable identity — `"{day}:{category}"` for a
    /// content notice, `peer ‖ row_id` for a contact ask, the bare row id for a
    /// feed-source ask or its approval (`bins/fauna-nest/src/family_handlers.rs`'s
    /// three `*_dedup_key` builders). The page itself is the destination: for
    /// the guardian it is where the pending queue (`family-approval-item`) and
    /// the per-ward readouts (`family-ward-content-notices`) render; for the
    /// ward it is where their own supervision state lives.
    Family,
    /// A page outside Fauna, handed to the OS default browser — today the
    /// `bsky.app` page of the post a bridged Bluesky notification is about.
    ///
    /// Minted from a `bluesky` row's `subject_uri` through
    /// [`fauna_core::bluesky_web_url::post_web_url`]: a fixed origin plus
    /// charset-validated segments, never the subject string itself, so a
    /// hostile subject cannot steer the browser off a post page. The user ruled
    /// 2026-09-25 that leaving the app for the bridged network's own site is
    /// the product's posture; an app opens it exactly as it opens any other
    /// external link (tui's `os_open`, apple's `OpenURL`, …), and never inside
    /// a Fauna page.
    External { url: String },
}

/// The destination for one notification row, or `None` when the row is
/// honestly informational.
///
/// **Keyed on `source` first, then `notif_type`** — not on `notif_type` alone.
/// A bridged row reuses the native type vocabulary (`like`, `reply`, …) while
/// carrying completely different ids: its `content_id` is the hex of the
/// notification's own AT-URI, a dedup token minted because a bridged row has
/// no 32-byte fauna sender or content key to dedup on
/// (`bins/fauna-nest/src/bluesky/notif_sync.rs`). Treating that as a post id
/// would deep-link every bridged like to a post that cannot exist, so the
/// source axis is load-bearing, not decoration.
///
/// An unknown `source`, an unknown `notif_type`, or a known one whose row is
/// missing the id its destination needs all return `None`: a newer nest may
/// mint types this build has never heard of, and forward-compatibility here
/// means rendering them inert rather than guessing.
pub fn notification_destination(item: &NotifItem) -> Option<NotificationDestination> {
    match item.source.as_str() {
        "fauna" => native_destination(item),
        // A Bluesky row opens the post it is ABOUT on bsky.app, whatever its
        // type: `subject_uri` is the AppView's `reasonSubject` — the liked,
        // reposted, quoted or replied-to post — relayed verbatim
        // (`libs/fauna-bridge-atproto/src/translate.rs`). It is read by
        // every arm rather than per type because the type vocabulary is the
        // AppView's to grow, and a subject that is a post is navigable
        // whichever reason carried it. A row without a post subject — a
        // `follow`, a `mention` (the AppView sends no subject for one) — is
        // informational; `content_id` is never consulted, since it is the hex
        // of the row's OWN record (a like record has no page).
        "bluesky" => item
            .subject_uri
            .as_deref()
            .and_then(fauna_core::bluesky_web_url::post_web_url)
            .map(|url| NotificationDestination::External { url }),
        // Any other bridged source (`nostr`, `activitypub`, or one a newer
        // nest mints) carries subjects this build has no address form for.
        _ => None,
    }
}

/// The `source == "fauna"` half of [`notification_destination`].
fn native_destination(item: &NotifItem) -> Option<NotificationDestination> {
    match &item.notif_type {
        NotifType::Like => item
            .content_id
            .clone()
            .map(|post_id| NotificationDestination::Post { post_id }),
        NotifType::Knock => item
            .sender_id
            .clone()
            .map(|sender_id| NotificationDestination::Knock { sender_id }),
        // All four family doorbells land on the Family page: the three rung at
        // the GUARDIAN, who decides there, and `family.feed_source_approved`,
        // rung at the WARD — user-ruled 2026-09-25 to go to the same page as
        // its siblings (uniformity) rather than back to the bridges surface
        // where the refusal happened and the "approved — try again" state
        // renders (`family-safety.md` § Feed-source approvals).
        NotifType::FamilyContentNotice
        | NotifType::FamilyContactRequest
        | NotifType::FamilyFeedSourceRequest
        | NotifType::FamilyFeedSourceApproved => Some(NotificationDestination::Family),
        // Everything else is informational, each for its own stated reason
        // (`behavior/notifications.md` § Deep-link destinations):
        // `security.notice` carries the whole notice in its own row and a
        // dedup token where an id would be; `mail.forward_queue_evicted`
        // carries no ids at all and its only candidate surface is admin-gated;
        // anything unknown postdates this build. Listed rather than `_`, so a
        // type added to `NotifType` is a compile error here until someone
        // decides where it goes.
        NotifType::Reply
        | NotifType::Repost
        | NotifType::Quote
        | NotifType::Mention
        | NotifType::Follow
        | NotifType::Interaction
        | NotifType::Message
        | NotifType::EventInvite
        | NotifType::GroupInvite
        | NotifType::SecurityNotice
        | NotifType::MailForwardQueueEvicted
        | NotifType::AbuseReportReceived
        | NotifType::AbuseReportResolved
        | NotifType::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: &str, notif_type: &str) -> NotifItem {
        NotifItem {
            notif_type: notif_type.into(),
            source: source.into(),
            summary: "something happened".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_native_like_opens_the_liked_post() {
        let mut item = row("fauna", "like");
        item.sender_id = Some("bb".repeat(32));
        item.content_id = Some("aa".repeat(32));
        assert_eq!(
            notification_destination(&item),
            Some(NotificationDestination::Post {
                post_id: "aa".repeat(32)
            })
        );
    }

    /// The trap this router exists to avoid: a bridged like reuses the native
    /// `notif_type` but its `content_id` is the hex of its own AT-URI, a dedup
    /// token. Routed as a post id it would deep-link to a post that cannot
    /// exist — so the `source` axis, not the type, decides first, and what it
    /// reads is `subject_uri`, the post the like is ABOUT, opened off-app on
    /// bsky.app (user-ruled 2026-09-25).
    #[test]
    fn a_bridged_like_opens_its_subject_post_on_bsky_app_never_its_content_id() {
        let mut item = row("bluesky", "like");
        item.content_id = Some(hex_of("at://did:plc:xyz/app.bsky.feed.like/1"));
        item.subject_uri = Some("at://did:plc:xyz/app.bsky.feed.post/3kpost".into());
        assert_eq!(
            notification_destination(&item),
            Some(NotificationDestination::External {
                url: "https://bsky.app/profile/did:plc:xyz/post/3kpost".into()
            }),
        );
        // Same row, subject gone: the content_id alone must never become a
        // destination of any kind — it is a dedup token, not an address.
        item.subject_uri = None;
        assert_eq!(
            notification_destination(&item),
            None,
            "a bridged row's content_id is a dedup token, never a fauna post id nor a link"
        );
    }

    /// Every bridged type with a post subject navigates off-app, not just
    /// `like` — the subject, not the type vocabulary, is what is navigable, so
    /// a reason the AppView adds later routes without a new arm here.
    #[test]
    fn every_bridged_type_with_a_post_subject_opens_it_off_app() {
        for notif_type in ["like", "reply", "repost", "quote", "mention", "other"] {
            let mut item = row("bluesky", notif_type);
            item.content_id = Some(hex_of("at://did:plc:xyz/app.bsky.feed.post/1"));
            item.subject_uri = Some("at://did:plc:xyz/app.bsky.feed.post/3kpost".into());
            assert_eq!(
                notification_destination(&item),
                Some(NotificationDestination::External {
                    url: "https://bsky.app/profile/did:plc:xyz/post/3kpost".into()
                }),
                "bridged {notif_type} must open its subject post"
            );
        }
    }

    /// A bridged row whose subject is not a post — a `follow` (no subject at
    /// all), or a subject naming some other record — is honestly inert: there
    /// is no post page to open, and guessing one would hand the browser a 404.
    #[test]
    fn a_bridged_row_without_a_post_subject_is_inert() {
        let mut follow = row("bluesky", "follow");
        follow.content_id = Some(hex_of("at://did:plc:xyz/app.bsky.graph.follow/1"));
        assert_eq!(notification_destination(&follow), None);

        let mut odd = row("bluesky", "like");
        odd.subject_uri = Some("at://did:plc:xyz/app.bsky.feed.generator/3kfeed".into());
        assert_eq!(notification_destination(&odd), None);
    }

    /// Only `bluesky` has an address form this build knows. Another bridged
    /// source's subject — or a source a newer nest mints — renders inert
    /// rather than being handed to bsky.app or guessed at.
    #[test]
    fn a_non_bluesky_bridged_source_is_inert_even_with_a_post_shaped_subject() {
        for source in ["nostr", "activitypub", "some.future.bridge"] {
            let mut item = row(source, "like");
            item.subject_uri = Some("at://did:plc:xyz/app.bsky.feed.post/3kpost".into());
            item.content_id = Some("aa".repeat(32));
            assert_eq!(
                notification_destination(&item),
                None,
                "{source} must stay inert"
            );
        }
    }

    #[test]
    fn a_knock_opens_its_sender_on_contacts() {
        let mut item = row("fauna", "knock");
        item.sender_id = Some("cc".repeat(32));
        assert_eq!(
            notification_destination(&item),
            Some(NotificationDestination::Knock {
                sender_id: "cc".repeat(32)
            })
        );
    }

    /// The three guardian-side doorbells all land on the one Family page, and
    /// none of them reads its `content_id` — which is a dedup token whose
    /// bytes are `"{day}:{category}"` or `peer ‖ row_id`, not an identity.
    #[test]
    fn the_guardian_family_doorbells_open_the_family_page() {
        for notif_type in [
            "family.content_notice",
            "family.contact_request",
            "family.feed_source_request",
        ] {
            let mut item = row("fauna", notif_type);
            item.sender_id = Some("dd".repeat(32));
            item.content_id = Some(hex_of("20260920:nsfw"));
            assert_eq!(
                notification_destination(&item),
                Some(NotificationDestination::Family),
                "{notif_type} must open the Family page"
            );
        }
    }

    /// The ward-side doorbell goes to the same Family page as its three
    /// guardian-side siblings (user-ruled 2026-09-25, for uniformity), and
    /// like them never reads its `content_id` — the bare row id, a dedup
    /// token.
    #[test]
    fn the_ward_side_feed_source_approval_opens_the_family_page() {
        let mut item = row("fauna", "family.feed_source_approved");
        item.content_id = Some(hex_of("42"));
        assert_eq!(
            notification_destination(&item),
            Some(NotificationDestination::Family)
        );
        item.content_id = None;
        assert_eq!(
            notification_destination(&item),
            Some(NotificationDestination::Family),
            "the page is the destination; no id is needed"
        );
    }

    /// A security notice's `content_id` is the inbox row id used as a dedup
    /// token — routing on it would open nothing. The notice's whole detail is
    /// already in the row it is read from.
    #[test]
    fn a_security_notice_is_informational() {
        let mut item = row("fauna", "security.notice");
        item.content_id = Some("2a00000000000000".into());
        assert_eq!(notification_destination(&item), None);
    }

    #[test]
    fn a_forward_queue_eviction_is_informational() {
        assert_eq!(
            notification_destination(&row("fauna", "mail.forward_queue_evicted")),
            None
        );
    }

    /// Forward compatibility: a newer nest's type renders inert rather than
    /// guessing a page, exactly as an unknown body key renders the summary.
    #[test]
    fn a_type_this_build_has_never_heard_of_is_inert() {
        let mut item = row("fauna", "fauna.some_future_thing");
        item.content_id = Some("aa".repeat(32));
        item.sender_id = Some("bb".repeat(32));
        assert_eq!(notification_destination(&item), None);
    }

    /// A known type missing the one id its destination needs is inert, not a
    /// panic and not a half-target — the nest's producers always populate it,
    /// so this is the corrupted-row / non-conforming-nest case.
    #[test]
    fn a_known_type_without_its_id_is_inert() {
        assert_eq!(notification_destination(&row("fauna", "like")), None);
        assert_eq!(notification_destination(&row("fauna", "knock")), None);
        assert_eq!(notification_destination(&row("bluesky", "like")), None);
    }

    /// The wasm boundary hands the web SPA this enum as a serde value; the
    /// `External` arm must arrive as `{ External: { url } }` like its
    /// siblings' externally-tagged shape, not as some renamed form.
    #[test]
    fn the_external_arm_serializes_externally_tagged() {
        let json = serde_json::to_value(NotificationDestination::External {
            url: "https://bsky.app/profile/did:plc:xyz/post/3kpost".into(),
        })
        .expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({ "External": { "url": "https://bsky.app/profile/did:plc:xyz/post/3kpost" } })
        );
        assert_eq!(
            serde_json::to_value(NotificationDestination::Family).expect("serializes"),
            serde_json::json!("Family")
        );
    }

    fn hex_of(s: &str) -> String {
        s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }
}
