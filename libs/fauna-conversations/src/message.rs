use crate::address::TypedAddress;
use crate::html_markdown::html_to_markdown;
use crate::reactions::ReactionGroup;
use fauna_core::render::{
    RenderBlock, RenderDocument, markdown_to_document, plaintext_to_document,
};
use serde::{Deserialize, Serialize};

/// `Ord` is the id's plain string order — it carries no meaning (the *thread*
/// order is [`MessageId::channel_position`]'s seq + timestamp,
/// `store::history`), and exists only so a `MessageId` can key the
/// `BTreeMap`/`BTreeSet` that the at-rest history slice needs for a
/// byte-stable canonical encoding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl MessageId {
    /// Where a fauna-native message sits: the channel its id names (hex, as
    /// the id spells it) and its channel `seq` — the id's
    /// `conv:{channel_hex}:{seq}` shape. `None` for every other rail's id.
    ///
    /// `seq` is assigned by the nest inside the per-channel lock, so it is the
    /// one ordering every device computes identically — what the thread order
    /// sorts on (`store::history`) and what a read marker's position compares
    /// against (`conversation-read-state.md` § The read-marker record).
    pub fn channel_position(&self) -> Option<(&str, u64)> {
        let (channel_hex, seq) = self.0.strip_prefix("conv:")?.rsplit_once(':')?;
        Some((channel_hex, seq.parse().ok()?))
    }
}

