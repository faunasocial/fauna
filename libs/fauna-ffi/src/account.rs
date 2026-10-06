//! UniFFI façade for the authenticated `fauna.account.*` / `fauna.quota.get`
//! / `fauna.profile.handle.change` Layer-3 WS-RPC kinds — the personal
//! account-management surface hit from Settings → Account, the status-bar
//! handle, the quota/usage view, and the admin-UI gate.
//!
//! [`FfiAccountClient`] wraps `fauna_client_account::AccountClient` (which in
//! turn wraps the shared `NestClient`); the mirror records below are the
//! FFI-visible shape of `fauna_protocol::account::*`. The Rust-native Linux
//! app (`apps/fauna-linux/src/client.rs`) calls the same `AccountClient`
//! directly — this seam gives Apple / Windows / Android the identical surface
//! over UniFFI.
//!
//! Replies flow one way (wire → client), so the mirrors only implement
//! `From<proto> for Ffi`; the requests are bare scalars taken as method args.
//! The `extra` forward-compat maps are dropped — not FFI-representable and
//! never rendered. Adding a field to a reply struct is a compile error in the
//! `From` impl here, so the mirror can't silently drift (priority #1/#4).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_account::AccountClient;
use fauna_client_account::account::{
    AccountDeleteReply, AccountDeviceLimit, AccountEviction, AccountGetQuota, AccountGetReply,
    AccountNodePolicy, ChangeHandleReply, QuotaDeviceUsage, QuotaFeatures, QuotaGetReply,
    UpgradeReply, UsageBytes,
};
use fauna_protocol::pending_actions::PendingActionSummary;

use crate::{FfiError, stringify};

// ── UsageBytes mirror ──────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::UsageBytes`] — a `{used, max}`
/// byte pair (inbox / storage cells).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiUsageBytes {
    pub used_bytes: i64,
    pub max_bytes: i64,
}

impl From<UsageBytes> for FfiUsageBytes {
    fn from(u: UsageBytes) -> Self {
        FfiUsageBytes {
            used_bytes: u.used_bytes,
            max_bytes: u.max_bytes,
        }
    }
}

// ── AccountGetReply mirrors ────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::AccountEviction`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountEviction {
    pub status: String,
    pub reason: String,
    pub category: String,
    /// Micros since epoch; absent until set.
    pub warned_at: Option<i64>,
    pub suspend_at: Option<i64>,
    pub delete_at: Option<i64>,
    /// One-time data-export token minted at eviction, when present.
    pub export_token: Option<String>,
}

impl From<AccountEviction> for FfiAccountEviction {
    fn from(e: AccountEviction) -> Self {
        FfiAccountEviction {
            status: e.status,
            reason: e.reason,
            category: e.category,
            warned_at: e.warned_at,
            suspend_at: e.suspend_at,
            delete_at: e.delete_at,
            export_token: e.export_token,
        }
    }
}

/// FFI mirror of [`fauna_protocol::account::AccountDeviceLimit`] — carries the
/// tier cap only (the account-state endpoint omits device usage; use
/// `fauna.quota.get` for `used`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountDeviceLimit {
    pub max: i64,
}

impl From<AccountDeviceLimit> for FfiAccountDeviceLimit {
    fn from(d: AccountDeviceLimit) -> Self {
        FfiAccountDeviceLimit { max: d.max }
    }
}

/// FFI mirror of [`fauna_protocol::account::AccountGetQuota`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountGetQuota {
    pub inbox: FfiUsageBytes,
    pub storage: FfiUsageBytes,
    pub devices: FfiAccountDeviceLimit,
}

impl From<AccountGetQuota> for FfiAccountGetQuota {
    fn from(q: AccountGetQuota) -> Self {
        FfiAccountGetQuota {
            inbox: q.inbox.into(),
            storage: q.storage.into(),
            devices: q.devices.into(),
        }
    }
}

/// FFI mirror of [`fauna_protocol::account::AccountNodePolicy`] — eviction
/// policy days, echoed so the client renders eviction warnings without a
/// separate node-info round-trip.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountNodePolicy {
    pub eviction_warning_days: i64,
    pub eviction_suspension_days: i64,
}

impl From<AccountNodePolicy> for FfiAccountNodePolicy {
    fn from(p: AccountNodePolicy) -> Self {
        FfiAccountNodePolicy {
            eviction_warning_days: p.eviction_warning_days,
            eviction_suspension_days: p.eviction_suspension_days,
        }
    }
}

/// FFI mirror of [`fauna_protocol::account::AccountGetReply`] — full account
/// state (the transparency endpoint). `eviction` is present only when the
/// account is under eviction.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountGetReply {
    /// 64-char hex of the calling actor's public key.
    pub actor_id: String,
    /// Current handle, when one is set.
    pub handle: Option<String>,
    pub tier: String,
    /// Account creation timestamp (micros since epoch).
    pub created_at: i64,
    pub eviction: Option<FfiAccountEviction>,
    pub quota: FfiAccountGetQuota,
    pub node_policy: FfiAccountNodePolicy,
}

