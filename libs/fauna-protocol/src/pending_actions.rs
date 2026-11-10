//! Pending-actions WS-RPC payload types. A behavior-preserving transport
//! migration of the (now-deleted) bearer-authed HTTP routes
//! `/api/v1/pending-actions{,/{id},/{id}/cancel,/{id}/approve}` onto the per-actor WS-RPC
//! connection — Track B20 of the WS-RPC-everywhere migration
//! (tracked internally). The handler logic
//! reuses the existing `CacheDb` pending-action methods exactly (no shared
//! core needed — each handler is a single DB call plus reply shaping), mirroring
//! the authenticated account surface in `account_handlers`.
//!
//! The four kinds (api-layers.md § Pending Actions, "Migrating to WS-RPC:
//! `pendingActions.{list,get,cancel,approve}`"):
//!
//! - `fauna.pending_actions.list` ≡ GET `/api/v1/pending-actions` — the calling
//!   actor's queued destructive operations (all statuses), newest first.
//! - `fauna.pending_actions.get` ≡ GET `/api/v1/pending-actions/{id}` — one
//!   action's detail; scoped to the connection actor (a row owned by another
//!   actor → `fauna.pending_actions.permission_denied`, mirroring the twin's
//!   403).
//! - `fauna.pending_actions.cancel` ≡ POST `/api/v1/pending-actions/{id}/cancel`
//!   — cancel before execution; the DB layer enforces the creator/target/admin
//!   authorization matrix.
//! - `fauna.pending_actions.approve` ≡ POST
//!   `/api/v1/pending-actions/{id}/approve` — quorum approval. **Admin-only**
//!   (the twin used `AdminBearerAuth`); enforced in
//!   `bridge_method_allowlist::is_permitted`.
//!
//! These ride the **bearer** connection (the calling actor IS the connection
//! `actor_id`), unlike the pre-identity kinds in `account.rs`.
//!
//! Wire convention (matching `account.rs` / `auth.rs`): the dag-cbor wire forbids
//! floats (every numeric field here is an `i64`/`bool`) and does not round-trip
//! `Option<Option>` (every optional is a plain `Option`). The twin emitted
//! `approvals` as a raw JSON-array *string*; the typed wire models it properly as
//! `Vec<String>` (the parsed list of approver-actor hexes) — same information,
//! better typed. `requires_quorum` keeps the DB/twin's name and `i64` shape (it
//! is the minimum-approvals count, `0` = no quorum needed).
//!
//! Kind registry: `kind.rs::register_pending_actions_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.pending_actions.list (≡ GET /api/v1/pending-actions) ──────────────

/// List the calling actor's pending actions. No parameters — the actor scope is
/// the connection actor (the twin keyed it on the bearer actor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row in the list response — the summary projection the twin emitted
/// (omits `payload` / `executed_at` / `ip_address`, which only the per-id detail
/// carries).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionSummary {
    /// Auto-increment row id (the `{id}` path-param for get/cancel/approve).
    pub id: i64,
    /// Action discriminator (`account.delete`, `handle.change`, `snapshot.delete`, …).
    pub action_type: String,
    /// Optional target (user actor_id hex / handle / snapshot id); `None` for
    /// self-scoped actions.
    pub target: Option<String>,
    /// `pending` / `executing` / `executed` / `cancelled` / `expired`. `executing`
    /// is the executor's transient claim on a running action; apps list only
    /// `pending` rows.
    pub status: String,
    pub created_at: i64,
    pub execute_after: i64,
    /// Minimum approvals required before execution (`0` = no quorum).
    pub requires_quorum: i64,
    /// Approver-actor hexes that have approved so far (parsed from the stored
    /// JSON array; empty when none).
    pub approvals: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl PendingActionSummary {
    /// What will happen, as one sentence — verb + target
    /// (`pending-action-description`'s `ui.yaml` contract). See
    /// [`describe_pending_action`] for the rendering rule.
    pub fn description(&self) -> String {
        describe_pending_action(&self.action_type, self.target.as_deref())
    }

    /// [`Self::description`] naming the folder an automatic prune targets
    /// through `folder_name` — see [`describe_pending_action_naming`].
    pub fn description_naming(&self, folder_name: impl Fn(&str) -> Option<String>) -> String {
        describe_pending_action_naming(&self.action_type, self.target.as_deref(), folder_name)
    }

    /// Still open to cancel — `status == "pending"`. The one predicate every
    /// app lists by (an `executing` / `executed` / `cancelled` / `expired` row
    /// has no cancel window left).
    pub fn is_pending(&self) -> bool {
        self.status == STATUS_PENDING
    }
}

