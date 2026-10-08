//! Multi-rail thread management for the unified conversations page.
//!
//! Design tracked internally; see `docs/goal/ui/conversations.md` for the
//! page-level spec.

// `ConversationsManager`/`FaunaMlsBackend`/`ConversationsSession`/`SmtpBackend`
// each hold `Arc<dyn ...Seam>` fields whose seam traits are bounded by
// `fauna_core::MaybeSendSync` (`Send + Sync` natively, empty on wasm32 — see
// that type's doc comment). On wasm32 this makes the owner `!Send`/`!Sync`,
// which is correct (wasm is single-threaded, no seam is ever moved across a
// real thread there) but trips `arc_with_non_send_sync` — the lint can't see
// the native arm of the same bound is `Send + Sync`, and swapping to `Rc`
// would fork the type per target instead of sharing one body (priority #2).
// wasm32-scoped (not a blanket allow) so the lint still guards native, where
// a genuinely-missing `Send + Sync` on some future unrelated type would still
// be a real bug worth catching.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

pub mod address;
pub mod backend;
pub mod backends;
pub mod capabilities;
pub mod compose;
pub mod contacts;
pub mod eviction;
pub mod html_markdown;
pub mod index_sink;
pub mod keying;
pub mod list_send;
pub mod mail_read;
pub mod manager;
/// The unattested-member review's list-form join.
///
/// ⚠ **Private, unlike every module beside it, and deliberately.** This crate
/// is glob-re-exported wholesale by `fauna-ffi`
/// (`libs/fauna-ffi/src/conversations.rs`'s `pub use fauna_conversations::*`),
/// which also carries a module of its own named `member_review` — the UniFFI
/// façade for this same surface. A `pub mod` here therefore shadows that one
/// through the glob, which rustc reports as `private_item_shadows_public_glob_reexport`
/// and `-D warnings` turns into a merge-gate red. The only public item is
/// re-exported at the crate root below, which is how both callers already
/// spell it, so nothing is lost by keeping the module itself out of the glob.
mod member_review;
pub mod message;
pub mod notification;
pub mod observer;
/// Where a conversation record sits on the account data plane — the identity an
/// app reports when it paints the body (`account-data-plane.md` § The replica
/// boundary → T1).
pub mod plane;
pub mod reactions;
/// Where the inbound rails record a refused scheduling change on its way to the
/// account plane (`backend::RefusedChangeLog`).
pub mod refused_changes;
pub mod rfc5322;
pub mod room;
pub mod room_settings;
pub mod session;
pub mod snapshot;
pub mod state_json;
pub mod store;
pub mod thread;

#[cfg(feature = "client-display")]
pub use address::typed_address_display;
pub use address::{Rail, TypedAddress, try_parse_typed_address};
pub use backend::{
    CommitGate, ConversationsRpc, FetchedRecord, InboundBucket, InboundMailPage, InboundMailRecord,
    InboundMailSource, LinkPreviewResolution, LinkPreviewRpc, MailFlagCallError, MailFlagChange,
    MailFlagChangesPage, OutboundMailSink, RailBackend, RailInboundMessage, SchedulingSink,
    SelfAddress, SendOutcome, SkippedMailRecord, WelcomeChannelKind, carries_seen_flag,
};
pub use capabilities::{DeliveryMode, ThreadCapabilities, ThreadEncryption};
pub use compose::{
    AddParticipantState, AttachmentDraft, ComposeState, RecipientPickerState, ReplyPreview,
    ResolveState, SendState,
};
pub use keying::{ThreadKey, normalize_subject};
pub use list_send::ListSendView;
pub use manager::ConversationsManager;
pub use member_review::member_review_flags;
pub use message::{
    AttachmentSnapshot, BodyFormat, DeleteClaim, MessageBadges, MessageId, MessageSnapshot,
    attachment_blocks,
};
pub use notification::{
    MessageNotificationTracker, ThreadActivity, banner_pass_completed, banner_pass_started,
    record_fired_banner,
};
pub use observer::SnapshotObserver;
pub use reactions::{
    MORE_GRID_EMOJIS, QUICKSET_EMOJIS, ReactionGroup, StampedReactionEvent, fold_reactions,
    more_grid_emojis, quickset_emojis,
};
pub use room::{
    HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomInvitationSnapshot, RoomMemberSnapshot,
    RoomNotice, RoomPendingInviteSnapshot, RoomPolicySnapshot, RoomRole, RoomSnapshot,
    derive_room_class, history_policy_label, join_rule_label, member_chip_text,
    prospective_room_class,
};
pub use room_settings::{RoomSettingsDraft, RoomSettingsEdit, room_may_name_labeler_kind};
pub use session::{ConversationsSession, RoomPostSeam};
#[cfg(not(target_arch = "wasm32"))]
pub use session::{KEYPACKAGE_TARGET, SessionClosed};
pub use snapshot::{
    ConversationsSnapshot, SortOrder, ThreadDetail, ThreadStateFacts, ThreadSummary,
    next_sort_order, sum_unread,
};
pub use thread::{ThreadFlavor, ThreadId};

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();
