//! Native-client FFI for the **recipient-side shared-folder pending-share
//! surface** — list the staged (knocked) shares, then `accept` (join the MLS
//! group off the chat rail + ack) or `decline` (ack-and-drop, never joins). The
//! recipient counterpart to the owner-side [`crate::folders_author`]
//! (`docs/goal/ui/folders.md` § Sharing a folder — *Recipient side*: "the
//! share lands as a `folder-pending-share` … you `folder-share-accept-button`
//! or `folder-share-decline-button`. The MLS Welcome is **staged unprocessed**
//! until accept").
//!
//! The B2 contact gate (native receive rail) already auto-joins a *contact's*
//! shared set and *stages a stranger's* Welcome un-acked; this
//! module surfaces + acts on those staged welcomes. The list logic itself is the
//! shared [`list_folder_pending_shares`] in `fauna-client-inbox` (written once,
//! native + wasm — priority #2); this seam gives Apple / Windows / Android the
//! identical high-level surface over UniFFI, and `fauna-wasm` folds its web twin
//! into the wasm receive arm (B5).
//!
//! Gated behind the default-on `conversations-session` feature (the accept fn
//! takes the cross-crate [`ConversationsSession`] Object). The Go mail-bridge
//! `--no-default-features` build drops it — a mail bridge has no share UI — the
//! same Go-incompatibility reason as `conversations-session` / `folders-author`,
//! so it owes **no** Go binding regen.

use std::sync::Arc;

use fauna_client_folders::{FoldersClient, decline_folder_share};
use fauna_client_inbox::{
    FolderPendingShare, InboxClient, PENDING_SHARE_PEEK_LIMIT, list_folder_pending_shares,
};
use fauna_conversations::ConversationsSession;

use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// One staged, not-yet-accepted cross-user folder share the recipient renders
/// as a `folder-pending-share` — the UniFFI projection of
/// [`FolderPendingShare`], carrying only the display + action metadata (the raw
/// Welcome bytes stay server-side: [`folders_accept_share`] re-resolves them by
/// `inbox_id`, so the large blob never crosses the FFI boundary).
#[derive(uniffi::Record)]
pub struct FfiPendingShare {
    /// The durable-inbox row id — the accept/decline target.
    pub inbox_id: i64,
    /// The nest-stamped sharer (hex ActorId), when present. `None` for an
    /// unstamped / cross-nest-relayed share. Prefer [`Self::shared_by_handle`]
    /// for display; fall back to a shortened form of this when the handle is
    /// absent.
    pub shared_by: Option<String>,
    /// The sharer's handle — bare for a same-nest sharer, paired with
    /// [`Self::shared_by_domain`] for a verified cross-nest one. Render
    /// [`Self::shared_by_display`], never this.
    pub shared_by_handle: Option<String>,
    /// The cross-nest sharer's handle domain (`None` same-nest) — the half of
    /// the owner label the display joins (`federation.md` § Cross-nest shared
    /// folders + channel append → *The cross-nest owner label*).
    #[uniffi(default = None)]
    pub shared_by_domain: Option<String>,
    /// The pre-computed "Shared by ‹…›" label — render this, never re-derive:
    /// the canonical `handle@domain` for a verified cross-nest sharer, the
    /// handle for a local one, else the canonical `short_id` of the sharer hex
    /// (`fauna_core::format::account_display_label` over `qualified_handle`,
    /// computed in `fauna-client-inbox`). Empty string only when the share is fully
    /// unstamped (no `shared_by` either) — render the client's unknown-sharer
    /// i18n label then.
    pub shared_by_display: String,
    /// The MLS app-level group id (hex), when the Welcome carried one.
    pub group_id: Option<String>,
    /// The fauna channel id (hex).
    pub channel_id: Option<String>,
    /// The shared set's name, resolved by its HOME nest from the claimed row
    /// (`WelcomeInbox.set_name`) — names the pending-share row. `None` when the share was unstamped or the best-effort
    /// claimed-row resolve missed ⇒ render the client's unknown-set fallback label.
    pub set_name: Option<String>,
}

impl From<FolderPendingShare> for FfiPendingShare {
    fn from(s: FolderPendingShare) -> Self {
        Self {
            inbox_id: s.inbox_id,
            shared_by: s.shared_by,
            shared_by_handle: s.shared_by_handle,
            shared_by_domain: s.shared_by_domain,
            shared_by_display: s.shared_by_display,
            group_id: s.group_id,
            channel_id: s.channel_id,
            set_name: s.set_name,
        }
    }
}

