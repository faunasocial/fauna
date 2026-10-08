//! Pending actions system: delayed destructive operations with quorum support.
//!
//! Destructive operations (account deletion, handle changes, admin changes, etc.)
//! are not executed immediately. Instead, a `PendingAction` row is created with an
//! `execute_after` timestamp. A background executor polls for ready actions and
//! runs them. Admins or users can cancel pending actions within the delay window.

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::admin::RosterWrite;
use crate::routes::AppState;

/// All action types that can be queued as pending actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionType {
    /// User changes their handle (username).
    HandleChange,
    /// User deletes a specific snapshot.
    SnapshotDelete,
    /// Automatic snapshot-retention prune of one folder's out-of-policy
    /// snapshots, scheduled by the nest itself
    /// (`backup::prune::schedule_auto_prune`) — no person asks for it.
    SnapshotBulkPrune,
    /// Automatic version-retention prune of one folder's out-of-bounds file
    /// versions (`file-versions.md` § Retention (3) — the version plane's
    /// `SnapshotBulkPrune` twin, scheduled by
    /// `backup::version_prune::schedule_version_auto_prune`).
    VersionBulkPrune,
    /// User requests account deletion.
    AccountDelete,
    /// Admin permanently deletes a user account.
    AdminDeleteUser,
    /// Admin bulk-deletes multiple users.
    AdminBulkDeleteUsers,
    /// Admin adds another admin.
    AdminAdd,
    /// Admin removes another admin.
    AdminRemove,
    /// Admin changes another admin's role.
    AdminChangeRole,
    /// Admin overrides the backup purge policy.
    AdminBackupPurgeOverride,
}

impl ActionType {
    /// Delay in seconds before this action becomes eligible for execution.
    pub fn delay_secs(&self) -> i64 {
        match self {
            ActionType::HandleChange => 6 * 3600,              // 6 hours
            ActionType::SnapshotDelete => 48 * 3600,           // 48 hours
            ActionType::SnapshotBulkPrune => 7 * 24 * 3600,    // 7 days
            ActionType::VersionBulkPrune => 7 * 24 * 3600,     // 7 days
            ActionType::AccountDelete => 14 * 24 * 3600,       // 14 days
            ActionType::AdminDeleteUser => 7 * 24 * 3600,      // 7 days
            ActionType::AdminBulkDeleteUsers => 7 * 24 * 3600, // 7 days
            ActionType::AdminAdd => 24 * 3600,                 // 1 day
            ActionType::AdminRemove => 24 * 3600,              // 1 day
            ActionType::AdminChangeRole => 24 * 3600,          // 1 day
            ActionType::AdminBackupPurgeOverride => 30 * 24 * 3600, // 30 days
        }
    }

    /// Whether this action requires quorum approval before it can execute.
    /// Returns the minimum number of distinct approvals needed (0 = no quorum).
    pub fn requires_quorum(&self) -> i64 {
        match self {
            ActionType::AdminRemove => 2,
            ActionType::AdminAdd | ActionType::AdminChangeRole => 1,
            ActionType::AdminBulkDeleteUsers => 2,
            ActionType::AdminBackupPurgeOverride => 2,
            _ => 0,
        }
    }

    /// Canonical string representation stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionType::HandleChange => "handle.change",
            ActionType::SnapshotDelete => "snapshot.delete",
            ActionType::SnapshotBulkPrune => "snapshot.bulk_prune",
            ActionType::VersionBulkPrune => "version.bulk_prune",
            ActionType::AccountDelete => "account.delete",
            ActionType::AdminDeleteUser => "admin.delete_user",
            ActionType::AdminBulkDeleteUsers => "admin.bulk_delete_users",
            ActionType::AdminAdd => "admin.add",
            ActionType::AdminRemove => "admin.remove",
            ActionType::AdminChangeRole => "admin.change_role",
            ActionType::AdminBackupPurgeOverride => "admin.backup_purge_override",
        }
    }

    /// Parse from the canonical string stored in the database.
    // reason: returns Option (unknown tag → None), not Result, so the
    // `FromStr` trait doesn't fit; the name mirrors `as_str` above.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "handle.change" => Some(ActionType::HandleChange),
            "snapshot.delete" => Some(ActionType::SnapshotDelete),
            "snapshot.bulk_prune" => Some(ActionType::SnapshotBulkPrune),
            "version.bulk_prune" => Some(ActionType::VersionBulkPrune),
            "account.delete" => Some(ActionType::AccountDelete),
            "admin.delete_user" => Some(ActionType::AdminDeleteUser),
            "admin.bulk_delete_users" => Some(ActionType::AdminBulkDeleteUsers),
            "admin.add" => Some(ActionType::AdminAdd),
            "admin.remove" => Some(ActionType::AdminRemove),
            "admin.change_role" => Some(ActionType::AdminChangeRole),
            "admin.backup_purge_override" => Some(ActionType::AdminBackupPurgeOverride),
            _ => None,
        }
    }

    /// Whether this action type is an admin management action (affects admin roster).
    pub fn is_admin_management(&self) -> bool {
        matches!(
            self,
            ActionType::AdminAdd
                | ActionType::AdminRemove
                | ActionType::AdminChangeRole
                | ActionType::AdminBackupPurgeOverride
        )
    }

    /// Whether the nest itself schedules this action, on no person's request:
    /// the automatic snapshot- and version-retention prunes. Such an action
    /// rings no security notice at any step ([`notify_transition`]) — a notice
    /// that fires on the nest's own housekeeping is noise that teaches the user
    /// to ignore the ones that matter.
    pub fn is_nest_scheduled(&self) -> bool {
        matches!(
            self,
            ActionType::SnapshotBulkPrune | ActionType::VersionBulkPrune
        )
    }

    /// Whether this action type is an admin action against a user (not admin roster).
    pub fn is_admin_action_against_user(&self) -> bool {
        matches!(
            self,
            ActionType::AdminDeleteUser | ActionType::AdminBulkDeleteUsers
        )
    }
}

/// A row from the `pending_actions` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingActionRow {
    pub id: i64,
    pub action_type: String,
    /// The actor who initiated this action.
    pub actor_id: Vec<u8>,
    /// Optional target identifier (e.g. user actor_id hex or handle, snapshot id).
    pub target: Option<String>,
    /// Optional JSON payload with action-specific parameters.
    pub payload: Option<String>,
    /// Current status: "pending", "executed", "cancelled", "expired".
    pub status: String,
    pub created_at: i64,
    pub execute_after: i64,
    pub executed_at: Option<i64>,
    /// The actor_id of whoever cancelled this action (if cancelled).
    pub cancelled_by: Option<Vec<u8>>,
    pub cancelled_at: Option<i64>,
    /// Minimum number of approvals required before execution (0 = no quorum needed).
    pub requires_quorum: i64,
    /// JSON array of actor_id hex strings that have approved this action.
    pub approvals: String,
    /// IP address of the requestor at creation time (for audit trail).
    pub ip_address: Option<String>,
    /// SHA-256 chain hash linking this action to the previous one.
    pub chain_hash: Option<Vec<u8>>,
}

// ==================== Security notices ====================

/// One lifecycle step of a pending action, as a security notice reports it.
#[derive(Debug, Clone, Copy)]
pub enum Transition<'a> {
    /// The action entered its delay window.
    Created,
    /// The executor ran it.
    Executed,
    /// Someone the cancel-authorization matrix admits called it off
    /// (`CacheDb::cancel_pending_action`); `by` is their actor id.
    Cancelled { by: &'a [u8] },
    /// The delay window closed short of the approvals the action needed, so
    /// the executor expired it (`execute_ready_actions`) and nothing changed.
    Expired,
}

/// How many approvals a person-initiated action needs before it may execute
/// (ruled by the user 2026-09-24): the type's nominal quorum
/// ([`ActionType::requires_quorum`]) **capped at the peers who could concur**
/// — every admin other than the creator (self-approval is refused,
/// `CacheDb::approve_pending_action`) and other than the target — and
/// **floored at one approval whenever any admin other than the creator
/// exists**. The floor is what keeps a two-admin removal from executing on
/// the creator's word alone: the one admin who can give that approval is the
/// target, so their approval is consent (the approve door refuses only the
/// creator) and their cancel stays their veto; an unconsented removal
/// expires. Only a sole admin's action needs nobody — there is nobody to ask —
/// which is what lets the ratified co-admin instrument (`admin.md` § Admin
/// continuity and succession) work on the deployment it exists for.
///
/// Per action, not per type: the dormant types follow the same formula.
/// Stored on the row at scheduling (`PendingActionRow::requires_quorum`), so
/// what the admin console shows as "needs N approvals" is what the executor
/// will test. Owner: `nest/common.md` § Pending Actions System.
pub fn effective_quorum(
    action_type: &ActionType,
    admins: &[Vec<u8>],
    creator: &[u8],
    target: Option<&[u8; 32]>,
) -> i64 {
    let nominal = action_type.requires_quorum();
    if nominal == 0 {
        return 0;
    }
    let others = admins.iter().filter(|a| a.as_slice() != creator);
    let peers = others
        .clone()
        .filter(|a| target.is_none_or(|t| a.as_slice() != &t[..]))
        .count() as i64;
    let others = others.count() as i64;
    nominal.min(peers).max(others.min(1))
}

