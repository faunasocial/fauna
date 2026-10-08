use crate::address::TypedAddress;
use crate::message::MessageId;
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// A staged compose attachment (`attachment-button` → `add_attachment`). Light
/// by design — the **bytes never live in the snapshot**: `add_attachment` hashes
/// the picked file's bytes, caches them under `blob_hash` in the manager's
/// attachment store, and stages only this metadata, so an observed
/// `ComposeState` stays cheap to diff over UniFFI even with a multi-MB image
/// staged. `send` re-resolves the bytes from the store by `blob_hash`
/// (`docs/goal/ui/conversations.md` § Attachments).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttachmentDraft {
    pub blob_hash: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub is_image: bool,
}

/// Guess a MIME type from a filename's extension, for a native file-picker
/// attach flow whose OS dialog hands back only a path (unlike web, where the
/// browser's `File.type` supplies it natively). Case-insensitive; an
/// unrecognized or missing extension falls back to `"application/octet-stream"`
/// (`docs/goal/ui/conversations.md` § Attachments — outbound staging).
pub fn guess_mime_type(filename: &str) -> &'static str {
    fauna_core::share::content_type_for_filename(filename)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RecipientPickerState {
    pub raw_input: String,
    pub chips: Vec<TypedAddress>,
    pub suggestions: Vec<TypedAddress>,
    pub resolve_state: ResolveState,
    /// The address the async backend probe (`ConversationsManager::resolve_recipient`)
    /// resolved `raw_input` to, when `resolve_state == Resolved`. Carries the
    /// *resolved* rail/identity — e.g. a 64-hex actor id promoted to
    /// `TypedAddress::Fauna` by the FaunaMls key-package probe — so committing
    /// the chip uses it rather than the format-only re-parse (which cannot
    /// produce `Fauna`). Cleared whenever `raw_input` changes or a chip commits.
    pub resolved: Option<TypedAddress>,
    /// Whether the room about to be created seats the user's **home nest** as a
    /// member — `recipient-picker-home-nest-toggle`'s `checked` attribute. The
    /// nest is never a chip, so this is how it joins the member set: checked,
    /// the room derives `community` (`conversation-rooms.md` § The three
    /// classes) and the first send founds one instead of an end-to-end group.
    /// A class is chosen only by choosing members, never by a flag on an
    /// existing room — this is the member choice, made before the room exists.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub include_home_nest: bool,
}

/// Decoded by hand (below): a state a newer build writes reads as
/// [`Self::Idle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ResolveState {
    #[default]
    Idle,
    Resolving,
    Resolved,
    /// The address was probed but no rail claimed it — a syntactically-valid
    /// recipient that isn't reachable (e.g. a Fauna actor id with no key
    /// package on the nest). Distinct from `Error` (a transport/probe failure)
    /// per `docs/goal/ui/conversations.md` § Errors & edge cases
    /// (`recipient-resolve-status`: resolving / resolved / error / not-found).
    NotFound,
    Error,
}

impl ResolveState {
    /// The state a serde **variant name** names — `Idle` for a name this build
    /// does not know, never a false state. The one projection both the at-rest
    /// decode below and [`recipient_resolve_status_from_variant`] read.
    pub fn from_variant_name(name: &str) -> Self {
        match name {
            "Resolving" => ResolveState::Resolving,
            "Resolved" => ResolveState::Resolved,
            "NotFound" => ResolveState::NotFound,
            "Error" => ResolveState::Error,
            _ => ResolveState::Idle,
        }
    }
}

/// The open arm of a transient field (`transport.md` § Schema and
/// forward-compat discipline → *Rule 3 in full*). A `ResolveState` rests only
/// inside the drafts blob, where every writer stores `Idle` (the drafts
/// store's `persistable_picker`: a probe is not running after a restart), so a
/// state a newer build might write there reads as `Idle` — the value this
/// build writes back — and nothing it could carry is lost: no build re-emits a
/// decoded state.
impl<'de> Deserialize<'de> for ResolveState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        Ok(Self::from_variant_name(&name))
    }
}

