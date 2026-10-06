//! Shared feed snapshot + rendering logic lifted out of the six apps.
//!
//! **Post source classification.** The wire `source` field is a
//! comma-separated protocol list, e.g. `"fauna, bluesky"` (per
//! `docs/goal/ui/feed.md` § Posts). Before this crate, web, linux,
//! apple, android and windows each reimplemented an identical
//! `source -> badge` switch (`protocolIcon`/`protocolLabel`,
//! `build_protocol_badge`, `ProtocolBadge`, `SourceBadge`, …). They now
//! share [`classify_sources`] / [`SourceKind`] and keep only their
//! genuinely platform-specific icon mapping (emoji vs SF Symbols vs
//! Material vs GTK icon names), keyed off [`SourceKind::id`].
//!
//! **The feed snapshot** ([`FeedManager`] / [`FeedSnapshot`]). The Feed page
//! renders from a single shared `FeedManager` snapshot — the direct analogue of
//! `fauna_conversations::ConversationsManager` (`docs/goal/ui/feed.md` § State &
//! data shape, ratified 2026-06-14): the manager owns the post-list read model,
//! compose validation, feed/bridge mutations, and the quoted-post projection;
//! the apps only render the snapshot + forward gestures, retiring the
//! priority-#1 divergence where each app open-coded its own post-list state.
//! See [`manager`] for the read-model rules (nest-side selection/filter/sort;
//! client-side dedup-by-`post_id` + preserve-order).

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_feed");

pub mod compose;
pub mod cue_tracker;
pub mod cues;
pub mod drafts;
pub mod manager;
pub mod observer;
pub mod personalization;
pub mod quote;
pub mod sealed_compose;
pub mod snapshot;
// `debug_assertions` arm: see the gate rationale on `manager.rs`'s
// `set_feed_snapshot_for_test` (testing.md convention 15 — visibility is
// profile-aware so a plain debug build of linux/tui reaches the seams without the
// dep naming the feature; release strips them unless the feature opts in).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub mod test_support;

pub use compose::{
    AttachedFile, BridgeFormState, ComposeAttachmentUpload, ComposeUploadPart, FactorWeightInput,
    FeedComposeState, FilterRuleInput, GateRoomOption, SellComposeState,
};
pub use cue_tracker::{CueRow, CueTracker, LeaveModel};
pub use cues::{
    CueEngine, CueObservation, CueRollup, CueVerdict, ItemCues, seal_cue_rollup, unseal_cue_rollup,
};
pub use drafts::{PostDraftRestoreError, PostDrafts};
pub use manager::{FeedManager, feed_posts_json, feed_reloads_json, refusal_i18n_key};
pub use observer::FeedSnapshotObserver;
pub use personalization::{
    ReviewNgram, ScoredExemplar, TrainResult, TrainVerb, TrainedModelReview,
};
// Re-exported for `FeedManager::set_trained_factor_for_test`'s signature: a
// client unit-testing its train-verb render has to be able to name the model
// type, and reaching past this crate into `fauna-text-model` for it would make
// every shell take a dependency on the model crate to write one test.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use fauna_text_model::topic::TopicModel;
pub use snapshot::{
    AuthorDisplayView, AvailableBridge, BridgeFeedView, FeedEmptyState, FeedSnapshot, FeedStatus,
    FeedSummaryView, PlaybackSource, PostResolution, PostSummary, QuotedPostView, ReplyAudience,
    TipSenderView, TipView, UnlockOfferView, feed_empty_state,
};

use fauna_core::source_glyph::{BridgeIdentitySnapshot, SourceGlyph};
use serde::{Deserialize, Serialize};

/// The origin protocol of a post, classified from a wire `source` token.
///
/// `Other` carries the raw (trimmed, lowercased) token so an unknown
/// source still surfaces something meaningful, matching the pre-lift
/// per-app `default` arms (web rendered the raw `source` string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SourceKind {
    /// A native Fauna post.
    Fauna,
    /// Bridged from Bluesky / the AT Protocol.
    Bluesky,
    /// Bridged from Nostr.
    Nostr,
    /// Bridged from the Fediverse (ActivityPub).
    ActivityPub,
    /// Bridged from email.
    Email,
    /// Re-authored from a Facebook export archive (`behavior/archive-import.md`).
    /// A native signed post whose *origin* is Facebook — see [`Self::is_native`].
    Facebook,
    /// Re-authored from an Instagram export archive — same rule as [`Self::Facebook`].
    Instagram,
    /// A source token outside the known set; carries the raw token.
    Other { raw: String },
    /// Carried by a third-party bridge: the token is a bridge's manifest id,
    /// resolved against the bridges roster to the identity that bridge
    /// declared (`ui/feed.md` § Implementation status today, `SourceKind::
    /// Bridged`; the one-adapter rule `ui/conversations.md` § Where logic lives
    /// → *The `Bridged` adapter* owns). Only [`classify_sources`] produces it —
    /// a token alone cannot tell a bridge from an unknown source. Appended
    /// last, so existing FFI discriminants stand.
    Bridged {
        id: String,
        label: String,
        glyph: SourceGlyph,
    },
}

