//! Compose + bridge-subscribe form state, and the create-feed rule input —
//! the mutable form state the manager owns and validates (`docs/goal/ui/feed.md`
//! § Where logic lives: compose validation is shared Rust; the file *picker* is
//! client glue, the *validation* + staged-file metadata are shared).

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// A file staged on the composer (`compose-file` / paste). The picker is
/// client glue; the manager holds only the light metadata + the content-address
/// (`blob_hash`, lowercase-hex BLAKE3) the post body references — never the
/// bytes, keeping the observed snapshot cheap over UniFFI (mirrors how
/// `ConversationsManager` keeps attachment bytes off `FeedComposeState`).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttachedFile {
    pub name: String,
    pub size: u64,
    /// Set once the client has uploaded the blob and resolved its hash; the
    /// post body's `MediaItem` references it. `None` while a file is picked but
    /// not yet uploaded.
    pub blob_hash: Option<String>,
    /// MIME type the client resolved at upload time (e.g. from the sealed-blob
    /// sidecar). `submit_post` puts it on the `MediaItem.media_type`; `None`
    /// falls back to `application/octet-stream`. The file *picker* + upload are
    /// client glue; this staged metadata is shared.
    #[serde(default)]
    pub media_type: Option<String>,
}

/// One blob's two `multipart/form-data` parts, exactly as the nest's
/// `POST /api/v1/blob` expects them — the FFI/wasm-expressible twin of
/// `fauna_media::pipeline::MultipartBlob` (that type is not a `uniffi::Record`
/// and `fauna-media` is not an FFI crate, so the shape is restated at this
/// boundary rather than the crate being dragged across it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ComposeUploadPart {
    /// Canonical DAG-CBOR `UploadSidecar` — the `sidecar` part.
    pub sidecar_cbor: Vec<u8>,
    /// Sealed (audience-restricted) or plaintext (public) bytes — the `bytes`
    /// part.
    pub bytes: Vec<u8>,
}

/// What [`FeedManager::seal_compose_attachment`](crate::FeedManager::seal_compose_attachment)
/// hands back: one compose attachment, processed and sealed for **the
/// composer's current audience**, ready for the app to POST.
///
/// The app POSTs [`thumbnail`](Self::thumbnail) first (the primary's sidecar
/// already names its hash — `fauna_media::pipeline::UploadPayload::into_multipart_parts`
/// owns that ordering rule), then [`primary`](Self::primary), then stages the
/// returned hash with `update_compose` and submits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ComposeAttachmentUpload {
    pub primary: ComposeUploadPart,
    /// Present when the on-device pipeline derived one; sealed under the same
    /// audience as the primary. Best-effort — a failed thumbnail POST must
    /// never fail the post.
    pub thumbnail: Option<ComposeUploadPart>,
    /// The plaintext's real sniffed MIME — what the post body's `MediaItem`
    /// must name. For a sealed attachment this deliberately differs from the
    /// sidecar's `application/octet-stream` (the sealed-class contract), which
    /// is why the app must take it from here and not from the sidecar or an OS
    /// filename guess.
    pub media_type: String,
    /// `true` when the parts are AEAD-sealed under the staged tier's period
    /// key — i.e. the composer was audience-restricted at seal time. Apps use
    /// it for nothing but honesty in logs and tests; the upload is identical
    /// either way.
    pub sealed: bool,
}