// UniFFI 0.31 does not support derive(uniffi::Record) on tuple-struct newtypes;
// expose MessageId as a String custom type instead.
#[cfg(feature = "uniffi")]
uniffi::custom_type!(MessageId, String, {
    lower: |id| id.0.clone(),
    try_lift: |s| Ok(MessageId(s)),
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BodyFormat {
    PlainText,
    Markdown,
    Html,
}

/// Whole-message crypto badges. Content credentials are not one of them: a
/// C2PA verdict belongs to the bytes it was probed over, so it rides each
/// attachment ([`AttachmentSnapshot::c2pa`]), never the message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MessageBadges {
    pub encrypted: bool,
    pub signed: bool,
    pub verified: bool,
    pub content_warning: Option<String>,
}

/// A rendered attachment on a received/sent message. The unified shape every
/// app renders off (`docs/goal/ui/conversations.md` § Attachments):
/// `dm-attachment-image[i]` for `is_image`, else `dm-attachment-file[i]`.
///
/// `blob_hash` is the content handle — lowercase-hex BLAKE3 of the attachment's
/// plaintext bytes. A client resolves it to the real bytes through the shared
/// loader: [`crate::ConversationsManager::attachment_bytes`] (the in-memory
/// cache the inbound parse / send echo populated for nest-backed rails), and —
/// for FaunaMls — the nest `__conv` blob store GET + `decrypt_blob` (follow-on).
/// This replaces the old `uri` (a per-app cache path), so the *handle* is
/// uniform and *resolution* is the only per-rail / client-glue concern.
///
/// `c2pa` is the per-attachment C2PA-signed verdict, probed at receive time
/// from the decrypted bytes (`fauna_media::process::detect_c2pa`,
/// `attachments_to_inbound`). Real on every native app (FFI included —
/// `c2pa-detect` has shipped there since 2026-06-30) and a genuine `false`
/// stub only on web (wasm never enables `c2pa-detect`, to keep the heavy
/// `c2pa` tree out of the bundle) — `docs/goal/ui/conversations.md` §
/// Attachments "C2PA on-device".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttachmentSnapshot {
    pub blob_hash: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub is_image: bool,
    pub c2pa: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MessageSnapshot {
    pub message_id: MessageId,
    pub sender: TypedAddress,
    pub sender_display: String,
    /// The raw body source (markdown / plaintext / already-converted inbound
    /// HTML). **Retained as the canonical text source**, not a deletable render
    /// sibling: [`document`](Self::document) below is its render projection, and
    /// the thread-list snippet ([`crate::store::threads`] `summarize` →
    /// `fauna_core::markdown::markdown_to_plaintext(&body)`) reads it — the exact
    /// analogue of feed's retained `fauna_feed::PostSummary::body` (which feeds
    /// the quoted-post projection). No client re-parses `body` at render time —
    /// every shell walks `document` (render-model.md § D1). The one residual is a
    /// priority-#1 e2e drift: the `dm-message-text` automation read exposes the
    /// painted *document* text on linux/web/android/windows but the raw `body`
    /// on apple — unified by swapping apple's `automationValue` closures to a
    /// document-derived plaintext (`RenderDocument::to_plaintext`), not by
    /// deleting this field.
    pub body: String,
    /// The structured, semantic render document for this message — the one
    /// representation every app paints (render-model.md § D1/D2). Produced
    /// once by the manager via [`document_for_message`]: the text body (the
    /// `body_format` discriminant is consumed *inside* the producer) plus one
    /// [`RenderBlock::Attachment`] block per attachment folded in body order. No
    /// client re-parses the body or reads a sibling `body_format` / `attachments`
    /// field at render time — the document is the complete tree.
    pub document: RenderDocument,
    pub timestamp_ms: i64,
    pub subject_line: Option<String>, // Some(_) when this message changes subject
    pub badges: MessageBadges,
    pub reply_to: Option<MessageId>,
    /// Aggregated reactions on this message, ordered by first-add appearance.
    /// Empty until the manager folds reaction events in (task B3+).
    pub reactions: Vec<ReactionGroup>,
    /// Whether this message has been deleted. `false` until the manager
    /// processes a matching `ChannelMessageBody::Delete` event (task B3+).
    pub deleted: bool,
    /// Whether the local actor sent this message. Used to gate sender-only
    /// delete (task B3+). Set at snapshot construction time.
    pub is_own: bool,
    /// `Some(reference)` iff this message was taken down under a legal obligation
    /// (`moderation.md` § Categories & enforcement item 1 — the conversation twin
    /// of the post tombstone / `fauna_feed::QuotedPostView::legal_takedown_ref`).
    /// The nest **withheld** the sealed envelope, so [`document`](Self::document)
    /// / [`body`](Self::body) are empty; every app renders the shared
    /// `legalTakedownTombstone(reference)` (`fauna_core::obligation`) **in place
    /// of the bubble** — never a blank / failed-to-decrypt bubble. Best-effort at
    /// the relay only: a message a device synced *before* the takedown is deduped
    /// in ([`crate::store::threads::ThreadStore::append_message`] keeps the
    /// already-present real copy), which is the intended E2E boundary — the nest
    /// cannot recall a delivered message. Additive `#[serde(default,
    /// skip_serializing_if)]` so the at-rest `ChannelHistorySlice` blob stays
    /// byte-identical for a normal message (`version-compatibility.md`;
    /// no-user-data-loss).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown_ref: Option<String>,
    /// `Some(reason_token)` iff this nest's admin **blocked** this message after
    /// a report (`moderation.md` § Admin block of a reported item) — the report
    /// reason's wire token. For every viewer but the author the relay withheld
    /// the sealed envelope, so [`document`](Self::document) / [`body`](Self::body)
    /// are empty and every app renders the shared `adminBlockNotice(reason)`
    /// (`fauna_core::obligation::admin_block_notice`) **in place of the
    /// bubble** — the [`legal_takedown_ref`](Self::legal_takedown_ref) twin,
    /// with the same best-effort boundary (a copy synced before the block
    /// stands). On the author's own devices the nest serves the body (its
    /// attested author, decision 4): the message carries its body AND this
    /// reason, and the notice paints **beside the bubble**
    /// ([`admin_block_in_place`](Self::admin_block_in_place) tells the two
    /// apart). Additive, and absent from the at-rest blob for every other
    /// message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub admin_block_reason: Option<String>,
    /// Per-category content-label verdicts (`moderation.md` § Per-row badge
    /// data path, ratified 2026-07-16) — the client-side twin of
    /// `fauna_feed::PostSummary::labels`. Populated post-decrypt by
    /// [`crate::ConversationsManager::ingest_inbound_to_thread`] from the same
    /// `fauna_core::text_heuristic::classify_text` pass that feeds the
    /// [`fauna_client_moderation::LocalDetectionStore`] (this content is
    /// MLS-sealed at rest — the nest never sees it, so the client is the only
    /// place it can be classified). Additive `#[serde(default,
    /// skip_serializing_if)]` so the at-rest `ChannelHistorySlice` blob stays
    /// byte-identical for an unlabelled message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// This message's identity on the **account data plane** — what an app
    /// needs to report the T1 body-rendered observation when it paints the body
    /// (`account-data-plane.md` § The replica boundary → T1).
    ///
    /// `Some` exactly for a record the nest sequenced onto a content-scope feed
    /// — FaunaMls conversation messages today. Every other rail (SMTP, a bridge,
    /// the mock) is not on the plane at all, so `None` there is the honest
    /// answer, not a gap.
    ///
    /// It lives on the snapshot rather than behind a manager lookup because it
    /// must survive a restart with the message it names: the snapshot is what
    /// [`crate::store::history::ChannelHistorySlice`] persists, so a restored
    /// thread can still report observations for messages this device already
    /// held. Additive `#[serde(default, skip_serializing_if)]`, so the at-rest
    /// slice stays byte-identical for a message that carries none
    /// (`version-compatibility.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane_ref: Option<PlaneRef>,
    /// Whether **this viewer** may delete this message — the one fact behind
    /// `dm-message-delete-button`, so no app branches on a role: the viewer's
    /// own message on a thread that supports delete, or any message when the
    /// viewer is owner or admin of a governed end-to-end room
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any message
    /// — the mechanism* → *The affordance*). Derived on every projection by
    /// [`crate::ConversationsManager::thread_detail`], never stored: a role
    /// changes without the message changing, so a persisted answer would be a
    /// stale one. Additive (`serde` + UniFFI defaults), so the at-rest slice
    /// stays byte-identical and no app constructor has to name it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub can_delete: bool,
    /// Whether this is the viewer's **own message that its bridge never
    /// took**: the bridge that carried the room was disconnected with the
    /// message still queued, and the nest deleted the undeliverable item and
    /// stamped the Sent copy (`apps/bridges.md` § Phase G → *When the bridge
    /// stops serving*, consequence 3). Painted as `dm-message-undelivered`
    /// under the body, which stays in view so the user can send it again.
    /// Projected on every [`crate::ConversationsManager::thread_detail`] from
    /// the rail's answer ([`crate::backend::RailBackend::message_undelivered`]),
    /// like [`Self::can_delete`]: the stamp can land long after this device
    /// stored the message. Additive (`serde` + UniFFI defaults), so the
    /// at-rest slice stays byte-identical for every other message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub undelivered: bool,
    /// The fresh-IP warm-up's hold on the viewer's **own sent mail**: the nest
    /// accepted it but will deliver it only when the ramp allows
    /// (`mail-deliverability.md` § Enforcement at submission time). Painted as
    /// `dm-message-warmup-deferred` under the body, worded by
    /// [`warmup_deferred_text`] — which also drops it once the release time
    /// has passed. Projected on every
    /// [`crate::ConversationsManager::thread_detail`] from what the latest
    /// `Sent` read said, like [`Self::undelivered`]. Additive (`serde` + UniFFI
    /// defaults), so the at-rest slice stays byte-identical for every other
    /// message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub warmup_hold: Option<crate::backend::MailWarmupHold>,
    /// The perimeter scan verdict of a **received** mail message, as the
    /// lines its `dm-message-scan-result` row paints — ClamAV, rspamd, the
    /// action taken, each a [`fauna_core::localized::LocalizedText`] from the
    /// shared [`fauna_core::mail_scan::scan_result_lines`]
    /// (`mail-content-scanning.md` § User sees their own message's row).
    /// Empty — and the row absent — for every other message, and for a
    /// received one whose scan row has not been read yet or does not exist.
    /// Projected on every [`crate::ConversationsManager::thread_detail`] from
    /// the manager's cache, which
    /// [`crate::ConversationsManager::refresh_message_scan_results`] fills
    /// when the thread opens; never stored, so the at-rest slice stays
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub scan_result_lines: Vec<fauna_core::localized::LocalizedText>,
    /// The "Delivered to: <address> [<label>]" tag of a **received** mail
    /// message — the alias it arrived on, as its `dm-message-delivered-to-alias`
    /// paints it ([`delivered_to_text`]; `mail-aliases.md` § Label). `None` —
    /// and the tag absent — for every other message, for one with no
    /// delivery stamp, and until the account's alias rows have been read.
    /// Projected on every [`crate::ConversationsManager::thread_detail`] from
    /// the stamps kept at ingest and the rows
    /// [`crate::ConversationsManager::refresh_delivered_aliases`] reads when
    /// the thread opens; never stored, so the at-rest slice stays
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub delivered_to: Option<String>,
    /// The nest's withholding of the viewer's **own** fauna-native message —
    /// its legal takedown or its admin block, as the author's moderation
    /// queue row records it — painted as `dm-message-withheld-badge` beside
    /// the body, whose tap opens that row's appeal (`moderation.md` § Legal
    /// takedown → *App render*, the fourth surface). The author's devices
    /// keep the real copy and never receive the tombstone, so without it the
    /// thread says nothing. Matched by record digest: the queue row's
    /// `content_id` is this message's [`PlaneRef::record_digest`]. Projected
    /// on every [`crate::ConversationsManager::thread_detail`] from the rows
    /// [`crate::ConversationsManager::refresh_withheld_messages`] reads when
    /// the thread opens; never stored, so the at-rest slice stays
    /// byte-identical — and never through
    /// [`legal_takedown_ref`](Self::legal_takedown_ref) (which collapses the
    /// bubble) or [`admin_block_reason`](Self::admin_block_reason) (which
    /// names one ground only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub withheld: Option<WithheldOwnMessage>,
}

/// Why the nest withholds a message from the conversation — the two
/// enforcement acts that write the author's queue row for a conversation
/// record (`moderation.md` § Legal takedown → *Conversations*, § Admin block
/// of a reported item).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum WithheldGround {
    /// A legal takedown (`ObligationAction::TakenDown`).
    TakenDown,
    /// An admin block after a report (`ObligationAction::Blocked`).
    Blocked,
}