impl From<AccountGetReply> for FfiAccountGetReply {
    fn from(r: AccountGetReply) -> Self {
        FfiAccountGetReply {
            actor_id: r.actor_id,
            handle: r.handle,
            tier: r.tier,
            created_at: r.created_at,
            eviction: r.eviction.map(Into::into),
            quota: r.quota.into(),
            node_policy: r.node_policy.into(),
        }
    }
}

// ── QuotaGetReply mirrors ──────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::QuotaDeviceUsage`] — `used` (live
/// count) + `max` (tier cap). Distinct from [`FfiAccountDeviceLimit`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiQuotaDeviceUsage {
    pub used: i64,
    pub max: i64,
}

impl From<QuotaDeviceUsage> for FfiQuotaDeviceUsage {
    fn from(d: QuotaDeviceUsage) -> Self {
        FfiQuotaDeviceUsage {
            used: d.used,
            max: d.max,
        }
    }
}

/// FFI mirror of [`fauna_protocol::account::QuotaFeatures`] — tier-derived
/// feature flags.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiQuotaFeatures {
    pub versioned_backup: bool,
    pub bridges: bool,
    pub max_feeds: i64,
}

impl From<QuotaFeatures> for FfiQuotaFeatures {
    fn from(f: QuotaFeatures) -> Self {
        FfiQuotaFeatures {
            versioned_backup: f.versioned_backup,
            bridges: f.bridges,
            max_feeds: f.max_feeds,
        }
    }
}

/// FFI mirror of [`fauna_protocol::account::QuotaGetReply`] — tier-aware usage
/// breakdown.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiQuotaGetReply {
    pub tier: String,
    pub inbox: FfiUsageBytes,
    pub storage: FfiUsageBytes,
    pub devices: FfiQuotaDeviceUsage,
    pub features: FfiQuotaFeatures,
}

impl From<QuotaGetReply> for FfiQuotaGetReply {
    fn from(r: QuotaGetReply) -> Self {
        FfiQuotaGetReply {
            tier: r.tier,
            inbox: r.inbox.into(),
            storage: r.storage.into(),
            devices: r.devices.into(),
            features: r.features.into(),
        }
    }
}

// ── ChangeHandleReply mirror ───────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::ChangeHandleReply`] — the queued
/// handle-change pending action (delayed + cancellable; poll `pending-actions`
/// for status).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChangeHandleReply {
    pub pending_action_id: i64,
    /// Earliest execution timestamp (micros since epoch).
    pub execute_after: i64,
    /// Always `"pending"` on creation.
    pub status: String,
    pub new_handle: String,
}

impl From<ChangeHandleReply> for FfiChangeHandleReply {
    fn from(r: ChangeHandleReply) -> Self {
        FfiChangeHandleReply {
            pending_action_id: r.pending_action_id,
            execute_after: r.execute_after,
            status: r.status,
            new_handle: r.new_handle,
        }
    }
}

// ── UpgradeReply mirror ────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::UpgradeReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiUpgradeReply {
    /// Always `true` on success.
    pub ok: bool,
    /// The granted (now-current) tier.
    pub tier: String,
}

impl From<UpgradeReply> for FfiUpgradeReply {
    fn from(r: UpgradeReply) -> Self {
        FfiUpgradeReply {
            ok: r.ok,
            tier: r.tier,
        }
    }
}

// ── AccountDeleteReply mirror ──────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::account::AccountDeleteReply`] — the queued
/// account-deletion pending action (sits in the queue for the cancellation
/// window before executing).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountDeleteReply {
    pub pending_action_id: i64,
    /// Earliest execution timestamp (micros since epoch).
    pub execute_after: i64,
    /// Always `"pending"` on creation.
    pub status: String,
    /// Human-readable confirmation line.
    pub message: String,
}

impl From<AccountDeleteReply> for FfiAccountDeleteReply {
    fn from(r: AccountDeleteReply) -> Self {
        FfiAccountDeleteReply {
            pending_action_id: r.pending_action_id,
            execute_after: r.execute_after,
            status: r.status,
            message: r.message,
        }
    }
}

// ── PendingActionSummary mirror (`settings.md` § Pending actions) ──────

/// FFI mirror of [`fauna_protocol::pending_actions::PendingActionSummary`] —
/// one row in the calling actor's pending-actions list. The `extra`
/// forward-compat map is dropped, matching every other mirror in this file.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPendingActionSummary {
    pub id: i64,
    /// `handle.change` / `account.delete` / `snapshot.delete` / …
    pub action_type: String,
    pub target: Option<String>,
    /// `pending` / `executed` / `cancelled` / `expired`.
    pub status: String,
    pub created_at: i64,
    pub execute_after: i64,
    pub requires_quorum: i64,
    pub approvals: Vec<String>,
}