/// The composer's editable state (`feed-compose-bar`). `submit_post` reads it,
/// builds + signs the post, and clears it on success.
///
/// `Feed`-prefixed (not bare `ComposeState`) because UniFFI compiles every
/// exported crate's bindings into one Swift module (`FaunaFFISwift`) / C#
/// namespace, where a bare `ComposeState` collides with
/// `fauna_conversations::ComposeState` (the DM/mail composer) — the same
/// unified-binding collision that renamed `SnapshotObserver` →
/// `FeedSnapshotObserver`. It broke the whole apple build (macOS + iOS, Swift's
/// flat module surfaces it; C# bindgen does not) when `feed-manager` joined the
/// FFI on 2026-06-15; cross-crate FFI type names must be domain-unique.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FeedComposeState {
    pub text: String,
    /// Raw `compose-tags-field` input (comma-separated); the manager
    /// tokenizes + normalizes it into tag facets on submit.
    pub tags: String,
    pub attached_file: Option<AttachedFile>,
    /// `compose-gate-tier-select` value: `None` = Public (the default),
    /// `Some(tier)` = gate the post to that tier — the full body seals under
    /// the tier's current period key at submit (`ui/feed.md` § Encryption at
    /// rest; monetization.md § Pillars 2+3 — app UX).
    #[serde(default)]
    pub gate_tier: Option<String>,
    /// `compose-gate-preview-field` value: the plaintext public teaser that
    /// becomes the gated post's `body`. Only meaningful when the post is gated
    /// — either `gate_tier` is set or [`sell`](Self::sell) is on (both gated
    /// submits validate it non-empty).
    #[serde(default)]
    pub gate_preview: String,
    /// `compose-gate-tier-select`'s **third** answer: "Sell this post…"
    /// (`monetization.md` § Per-post pay-to-unlock). `Some` *is* sell mode.
    ///
    /// Mutually exclusive with [`gate_tier`](Self::gate_tier) **by
    /// construction** — the two setters each clear the other, because both are
    /// answers to the one select asking "who can read this?". That is not
    /// tidiness: [`FeedManager::prepare_sell_post`] mints its own degenerate
    /// tier and never reads `gate_tier`, so "gated to X *and* selling" has no
    /// defined meaning and must not be representable. UI shape user-ratified
    /// 2026-07-29.
    #[serde(default)]
    pub sell: Option<SellComposeState>,
    /// `compose-gate-tier-select`'s **fourth** answer: a room the author sits
    /// on the floor of, as its hex channel id — the post is room-restricted,
    /// its body sealed so only the room's floor members open it (`ui/feed.md`
    /// § Encryption at rest → *Room-restricted — the ruling*). The options are
    /// [`crate::FeedSnapshot::own_rooms`].
    ///
    /// Mutually exclusive with [`gate_tier`](Self::gate_tier) and
    /// [`sell`](Self::sell) by construction, for `sell`'s own reason: all
    /// three answer the one select asking "who can read this?", and each
    /// setter clears the other two.
    #[serde(default)]
    pub gate_room: Option<String>,
    /// Composer-scoped error → `compose-error`.
    pub error: Option<LocalizedText>,
    pub submitting: bool,
}