impl WithheldGround {
    /// The ground an obligation-action discriminant names, `None` for every
    /// action that withholds nothing from a conversation.
    pub fn from_obligation_action(action: u8) -> Option<Self> {
        use fauna_core::obligation::ObligationAction;
        if action == ObligationAction::TakenDown as u8 {
            Some(Self::TakenDown)
        } else if action == ObligationAction::Blocked as u8 {
            Some(Self::Blocked)
        } else {
            None
        }
    }
}

/// One of the viewer's own conversation records the nest withholds, as the
/// author's queue row (`fauna.moderation.actions`) names it — what the rail
/// answers [`crate::ConversationsManager::refresh_withheld_messages`] with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithheldRecord {
    /// The queue row's `content_id`: the record digest, lowercase hex.
    pub content_id: String,
    pub ground: WithheldGround,
    /// The handle domain of the nest whose admin decided, on a row another
    /// nest delivered; `None` for this nest's own decision.
    pub issuer_nest: Option<String>,
    /// The row's microsecond timestamp — the newest row for a record wins.
    pub timestamp: i64,
}

impl WithheldRecord {
    /// The record a queue row names, `None` for a row that is not a
    /// conversation record's withholding (every post row, every classifier
    /// row) — the one filter, so no rail glue re-derives it.
    pub fn from_queue_row(
        content_type: &str,
        content_id: &str,
        action: u8,
        issuer_nest: Option<String>,
        timestamp: i64,
    ) -> Option<Self> {
        if content_type != "conversation" || content_id.is_empty() {
            return None;
        }
        Some(Self {
            content_id: content_id.to_ascii_lowercase(),
            ground: WithheldGround::from_obligation_action(action)?,
            issuer_nest: issuer_nest.filter(|nest| !nest.is_empty()),
            timestamp,
        })
    }
}