/// The wire spelling of a still-open action's `status`.
pub const STATUS_PENDING: &str = "pending";

impl PendingActionsListReply {
    /// The reply's still-`pending` rows, served order (newest first) kept.
    pub fn into_pending(self) -> Vec<PendingActionSummary> {
        self.actions
            .into_iter()
            .filter(|a| a.is_pending())
            .collect()
    }
}

/// What will happen, as one sentence — verb + target
/// (`pending-action-description`'s `ui.yaml` contract). An unrecognized
/// `action_type` (a newer nest scheduling a kind this build predates) paints
/// the raw discriminator plus its target rather than nothing — a row a user
/// cannot READ is a row they cannot decide to cancel, and full bidirectional
/// client↔nest skew is the normal state. Shared by every app that renders a
/// pending-action row (tui `settings/account.rs`, linux `settings/pending_actions.rs`),
/// and by the admin console's pending-admin-actions section for every admin
/// `ActionType` — the same sentence the target reads on their own list
/// ("Delete the account …"), so a row means one thing wherever it is shown.
///
/// An automatic retention prune (`version.bulk_prune`, `snapshot.bulk_prune`)
/// targets a folder by its id, which is no name a user knows: this form paints
/// the neutral sentence, and [`describe_pending_action_naming`] names the
/// folder where the app holds its name.
pub fn describe_pending_action(action_type: &str, target: Option<&str>) -> String {
    describe_pending_action_naming(action_type, target, |_| None)
}

/// [`describe_pending_action_naming`] for a caller that has already resolved
/// the one folder name (or none) — the wasm and UniFFI faces' shared form, so
/// an app over a foreign-language boundary passes the summary plus the name its
/// folder roster gave (`DevicesMachine::folder_name`), never a resolver of its
/// own. `None` paints the neutral sentence.
pub fn describe_pending_action_with_folder(
    action_type: &str,
    target: Option<&str>,
    folder_name: Option<&str>,
) -> String {
    describe_pending_action_naming(action_type, target, |_| folder_name.map(str::to_owned))
}

/// [`describe_pending_action`], with `folder_name` resolving the folder id an
/// automatic prune targets to the name the app already holds (its folder
/// roster, unsealed). `None` — no roster yet, a folder this device cannot
/// name — paints the neutral sentence with no id: the id is never shown
/// (`ui/settings.md` § Pending actions, "what will happen, as one sentence").
pub fn describe_pending_action_naming(
    action_type: &str,
    target: Option<&str>,
    folder_name: impl Fn(&str) -> Option<String>,
) -> String {
    use fauna_i18n::strings::settings::pending_actions as pa;
    let named = |target: Option<&str>| {
        target
            .and_then(&folder_name)
            .filter(|name| !name.trim().is_empty())
    };
    match (action_type, target) {
        ("version.bulk_prune", folder) => match named(folder) {
            Some(name) => pa::prune_versions_in(&name),
            None => pa::PRUNE_VERSIONS.to_string(),
        },
        ("snapshot.bulk_prune", folder) => match named(folder) {
            Some(name) => pa::prune_snapshots_of(&name),
            None => pa::PRUNE_SNAPSHOTS.to_string(),
        },
        ("handle.change", Some(handle)) => pa::change_handle_to(handle),
        ("account.delete", _) => pa::DELETE_ACCOUNT.to_string(),
        ("snapshot.delete", Some(snapshot)) => pa::delete_snapshot(snapshot),
        ("admin.delete_user", Some(target)) => pa::admin_delete_user(target),
        ("admin.add", Some(target)) => pa::admin_add(target),
        ("admin.remove", Some(target)) => pa::admin_remove(target),
        ("admin.change_role", Some(target)) => pa::admin_change_role(target),
        (other, Some(target)) => format!("{other}: {target}"),
        (other, None) => other.to_string(),
    }
}

/// The list reply — the calling actor's actions, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionsListReply {
    pub actions: Vec<PendingActionSummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.pending_actions.get (≡ GET /api/v1/pending-actions/{id}) ──────────