impl From<PendingActionSummary> for FfiPendingActionSummary {
    fn from(r: PendingActionSummary) -> Self {
        FfiPendingActionSummary {
            id: r.id,
            action_type: r.action_type,
            target: r.target,
            status: r.status,
            created_at: r.created_at,
            execute_after: r.execute_after,
            requires_quorum: r.requires_quorum,
            approvals: r.approvals,
        }
    }
}

/// `fauna_protocol::pending_actions::describe_pending_action` — what a
/// scheduled action will do, as one sentence (`pending-action-description`'s
/// `ui.yaml` contract). Shared with tui/linux/web so an unrecognized
/// `action_type` (client/nest skew) paints the same raw fallback everywhere.
/// See `docs/goal/ui/settings.md` § Pending actions.
#[uniffi::export]
pub fn describe_pending_action(action_type: String, target: Option<String>) -> String {
    fauna_protocol::pending_actions::describe_pending_action(&action_type, target.as_deref())
}

// ── FfiAccountClient ───────────────────────────────────────────────────

/// UniFFI handle for the `fauna.account.*` / `fauna.quota.get` /
/// `fauna.profile.handle.change` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::account`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`. All kinds ride the
/// bearer connection — the calling actor is the connection actor.
#[derive(uniffi::Object)]
pub struct FfiAccountClient {
    nest: Arc<NestClient>,
}