/// The projection behind `dm-message-withheld-badge` (see
/// [`MessageSnapshot::withheld`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WithheldOwnMessage {
    /// The matched queue row's `content_id` — what the appeal names.
    pub content_id: String,
    pub ground: WithheldGround,
    /// The deciding nest's handle domain, on a row another nest delivered.
    pub issuer_nest: Option<String>,
    /// The badge's words, side by side: the ground — the block notice with
    /// its reason when this copy carries one, so the bubble paints ONE notice
    /// and never the badge beside a second — then the shared "decided by
    /// {nest}" suffix when another nest decided, then the appeal entry. The
    /// ground line's argument is itself a key: resolve nested.
    pub lines: Vec<fauna_core::localized::LocalizedText>,
}

impl WithheldOwnMessage {
    /// The badge for `record` on a message whose copy carries
    /// `admin_block_reason` (the author-exempt fetch of a blocked record,
    /// painted beside the body).
    pub fn new(record: &WithheldRecord, admin_block_reason: Option<&str>) -> Self {
        use fauna_core::localized::LocalizedText;
        use fauna_core::obligation::{
            ObligationAction, admin_block_notice, obligation_action_label,
        };
        let ground = match (record.ground, admin_block_reason) {
            (WithheldGround::Blocked, Some(reason)) => admin_block_notice(reason),
            (WithheldGround::Blocked, None) => {
                obligation_action_label(ObligationAction::Blocked as u8)
            }
            (WithheldGround::TakenDown, _) => {
                obligation_action_label(ObligationAction::TakenDown as u8)
            }
        };
        let mut lines = vec![ground];
        if let Some(nest) = &record.issuer_nest {
            lines.push(LocalizedText::key_arg(
                fauna_client_moderation::ISSUED_BY_KEY,
                "nest",
                nest.clone(),
            ));
        }
        lines.push(LocalizedText::key(WITHHELD_APPEAL_KEY));
        Self {
            content_id: record.content_id.clone(),
            ground: record.ground,
            issuer_nest: record.issuer_nest.clone(),
            lines,
        }
    }

