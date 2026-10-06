//! Typed-call wrapper for the user-facing moderation WS-RPC kinds —
//! `fauna.moderation.{actions,train,…}`. `actions` reads the caller's
//! moderation queue (the obligation-action records: why their content was
//! labeled / quarantined / rejected) and `train` reports a per-post spam/ham
//! correction (the nest half of the training correction — read gate + report
//! capture; the model half is the client's sealed write).
//!
//! Nest handlers: `bins/fauna-nest/src/moderation_handlers.rs`.
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-search`, `-spam`, `-sync`) — a thin
//! `ModerationClient<R: RpcRequester>`, one async method per kind, no state
//! machine. Generic over the WS-RPC transport: native call sites pass
//! `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`. The remaining
//! moderation kinds (`stats`/`appeal`) join here as those surfaces migrate off
//! HTTP (priority #2). Written once, shared across native + wasm.

use fauna_protocol::RpcRequester;
use fauna_protocol::moderation::{
    AbuseReportMineReply, AbuseReportMineRequest, AbuseReportSubmitReply, AbuseReportSubmitRequest,
    AbuseReportWithdrawReply, AbuseReportWithdrawRequest,
};
use fauna_protocol::moderation::{
    ModerationActionsReply, ModerationActionsRequest, ModerationLegalTakedownReply,
    ModerationLegalTakedownRequest, ModerationReportShareSetReply, ModerationReportShareSetRequest,
    ModerationReportShareStatusReply, ModerationReportShareStatusRequest,
    ModerationSignalContributeReply, ModerationSignalContributeRequest,
    ModerationSignalShareSetReply, ModerationSignalShareSetRequest,
    ModerationSignalShareStatusReply, ModerationSignalShareStatusRequest, ModerationTrainReply,
    ModerationTrainRequest,
};
use fauna_protocol::moderation::{ModerationAppealReply, ModerationAppealRequest};

pub use fauna_protocol::moderation;

// UniFFI scaffolding — required once per crate when the `uniffi` feature derives
// `uniffi::Record`/`uniffi::Enum` on the moderation-queue reader types
// (`detections::{LocalDetection, QueueRow, QueueRowSource}`), so a native app's
// `ConversationsSession` reader can return them across the FFI boundary. Off in the
// Go mail-bridge's `--no-default-features` build (the feature is), so no scaffolding
// there — mirrors `fauna-core` / `fauna-conversations`.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

/// The shared client-side local-detection store + the moderation-queue union/dedupe
/// helper — the encrypted-mode social-content signal the server queue can't carry.
pub mod detections;
pub use detections::{
    DetectionLabel, LocalDetection, LocalDetectionStore, QueueRow, QueueRowSource, merge_queue,
};

/// The legal-takedown console's shared form/confirm/verdict decisions
/// (`moderation.md` § Legal takedown → *Invocation surface*).
pub mod takedown;
pub use takedown::{
    TakedownContentType, TakedownForm, TakedownFormView, takedown_form_view, takedown_verdict,
};

/// The moderation queue's shared appeal decisions — which rows offer an appeal,
/// the form's guard, and the verdict wording (`moderation.md` § Legal takedown,
/// the transparency triple's second leg).
pub mod appeal;
pub use appeal::{AppealForm, AppealFormView, appeal_form_view, appeal_verdict, row_is_appealable};

/// User-initiated reporting's shared sheet / acknowledgement / ledger / queue
/// decisions (`moderation.md` § User-initiated reporting → *Where logic lives*).
pub mod report;
pub use report::{
    LedgerRowView, ReportForm, ReportReasonOption, ReportSheetView, ReportTarget, ledger_row_view,
    report_acknowledgement, report_request, report_sheet_view,
};

/// Typed `fauna.moderation.*` call surface. Errors propagate as the transport's
/// `R::Error`; the namespaced `RpcError`s the handlers emit
/// (`fauna.moderation.{invalid_params,permission_denied,not_found}`) surface
/// through that error channel.
pub struct ModerationClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> ModerationClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.moderation.actions` — read the connection actor's **own**
    /// obligation-action records (the moderation queue: why each piece of the
    /// caller's content was labeled / quarantined / rejected). Empty request —
    /// the WS-RPC connection scopes to its caller (the HTTP twin's `?actor=`
    /// query is dropped). Pure read; `reply.actions` is the queue, one
    /// [`moderation::ObligationAction`] per row.
    pub async fn actions(&self) -> Result<ModerationActionsReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.actions",
                ModerationActionsRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.train` — report a spam/ham correction for one post
    /// (`content_id` = hex 32-byte post id): the nest half of a training
    /// correction — its read gate (an unreadable post is `not_found`) and the
    /// report capture. It trains nothing (the per-user model rests sealed; the
    /// model half is the client's sealed write —
    /// `MailSettingsMachine::train_moderation_correction` runs both). `verdict`
    /// must be `"spam"` or `"ham"` (else `fauna.moderation.invalid_params`).
    pub async fn train(
        &self,
        content_id: impl Into<String>,
        verdict: impl Into<String>,
    ) -> Result<ModerationTrainReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.train",
                ModerationTrainRequest {
                    content_id: content_id.into(),
                    verdict: verdict.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.legal_takedown` — the Admin-only legal-compulsion
    /// takedown / overturn (`moderation.md` § Legal takedown). `content_type`
    /// is `"post"` or `"conversation"` ([`takedown::TakedownContentType::wire`]);
    /// `legal_reference` is **required** by the nest when `restore == false`
    /// (the structural guard that makes this compulsion, not policy — render it
    /// via [`takedown_form_view`] so the refusal happens before dispatch);
    /// `restore == true` overturns, with the reference as the optional note.
    /// The reply's `status` echoes `"taken_down"` / `"restored"`.
    pub async fn legal_takedown(
        &self,
        content_id: impl Into<String>,
        content_type: impl Into<String>,
        legal_reference: impl Into<String>,
        restore: bool,
    ) -> Result<ModerationLegalTakedownReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.legal_takedown",
                ModerationLegalTakedownRequest {
                    content_id: content_id.into(),
                    content_type: content_type.into(),
                    legal_reference: legal_reference.into(),
                    restore,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.appeal` — appeal an enforcement decision recorded
    /// against `content_id` (a moderation-queue row's own id; for a
    /// conversation, the taken-down record's 32-byte hex id). The second leg of
    /// the transparency triple (`moderation.md` § Legal takedown): the nest
    /// writes the appeal to the permanent audit trail for an admin to review.
    ///
    /// `reason` is **required and bounded** (empty or over
    /// `MAX_APPEAL_REASON_BYTES` → `fauna.moderation.invalid_params`; render
    /// the guard through [`appeal_form_view`] so the refusal happens before
    /// dispatch), and the nest refuses a `content_id` it holds no enforcement
    /// record for with `fauna.moderation.not_found` — appeal only a row the
    /// queue actually carries ([`row_is_appealable`]) — and a post appeal from
    /// anyone but its author with `fauna.moderation.permission_denied`. The
    /// reply's `status` is `"appeal_recorded"`, or `"appeal_already_recorded"`
    /// when this caller's earlier appeal is still pending; recorded for
    /// review, not granted, either way.
    pub async fn appeal(
        &self,
        content_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ModerationAppealReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.appeal",
                ModerationAppealRequest {
                    content_id: content_id.into(),
                    reason: reason.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    // `scan_report` was removed 2026-07-19 (the client-side compliance-beacon
    // producer was retired without replacement — `moderation.md` § State & data
    // shape owns the verdict) and the `fauna.moderation.scan_report` kind
    // itself left the wire 2026-09-24 with the compat-remnant sweep.

    /// `fauna.moderation.report_share.set` — set the caller's distributed
    /// report-sharing opt-in (`report-sharing.md` § Client wire; default off).
    /// Caller-scoped — the subject is always the connection actor, so this sets
    /// only the caller's own preference. `share=false` also withdraws every
    /// report the caller contributed (nest-side opt-out sweep). `reply.share`
    /// echoes the new state.
    pub async fn report_share_set(
        &self,
        share: bool,
    ) -> Result<ModerationReportShareSetReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.report_share.set",
                ModerationReportShareSetRequest {
                    share,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.report_share.status` — read the caller's opt-in state
    /// **and** the transparency list: `reply.published` is exactly what this
    /// nest exports to peers (`report-sharing.md` § Client wire — the same ≥k
    /// `(content_hash, factor, count)` list a peer nest receives, every entry
    /// past the k-anonymity gate). Empty request; the connection scopes to its
    /// caller.
    pub async fn report_share_status(&self) -> Result<ModerationReportShareStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.report_share.status",
                ModerationReportShareStatusRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `report_share_set` then a confirming re-read, so the caller reflects
    /// the **persisted** value, never the local edit — tui and linux each
    /// hand-copied this exact round trip; same shape as
    /// `MailAccountClient::set_spam_threshold_override_and_reload`.
    pub async fn report_share_set_and_reload(
        &self,
        share: bool,
    ) -> Result<ModerationReportShareStatusReply, R::Error> {
        self.report_share_set(share).await?;
        self.report_share_status().await
    }

    /// `fauna.moderation.signal_share.set` — set the caller's Layer-B
    /// engagement-signal-sharing opt-in (`engagement-cues.md` § Layer B; default
    /// off, INDEPENDENT of report sharing). Caller-scoped. `share=false` also
    /// withdraws every `signal:*` contribution the caller made (nest-side
    /// factor-scoped opt-out sweep — never touches their `report:spam` rows).
    /// `reply.share` echoes the new state.
    pub async fn signal_share_set(
        &self,
        share: bool,
    ) -> Result<ModerationSignalShareSetReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.signal_share.set",
                ModerationSignalShareSetRequest {
                    share,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.signal_share.status` — read the caller's signal opt-in
    /// state **and** the transparency list. `reply.published` is the same export
    /// view `report_share.status` returns (byte-identical to what a peer nest
    /// receives — every ≥k `(content_hash, factor, count)` this nest publishes,
    /// `report:*` and `signal:*` alike). Empty request; caller-scoped `share`.
    pub async fn signal_share_status(&self) -> Result<ModerationSignalShareStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.signal_share.status",
                ModerationSignalShareStatusRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.signal_contribute` — contribute one derived cue verdict
    /// for a **public** post (`engagement-cues.md` § Layer B write path). Honored
    /// only when the caller opted in, except `signal = "withdraw"` which always
    /// applies (a retraction). `content_id` is the hex 32-byte post id; `signal`
    /// is `"watch-complete"`, `"skip"`, or `"withdraw"` (last-wins per item).
    pub async fn signal_contribute(
        &self,
        content_id: String,
        signal: String,
    ) -> Result<ModerationSignalContributeReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.signal_contribute",
                ModerationSignalContributeRequest {
                    content_id,
                    signal,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.moderation.abuse_report.submit` — file a report
    /// (`moderation.md` § User-initiated reporting). Build `request` with
    /// [`report::report_request`] so the bounds and the excerpt rule are the
    /// shared ones. A repeat on a subject the caller already has an open report
    /// on writes nothing and returns that report; past the per-hour cap the
    /// nest answers `fauna.moderation.rate_limited`. `reply.routed_to` feeds
    /// [`report::report_acknowledgement`]. `block_author` is recorded only —
    /// the caller chains `fauna.knocks.block` through its own contacts seam.
    pub async fn abuse_report_submit(
        &self,
        request: AbuseReportSubmitRequest,
    ) -> Result<AbuseReportSubmitReply, R::Error> {
        self.nest
            .request("fauna.moderation.abuse_report.submit", request)
            .await
    }

    /// `fauna.moderation.abuse_report.mine` — the caller's own reports, newest
    /// first (the Moderation page's ledger; word rows via
    /// [`report::ledger_row_view`]).
    pub async fn abuse_report_mine(&self) -> Result<AbuseReportMineReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.abuse_report.mine",
                AbuseReportMineRequest::default(),
            )
            .await
    }

    /// `fauna.moderation.abuse_report.withdraw` — withdraw one of the caller's
    /// open reports; its note and excerpt are deleted everywhere they went.
    pub async fn abuse_report_withdraw(
        &self,
        report_id: impl Into<String>,
    ) -> Result<AbuseReportWithdrawReply, R::Error> {
        self.nest
            .request(
                "fauna.moderation.abuse_report.withdraw",
                AbuseReportWithdrawRequest {
                    report_id: report_id.into(),
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
        let _c = ModerationClient::new(MockRequester);
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.moderation.actions" => {
                fauna_protocol::encode_canonical(&ModerationActionsReply {
                    actions: vec![fauna_protocol::moderation::ObligationAction {
                        id: 1,
                        content_type: "post".into(),
                        content_id: "ab".repeat(32),
                        category: "spam".into(),
                        confidence_per_mille: 880,
                        action: 2,
                        timestamp: 1_700_000_000_000,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                })
            }
            "fauna.moderation.train" => fauna_protocol::encode_canonical(&ModerationTrainReply {
                status: "ok".into(),
                verdict: "spam".into(),
                extra: Default::default(),
            }),
            "fauna.moderation.report_share.set" => {
                fauna_protocol::encode_canonical(&ModerationReportShareSetReply {
                    share: true,
                    extra: Default::default(),
                })
            }
            "fauna.moderation.report_share.status" => {
                fauna_protocol::encode_canonical(&ModerationReportShareStatusReply {
                    share: true,
                    published: vec![fauna_protocol::moderation::ReportShareEntry {
                        content_hash: "ab".repeat(32),
                        factor: "report:spam".into(),
                        count: 3,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                })
            }
            "fauna.moderation.signal_share.set" => {
                fauna_protocol::encode_canonical(&ModerationSignalShareSetReply {
                    share: true,
                    extra: Default::default(),
                })
            }
            "fauna.moderation.signal_share.status" => {
                fauna_protocol::encode_canonical(&ModerationSignalShareStatusReply {
                    share: true,
                    published: vec![fauna_protocol::moderation::ReportShareEntry {
                        content_hash: "cd".repeat(32),
                        factor: "signal:watch-complete".into(),
                        count: 4,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                })
            }
            "fauna.moderation.legal_takedown" => {
                fauna_protocol::encode_canonical(&ModerationLegalTakedownReply {
                    status: "taken_down".into(),
                    content_id: "ab".repeat(32),
                    extra: Default::default(),
                })
            }
            "fauna.moderation.signal_contribute" => {
                fauna_protocol::encode_canonical(&ModerationSignalContributeReply {
                    status: "recorded".into(),
                    signal: "watch-complete".into(),
                    extra: Default::default(),
                })
            }
            "fauna.moderation.abuse_report.submit" => {
                fauna_protocol::encode_canonical(&AbuseReportSubmitReply {
                    report_id: "0f".repeat(16),
                    routed_to: vec!["a.example".into(), "b.example".into()],
                    extra: Default::default(),
                })
            }
            "fauna.moderation.abuse_report.mine" => {
                fauna_protocol::encode_canonical(&AbuseReportMineReply::default())
            }
            "fauna.moderation.abuse_report.withdraw" => {
                fauna_protocol::encode_canonical(&AbuseReportWithdrawReply::default())
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn actions_composes_kind_and_empty_request() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.actions()).expect("infallible mock");
        // The reply carries the caller's queue rows.
        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.actions[0].category, "spam");
        assert_eq!(reply.actions[0].content_type, "post");
        assert_eq!(reply.actions[0].confidence_per_mille, 880);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.actions");
        // Empty request — the connection scopes to its caller (no actor param).
        let req: ModerationActionsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.extra.is_empty());
    }

    #[test]
    fn train_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        block_on(client.train("ab12", "ham")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.train");
        let req: ModerationTrainRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.content_id, "ab12");
        assert_eq!(req.verdict, "ham");
    }

    #[test]
    fn report_share_set_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.report_share_set(true)).expect("infallible mock");
        assert!(reply.share);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.report_share.set");
        let req: ModerationReportShareSetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.share);
    }

    #[test]
    fn report_share_status_composes_kind_and_reads_published() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.report_share_status()).expect("infallible mock");
        assert!(reply.share);
        // The transparency list surfaces the nest's ≥k export entries.
        assert_eq!(reply.published.len(), 1);
        assert_eq!(reply.published[0].factor, "report:spam");
        assert_eq!(reply.published[0].count, 3);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.report_share.status");
        // Empty request — the connection scopes to its caller.
        let req: ModerationReportShareStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.extra.is_empty());
    }

    #[test]
    fn signal_share_set_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.signal_share_set(true)).expect("infallible mock");
        assert!(reply.share);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.signal_share.set");
        let req: ModerationSignalShareSetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.share);
    }

    #[test]
    fn signal_share_status_composes_kind_and_reads_published() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.signal_share_status()).expect("infallible mock");
        assert!(reply.share);
        // The transparency list surfaces the nest's ≥k export entries (the shared
        // export view carries signal:* aggregates too).
        assert_eq!(reply.published.len(), 1);
        assert_eq!(reply.published[0].factor, "signal:watch-complete");
        assert_eq!(reply.published[0].count, 4);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.signal_share.status");
        let req: ModerationSignalShareStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.extra.is_empty());
    }

    #[test]
    fn legal_takedown_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply =
            block_on(client.legal_takedown("ab".repeat(32), "post", "Court order 42/2026", false))
                .expect("infallible mock");
        assert_eq!(reply.status, "taken_down");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.legal_takedown");
        let req: ModerationLegalTakedownRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.content_id, "ab".repeat(32));
        assert_eq!(req.content_type, "post");
        assert_eq!(req.legal_reference, "Court order 42/2026");
        assert!(!req.restore);
    }

    #[test]
    fn signal_contribute_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let reply = block_on(client.signal_contribute("ab12".into(), "skip".into()))
            .expect("infallible mock");
        assert_eq!(reply.status, "recorded");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.signal_contribute");
        let req: ModerationSignalContributeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.content_id, "ab12");
        assert_eq!(req.signal, "skip");
    }

    #[test]
    fn abuse_report_calls_compose_kind_and_payload() {
        use fauna_protocol::moderation::{AbuseReportReason, AbuseReportSubject};
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ModerationClient::new(rec.clone());
        let subject = AbuseReportSubject::Post {
            cid: "ab".repeat(32),
        };
        let form = ReportForm {
            reason: Some(AbuseReportReason::Spam),
            note: "note".into(),
            include_text: false,
            block_author: true,
        };
        let target = ReportTarget {
            subject: subject.clone(),
            sealed: false,
            author: Some("ef".repeat(32)),
            plaintext: None,
        };
        let request = report_request(&target, &form).expect("sendable");
        let reply = block_on(client.abuse_report_submit(request)).expect("infallible mock");
        assert_eq!(reply.routed_to.len(), 2);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.abuse_report.submit");
        let req: AbuseReportSubmitRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.subject, subject);
        assert_eq!(req.note.as_deref(), Some("note"));
        assert!(req.block_author);

        block_on(client.abuse_report_mine()).expect("infallible mock");
        assert_eq!(rec.recorded().0, "fauna.moderation.abuse_report.mine");

        block_on(client.abuse_report_withdraw("0f".repeat(16))).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.moderation.abuse_report.withdraw");
        let req: AbuseReportWithdrawRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.report_id, "0f".repeat(16));
    }
}