impl FfiAccountClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> AccountClient<Arc<NestClient>> {
        AccountClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiAccountClient {
    /// `fauna.account.get` — full account state for the calling actor.
    pub async fn get(&self) -> Result<FfiAccountGetReply, FfiError> {
        let reply = self.client().get().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.quota.get` — tier-aware usage breakdown.
    pub async fn quota_get(&self) -> Result<FfiQuotaGetReply, FfiError> {
        let reply = self.client().quota_get().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.account.am_i_admin` — whether the calling actor is a nest admin
    /// (so the client shows/hides admin UI without a separate round-trip).
    pub async fn am_i_admin(&self) -> Result<bool, FfiError> {
        let reply = self.client().am_i_admin().await.map_err(stringify)?;
        Ok(reply.admin)
    }

    /// `fauna.profile.handle.change` — queue a handle change as a pending
    /// action; the reply carries the pending-action id + execute-after.
    pub async fn change_handle(&self, handle: String) -> Result<FfiChangeHandleReply, FfiError> {
        let reply = self
            .client()
            .change_handle(handle)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.account.upgrade` — move to a higher tier, consuming an invite
    /// code that authorizes it.
    pub async fn upgrade(
        &self,
        tier: String,
        invite_code: String,
    ) -> Result<FfiUpgradeReply, FfiError> {
        let reply = self
            .client()
            .upgrade(tier, invite_code)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.account.delete` — queue account deletion as a pending action
    /// with a cancellation window. No sign-out, no navigation: the account
    /// stays active until the pending action executes, and the
    /// pending-actions list is the receipt + the way back (`settings.md`
    /// § Pending actions).
    pub async fn delete(&self) -> Result<FfiAccountDeleteReply, FfiError> {
        let reply = self.client().delete().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.pending_actions.list` — this actor's queued destructive
    /// operations (ALL statuses, newest first), unfiltered: the caller
    /// narrows to still-`pending` rows, mirroring tui's/linux's own
    /// `list_pending_actions` helper (`settings.md` § Pending actions).
    pub async fn pending_actions_list(&self) -> Result<Vec<FfiPendingActionSummary>, FfiError> {
        let reply = self
            .client()
            .pending_actions_list()
            .await
            .map_err(stringify)?;
        Ok(reply.actions.into_iter().map(Into::into).collect())
    }

    /// `fauna.pending_actions.cancel` — cancel a scheduled action before it
    /// executes (one click, no confirm — cancelling is the safe direction).
    pub async fn pending_action_cancel(&self, id: i64) -> Result<bool, FfiError> {
        let reply = self
            .client()
            .pending_action_cancel(id)
            .await
            .map_err(stringify)?;
        Ok(reply.ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(used: i64, max: i64) -> UsageBytes {
        UsageBytes {
            used_bytes: used,
            max_bytes: max,
            extra: Default::default(),
        }
    }

    #[test]
    fn account_get_reply_maps_with_eviction() {
        let proto = AccountGetReply {
            actor_id: "ab".repeat(32),
            handle: Some("alice".into()),
            tier: "personal".into(),
            created_at: 1_700_000_000_000,
            eviction: Some(AccountEviction {
                status: "warned".into(),
                reason: "over quota".into(),
                category: "storage".into(),
                warned_at: Some(1),
                suspend_at: None,
                delete_at: None,
                export_token: Some("tok".into()),
                extra: Default::default(),
            }),
            quota: AccountGetQuota {
                inbox: usage(10, 100),
                storage: usage(20, 200),
                devices: AccountDeviceLimit {
                    max: 5,
                    extra: Default::default(),
                },
                extra: Default::default(),
            },
            node_policy: AccountNodePolicy {
                eviction_warning_days: 30,
                eviction_suspension_days: 60,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let ffi: FfiAccountGetReply = proto.into();
        assert_eq!(ffi.handle.as_deref(), Some("alice"));
        assert_eq!(ffi.tier, "personal");
        assert_eq!(ffi.quota.inbox.max_bytes, 100);
        assert_eq!(ffi.quota.devices.max, 5);
        assert_eq!(ffi.node_policy.eviction_suspension_days, 60);
        let ev = ffi.eviction.expect("eviction present");
        assert_eq!(ev.status, "warned");
        assert_eq!(ev.export_token.as_deref(), Some("tok"));
    }

    #[test]
    fn account_get_reply_maps_without_eviction() {
        let proto = AccountGetReply {
            actor_id: "cd".repeat(32),
            handle: None,
            tier: "free".into(),
            created_at: 0,
            eviction: None,
            quota: AccountGetQuota {
                inbox: usage(0, 0),
                storage: usage(0, 0),
                devices: AccountDeviceLimit {
                    max: 1,
                    extra: Default::default(),
                },
                extra: Default::default(),
            },
            node_policy: AccountNodePolicy {
                eviction_warning_days: 0,
                eviction_suspension_days: 0,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let ffi: FfiAccountGetReply = proto.into();
        assert_eq!(ffi.handle, None);
        assert!(ffi.eviction.is_none());
    }

    #[test]
    fn quota_get_reply_maps() {
        let proto = QuotaGetReply {
            tier: "community".into(),
            inbox: usage(5, 50),
            storage: usage(6, 60),
            devices: QuotaDeviceUsage {
                used: 2,
                max: 10,
                extra: Default::default(),
            },
            features: QuotaFeatures {
                versioned_backup: true,
                bridges: false,
                max_feeds: 25,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let ffi: FfiQuotaGetReply = proto.into();
        assert_eq!(ffi.tier, "community");
        assert_eq!(ffi.devices.used, 2);
        assert!(ffi.features.versioned_backup);
        assert!(!ffi.features.bridges);
        assert_eq!(ffi.features.max_feeds, 25);
    }

    #[test]
    fn change_handle_reply_maps() {
        let proto = ChangeHandleReply {
            pending_action_id: 7,
            execute_after: 123,
            status: "pending".into(),
            new_handle: "bob".into(),
            extra: Default::default(),
        };
        let ffi: FfiChangeHandleReply = proto.into();
        assert_eq!(ffi.pending_action_id, 7);
        assert_eq!(ffi.new_handle, "bob");
    }

    #[test]
    fn upgrade_reply_maps() {
        let proto = UpgradeReply {
            ok: true,
            tier: "personal".into(),
            extra: Default::default(),
        };
        let ffi: FfiUpgradeReply = proto.into();
        assert!(ffi.ok);
        assert_eq!(ffi.tier, "personal");
    }

    #[test]
    fn account_delete_reply_maps() {
        let proto = AccountDeleteReply {
            pending_action_id: 9,
            execute_after: 456,
            status: "pending".into(),
            message: "Deletion scheduled".into(),
            extra: Default::default(),
        };
        let ffi: FfiAccountDeleteReply = proto.into();
        assert_eq!(ffi.pending_action_id, 9);
        assert_eq!(ffi.message, "Deletion scheduled");
    }

    #[test]
    fn pending_action_summary_maps() {
        let proto = PendingActionSummary {
            id: 7,
            action_type: "handle.change".into(),
            target: Some("bob".into()),
            status: "pending".into(),
            created_at: 100,
            execute_after: 456,
            requires_quorum: 0,
            approvals: vec!["ab".repeat(32)],
            extra: Default::default(),
        };
        let ffi: FfiPendingActionSummary = proto.into();
        assert_eq!(ffi.id, 7);
        assert_eq!(ffi.action_type, "handle.change");
        assert_eq!(ffi.target.as_deref(), Some("bob"));
        assert_eq!(ffi.execute_after, 456);
        assert_eq!(ffi.approvals, vec!["ab".repeat(32)]);
    }

    #[test]
    fn describe_pending_action_delegates_to_the_shared_renderer() {
        assert_eq!(
            describe_pending_action("handle.change".into(), Some("bob".into())),
            fauna_protocol::pending_actions::describe_pending_action("handle.change", Some("bob")),
        );
    }
}