/// The `recipient-resolve-status` element's render-ready view: the kebab-case
/// state `token` (`idle` / `resolving` / `resolved` / `not-found` / `error` —
/// the vocabulary `docs/goal/ui/conversations.md` § Errors & edge cases
/// ratifies, driving the element's `state` automation attribute) plus the
/// status `label` each app resolves through its own i18n pipeline
/// (`conversations.unified.recipient_resolve_*`; `None` for `Idle`, which
/// renders empty — the [`fauna_core::format`] `Option`-as-signal shape).
///
/// Shared so the state→(token, label) map can't drift per-app — previously
/// hand-rolled in all five apps (linux/web/android full 5-arm copies;
/// windows split across an enum→token hop and a token→text hop; apple carried
/// TWO copies, and its folder share-sheet copy covered only 3 arms, so a
/// `resolved`/`not-found` recipient rendered a blank status — the drift this
/// lift retires, priority #4). Styling of the status line stays per-app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ResolveStatusView {
    pub token: String,
    pub label: Option<fauna_core::localized::LocalizedText>,
}

/// [`ResolveState`] → its [`ResolveStatusView`] (token + status label). See the
/// view's doc for the contract; native apps pass their typed snapshot state.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn recipient_resolve_status(state: ResolveState) -> ResolveStatusView {
    use fauna_core::localized::LocalizedText;
    let (token, key) = match state {
        ResolveState::Idle => ("idle", None),
        ResolveState::Resolving => (
            "resolving",
            Some("conversations.unified.recipient_resolve_resolving"),
        ),
        ResolveState::Resolved => (
            "resolved",
            Some("conversations.unified.recipient_resolve_resolved"),
        ),
        ResolveState::NotFound => (
            "not-found",
            Some("conversations.unified.recipient_resolve_not_found"),
        ),
        ResolveState::Error => (
            "error",
            Some("conversations.unified.recipient_resolve_error"),
        ),
    };
    ResolveStatusView {
        token: token.to_string(),
        label: key.map(LocalizedText::key),
    }
}

/// [`recipient_resolve_status`] keyed by the serde **variant name** (`"Resolved"`,
/// `"NotFound"`, …) instead of the typed enum — for consumers that hold the
/// snapshot as JSON (web reads `recipient_picker.resolve_state` off the wasm
/// snapshot as a string; the same take-the-serde-name shape as
/// `fauna_core::format::dns_verdict_label`). An unknown/absent name degrades to
/// the `Idle` view (empty status), never a false state.
pub fn recipient_resolve_status_from_variant(name: &str) -> ResolveStatusView {
    recipient_resolve_status(ResolveState::from_variant_name(name))
}

/// Decoded by hand (below): a state a newer build writes reads as
/// [`Self::Idle`].
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[derive(Default)]
pub enum SendState {
    #[default]
    Idle,
    Sending,
    /// A send the backend rejected. `reason` is a [`LocalizedText`] — the same
    /// carrier `ConversationsSnapshot::error` uses, because both feed the one
    /// `error-message` element and `conversations.md` § Architectural rules 3
    /// ("never hardcode English") governs the whole element, not one of its two
    /// producers. Always the key `conversations.unified.error_send` with the
    /// backend's own detail as `{message}`: sends have a single producer
    /// (`ConversationsManager::send`, which `send_new_thread` tail-calls), so
    /// unlike the page error there is no per-gesture key to choose.
    Failed {
        reason: LocalizedText,
    },
}