/// The one account an admin action names, when its `target` is an actor id
/// (the against-user actions and the roster actions; a handle-change or
/// snapshot target is not one).
fn named_account(action: &PendingActionRow) -> Option<[u8; 32]> {
    let action_type = ActionType::from_str(&action.action_type)?;
    if !(action_type.is_admin_action_against_user() || action_type.is_admin_management()) {
        return None;
    }
    action
        .target
        .as_deref()
        .and_then(|t| fauna_core::hex32::decode(t).ok())
        .filter(|t| t[..] != action.actor_id[..])
}

/// Schedule a **person-initiated** pending action: create the row (recording
/// the caller's address when the connection captured one, and the
/// [`effective_quorum`] the current roster can meet), read it back, and ring
/// its creation notices. Every user and admin creator goes through here; the
/// nest's own scheduled prunes call `CacheDb::create_pending_action` directly,
/// and would ring nothing anyway ([`ActionType::is_nest_scheduled`]).
pub async fn schedule(
    state: &Arc<AppState>,
    action_type: &ActionType,
    actor_id: &[u8],
    target: Option<&str>,
    payload: Option<&str>,
) -> Result<PendingActionRow> {
    let ip = crate::dispatch_core::current_caller_ip();
    let quorum = if action_type.requires_quorum() > 0 {
        let admins: Vec<Vec<u8>> = state
            .db
            .list_admin_actors()
            .await?
            .into_iter()
            .map(|(actor, _added_at)| actor)
            .collect();
        let target_id = target.and_then(|t| fauna_core::hex32::decode(t).ok());
        effective_quorum(action_type, &admins, actor_id, target_id.as_ref())
    } else {
        0
    };
    let id = state
        .db
        .create_pending_action_with_quorum(
            action_type,
            actor_id,
            target,
            payload,
            ip.as_deref(),
            quorum,
        )
        .await?;
    let row = state
        .db
        .get_pending_action(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pending action not found after creation"))?;
    notify_transition(state, &row, Transition::Created).await;
    Ok(row)
}

/// Ring the security notices one lifecycle step of `action` owes
/// (`behavior/notifications.md` § Security notices → *Pending actions*). The
/// audience is **everyone the cancel-authorization matrix admits**
/// (`CacheDb::cancel_pending_action`), each told once per step:
///
/// - the **creator**, of every step — `PendingActionCreated`,
///   `ActionExecuted`, `ActionCancelled`, `ActionExpired` — from their own
///   Settings → Pending actions;
/// - the **target** of an admin action against their account
///   (`admin.delete_user`, …): `PendingActionAgainstYou` at scheduling (their
///   Settings → Pending actions lists it, `CacheDb::list_pending_actions_for_actor`),
///   `ActionCancelled` / `ActionExpired` after, and `AdminChange` when an
///   action naming one account executes (a deleted account hears nothing);
/// - every **co-admin** (the roster minus the creator) of an admin action of
///   either kind: `AdminActionPending` at scheduling — the notice that makes a
///   quorum approval possible at all — then `AdminChange` / `AdminActionCancelled`
///   / `AdminActionExpired`, acted on from the admin console's pending-actions
///   section. The target of a roster action is an admin and hears it this way.
///
/// Nobody is told anything about the nest's own scheduled prunes.
///
/// Awaited inline rather than spawned: every step is rare, `notify` never
/// fails (its channels log), and the caller's reply then implies the notice is
/// already on the user's list.
pub async fn notify_transition(
    state: &Arc<AppState>,
    action: &PendingActionRow,
    transition: Transition<'_>,
) {
    use crate::security_notify::SecurityEvent;

    let Some(action_type) = ActionType::from_str(&action.action_type) else {
        return;
    };
    if action_type.is_nest_scheduled() {
        return;
    }
    let creator = <[u8; 32]>::try_from(action.actor_id.as_slice()).ok();
    let target = named_account(action);
    let is_admin_action =
        action_type.is_admin_action_against_user() || action_type.is_admin_management();

    // ── The creator ──
    let own_event = match transition {
        Transition::Created => SecurityEvent::PendingActionCreated {
            action_id: action.id,
            action_type: action.action_type.clone(),
            execute_after: action.execute_after,
        },
        Transition::Executed => SecurityEvent::ActionExecuted {
            action_id: action.id,
            action_type: action.action_type.clone(),
        },
        Transition::Cancelled { by } => SecurityEvent::ActionCancelled {
            action_id: action.id,
            action_type: action.action_type.clone(),
            cancelled_by: account_label(state, by).await,
        },
        Transition::Expired => SecurityEvent::ActionExpired {
            action_id: action.id,
            action_type: action.action_type.clone(),
        },
    };
    if let Some(creator) = creator {
        notify_if_present(state, &creator, &own_event).await;
    }

    // ── The target ──
    let mut target_told = false;
    if let Some(target) = target {
        let against_user = action_type.is_admin_action_against_user();
        let event = match transition {
            Transition::Created if against_user => Some(SecurityEvent::PendingActionAgainstYou {
                action_id: action.id,
                action_type: action.action_type.clone(),
                execute_after: action.execute_after,
                by: account_label(state, &action.actor_id).await,
            }),
            // The target of a roster action is an admin: told as a co-admin below.
            Transition::Created => None,
            // A just-deleted account has nobody left to read the notice.
            Transition::Executed if action_type != ActionType::AdminDeleteUser => {
                Some(SecurityEvent::AdminChange {
                    change_type: action.action_type.clone(),
                    target: account_label(state, &target).await,
                })
            }
            Transition::Executed => None,
            // "on your account" reads right for the target too.
            Transition::Cancelled { .. } | Transition::Expired if against_user => {
                Some(own_event.clone())
            }
            Transition::Cancelled { .. } | Transition::Expired => None,
        };
        if let Some(event) = event {
            notify_if_present(state, &target, &event).await;
            target_told = true;
        }
    }

    // ── The co-admins ──
    if !is_admin_action {
        return;
    }
    let admins = match state.db.list_admin_actors().await {
        Ok(admins) => admins,
        Err(e) => {
            tracing::warn!(
                action_id = action.id,
                "co-admin notice: list_admin_actors failed: {e:#}"
            );
            return;
        }
    };
    let target_label = match target {
        Some(t) => account_label(state, &t).await,
        None => action.target.clone().unwrap_or_default(),
    };
    let approvals: Vec<String> = serde_json::from_str(&action.approvals).unwrap_or_default();
    let event = match transition {
        Transition::Created => SecurityEvent::AdminActionPending {
            action_id: action.id,
            action_type: action.action_type.clone(),
            execute_after: action.execute_after,
            by: account_label(state, &action.actor_id).await,
            target: target_label,
            approvals_needed: (action.requires_quorum - approvals.len() as i64).max(0),
        },
        Transition::Executed => SecurityEvent::AdminChange {
            change_type: action.action_type.clone(),
            target: target_label,
        },
        Transition::Cancelled { by } => SecurityEvent::AdminActionCancelled {
            action_id: action.id,
            action_type: action.action_type.clone(),
            target: target_label,
            cancelled_by: account_label(state, by).await,
        },
        Transition::Expired => SecurityEvent::AdminActionExpired {
            action_id: action.id,
            action_type: action.action_type.clone(),
            target: target_label,
        },
    };
    for (admin, _added_at) in admins {
        let Ok(admin) = <[u8; 32]>::try_from(admin.as_slice()) else {
            continue;
        };
        if Some(admin) == creator {
            continue;
        }
        // A target already told above (an against-user target who is also an
        // admin; the just-promoted admin of an executed grant) hears it once.
        if target_told && Some(admin) == target {
            continue;
        }
        notify_if_present(state, &admin, &event).await;
    }
}

/// Notify `actor` only while its account exists — a just-executed deletion
/// leaves nobody to read the notice.
async fn notify_if_present(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    event: &crate::security_notify::SecurityEvent,
) {
    if matches!(state.db.get_user(actor).await, Ok(Some(_))) {
        state.security_notifier.notify(state, actor, event).await;
    }
}

/// How a notice names an account: its handle when it has one, else the actor
/// id in hex.
async fn account_label(state: &AppState, actor: &[u8]) -> String {
    let handle = match <[u8; 32]>::try_from(actor) {
        Ok(id) => state.db.get_handle(&id).await.ok().flatten(),
        Err(_) => None,
    };
    match handle {
        Some(handle) if !handle.is_empty() => handle,
        _ => hex::encode(actor),
    }
}

// ==================== Background Executor ====================

/// Retract every post the actor authored, through the **same per-post path** a
/// user's own `fauna.posts.delete` takes (`routes::delete_post_core`).
///
/// `account-data-plane.md` § Nest-side requirements item 1 states why posts sit
/// at `Policy::Retain` in `db::actor_tables::ACTOR_TABLES`: they "need the
/// existing federation-aware per-post retraction, not a raw purge". This is the
/// leg that performs it — before this existed, nothing did, so a deleted
/// account's posts stayed live and servable forever.
///
/// **Why the per-post path rather than a bulk `DELETE`.** `delete_post_core` is
/// the one owner of the removal order (projection rows first, so a crash never
/// serves an empty-bodied ghost) *and* of all four federation legs — nostr
/// kind-5, Bluesky write-through, paired-replica twin, ActivityPub `Delete`.
/// A bulk delete would be a second implementation of that order, free to drift;
/// `bridge_withdraw` already refuses exactly this shortcut for user data ("a
/// local account's own post is user data, and `fauna.posts.delete` is the only
/// verb that may destroy it").
///
/// **Why an unsigned `Tombstone` is legitimate here.** The signed tombstone
/// authenticates a *caller* who might not be the author; this call site has
/// already established the author identity out of band — a user-initiated,
/// 14-day-delayed account deletion, or an admin eviction. `interact_routes`'s
/// `unrepost` sets the precedent: it builds the same unsigned tombstone at
/// connection-actor trust level and routes through the same core. Both author
/// checks inside the core still run and still bind (the tombstone author, the
/// stored post's author), so this cannot become a way to destroy someone
/// else's post.
///
/// **The web-render leg (`delete_post_core` step 1c) is skipped per post and
/// rendered once for the whole pass instead.** Each call passes
/// [`crate::routes::RenderSite::Skip`], so the inline render never fires; this
/// function renders the actor's site itself, at most once, after the loop —
/// on the success path AND before an early `Err` return — iff the pass
/// retracted at least one still-web-published post. Rendering once per
/// still-published post here (the old behavior) cost up to `MAX_TEMPLATES` ×
/// `MAX_RENDERED_POSTS` `render_bounded` calls (`web_content/service.rs`) for
/// EVERY retracted post, every one of it thrown away the moment the purge
/// sweep below clears `web_rendered` anyway. **One site render per pass,
/// bounded the same way a single-post delete's render is (≤100 templates +
/// ≤1000 post pages, 5 s each), is the real cap** — not "each render is
/// capped" read as a per-post allowance (`web-content-hosting.md` § Routing,
/// render, serving names this pass as its own render trigger). Rendering
/// before an `Err` return, not only on loop success, matters because retries
/// are uncapped (no per-tick bound, see below) and the pending action can
/// also be cancelled by the user: a bare "skip and let some later render
/// catch up" would leave whatever THIS pass retracted stale indefinitely.
///
/// **Ordering is load-bearing, which is why this runs first.** Both witnesses
/// the federation legs depend on are `Policy::Purge`: `nostr_accounts` holds
/// the author's encrypted nsec — the only key that can sign the kind-5
/// retraction — and `ap_post_map` is the ActivityPub `Delete`-push witness.
/// Once the purge sweep below has run, a retraction can never reach the
/// federated copies again; the local rows would go quiet while every remote
/// copy stayed live and unretractable.
///
/// **Partial failure converges rather than being papered over.** A storage
/// failure returns `Err`, which leaves the pending action *unexecuted*
/// (`execute_ready_actions` only marks success), so the executor retries it on
/// the next tick — and `delete_post_core` is idempotent (`AlreadyGone`), so the
/// already-retracted posts are cheap no-ops. Proceeding past a failure would
/// destroy the signing key mid-way and forfeit the retraction permanently. A
/// post whose *stored* author disagrees with the `content.author` row that
/// listed it (`NotAuthor`) is a corrupt row rather than a retryable failure:
/// it is logged loudly and skipped, because blocking a user's account deletion
/// forever on an inconsistent row is the worse failure (`common.md`
/// § Client-state recoverability).
///
/// **Deliberately unbatched, and that was the choice rather than the default.**
/// A per-tick bound would resume naturally on the next tick (each retraction
/// removes the `content` row that listed it, so the next pass enumerates only
/// what is left) — but the only way to *stop* mid-way is to return `Err`, which
/// logs the deletion as a failure every tick and still cannot let the purge
/// proceed, since the purge is exactly what must not run until retraction is
/// complete. So a bound would buy tick latency at the cost of an honest success
/// signal, on a background executor where latency is the cheapest thing we
/// have. If an account large enough to matter ever appears, the shape to reach
/// for is a resumable retraction *state* on the pending action — not a silent
/// cap here, which would purge the keys with posts still un-retracted.
async fn retract_actor_posts(state: &Arc<AppState>, actor_id: &[u8; 32]) -> Result<()> {
    let post_ids = state.db.list_posts_by_author(actor_id).await?;
    if post_ids.is_empty() {
        return Ok(());
    }
    let total = post_ids.len();
    let mut retracted = 0usize;
    let mut skipped = 0usize;
    let mut failure: Option<anyhow::Error> = None;

    for raw in post_ids {
        let digest: [u8; 32] = match raw.as_slice().try_into() {
            Ok(digest) => digest,
            Err(_) => {
                failure = Some(anyhow::anyhow!("content row id is not a 32-byte post id"));
                break;
            }
        };
        let tombstone = fauna_core::data::Tombstone {
            author: fauna_core::identity::ActorId(*actor_id),
            post_id: fauna_core::data::PostId::from_digest_dag_cbor(digest),
            created_at: fauna_core::data::Timestamp::now(),
        };
        match crate::routes::delete_post_core(
            state,
            *actor_id,
            &tombstone,
            digest,
            crate::routes::RenderSite::Skip,
        )
        .await
        {
            Ok(_) => retracted += 1,
            Err(crate::routes::PostDeleteError::NotAuthor) => {
                skipped += 1;
                tracing::error!(
                    post_id = %hex::encode(digest),
                    actor_id = %hex::encode(actor_id),
                    "account deletion: post listed under this author but its stored author \
                     disagrees — skipping retraction (inconsistent row); deletion continues"
                );
            }
            Err(crate::routes::PostDeleteError::Internal(msg)) => {
                failure = Some(anyhow::anyhow!(
                    "account deletion: retracting post {} failed: {msg} — \
                     leaving the action pending so the executor retries before the \
                     purge sweep destroys the keys the federation legs need",
                    hex::encode(digest)
                ));
                break;
            }
        }
    }

    // Render once for the whole pass — see the render-policy note on this
    // function's doc comment — on BOTH exits: the loop running to completion
    // and an early `break` on `failure`. Whatever this pass retracted must
    // stop serving now; the caller may never see a successful pass again
    // (retries are uncapped, and the user can cancel the pending action).
    // Keyed on the owed-render marker each still-published post's delete
    // transaction wrote, so a pass that retracted none renders nothing; fails
    // closed like every revoking door.
    if let Some(wcs) = &state.web_content_service
        && let Err(e) = wcs
            .render_owed(actor_id, "an account-deletion retraction pass")
            .await
    {
        tracing::warn!(
            actor_id = %hex::encode(actor_id),
            error = %e,
            "web render after account-deletion retraction pass failed"
        );
    }

    if let Some(e) = failure {
        return Err(e);
    }

    tracing::info!(
        actor_id = %hex::encode(actor_id),
        total,
        retracted,
        skipped,
        "account deletion: retracted the actor's posts"
    );
    Ok(())
}

/// Finalize a user's removal from this nest: retract the posts they authored,
/// reclaim everything else the actor owns here, then drop the `users` row.
/// Shared by `account.delete` (self-delete), `admin.delete_user`,
/// `admin.bulk_delete_users` and the eviction ladder's deletion step
/// (`eviction::run_eviction_tick`) so the cleanup is defined once.
///
/// Takes `&Arc<AppState>` rather than the bare `&Arc<CacheDb>` it used to,
/// because the post-retraction leg needs it — and taking it here is what makes
/// the ordering structural: no caller can reach the purge sweep without having
/// run the retraction first (see [`retract_actor_posts`] for why that order is
/// load-bearing).
///
/// Includes [`crate::db::CacheDb::delete_all_folders_for_actor`] — the reclaim half of
/// holder-side "stop hosting" for a held-for-friends backup guest
/// (`docs/goal/behavior/backup-destinations.md` § Held-for-friends enrollment → "the guest's
/// reserved sets + chunks are **reclaimed**"). Without it a deleted user's
/// reserved folders + `backup_custody` stayed orphaned and the GC reference
/// walk protected their chunks forever, so no space was ever reclaimed.
pub async fn finalize_user_deletion(state: &Arc<AppState>, actor_id: &[u8; 32]) -> Result<()> {
    let db = &state.db;
    // Family-safety fail-safe (family-safety.md § Lifecycle gates): a guardian
    // account must never be finalized while guardianship links reference it —
    // the scheduling handlers gate this, but a link created between scheduling
    // and execution would otherwise strand the ward. Refusing here keeps the
    // stranded state unrepresentable; the admin resolves (transfer/graduate)
    // and re-schedules.
    if !db.list_wards(actor_id).await?.is_empty() {
        anyhow::bail!(
            "actor still guards supervised accounts — resolve the guardianship links \
             (fauna.family.transfer / fauna.family.graduate) before deletion"
        );
    }
    // Admin fail-safe (common.md § Client-state recoverability). `is_admin` reads
    // `admin_actor_ids`, a table `delete_user` does not touch, so dropping an
    // admin's `users` row leaves the admin row orphaned. The claim gate keys on
    // `admin_count > 0` (claim_core.rs), so the box then reports *claimed* while
    // its only admin can no longer authenticate — `check_actor_active` refuses a
    // token to an actor with no `users` row once the current bearer expires. That
    // is an off-box brick, fixable only by DB surgery.
    //
    // The scheduling handlers gate this (`require_not_admin` on
    // `fauna.admin.users.delete`, the `admin_role_held` refusal on
    // `fauna.account.delete`), but pending actions are **persisted**: an
    // `AccountDelete` queued for an actor who was promoted to admin after it was
    // scheduled is still pending, and no handler can reach backwards in time. Refusing here is what makes the orphan unrepresentable.
    //
    // Refuse rather than strip the role: a failed action is never marked executed
    // (`execute_ready_actions`), so this retries each tick and self-heals the
    // moment the actor is demoted — whereas stripping it would silently return a
    // *populated* nest to fresh/unclaimed, where any stranger who reaches the box
    // may claim it and inherit the existing users' data.
    if db.is_admin(&actor_id[..]).await? {
        anyhow::bail!(
            "actor still holds the admin role — remove it (fauna.admin.admins.remove) before \
             deletion; a sole superadmin cannot be demoted, so its exit is fauna.admin.factory_reset"
        );
    }
    // The same refusal over the actor's local predecessors: the purge walk below
    // drops each predecessor's `admin_actor_ids` row and `delete_user` its
    // `users` row, so a predecessor's admin row would otherwise leave without
    // `admin.remove`'s quorum or the superadmin floor — the last superadmin
    // included (`admin.md` § Admin continuity and succession). Refuse, never
    // strip, for the reason above; `admin.remove` on the predecessor unblocks it.
    let holders = db.local_predecessors_holding_admin(actor_id).await?;
    if let Some(held) = holders.first() {
        anyhow::bail!(
            "a retired identity of this actor ({}) still holds the admin role — remove it \
             (fauna.admin.admins.remove) before deletion",
            hex::encode(held)
        );
    }
    // Retract the actor's posts BEFORE anything below destroys the keys and
    // witnesses the federation legs need (`nostr_accounts`, `ap_post_map` — both
    // `Policy::Purge`). Deliberately placed *after* the two fail-safes above,
    // which refuse the deletion outright: retracting first would destroy the
    // user's content for a deletion that then refuses and retries forever.
    // Full reasoning in `retract_actor_posts`.
    retract_actor_posts(state, actor_id).await?;
    db.delete_inbox_for_actor(actor_id).await?;
    db.delete_sync_devices_for_actor(actor_id).await?;
    db.delete_eviction_tokens(actor_id).await?;
    db.delete_all_folders_for_actor(actor_id).await?;
    // Family cascade: a supervised account's link + policy go with it, and any
    // pending transfer naming the deleted actor (as ward or proposed guardian).
    db.delete_family_rows_for_supervised(actor_id).await?;
    // Mailbox-export blobs are FILES, and the registry sweep below deletes only
    // the rows that name them — so they are unlinked first, while the rows
    // still say where they are, and a failed unlink fails this pass with its
    // row intact for the retry (`mail-export.md` § Reclaim).
    crate::mail_export_blobs::unlink_export_blobs_for_actor(state, actor_id).await?;
    // The enumerable per-actor boundary: every
    // Policy::Purge table in db::actor_tables::ACTOR_TABLES, minus the
    // Policy::Retain tables (posts, financial/entitlement records,
    // identity-lifecycle audit trails) that need business logic or a
    // product ruling instead of a raw delete. Runs after the helpers above
    // so their more targeted deletes are unaffected; re-covering their
    // tables here is a zero-row no-op.
    db.purge_orphaned_actor_rows(actor_id).await?;
    db.delete_user(actor_id).await?;
    Ok(())
}

/// Finalize a user's **own** deletion (`account.delete`) — the one deletion
/// path that also answers for the rooms the user owns.
///
/// A user who owns a ceremony-born room in which another member homed here
/// could take ownership over is refused until they transfer
/// (`conversation-rooms.md` § Roles and authorization, the owner rule). This is
/// the executor's half of the `room_ownership_held` refusal at the scheduling
/// door: a persisted deletion can outlive that door, and a member can join
/// between scheduling and execution. It refuses rather than proceeding — a
/// failed action is never marked executed, so it retries each tick and
/// completes the moment the owner transfers, or ends when the user cancels.
///
/// Deliberately NOT inside [`finalize_user_deletion`], which the admin paths
/// share: an admin deleting a user holds no door that could transfer that
/// user's room, so refusing there would let any user make themselves
/// undeletable by seating one other account.
pub async fn finalize_self_deletion(state: &Arc<AppState>, actor_id: &[u8; 32]) -> Result<()> {
    let held = state
        .db
        .rooms_awaiting_an_ownership_transfer(actor_id)
        .await?;
    if !held.is_empty() {
        anyhow::bail!(
            "actor still owns {} room(s) another member could take over — transfer ownership \
             (fauna.conversations.room.transfer_ownership) before deletion",
            held.len()
        );
    }
    finalize_user_deletion(state, actor_id).await
}

/// Revoke a deleted actor's authority: kill its bearer tokens, then close the
/// WebSockets it already holds.
///
/// Deletion used to do neither. The actor kept a valid bearer until its TTL
/// expired (`auth_core::TOKEN_TTL_SECS`, 1 h) and its live socket kept
/// dispatching indefinitely — `caller_class_for_actor` resolves any non-zero
/// actor to `CallerClass::User` without checking that a `users` row exists, so
/// dropping the row did not even downgrade the connection. The executor could
/// not have fixed this before: it took only `&Arc<CacheDb>` and never saw the
/// token store or the WS registry.
///
/// Per `transport.md` § Revocation teardown; both halves are required (see
/// [`crate::ws::WsState::disconnect_actor`]).
async fn revoke_deleted_actor(state: &Arc<AppState>, actor_id: &[u8; 32]) {
    state.revoke_actor_authority(actor_id).await;
}

/// Execute a specific action based on its type.
///
/// Some action types depend on methods from later tasks (soft_delete_snapshot,
/// suspend_user, set_admin_role) and are stubbed with a warning log until those
/// tasks are implemented.
pub async fn execute_action(state: &Arc<AppState>, action: &PendingActionRow) -> Result<()> {
    let db = &state.db;
    match action.action_type.as_str() {
        "account.delete" => {
            let actor_id: [u8; 32] = action
                .actor_id
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid actor_id length for account_delete"))?;
            finalize_self_deletion(state, &actor_id).await?;
            revoke_deleted_actor(state, &actor_id).await;
        }
        "handle.change" => {
            let actor_id: [u8; 32] = action
                .actor_id
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid actor_id length for handle_change"))?;
            let payload_str = action
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("handle_change missing payload"))?;
            let payload: serde_json::Value = serde_json::from_str(payload_str)
                .map_err(|e| anyhow::anyhow!("handle_change payload parse error: {e}"))?;
            let new_handle = payload["new_handle"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("handle_change payload missing 'new_handle'"))?;
            db.set_handle(&actor_id, new_handle).await?;
            // The ATProto handle is derived from the Fauna handle at read time,
            // so this write is what a rename IS as far as the PDS bridge is
            // concerned (`atproto-pds-bridge.md` § Identity — handle changes
            // follow the Fauna handle). Nudge the bridge to reconcile now
            // instead of at its next poll; the nudge is latency only, exactly
            // like the outbound_ready pattern — the bridge compares the derived
            // handle against the one it published on every pass, so a dropped
            // push can delay a rename but never lose it.
            crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(
                state,
                Some(actor_id),
            )
            .await;
        }
        "snapshot.delete" => {
            let target_str = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("snapshot_delete missing target snapshot_id"))?;
            let snapshot_id: i64 = target_str
                .parse()
                .map_err(|e| anyhow::anyhow!("snapshot_delete invalid snapshot_id: {e}"))?;
            db.soft_delete_snapshot(snapshot_id).await?;
        }
        "admin.delete_user" => {
            let target_hex = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_delete_user missing target"))?;
            let actor_id: [u8; 32] = fauna_core::hex32::decode(target_hex)
                .map_err(|e| anyhow::anyhow!("admin_delete_user invalid target hex: {e}"))?;
            finalize_user_deletion(state, &actor_id).await?;
            revoke_deleted_actor(state, &actor_id).await;
        }
        "admin.add" => {
            let target_hex = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_add missing target"))?;
            let target_bytes = hex::decode(target_hex)
                .map_err(|e| anyhow::anyhow!("admin_add invalid target hex: {e}"))?;
            // `Admin ⊇ User` fail-safe, the twin of `finalize_user_deletion`'s
            // admin refusal (api-layers.md § `Admin ⊇ User`). The door guard
            // (`admin_ws_handlers::require_registered_user`) is not sufficient on
            // its own: a pending action can outlive the upgrade that added the
            // door, and the target's `users` row can be deleted during the
            // action's delay window. Refuse rather than promote — an admin with
            // no account is invisible to every guard that reasons over `users`,
            // and is the orphaned-admin brick (common.md § Client-state
            // recoverability). Bailing leaves the action pending (the executor
            // logs and retries), so registering the target still lets it apply.
            let target: [u8; 32] = target_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("admin_add target is not a 32-byte actor id"))?;
            if db.get_user(&target).await?.is_none() {
                anyhow::bail!(
                    "admin_add refused: target is not a registered user — \
                     an admin must first hold a `users` row"
                );
            }
            // A retired identity keeps a handle-less `users` row until its
            // successor is deleted, so the check above passes for a key that can
            // never log in again (`refuse_if_superseded`). A recovery ceremony
            // can run inside the action's window, so the door's refusal of a
            // retired key does not cover this; park the grant rather than mint
            // dead weight in `admin_count` and the removal quorum.
            if crate::auth_core::successor_of(state, &target)
                .await
                .map_err(|e| anyhow::anyhow!("admin_add supersession consult failed: {e:?}"))?
                .is_some()
            {
                anyhow::bail!(
                    "admin_add refused: target is a retired (succeeded) identity — \
                     grant the role to its successor instead"
                );
            }
            db.add_admin_actor(&target_bytes).await?;
            // The new admin's pairing rows now name the deployment's topology
            // (`private-mode.md` § Pairing Flow): rebuild the dialer's table.
            crate::nest_sync_worker::refresh_pairing_targets(state).await;
        }
        "admin.remove" => {
            let target_hex = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_remove missing target"))?;
            let target_bytes = hex::decode(target_hex)
                .map_err(|e| anyhow::anyhow!("admin_remove invalid target hex: {e}"))?;
            // The superadmin floor's authoritative refusal lives in the writer
            // (`admin.md` § 2 Users → *Cutting a user off*): the door's 409 is
            // schedule-time-only, and N individually-legal removals scheduled
            // while the roster was full would otherwise all execute after the
            // 24 h delay and empty it — the off-box brick `admin.md:121`
            // declares unrepresentable. Bailing leaves the action pending
            // (the `admin.add` arm's posture): adding another superadmin lets
            // it apply. An already-gone target completes idempotently instead
            // of retrying forever.
            match db.remove_admin_actor(&target_bytes).await? {
                RosterWrite::Applied => {
                    // The per-RPC `is_admin` re-read already denies the demoted
                    // admin's *next* dispatch, but their open socket kept receiving
                    // Push events targeted at them as an admin until it self-closed.
                    // Close it (transport.md § Revocation teardown); they remain a
                    // valid user, so the forced re-auth succeeds and the client
                    // reconnects with plain User authority.
                    if let Ok(target) = <[u8; 32]>::try_from(target_bytes.as_slice()) {
                        state.revoke_actor_authority(&target).await;
                    }
                    // The address-guard exemption lapses with the admin role.
                    crate::nest_sync_worker::refresh_pairing_targets(state).await;
                }
                RosterWrite::NotAnAdmin => {}
                RosterWrite::RefusedLastSuperadmin => anyhow::bail!(
                    "admin_remove refused: removing the last superadmin would \
                     empty the roster — parked until another superadmin exists"
                ),
            }
        }
        "admin.change_role" => {
            let target_hex = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_change_role missing target"))?;
            let actor_id: [u8; 32] = fauna_core::hex32::decode(target_hex)
                .map_err(|e| anyhow::anyhow!("admin_change_role invalid target hex: {e}"))?;
            let payload_str = action
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_change_role missing payload"))?;
            let payload: serde_json::Value = serde_json::from_str(payload_str)
                .map_err(|e| anyhow::anyhow!("admin_change_role payload parse error: {e}"))?;
            let role = payload["role"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("admin_change_role payload missing 'role'"))?;
            // Same floor as `admin.remove`'s arm: demoting the last superadmin
            // empties the tier exactly as deleting them would. The writer
            // refuses; the bail parks the action pending-and-retryable.
            match db.set_admin_role(&actor_id, role).await? {
                RosterWrite::Applied => {
                    // Re-baseline live connections on any role change (superadmin ↔
                    // admin): a demotion must not leave the old, higher authority's
                    // socket open, and distinguishing demote from promote here would
                    // need a pre-read of the old role for no security gain — the
                    // forced re-auth succeeds either way and the client reconnects
                    // with the new role (transport.md § Revocation teardown).
                    state.revoke_actor_authority(&actor_id).await;
                    // No pairing-target refresh, unlike `admin.add` /
                    // `admin.remove`: the table's join asks only whether the
                    // row's actor is an admin, never which role it holds, so a
                    // role change leaves it as it was.
                }
                RosterWrite::NotAnAdmin => {}
                RosterWrite::RefusedLastSuperadmin => anyhow::bail!(
                    "admin_change_role refused: demoting the last superadmin would \
                     empty the roster — parked until another superadmin exists"
                ),
            }
        }
        // The § 7 Layer-2 → Layer-3 hop for an automatic retention prune
        // (`backup-restore.md` § 7 Deletion Safety + § 8 RULING consequence
        // (iii)): the 7-day cancellable window has elapsed, so each target moves
        // to the *soft*-deleted state with its own 30-day `purge_after` window.
        // The terminal step is deliberately `soft_delete_snapshot` — the same
        // one `snapshot.delete` reaches — and NEVER `delete_snapshots`, which is
        // the purge primitive GC applies after `purge_after` has passed. Payload
        // is `{"snapshot_ids": [...]}`, written by
        // `backup::prune::schedule_auto_prune`.
        "snapshot.bulk_prune" => {
            let payload_str = action
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("snapshot_bulk_prune missing payload"))?;
            let payload: serde_json::Value = serde_json::from_str(payload_str)
                .map_err(|e| anyhow::anyhow!("snapshot_bulk_prune payload parse error: {e}"))?;
            let ids = payload["snapshot_ids"].as_array().ok_or_else(|| {
                anyhow::anyhow!("snapshot_bulk_prune payload missing 'snapshot_ids'")
            })?;
            let mut soft_deleted = 0usize;
            for id_val in ids {
                let snapshot_id = id_val.as_i64().ok_or_else(|| {
                    anyhow::anyhow!("snapshot_bulk_prune snapshot_ids entry is not an integer")
                })?;
                db.soft_delete_snapshot(snapshot_id).await?;
                soft_deleted += 1;
            }
            tracing::info!(
                action_id = action.id,
                soft_deleted,
                "auto-prune executed: snapshots soft-deleted, recoverable until purge_after"
            );
        }
        // The version plane's Layer-2 → Layer-3 hop (`file-versions.md`
        // § Retention (3)): the 7-day cancellable window has elapsed, so each
        // target version moves to the *soft*-pruned state — out of the default
        // `versions.list` projection, recoverable for 30 days via
        // `fauna.files.versions.undelete`. The terminal `superseded_at` stamp
        // is deliberately NOT this arm's — it is the GC-cycle purge step's,
        // and only past `purge_after`. `soft_prune_version` skips a row that
        // was superseded or tombstoned in the window (it returns false), so a
        // racing owner supersede never double-marks. Payload is
        // `{"version_seqs": [...]}`, written by
        // `backup::version_prune::schedule_version_auto_prune`.
        "version.bulk_prune" => {
            let payload_str = action
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("version_bulk_prune missing payload"))?;
            let payload: serde_json::Value = serde_json::from_str(payload_str)
                .map_err(|e| anyhow::anyhow!("version_bulk_prune payload parse error: {e}"))?;
            let seqs = payload["version_seqs"].as_array().ok_or_else(|| {
                anyhow::anyhow!("version_bulk_prune payload missing 'version_seqs'")
            })?;
            let mut soft_pruned = 0usize;
            for seq_val in seqs {
                let seq = seq_val.as_i64().ok_or_else(|| {
                    anyhow::anyhow!("version_bulk_prune version_seqs entry is not an integer")
                })?;
                if db.soft_prune_version(seq).await? {
                    soft_pruned += 1;
                }
            }
            tracing::info!(
                action_id = action.id,
                soft_pruned,
                "version auto-prune executed: versions soft-pruned, recoverable until purge_after"
            );
        }
        "admin.bulk_delete_users" => {
            let payload_str = action
                .payload
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_bulk_delete_users missing payload"))?;
            let payload: serde_json::Value = serde_json::from_str(payload_str)
                .map_err(|e| anyhow::anyhow!("admin_bulk_delete_users payload parse error: {e}"))?;
            let actor_ids = payload["actor_ids"].as_array().ok_or_else(|| {
                anyhow::anyhow!("admin_bulk_delete_users payload missing 'actor_ids'")
            })?;
            for id_val in actor_ids {
                let hex_str = id_val
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("actor_ids entry is not a string"))?;
                let actor_id: [u8; 32] = fauna_core::hex32::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("admin_bulk_delete_users invalid hex: {e}"))?;
                finalize_user_deletion(state, &actor_id).await?;
                revoke_deleted_actor(state, &actor_id).await;
            }
        }
        "admin.backup_purge_override" => {
            let target_str = action
                .target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("admin_backup_purge_override missing target"))?;
            let snapshot_id: i64 = target_str.parse().map_err(|e| {
                anyhow::anyhow!("admin_backup_purge_override invalid snapshot_id: {e}")
            })?;
            db.delete_snapshots(&[snapshot_id]).await?;
        }
        other => {
            tracing::warn!(action_type = other, "unhandled pending action type");
        }
    }
    Ok(())
}