    /// Whether this badge already says what the beside-body block notice
    /// would — an app then paints the badge alone.
    pub fn carries_block_notice(&self) -> bool {
        self.ground == WithheldGround::Blocked
    }
}

/// The appeal entry's word — the queue's own `appeal-button` label, since the
/// badge opens the same appeal.
pub const WITHHELD_APPEAL_KEY: &str = "moderation.appeal";

impl MessageSnapshot {
    /// Whether the admin-block notice stands **in place of** the bubble — the
    /// relay withheld the body, so there is nothing else to paint. `false` for
    /// the author's own blocked message, which keeps its body and paints the
    /// notice beside it (`moderation.md` § Admin block of a reported item,
    /// decision 4), and for every unblocked message.
    pub fn admin_block_in_place(&self) -> bool {
        self.admin_block_reason.is_some() && self.document.blocks.is_empty()
    }
}

/// The `dm-message-delivered-to-alias` line for a received message's kept
/// delivery stamps ([`fauna_mail::aliases::delivered::delivery_stamps`]) and
/// the account's alias rows — the one wording every app paints
/// (`mail-aliases.md` § Label): the alias's own label, or, when it has none,
/// the alias kind plus its pattern (the caught domain, for a catch-all; the
/// kind alone for a row deleted since delivery). `None` when the shared
/// resolver resolves nothing.
pub fn delivered_to_text(
    stamps: &[u8],
    rows: &[fauna_mail::aliases::delivered::AliasLabelRow],
) -> Option<String> {
    use fauna_i18n::strings::mail_aliases as t;
    use fauna_mail::aliases::delivered::{DeliveredAliasKind, delivered_alias};
    let hit = delivered_alias(stamps, rows)?;
    let label = if !hit.label.is_empty() {
        hit.label
    } else {
        let (kind, detail) = match &hit.kind {
            DeliveredAliasKind::Exact => (t::KIND_EXACT, hit.pattern.clone()),
            DeliveredAliasKind::Subaddress => (t::KIND_SUBADDRESS, hit.pattern.clone()),
            DeliveredAliasKind::WildcardPrefix => (t::KIND_WILDCARD, hit.pattern.clone()),
            DeliveredAliasKind::Disposable => (t::KIND_DISPOSABLE, hit.pattern.clone()),
            DeliveredAliasKind::Catchall { domain } => (t::KIND_CATCHALL, Some(domain.clone())),
        };
        match detail {
            Some(detail) => format!("{kind} {detail}"),
            None => kind.to_string(),
        }
    };
    Some(t::delivered_to(&hit.address, &label))
}

/// The `dm-message-warmup-deferred` line for a held sent message — the one
/// wording every app paints: the release time in the viewer's local clock and
/// the queue position. `None` once `now_secs` has reached the release, so a
/// thread left open past midnight UTC stops saying the message waits.
///
/// Native only (`local-clock`); exported over UniFFI to the native apps beside
/// [`crate::typed_address_display`] (same `client-display` gate, so the Go
/// mail-bridge's FFI build drops it). The web leg has no OS timezone database
/// in wasm, so it calls [`warmup_deferred_text_at`] with the release time it
/// formatted itself.
#[cfg(feature = "local-clock")]
#[cfg_attr(all(feature = "uniffi", feature = "client-display"), uniffi::export)]
pub fn warmup_deferred_text(
    hold: &crate::backend::MailWarmupHold,
    now_secs: i64,
) -> Option<String> {
    warmup_deferred_text_at(
        hold,
        now_secs,
        &fauna_core::format::format_unix_local(hold.release_at_secs),
    )
}

/// [`warmup_deferred_text`] with the release time already formatted
/// (`release_label`) — everything but the clock, so the wasm leg shares the
/// wording, the count and the release-has-come rule while the web app formats
/// only the date.
pub fn warmup_deferred_text_at(
    hold: &crate::backend::MailWarmupHold,
    now_secs: i64,
    release_label: &str,
) -> Option<String> {
    use fauna_i18n::strings::conversations::unified;
    if now_secs >= hold.release_at_secs {
        return None;
    }
    let until = unified::dm_message_warmup_deferred(release_label);
    let queue = match hold.messages_ahead {
        0 => unified::DM_MESSAGE_WARMUP_QUEUE_NEXT.to_string(),
        1 => unified::DM_MESSAGE_WARMUP_QUEUE_ONE.to_string(),
        n => unified::dm_message_warmup_queue(&n.to_string()),
    };
    Some(format!("{until} {queue}"))
}