/// The open arm of a transient field (`transport.md` § Schema and
/// forward-compat discipline → *Rule 3 in full*). A `SendState` rests only
/// inside the drafts blob, where every writer stores `Idle` (the drafts
/// store's `persistable`: a restored `Sending` would disable the send button
/// with no gesture that clears it), so a state a newer build might write there
/// — a new variant, unit or carrying data — reads as `Idle`, the value this
/// build writes back. No build re-emits a decoded state, so a carrying arm
/// would carry nothing anyone reads: the arm collapses, and needs no variant
/// that every app would have to render. The known forms decode as derived.
impl<'de> Deserialize<'de> for SendState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use fauna_cbor::Value;
        use serde::de::Error as _;

        #[derive(Deserialize)]
        struct FailedFields {
            reason: LocalizedText,
        }

        let value = Value::deserialize(d)?;
        Ok(match &value {
            Value::String(name) if name == "Idle" => SendState::Idle,
            Value::String(name) if name == "Sending" => SendState::Sending,
            Value::Map(map) if map.len() == 1 && map.contains_key("Failed") => {
                let bytes =
                    fauna_cbor::encode_canonical(&map["Failed"]).map_err(D::Error::custom)?;
                let fields: FailedFields =
                    fauna_cbor::decode_strict(&bytes).map_err(D::Error::custom)?;
                SendState::Failed {
                    reason: fields.reason,
                }
            }
            _ => SendState::Idle,
        })
    }
}

/// The one i18n key every send failure carries (see [`SendState::Failed`]).
pub const SEND_FAILURE_KEY: &str = "conversations.unified.error_send";