impl FeedComposeState {
    /// The composer after `sent` was successfully posted: clear what was SENT,
    /// keep what the user changed since (`ui/feed.md` § User actions, the
    /// `post-submit-button` row). Every submit arm clears through this.
    ///
    /// A submit reads the composer once and then awaits — the create, and for a
    /// gated post the upload before it — and the composer stays editable the
    /// whole time. Resetting it wholesale when the create confirms erases
    /// whatever the user started typing for their next post in that window; on
    /// web, whose window was the whole submit including the reload, that was a
    /// measured, silent loss. Per content field (`text`/`tags`/`attached_file`):
    /// one still equal to what was sent IS the sent value and clears; one that
    /// differs is new input and stays. The audience group
    /// (`gate_tier`/`gate_preview`/`sell`/`gate_room`) is **not** symmetric with
    /// content: it is sticky, and clears only when the composer is otherwise
    /// untouched (`ui/feed.md` § User actions) — a picked
    /// audience must not silently widen to Public just because the user kept
    /// typing the next post. The transient pair — `error`, `submitting` —
    /// always resets: it describes the attempt that just succeeded, not
    /// anything the user wrote.
    pub fn clear_sent(&mut self, sent: &FeedComposeState) {
        let empty = Self::default();
        // The audience group — `gate_tier`/`gate_preview`/`sell`/`gate_room` —
        // is sticky (owner ruling): it clears only when the
        // composer is otherwise untouched. If the user has already started
        // the next post (any of the three fields below just changed), the
        // audience they picked stays instead of silently widening to Public;
        // it clears with the post it actually belongs to on a later,
        // untouched submit. A field that already differs from `sent` (the
        // user picked something else mid-flight) was never going to clear
        // anyway, ruling or not. Read before the fields below are mutated.
        let untouched = self.text == sent.text
            && self.tags == sent.tags
            && self.attached_file == sent.attached_file;
        if self.text == sent.text {
            self.text = empty.text;
        }
        if self.tags == sent.tags {
            self.tags = empty.tags;
        }
        if self.attached_file == sent.attached_file {
            self.attached_file = empty.attached_file;
        }
        if untouched {
            if self.gate_tier == sent.gate_tier {
                self.gate_tier = empty.gate_tier;
            }
            if self.gate_preview == sent.gate_preview {
                self.gate_preview = empty.gate_preview;
            }
            // `gate_tier` and `sell` stay mutually exclusive: the live pair
            // already is (each setter clears the other), and clearing either
            // to `None` cannot make it otherwise. The price fields ride
            // inside `SellComposeState`, so bundling `sell` here as one unit
            // already makes them sticky together with the sale.
            if self.sell == sent.sell {
                self.sell = empty.sell;
            }
            // The room answer is the same kind of field as `gate_tier` — the
            // audience this submit sent under — and clears on the same rule.
            if self.gate_room == sent.gate_room {
                self.gate_room = empty.gate_room;
            }
        }
        self.error = empty.error;
        self.submitting = empty.submitting;
    }
}

/// The composer's "Sell this post…" parameters — the two controls that appear
/// when `compose-gate-tier-select` is in sell mode.
///
/// Carried as `Option<SellComposeState>` rather than flat fields on
/// [`FeedComposeState`] so a price can't linger from a sell the author backed
/// out of, and so the "defaults on" rule below lives in exactly one place.
///
/// `monetization.md` § Per-post pay-to-unlock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SellComposeState {
    /// `compose-sell-price` — free text, handed to `prepare_sell_post` as the
    /// minted tier's `price_hint` (display-only, the zero-integration floor of
    /// `monetization.md` § Deliberate generality bounds).
    pub price: String,
    /// `compose-sell-asking-price` — free text, parsed as a whole-sats `u64`
    /// and handed to `prepare_sell_post`'s `asking_price_sats`. Independent of
    /// `price` (the free-text hint above) — no parsing ever infers one from
    /// the other (`monetization.md` § The asking price). Empty means no
    /// machine price: the minted tier stays a tip target forever, never a
    /// purchase. `#[serde(default)]` so an on-disk draft saved before this
    /// field existed still deserializes.
    #[serde(default)]
    pub asking_price: String,
    /// `compose-sell-subscribers-free` — the single ratified rank knob
    /// (`monetization.md:126`). `true` mints the unlock tier at rank 1, inside
    /// every paid subscription; `false` mints it above the author's highest
    /// regular rank (pure pay-per-view).
    ///
    /// **Defaults `true`** (user-ratified 2026-07-29): an existing paying
    /// subscriber should not be charged twice for a post their subscription
    /// would reasonably cover, so pay-per-view is the deliberate opt-in. This
    /// is why the type carries a hand-written [`Default`] rather than deriving
    /// one.
    pub subscribers_get_it_free: bool,
}

impl Default for SellComposeState {
    fn default() -> Self {
        Self {
            price: String::new(),
            asking_price: String::new(),
            subscribers_get_it_free: true,
        }
    }
}

/// One option in the composer's gate-to-tier select: a tier the local actor
/// authors (from `fauna.subscriptions.tiers.list`), carrying the rank the
/// gated build stamps into `GatedInfo.tier_rank`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GateTierOption {
    pub name: String,
    pub rank: u32,
}

/// One room option in the composer's audience select: a room the author can
/// address a post to right now (`RoomPostKeys::room_post_rooms`), by its hex
/// channel id and the label their own conversation list reads it by.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct GateRoomOption {
    pub room: String,
    pub label: String,
}