impl SourceKind {
    /// Classify a single source token. Case-insensitive; surrounding
    /// whitespace is ignored. Tokens outside the known set — including
    /// the empty string — become [`SourceKind::Other`] carrying the
    /// normalized (trimmed, lowercased) token.
    ///
    /// The known set is exactly the canonical lowercase protocol names
    /// the wire emits and the apps already switched on
    /// (`fauna`/`bluesky`/`nostr`/`activitypub`/`email`), plus the
    /// archive-import platforms `facebook`/`instagram`
    /// (`fauna_core::source::ARCHIVE_PLATFORMS`); no speculative
    /// aliases, so behavior matches the pre-lift clients exactly.
    pub fn classify(token: &str) -> SourceKind {
        let normalized = token.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "fauna" => SourceKind::Fauna,
            "bluesky" => SourceKind::Bluesky,
            "nostr" => SourceKind::Nostr,
            "activitypub" => SourceKind::ActivityPub,
            "email" => SourceKind::Email,
            fauna_core::source::FACEBOOK => SourceKind::Facebook,
            fauna_core::source::INSTAGRAM => SourceKind::Instagram,
            _ => SourceKind::Other { raw: normalized },
        }
    }

    /// Stable lowercase discriminant for keying a client's platform icon
    /// map — or an i18n string table — without depending on [`label`]'s
    /// English text: one of `"fauna" | "bluesky" | "nostr" |
    /// "activitypub" | "email" | "facebook" | "instagram" | "other"`, or a
    /// bridged post's bridge id. Unlike the raw token, every `Other`
    /// collapses to `"other"` so clients have a single fallback key.
    ///
    /// [`label`]: SourceKind::label
    pub fn id(&self) -> String {
        match self {
            SourceKind::Fauna => "fauna",
            SourceKind::Bluesky => "bluesky",
            SourceKind::Nostr => "nostr",
            SourceKind::ActivityPub => "activitypub",
            SourceKind::Email => "email",
            SourceKind::Facebook => fauna_core::source::FACEBOOK,
            SourceKind::Instagram => fauna_core::source::INSTAGRAM,
            SourceKind::Other { .. } => "other",
            SourceKind::Bridged { id, .. } => return id.clone(),
        }
        .to_string()
    }

    /// Canonical user-facing display label. `ActivityPub` renders as
    /// "Fediverse" (the user-facing term the web app already used);
    /// an `Other` with a non-empty raw token shows the raw token, and an
    /// empty one shows "Unknown" (matching web's `protocolLabel`
    /// default).
    pub fn label(&self) -> String {
        match self {
            SourceKind::Fauna => "Fauna".to_string(),
            SourceKind::Bluesky => "Bluesky".to_string(),
            SourceKind::Nostr => "Nostr".to_string(),
            SourceKind::ActivityPub => "Fediverse".to_string(),
            SourceKind::Email => "Email".to_string(),
            SourceKind::Facebook => "Facebook".to_string(),
            SourceKind::Instagram => "Instagram".to_string(),
            SourceKind::Other { raw } => {
                if raw.is_empty() {
                    "Unknown".to_string()
                } else {
                    raw.clone()
                }
            }
            SourceKind::Bridged { label, .. } => label.clone(),
        }
    }

    /// The canonical icon concept for this source — the feed-side half of the
    /// shared `Rail`/`SourceKind → SourceGlyph` mapping (the conversations rail
    /// uses [`fauna_conversations::Rail::glyph`]). Lets every app key one
    /// native-asset map off [`SourceGlyph`] for both the feed badge and the
    /// rail. See `docs/goal/architecture/render-model.md` § Deltas → D5.
    pub fn glyph(&self) -> SourceGlyph {
        match self {
            SourceKind::Fauna => SourceGlyph::Fox,
            SourceKind::Bluesky => SourceGlyph::Butterfly,
            SourceKind::Nostr => SourceGlyph::Bolt,
            SourceKind::ActivityPub => SourceGlyph::Globe,
            SourceKind::Email => SourceGlyph::Envelope,
            SourceKind::Facebook | SourceKind::Instagram => SourceGlyph::Archive,
            SourceKind::Other { .. } => SourceGlyph::Unknown,
            SourceKind::Bridged { glyph, .. } => *glyph,
        }
    }

    /// Whether a post carrying this source is a **native Fauna post** — a
    /// signed record on a Fauna nest whose likes, reposts and replies are
    /// signed Fauna posts referencing it — as opposed to **bridged** content
    /// whose canonical home is another network and whose interactions travel
    /// through the bridge's interact door. An archive import is native: the
    /// account re-authored and signed the post itself; only its *origin* is
    /// another platform (`behavior/archive-import.md` § What each category
    /// becomes → the post origin field). `Other` is not native — the safe
    /// direction for a token this build does not know. The token-level twin
    /// the nest uses is [`fauna_core::source::is_native`].
    pub fn is_native(&self) -> bool {
        matches!(
            self,
            SourceKind::Fauna | SourceKind::Facebook | SourceKind::Instagram
        )
    }
}