#[cfg(all(test, feature = "local-clock"))]
mod warmup_deferred_text_tests {
    use super::warmup_deferred_text;
    use crate::backend::MailWarmupHold;
    use fauna_i18n::strings::conversations::unified;

    const RELEASE: i64 = 1_700_006_400;

    fn text(messages_ahead: u32, now: i64) -> Option<String> {
        warmup_deferred_text(
            &MailWarmupHold {
                release_at_secs: RELEASE,
                messages_ahead,
            },
            now,
        )
    }

    #[test]
    fn says_the_release_time_then_the_queue_position() {
        let until =
            unified::dm_message_warmup_deferred(&fauna_core::format::format_unix_local(RELEASE));
        assert_eq!(
            text(0, RELEASE - 1),
            Some(format!("{until} {}", unified::DM_MESSAGE_WARMUP_QUEUE_NEXT))
        );
        assert_eq!(
            text(1, RELEASE - 1),
            Some(format!("{until} {}", unified::DM_MESSAGE_WARMUP_QUEUE_ONE))
        );
        assert_eq!(
            text(7, RELEASE - 1),
            Some(format!("{until} {}", unified::dm_message_warmup_queue("7")))
        );
    }

    #[test]
    fn says_nothing_once_the_release_has_come() {
        assert_eq!(text(2, RELEASE), None);
        assert_eq!(text(2, RELEASE + 60), None);
    }
}

#[cfg(test)]
mod warmup_deferred_text_at_tests {
    use super::warmup_deferred_text_at;
    use crate::backend::MailWarmupHold;
    use fauna_i18n::strings::conversations::unified;

    const RELEASE: i64 = 1_700_006_400;

    /// The web leg's door: the caller's own release label, the shared count
    /// and the shared release-has-come rule.
    #[test]
    fn words_the_callers_release_label_and_falls_silent_at_the_release() {
        let hold = MailWarmupHold {
            release_at_secs: RELEASE,
            messages_ahead: 3,
        };
        assert_eq!(
            warmup_deferred_text_at(&hold, RELEASE - 1, "15 Nov, 01:00"),
            Some(format!(
                "{} {}",
                unified::dm_message_warmup_deferred("15 Nov, 01:00"),
                unified::dm_message_warmup_queue("3")
            ))
        );
        assert_eq!(warmup_deferred_text_at(&hold, RELEASE, "x"), None);
    }
}

/// One inbound cooperative-delete claim, as the rail folded it
/// (`conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism*): who posted the delete, where, and **the role that author
/// held under the policy the delete was made under**. The verdict is recorded
/// here at fold time and the projection reads it; it never re-asks the current
/// policy, so a later demotion cannot un-delete and a later appointment cannot
/// back-date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteClaim {
    /// The delete's **authenticated** author — never a self-asserted field.
    pub claimant: fauna_core::identity::ActorId,
    /// The delete record's own position in the room log, when the rail knows it.
    pub delete_seq: Option<u64>,
    /// The claimant's role under the policy the delete was made under; `None`
    /// when the rail has none to vouch for (a policy-less room, another rail, a
    /// delete from an epoch whose group context is no longer held) — which
    /// leaves the claim to the sender match alone.
    pub role: Option<crate::room::RoomRole>,
}

impl DeleteClaim {
    /// Whether this claim tombstones a message `sender` sent: the sender match
    /// (the floor, kept), or a governing role recorded at fold (the second
    /// admission beside it).
    pub fn admits(&self, sender: Option<fauna_core::identity::ActorId>) -> bool {
        Some(self.claimant) == sender || self.role.is_some_and(|r| r.is_admin_or_owner())
    }
}

/// Where one message sits on the account data plane: the content scope whose
/// feed carried it, and the record it is on that feed.
///
/// Deliberately two *strings* rather than the typed `ContentScope` /
/// `ContentHash`: this crate is the conversations model, shared with the Go
/// mail bridge and every app's binding, and it has no business depending on the
/// sync engine's types. The one consumer that needs typed values —
/// `fauna_sync_engine::observation_intake::Observation` — parses them at its own
/// seam (`Observation::parse`), so no app hand-parses hex or hand-builds a
/// scope string. [`crate::plane`] is where this pair is derived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PlaneRef {
    /// The canonical content-scope string — `content:conv:<channel-hex>` for a
    /// conversation message.
    pub scope: String,
    /// The record CID's **digest**, lowercase hex. The codec half is not
    /// carried because it is not a variable: every record on this plane is
    /// dag-cbor-coded, the same assumption the content-scope walk makes when it
    /// rebuilds a CID from the feed row's `path_hash`
    /// (`fauna_sync_engine::content_scope_plane`).
    pub record_digest: String,
}

