//! Shared orchestration for the user-facing `mail-lists` + `mail-list-members`
//! pages (per-account mailing lists — a list is a sixth alias kind): create /
//! edit / delete a list, and manage its subscribed/unsubscribed members.
//!
//! Authority for behavior: `docs/goal/behavior/mail-mass-mailing.md`
//! § `mail-lists` page UX (the list rows + add-list sheet), § `mail-list-members`
//! page (member add / batch-import / unsubscribe / resubscribe), § Wire shapes
//! (`{list,create,update,delete}_account_list` + `{list,add,batch_import,
//! unsubscribe,resubscribe}_list_member`). Authority for UX/IDs:
//! `tests/e2e-unified/ui.yaml` `mail-lists` + `mail-list-members` pages + their
//! `*-list` components.
//!
//! Two machines (mirrors `forwarders.rs` / `aliases.rs` / `spam.rs`):
//! [`MailListsMachine`] (the lists page) and [`MailListMembersMachine`] (the
//! per-list members page, scoped to one `list_id`).
//!
//! # Wire → view projection
//!
//! The seams hand the machines [`ListView`] / [`MemberView`] rather than wire
//! rows, so the wire→view mapping is a plain function every app shares:
//! [`project_list_row`], [`project_member_row`] and [`derive_list_domains`]
//! (priority #2 — the address composition, the friendly-name fallback, the
//! status-from-`unsubscribed_at` derivation and the user-tier domain sourcing are
//! each written exactly once).
//!
//! The list backend (the nine `fauna.bridges.*_list_*` RPCs) went live
//! 2026-06-13/14; the `rpc_glue` seams were rewired onto it 2026-07-29. Before
//! that they returned an honest `unimplemented` rejection, which is why the pages
//! could render on six apps while doing nothing — see `mail-mass-mailing.md`
//! § Implementation status today.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_protocol::MaybeSendSync;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

// ════════════════════════════════════════════════════════════════════════
// mail-lists page
// ════════════════════════════════════════════════════════════════════════

/// One mailing list as the `mail-lists-list` renders it. Projected by the seam
/// from the (unbuilt) `mail_lists` wire row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ListView {
    /// Lowercase hex of the 16-byte list id (the alias-row id; a list is a sixth
    /// alias kind). Carried into Update/Delete + the members-page navigation.
    pub list_id_hex: String,
    /// `mail-lists-list-item-name` (friendly name).
    pub friendly_name: String,
    /// The list's send-from local-part.
    pub local_part: String,
    /// The list's domain.
    pub local_domain: String,
    /// The full send-from address (`<local_part>@<local_domain>`) — rendered
    /// beside the friendly name.
    pub address: String,
    /// Optional description (edit-sheet field).
    pub description: String,
    /// `mail-lists-list-item-member-count` (subscribed only).
    pub member_count: u32,
    /// `mail-lists-list-item-last-send` (epoch-millis; `None` = never sent).
    pub last_send_at_ms: Option<i64>,
    /// `mail-lists-list-item-quota` — the user's own running quota meter.
    pub sends_today: u32,
    pub recipients_today: u32,
    /// Optional `List-Help` URL (edit-sheet field).
    pub list_help_url: String,
    /// Optional `List-Archive` URL (edit-sheet field).
    pub list_archive_url: String,
    /// Optional per-send recipient cap (≤ admin ceiling).
    pub recipients_per_send: Option<u32>,
}

impl ListView {
    /// Build a row from its parts, computing `address` + hex-encoding the id. The
    /// test `FakeNest` and (once it lands) the real glue both project the wire row
    /// through here.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        list_id: &[u8],
        friendly_name: impl Into<String>,
        local_part: impl Into<String>,
        local_domain: impl Into<String>,
        description: impl Into<String>,
        member_count: u32,
        last_send_at_ms: Option<i64>,
        sends_today: u32,
        recipients_today: u32,
        list_help_url: impl Into<String>,
        list_archive_url: impl Into<String>,
        recipients_per_send: Option<u32>,
    ) -> Self {
        let local_part = local_part.into();
        let local_domain = local_domain.into();
        let address = format!("{local_part}@{local_domain}");
        Self {
            list_id_hex: hex::encode(list_id),
            friendly_name: friendly_name.into(),
            local_part,
            local_domain,
            address,
            description: description.into(),
            member_count,
            last_send_at_ms,
            sends_today,
            recipients_today,
            list_help_url: list_help_url.into(),
            list_archive_url: list_archive_url.into(),
            recipients_per_send,
        }
    }
}

