//! Security event notifications — inbox messages with best-effort push/email,
//! plus the notifications row every app renders.
//!
//! Channel 1 (guaranteed): local inbox system message via `push_inbox`.
//! Channel 1b (rides channel 1): a `notifications` row — the render surface
//! (`notifications.md` § Security notices): without it, a deployment with no
//! claimed mail domain never shows the user a security notice at all.
//! Channel 2 (a push notification POSTed to a push relay URL) was DELETED; the rationale is at its
//! former site in `notify`. Channel numbering keeps the gap on purpose.
//! Channel 3 (best-effort): email sealed into the actor's INBOX (always an
//! in-domain local delivery — never the MX-relay queue).

use std::sync::Arc;

use crate::db::CacheDb;
use crate::nest_identity::NestIdentity;
use crate::routes::AppState;

/// Security-sensitive events that trigger notifications.
#[derive(Debug, Clone)]
pub enum SecurityEvent {
    /// A person-initiated pending action entered its delay window. It carries
    /// the action id, not a URL: cancelling is the `fauna.pending_actions.cancel`
    /// kind, driven from Settings → Pending actions on every app
    /// (`ui/settings.md` § Pending actions) — there is no cancel link to hand
    /// out.
    PendingActionCreated {
        action_id: i64,
        action_type: String,
        execute_after: i64,
    },
    ActionExecuted {
        action_id: i64,
        action_type: String,
    },
    ActionCancelled {
        action_id: i64,
        action_type: String,
        cancelled_by: String,
    },
    /// The delay window closed without the approvals the action needed, so
    /// the executor expired it and nothing changed. Rung for the creator and
    /// for the target of an action against one account
    /// (`pending_actions::Transition::Expired`).
    ActionExpired {
        action_id: i64,
        action_type: String,
    },
    /// An administrator scheduled an action **against the recipient's own
    /// account** (`admin.delete_user`, …). The recipient is one of the parties
    /// the cancel-authorization matrix admits, and their own Settings →
    /// Pending actions section lists the row — so the notice names where to
    /// cancel (`notifications.md` § Security notices → *Pending actions*).
    PendingActionAgainstYou {
        action_id: i64,
        action_type: String,
        execute_after: i64,
        /// How the notice names the scheduling admin (handle, else hex).
        by: String,
    },
    /// A **co-admin's** view of a pending admin action another admin
    /// scheduled: the action, who scheduled it, whom it names, when it runs
    /// and how many more approvals it needs — the notice that makes approval
    /// (or a veto) possible at all, since a quorum-gated action nobody
    /// approves expires. Acted on from the admin console's pending-actions
    /// section (`admin.md` § Pending admin actions).
    AdminActionPending {
        action_id: i64,
        action_type: String,
        execute_after: i64,
        by: String,
        target: String,
        /// Approvals still owed by admins other than the creator (`0` when
        /// the action executes on the delay alone).
        approvals_needed: i64,
    },
    /// A co-admin's view of a pending admin action being called off.
    AdminActionCancelled {
        action_id: i64,
        action_type: String,
        target: String,
        cancelled_by: String,
    },
    /// A co-admin's view of a pending admin action expiring unapproved.
    AdminActionExpired {
        action_id: i64,
        action_type: String,
        target: String,
    },
    NewTokenIssued {
        ip_address: Option<String>,
    },
    AdminChange {
        change_type: String,
        target: String,
    },
    /// A seed-initiated RecoveryKey replacement entered its 30-day pending
    /// window (`identity-succession.md:37`) — the one-shot alarm half of the
    /// loud-on-every-device requirement (the standing banner half is the
    /// client's `fauna.recovery.replacement.status` read).
    RecoveryReplacementPending {
        /// Hex of the RecoveryKey public half that would be registered.
        new_key: String,
        /// Unix seconds the replacement lands if uncontested.
        lands_at: i64,
    },
    /// The window elapsed uncontested and the replacement registration landed.
    RecoveryReplacementLanded {
        new_key: String,
    },
    /// A pending replacement was cancelled before landing.
    RecoveryReplacementCancelled {
        /// What cancelled it: the RecoveryKey veto, or a RecoveryKey-authorized
        /// registration landing during the window.
        cancelled_by: String,
    },
    /// The identity was **succeeded** — the account is now re-pointed to a new
    /// actor id and this one is refused everywhere
    /// (`identity-succession.md` § Enforcement on the home nest).
    ///
    /// Notified on the **old** identity, which is deliberate and slightly
    /// counter-intuitive: the succession is normally the *legitimate owner's*
    /// remedy, so this is not an alarm about an attack, it is the receipt.
    /// Delivering it to the old identity's inbox is what puts a permanent
    /// record where a reader of the old account will find it — and in the rare
    /// hostile case (a stolen recovery kit) it is the only warning the real
    /// owner gets.
    IdentitySucceeded {
        /// Hex of the successor actor id.
        new_actor_id: String,
    },
    /// The account's **full archive was downloaded** (`GET /api/v1/export`) —
    /// the one request that hands back everything at once, so its use is never
    /// silent (a mere new-IP sign-in already rang
    /// `NewTokenIssued` while the whole-account exfil rang nothing). Fired on
    /// every archive download, both credential paths (session bearer AND
    /// eviction export token).
    ArchiveExported {
        /// Peer address when the serving path injected one; `None` on a
        /// direct handler dispatch (tests) or a path without connect info.
        ip_address: Option<String>,
        /// Whether blob content rode along (`?include_blobs`), or metadata only.
        include_blobs: bool,
    },
    /// A **mailbox export was downloaded** (`GET /api/v1/export/{session_id}`)
    /// — the mail twin of [`ArchiveExported`](Self::ArchiveExported): one
    /// request that hands back a whole mailbox snapshot, so its use is never
    /// silent either. A variant of its own rather than a
    /// widened `ArchiveExported`, because the row must say *which* thing left:
    /// a sealed mailbox archive, not the full account archive. Fired on every
    /// successful download of a completed session, after every gate.
    MailboxExportDownloaded {
        /// Peer address when the serving path injected one; `None` on a
        /// direct handler dispatch (tests) or a path without connect info.
        ip_address: Option<String>,
        /// The session's archive format (`mbox`, `maildir`, `eml`).
        format: String,
    },
}