/// Fetch one pending action by id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionGetRequest {
    pub id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The per-id detail — the full projection the twin emitted (adds `payload`,
/// `executed_at`, `ip_address` over the list summary).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionGetReply {
    pub id: i64,
    pub action_type: String,
    pub target: Option<String>,
    /// Action-specific JSON parameters (e.g. `{"new_handle":"…"}`); `None` when
    /// the action carries no payload.
    pub payload: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub execute_after: i64,
    /// Unix-seconds the executor ran the action; `None` while still pending /
    /// cancelled.
    pub executed_at: Option<i64>,
    pub requires_quorum: i64,
    pub approvals: Vec<String>,
    /// Requestor IP captured at creation (audit trail); `None` when not recorded
    /// (e.g. WS-RPC-created actions — no per-request IP yet, the A1.1 deferral).
    pub ip_address: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.pending_actions.cancel (≡ POST …/{id}/cancel) ─────────────────────

/// Cancel a pending action before it executes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionCancelRequest {
    pub id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success (the twin's body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionCancelReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.pending_actions.approve (≡ POST …/{id}/approve) ───────────────────

/// Approve a quorum action (admin-only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionApproveRequest {
    pub id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success (the twin's body). A duplicate approval from the
/// same approver is idempotent (the DB silently succeeds).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingActionApproveReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn only_pending_rows_survive_into_pending_in_served_order() {
        let row = |id: i64, status: &str| PendingActionSummary {
            id,
            action_type: "account.delete".into(),
            target: None,
            status: status.into(),
            created_at: 0,
            execute_after: 0,
            requires_quorum: 0,
            approvals: vec![],
            extra: BTreeMap::new(),
        };
        let reply = PendingActionsListReply {
            actions: vec![
                row(4, "pending"),
                row(3, "executing"),
                row(2, "cancelled"),
                row(1, "pending"),
            ],
            extra: BTreeMap::new(),
        };
        let ids: Vec<i64> = reply.into_pending().iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![4, 1]);
    }

    #[test]
    fn list_request_and_reply_round_trip() {
        assert_round_trips(&PendingActionsListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PendingActionsListReply {
            actions: vec![
                PendingActionSummary {
                    id: 1,
                    action_type: "account.delete".into(),
                    target: None,
                    status: "pending".into(),
                    created_at: 1_700_000_000,
                    execute_after: 1_701_209_600,
                    requires_quorum: 0,
                    approvals: vec![],
                    extra: BTreeMap::new(),
                },
                PendingActionSummary {
                    id: 2,
                    action_type: "admin.delete_user".into(),
                    target: Some("ab".repeat(32)),
                    status: "pending".into(),
                    created_at: 1_700_000_100,
                    execute_after: 1_700_086_500,
                    requires_quorum: 2,
                    approvals: vec!["cd".repeat(32)],
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
        // Empty list round-trips to an empty Vec.
        let empty = PendingActionsListReply {
            actions: vec![],
            extra: BTreeMap::new(),
        };
        let decoded: PendingActionsListReply = decode(&encode_canonical(&empty).unwrap()).unwrap();
        assert!(decoded.actions.is_empty());
    }

    /// `pending-action-description`'s contract: verb + target as one
    /// sentence; an unrecognized action_type paints the raw discriminator
    /// (+ target) rather than nothing — client↔nest skew is the normal
    /// state, and an unreadable row cannot be decided on. The one canonical
    /// copy of a test every app (tui, linux) used to carry independently.
    #[test]
    fn describe_pending_action_names_each_verb_and_survives_skew() {
        use fauna_i18n::strings::settings::pending_actions as pa;
        assert_eq!(
            describe_pending_action("handle.change", Some("bob")),
            pa::change_handle_to("bob")
        );
        assert_eq!(
            describe_pending_action("account.delete", None),
            pa::DELETE_ACCOUNT
        );
        assert_eq!(
            describe_pending_action("snapshot.delete", Some("snap-3")),
            pa::delete_snapshot("snap-3")
        );
        assert_eq!(
            describe_pending_action("mailbox.purge", Some("inbox")),
            "mailbox.purge: inbox"
        );
        assert_eq!(
            describe_pending_action("mailbox.purge", None),
            "mailbox.purge"
        );
    }

    /// The automatic prunes name the folder where the app holds its name and
    /// otherwise read the neutral sentence — never the wire kind, never the
    /// folder id (`ui/settings.md` § Pending actions).
    #[test]
    fn describe_pending_action_names_the_folder_of_an_automatic_prune_never_its_id() {
        use fauna_i18n::strings::settings::pending_actions as pa;
        let id = "4711";
        let roster = |wanted: &str| (wanted == id).then(|| "Photos".to_string());
        for (kind, named, neutral) in [
            (
                "version.bulk_prune",
                pa::prune_versions_in("Photos"),
                pa::PRUNE_VERSIONS.to_string(),
            ),
            (
                "snapshot.bulk_prune",
                pa::prune_snapshots_of("Photos"),
                pa::PRUNE_SNAPSHOTS.to_string(),
            ),
        ] {
            let with_name = describe_pending_action_naming(kind, Some(id), roster);
            assert_eq!(with_name, named);
            assert!(with_name.contains("Photos"));
            let without = [
                describe_pending_action(kind, Some(id)),
                describe_pending_action_naming(kind, Some("9999"), roster),
                describe_pending_action_naming(kind, Some(id), |_| Some("  ".to_string())),
                describe_pending_action(kind, None),
            ];
            for text in std::iter::once(&with_name).chain(without.iter()) {
                assert!(
                    !text.contains(id),
                    "{kind}: the folder id leaked into {text:?}"
                );
                assert!(
                    !text.contains(kind),
                    "{kind}: the wire kind leaked into {text:?}"
                );
                assert!(!text.contains("bulk_prune"), "{text:?}");
            }
            for text in &without {
                assert_eq!(text, &neutral);
            }
        }
    }

    /// The one-name form the wasm and UniFFI faces share paints exactly what
    /// the resolver form does: the named sentence for a name, the neutral one
    /// for none or a blank, and a non-prune kind untouched by any name.
    #[test]
    fn describe_pending_action_with_folder_matches_the_resolver_form() {
        use fauna_i18n::strings::settings::pending_actions as pa;
        assert_eq!(
            describe_pending_action_with_folder("version.bulk_prune", Some("4711"), Some("Photos")),
            pa::prune_versions_in("Photos")
        );
        assert_eq!(
            describe_pending_action_with_folder("snapshot.bulk_prune", Some("4711"), None),
            pa::PRUNE_SNAPSHOTS.to_string()
        );
        assert_eq!(
            describe_pending_action_with_folder("version.bulk_prune", Some("4711"), Some(" ")),
            pa::PRUNE_VERSIONS.to_string()
        );
        assert_eq!(
            describe_pending_action_with_folder("handle.change", Some("bob"), Some("Photos")),
            describe_pending_action("handle.change", Some("bob"))
        );
    }

    #[test]
    fn description_method_delegates_to_describe_pending_action() {
        let summary = PendingActionSummary {
            id: 1,
            action_type: "handle.change".into(),
            target: Some("bob".into()),
            status: "pending".into(),
            created_at: 1_700_000_000,
            execute_after: 1_700_021_600,
            requires_quorum: 0,
            approvals: vec![],
            extra: BTreeMap::new(),
        };
        assert_eq!(
            summary.description(),
            describe_pending_action("handle.change", Some("bob"))
        );
    }

    #[test]
    fn get_request_and_reply_round_trip_with_and_without_optionals() {
        assert_round_trips(&PendingActionGetRequest {
            id: 42,
            extra: BTreeMap::new(),
        });
        // Full detail (every Option present).
        assert_round_trips(&PendingActionGetReply {
            id: 7,
            action_type: "handle.change".into(),
            target: Some("alice".into()),
            payload: Some(r#"{"new_handle":"bob"}"#.into()),
            status: "executed".into(),
            created_at: 1_700_000_000,
            execute_after: 1_700_021_600,
            executed_at: Some(1_700_021_700),
            requires_quorum: 0,
            approvals: vec![],
            ip_address: Some("203.0.113.7".into()),
            extra: BTreeMap::new(),
        });
        // All optionals None → null on the wire → round-trips back to None
        // (plain Option, not Option<Option>).
        let bare = PendingActionGetReply {
            id: 8,
            action_type: "account.delete".into(),
            target: None,
            payload: None,
            status: "pending".into(),
            created_at: 1_700_000_000,
            execute_after: 1_701_209_600,
            executed_at: None,
            requires_quorum: 0,
            approvals: vec![],
            ip_address: None,
            extra: BTreeMap::new(),
        };
        let decoded: PendingActionGetReply = decode(&encode_canonical(&bare).unwrap()).unwrap();
        assert_eq!(bare, decoded);
        assert!(decoded.target.is_none());
        assert!(decoded.payload.is_none());
        assert!(decoded.executed_at.is_none());
        assert!(decoded.ip_address.is_none());
    }

    #[test]
    fn cancel_and_approve_round_trip() {
        assert_round_trips(&PendingActionCancelRequest {
            id: 1,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PendingActionCancelReply {
            ok: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PendingActionApproveRequest {
            id: 1,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PendingActionApproveReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }
}