/// Parse the wire `source` field — a comma-separated protocol list such
/// as `"fauna, bluesky"` (`docs/goal/ui/feed.md` § Posts) — into a
/// deduplicated list of [`SourceKind`]s, preserving the order tokens
/// appear in. Empty tokens (e.g. from a trailing comma or an absent
/// field) are skipped; duplicates collapse to their first occurrence.
/// An empty or whitespace-only field yields an empty list (no badge).
///
/// `bridges` is the account's bridges roster — each listed bridge's declared
/// identity (`fauna.bridges.list`'s `BridgeStatus`, `ui/feed.md`
/// § Implementation status today). A token naming a listed bridge classifies
/// [`SourceKind::Bridged`] with that bridge's label and glyph; an unlisted one
/// stays [`SourceKind::Other`]. The first-party tokens are reserved bridge ids
/// (`architecture/third-party.md` § The manifest → *The `bridge` block*), so a
/// known token never reaches the roster.
pub fn classify_sources(source_field: &str, bridges: &[BridgeIdentitySnapshot]) -> Vec<SourceKind> {
    let mut out: Vec<SourceKind> = Vec::new();
    for token in source_field.split(',') {
        if token.trim().is_empty() {
            continue;
        }
        let kind = match SourceKind::classify(token) {
            SourceKind::Other { raw } => match bridges.iter().find(|b| b.id == raw) {
                Some(bridge) => SourceKind::Bridged {
                    id: bridge.id.clone(),
                    label: bridge.label.clone(),
                    glyph: bridge.glyph,
                },
                None => SourceKind::Other { raw },
            },
            known => known,
        };
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    out
}

/// Project a `fauna.bridges.list` reply's rows onto the roster
/// [`classify_sources`] resolves a bridged post's source token against
/// (`ui/feed.md` § Implementation status today, `SourceKind::Bridged`). A row
/// carrying a declared `glyph` is a consented third-party conversation bridge
/// and becomes one roster entry — its manifest id, its display name as the
/// label, the glyph read through [`SourceGlyph::from_id`]. A row without one is
/// a first-party provider, whose token is a reserved id [`SourceKind::classify`]
/// already knows, so it never needs a roster entry. `available` is deliberately
/// not consulted: a post ingested while the bridge was up still names it.
pub fn bridge_roster(
    bridges: &[fauna_protocol::bridges_ui::BridgeStatus],
) -> Vec<BridgeIdentitySnapshot> {
    bridges
        .iter()
        .filter_map(|b| {
            let glyph = b.glyph.as_deref()?;
            Some(BridgeIdentitySnapshot {
                id: b.id.clone(),
                label: b.name.clone(),
                glyph: SourceGlyph::from_id(glyph),
            })
        })
        .collect()
}

/// Canonical display label for a [`SourceKind`], exported for native
/// (UniFFI) clients. UniFFI `Enum` types can't carry exported methods,
/// so this free function is how Swift/Kotlin/C#/Linux reach
/// [`SourceKind::label`] — without it each would re-hardcode labels,
/// re-introducing the divergence this crate removes. (Web reads the
/// label off the `fauna_wasm::classifySources` `{ id, label }` shape
/// instead.)
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn source_label(kind: SourceKind) -> String {
    kind.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_tokens() {
        assert_eq!(SourceKind::classify("fauna"), SourceKind::Fauna);
        assert_eq!(SourceKind::classify("bluesky"), SourceKind::Bluesky);
        assert_eq!(SourceKind::classify("nostr"), SourceKind::Nostr);
        assert_eq!(SourceKind::classify("activitypub"), SourceKind::ActivityPub);
        assert_eq!(SourceKind::classify("email"), SourceKind::Email);
        assert_eq!(SourceKind::classify("facebook"), SourceKind::Facebook);
        assert_eq!(SourceKind::classify("Instagram"), SourceKind::Instagram);
    }

    #[test]
    fn classify_is_case_and_whitespace_insensitive() {
        assert_eq!(SourceKind::classify("  FAUNA "), SourceKind::Fauna);
        assert_eq!(SourceKind::classify("BlueSky"), SourceKind::Bluesky);
        assert_eq!(SourceKind::classify("ActivityPub"), SourceKind::ActivityPub);
    }

    #[test]
    fn classify_unknown_keeps_normalized_raw() {
        assert_eq!(
            SourceKind::classify(" RSS "),
            SourceKind::Other {
                raw: "rss".to_string()
            }
        );
        // Empty token is a defined Other("") — callers normally pre-filter.
        assert_eq!(
            SourceKind::classify(""),
            SourceKind::Other { raw: String::new() }
        );
    }

    #[test]
    fn id_is_stable_discriminant() {
        assert_eq!(SourceKind::Fauna.id(), "fauna");
        assert_eq!(SourceKind::Bluesky.id(), "bluesky");
        assert_eq!(SourceKind::Nostr.id(), "nostr");
        assert_eq!(SourceKind::ActivityPub.id(), "activitypub");
        assert_eq!(SourceKind::Email.id(), "email");
        assert_eq!(SourceKind::Facebook.id(), "facebook");
        assert_eq!(SourceKind::Instagram.id(), "instagram");
        // Every Other collapses to a single fallback key.
        assert_eq!(SourceKind::Other { raw: "rss".into() }.id(), "other");
        assert_eq!(SourceKind::Other { raw: String::new() }.id(), "other");
    }

    #[test]
    fn glyph_is_canonical_concept() {
        use fauna_core::source_glyph::SourceGlyph;
        // The brand decision ratified by the user 2026-06-22 (render-model.md
        // § Deltas → D5). A change here must be a deliberate re-brand.
        assert_eq!(SourceKind::Fauna.glyph(), SourceGlyph::Fox);
        assert_eq!(SourceKind::Bluesky.glyph(), SourceGlyph::Butterfly);
        assert_eq!(SourceKind::Nostr.glyph(), SourceGlyph::Bolt);
        // Fediverse → the generic globe, not the Mastodon-specific elephant.
        assert_eq!(SourceKind::ActivityPub.glyph(), SourceGlyph::Globe);
        assert_eq!(SourceKind::Email.glyph(), SourceGlyph::Envelope);
        // An archive import is one concept — the box — whatever the platform;
        // the badge LABEL names the platform (archive-import.md § What each
        // category becomes → the post origin field).
        assert_eq!(SourceKind::Facebook.glyph(), SourceGlyph::Archive);
        assert_eq!(SourceKind::Instagram.glyph(), SourceGlyph::Archive);
        assert_eq!(
            SourceKind::Other { raw: "rss".into() }.glyph(),
            SourceGlyph::Unknown
        );
    }

    #[test]
    fn label_matches_pre_lift_clients() {
        assert_eq!(SourceKind::Fauna.label(), "Fauna");
        assert_eq!(SourceKind::Bluesky.label(), "Bluesky");
        assert_eq!(SourceKind::Nostr.label(), "Nostr");
        // Web showed "Fediverse" for activitypub — the canonical label.
        assert_eq!(SourceKind::ActivityPub.label(), "Fediverse");
        assert_eq!(SourceKind::Email.label(), "Email");
        assert_eq!(SourceKind::Facebook.label(), "Facebook");
        assert_eq!(SourceKind::Instagram.label(), "Instagram");
        // Other shows the raw token, or "Unknown" when empty.
        assert_eq!(SourceKind::Other { raw: "rss".into() }.label(), "rss");
        assert_eq!(SourceKind::Other { raw: String::new() }.label(), "Unknown");
    }

    /// The feed manager routes like/repost/reply by this: a native post gets a
    /// signed Fauna post referencing it, a bridged one goes through the bridge
    /// interact door. An archive import is native — the account signed it —
    /// so its interactions must never be sent to a bridge that does not exist.
    #[test]
    fn native_is_fauna_and_the_archive_platforms_and_agrees_with_fauna_core() {
        assert!(SourceKind::Fauna.is_native());
        assert!(SourceKind::Facebook.is_native());
        assert!(SourceKind::Instagram.is_native());
        assert!(!SourceKind::Bluesky.is_native());
        assert!(!SourceKind::Nostr.is_native());
        assert!(!SourceKind::ActivityPub.is_native());
        assert!(!SourceKind::Email.is_native());
        assert!(!SourceKind::Other { raw: "rss".into() }.is_native());
        assert!(!SourceKind::Other { raw: String::new() }.is_native());
        // One vocabulary: the nest's interact door asks fauna_core the same
        // question by token and must get the same answer.
        for kind in [
            SourceKind::Fauna,
            SourceKind::Bluesky,
            SourceKind::Nostr,
            SourceKind::ActivityPub,
            SourceKind::Email,
            SourceKind::Facebook,
            SourceKind::Instagram,
        ] {
            assert_eq!(
                kind.is_native(),
                fauna_core::source::is_native(&kind.id()),
                "{kind:?}"
            );
        }
    }

    fn matrix() -> BridgeIdentitySnapshot {
        BridgeIdentitySnapshot {
            id: "matrix".into(),
            label: "Matrix".into(),
            glyph: SourceGlyph::Bridge,
        }
    }

    /// A token naming a listed bridge is that bridge — its id, its declared
    /// label and glyph; the same token with no roster entry is an unknown
    /// source, as before.
    #[test]
    fn a_token_naming_a_listed_bridge_classifies_bridged() {
        let bridged = SourceKind::Bridged {
            id: "matrix".into(),
            label: "Matrix".into(),
            glyph: SourceGlyph::Bridge,
        };
        assert_eq!(
            classify_sources("fauna, Matrix", &[matrix()]),
            vec![SourceKind::Fauna, bridged.clone()]
        );
        assert_eq!(bridged.id(), "matrix");
        assert_eq!(bridged.label(), "Matrix");
        assert_eq!(bridged.glyph(), SourceGlyph::Bridge);
        assert!(!bridged.is_native());
        assert_eq!(
            classify_sources("matrix", &[]),
            vec![SourceKind::Other {
                raw: "matrix".into()
            }]
        );
        // A reserved first-party id never resolves through the roster.
        let squatter = BridgeIdentitySnapshot {
            id: "bluesky".into(),
            ..matrix()
        };
        assert_eq!(
            classify_sources("bluesky", &[squatter]),
            vec![SourceKind::Bluesky]
        );
    }

    #[test]
    fn single_source_field() {
        assert_eq!(classify_sources("fauna", &[]), vec![SourceKind::Fauna]);
        assert_eq!(
            classify_sources("  bluesky  ", &[]),
            vec![SourceKind::Bluesky]
        );
    }

    #[test]
    fn multi_source_field_preserves_order() {
        assert_eq!(
            classify_sources("fauna, bluesky", &[]),
            vec![SourceKind::Fauna, SourceKind::Bluesky]
        );
        assert_eq!(
            classify_sources("nostr, activitypub, email", &[]),
            vec![
                SourceKind::Nostr,
                SourceKind::ActivityPub,
                SourceKind::Email
            ]
        );
    }

    #[test]
    fn dedups_preserving_first_occurrence() {
        assert_eq!(
            classify_sources("bluesky, bluesky", &[]),
            vec![SourceKind::Bluesky]
        );
        assert_eq!(
            classify_sources("fauna, bluesky, fauna", &[]),
            vec![SourceKind::Fauna, SourceKind::Bluesky]
        );
    }

    #[test]
    fn skips_empty_tokens() {
        assert_eq!(
            classify_sources("fauna,, bluesky", &[]),
            vec![SourceKind::Fauna, SourceKind::Bluesky]
        );
        assert_eq!(classify_sources("fauna, ", &[]), vec![SourceKind::Fauna]);
    }

    #[test]
    fn empty_field_yields_no_badge() {
        assert_eq!(classify_sources("", &[]), Vec::<SourceKind>::new());
        assert_eq!(classify_sources("   ", &[]), Vec::<SourceKind>::new());
        assert_eq!(classify_sources(", ,", &[]), Vec::<SourceKind>::new());
    }

    #[test]
    fn unknown_source_round_trips_raw() {
        assert_eq!(
            classify_sources("fauna, rss", &[]),
            vec![
                SourceKind::Fauna,
                SourceKind::Other {
                    raw: "rss".to_string()
                }
            ]
        );
    }

    #[test]
    fn serde_round_trip() {
        let kinds = classify_sources("fauna, rss", &[]);
        let json = serde_json::to_string(&kinds).unwrap();
        let back: Vec<SourceKind> = serde_json::from_str(&json).unwrap();
        assert_eq!(kinds, back);
    }
}
