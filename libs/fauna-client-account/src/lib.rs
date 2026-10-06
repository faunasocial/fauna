//! Typed-call wrapper for the authenticated `fauna.account.*` /
//! `fauna.quota.get` / `fauna.profile.handle.change` WS-RPC kinds — the
//! personal account-management surface clients hit from Settings → Account, the
//! status-bar handle, the quota/usage view, and the admin-UI gate (part of the
//! WS-RPC-everywhere migration; tracked internally).
//!
//! Pattern: same shape as `fauna-client-feed` / `fauna-client-posts` — a thin
//! `pub struct AccountClient<R: RpcRequester> { nest: R }`, one async method per
//! kind, no state machine, generic over the WS-RPC transport so the kind logic
//! is written once and shared across native + wasm (priority #2): native call
//! sites pass `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`. This
//! crate is the transport surface only; the account view-model lives in the
//! client's shared state layer.
//!
//! All kinds ride the **bearer** connection — the calling actor is the
//! connection actor (the HTTP twins took it from the bearer/path). `register`
//! is **not** here: it is a pre-identity kind on the anonymous connection
//! (`fauna-protocol::account::RegisterRequest`, the onboarding flow), a
//! different transport surface.

use fauna_protocol::RpcRequester;
use fauna_protocol::account::{
    AccountDeleteReply, AccountDeleteRequest, AccountGetReply, AccountGetRequest, AmIAdminReply,
    AmIAdminRequest, ChangeHandleReply, ChangeHandleRequest, QuotaGetReply, QuotaGetRequest,
    UpgradeReply, UpgradeRequest,
};

pub use fauna_protocol::account;

pub mod sessions;
pub mod sessions_view;
pub use sessions::{SessionsClient, SessionsError};