/// Project one `fauna.bridges.list_account_lists` wire row onto the [`ListView`]
/// the `mail-lists-list` renders.
///
/// Shared so no app re-derives it (priority #2) — the address composition, the
/// friendly-name fallback and the `Option<String>` → `String` flattening are the
/// three places a per-app hand-roll would drift. An absent optional renders as an
/// **empty** edit-sheet input, never a fabricated placeholder (the
/// `list_archive_url` case matters: `mail-mass-mailing.md:602` says the
/// List-Archive header is omitted when unset, never fabricated).
pub fn project_list_row(row: fauna_client_bridges::MailListRow) -> ListView {
    // A list with no friendly name falls back to its posting local-part, so the
    // row never renders blank (nest-side the List-Id falls back to the list id).
    let friendly_name = row
        .friendly_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| row.pattern.clone());
    ListView::from_parts(
        row.list_id.as_ref(),
        friendly_name,
        row.pattern,
        row.local_domain,
        row.description.unwrap_or_default(),
        row.member_count.max(0) as u32,
        row.last_send_at,
        row.sends_today.max(0) as u32,
        row.recipients_today.max(0) as u32,
        row.list_help_url.unwrap_or_default(),
        row.list_archive_url.unwrap_or_default(),
        row.recipients_per_send.map(|c| c.max(0) as u32),
    )
}

/// Project one `fauna.bridges.list_list_members` wire row onto the [`MemberView`]
/// the `mail-list-members-list` renders. The subscription status is *derived*
/// from `unsubscribed_at` — the wire carries no separate status field, so this is
/// the one place the mapping lives (priority #2).
pub fn project_member_row(row: fauna_client_bridges::MailListMemberRow) -> MemberView {
    MemberView {
        address: row.recipient_address,
        subscribed_at_ms: Some(row.subscribed_at),
        status: match row.unsubscribed_at {
            Some(_) => MemberStatus::Unsubscribed,
            None => MemberStatus::Subscribed,
        },
    }
}

/// The domains the add-list sheet's `mail-lists-add-sheet-domain-picker` offers.
///
/// **User-tier by construction.** `mail-mass-mailing.md:394` makes lists a
/// user-tier surface, so the picker must not read the Admin-class
/// `fauna.bridges.list_local_domains` — a plain user is not allowed to call it.
/// Instead the options are derived from rows the caller already owns: the domains
/// of their existing lists, then of their existing aliases (the canonical exact
/// alias is created at mail-enable, so this is non-empty in practice). Deduped,
/// order-stable, list domains first.
pub fn derive_list_domains(list_domains: &[String], alias_domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in list_domains.iter().chain(alias_domains.iter()) {
        let d = d.trim();
        if !d.is_empty() && !out.iter().any(|seen| seen == d) {
            out.push(d.to_string());
        }
    }
    out
}

/// The fields the add/edit sheet collects (`mail-lists-add-sheet-*`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ListDraft {
    pub friendly_name: String,
    pub local_part: String,
    pub local_domain: String,
    pub description: String,
    pub list_help_url: String,
    pub list_archive_url: String,
    pub recipients_per_send: Option<u32>,
}

/// Coarse machine status (mirrors `ForwarderStatus`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ListsStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `mail-lists`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailListsSnapshot {
    pub lists: Vec<ListView>,
    /// The user's owned domains (`mail-lists-add-sheet-domain-picker` options).
    pub local_domains: Vec<String>,
    pub status: ListsStatus,
    pub error: Option<String>,
}

impl MailListsSnapshot {
    fn empty() -> Self {
        Self {
            lists: Vec::new(),
            local_domains: Vec::new(),
            status: ListsStatus::Idle,
            error: None,
        }
    }
}

/// Actions the `mail-lists` page dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailListsAction {
    Refresh,
    Create {
        draft: ListDraft,
    },
    Update {
        list_id_hex: String,
        draft: ListDraft,
    },
    Delete {
        list_id_hex: String,
    },
}