/// Build the structured [`RenderDocument`] for a message body, choosing the
/// `fauna_core::render` producer from `format` (render-model.md § D1). This is
/// the single place the `body_format` discriminant is consumed — it is folded
/// into [`document_for_message`], so no client re-derives body structure (nor
/// reads a sibling `body_format` field) at render time.
///
/// The inbound-HTML path is **not** a separate producer: a raw HTML body is
/// converted to markdown by the shared [`html_to_markdown`] (html-mail.md —
/// markdown-as-interchange) and then runs through `markdown_to_document`, so it
/// reaches the one model an authored markdown message does (priority #2/#4 — the
/// converter is not duplicated). In practice received HTML mail is *already*
/// converted to markdown upstream by [`html_markdown::inbound_mail_body`] and
/// arrives stamped [`BodyFormat::Markdown`], so the [`BodyFormat::Html`] arm is a
/// defensive path for any rail that ever stores a raw HTML body.
pub(crate) fn document_for_body(body: &str, format: BodyFormat) -> RenderDocument {
    match format {
        BodyFormat::Markdown => markdown_to_document(body),
        BodyFormat::PlainText => plaintext_to_document(body),
        BodyFormat::Html => markdown_to_document(&html_to_markdown(body)),
    }
}

/// Build the **complete** [`RenderDocument`] for a message — the text body (via
/// [`document_for_body`], which consumes the `format` discriminant) plus one
/// [`RenderBlock::Attachment`] block per attachment folded in body order, **after**
/// the text (the `[body] → [attachments]` order all 7 apps already render —
/// render-model.md § D1/D2: embeds are first-class blocks, not sibling fields).
///
/// This is the single producer every `MessageSnapshot` construction site calls, so
/// `document` is the complete tree every reader paints and the snapshot carries no
/// sibling `body_format` / `attachments` render field. The `Attachment` node carries
/// the same six [`AttachmentSnapshot`] fields (a strict superset — priority #4); the
/// client still resolves bytes through its existing `attachment_bytes(blob_hash)`
/// loader (resolution unchanged; only placement moved into the document).
pub(crate) fn document_for_message(
    body: &str,
    format: BodyFormat,
    attachments: &[AttachmentSnapshot],
) -> RenderDocument {
    let mut document = document_for_body(body, format);
    for att in attachments {
        document.blocks.push(RenderBlock::Attachment {
            blob_hash: att.blob_hash.clone(),
            filename: att.filename.clone(),
            mime_type: att.mime_type.clone(),
            size_bytes: att.size_bytes,
            is_image: att.is_image,
            c2pa: att.c2pa,
        });
    }
    document
}