/// The bridge-subscribe form (`bridge-form-*`) — subscribe a Bluesky /
/// ActivityPub URI as a synthesised feed.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BridgeFormState {
    /// `bridge-form-bridge-select` value (`bluesky` / `activitypub` / …).
    pub bridge_kind: String,
    pub uri: String,
    pub name: String,
    pub error: Option<LocalizedText>,
    pub submitting: bool,
}

/// One create-feed form rule (`create_feed` sub-page). The `(rule_type, value,
/// required)` triple the shared `encode_filter_rule` turns into the
/// externally-tagged `FilterRule` wire JSON (`feed.md` § Where logic lives —
/// feed rule encoding). The form is client glue; the encoding is shared.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FilterRuleInput {
    pub rule_type: String,
    pub value: String,
    pub required: bool,
}

/// One create-feed form factor-weight entry (`feed-factor-*`,
/// `content-moderation-and-ranking.md` § Composition; UI approved
/// 2026-07-08). The factor picker offers the shared built-ins
/// (`fauna_client_feed::builtin_factor_options` — `engagement`, `trending`),
/// then any `labeler:<hex>` factor the caller is subscribed to and their
/// trained `topic:<hex>` factors (client glue — `FeedManager` doesn't reach into
/// the labeler catalog or the sealed topic registry); `weight_permille` is
/// the wire's signed per-mille weight (the dialog's decimal-multiplier input
/// ×1000). `global` routes the entry to the caller's global factor set
/// (`fauna.feed.factors.set`, folded into every feed) instead of this feed's
/// own `composition` — [`FeedManager::create_feed`] does the split + the
/// global-set upsert-merge (a whole-set overwrite must never silently drop a
/// factor this dialog isn't touching).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FactorWeightInput {
    pub factor: String,
    pub weight_permille: i64,
    pub global: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(name: &str) -> AttachedFile {
        AttachedFile {
            name: name.to_string(),
            size: 3,
            blob_hash: Some("ab".repeat(32)),
            media_type: Some("image/png".to_string()),
        }
    }

    /// Nobody touched the composer while the post was in flight: it clears
    /// completely, exactly as the old wholesale reset did.
    #[test]
    fn an_untouched_composer_clears_to_empty() {
        let sent = FeedComposeState {
            text: "first post".into(),
            tags: "rust".into(),
            attached_file: Some(photo("a.png")),
            gate_tier: Some("supporters".into()),
            gate_preview: "teaser".into(),
            submitting: true,
            ..Default::default()
        };
        let mut live = sent.clone();
        live.clear_sent(&sent);
        assert_eq!(live, FeedComposeState::default());
    }

    /// The room answer is an audience like any other: it clears with the post
    /// it went out with, and a room picked for the NEXT post survives.
    #[test]
    fn a_room_answer_clears_with_its_post_and_a_later_one_survives() {
        let sent = FeedComposeState {
            text: "for the room".into(),
            gate_room: Some("c7".repeat(32)),
            gate_preview: "teaser".into(),
            ..Default::default()
        };
        let mut live = sent.clone();
        live.clear_sent(&sent);
        assert_eq!(live.gate_room, None);

        let mut live = sent.clone();
        live.gate_room = Some("0a".repeat(32));
        live.clear_sent(&sent);
        assert_eq!(
            live.gate_room,
            Some("0a".repeat(32)),
            "a room chosen after the click belongs to the next post"
        );
    }

    /// The case the rule exists for: the next post was started before the
    /// first one's create confirmed. What the user changed survives; what went
    /// out with the post clears.
    #[test]
    fn what_the_user_changed_after_the_click_survives() {
        let sent = FeedComposeState {
            text: "first post".into(),
            tags: "rust".into(),
            attached_file: Some(photo("plain.png")),
            ..Default::default()
        };
        let mut live = FeedComposeState {
            text: "second post".into(),
            attached_file: Some(photo("signed.png")),
            submitting: true,
            error: Some(LocalizedText::key("feed.compose_empty")),
            ..sent.clone()
        };
        live.clear_sent(&sent);
        assert_eq!(
            live.text, "second post",
            "the next post's text must not be erased"
        );
        assert_eq!(
            live.attached_file,
            Some(photo("signed.png")),
            "the newly staged file must survive"
        );
        assert_eq!(
            live.tags, "",
            "the unchanged tags went out with the post, so they clear"
        );
        assert!(!live.submitting, "the attempt that just succeeded is over");
        assert_eq!(live.error, None);
    }

    /// Switching audience after the click is a change like any other, and the
    /// select's two answers stay mutually exclusive through the clear.
    #[test]
    fn an_audience_switch_after_the_click_survives_and_stays_exclusive() {
        let sent = FeedComposeState {
            text: "gated".into(),
            gate_tier: Some("supporters".into()),
            gate_preview: "teaser".into(),
            ..Default::default()
        };
        let mut live = FeedComposeState {
            gate_tier: None,
            sell: Some(SellComposeState::default()),
            ..sent.clone()
        };
        live.clear_sent(&sent);
        assert_eq!(live.sell, Some(SellComposeState::default()));
        assert_eq!(live.gate_tier, None);
        assert_eq!(live.text, "", "the body went out with the post");
        assert_eq!(live.gate_preview, "", "so did the teaser");
    }

    /// The security-review case (`docs/goal/ui/feed.md` § User actions): the user picks a tier, submits, and keeps typing the
    /// next post's text before the create confirms — without re-touching the
    /// picker. The audience must not silently widen to Public; it stays
    /// sticky whenever any other content field changed since the click, and
    /// its teaser stays with it.
    #[test]
    fn audience_and_teaser_stay_sticky_when_other_content_changes() {
        let sent = FeedComposeState {
            text: "first post".into(),
            gate_tier: Some("supporters".into()),
            gate_preview: "teaser".into(),
            ..Default::default()
        };
        let mut live = FeedComposeState {
            text: "second post".into(),
            ..sent.clone()
        };
        live.clear_sent(&sent);
        assert_eq!(
            live.gate_tier,
            Some("supporters".into()),
            "the audience must not clear to Public while the user keeps typing"
        );
        assert_eq!(
            live.gate_preview, "teaser",
            "the teaser stays with its sticky tier"
        );
    }

    /// A sold post's price fields are part of the audience answer for this
    /// rule too — a sticky sale without its price would leave the next
    /// gated submit unable to reconstruct what was being sold.
    #[test]
    fn a_sold_post_s_price_stays_sticky_alongside_the_sale() {
        let sell = SellComposeState {
            price: "5".into(),
            asking_price: "500".into(),
            subscribers_get_it_free: false,
        };
        let sent = FeedComposeState {
            text: "sold post".into(),
            sell: Some(sell.clone()),
            gate_preview: "teaser".into(),
            ..Default::default()
        };
        let mut live = FeedComposeState {
            attached_file: Some(photo("next.png")),
            ..sent.clone()
        };
        live.clear_sent(&sent);
        assert_eq!(
            live.sell,
            Some(sell),
            "the sale, price included, stays sticky"
        );
        assert_eq!(live.gate_preview, "teaser");
    }

    /// Same rule for the room answer: a room chosen for a post that is still
    /// in flight when the user attaches a file to the next one must not
    /// silently drop to Public.
    #[test]
    fn a_room_answer_stays_sticky_when_other_content_changes() {
        let sent = FeedComposeState {
            text: "for the room".into(),
            gate_room: Some("c7".repeat(32)),
            gate_preview: "teaser".into(),
            ..Default::default()
        };
        let mut live = FeedComposeState {
            tags: "new-tag".into(),
            ..sent.clone()
        };
        live.clear_sent(&sent);
        assert_eq!(live.gate_room, Some("c7".repeat(32)));
        assert_eq!(live.gate_preview, "teaser");
    }
}