/// Process all ready pending actions (status='pending' AND execute_after <= now).
///
/// Returns the count of actions that were executed (not expired). Each row the
/// batch read returned is handed to [`run_ready_action`].
pub async fn execute_ready_actions(state: &Arc<AppState>) -> Result<usize> {
    let actions = state.db.list_ready_pending_actions().await?;
    let mut executed_count = 0usize;
    for action in actions {
        if run_ready_action(state, action).await {
            executed_count += 1;
        }
    }
    Ok(executed_count)
}

/// Run one row of the executor's batch: claim it, then expire it when its
/// quorum is short, otherwise execute it. Returns whether it executed.
///
/// `held` is the batch read's copy, which may be stale by this row's turn: a
/// cancel can land in between. So the row is first claimed (`pending` →
/// `executing`, [`CacheDb::claim_pending_action`]) and every decision below is
/// taken from the row the claim returned; a row no longer `pending` is skipped
/// (`nest/common.md` § Pending Actions System → *The executor claims before
/// it acts*).
///
/// [`CacheDb::claim_pending_action`]: crate::db::CacheDb::claim_pending_action
pub(crate) async fn run_ready_action(state: &Arc<AppState>, held: PendingActionRow) -> bool {
    let db = &state.db;
    let action = match db.claim_pending_action(held.id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::info!(
                action_id = held.id,
                action_type = %held.action_type,
                "pending action no longer pending at its turn; skipped"
            );
            return false;
        }
        Err(e) => {
            tracing::warn!(action_id = held.id, error = %e, "failed to claim pending action");
            return false;
        }
    };
    let approvals: Vec<String> = serde_json::from_str(&action.approvals).unwrap_or_default();

    if action.requires_quorum > 0 && (approvals.len() as i64) < action.requires_quorum {
        // Insufficient approvals — expire
        match db.mark_pending_action_expired(action.id).await {
            Ok(true) => {}
            // The succession disarm cancelled the claimed row first; its
            // cancel stands.
            Ok(false) => return false,
            Err(e) => {
                tracing::warn!(
                    action_id = action.id,
                    error = %e,
                    "failed to mark pending action expired"
                );
                release_claim(state, action.id).await;
                return false;
            }
        }
        let _ = db
            .audit(
                Some(&action.actor_id),
                "pending_action.expired",
                action.target.as_deref(),
                Some(&format!("id={} type={}", action.id, action.action_type)),
            )
            .await;
        tracing::info!(
            action_id = action.id,
            action_type = %action.action_type,
            requires_quorum = action.requires_quorum,
            approvals = approvals.len(),
            "pending action expired (insufficient quorum)"
        );
        notify_transition(state, &action, Transition::Expired).await;
        false
    } else {
        // Execute
        if let Err(e) = execute_action(state, &action).await {
            tracing::warn!(
                action_id = action.id,
                action_type = %action.action_type,
                error = %e,
                "pending action execution failed"
            );
            // Back to `pending`: the next tick retries it, as before the claim.
            release_claim(state, action.id).await;
            return false;
        }
        match db.mark_pending_action_executed(action.id).await {
            Ok(true) => {}
            // The row is gone: an account deletion's run purges its own
            // actor's rows, this one included (`pending_actions` is
            // `Policy::Purge`). It executed.
            Ok(false) if matches!(db.get_pending_action(action.id).await, Ok(None)) => {}
            Ok(false) => {
                // Only the succession disarm moves a claimed row. It could not
                // recall the run already under way; the row keeps its
                // `cancelled`, and the audit log says the action ran anyway.
                let _ = db
                    .audit(
                        Some(&action.actor_id),
                        "pending_action.executed_after_disarm",
                        action.target.as_deref(),
                        Some(&format!("id={} type={}", action.id, action.action_type)),
                    )
                    .await;
                tracing::warn!(
                    action_id = action.id,
                    action_type = %action.action_type,
                    "pending action ran after the succession disarm cancelled it"
                );
                return false;
            }
            Err(e) => {
                // The retry an unguarded mark failure always caused: the row
                // goes back to `pending` and the next tick runs it again (a
                // release that fails too leaves it to the boot reconcile).
                tracing::warn!(
                    action_id = action.id,
                    error = %e,
                    "failed to mark pending action executed"
                );
                release_claim(state, action.id).await;
                return false;
            }
        }
        let _ = db
            .audit(
                Some(&action.actor_id),
                "pending_action.executed",
                action.target.as_deref(),
                Some(&format!("id={} type={}", action.id, action.action_type)),
            )
            .await;
        tracing::info!(
            action_id = action.id,
            action_type = %action.action_type,
            "pending action executed"
        );
        notify_transition(state, &action, Transition::Executed).await;
        true
    }
}