/// The [`AttachmentSnapshot`]s carried by a render document's
/// [`RenderBlock::Attachment`] blocks, in body order — the Rust counterpart of
/// each app's `documentAttachments` walk (render-model.md § D2: attachments
/// are first-class blocks, not a sibling field). The inverse of the fold
/// [`document_for_message`] performs; for any Rust consumer (or test) that wants
/// the attachment list a client paints from `document`.
pub fn attachment_blocks(document: &RenderDocument) -> Vec<AttachmentSnapshot> {
    document
        .blocks
        .iter()
        .filter_map(|b| match b {
            RenderBlock::Attachment {
                blob_hash,
                filename,
                mime_type,
                size_bytes,
                is_image,
                c2pa,
            } => Some(AttachmentSnapshot {
                blob_hash: blob_hash.clone(),
                filename: filename.clone(),
                mime_type: mime_type.clone(),
                size_bytes: *size_bytes,
                is_image: *is_image,
                c2pa: *c2pa,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::render::{Inline, RenderBlock};

    #[test]
    fn markdown_body_parses_inline_emphasis() {
        let doc = document_for_body("**bold**", BodyFormat::Markdown);
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Bold {
                    inlines: vec![Inline::Text {
                        text: "bold".into()
                    }]
                }]
            }]
        );
    }

    #[test]
    fn plaintext_body_does_not_parse_markdown() {
        // A plaintext rail (a bridge) must render `**x**` literally, not bold.
        let doc = document_for_body("**x**", BodyFormat::PlainText);
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text {
                    text: "**x**".into()
                }]
            }]
        );
    }

    #[test]
    fn attachments_append_as_blocks_after_the_body_in_order() {
        // D1/D2: `document_for_message` produces the text body then appends one
        // `Attachment` block per attachment after it, in order, carrying the same
        // six fields (strict superset). No attachments → just the text body.
        assert_eq!(
            document_for_message("hi", BodyFormat::Markdown, &[]).blocks,
            document_for_body("hi", BodyFormat::Markdown).blocks,
        );

        let attachments = vec![
            AttachmentSnapshot {
                blob_hash: "aa01".into(),
                filename: "pic.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 10,
                is_image: true,
                c2pa: false,
            },
            AttachmentSnapshot {
                blob_hash: "bb02".into(),
                filename: "doc.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 20,
                is_image: false,
                c2pa: true,
            },
        ];
        let doc = document_for_message("hi", BodyFormat::Markdown, &attachments);
        assert_eq!(
            doc.blocks,
            vec![
                RenderBlock::Paragraph {
                    inlines: vec![Inline::Text { text: "hi".into() }]
                },
                RenderBlock::Attachment {
                    blob_hash: "aa01".into(),
                    filename: "pic.png".into(),
                    mime_type: "image/png".into(),
                    size_bytes: 10,
                    is_image: true,
                    c2pa: false,
                },
                RenderBlock::Attachment {
                    blob_hash: "bb02".into(),
                    filename: "doc.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size_bytes: 20,
                    is_image: false,
                    c2pa: true,
                },
            ]
        );
    }

    #[test]
    fn html_body_routes_through_html_to_markdown() {
        // Inbound HTML must reach the same model as authored markdown, via the
        // shared converter — `<strong>` becomes a Bold inline, not raw tag text.
        let doc = document_for_body("<p><strong>hi</strong></p>", BodyFormat::Html);
        assert_eq!(
            doc.blocks,
            vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Bold {
                    inlines: vec![Inline::Text { text: "hi".into() }]
                }]
            }]
        );
    }
}

#[cfg(test)]
mod withheld_tests {
    use super::*;

    fn row(content_type: &str, action: u8, issuer: Option<&str>) -> Option<WithheldRecord> {
        WithheldRecord::from_queue_row(
            content_type,
            "ABCD01",
            action,
            issuer.map(str::to_string),
            42,
        )
    }

    #[test]
    fn only_a_conversation_records_withholding_is_kept() {
        let taken = row("conversation", 7, None).expect("a takedown row");
        assert_eq!(taken.ground, WithheldGround::TakenDown);
        assert_eq!(
            taken.content_id, "abcd01",
            "matched against the lowercase digest"
        );
        assert_eq!(
            row("conversation", 10, None).map(|r| r.ground),
            Some(WithheldGround::Blocked)
        );
        assert_eq!(row("post", 7, None), None, "a post row is not a message's");
        assert_eq!(
            row("conversation", 6, None),
            None,
            "a label withholds nothing"
        );
        assert_eq!(
            row("conversation", 7, Some("")).and_then(|r| r.issuer_nest),
            None,
            "an empty domain names no nest"
        );
    }

    #[test]
    fn the_badge_names_the_ground_the_issuer_and_the_appeal() {
        let record = row("conversation", 7, Some("issuer.example")).unwrap();
        let badge = WithheldOwnMessage::new(&record, None);
        let keys: Vec<&str> = badge.lines.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "moderation.action.taken_down",
                fauna_client_moderation::ISSUED_BY_KEY,
                WITHHELD_APPEAL_KEY
            ]
        );
        assert_eq!(badge.lines[1].args["nest"], "issuer.example");
        assert!(!badge.carries_block_notice());
    }

    #[test]
    fn a_blocked_copy_carrying_its_notice_gets_one_notice_with_the_reason() {
        let record = row("conversation", 10, None).unwrap();
        let synced_before = WithheldOwnMessage::new(&record, None);
        assert_eq!(synced_before.lines[0].key, "moderation.action.blocked");
        let beside = WithheldOwnMessage::new(&record, Some("harassment"));
        assert_eq!(
            beside.lines[0],
            fauna_core::obligation::admin_block_notice("harassment")
        );
        assert_eq!(beside.lines.len(), 2, "the notice, then the appeal entry");
        assert!(beside.carries_block_notice());
    }
}