/// List the recipient's staged folder shares (the knocked, un-acked
/// `channel_type == "folder"` welcomes) — the `folder-pending-share` list.
/// A **peek** (never acks): listing never consumes a knock.
#[fauna_uniffi_async::export]
pub async fn folders_pending_shares(
    nest: Arc<FfiNestClient>,
) -> Result<Vec<FfiPendingShare>, FfiError> {
    let inbox = InboxClient::new(nest.nest_arc());
    let shares = list_folder_pending_shares(&inbox, PENDING_SHARE_PEEK_LIMIT)
        .await
        .map_err(general_err)?;
    Ok(shares.into_iter().map(FfiPendingShare::from).collect())
}

/// Accept a staged folder share (`folder-share-accept-button`): resolve the
/// Welcome by `inbox_id`, join the MLS group **off the chat rail**
/// ([`ConversationsSession::join_folder_welcome`] — no chat thread), then `ack`
/// the durable row. Accept **bypasses the contact gate** — the user has
/// explicitly accepted (the gate only decides *pushed* welcomes). Crash-safe: the
/// ack happens only after the join succeeds, and the join is idempotent, so a
/// crash between them re-lists the share and re-accepting is a no-op.
///
/// The peek, the unjoinable refusal and that load-bearing join-before-ack
/// ordering all come from the shared `fauna_client_folders::accept_folder_share`
/// — the same recipe tui, linux and the wasm `foldersAcceptShare` twin run, so
/// the ordering is stated once (priority #2). Only the join itself is supplied
/// here, because this face holds its MLS handle as a `ConversationsSession`.
#[fauna_uniffi_async::export]
pub async fn folders_accept_share(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    inbox_id: i64,
) -> Result<(), FfiError> {
    let inbox = InboxClient::new(nest.nest_arc());
    fauna_client_folders::accept_folder_share(&inbox, inbox_id, |join| async move {
        let (channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx) = join.into_join_args();
        session
            .join_folder_welcome(channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx)
            .await
            .map(|_| ())
    })
    .await
    .map_err(general_err)?;
    Ok(())
}

/// Decline a staged folder share (`folder-share-decline-button`): drop the
/// recipient's roster row, then `ack` the durable row — the Welcome is dropped
/// **unprocessed**, so declining **never joins** the group (`folders.md`
/// § Sharing: "declining never joins the group"). The roster drop is what keeps
/// the owner's "Shared with" list honest and a later re-share a genuine re-invite
/// (§ Sharing → *Adding the 2nd..Nth member*). Both halves — and the load-bearing
/// leave-before-ack ordering — live in the shared
/// [`fauna_client_folders::decline_folder_share`], so the web SPA and the four
/// native apps run one recipe (priority #2).
#[fauna_uniffi_async::export]
pub async fn folders_decline_share(
    nest: Arc<FfiNestClient>,
    inbox_id: i64,
) -> Result<(), FfiError> {
    decline_folder_share(
        &InboxClient::new(nest.nest_arc()),
        &FoldersClient::new(nest.nest_arc()),
        inbox_id,
    )
    .await
    .map_err(general_err)?;
    Ok(())
}

/// Leave a shared folder that was shared *with* the caller
/// (`folder-leave-button`, `folders.md` § Sharing: "a `folder-leave-button` …
/// to remove yourself"). The recipient counterpart to the owner-side
/// `folders_remove_member`: **self-scoped** — it drops only the caller's own row
/// and never touches another member, so it needs **no** `ownerSecret` and does
/// **not** rotate the owner's content key (a voluntary leaver keeps the generations
/// they already held — `mls-group-key-material.md` § M2). Addressed by the raw MLS
/// `group_id` (hex) the caller holds in their member-visible
/// `FolderSummary.mls_group_id` (B3), not the owner-only set name.
///
/// The shared [`fauna_client_folders::leave_share`] sequence — nest
/// roster-drop first (the durable, security-meaningful half: off the roster,
/// `content_key.get` is denied, so they stop receiving content-key
/// rotations), then the local MLS-group forget. Both steps are idempotent, so
/// a re-run after a partial failure converges.
#[fauna_uniffi_async::export]
pub async fn folders_leave(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    group_id: String,
) -> Result<(), FfiError> {
    fauna_client_folders::leave_share(nest.nest_arc(), &session, group_id)
        .await
        .map_err(general_err)
}