impl SendState {
    /// Build [`SendState::Failed`] from a backend rejection detail — the single
    /// place the send-failure i18n key is chosen, so the real producer and the
    /// e2e injection seam cannot drift apart on it (and no caller has to know
    /// the key to stamp a failure).
    pub fn failed(detail: impl Into<String>) -> Self {
        SendState::Failed {
            reason: LocalizedText::key_arg(SEND_FAILURE_KEY, "message", detail),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ComposeState {
    pub body_draft: String,
    pub subject_draft: Option<String>, // Some("") = "+ topic" expanded but blank
    pub attachments: Vec<AttachmentDraft>,
    pub reply_to: Option<MessageId>,
    /// The To/Cc of the reply about to be sent — an editable per-reply draft,
    /// seeded by `dm-reply-button` (sender-only) or `dm-reply-all-button`
    /// (every thread participant but self), then editable via the always-visible
    /// "To" line (`conversations.md` § Participants vs reply recipients).
    /// Populated only on rails whose `ThreadCapabilities.supports_recipient_selection`
    /// is true (mail); empty elsewhere — and when empty the SMTP backend falls
    /// back to the historical participants-minus-self, so a plain send (no reply
    /// seed) still addresses the thread. Removing a chip drops a recipient from
    /// *this reply only* — thread history is untouched.
    pub reply_recipients: Vec<TypedAddress>,
    pub recipient_picker: Option<RecipientPickerState>, // Some only for new_thread_compose
    pub send_state: SendState,
    /// The list-send view when this compose's one mail recipient is one of the
    /// account's own mailing lists (`mail-mass-mailing.md` § Composing a list
    /// message) — `None` for every other compose. Derived by
    /// [`crate::ConversationsManager::refresh_list_send`] from the nest's
    /// figures, so it never rests with the draft (`store::drafts::persistable`
    /// writes it `None`): a restored draft re-derives.
    /// Defaulted for UniFFI so the shells' positional test constructors stay
    /// valid as the record grows.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub list_send: Option<crate::list_send::ListSendView>,
}

/// What the compose bar's reply preview (`dm-reply-preview`) shows for the
/// reply in progress: who wrote the answered message and a plain-text excerpt
/// of it (`conversations.md` § Layout & flow — "a reply in progress shows the
/// message you are answering"). Built once, by
/// [`crate::ConversationsManager::reply_preview`], so the apps stop deriving
/// three different things (the body, the sender's name, the bare message id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReplyPreview {
    /// The answered message's sender, as the bubble names it (`dm-sender`).
    pub sender_display: String,
    /// The answered message as plain text — markdown stripped, bounded like a
    /// list row's snippet and cut back to a word boundary.
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AddParticipantState {
    pub target_thread_id: crate::thread::ThreadId,
    pub picker: RecipientPickerState,
    /// Does confirming this overlay reach the wire? `true` exactly when the
    /// target is a bound FaunaMls **group** — the one `(rail, flavor)` that
    /// adds *in place*, opening the add commit by fetching the newcomer's key
    /// package. Every other pairing forks or edits the snapshot only: a
    /// FaunaMls 1:1 forks a new group whose first *send* bootstraps it, and a
    /// non-FaunaMls rail has no wire membership op at all.
    ///
    /// **Carried for the offline gate, and that is the whole reason it
    /// exists.** The two arms are "needs a nest" and "issues nothing", which
    /// is a class difference no undiscriminated state could state — so a
    /// client gating `add-participant-confirm` on
    /// `fauna.conversations.keypackage.fetch` unconditionally would grey a
    /// fork that works perfectly offline, exactly the over-claim
    /// `../../../docs/goal/architecture/account-data-plane.md`
    /// § The offline-mutation contract → *How a surface asks* forbids. Every
    /// app inherits the one discriminant instead of re-deriving it: tui reads
    /// it straight into `Action::ConfirmAddParticipant { in_place_mls_group }`,
    /// and the UniFFI apps read the same field off the snapshot (priority #2 —
    /// the rail/flavor test is shared logic, not per-app glue).
    ///
    /// **Not the authority for the wire op.** [`ConversationsManager::
    /// confirm_add_participant`] re-derives the same test from the live thread
    /// and acts on *that*, so a snapshot a client held too long can never cause
    /// a wrong commit — the field only decides what the paint offers. The two
    /// agree by construction: a thread's `rail`/`flavor` are set once, when the
    /// store builds it (`store/threads.rs`), and are never reassigned, so they
    /// cannot drift while an overlay is open.
    pub in_place_mls_group: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipient_resolve_status_maps_every_state_to_token_and_label() {
        // The 5-way vocabulary conversations.md § Errors & edge cases ratifies;
        // Idle renders empty (label None), the other four carry their
        // conversations.unified.recipient_resolve_* key.
        let idle = recipient_resolve_status(ResolveState::Idle);
        assert_eq!(idle.token, "idle");
        assert_eq!(idle.label, None);
        let cases = [
            (
                ResolveState::Resolving,
                "resolving",
                "conversations.unified.recipient_resolve_resolving",
            ),
            (
                ResolveState::Resolved,
                "resolved",
                "conversations.unified.recipient_resolve_resolved",
            ),
            (
                ResolveState::NotFound,
                "not-found",
                "conversations.unified.recipient_resolve_not_found",
            ),
            (
                ResolveState::Error,
                "error",
                "conversations.unified.recipient_resolve_error",
            ),
        ];
        for (state, token, key) in cases {
            let view = recipient_resolve_status(state);
            assert_eq!(view.token, token);
            assert_eq!(view.label.expect("non-idle states carry a label").key, key);
        }
    }

    #[test]
    fn recipient_resolve_status_from_variant_degrades_unknown_to_idle() {
        // Web passes the serde variant name off the JSON snapshot; an unknown
        // or absent value must read as idle (empty status), never a false state.
        assert_eq!(
            recipient_resolve_status_from_variant("NotFound").token,
            "not-found"
        );
        assert_eq!(
            recipient_resolve_status_from_variant("Resolved").token,
            "resolved"
        );
        assert_eq!(
            recipient_resolve_status_from_variant("garbage").token,
            "idle"
        );
        assert_eq!(recipient_resolve_status_from_variant("").token, "idle");
    }

    #[test]
    fn guess_mime_type_recognizes_common_extensions_case_insensitively() {
        let cases = [
            ("photo.png", "image/png"),
            ("photo.PNG", "image/png"),
            ("photo.jpg", "image/jpeg"),
            ("photo.jpeg", "image/jpeg"),
            ("anim.gif", "image/gif"),
            ("pic.webp", "image/webp"),
            ("doc.pdf", "application/pdf"),
            ("notes.txt", "text/plain"),
            (
                "report.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            ("clip.mp4", "video/mp4"),
        ];
        for (filename, expected) in cases {
            assert_eq!(guess_mime_type(filename), expected, "for {filename}");
        }
    }

    #[test]
    fn guess_mime_type_falls_back_to_octet_stream_for_unknown_or_missing_extension() {
        assert_eq!(guess_mime_type("README"), "application/octet-stream");
        assert_eq!(
            guess_mime_type("archive.xyz123"),
            "application/octet-stream"
        );
        assert_eq!(guess_mime_type(""), "application/octet-stream");
    }
}
