//! Shared state machine for the `mail-settings` page (per-actor mail
//! credential lifecycle).
//!
//! Authority for behavior: `docs/goal/behavior/mail-credentials.md`.
//! Authority for UX: `docs/goal/ui/mail-settings.md`.
//!
//! The state machine owns the typed view the per-app UI renders
//! (`MailSettingsSnapshot`) and the action surface the UI dispatches
//! (`MailSettingsAction`). It drives the three `fauna.bridges.provision_*`
//! WS-RPC seams (via `NestClient`), reads/writes the account's mail custody
//! (`fauna.state.mail`, via `MailStore`), and uses the typed
//! `fauna_mls::wrapped_blob::seal_*` API directly — the
//! `libs/fauna-ffi` seal exports are for per-app UI, not for shared
//! Rust callers.
//!
//! Pattern: same shape as `libs/fauna-onboarding-machine/` and
//! `libs/fauna-launch-machine/`.

// This crate's various `*Nest`/`*PolicyNest`/`MailStore` seam traits are
// bounded by `MaybeSendSync` (`Send + Sync` natively, empty on wasm32), so an
// `Arc<dyn ...>`-holding seam struct is correctly `!Send`/`!Sync` on wasm32
// but trips `arc_with_non_send_sync` there. wasm32-scoped so native, where
// the bound resolves to `Send + Sync`, keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_mail_settings");

pub mod admin_policy;
pub mod aliases;
mod bool_toggle_policy;
pub mod bridge_approval;
pub mod caldav_policy;
pub mod carddav_policy;
pub mod credential;
pub mod error;
pub mod export;
pub mod forwarders;
pub mod forwarding;
pub mod import;
pub mod inbox_scorer;
pub mod lists;
pub mod local_domains;
pub mod machine;
pub mod password_gen;
pub mod primary_domain_rename;
mod read_order;
pub mod rotation;
#[cfg(feature = "rpc-glue")]
pub mod rpc_glue;
pub mod serving_enablement;
pub mod snapshot_sync;
pub mod spam;
pub mod state;
pub mod succession;
pub mod token_refresh;
pub mod webdav_policy;
pub mod wrap;

#[cfg(any(test, feature = "test-helpers"))]
pub mod testing;

pub use admin_policy::{
    AliasPolicyView, AuthPolicyView, FcrdnsMode, ImapDeleteNonempty, ImapPolicyView,
    MailPolicyAction, MailPolicyMachine, MailPolicyNest, MailPolicySnapshot, MailPolicyStatus,
    OutboundPolicyView, SpamPolicyView, SubmissionPolicyView, fcrdns_mode_options,
    imap_delete_nonempty_options,
};
pub use aliases::{
    AliasKind, AliasView, AliasesStatus, ImportAliasOutcomeView, ImportAliasStatusView,
    ImportResultView, MailAliasesAction, MailAliasesMachine, MailAliasesNest, MailAliasesSnapshot,
    alias_hits_label, alias_kind_badge,
};
pub use bridge_approval::{
    ApprovedBridgeView, BridgeApprovalAction, BridgeApprovalMachine, BridgeApprovalNest,
    BridgeApprovalSnapshot, BridgeApprovalStatus, PendingBridgeView, bridge_display_name,
};
pub use caldav_policy::{
    CaldavPolicyAction, CaldavPolicyMachine, CaldavPolicyNest, CaldavPolicySnapshot,
    CaldavPolicyStatus,
};
pub use carddav_policy::{
    CarddavPolicyAction, CarddavPolicyMachine, CarddavPolicyNest, CarddavPolicySnapshot,
    CarddavPolicyStatus,
};
pub use credential::{Credential, derive_credential_id};
pub use error::{DispatchError, NestError, SignerError, StoreError};
pub use export::{
    ExportFormat, ExportScope, ExportSessionState, ExportSessionView, ExportStatus, ExportStep,
    MailExportAction, MailExportMachine, MailExportNest, MailExportSnapshot, MailboxOption,
    MailboxProgressView, export_format_label,
};
pub use forwarders::{
    ForwarderAction, ForwarderMachine, ForwarderNest, ForwarderStatus, ForwarderView,
    ForwardersSnapshot,
};
pub use forwarding::{
    forward_per_hour_hint, parse_forward_all_to_draft, parse_forward_per_hour_draft,
};
pub use import::{
    DEFAULT_MAX_SIZE_MB, ImportSessionState, ImportSessionView, ImportSourceKind, ImportSourceNest,
    ImportStatus, ImportStep, ImportTlsMode, MailImportAction, MailImportMachine, MailImportNest,
    MailImportSnapshot, SOURCE_KINDS, SourceConnectParams, SourceMailboxOption, SourceMailboxView,
    TLS_MODES, connect_actions, import_source_kind_label, import_tls_mode_label,
    scope_next_actions,
};
pub use inbox_scorer::InboxSpamScorer;
pub use lists::{
    ImportResult, ListDraft, ListMembers, ListView, ListsStatus, MailListMembersAction,
    MailListMembersMachine, MailListMembersNest, MailListMembersSnapshot, MailListsAction,
    MailListsMachine, MailListsNest, MailListsSnapshot, MemberStatus, MemberView,
    member_status_label,
};
pub use local_domains::{
    DEFAULT_CERT_MODE, DomainDmarcPolicy, LocalDomainAction, LocalDomainMachine, LocalDomainNest,
    LocalDomainStatus, LocalDomainView, LocalDomainsSnapshot,
};
pub use primary_domain_rename::{PrimaryDomainRenameView, rename_available};
pub use succession::{MailBurnOutcome, MailBurnProgress, burn_mail_after_succession};

pub use spam::{
    MailSpamAction, MailSpamMachine, MailSpamNest, MailSpamSnapshot, SpamHistory, SpamStatus,
    SpamTrainingView, TrainingLabel, TrainingSource, training_label_badge, training_source_badge,
};
pub use webdav_policy::{
    WebdavPolicyAction, WebdavPolicyMachine, WebdavPolicyNest, WebdavPolicySnapshot,
    WebdavPolicyStatus,
};
// Re-exported next to `MailSettingsAction`: the secret-bearing action variants
// carry their bytes in `SecretBytes`, so a consumer constructing an action finds
// the type in this crate's surface (it lives in `fauna-core`).
pub use fauna_core::secret::SecretBytes;
pub use machine::{
    HistoryWrite, IdentitySigner, MailSettingsMachine, NestClient, SealedModelWriter,
    SpamModelClientWrite, SpamModelWriteOutcome, WebdavServedSets,
};
// The client model-write op the moderation-queue / mail-spam surfaces build and
// hand to `MailSettingsMachine::apply_spam_model_write` (re-exported so a consumer
// finds it on this crate's surface, next to `SpamModelWriteOutcome`).
pub use fauna_mail::spam::model_write::ModelWriteOp;
pub use state::{
    CredentialKind, MailCredentialSummary, MailSettingsAction, MailSettingsSnapshot,
    MuaInstructions, PendingRotationStatus, SettingsStatus, credential_kind_badge,
    resolve_mua_username, settings_status_label,
};
