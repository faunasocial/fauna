//! Typed-call wrapper for the **family-safety** WS-RPC kinds
//! (`fauna.family.*` — `docs/goal/behavior/family-safety.md` § Wire & data
//! shape). The shared home for the Family surface's call composition on all 6
//! apps (priority #2): the guardian side (wards, reach-policy editor,
//! approvals queue, graduate/transfer, contact pre-approval) and the
//! supervised side (the indicator's `status` read).
//!
//! Pattern: the same shape as [`fauna_client_admin::AdminClient`] — a thin
//! `pub struct FamilyClient<R: RpcRequester>`, one async method per kind, no
//! state machine. Native call sites pass `Arc<NestClient>`, the wasm SPA its
//! `WsRpcClient`. Errors propagate as the transport's `R::Error`.
//!
//! Authorization is nest-side and per-target (the guardianship link table);
//! this crate adds no client-side authority. End-to-end conformance against
//! the real router: `bins/fauna-nest/tests/conformance_family.rs`.

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::family::{
    FamilyApprovalDecideRequest, FamilyApprovalsListReply, FamilyApprovalsListRequest,
    FamilyContactAddRequest, FamilyContactRequestRequest, FamilyContentNotice,
    FamilyDeviceMarkRequest, FamilyFeedSourceRequestRequest, FamilyGraduateRequest,
    FamilyNotifyReportRequest, FamilyOkReply, FamilyPolicyUpdateRequest, FamilyStatusReply,
    FamilyStatusRequest, FamilyTransferAcceptRequest, FamilyTransferCancelRequest,
    FamilyTransferDeclineRequest, FamilyTransferRequest, FamilyUsageReportReply,
    FamilyUsageReportRequest, ReachPolicy,
};

// Re-exported so a client reads/builds the family wire shapes through this
// wrapper crate's typed surface without depending on `fauna-protocol`
// directly (mirrors `fauna-client-admin`).
pub use fauna_protocol::family;
pub use fauna_protocol::family::{
    FamilyApprovalEntry, FamilyContactRequestInfo, FamilyFeedRequestInfo, FamilyGuardianInfo,
    FamilyIncomingTransferInfo, FamilyPendingTransferInfo, FamilyWardInfo,
};