impl SecurityEvent {
    /// What the notifications-page **row** says: a per-event catalog key whose
    /// args carry the actionable detail the row must show (the IP address, a
    /// pending action's id and deadline) — `behavior/notifications.md` § Localized
    /// body → *Security notices*. [`subject`](Self::subject) and
    /// [`body`](Self::body) stay nest-composed English: they are the mail
    /// channel's content and the row's `summary` compat fallback.
    pub fn row_body(&self) -> fauna_protocol::LocalizedText {
        use fauna_protocol::LocalizedText as T;
        let unknown = || "unknown".to_string();
        match self {
            Self::PendingActionCreated {
                action_id,
                action_type,
                execute_after,
            } => T::new("notifications.row_security_pending_action_queued")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string())
                .with_arg("execute_after", execute_after.to_string()),
            Self::ActionExecuted { action_type, .. } => {
                T::new("notifications.row_security_action_executed")
                    .with_arg("action_type", action_type)
            }
            Self::ActionCancelled {
                action_type,
                cancelled_by,
                ..
            } => T::new("notifications.row_security_action_cancelled")
                .with_arg("action_type", action_type)
                .with_arg("cancelled_by", cancelled_by),
            Self::ActionExpired {
                action_id,
                action_type,
            } => T::new("notifications.row_security_action_expired")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string()),
            Self::PendingActionAgainstYou {
                action_id,
                action_type,
                execute_after,
                by,
            } => T::new("notifications.row_security_pending_action_against_you")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string())
                .with_arg("execute_after", execute_after.to_string())
                .with_arg("by", by),
            Self::AdminActionPending {
                action_id,
                action_type,
                execute_after,
                by,
                target,
                approvals_needed,
            } => T::new("notifications.row_security_admin_action_pending")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string())
                .with_arg("execute_after", execute_after.to_string())
                .with_arg("by", by)
                .with_arg("target", target)
                .with_arg("approvals_needed", approvals_needed.to_string()),
            Self::AdminActionCancelled {
                action_id,
                action_type,
                target,
                cancelled_by,
            } => T::new("notifications.row_security_admin_action_cancelled")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string())
                .with_arg("target", target)
                .with_arg("cancelled_by", cancelled_by),
            Self::AdminActionExpired {
                action_id,
                action_type,
                target,
            } => T::new("notifications.row_security_admin_action_expired")
                .with_arg("action_type", action_type)
                .with_arg("action_id", action_id.to_string())
                .with_arg("target", target),
            Self::NewTokenIssued { ip_address } => T::new("notifications.row_security_new_token")
                .with_arg("ip", ip_address.clone().unwrap_or_else(unknown)),
            Self::AdminChange {
                change_type,
                target,
            } => T::new("notifications.row_security_admin_change")
                .with_arg("change_type", change_type)
                .with_arg("target", target),
            Self::RecoveryReplacementPending { new_key, lands_at } => {
                T::new("notifications.row_security_recovery_replacement_pending")
                    .with_arg("new_key", new_key)
                    .with_arg("lands_at", lands_at.to_string())
            }
            Self::RecoveryReplacementLanded { new_key } => {
                T::new("notifications.row_security_recovery_replacement_landed")
                    .with_arg("new_key", new_key)
            }
            Self::RecoveryReplacementCancelled { cancelled_by } => {
                T::new("notifications.row_security_recovery_replacement_cancelled")
                    .with_arg("cancelled_by", cancelled_by)
            }
            Self::IdentitySucceeded { new_actor_id } => {
                T::new("notifications.row_security_identity_succeeded")
                    .with_arg("new_actor_id", new_actor_id)
            }
            Self::ArchiveExported { ip_address, .. } => {
                T::new("notifications.row_security_archive_exported")
                    .with_arg("ip", ip_address.clone().unwrap_or_else(unknown))
            }
            Self::MailboxExportDownloaded { ip_address, format } => {
                T::new("notifications.row_security_mailbox_export_downloaded")
                    .with_arg("ip", ip_address.clone().unwrap_or_else(unknown))
                    .with_arg("format", format)
            }
        }
    }

    /// Human-readable subject line for the inbox message.
    pub fn subject(&self) -> String {
        match self {
            Self::PendingActionCreated { action_type, .. } => {
                format!("Security: pending action queued — {action_type}")
            }
            Self::ActionExecuted { action_type, .. } => {
                format!("Security: action executed — {action_type}")
            }
            Self::ActionCancelled { action_type, .. } => {
                format!("Security: action cancelled — {action_type}")
            }
            Self::ActionExpired { action_type, .. } => {
                format!("Security: action expired unapproved — {action_type}")
            }
            Self::PendingActionAgainstYou { action_type, .. } => {
                format!("Security: an administrator scheduled {action_type} on your account")
            }
            Self::AdminActionPending { action_type, .. } => {
                format!("Security: admin action awaiting review — {action_type}")
            }
            Self::AdminActionCancelled { action_type, .. } => {
                format!("Security: admin action cancelled — {action_type}")
            }
            Self::AdminActionExpired { action_type, .. } => {
                format!("Security: admin action expired unapproved — {action_type}")
            }
            Self::NewTokenIssued { .. } => {
                "Security: new sign-in from a different IP address".to_string()
            }
            Self::AdminChange { change_type, .. } => {
                format!("Security: admin change — {change_type}")
            }
            Self::RecoveryReplacementPending { .. } => {
                "Security: recovery key replacement requested — veto within 30 days if this \
                 wasn't you"
                    .to_string()
            }
            Self::RecoveryReplacementLanded { .. } => {
                "Security: your recovery key was replaced".to_string()
            }
            Self::RecoveryReplacementCancelled { .. } => {
                "Security: pending recovery key replacement cancelled".to_string()
            }
            Self::IdentitySucceeded { .. } => {
                "Security: this identity was succeeded — your account moved to a new key"
                    .to_string()
            }
            Self::ArchiveExported { .. } => {
                "Security: your full account archive was downloaded".to_string()
            }
            Self::MailboxExportDownloaded { .. } => {
                "Security: a mailbox export was downloaded".to_string()
            }
        }
    }

    /// Human-readable body for the inbox message.
    pub fn body(&self) -> String {
        match self {
            Self::PendingActionCreated {
                action_id,
                action_type,
                execute_after,
            } => {
                format!(
                    "A security-sensitive action has been queued by your account.\n\
                     \n\
                     Action ID:   {action_id}\n\
                     Action type: {action_type}\n\
                     Executes at: {execute_after} (Unix seconds)\n\
                     \n\
                     If you did not request this, cancel it immediately from\n\
                     Settings -> Pending actions in any of your apps.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::ActionExecuted {
                action_id,
                action_type,
            } => {
                format!(
                    "A pending action has been executed on your account.\n\
                     \n\
                     Action ID:   {action_id}\n\
                     Action type: {action_type}\n\
                     \n\
                     If you did not authorize this action, contact your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::ActionCancelled {
                action_id,
                action_type,
                cancelled_by,
            } => {
                format!(
                    "A pending action on your account has been cancelled.\n\
                     \n\
                     Action ID:    {action_id}\n\
                     Action type:  {action_type}\n\
                     Cancelled by: {cancelled_by}\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::ActionExpired {
                action_id,
                action_type,
            } => {
                format!(
                    "A pending action on your account reached its execution time without \
                     the approvals it needed, so it expired and nothing changed.\n\
                     \n\
                     Action ID:   {action_id}\n\
                     Action type: {action_type}\n\
                     \n\
                     Schedule it again if it is still wanted.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::PendingActionAgainstYou {
                action_id,
                action_type,
                execute_after,
                by,
            } => {
                format!(
                    "An administrator has scheduled an action against your account.\n\
                     \n\
                     Action ID:    {action_id}\n\
                     Action type:  {action_type}\n\
                     Scheduled by: {by}\n\
                     Executes at:  {execute_after} (Unix seconds)\n\
                     \n\
                     You can cancel it from Settings -> Pending actions in any of your \
                     apps before it executes. If you do not understand why it was \
                     scheduled, contact your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::AdminActionPending {
                action_id,
                action_type,
                execute_after,
                by,
                target,
                approvals_needed,
            } => {
                format!(
                    "Another administrator has scheduled an admin action that needs your \
                     attention.\n\
                     \n\
                     Action ID:        {action_id}\n\
                     Action type:      {action_type}\n\
                     Scheduled by:     {by}\n\
                     Target:           {target}\n\
                     Executes at:      {execute_after} (Unix seconds)\n\
                     Approvals needed: {approvals_needed}\n\
                     \n\
                     Review it under Admin -> Users -> Pending admin actions: approve \
                     it if it is wanted, or cancel it if it is not. An action that \
                     still needs approvals when its time comes expires unexecuted.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::AdminActionCancelled {
                action_id,
                action_type,
                target,
                cancelled_by,
            } => {
                format!(
                    "A pending admin action was cancelled.\n\
                     \n\
                     Action ID:    {action_id}\n\
                     Action type:  {action_type}\n\
                     Target:       {target}\n\
                     Cancelled by: {cancelled_by}\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::AdminActionExpired {
                action_id,
                action_type,
                target,
            } => {
                format!(
                    "A pending admin action reached its execution time without the \
                     approvals it needed, so it expired and nothing changed.\n\
                     \n\
                     Action ID:   {action_id}\n\
                     Action type: {action_type}\n\
                     Target:      {target}\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::NewTokenIssued { ip_address } => {
                let ip_line = match ip_address {
                    Some(ip) => format!("IP address: {ip}"),
                    None => "IP address: unknown".to_string(),
                };
                format!(
                    "A new authentication token was issued for your account from an IP address \
                     that differs from your last sign-in.\n\
                     \n\
                     {ip_line}\n\
                     \n\
                     If this was you signing in from a new location, no action is needed.\n\
                     If you did not sign in, your account may be compromised — change your \
                     keys immediately and contact your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::AdminChange {
                change_type,
                target,
            } => {
                format!(
                    "An administrative change has been made that affects your account.\n\
                     \n\
                     Change type: {change_type}\n\
                     Target:      {target}\n\
                     \n\
                     If you did not request this change, contact your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::RecoveryReplacementPending { new_key, lands_at } => {
                format!(
                    "A replacement of your account's recovery key has been requested using \
                     your identity key alone, and is now in its 30-day pending window.\n\
                     \n\
                     New recovery key: {new_key}\n\
                     Takes effect at:  {lands_at} (Unix seconds), unless vetoed\n\
                     \n\
                     If this was you (for example, you lost your recovery kit), no action is \
                     needed — the replacement takes effect after the window.\n\
                     \n\
                     If this was NOT you, your identity key may be stolen. Veto the \
                     replacement now from any of your apps' Security settings using your \
                     current recovery kit — the veto takes effect instantly.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::RecoveryReplacementLanded { new_key } => {
                format!(
                    "The pending replacement of your account's recovery key completed its \
                     30-day window with no veto and is now in effect.\n\
                     \n\
                     New recovery key: {new_key}\n\
                     \n\
                     Your previous recovery kit no longer opens anything, and the sealed \
                     copy of your identity secret was removed when the new key took \
                     effect — so your recovery phrase cannot recover this account right \
                     now. To restore that protection, open Settings -> Account -> \
                     Recovery Kit on a signed-in device, enter the new kit you wrote \
                     down when you requested the replacement, and press Restore Phrase \
                     Recovery.\n\
                     \n\
                     If you did not request this replacement, your account may be \
                     compromised — contact your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::RecoveryReplacementCancelled { cancelled_by } => {
                format!(
                    "The pending replacement of your account's recovery key was cancelled \
                     before taking effect.\n\
                     \n\
                     Cancelled by: {cancelled_by}\n\
                     \n\
                     Your current recovery key remains in effect.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::IdentitySucceeded { new_actor_id } => {
                format!(
                    "Your account on this nest has been re-pointed to a new identity, \
                     authorized by your recovery key.\n\
                     \n\
                     New identity: {new_actor_id}\n\
                     \n\
                     This identity key no longer works anywhere on this nest: sign-in, \
                     sessions and content writes using it are all refused from now on. Your \
                     handle, tier and admin role (if any) moved to the new identity; your \
                     existing posts and messages stay attributed to the old one.\n\
                     \n\
                     On each of your devices, import the new identity to continue. Capability \
                     grants you had issued were revoked — re-issue them from the new identity.\n\
                     \n\
                     If you did NOT perform this recovery, whoever holds your recovery kit \
                     now controls the account — contact your nest administrator immediately.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::ArchiveExported {
                ip_address,
                include_blobs,
            } => {
                let ip_line = match ip_address {
                    Some(ip) => format!("IP address: {ip}"),
                    None => "IP address: unknown".to_string(),
                };
                let scope_line = if *include_blobs {
                    "Scope:      full archive including file contents"
                } else {
                    "Scope:      full archive (metadata; file contents not included)"
                };
                format!(
                    "A complete export of your account's data was just downloaded from \
                     your nest.\n\
                     \n\
                     {ip_line}\n\
                     {scope_line}\n\
                     \n\
                     If this was you, no action is needed.\n\
                     \n\
                     If it was NOT you, whoever downloaded it holds a copy of your \
                     account's data: revoke your sessions from Settings -> Security on a \
                     device you trust (or use the emergency lockout), and contact your \
                     nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
            Self::MailboxExportDownloaded { ip_address, format } => {
                let ip_line = match ip_address {
                    Some(ip) => format!("IP address: {ip}"),
                    None => "IP address: unknown".to_string(),
                };
                format!(
                    "A mailbox export you prepared was just downloaded from your nest.\n\
                     \n\
                     {ip_line}\n\
                     Format:     {format}\n\
                     \n\
                     If this was you, no action is needed.\n\
                     \n\
                     If it was NOT you, whoever downloaded it holds a copy of your mail, \
                     sealed so that only your account's keys open it: revoke your sessions \
                     from Settings -> Security on a device you trust (or use the emergency \
                     lockout), discard the export from the mail export page, and contact \
                     your nest administrator.\n\
                     \n\
                     This message was generated automatically by your Fauna nest."
                )
            }
        }
    }
}

/// Sends multi-channel security notifications for a given actor.
///
/// Channel 1 (guaranteed): local inbox system message via `push_inbox`.
/// Channel 3 (best-effort): email enqueued via the SMTP outbound queue.
///
/// Channel 2 (a push-relay POST) was deleted 2026-08-15 — see the note at its
/// former site in [`SecurityNotifier::notify`]. The numbering is deliberately
/// left with the gap so the channel names stay stable across the docs and
/// review findings that refer to them.
pub struct SecurityNotifier {
    db: Arc<CacheDb>,
    #[allow(dead_code)]
    nest_identity: Arc<NestIdentity>,
}

impl SecurityNotifier {
    pub fn new(db: Arc<CacheDb>, nest_identity: Arc<NestIdentity>) -> Self {
        Self { db, nest_identity }
    }

    /// Send a security notification to `actor_id`.
    ///
    /// Always inserts an inbox system message plus the `notifications` row the
    /// apps render (channel 1b). Push and email channels are attempted
    /// best-effort and failures are only logged. `state` is needed by the
    /// push-event fan-out (channel 1b) and by the email channel to seal the
    /// notification into the actor's INBOX (always an in-domain local delivery
    /// — see Channel 3).
    pub async fn notify(&self, state: &Arc<AppState>, actor_id: &[u8; 32], event: &SecurityEvent) {
        let subject = event.subject();
        let body = event.body();

        // Channel 1: guaranteed inbox delivery. Wrap the notice in the canonical
        // DAG-CBOR inbox envelope (layer 1) as a SecurityNotice so the shared
        // drain surfaces it by `kind` (replaces the old plain-UTF-8 payload).
        match fauna_protocol::inbox::InboxEnvelope::security_notice(
            &fauna_protocol::inbox::SecurityNoticeInbox {
                subject: subject.clone(),
                body: body.clone(),
                extra: Default::default(),
            },
        )
        .and_then(|e| e.to_canonical_bytes())
        {
            Ok(payload) => match self.db.push_inbox(actor_id, &payload, None).await {
                Ok(inbox_row_id) => {
                    tracing::debug!(
                        actor = hex::encode(actor_id),
                        subject = %subject,
                        "security notification delivered to inbox"
                    );

                    // Channel 1b: the notifications row — what the apps'
                    // Notifications page actually renders (`notifications.md`
                    // § Security notices). Clients ack the redundant inbox
                    // envelope on sight because this row is the durable render
                    // surface. The summary carries the FULL notice (subject +
                    // body): the body holds the actionable detail (the IP, the
                    // pending action id), and on a no-mail deployment this row is the
                    // only place it ever reaches the user.
                    //
                    // The inbox row id is a per-event dedup token, not a
                    // content identifier (the `feed_request_dedup_key`
                    // pattern): `insert_notification` dedups on
                    // (actor, type, sender, content), and a second
                    // `NewTokenIssued` — a fresh compromise signal — must ring
                    // again, never be swallowed as a duplicate of the first.
                    let notif_text = crate::db::notifications::NotificationText::new(
                        event.row_body(),
                        format!("{subject}\n\n{body}"),
                    );
                    let dedup_token = inbox_row_id.to_le_bytes();
                    let now_micros = fauna_core::data::Timestamp::now().as_i64();
                    match self
                        .db
                        .insert_notification(
                            actor_id,
                            &fauna_protocol::notifications::NotifType::SecurityNotice,
                            "fauna",
                            None,
                            Some(&dedup_token),
                            None,
                            &notif_text,
                            now_micros,
                        )
                        .await
                    {
                        Ok(Some(notif_id)) => {
                            state.ws.notify_push(
                                actor_id,
                                fauna_protocol::PushEvent::Notification(
                                    fauna_protocol::push_events::NotificationPayload {
                                        notification_id: notif_id,
                                        notif_type: fauna_protocol::notifications::NotifType::SecurityNotice,
                                        source: "fauna".into(),
                                        sender_id: None,
                                        content_id: Some(hex::encode(dedup_token)),
                                        summary: notif_text.summary().to_string(),
                                        body: notif_text.body().cloned(),
                                        timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                                        extra: std::collections::BTreeMap::new(),
                                    },
                                ),
                            );
                        }
                        // Unreachable with a fresh inbox row id in the token;
                        // an already-standing row needs no second push.
                        Ok(None) => {}
                        Err(e) => {
                            tracing::error!(
                                actor = hex::encode(actor_id),
                                subject = %subject,
                                error = %e,
                                "failed to write security notification row"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        actor = hex::encode(actor_id),
                        subject = %subject,
                        error = %e,
                        "failed to deliver security notification to inbox"
                    );
                }
            },
            // Best-effort: skip the inbox channel on the (effectively impossible)
            // encode failure of two short strings; channels 2/3 still attempt.
            Err(e) => {
                tracing::error!(
                    actor = hex::encode(actor_id),
                    subject = %subject,
                    error = %e,
                    "failed to encode security notice inbox envelope"
                );
            }
        }

        // Channel 2 (the push-relay POST) was DELETED. It aimed at `{relay_url}/v1/notify`, a route
        // `bins/fauna-push-relay` has never implemented, so it could only ever
        // 404 — and it never even got that far in a shipped deployment, because
        // its `push_relay_url` came solely from a `--push-relay-url` CLI flag no
        // artifact sets. Two reasons it is gone rather than built:
        //
        //   1. The flag was the banned operator tier. The relay is a first-party
        //      Fauna service, not part of a nest deployment
        //      (`api-layers.md` § Push relay), so no user or admin ever chooses
        //      its URL — exactly the reasoning the VAPID keypair beside it
        //      already follows (`nest/common.md` § Web Push).
        //   2. Implementing the route as the payload was written would have been
        //      a phishing primitive: it carried no signature, so anyone could
        //      POST an arbitrary "security notice" to any actor. Doing it safely
        //      needs a nest→relay identity the relay has no registry for, and
        //      the whole relay wire has no deployed producer yet — so the design
        //      question is answered against a real requirement when mobile push
        //      actually lands, not invented here.
        //
        // Nothing user-visible is lost: channel 1 (guaranteed inbox message +
        // the 1b `notifications` row) and channel 3 (sealed INBOX email) both
        // still deliver every notice, and are pinned as the surviving floor by
        // `every_security_notice_reaches_the_user_without_a_push_relay`.

        // Channel 3: email, delivered LOCALLY into the actor's sealed INBOX.
        //
        // A security notification always targets a known in-domain actor
        // (`{handle}@{domain}`), so it is a local delivery, never "outbound".
        // Looks up the actor's handle to construct the `To:` header, builds a
        // minimal RFC 5322 message, and seals it straight into the actor's INBOX.
        // The domain is the deployment's active primary mail domain, resolved
        // from the runtime `local_domains` table (provisioned by a client, the
        // product invariant) — so this channel fires once mail is enabled rather
        // than from a boot-time config field. Failures are best-effort and only
        // logged; no claimed domain → skip the email channel (inbox + push still
        // delivered above).
        let primary_domain = match self.db.lookup_primary_mail_domain().await {
            Ok(opt) => opt.map(|d| d.domain_name),
            Err(e) => {
                tracing::warn!(error = %e, "security email: primary mail-domain lookup failed");
                None
            }
        };
        if let Some(ref domain) = primary_domain {
            let handle_result = self.db.get_handle(actor_id).await;
            match handle_result {
                Ok(Some(handle)) => {
                    let to_addr = format!("{handle}@{domain}");
                    let from_addr = format!("security@{domain}");
                    // Construct a minimal RFC 5322 message.
                    let raw_email = format!(
                        "From: Fauna Security <{from_addr}>\r\n\
                         To: {to_addr}\r\n\
                         Subject: {subject}\r\n\
                         MIME-Version: 1.0\r\n\
                         Content-Type: text/plain; charset=UTF-8\r\n\
                         \r\n\
                         {body}\r\n"
                    );
                    // We already hold `actor_id`, so seal the message straight to
                    // it — no re-resolution, no MX-relay queue (which would
                    // self-loop and `554`-bounce on a containerized deploy at the
                    // inbound HELO-identity check; smtp-server.md § Outbound
                    // submission flow). `from_addr` rides the `From:` header above;
                    // `domain` is the sealed copy's `sender_domain`.
                    // `MailIngress::System`: a security notification is
                    // nest-generated and never passes the guardian mail gate —
                    // a supervised account must always learn about a new login
                    // or a key change (`family-safety.md` § The mail gate).
                    match crate::bridge_routing_handlers::seal_and_ingest_local(
                        state,
                        actor_id,
                        raw_email.as_bytes(),
                        domain,
                        fauna_core::data::MailIngress::System,
                        // Nest-generated: no alias was matched, so there is
                        // nothing to stamp.
                        &[],
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                actor = hex::encode(actor_id),
                                to = %to_addr,
                                "security email delivered locally"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                actor = hex::encode(actor_id),
                                error = %e.code,
                                "failed to deliver security email locally"
                            );
                        }
                    }
                }
                Ok(None) => {
                    tracing::debug!(
                        actor = hex::encode(actor_id),
                        "actor has no handle; skipping security email"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        actor = hex::encode(actor_id),
                        error = %e,
                        "failed to look up actor handle for security email"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every event's row body names a catalog sentence and fills it. A new
    /// variant fails to compile in `row_body` until it has a key, and fails
    /// here until that key is in `en.yaml` with its args
    /// (`behavior/notifications.md` § Localized body → *Security notices*).
    #[test]
    fn every_security_event_row_body_is_a_complete_catalog_sentence() {
        use super::SecurityEvent as E;
        let s = |v: &str| v.to_string();
        let events = [
            E::PendingActionCreated {
                action_id: 1,
                action_type: s("delete_account"),
                execute_after: 99,
            },
            E::ActionExecuted {
                action_id: 1,
                action_type: s("delete_account"),
            },
            E::ActionCancelled {
                action_id: 1,
                action_type: s("delete_account"),
                cancelled_by: s("user"),
            },
            E::ActionExpired {
                action_id: 1,
                action_type: s("admin.add"),
            },
            E::PendingActionAgainstYou {
                action_id: 1,
                action_type: s("admin.delete_user"),
                execute_after: 99,
                by: s("admin"),
            },
            E::AdminActionPending {
                action_id: 1,
                action_type: s("admin.add"),
                execute_after: 99,
                by: s("admin"),
                target: s("alice"),
                approvals_needed: 1,
            },
            E::AdminActionCancelled {
                action_id: 1,
                action_type: s("admin.add"),
                target: s("alice"),
                cancelled_by: s("admin"),
            },
            E::AdminActionExpired {
                action_id: 1,
                action_type: s("admin.add"),
                target: s("alice"),
            },
            E::NewTokenIssued {
                ip_address: Some(s("203.0.113.7")),
            },
            E::NewTokenIssued { ip_address: None },
            E::AdminChange {
                change_type: s("tier"),
                target: s("alice"),
            },
            E::RecoveryReplacementPending {
                new_key: s("ab12"),
                lands_at: 99,
            },
            E::RecoveryReplacementLanded { new_key: s("ab12") },
            E::RecoveryReplacementCancelled {
                cancelled_by: s("veto"),
            },
            E::IdentitySucceeded {
                new_actor_id: s("cd34"),
            },
            E::ArchiveExported {
                ip_address: None,
                include_blobs: true,
            },
            E::MailboxExportDownloaded {
                ip_address: Some(s("203.0.113.9")),
                format: s("mbox"),
            },
        ];
        for event in &events {
            crate::db::notifications::assert_body_is_catalog_complete(&event.row_body());
        }
        // The actionable detail the row exists to show rides as an arg: the
        // action's id and deadline (there is no cancel URL — cancelling is the
        // Settings → Pending actions section).
        let pending = events[0].row_body();
        assert_eq!(pending.args.get("action_id").map(String::as_str), Some("1"));
        assert_eq!(
            pending.args.get("execute_after").map(String::as_str),
            Some("99")
        );
        let new_token = events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    E::NewTokenIssued {
                        ip_address: Some(_)
                    }
                )
            })
            .expect("the seeded new-sign-in event");
        assert_eq!(
            new_token.row_body().args.get("ip").map(String::as_str),
            Some("203.0.113.7")
        );
    }

    use super::*;
    use crate::db::CacheDb;
    use ed25519_dalek::SigningKey;
    use std::sync::Arc;

    fn make_db() -> Arc<CacheDb> {
        Arc::new(CacheDb::open_in_memory().unwrap())
    }

    fn make_nest_identity() -> Arc<NestIdentity> {
        // Generate a deterministic Ed25519 key for tests (all-zeroes seed is
        // valid for ed25519-dalek).
        let seed = [0u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        })
    }

    #[tokio::test]
    async fn security_notify_delivers_to_inbox() {
        let db = make_db();
        let actor_id = [0x42u8; 32];
        let identity = make_nest_identity();

        // No push relay or email domain — only the inbox channel is exercised.
        let notifier = SecurityNotifier::new(Arc::clone(&db), identity);

        let event = SecurityEvent::NewTokenIssued {
            ip_address: Some("192.168.1.1".to_string()),
        };

        let state = Arc::new(AppState::for_test(Arc::clone(&db)));
        notifier.notify(&state, &actor_id, &event).await;

        // The inbox should now contain exactly one message.
        let messages = db.poll_inbox(&actor_id).await.unwrap();
        assert_eq!(
            messages.len(),
            1,
            "inbox should have exactly one security notification"
        );

        // The payload is the canonical inbox envelope (layer 1); decode the
        // SecurityNotice and assert subject+body carry the expected text.
        let (_, payload, _) = &messages[0];
        let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(payload)
            .expect("inbox payload is a canonical envelope");
        assert_eq!(env.kind, fauna_protocol::inbox::InboxKind::SecurityNotice);
        let notice = env
            .decode_security_notice()
            .expect("security notice payload");
        let text = format!("{}\n{}", notice.subject, notice.body);
        assert!(
            text.contains("Security: new sign-in"),
            "notification should contain the event subject, got: {text:?}"
        );
        assert!(
            text.contains("192.168.1.1"),
            "notification should contain the IP address, got: {text:?}"
        );
    }

    #[tokio::test]
    async fn security_notify_delivers_pending_action_to_inbox() {
        let db = make_db();
        let actor_id = [0x55u8; 32];
        let identity = make_nest_identity();

        let notifier = SecurityNotifier::new(Arc::clone(&db), identity);

        let event = SecurityEvent::PendingActionCreated {
            action_id: 42,
            action_type: "key-rotation".to_string(),
            execute_after: 9999999999,
        };

        let state = Arc::new(AppState::for_test(Arc::clone(&db)));
        notifier.notify(&state, &actor_id, &event).await;

        let messages = db.poll_inbox(&actor_id).await.unwrap();
        assert_eq!(
            messages.len(),
            1,
            "inbox should have exactly one security notification"
        );

        let (_, payload, _) = &messages[0];
        let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(payload)
            .expect("inbox payload is a canonical envelope");
        assert_eq!(env.kind, fauna_protocol::inbox::InboxKind::SecurityNotice);
        let notice = env
            .decode_security_notice()
            .expect("security notice payload");
        let text = format!("{}\n{}", notice.subject, notice.body);
        assert!(
            text.contains("key-rotation"),
            "notification should mention the action type, got: {text:?}"
        );
        assert!(
            text.contains("Settings -> Pending actions"),
            "notification should say where to cancel it, got: {text:?}"
        );
    }

    #[tokio::test]
    async fn security_notify_writes_one_notifications_row_per_event() {
        // The notifications row is the render surface all 7 apps already ship
        // (`notifications.md` § Security notices, ruled 2026-08-10) — without it
        // a deployment with no mail domain never shows the user a security
        // notice at all. Two properties pinned here:
        //
        // 1. notify() writes a notifications row alongside the inbox envelope,
        //    with the full notice text (subject AND body — the body carries the
        //    actionable detail: the IP, the pending action id) and `created_at` in
        //    MICROSECONDS (the column's documented unit; a seconds value sorts
        //    the row into 1970 and buries it at the bottom of the list).
        // 2. Every event gets its OWN row: `insert_notification` dedups on
        //    (actor, type, sender, content), so without a per-event dedup token
        //    a second `NewTokenIssued` — a fresh compromise signal — would be
        //    silently swallowed as a duplicate of the first.
        let db = make_db();
        let actor_id = [0x21u8; 32];
        let identity = make_nest_identity();
        let notifier = SecurityNotifier::new(Arc::clone(&db), identity);
        let state = Arc::new(AppState::for_test(Arc::clone(&db)));

        let first = SecurityEvent::NewTokenIssued {
            ip_address: Some("198.51.100.1".to_string()),
        };
        notifier.notify(&state, &actor_id, &first).await;

        let rows = db.list_notifications(&actor_id, None, 10).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "notify() must write exactly one notifications row"
        );
        assert_eq!(
            rows[0].notif_type,
            fauna_protocol::notifications::NotifType::SecurityNotice
        );
        assert_eq!(rows[0].source, "fauna");
        assert!(
            rows[0].summary.contains("Security: new sign-in"),
            "row summary must carry the subject, got: {:?}",
            rows[0].summary
        );
        assert!(
            rows[0].summary.contains("198.51.100.1"),
            "row summary must carry the body detail (the IP), got: {:?}",
            rows[0].summary
        );
        // Micros, not seconds: any plausible current time in micros is > 1e15;
        // the same moment in seconds is ~1.7e9 and would fail this floor.
        assert!(
            rows[0].created_at > 1_000_000_000_000_000,
            "created_at must be microseconds (column unit), got {}",
            rows[0].created_at
        );

        // A second event of the SAME kind must ring separately — the dedup
        // bucket may not swallow it.
        let second = SecurityEvent::NewTokenIssued {
            ip_address: Some("203.0.113.9".to_string()),
        };
        notifier.notify(&state, &actor_id, &second).await;

        let rows = db.list_notifications(&actor_id, None, 10).await.unwrap();
        assert_eq!(
            rows.len(),
            2,
            "each security event must get its own notifications row; the \
             second event was dedup-swallowed"
        );
    }

    #[tokio::test]
    async fn every_security_notice_reaches_the_user_without_a_push_relay() {
        // The surviving floor after channel 2 (the push-relay POST) was deleted. The deletion is only safe because
        // the user is still TOLD, so this pins the two surfaces that tell them
        // rather than merely asserting `notify()` does not blow up:
        //
        //   channel 1  — the inbox system message
        //   channel 1b — the `notifications` row, which is the surface the apps
        //                actually render (`notifications.md` § Security notices)
        //
        // 1b is the load-bearing half: without it, a deployment that has not yet
        // claimed a mail domain (so channel 3 cannot fire either) would show the
        // user nothing at all. That is exactly the state a fresh nest is in, and
        // it is why "channel 1 delivered" alone would be a false floor.
        //
        // There is deliberately no network stub here: with the relay gone there
        // is no outbound call left in `notify()` to stub. The structural half
        // of that claim — this module never grows one back — is pinned below,
        // in `security_notify_carries_no_outbound_http_call`.
        let db = make_db();
        let actor_id = [0x77u8; 32];
        let identity = make_nest_identity();

        let notifier = SecurityNotifier::new(Arc::clone(&db), identity);

        let event = SecurityEvent::AdminChange {
            change_type: "role-grant".to_string(),
            target: "alice".to_string(),
        };

        let state = Arc::new(AppState::for_test(Arc::clone(&db)));
        notifier.notify(&state, &actor_id, &event).await;

        let messages = db.poll_inbox(&actor_id).await.unwrap();
        assert_eq!(
            messages.len(),
            1,
            "channel 1 (inbox) must always deliver — it is the guaranteed channel"
        );

        let rows = db.list_notifications(&actor_id, None, 10).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "channel 1b (the notifications row) must still fire with no push \
             relay — it is the only surface the apps render, so losing it would \
             leave a fresh nest showing the user nothing"
        );
        assert_eq!(
            rows[0].notif_type,
            fauna_protocol::notifications::NotifType::SecurityNotice
        );
        assert!(
            rows[0].summary.contains("admin change"),
            "the rendered row must carry the subject, got: {:?}",
            rows[0].summary
        );
    }

    /// The structural half of the pin above: this module carries zero
    /// outbound HTTP surface today (channel 2, the unsigned push-relay POST,
    /// was deleted — `api-layers.md` § Push relay → *Implementation status
    /// today*), so the "never POST an unsigned notice" rule is free to pin
    /// NOW rather than argued for inside the very change that reintroduces a
    /// call. A future nest→relay call must arrive as a *signed* message in
    /// the `push-token`/`wake` shape and a compiled-in first-party URL —
    /// never an unsigned payload, never a flag.
    #[test]
    fn security_notify_carries_no_outbound_http_call() {
        let src = include_str!("security_notify.rs");
        // Bound the haystack to the non-test region: this file has exactly
        // one `#[cfg(test)]`, and an unbounded search would match the
        // needles quoted in this very assert message — the self-match trap
        // `auth_core.rs`'s guard records.
        let body = src
            .split_once("#[cfg(test)]")
            .expect("security_notify.rs has a #[cfg(test)] module")
            .0;
        for needle in [
            "reqwest::",
            ".post(",
            "hyper::",
            "http::Request",
            "Method::POST",
            "ssrf_safe_https_client",
        ] {
            assert!(
                !body.contains(needle),
                "security_notify.rs grew an outbound HTTP call ({needle:?}) — \
                 any nest to relay call must be a signed message per \
                 api-layers.md Push relay, never an unsigned payload from \
                 this module"
            );
        }
    }

    #[tokio::test]
    async fn security_email_delivers_locally_not_mx_relay() {
        // The email channel always targets a known in-domain actor
        // (`{handle}@{domain}`) → it must seal into that actor's INBOX, NEVER the
        // MX-relay queue (which self-loops and `554`-bounces on a containerized
        // deploy at the inbound HELO-identity check). Regression guard for the
        // nest-internal direct-enqueue self-loop class (smtp-server.md
        // § Outbound submission flow).
        let db = make_db();
        let identity = make_nest_identity();
        let actor_id = [0x55u8; 32];
        db.create_user(&actor_id, "free", "alice test")
            .await
            .unwrap();
        db.set_handle(&actor_id, "alice").await.unwrap();
        crate::test_support::seed_recipient_seal_key(
            &db,
            &actor_id,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        // The email channel resolves the domain from the runtime `local_domains`
        // table (the production path), so register a primary mail domain row —
        // the seam that used to be the boot-time `state.email.domain` field.
        db.add_mail_domain("example.com", true, "testing", "self_signed", None, None)
            .await
            .unwrap();

        let notifier = SecurityNotifier::new(Arc::clone(&db), identity);
        let state = Arc::new(AppState::for_test(Arc::clone(&db)));

        let event = SecurityEvent::NewTokenIssued {
            ip_address: Some("203.0.113.7".to_string()),
        };
        notifier.notify(&state, &actor_id, &event).await;

        // Sealed into alice's mail INBOX (local delivery — distinct from the
        // Channel-1 system-inbox message).
        let inbox = db
            .query_bridge_imap_messages(&actor_id, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "security email must be sealed into the in-domain actor's INBOX"
        );

        // NOTHING on the MX-relay queue (the self-loop bug enqueued it here).
        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            rows.is_empty(),
            "security email must NOT be enqueued for MX relay; got {:?}",
            rows.iter().map(|r| r.recipient.clone()).collect::<Vec<_>>()
        );
    }
}