/// WS-RPC seam for the lists page (mirrors `ForwarderNest`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailListsNest: MaybeSendSync {
    /// `fauna.bridges.list_account_lists` — the user's own lists.
    async fn list_account_lists(&self) -> Result<Vec<ListView>, NestError>;
    /// The user's owned domains (the add-sheet picker). The general mail surface.
    async fn list_local_domains(&self) -> Result<Vec<String>, NestError>;
    /// `fauna.bridges.create_account_list`.
    async fn create_account_list(&self, draft: ListDraft) -> Result<(), NestError>;
    /// `fauna.bridges.update_account_list`.
    async fn update_account_list(
        &self,
        list_id: Vec<u8>,
        draft: ListDraft,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.delete_account_list` — cascades members.
    async fn delete_account_list(&self, list_id: Vec<u8>) -> Result<(), NestError>;
}

/// Decode a list's hex id to the 16-byte wire form.
fn decode_list_id(list_id_hex: &str) -> Result<Vec<u8>, DispatchError> {
    crate::error::decode_hex_id16("list id", list_id_hex)
}

/// One instance per user client for the `mail-lists` page.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailListsMachine {
    nest: Arc<dyn MailListsNest>,
    inner: Mutex<MailListsSnapshot>,
}

impl MailListsMachine {
    pub fn new(nest: Arc<dyn MailListsNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(MailListsSnapshot::empty()),
        }
    }

    fn set_status(&self, status: ListsStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Loading);
        let lists = self.nest.list_account_lists().await?;
        let local_domains = self.nest.list_local_domains().await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.lists = lists;
        snap.local_domains = local_domains;
        snap.status = ListsStatus::Idle;
        Ok(())
    }

    async fn create(&self, draft: ListDraft) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        self.nest.create_account_list(draft).await?;
        self.refresh().await
    }

    async fn update(&self, list_id_hex: String, draft: ListDraft) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        let id = decode_list_id(&list_id_hex)?;
        self.nest.update_account_list(id, draft).await?;
        self.refresh().await
    }

    async fn delete(&self, list_id_hex: String) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        let id = decode_list_id(&list_id_hex)?;
        self.nest.delete_account_list(id).await?;
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailListsMachine {
    pub fn snapshot(&self) -> MailListsSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailListsMachine {
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: MailListsAction) -> Result<(), DispatchError> {
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            ListsStatus,
            match action {
                MailListsAction::Refresh => self.refresh().await,
                MailListsAction::Create { draft } => self.create(draft).await,
                MailListsAction::Update { list_id_hex, draft } => {
                    self.update(list_id_hex, draft).await
                }
                MailListsAction::Delete { list_id_hex } => self.delete(list_id_hex).await,
            }
        )
    }
}

// ════════════════════════════════════════════════════════════════════════
// mail-list-members page
// ════════════════════════════════════════════════════════════════════════

/// A member's subscription state — `mail-list-members-list-item-status`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MemberStatus {
    Subscribed,
    Unsubscribed,
}

/// Canonical label for a [`MemberStatus`], returned as [`LocalizedText`] so each
/// app resolves it through its own i18n runtime (mirrors
/// `fauna_client_search::render::content_type_badge`). Lifts the identical
/// two-arm map that linux/windows/apple/android/web each hard-coded — one source
/// of truth for the `mail-list-members-list-item-status` label (priority #1/#2).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn member_status_label(status: MemberStatus) -> LocalizedText {
    match status {
        MemberStatus::Subscribed => LocalizedText::key("mail_lists.status_subscribed"),
        MemberStatus::Unsubscribed => LocalizedText::key("mail_lists.status_unsubscribed"),
    }
}

/// One list member as `mail-list-members-list` renders it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MemberView {
    /// `mail-list-members-list-item-address`.
    pub address: String,
    /// `mail-list-members-list-item-subscribed-at` (epoch-millis).
    pub subscribed_at_ms: Option<i64>,
    /// `mail-list-members-list-item-status`.
    pub status: MemberStatus,
}

/// What `list_list_members` returns: the rows + the summary counts
/// (`mail-list-members-summary`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ListMembers {
    pub members: Vec<MemberView>,
    pub subscribed_count: u32,
    pub unsubscribed_count: u32,
}

/// The result of a batch import (`mail-list-members-import-sheet`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportResult {
    pub added: u32,
    pub skipped_invalid: u32,
    pub skipped_duplicate: u32,
}

/// Read-only snapshot the per-app UI renders for `mail-list-members`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailListMembersSnapshot {
    /// The list this page is scoped to (hex id) + its friendly name (page heading).
    pub list_id_hex: String,
    pub list_name: String,
    pub members: Vec<MemberView>,
    pub subscribed_count: u32,
    pub unsubscribed_count: u32,
    pub status: ListsStatus,
    pub error: Option<String>,
    /// The last import's tally (surfaced transiently after a batch import).
    pub last_import: Option<ImportResult>,
}

impl MailListMembersSnapshot {
    fn new(list_id_hex: String, list_name: String) -> Self {
        Self {
            list_id_hex,
            list_name,
            members: Vec::new(),
            subscribed_count: 0,
            unsubscribed_count: 0,
            status: ListsStatus::Idle,
            error: None,
            last_import: None,
        }
    }
}

/// Actions the `mail-list-members` page dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailListMembersAction {
    Refresh,
    AddMember {
        address: String,
    },
    /// One address per line (the import sheet's text area).
    BatchImport {
        addresses: String,
    },
    Unsubscribe {
        address: String,
    },
    Resubscribe {
        address: String,
    },
}

/// WS-RPC seam for the members page. Every call carries the page's `list_id`
/// (the machine holds it).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailListMembersNest: MaybeSendSync {
    /// `fauna.bridges.list_list_members`.
    async fn list_list_members(&self, list_id: Vec<u8>) -> Result<ListMembers, NestError>;
    /// `fauna.bridges.add_list_member`.
    async fn add_list_member(&self, list_id: Vec<u8>, address: String) -> Result<(), NestError>;
    /// `fauna.bridges.batch_import_list_members`.
    async fn batch_import_list_members(
        &self,
        list_id: Vec<u8>,
        addresses: Vec<String>,
    ) -> Result<ImportResult, NestError>;
    /// `fauna.bridges.unsubscribe_list_member` (manual; flips `unsubscribed_at`).
    async fn unsubscribe_list_member(
        &self,
        list_id: Vec<u8>,
        address: String,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.resubscribe_list_member`.
    async fn resubscribe_list_member(
        &self,
        list_id: Vec<u8>,
        address: String,
    ) -> Result<(), NestError>;
}