/// Typed `fauna.family.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`). One async method per nest kind; `{ ok }` replies are
/// discarded (failures surface as the namespaced `RpcError`).
pub struct FamilyClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> FamilyClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.family.status` — one caller-scoped read answering "what are my
    /// family relationships?" for both roles: `supervised_by` + `policy` when
    /// the caller is supervised (drives the supervised indicator), `wards`
    /// when the caller guards someone (drives the Family surface).
    pub async fn status(&self) -> Result<FamilyStatusReply, R::Error> {
        self.nest
            .request("fauna.family.status", FamilyStatusRequest::default())
            .await
    }

    /// `fauna.family.policy.update` — replace a ward's reach-policy document.
    /// Guardian-only nest-side.
    pub async fn policy_update(
        &self,
        supervised_actor_id: Vec<u8>,
        policy: ReachPolicy,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.policy.update",
                FamilyPolicyUpdateRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    policy,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.notify_report` — the **supervised** caller reports coarse
    /// per-category enforcement counts (`family-safety.md` § Guardian Notify).
    /// Each `entries` count is a delta the nest accumulates; carries no content
    /// ids. `utc_offset_minutes` is the device's UTC offset (nest-clamped to
    /// `-720..=840`) — the day-bucket rule (§ Screen time). A no-op nest-side
    /// unless the caller is supervised with the guardian's `content_notify` on.
    pub async fn notify_report(
        &self,
        entries: Vec<FamilyContentNotice>,
        utc_offset_minutes: i32,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.notify_report",
                FamilyNotifyReportRequest {
                    entries,
                    utc_offset_minutes,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.usage_report` — the **supervised** caller heartbeats
    /// coarse foreground minutes for the daily screen-time budget
    /// (`family-safety.md` § Screen time). `minutes` is the foreground delta
    /// since the last successful report (nest-clamped); `0` is a pure read.
    /// The reply carries the day's **cross-device** total — the number
    /// [`fauna_core::screen_time::ScreenTimePolicy::lock_state`] locks on.
    /// A silent zero-reply no-op nest-side unless the caller is supervised
    /// with a daily budget set.
    pub async fn usage_report(
        &self,
        minutes: u32,
        utc_offset_minutes: i32,
    ) -> Result<FamilyUsageReportReply, R::Error> {
        self.nest
            .request(
                "fauna.family.usage_report",
                FamilyUsageReportRequest {
                    minutes,
                    utc_offset_minutes,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.family.approvals.list` — the guardian's pending reach approvals
    /// across all wards (contact knocks in v1; mail holds join additively).
    pub async fn approvals_list(&self) -> Result<FamilyApprovalsListReply, R::Error> {
        self.nest
            .request(
                "fauna.family.approvals.list",
                FamilyApprovalsListRequest::default(),
            )
            .await
    }

    /// `fauna.family.approvals.decide` — approve or deny one pending item on
    /// the ward's behalf.
    ///
    /// **Each kind names its item with a different key** — pass that kind's key
    /// and leave the others empty; every field here is one of those keys, and an
    /// entry from [`Self::approvals_list`] carries all of them:
    ///
    /// | kind | key |
    /// |---|---|
    /// | `contact`, `contact_request` | `peer_actor_id` |
    /// | `mail_hold` | `message_id` — the message, never the address, so approving one held message never sweeps every message from that sender |
    /// | `feed_source` | the whole `(bridge_id, operation, target)` triple — `target` is empty for a `link`, and `label` is never part of the key |
    /// | `dm_hold` | `(bridge_id, peer_address)` — the external peer, which is not an actor on this nest and so cannot ride `peer_actor_id` |
    // Deliberately one method with every kind's key rather than one method per
    // kind: this mirrors the single `fauna.family.approvals.decide` request 1:1,
    // and each new kind then costs a parameter instead of a new method every
    // app must learn. Grouping the keys into a struct would need a
    // hand-written mirror in four binding languages (UniFFI record, JS object,
    // C#, Kotlin) to save one signature.
    #[allow(clippy::too_many_arguments)]
    pub async fn approvals_decide(
        &self,
        supervised_actor_id: Vec<u8>,
        kind: impl Into<String>,
        peer_actor_id: Vec<u8>,
        message_id: Vec<u8>,
        bridge_id: impl Into<String>,
        operation: impl Into<String>,
        target: impl Into<String>,
        peer_address: impl Into<String>,
        approve: bool,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.approvals.decide",
                FamilyApprovalDecideRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    kind: kind.into(),
                    peer_actor_id: ByteBuf::from(peer_actor_id),
                    message_id: ByteBuf::from(message_id),
                    bridge_id: bridge_id.into(),
                    operation: operation.into(),
                    target: target.into(),
                    peer_address: peer_address.into(),
                    approve,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// The guardian's **un-deny** of one bridge-DM peer — the one-click flip
    /// back for a row of `FamilyWardInfo::blocked_dm_peers`
    /// (`family-safety.md` § The bridge-DM gate → *The un-deny surface*).
    ///
    /// Just [`Self::approvals_decide`] for `kind: "dm_hold"` with `approve:
    /// true`, named once here so no app spells the wire kind or remembers that
    /// the external peer rides `peer_address` rather than `peer_actor_id`. Takes
    /// the denied row's own `(bridge_id, peer_id)`, so the call cannot address a
    /// different peer than the row the guardian read. Idempotent and not
    /// queue-scoped: it works long after the hold row is gone.
    pub async fn allow_blocked_dm_peer(
        &self,
        supervised_actor_id: Vec<u8>,
        bridge_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Result<(), R::Error> {
        self.approvals_decide(
            supervised_actor_id,
            "dm_hold",
            vec![],
            vec![],
            bridge_id,
            "",
            "",
            peer_id,
            true,
        )
        .await
    }

    /// `fauna.family.contact.add` — pre-approve a contact on the ward's
    /// behalf (the outbound complement of contact-approval mode).
    pub async fn contact_add(
        &self,
        supervised_actor_id: Vec<u8>,
        peer_actor_id: Vec<u8>,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.contact.add",
                FamilyContactAddRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    peer_actor_id: ByteBuf::from(peer_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.contact.request` — the **supervised** caller's in-app ask
    /// to contact a peer (`family-safety.md` § Child-initiated contact
    /// requests). Pending in the guardian's queue until decided; the caller's
    /// own pending asks ride `status().contact_requests`.
    pub async fn contact_request(&self, peer_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.contact.request",
                FamilyContactRequestRequest {
                    peer_actor_id: ByteBuf::from(peer_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.feed_source.request` — the **supervised** caller's in-app
    /// ask to add an external source their `feed_sources = "block"` policy just
    /// refused (`family-safety.md` § Feed-source approvals). Pending in the
    /// guardian's queue until decided; the caller's own asks ride
    /// `status().feed_requests` with their `pending | approved` state.
    ///
    /// Approving mints a **single-use grant**, it does not perform the
    /// operation: on the approved doorbell the client simply retries the
    /// original call, which now passes the gate once.
    ///
    /// `operation` must be one of `fauna_core::data::FeedSourceOperation`'s wire
    /// values — pass `op.as_str()` rather than a literal, so a client cannot mint
    /// an ask no gate could ever redeem. `target` is the follow id / feed URI,
    /// and **empty for `link`**; `label` is display-only.
    pub async fn feed_source_request(
        &self,
        bridge_id: String,
        operation: String,
        target: String,
        label: String,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.feed_source.request",
                FamilyFeedSourceRequestRequest {
                    bridge_id,
                    operation,
                    target,
                    label,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.graduate` — supervised → full account, in place.
    /// Guardian or admin nest-side.
    pub async fn graduate(&self, supervised_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.graduate",
                FamilyGraduateRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.device.mark` — set/clear the guardian-enrolled-device
    /// marker on one of the ward's devices (`family-safety.md` § Full
    /// visibility). Guardian-only nest-side (never the admin, never the ward).
    ///
    /// Marking makes the device un-removable by the ward and auto-revoked at
    /// graduation; clearing it is the guardian's un-enroll step, after which
    /// ordinary device removal proceeds.
    pub async fn device_mark(
        &self,
        supervised_actor_id: Vec<u8>,
        device_id: String,
        marked: bool,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.device.mark",
                FamilyDeviceMarkRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    device_id,
                    marked,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.transfer` — propose a new guardian for a ward. Pending
    /// until the proposed guardian accepts (`family-safety.md` § Graduation &
    /// transfer); a self-proposal completes immediately. Guardian or admin
    /// nest-side.
    pub async fn transfer(
        &self,
        supervised_actor_id: Vec<u8>,
        new_guardian_actor_id: Vec<u8>,
    ) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.transfer",
                FamilyTransferRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    new_guardian_actor_id: ByteBuf::from(new_guardian_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.transfer.accept` — consent to a proposal naming the
    /// caller as the ward's new guardian; completes the re-point.
    pub async fn transfer_accept(&self, supervised_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.transfer.accept",
                FamilyTransferAcceptRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.transfer.decline` — refuse a proposal naming the caller;
    /// the link stands.
    pub async fn transfer_decline(&self, supervised_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.transfer.decline",
                FamilyTransferDeclineRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.family.transfer.cancel` — withdraw the ward's pending proposal.
    /// Guardian or admin nest-side.
    pub async fn transfer_cancel(&self, supervised_actor_id: Vec<u8>) -> Result<(), R::Error> {
        let _: FamilyOkReply = self
            .nest
            .request(
                "fauna.family.transfer.cancel",
                FamilyTransferCancelRequest {
                    supervised_actor_id: ByteBuf::from(supervised_actor_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }
}

pub mod kids;
pub mod row_text;
pub mod supervision_snapshot;
pub mod ward_asks;

pub use kids::kids_app_eligible;

pub use supervision_snapshot::{SnapshotGuardian, SupervisionSnapshot};
pub use ward_asks::{FeedRequestState, contact_ask_pending, feed_request_state};

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailedRequest, FailingRequester, RecordingRequester, block_on};
    use std::sync::Arc;

    /// This crate's reply table for the shared [`RecordingRequester`]: one arm
    /// per kind, each the minimal valid shape its `Reply` decodes. Answering a
    /// canned reply is what proves each thin wrapper composes the correct kind
    /// string + payload struct (the realistic failure mode).
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.family.status" => {
                fauna_protocol::encode_canonical(&FamilyStatusReply::default()).unwrap()
            }
            "fauna.family.approvals.list" => {
                fauna_protocol::encode_canonical(&FamilyApprovalsListReply::default()).unwrap()
            }
            "fauna.family.usage_report" => {
                fauna_protocol::encode_canonical(&FamilyUsageReportReply {
                    day: 20_650,
                    day_total_minutes: 75,
                    ..Default::default()
                })
                .unwrap()
            }
            _ => fauna_protocol::encode_canonical(&FamilyOkReply {
                ok: true,
                ..Default::default()
            })
            .unwrap(),
        }
        .to_vec()
    }

    fn decode_last<T: serde::de::DeserializeOwned>(r: &RecordingRequester, expect_kind: &str) -> T {
        let (kind, bytes) = r.recorded();
        assert_eq!(kind, expect_kind);
        fauna_protocol::decode_strict(&bytes).expect("decode recorded payload")
    }

    #[test]
    fn wrappers_compose_kind_and_payload() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let c = FamilyClient::new(rec.clone());

        block_on(c.status()).unwrap();
        let _: FamilyStatusRequest = decode_last(&rec, "fauna.family.status");

        let ward = vec![2u8; 32];
        block_on(c.policy_update(
            ward.clone(),
            ReachPolicy {
                contact_approval: true,
                ..Default::default()
            },
        ))
        .unwrap();
        let p: FamilyPolicyUpdateRequest = decode_last(&rec, "fauna.family.policy.update");
        assert_eq!(p.supervised_actor_id.as_slice(), ward.as_slice());
        assert!(p.policy.contact_approval);

        block_on(c.approvals_list()).unwrap();
        let _: FamilyApprovalsListRequest = decode_last(&rec, "fauna.family.approvals.list");

        block_on(c.approvals_decide(
            ward.clone(),
            "contact",
            vec![7u8; 32],
            vec![],
            "",
            "",
            "",
            "",
            true,
        ))
        .unwrap();
        let d: FamilyApprovalDecideRequest = decode_last(&rec, "fauna.family.approvals.decide");
        assert_eq!(d.kind, "contact");
        assert_eq!(d.peer_actor_id.as_slice(), [7u8; 32]);
        assert!(d.message_id.is_empty(), "a contact carries no message id");
        assert!(d.approve);

        // A `mail_hold` names its item by message id instead — deciding one held
        // message must never sweep every message from that sender.
        block_on(c.approvals_decide(
            ward.clone(),
            "mail_hold",
            vec![],
            vec![9u8; 32],
            "",
            "",
            "",
            "",
            false,
        ))
        .unwrap();
        let m: FamilyApprovalDecideRequest = decode_last(&rec, "fauna.family.approvals.decide");
        assert_eq!(m.kind, "mail_hold");
        assert_eq!(m.message_id.as_slice(), [9u8; 32]);
        assert!(m.peer_actor_id.is_empty(), "a mail sender has no actor");

        // A `feed_source` names its item by the whole (bridge, operation,
        // target) triple — the key the redeeming gate matches on.
        block_on(c.approvals_decide(
            ward.clone(),
            "feed_source",
            vec![],
            vec![],
            "bluesky",
            "feed",
            "at://f",
            "",
            true,
        ))
        .unwrap();
        let f: FamilyApprovalDecideRequest = decode_last(&rec, "fauna.family.approvals.decide");
        assert_eq!(f.kind, "feed_source");
        assert_eq!(f.bridge_id, "bluesky");
        assert_eq!(f.operation, "feed");
        assert_eq!(f.target, "at://f");
        assert!(
            f.peer_actor_id.is_empty() && f.message_id.is_empty(),
            "a bridge object has neither an actor nor a message"
        );
        assert!(!m.approve);

        // A `dm_hold` names its item by `(bridge_id, peer_address)` — an external
        // DM peer is not an actor on this nest, which is exactly why it cannot
        // ride `peer_actor_id` (§ Reach approvals).
        block_on(c.approvals_decide(
            ward.clone(),
            "dm_hold",
            vec![],
            vec![],
            "nostr",
            "",
            "",
            "abc123",
            false,
        ))
        .unwrap();
        let dm: FamilyApprovalDecideRequest = decode_last(&rec, "fauna.family.approvals.decide");
        assert_eq!(dm.kind, "dm_hold");
        assert_eq!(dm.bridge_id, "nostr");
        assert_eq!(dm.peer_address, "abc123");
        assert!(
            dm.peer_actor_id.is_empty() && dm.message_id.is_empty(),
            "an external DM peer has neither an actor on this nest nor a message id"
        );
        assert!(!dm.approve, "deny writes the block verdict");

        // The un-deny (`family-safety.md` § The bridge-DM gate → *The un-deny
        // surface*) is that same `dm_hold` decide with `approve: true`, addressed
        // by the denied row's own `(bridge_id, peer_id)` — the pair the guardian
        // read off `blocked_dm_peers`, with `peer_id` riding `peer_address`.
        block_on(c.allow_blocked_dm_peer(ward.clone(), "nostr", "npub1denied")).unwrap();
        let allow: FamilyApprovalDecideRequest = decode_last(&rec, "fauna.family.approvals.decide");
        assert_eq!(allow.kind, "dm_hold");
        assert_eq!(allow.supervised_actor_id.as_slice(), ward.as_slice());
        assert_eq!(allow.bridge_id, "nostr");
        assert_eq!(allow.peer_address, "npub1denied");
        assert!(allow.approve, "un-deny writes the allow verdict");
        assert!(
            allow.peer_actor_id.is_empty() && allow.message_id.is_empty(),
            "an external DM peer has neither an actor on this nest nor a message id"
        );

        block_on(c.contact_add(ward.clone(), vec![7u8; 32])).unwrap();
        let a: FamilyContactAddRequest = decode_last(&rec, "fauna.family.contact.add");
        assert_eq!(a.peer_actor_id.as_slice(), [7u8; 32]);

        block_on(c.contact_request(vec![7u8; 32])).unwrap();
        let cr: FamilyContactRequestRequest = decode_last(&rec, "fauna.family.contact.request");
        assert_eq!(cr.peer_actor_id.as_slice(), [7u8; 32]);

        block_on(c.device_mark(ward.clone(), "device-1".into(), true)).unwrap();
        let dm1: FamilyDeviceMarkRequest = decode_last(&rec, "fauna.family.device.mark");
        assert_eq!(dm1.supervised_actor_id.as_slice(), ward.as_slice());
        assert_eq!(dm1.device_id, "device-1");
        assert!(dm1.marked, "marking a device sets marked=true");

        block_on(c.device_mark(ward.clone(), "device-1".into(), false)).unwrap();
        let dm2: FamilyDeviceMarkRequest = decode_last(&rec, "fauna.family.device.mark");
        assert!(
            !dm2.marked,
            "clearing a mark sets marked=false, not a distinct kind"
        );

        block_on(c.graduate(ward.clone())).unwrap();
        let g: FamilyGraduateRequest = decode_last(&rec, "fauna.family.graduate");
        assert_eq!(g.supervised_actor_id.as_slice(), ward.as_slice());

        block_on(c.transfer(ward.clone(), vec![9u8; 32])).unwrap();
        let t: FamilyTransferRequest = decode_last(&rec, "fauna.family.transfer");
        assert_eq!(t.new_guardian_actor_id.as_slice(), [9u8; 32]);

        block_on(c.transfer_accept(ward.clone())).unwrap();
        let t: FamilyTransferAcceptRequest = decode_last(&rec, "fauna.family.transfer.accept");
        assert_eq!(t.supervised_actor_id.as_slice(), ward.as_slice());

        block_on(c.transfer_decline(ward.clone())).unwrap();
        let t: FamilyTransferDeclineRequest = decode_last(&rec, "fauna.family.transfer.decline");
        assert_eq!(t.supervised_actor_id.as_slice(), ward.as_slice());

        block_on(c.transfer_cancel(ward.clone())).unwrap();
        let t: FamilyTransferCancelRequest = decode_last(&rec, "fauna.family.transfer.cancel");
        assert_eq!(t.supervised_actor_id.as_slice(), ward.as_slice());

        block_on(c.feed_source_request(
            "bluesky".into(),
            "follow".into(),
            "did:plc:x".into(),
            "Museum".into(),
        ))
        .unwrap();
        let fr: FamilyFeedSourceRequestRequest =
            decode_last(&rec, "fauna.family.feed_source.request");
        assert_eq!(fr.bridge_id, "bluesky");
        assert_eq!(fr.operation, "follow");
        assert_eq!(fr.target, "did:plc:x");
        assert_eq!(fr.label, "Museum");

        block_on(c.notify_report(vec![], -720)).unwrap();
        let n: FamilyNotifyReportRequest = decode_last(&rec, "fauna.family.notify_report");
        assert_eq!(n.utc_offset_minutes, -720);

        let u = block_on(c.usage_report(5, 120)).unwrap();
        let r: FamilyUsageReportRequest = decode_last(&rec, "fauna.family.usage_report");
        assert_eq!(r.minutes, 5);
        assert_eq!(r.utc_offset_minutes, 120);
        assert_eq!(u.day_total_minutes, 75, "the reply's total surfaces");
    }

    #[test]
    fn wrappers_propagate_transport_error_unchanged() {
        let c = FamilyClient::new(FailingRequester::new("transport unreachable"));
        let err = FailedRequest("transport unreachable".to_string());
        assert_eq!(block_on(c.status()).unwrap_err(), err);
        assert_eq!(block_on(c.contact_request(vec![7u8; 32])).unwrap_err(), err);
        assert_eq!(
            block_on(c.device_mark(vec![2u8; 32], "device-1".into(), true)).unwrap_err(),
            err
        );
        assert_eq!(block_on(c.usage_report(5, 120)).unwrap_err(), err);
    }
}