/// Typed `fauna.account.*` / `fauna.quota.get` / `fauna.profile.handle.change`
/// call surface, generic over the WS-RPC transport (`R: RpcRequester`): native
/// call sites pass `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`.
/// Errors propagate as the transport's `R::Error` (native `NestClientError`,
/// wasm rpc-wasm error).
pub struct AccountClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> AccountClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.account.get` — full account state for the calling actor (handle,
    /// tier, created_at, eviction status, quota, node policy). Pure read;
    /// replay-safe at 5 s.
    pub async fn get(&self) -> Result<AccountGetReply, R::Error> {
        self.nest
            .request(
                "fauna.account.get",
                AccountGetRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.quota.get` — tier-aware usage breakdown (inbox/storage bytes,
    /// device count, feature flags). Pure read; replay-safe at 5 s.
    pub async fn quota_get(&self) -> Result<QuotaGetReply, R::Error> {
        self.nest
            .request(
                "fauna.quota.get",
                QuotaGetRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.account.am_i_admin` — whether the calling actor is a nest admin
    /// (so the client can show/hide admin UI without a separate round-trip).
    /// Pure read; replay-safe at 5 s.
    pub async fn am_i_admin(&self) -> Result<AmIAdminReply, R::Error> {
        self.nest
            .request(
                "fauna.account.am_i_admin",
                AmIAdminRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.profile.handle.change` — queue a handle change as a pending
    /// action (delayed + cancellable). The reply carries the pending-action id
    /// + execute-after; the client polls `pending-actions` for status.
    pub async fn change_handle(
        &self,
        handle: impl Into<String>,
    ) -> Result<ChangeHandleReply, R::Error> {
        self.nest
            .request(
                "fauna.profile.handle.change",
                ChangeHandleRequest {
                    handle: handle.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.account.upgrade` — move the account to a higher tier, consuming an
    /// invite code that authorizes it. The reply echoes the granted tier.
    pub async fn upgrade(
        &self,
        tier: impl Into<String>,
        invite_code: impl Into<String>,
    ) -> Result<UpgradeReply, R::Error> {
        self.nest
            .request(
                "fauna.account.upgrade",
                UpgradeRequest {
                    tier: tier.into(),
                    invite_code: invite_code.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.account.delete` — queue account deletion as a pending action with
    /// a cancellation window. The reply carries the pending-action id +
    /// execute-after.
    pub async fn delete(&self) -> Result<AccountDeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.account.delete",
                AccountDeleteRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.pending_actions.list` — the calling actor's queued destructive
    /// operations (all statuses, newest first): the read half of the
    /// cancellation window the three delayed verbs open (`ui/settings.md`
    /// § Pending actions — the standing account-page section). Pure read.
    pub async fn pending_actions_list(
        &self,
    ) -> Result<fauna_protocol::pending_actions::PendingActionsListReply, R::Error> {
        self.nest
            .request(
                "fauna.pending_actions.list",
                fauna_protocol::pending_actions::PendingActionsListRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.pending_actions.cancel` — cancel a scheduled action before it
    /// executes (one click, no confirm — cancelling is the safe direction;
    /// the creator/target/admin authorization matrix is enforced nest-side).
    /// The admin-only `approve` kind is deliberately NOT here — it belongs to
    /// the admin surface, not the personal account one.
    pub async fn pending_action_cancel(
        &self,
        id: i64,
    ) -> Result<fauna_protocol::pending_actions::PendingActionCancelReply, R::Error> {
        self.nest
            .request(
                "fauna.pending_actions.cancel",
                fauna_protocol::pending_actions::PendingActionCancelRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = AccountClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `AccountClient` method must
    // send its exact `fauna.account.*` / `fauna.quota.get` /
    // `fauna.profile.handle.change` kind and a payload that round-trips back to
    // the typed request. No nest-side conformance test routes through this
    // adapter (they use literal kind strings), so an adapter-method kind rename
    // is otherwise caught by nothing. The pattern mirrors the
    // `RecordingRequester` in `fauna-client-events` / `-snapshots` / `-sync`
    // (transport-free, so it runs on every target including wasm); real
    // end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_account.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use account::*;
        let usage = || UsageBytes {
            used_bytes: 0,
            max_bytes: 0,
            extra: Default::default(),
        };
        match kind {
            "fauna.account.get" => fauna_protocol::encode_canonical(&AccountGetReply {
                actor_id: "ab".repeat(32),
                handle: Some("alice".into()),
                tier: "free".into(),
                created_at: 0,
                eviction: None,
                quota: AccountGetQuota {
                    inbox: usage(),
                    storage: usage(),
                    devices: AccountDeviceLimit {
                        max: 0,
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
            }),
            "fauna.quota.get" => fauna_protocol::encode_canonical(&QuotaGetReply {
                tier: "free".into(),
                inbox: usage(),
                storage: usage(),
                devices: QuotaDeviceUsage {
                    used: 0,
                    max: 0,
                    extra: Default::default(),
                },
                features: QuotaFeatures {
                    versioned_backup: false,
                    bridges: false,
                    max_feeds: 0,
                    extra: Default::default(),
                },
                extra: Default::default(),
            }),
            "fauna.account.am_i_admin" => fauna_protocol::encode_canonical(&AmIAdminReply {
                admin: false,
                extra: Default::default(),
            }),
            "fauna.profile.handle.change" => fauna_protocol::encode_canonical(&ChangeHandleReply {
                pending_action_id: 1,
                execute_after: 0,
                status: "pending".into(),
                new_handle: "bob".into(),
                extra: Default::default(),
            }),
            "fauna.account.upgrade" => fauna_protocol::encode_canonical(&UpgradeReply {
                ok: true,
                tier: "personal".into(),
                extra: Default::default(),
            }),
            "fauna.account.delete" => fauna_protocol::encode_canonical(&AccountDeleteReply {
                pending_action_id: 1,
                execute_after: 0,
                status: "pending".into(),
                message: "queued".into(),
                extra: Default::default(),
            }),
            "fauna.pending_actions.list" => fauna_protocol::encode_canonical(
                &fauna_protocol::pending_actions::PendingActionsListReply {
                    actions: vec![fauna_protocol::pending_actions::PendingActionSummary {
                        id: 7,
                        action_type: "handle.change".into(),
                        target: Some("bob".into()),
                        status: "pending".into(),
                        created_at: 0,
                        execute_after: 86_400,
                        requires_quorum: 0,
                        approvals: Vec::new(),
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                },
            ),
            "fauna.pending_actions.cancel" => fauna_protocol::encode_canonical(
                &fauna_protocol::pending_actions::PendingActionCancelReply {
                    ok: true,
                    extra: Default::default(),
                },
            ),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        AccountClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = AccountClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.get()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.account.get");
        let _req: account::AccountGetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn quota_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.quota_get()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.quota.get");
        let _req: account::QuotaGetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn am_i_admin_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.am_i_admin()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.account.am_i_admin");
        let _req: account::AmIAdminRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn change_handle_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.change_handle("newhandle")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.profile.handle.change");
        let req: account::ChangeHandleRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.handle, "newhandle");
    }

    #[test]
    fn upgrade_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.upgrade("personal", "INVITE2026")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.account.upgrade");
        let req: account::UpgradeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tier, "personal");
        assert_eq!(req.invite_code, "INVITE2026");
    }

    #[test]
    fn delete_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.delete()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.account.delete");
        let _req: account::AccountDeleteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn pending_actions_list_composes_kind_and_payload() {
        let (rec, c) = client();
        let reply = block_on(c.pending_actions_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pending_actions.list");
        let _req: fauna_protocol::pending_actions::PendingActionsListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(reply.actions.len(), 1, "the canned row round-trips");
        assert_eq!(reply.actions[0].action_type, "handle.change");
    }

    #[test]
    fn pending_action_cancel_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.pending_action_cancel(7)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pending_actions.cancel");
        let req: fauna_protocol::pending_actions::PendingActionCancelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 7);
    }
}