/// One instance per opened list. Holds the `list_id` it's scoped to.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailListMembersMachine {
    nest: Arc<dyn MailListMembersNest>,
    list_id: Vec<u8>,
    inner: Mutex<MailListMembersSnapshot>,
}

impl MailListMembersMachine {
    /// `list_id_hex` is the hex id of the list whose members this page manages
    /// (from a rendered [`ListView`]); `list_name` is its friendly name (heading).
    pub fn new(
        nest: Arc<dyn MailListMembersNest>,
        list_id_hex: String,
        list_name: String,
    ) -> Result<Self, DispatchError> {
        let list_id = decode_list_id(&list_id_hex)?;
        Ok(Self {
            nest,
            list_id,
            inner: Mutex::new(MailListMembersSnapshot::new(list_id_hex, list_name)),
        })
    }

    fn set_status(&self, status: ListsStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Loading);
        let reply = self.nest.list_list_members(self.list_id.clone()).await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.members = reply.members;
        snap.subscribed_count = reply.subscribed_count;
        snap.unsubscribed_count = reply.unsubscribed_count;
        snap.status = ListsStatus::Idle;
        Ok(())
    }

    async fn add_member(&self, address: String) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        self.nest
            .add_list_member(self.list_id.clone(), address)
            .await?;
        self.refresh().await
    }

    async fn batch_import(&self, addresses: String) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        // `str::lines()` only recognizes `\n`/`\r\n` — never a bare `\r`. A WinUI
        // multi-line `TextBox` (`AcceptsReturn="True"`) reports its `ValuePattern`
        // value with bare `\r` as the line separator (Rich-Edit-based text
        // services heritage), which `.lines()` treats as a SINGLE line, silently
        // collapsing every pasted address into one string that fails validation —
        // 0 added, 0 errors surfaced. Split on either character so all three
        // conventions parse the same way.
        let parsed: Vec<String> = addresses
            .split(['\r', '\n'])
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        let result = self
            .nest
            .batch_import_list_members(self.list_id.clone(), parsed)
            .await?;
        self.inner.lock().expect("snapshot mutex").last_import = Some(result);
        self.refresh().await
    }

    async fn unsubscribe(&self, address: String) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        self.nest
            .unsubscribe_list_member(self.list_id.clone(), address)
            .await?;
        self.refresh().await
    }

    async fn resubscribe(&self, address: String) -> Result<(), DispatchError> {
        self.set_status(ListsStatus::Working);
        self.nest
            .resubscribe_list_member(self.list_id.clone(), address)
            .await?;
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailListMembersMachine {
    pub fn snapshot(&self) -> MailListMembersSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailListMembersMachine {
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: MailListMembersAction) -> Result<(), DispatchError> {
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.error = None;
            snap.last_import = None;
        }
        crate::dispatch_capturing_error!(
            self,
            ListsStatus,
            match action {
                MailListMembersAction::Refresh => self.refresh().await,
                MailListMembersAction::AddMember { address } => self.add_member(address).await,
                MailListMembersAction::BatchImport { addresses } => {
                    self.batch_import(addresses).await
                }
                MailListMembersAction::Unsubscribe { address } => {
                    self.unsubscribe(address).await
                }
                MailListMembersAction::Resubscribe { address } => {
                    self.resubscribe(address).await
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // ── wire → view projections (shared by all 7 apps) ──────────────────

    fn wire_list_row() -> fauna_client_bridges::MailListRow {
        fauna_client_bridges::MailListRow {
            list_id: fauna_protocol::ByteBuf::from(vec![0xa1u8; 16]),
            alias_id: fauna_protocol::ByteBuf::from(vec![0xa2u8; 16]),
            owner_actor_id: fauna_protocol::ByteBuf::from(vec![0xa3u8; 32]),
            local_domain: "example.com".into(),
            pattern: "bob-weekly".into(),
            friendly_name: Some("Bob's Weekly".into()),
            description: Some("A newsletter".into()),
            list_help_url: Some("https://example.com/help".into()),
            list_archive_url: None,
            recipients_per_send: Some(2500),
            created_at: 1_700_000_000_000,
            last_send_at: Some(1_700_000_500_000),
            member_count: 3,
            sends_today: 1,
            recipients_today: 3,
        }
    }

    #[test]
    fn list_row_projects_onto_the_rendered_row() {
        let view = project_list_row(wire_list_row());
        assert_eq!(view.list_id_hex, "a1".repeat(16));
        assert_eq!(view.friendly_name, "Bob's Weekly");
        assert_eq!(view.local_part, "bob-weekly");
        // `mail-lists-list-item-name` renders name + address, so the projection
        // composes the address rather than leaving each app to do it.
        assert_eq!(view.address, "bob-weekly@example.com");
        assert_eq!(view.member_count, 3);
        assert_eq!(view.last_send_at_ms, Some(1_700_000_500_000));
        assert_eq!(view.sends_today, 1);
        assert_eq!(view.recipients_today, 3);
        assert_eq!(view.recipients_per_send, Some(2500));
    }

    #[test]
    fn absent_wire_options_project_to_empty_strings_not_placeholders() {
        // The optional metadata fields are `Option<String>` on the wire and plain
        // `String` on the view (the edit sheet's text inputs). An absent value is
        // an empty input, never a fabricated placeholder — `list_archive_url`
        // especially: `mail-mass-mailing.md` § Layout says the List-Archive
        // header is omitted when unset, never fabricated.
        let mut row = wire_list_row();
        row.friendly_name = None;
        row.description = None;
        row.list_help_url = None;
        row.list_archive_url = None;
        let view = project_list_row(row);
        assert_eq!(view.description, "");
        assert_eq!(view.list_help_url, "");
        assert_eq!(view.list_archive_url, "");
        // A list with no friendly name falls back to its posting local-part so
        // the row is never blank (List-Id falls back to the id nest-side).
        assert_eq!(view.friendly_name, "bob-weekly");
    }

    #[test]
    fn never_sent_list_projects_a_none_last_send() {
        let mut row = wire_list_row();
        row.last_send_at = None;
        assert_eq!(project_list_row(row).last_send_at_ms, None);
    }

    #[test]
    fn member_row_projects_status_from_the_unsubscribed_timestamp() {
        let subscribed = fauna_client_bridges::MailListMemberRow {
            member_id: fauna_protocol::ByteBuf::from(vec![0xb1u8; 16]),
            recipient_address: "reader@example.net".into(),
            subscribed_at: 1_700_000_000_000,
            unsubscribed_at: None,
        };
        let view = project_member_row(subscribed.clone());
        assert_eq!(view.address, "reader@example.net");
        assert_eq!(view.subscribed_at_ms, Some(1_700_000_000_000));
        assert_eq!(view.status, MemberStatus::Subscribed);

        let gone = fauna_client_bridges::MailListMemberRow {
            unsubscribed_at: Some(1_700_000_900_000),
            ..subscribed
        };
        assert_eq!(project_member_row(gone).status, MemberStatus::Unsubscribed);
    }

    #[test]
    fn domains_for_the_add_sheet_come_from_the_users_own_lists_and_aliases() {
        // `mail-mass-mailing.md:394` — lists are user-tier, so the add-sheet
        // picker must NOT read the Admin-class `list_local_domains`. The domains
        // are derived from rows the user already owns, deduped, order-stable.
        let domains = derive_list_domains(
            &["example.com".to_string(), "example.com".to_string()],
            &["second.example".to_string(), "example.com".to_string()],
        );
        assert_eq!(domains, vec!["example.com", "second.example"]);
    }

    #[test]
    fn domain_derivation_survives_a_user_with_no_lists_yet() {
        // The common first-run case: no lists, one canonical alias domain.
        assert_eq!(
            derive_list_domains(&[], &["example.com".to_string()]),
            vec!["example.com"]
        );
        // And the degenerate one — nothing to offer, an empty picker rather than
        // a fabricated domain.
        assert!(derive_list_domains(&[], &[]).is_empty());
    }

    // ── mail-lists ──────────────────────────────────────────────────────

    #[derive(Default)]
    struct FakeListsNest {
        lists: StdMutex<Vec<ListView>>,
        domains: StdMutex<Vec<String>>,
        next_id: StdMutex<u8>,
    }

    #[async_trait]
    impl MailListsNest for FakeListsNest {
        async fn list_account_lists(&self) -> Result<Vec<ListView>, NestError> {
            Ok(self.lists.lock().unwrap().clone())
        }
        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            Ok(self.domains.lock().unwrap().clone())
        }
        async fn create_account_list(&self, draft: ListDraft) -> Result<(), NestError> {
            let mut lists = self.lists.lock().unwrap();
            if lists
                .iter()
                .any(|l| l.local_part == draft.local_part && l.local_domain == draft.local_domain)
            {
                return Err(NestError::Rejected("conflicts_with_existing_alias".into()));
            }
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            lists.push(ListView::from_parts(
                &[*id; 16],
                draft.friendly_name,
                draft.local_part,
                draft.local_domain,
                draft.description,
                0,
                None,
                0,
                0,
                draft.list_help_url,
                draft.list_archive_url,
                draft.recipients_per_send,
            ));
            Ok(())
        }
        async fn update_account_list(
            &self,
            list_id: Vec<u8>,
            draft: ListDraft,
        ) -> Result<(), NestError> {
            let mut lists = self.lists.lock().unwrap();
            let row = lists
                .iter_mut()
                .find(|l| hex::decode(&l.list_id_hex).unwrap() == list_id)
                .ok_or(NestError::Rejected("fauna.bridges.not_found".into()))?;
            row.friendly_name = draft.friendly_name;
            row.description = draft.description;
            row.list_help_url = draft.list_help_url;
            row.list_archive_url = draft.list_archive_url;
            row.recipients_per_send = draft.recipients_per_send;
            Ok(())
        }
        async fn delete_account_list(&self, list_id: Vec<u8>) -> Result<(), NestError> {
            let mut lists = self.lists.lock().unwrap();
            let before = lists.len();
            lists.retain(|l| hex::decode(&l.list_id_hex).unwrap() != list_id);
            if lists.len() == before {
                return Err(NestError::Rejected("fauna.bridges.not_found".into()));
            }
            Ok(())
        }
    }

    fn lists_machine(domains: &[&str]) -> MailListsMachine {
        MailListsMachine::new(Arc::new(FakeListsNest {
            lists: StdMutex::new(Vec::new()),
            domains: StdMutex::new(domains.iter().map(|s| s.to_string()).collect()),
            next_id: StdMutex::new(0x10),
        }))
    }

    fn draft(name: &str, local_part: &str, domain: &str) -> ListDraft {
        ListDraft {
            friendly_name: name.into(),
            local_part: local_part.into(),
            local_domain: domain.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn lists_hydrate_projects_domains() {
        let m = lists_machine(&["example.com", "other.test"]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert!(snap.lists.is_empty());
        assert_eq!(snap.local_domains, vec!["example.com", "other.test"]);
        assert_eq!(snap.status, ListsStatus::Idle);
    }

    #[tokio::test]
    async fn lists_create_computes_address_and_relists() {
        let m = lists_machine(&["example.com"]);
        m.hydrate().await.unwrap();
        m.dispatch(MailListsAction::Create {
            draft: draft("Bob's Weekly", "bob-weekly", "example.com"),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.lists.len(), 1);
        assert_eq!(snap.lists[0].address, "bob-weekly@example.com");
        assert_eq!(snap.lists[0].friendly_name, "Bob's Weekly");
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn lists_create_collision_surfaces_error_and_keeps_list() {
        let m = lists_machine(&["example.com"]);
        m.hydrate().await.unwrap();
        m.dispatch(MailListsAction::Create {
            draft: draft("A", "news", "example.com"),
        })
        .await
        .unwrap();
        let err = m
            .dispatch(MailListsAction::Create {
                draft: draft("B", "news", "example.com"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(snap.error.as_deref().unwrap().contains("conflicts"));
        assert_eq!(snap.lists.len(), 1, "list untouched");
    }

    #[tokio::test]
    async fn lists_update_then_delete() {
        let m = lists_machine(&["example.com"]);
        m.hydrate().await.unwrap();
        m.dispatch(MailListsAction::Create {
            draft: draft("Old", "news", "example.com"),
        })
        .await
        .unwrap();
        let id = m.snapshot().lists[0].list_id_hex.clone();
        let mut d = draft("New name", "news", "example.com");
        d.description = "now described".into();
        m.dispatch(MailListsAction::Update {
            list_id_hex: id.clone(),
            draft: d,
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().lists[0].friendly_name, "New name");
        assert_eq!(m.snapshot().lists[0].description, "now described");
        m.dispatch(MailListsAction::Delete { list_id_hex: id })
            .await
            .unwrap();
        assert!(m.snapshot().lists.is_empty());
    }

    #[tokio::test]
    async fn lists_delete_malformed_hex_surfaces_wrap() {
        let m = lists_machine(&["example.com"]);
        let err = m
            .dispatch(MailListsAction::Delete {
                list_id_hex: "zz".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Wrap(_)), "got {err:?}");
    }

    // ── mail-list-members ───────────────────────────────────────────────

    #[derive(Default)]
    struct FakeMembersNest {
        members: StdMutex<Vec<MemberView>>,
    }

    #[async_trait]
    impl MailListMembersNest for FakeMembersNest {
        async fn list_list_members(&self, _list_id: Vec<u8>) -> Result<ListMembers, NestError> {
            let members = self.members.lock().unwrap().clone();
            let subscribed_count = members
                .iter()
                .filter(|m| m.status == MemberStatus::Subscribed)
                .count() as u32;
            let unsubscribed_count = members.len() as u32 - subscribed_count;
            Ok(ListMembers {
                members,
                subscribed_count,
                unsubscribed_count,
            })
        }
        async fn add_list_member(
            &self,
            _list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let mut members = self.members.lock().unwrap();
            if members.iter().any(|m| m.address == address) {
                return Err(NestError::Rejected("duplicate".into()));
            }
            members.push(MemberView {
                address,
                subscribed_at_ms: Some(1_700_000_000_000),
                status: MemberStatus::Subscribed,
            });
            Ok(())
        }
        async fn batch_import_list_members(
            &self,
            _list_id: Vec<u8>,
            addresses: Vec<String>,
        ) -> Result<ImportResult, NestError> {
            let mut members = self.members.lock().unwrap();
            let mut added = 0;
            let mut skipped_invalid = 0;
            let mut skipped_duplicate = 0;
            for a in addresses {
                if !a.contains('@') {
                    skipped_invalid += 1;
                } else if members.iter().any(|m| m.address == a) {
                    skipped_duplicate += 1;
                } else {
                    members.push(MemberView {
                        address: a,
                        subscribed_at_ms: Some(1_700_000_000_000),
                        status: MemberStatus::Subscribed,
                    });
                    added += 1;
                }
            }
            Ok(ImportResult {
                added,
                skipped_invalid,
                skipped_duplicate,
            })
        }
        async fn unsubscribe_list_member(
            &self,
            _list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let mut members = self.members.lock().unwrap();
            if let Some(m) = members.iter_mut().find(|m| m.address == address) {
                m.status = MemberStatus::Unsubscribed;
            }
            Ok(())
        }
        async fn resubscribe_list_member(
            &self,
            _list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let mut members = self.members.lock().unwrap();
            if let Some(m) = members.iter_mut().find(|m| m.address == address) {
                m.status = MemberStatus::Subscribed;
            }
            Ok(())
        }
    }

    fn members_machine() -> MailListMembersMachine {
        MailListMembersMachine::new(
            Arc::new(FakeMembersNest::default()),
            hex::encode([0x11u8; 16]),
            "Bob's Weekly".into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn members_add_and_count() {
        let m = members_machine();
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().list_name, "Bob's Weekly");
        m.dispatch(MailListMembersAction::AddMember {
            address: "a@x.example".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.members.len(), 1);
        assert_eq!(snap.subscribed_count, 1);
        assert_eq!(snap.members[0].status, MemberStatus::Subscribed);
    }

    #[tokio::test]
    async fn members_batch_import_tallies() {
        let m = members_machine();
        m.hydrate().await.unwrap();
        m.dispatch(MailListMembersAction::BatchImport {
            addresses: "a@x.example\nnot-an-email\na@x.example\nb@y.example\n".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        let imp = snap.last_import.unwrap();
        assert_eq!(imp.added, 2);
        assert_eq!(imp.skipped_invalid, 1);
        assert_eq!(imp.skipped_duplicate, 1);
        assert_eq!(snap.subscribed_count, 2);
    }

    /// Row 29: a WinUI multi-line `TextBox` with
    /// `AcceptsReturn="True"` reports its `ValuePattern` value with bare `\r` as
    /// the line separator (its Rich-Edit-based text services heritage), never
    /// `\n`/`\r\n` — confirmed via a live e2e repro reading the textbox's actual
    /// UIA value straight after typing. `str::lines()` does not split on a lone
    /// `\r`, so the whole windows paste collapsed into one garbled "address"
    /// that failed validation — 0 added, 0 errors, exactly the observed "silent
    /// empty" failure. The parser must accept all three conventions.
    #[tokio::test]
    async fn members_batch_import_tallies_cr_only_line_endings() {
        let m = members_machine();
        m.hydrate().await.unwrap();
        m.dispatch(MailListMembersAction::BatchImport {
            addresses: "a@x.example\rnot-an-email\ra@x.example\rb@y.example\r".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        let imp = snap.last_import.unwrap();
        assert_eq!(imp.added, 2);
        assert_eq!(imp.skipped_invalid, 1);
        assert_eq!(imp.skipped_duplicate, 1);
        assert_eq!(snap.subscribed_count, 2);
    }

    #[tokio::test]
    async fn members_unsubscribe_then_resubscribe() {
        let m = members_machine();
        m.hydrate().await.unwrap();
        m.dispatch(MailListMembersAction::AddMember {
            address: "a@x.example".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailListMembersAction::Unsubscribe {
            address: "a@x.example".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.subscribed_count, 0);
        assert_eq!(snap.unsubscribed_count, 1);
        assert_eq!(snap.members[0].status, MemberStatus::Unsubscribed);
        m.dispatch(MailListMembersAction::Resubscribe {
            address: "a@x.example".into(),
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().subscribed_count, 1);
    }

    #[tokio::test]
    async fn members_machine_rejects_bad_list_id() {
        let result = MailListMembersMachine::new(
            Arc::new(FakeMembersNest::default()),
            "nothex".into(),
            "x".into(),
        );
        assert!(
            matches!(result, Err(DispatchError::Wrap(_))),
            "expected Wrap error on malformed list id",
        );
    }

    // ── member status label ─────────────────────────────────────────────

    #[test]
    fn member_status_label_maps_subscribed() {
        assert_eq!(
            member_status_label(MemberStatus::Subscribed).key,
            "mail_lists.status_subscribed",
        );
    }

    #[test]
    fn member_status_label_maps_unsubscribed() {
        assert_eq!(
            member_status_label(MemberStatus::Unsubscribed).key,
            "mail_lists.status_unsubscribed",
        );
    }
}