async fn release_claim(state: &Arc<AppState>, id: i64) {
    if let Err(e) = state.db.release_pending_action_claim(id).await {
        tracing::warn!(action_id = id, error = %e, "failed to release pending action claim");
    }
}

/// Spawn a background tokio task that ticks every 60 seconds and processes
/// all ready pending actions.
///
/// Takes the whole [`AppState`], not just the `CacheDb`: finalizing a deletion
/// must also revoke the actor's tokens and close its live WebSockets, which live
/// in `state.auth.token_store` and `state.ws`.
pub fn start_executor(state: Arc<AppState>) {
    state.clone().spawn_scoped(async move {
        // Boot reconcile, before the first tick: a row a crash left claimed
        // (`executing`) goes back to the queue, never stranded
        // (`nest/common.md` § Client-state recoverability).
        match state.db.requeue_stranded_pending_actions().await {
            Ok(0) => {}
            Ok(n) => tracing::info!("pending action executor: re-queued {n} stranded claims"),
            Err(e) => tracing::warn!("pending action executor: boot re-queue failed: {e}"),
        }
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            match execute_ready_actions(&state).await {
                Ok(n) if n > 0 => tracing::info!("pending action executor: executed {n} actions"),
                Ok(_) => {}
                Err(e) => tracing::warn!("pending action executor error: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod security_notice_tests {
    //! Which pending-action steps ring, and who hears them
    //! (`behavior/notifications.md` § Security notices → *Pending actions*).
    //! Each person-initiated transition writes exactly one `security.notice`
    //! row for the creator; the target of an executed admin action against one
    //! account hears `AdminChange`; the nest's own scheduled prunes ring nothing.
    use super::*;
    use crate::db::CacheDb;

    async fn state_with(users: &[[u8; 32]]) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        for u in users {
            db.create_user(u, "free", "test").await.unwrap();
        }
        Arc::new(AppState::for_test(db))
    }

    /// The row-body keys of `actor`'s security notices, oldest first.
    async fn notice_keys(state: &AppState, actor: &[u8; 32]) -> Vec<String> {
        let mut rows = state
            .db
            .list_notifications(actor, None, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.notif_type == fauna_protocol::notifications::NotifType::SecurityNotice)
            .collect::<Vec<_>>();
        rows.sort_by_key(|r| r.id);
        rows.into_iter()
            .map(|r| r.body.expect("a security row carries a localized body").key)
            .collect()
    }

    #[tokio::test]
    async fn a_person_initiated_action_rings_once_at_creation_and_once_at_execution() {
        let creator = [0x31u8; 32];
        let state = state_with(&[creator]).await;

        let row = schedule(
            &state,
            &ActionType::HandleChange,
            &creator,
            Some("renamed"),
            Some(r#"{"new_handle": "renamed"}"#),
        )
        .await
        .unwrap();
        assert_eq!(
            notice_keys(&state, &creator).await,
            ["notifications.row_security_pending_action_queued"]
        );
        let queued = state
            .db
            .list_notifications(&creator, None, 1)
            .await
            .unwrap()
            .remove(0)
            .body
            .unwrap();
        assert_eq!(
            queued.args.get("action_id").map(String::as_str),
            Some(row.id.to_string().as_str()),
            "the row names the action it reports"
        );

        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 1);
        assert_eq!(
            notice_keys(&state, &creator).await,
            [
                "notifications.row_security_pending_action_queued",
                "notifications.row_security_action_executed",
            ]
        );
    }

    #[tokio::test]
    async fn the_nests_own_scheduled_prunes_never_ring() {
        let owner = [0x32u8; 32];
        let state = state_with(&[owner]).await;
        for action_type in [ActionType::SnapshotBulkPrune, ActionType::VersionBulkPrune] {
            assert!(action_type.is_nest_scheduled());
            let id = state
                .db
                .create_pending_action(&action_type, &owner, Some("1"), Some("{}"), None)
                .await
                .unwrap();
            let row = state.db.get_pending_action(id).await.unwrap().unwrap();
            for step in [
                Transition::Created,
                Transition::Executed,
                Transition::Cancelled { by: &owner },
            ] {
                notify_transition(&state, &row, step).await;
            }
        }
        assert!(notice_keys(&state, &owner).await.is_empty());
    }

    #[tokio::test]
    async fn an_executed_admin_action_tells_its_target_as_an_admin_change() {
        let (admin, target) = ([0x33u8; 32], [0x34u8; 32]);
        let state = state_with(&[admin, target]).await;
        let id = state
            .db
            .create_pending_action(
                &ActionType::AdminAdd,
                &admin,
                Some(&hex::encode(target)),
                None,
                None,
            )
            .await
            .unwrap();
        let row = state.db.get_pending_action(id).await.unwrap().unwrap();

        notify_transition(&state, &row, Transition::Created).await;
        assert!(
            notice_keys(&state, &target).await.is_empty(),
            "the target is not told at scheduling"
        );
        notify_transition(&state, &row, Transition::Executed).await;
        assert_eq!(
            notice_keys(&state, &target).await,
            ["notifications.row_security_admin_change"]
        );
        assert_eq!(
            notice_keys(&state, &admin).await,
            [
                "notifications.row_security_pending_action_queued",
                "notifications.row_security_action_executed",
            ]
        );
    }

    #[tokio::test]
    async fn an_account_that_no_longer_exists_is_told_nothing() {
        let gone = [0x35u8; 32];
        let state = state_with(&[]).await;
        let id = state
            .db
            .create_pending_action(&ActionType::AccountDelete, &gone, None, None, None)
            .await
            .unwrap();
        let row = state.db.get_pending_action(id).await.unwrap().unwrap();
        notify_transition(&state, &row, Transition::Executed).await;
        assert!(notice_keys(&state, &gone).await.is_empty());
    }

    const QUEUED: &str = "notifications.row_security_pending_action_queued";
    const EXECUTED: &str = "notifications.row_security_action_executed";
    const CANCELLED: &str = "notifications.row_security_action_cancelled";
    const EXPIRED: &str = "notifications.row_security_action_expired";
    const AGAINST_YOU: &str = "notifications.row_security_pending_action_against_you";
    const ADMIN_CHANGE: &str = "notifications.row_security_admin_change";
    const ADMIN_PENDING: &str = "notifications.row_security_admin_action_pending";
    const ADMIN_CANCELLED: &str = "notifications.row_security_admin_action_cancelled";
    const ADMIN_EXPIRED: &str = "notifications.row_security_admin_action_expired";

    /// The target of an admin action against their account is told at
    /// scheduling, sees the row in their own list, and hears the cancel.
    #[tokio::test]
    async fn the_target_of_an_action_against_them_is_told_and_can_see_and_cancel_it() {
        let (admin, target) = ([0x36u8; 32], [0x37u8; 32]);
        let state = state_with(&[admin, target]).await;
        state.db.add_admin_actor(&admin[..]).await.unwrap();

        let row = schedule(
            &state,
            &ActionType::AdminDeleteUser,
            &admin,
            Some(&hex::encode(target)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(notice_keys(&state, &target).await, [AGAINST_YOU]);
        assert_eq!(notice_keys(&state, &admin).await, [QUEUED]);
        let against_you = state
            .db
            .list_notifications(&target, None, 1)
            .await
            .unwrap()
            .remove(0)
            .body
            .unwrap();
        assert_eq!(
            against_you.args.get("action_id").map(String::as_str),
            Some(row.id.to_string().as_str()),
            "the notice names the action it reports"
        );

        let own_list = state
            .db
            .list_pending_actions_for_actor(&target)
            .await
            .unwrap();
        assert!(
            own_list.iter().any(|r| r.id == row.id),
            "the target's Settings → Pending actions list carries the action against them"
        );

        state
            .db
            .cancel_pending_action(row.id, &target)
            .await
            .unwrap();
        let row = state.db.get_pending_action(row.id).await.unwrap().unwrap();
        notify_transition(&state, &row, Transition::Cancelled { by: &target }).await;
        assert_eq!(notice_keys(&state, &target).await, [AGAINST_YOU, CANCELLED]);
        assert_eq!(notice_keys(&state, &admin).await, [QUEUED, CANCELLED]);
    }

    /// Every co-admin hears a roster action at scheduling — the notice that
    /// makes a quorum approval possible — and again when it executes; the
    /// future admin is told only once the role is theirs.
    #[tokio::test]
    async fn co_admins_hear_a_roster_action_at_every_step() {
        let (a, b, c, u) = ([0x38u8; 32], [0x39u8; 32], [0x3au8; 32], [0x3bu8; 32]);
        let state = state_with(&[a, b, c, u]).await;
        for admin in [a, b, c] {
            state.db.add_admin_actor(&admin[..]).await.unwrap();
        }

        let row = schedule(
            &state,
            &ActionType::AdminAdd,
            &a,
            Some(&hex::encode(u)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            row.requires_quorum, 1,
            "two peers can approve: the nominal quorum stands"
        );
        for admin in [b, c] {
            assert_eq!(notice_keys(&state, &admin).await, [ADMIN_PENDING]);
        }
        assert!(
            notice_keys(&state, &u).await.is_empty(),
            "the future admin cannot act yet; they are told at execution"
        );

        state.db.approve_pending_action(row.id, &b).await.unwrap();
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 1);
        assert!(state.db.is_admin(&u).await.unwrap());
        for admin in [b, c] {
            assert_eq!(
                notice_keys(&state, &admin).await,
                [ADMIN_PENDING, ADMIN_CHANGE]
            );
        }
        assert_eq!(notice_keys(&state, &u).await, [ADMIN_CHANGE]);
        assert_eq!(notice_keys(&state, &a).await, [QUEUED, EXECUTED]);
    }

    /// An action that reaches its time short of approvals expires — and says
    /// so to everyone it concerned, instead of vanishing.
    #[tokio::test]
    async fn an_action_that_misses_its_quorum_expires_loudly() {
        let (a, b, c, d) = ([0x3cu8; 32], [0x3du8; 32], [0x3eu8; 32], [0x3fu8; 32]);
        let state = state_with(&[a, b, c, d]).await;
        for admin in [a, b, c, d] {
            state.db.add_admin_actor(&admin[..]).await.unwrap();
        }

        let row = schedule(
            &state,
            &ActionType::AdminRemove,
            &a,
            Some(&hex::encode(d)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            row.requires_quorum, 2,
            "b and c are the peers who can approve"
        );
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 0);
        let row = state.db.get_pending_action(row.id).await.unwrap().unwrap();
        assert_eq!(row.status, "expired");
        assert!(state.db.is_admin(&d).await.unwrap(), "nothing changed");
        assert_eq!(notice_keys(&state, &a).await, [QUEUED, EXPIRED]);
        for admin in [b, c, d] {
            assert_eq!(
                notice_keys(&state, &admin).await,
                [ADMIN_PENDING, ADMIN_EXPIRED]
            );
        }
    }

    /// A co-admin who calls a roster action off is heard by the rest of the
    /// roster, not only by the creator.
    #[tokio::test]
    async fn a_co_admins_cancel_is_heard_by_the_roster() {
        let (a, b, c, u) = ([0x40u8; 32], [0x41u8; 32], [0x42u8; 32], [0x43u8; 32]);
        let state = state_with(&[a, b, c, u]).await;
        for admin in [a, b, c] {
            state.db.add_admin_actor(&admin[..]).await.unwrap();
        }
        let row = schedule(
            &state,
            &ActionType::AdminAdd,
            &a,
            Some(&hex::encode(u)),
            None,
        )
        .await
        .unwrap();
        state.db.cancel_pending_action(row.id, &b).await.unwrap();
        let row = state.db.get_pending_action(row.id).await.unwrap().unwrap();
        notify_transition(&state, &row, Transition::Cancelled { by: &b }).await;
        assert_eq!(notice_keys(&state, &a).await, [QUEUED, CANCELLED]);
        for admin in [b, c] {
            assert_eq!(
                notice_keys(&state, &admin).await,
                [ADMIN_PENDING, ADMIN_CANCELLED]
            );
        }
        assert!(notice_keys(&state, &u).await.is_empty());
    }

    /// The deployment the co-admin instrument exists for: one admin, who
    /// grants a second. No peer exists to approve, so the delay window and
    /// the creator's own cancel are the guard — the grant must execute, not
    /// expire.
    #[tokio::test]
    async fn a_sole_admin_can_grant_a_co_admin_on_the_delay_alone() {
        let (a, u) = ([0x44u8; 32], [0x45u8; 32]);
        let state = state_with(&[a, u]).await;
        state.db.add_admin_actor(&a[..]).await.unwrap();

        let row = schedule(
            &state,
            &ActionType::AdminAdd,
            &a,
            Some(&hex::encode(u)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(row.requires_quorum, 0, "nobody else could approve it");
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 1);
        assert!(state.db.is_admin(&u).await.unwrap());
        assert_eq!(notice_keys(&state, &u).await, [ADMIN_CHANGE]);
        assert_eq!(notice_keys(&state, &a).await, [QUEUED, EXECUTED]);
    }

    /// Two admins, one removing the other: the ruled floor (user, 2026-09-24)
    /// stores one approval, and the only admin who can give it is the target
    /// — so the removal expires unless the target consents, and executes when
    /// they do. Before the ruling the row stored 0 and one admin removed the
    /// other on the delay alone.
    #[tokio::test]
    async fn a_two_admin_removal_needs_the_targets_consent() {
        let (a, b) = ([0x46u8; 32], [0x47u8; 32]);
        let state = state_with(&[a, b]).await;
        for admin in [a, b] {
            state.db.add_admin_actor(&admin[..]).await.unwrap();
        }

        let row = schedule(
            &state,
            &ActionType::AdminRemove,
            &a,
            Some(&hex::encode(b)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            row.requires_quorum, 1,
            "no peer exists: the floor holds at one, the target's consent"
        );
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 0);
        let row = state.db.get_pending_action(row.id).await.unwrap().unwrap();
        assert_eq!(row.status, "expired", "unconsented, it expires");
        assert!(state.db.is_admin(&b).await.unwrap(), "b keeps the role");

        let row = schedule(
            &state,
            &ActionType::AdminRemove,
            &a,
            Some(&hex::encode(b)),
            None,
        )
        .await
        .unwrap();
        state.db.approve_pending_action(row.id, &b).await.unwrap();
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 1);
        assert!(
            !state.db.is_admin(&b).await.unwrap(),
            "b consented: removed"
        );
    }

    #[test]
    fn effective_quorum_caps_at_the_peers_who_can_approve() {
        let (a, b, c, d) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
        let roster = |ids: &[[u8; 32]]| ids.iter().map(|i| i.to_vec()).collect::<Vec<_>>();
        // A sole admin: no peer exists to approve.
        assert_eq!(
            effective_quorum(&ActionType::AdminAdd, &roster(&[a]), &a, None),
            0
        );
        // Two admins, one removing the other: no peer exists, so the floor
        // holds — one approval, which only the target's own consent can give
        // (ruled by the user 2026-09-24).
        assert_eq!(
            effective_quorum(&ActionType::AdminRemove, &roster(&[a, b]), &a, Some(&b)),
            1
        );
        // The floor is per formula, not per type: a dormant role change on
        // the same two-admin roster needs the target's consent too.
        assert_eq!(
            effective_quorum(&ActionType::AdminChangeRole, &roster(&[a, b]), &a, Some(&b)),
            1
        );
        // Three: one peer can concur (or the target consents).
        assert_eq!(
            effective_quorum(&ActionType::AdminRemove, &roster(&[a, b, c]), &a, Some(&c)),
            1
        );
        // Enough peers: the nominal count stands.
        assert_eq!(
            effective_quorum(
                &ActionType::AdminRemove,
                &roster(&[a, b, c, d]),
                &a,
                Some(&d)
            ),
            2
        );
        assert_eq!(
            effective_quorum(&ActionType::AdminAdd, &roster(&[a, b]), &a, Some(&c)),
            1
        );
        // A type with no quorum is untouched by the roster.
        assert_eq!(
            effective_quorum(&ActionType::HandleChange, &roster(&[]), &a, None),
            0
        );
    }
}

#[cfg(test)]
mod claim_tests {
    //! A cancel the nest accepts must hold: the executor claims each row
    //! (`pending` → `executing`) before it acts, so a cancel landing between
    //! the tick's batch read and the row's turn wins, and a cancel landing
    //! after the claim is refused honestly (`nest/common.md` § Pending Actions
    //! System → *The executor claims before it acts*).
    use super::*;
    use crate::db::CacheDb;

    async fn state_with_handle(actor: &[u8; 32], handle: &str) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(actor, "free", "test").await.unwrap();
        db.set_handle(actor, handle).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    async fn due_handle_change(state: &Arc<AppState>, actor: &[u8; 32]) -> i64 {
        let row = schedule(
            state,
            &ActionType::HandleChange,
            actor,
            Some("renamed"),
            Some(r#"{"new_handle": "renamed"}"#),
        )
        .await
        .unwrap();
        state.db.test_set_execute_after(row.id, 1).await.unwrap();
        row.id
    }

    /// The review's witness (PROBE-801-28-A), adapted to the claim step: the
    /// tick has read its batch, the creator cancels, then the loop reaches the
    /// held row.
    #[tokio::test]
    async fn a_cancel_landing_after_the_batch_read_wins() {
        let creator = [0x41u8; 32];
        let state = state_with_handle(&creator, "original").await;
        let id = due_handle_change(&state, &creator).await;

        let batch = state.db.list_ready_pending_actions().await.unwrap();
        let held = batch.into_iter().find(|a| a.id == id).unwrap();
        state
            .db
            .cancel_pending_action(id, &creator)
            .await
            .expect("the cancel is accepted and reported as done");

        assert!(!run_ready_action(&state, held).await, "nothing executed");
        assert_eq!(
            state.db.get_handle(&creator).await.unwrap().as_deref(),
            Some("original"),
            "a cancelled action must not execute"
        );
        let after = state.db.get_pending_action(id).await.unwrap().unwrap();
        assert_eq!(
            after.status, "cancelled",
            "the cancel must not be overwritten"
        );
    }

    /// Once the executor holds the row, the action is running: a cancel is
    /// refused rather than reported done over an action that then runs.
    #[tokio::test]
    async fn a_cancel_after_the_claim_is_refused_honestly() {
        let creator = [0x42u8; 32];
        let state = state_with_handle(&creator, "original").await;
        let id = due_handle_change(&state, &creator).await;

        let claimed = state.db.claim_pending_action(id).await.unwrap().unwrap();
        assert_eq!(claimed.status, "executing");
        let err = state
            .db
            .cancel_pending_action(id, &creator)
            .await
            .expect_err("a claimed action is not cancellable");
        assert!(err.to_string().contains("not cancellable"), "{err}");
        assert!(state.db.mark_pending_action_executed(id).await.unwrap());
        let after = state.db.get_pending_action(id).await.unwrap().unwrap();
        assert_eq!(after.status, "executed");
    }

    /// A crash between the claim and the mark leaves the row `executing`;
    /// the boot reconcile hands it back to the queue, and the next tick runs it.
    #[tokio::test]
    async fn a_row_a_crash_left_claimed_is_requeued_at_boot() {
        let creator = [0x43u8; 32];
        let state = state_with_handle(&creator, "original").await;
        let id = due_handle_change(&state, &creator).await;
        state.db.claim_pending_action(id).await.unwrap().unwrap();
        assert!(
            state
                .db
                .list_ready_pending_actions()
                .await
                .unwrap()
                .is_empty(),
            "a claimed row is not ready"
        );

        assert_eq!(
            state.db.requeue_stranded_pending_actions().await.unwrap(),
            1
        );
        let row = state.db.get_pending_action(id).await.unwrap().unwrap();
        assert_eq!(row.status, "pending");
        assert_eq!(execute_ready_actions(&state).await.unwrap(), 1);
        assert_eq!(
            state.db.get_handle(&creator).await.unwrap().as_deref(),
            Some("renamed")
        );
    }

    /// A failed run hands its claim back: the row is `pending` again and the
    /// next tick retries it.
    #[tokio::test]
    async fn a_failed_run_leaves_the_row_able_to_run_again() {
        let creator = [0x44u8; 32];
        let state = state_with_handle(&creator, "original").await;
        // A handle change with no payload fails in `execute_action`.
        let id = state
            .db
            .create_pending_action(&ActionType::HandleChange, &creator, None, None, None)
            .await
            .unwrap();
        state.db.test_set_execute_after(id, 1).await.unwrap();

        assert_eq!(execute_ready_actions(&state).await.unwrap(), 0);
        let row = state.db.get_pending_action(id).await.unwrap().unwrap();
        assert_eq!(row.status, "pending");
        assert!(
            state
                .db
                .list_ready_pending_actions()
                .await
                .unwrap()
                .iter()
                .any(|a| a.id == id),
            "the next tick sees it again"
        );
    }

    /// The terminal marks move only a claimed row: a cancelled row stays
    /// cancelled whichever mark reaches it.
    #[tokio::test]
    async fn the_terminal_marks_never_overwrite_a_terminal_status() {
        let creator = [0x45u8; 32];
        let state = state_with_handle(&creator, "original").await;
        let id = due_handle_change(&state, &creator).await;
        state.db.cancel_pending_action(id, &creator).await.unwrap();

        assert!(!state.db.mark_pending_action_executed(id).await.unwrap());
        assert!(!state.db.mark_pending_action_expired(id).await.unwrap());
        assert!(state.db.claim_pending_action(id).await.unwrap().is_none());
        let row = state.db.get_pending_action(id).await.unwrap().unwrap();
        assert_eq!(row.status, "cancelled");
        assert!(row.executed_at.is_none());
    }
}
