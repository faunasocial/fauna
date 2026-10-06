//! Shared WS-RPC seam glue for the admin mail machines
//! ([`LocalDomainMachine`], [`BridgeApprovalMachine`], …).
//!
//! Each machine drives an `Arc<dyn …Nest>` seam; this module provides the one
//! seam impl every native app shares (priority #2/#4) — `fauna-ffi`
//! (windows/macos/ios/android) and linux both call the `build_*_machine`
//! constructors instead of hand-rolling `impl …Nest`. The reply→domain
//! projection + the `R::Error`→[`NestError`] map (via the shared
//! [`crate::error::nest_error`] / [`fauna_protocol::RpcErrorClass`]) are written
//! once here.
//!
//! **Why concrete over the transport (not generic over `R`):** the `…Nest`
//! traits are `#[async_trait]` (boxed `+ Send` futures, required by the
//! `Arc<dyn …Nest>` the machines hold), but `RpcRequester::request` is AFIT with
//! per-impl `Send` inference — its future is not provably `Send` in a generic
//! context, and RTN can't bound a method with generic type params. So a single
//! `impl<R> …Nest` is impossible; the seam binds the concrete transport per
//! target — native `Arc<NestClient>` in the `native` module, the browser
//! `WsRpcClient` in the `wasm` module. Behind the `rpc-glue` feature, which
//! pulls `fauna-client-bridges`'s `MailAdminClient` (+ the per-target transport)
//! — pure-machine consumers skip it.

use crate::error::{NestError, nest_error};
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// Attempts (initial + retries) for an idempotent mail provision/revoke RPC
/// against a transiently-unreachable nest.
const MAIL_PROVISION_MAX_ATTEMPTS: u32 = 4;

/// Issue an idempotent mail provision/revoke RPC, retrying on a transient
/// transport failure.
///
/// A mid-flight WS drop (`RpcDisconnected { was_in_flight: true }` natively, the
/// equivalent on wasm) surfaces — via the shared [`nest_error`] classifier — as
/// [`NestError::Transient`]. A single such drop used to abort the whole 4-call
/// `EnableMail` provision sequence with no retry, so against a remote (flaky)
/// nest the client reverted to "Mail is disabled" (status "Syncing… → Mail is
/// disabled") even though the same flow worked on loopback. Every mail
/// provision/revoke kind is idempotent on the nest (`INSERT OR REPLACE`,
/// `ON CONFLICT DO UPDATE`, or `DELETE` — see `bins/fauna-nest/src/db/`
/// `bridge_blobs.rs` and `bridge_routing.rs`), so re-issuing a call whose reply
/// was lost to a disconnect is a safe no-op.
///
/// A `Rejected` error (the request reached nest and was refused) is returned
/// immediately — it is not a transport fault.
///
/// `sleep` is the backoff timer, injected so this helper stays
/// transport-agnostic; the per-target split it used to name lives once now, in
/// `fauna-sleep`.
/// Generic over `R: RpcRequester` but only ever called from a *concrete* seam
/// method (`R` = `NestClient` / `WsRpcClient`), so the returned future's `Send`
/// is inferred per instantiation — sidestepping the blanket-`impl<R>` `Send`
/// problem documented above.
async fn request_idempotent<R, Req, Reply, Sleep, SleepFut>(
    nest: &R,
    kind: &'static str,
    payload: Req,
    sleep: Sleep,
) -> Result<Reply, NestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    Req: serde::Serialize + Clone,
    Reply: serde::de::DeserializeOwned,
    Sleep: Fn(u32) -> SleepFut,
    SleepFut: core::future::Future<Output = ()>,
{
    let mut attempt = 1u32;
    loop {
        match <R as RpcRequester>::request::<Req, Reply>(nest, kind, payload.clone()).await {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                let mapped = nest_error(e);
                if attempt < MAIL_PROVISION_MAX_ATTEMPTS
                    && matches!(mapped, NestError::Transient(_))
                {
                    // 200ms, 400ms, 800ms — give the reconnect supervisor time
                    // to re-establish the WS before re-issuing.
                    sleep(200u32 << (attempt - 1)).await;
                    attempt += 1;
                    continue;
                }
                return Err(mapped);
            }
        }
    }
}

/// The owner's custody's served state, as the mail-settings seam holds it —
/// what `serves_any_webdav_set` folds the rows through (ruling (7)(b)(ii)
/// rule (2)). `None` (a machine that never hydrates the URL row) reads not
/// serving, the best-effort contract's degrade; the nest's `webdav_enabled`
/// is never the answer.
pub type FolderKeys = Option<std::sync::Arc<dyn crate::machine::WebdavServedSets>>;

async fn custody_serves_any(
    folders: Vec<fauna_protocol::folders::FolderSummary>,
    keys: &FolderKeys,
) -> bool {
    match keys {
        Some(served) => served.serves_any(folders).await,
        None => false,
    }
}

/// `fauna.posts.get` → the post's trainable text (`Post::body_text`) — the body
/// read of the moderation-queue training correction's model half
/// ([`crate::machine::NestClient::fetch_post_body_text`]), written once for the
/// native + wasm seams. Generic over `R` but only called from a concrete seam
/// method (see [`request_idempotent`] for why).
async fn fetch_post_body_text<R>(nest: R, content_id: &str) -> Result<Option<String>, NestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply = fauna_client_posts::PostsClient::new(nest)
        .posts_get(content_id)
        .await
        .map_err(nest_error)?;
    Ok(fauna_core::data::Post::decode_resolved_bytes(&reply.body).map(|p| p.body_text()))
}

/// `fauna.moderation.train` — the nest half of the moderation-queue training
/// correction ([`crate::machine::NestClient::moderation_train`]): the read gate
/// + report capture, no model train. Written once for the native + wasm seams.
async fn moderation_train<R>(nest: R, content_id: &str, verdict: &str) -> Result<(), NestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    fauna_client_moderation::ModerationClient::new(nest)
        .train(content_id, verdict)
        .await
        .map(|_reply| ())
        .map_err(nest_error)
}

/// The honest `Rejected` an unbuilt-backend seam method returns. Shared by the
/// native + wasm `MailSpamNest` stubs (the per-user spam backend has no nest
/// handler yet — `mail-spam.md` § Implementation status today); the page renders
/// it via `error-message`, never fakes green.
fn unimplemented_rejection(rpc: &str, doc: &str) -> NestError {
    NestError::Rejected(format!("unimplemented: {rpc} (backend not built — {doc})"))
}

/// A sheet text field → its optional wire field: an empty (or whitespace-only)
/// input means "unset", not an empty string. Shared by the native + wasm
/// `MailListsNest` seams so a cleared `mail-lists-add-sheet-list-archive-url-input`
/// actually clears the List-Archive header rather than stamping an empty one
/// (`mail-mass-mailing.md` § Layout — the header is omitted when unset, never
/// fabricated). Round-trips with `lists::project_list_row`, which flattens the
/// same fields back to `String` for the edit sheet.
fn opt_text(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Project the `fauna.bridges.list_spam_training_history` wire reply into the
/// [`SpamHistory`](crate::spam::SpamHistory) the `mail-spam` page renders —
/// shared by the native + wasm [`MailSpamNest`](crate::spam::MailSpamNest) seams
/// (the wire→view mapping is written once, priority #2/#4). Maps each row's
/// `SpamLabel`/`TrainingSource` to the client enums (the wire `ManualOther` →
/// client `ExplicitButton`, the first-party button), keying on the *stored*
/// wire enums, and hex-encodes the 16-byte `history_id` via
/// [`SpamTrainingView::from_parts`](crate::spam::SpamTrainingView::from_parts).
fn project_spam_history(
    reply: fauna_protocol::bridge_routing::ListSpamTrainingHistoryReply,
) -> crate::spam::SpamHistory {
    use crate::spam::{SpamHistory, SpamTrainingView, TrainingLabel, TrainingSource};
    use fauna_protocol::bridge_routing::{SpamLabel, TrainingSource as WireSource};

    let events = reply
        .events
        .into_iter()
        .map(|r| {
            let label = match r.label {
                SpamLabel::Spam => TrainingLabel::Spam,
                SpamLabel::Ham => TrainingLabel::Ham,
                SpamLabel::Unknown => TrainingLabel::Unknown,
            };
            let source = match r.source {
                WireSource::ImapJunkFlag => TrainingSource::ImapJunkFlag,
                WireSource::ImapJunkMove => TrainingSource::ImapJunkMove,
                // The ratified first-party-button source (`mail-spam.md`
                // § Wire shapes, ruled 2026-07-08): the app's own Mark-as-spam
                // writes it, so it reads "Fauna app".
                WireSource::ManualOther => TrainingSource::ExplicitButton,
                WireSource::Unknown => TrainingSource::Unknown,
            };
            SpamTrainingView::from_parts(
                &r.history_id,
                r.message,
                label,
                source,
                r.created_at_ms,
                r.model_delta_applied,
                r.sealed_subject,
                r.mailbox,
            )
        })
        .collect();
    SpamHistory {
        events,
        contribute_baseline: reply.contribute_baseline,
    }
}

/// The retry backoff [`request_idempotent`] waits out between attempts.
///
/// ONE definition for both targets: this used to be two functions — a tokio one
/// in `mod native`, a gloo-timers one in `mod wasm` — which is the same
/// hand-written `#[cfg]` split six other crates carried. `fauna-sleep` owns that
/// split now, so the two bodies became identical and collapse to this.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
async fn provision_backoff(ms: u32) {
    fauna_sleep::sleep(std::time::Duration::from_millis(ms as u64)).await;
}

/// One [`fauna_mail::imap_client::ImportUnitReply`] → the domain
/// [`SendUnitReply`](crate::import::SendUnitReply) `run_import` folds into its
/// progress counters + § Per-message error budget. Shared by native + wasm
/// (`MailImportNest`'s real impl in each — the projection is wire→view, not
/// transport-specific, unlike the seam impls themselves).
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn project_import_outcome(
    o: fauna_protocol::bridge_routing::ImportMessageOutcome,
) -> crate::import::SendOutcome {
    use crate::import::SendOutcome;
    use fauna_protocol::bridge_routing::ImportMessageOutcome;
    match o {
        ImportMessageOutcome::Imported { .. } => SendOutcome::Imported,
        ImportMessageOutcome::Skipped { reason } => SendOutcome::Skipped { reason },
        ImportMessageOutcome::Errored { reason } => SendOutcome::Errored { reason },
        // An outcome a newer nest added: counted as errored, so it is logged and
        // spends the error budget rather than reading as imported or skipped.
        ImportMessageOutcome::Unknown => SendOutcome::Errored {
            reason: "unknown import outcome".into(),
        },
    }
}

#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn project_import_unit_reply(
    reply: fauna_mail::imap_client::ImportUnitReply,
) -> crate::import::SendUnitReply {
    use fauna_mail::imap_client::ImportUnitReply;
    let outcomes = match reply {
        ImportUnitReply::Single(r) => vec![project_import_outcome(r.outcome)],
        ImportUnitReply::Batch(r) => r.outcomes.into_iter().map(project_import_outcome).collect(),
    };
    crate::import::SendUnitReply { outcomes }
}

/// One [`fauna_protocol::bridge_routing::ImportSessionInfo`] →
/// [`ImportSessionView`](crate::import::ImportSessionView). `info.state`'s five
/// values (`bins/fauna-nest/src/bridge_import_handlers.rs`'s
/// `session_transition_handler` table) are exhaustive by the nest's own
/// contract; an unrecognized string (a future nest state this client predates)
/// degrades to `Errored` rather than silently rendering as still-running.
///
/// `keys` is the reader's owner custody, used to render `source_descriptor`
/// sealed-first. `None` (a keyless/bearer-only connection, or the wasm arm)
/// falls back to whatever plaintext the nest still holds.
///
/// Rendering matters even though the nest also sends the plaintext: once the
/// row rests sealed, the boot scrub blanks that plaintext, so a session that
/// survives a reboot has nothing but the seal left to label it with
/// (`mailbox-migration.md` § Resume protocol step 1).
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn project_import_session(
    info: fauna_protocol::bridge_routing::ImportSessionInfo,
    keys: Option<&fauna_core::file_download::FileDownloadKeys>,
) -> crate::import::ImportSessionView {
    use crate::import::{ImportSessionState, ImportSessionView};
    let state = match info.state.as_str() {
        "running" => ImportSessionState::Running,
        "paused" => ImportSessionState::Paused,
        "completed" => ImportSessionState::Completed,
        "cancelled" => ImportSessionState::Cancelled,
        _ => ImportSessionState::Errored,
    };
    let source_descriptor = match keys {
        Some(keys) => fauna_core::label_custody::render_import_source(
            keys,
            info.source_sealed.as_ref().map(|b| &b[..]),
            &info.source_descriptor,
            info.source_hash.as_ref().map(|b| &b[..]),
        )
        .text()
        .unwrap_or_default()
        .to_string(),
        None => info.source_descriptor,
    };
    ImportSessionView {
        session_id: info.session_id,
        state,
        source_descriptor,
        total_count: info.total_count,
        imported_count: info.imported_count,
        skipped_count: info.skipped_count,
        errored_count: info.errored_count,
        error_reason: info.error_reason,
        scope: info.scope,
        date_from: info.date_from,
    }
}

/// `fauna.bridges.start_import_session`'s reply carries only the freshly
/// server-minted `session_id` (`StartImportSessionReply`), not a full
/// `ImportSessionInfo` — unlike every other import RPC. A just-started session
/// is always `Running` with zero counts, so the view is assembled from what
/// the caller already knows plus the minted id.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn fresh_import_session(
    session_id: String,
    source_descriptor: String,
    total_count: u64,
    scope: Vec<String>,
    date_from: String,
) -> crate::import::ImportSessionView {
    crate::import::ImportSessionView {
        session_id,
        state: crate::import::ImportSessionState::Running,
        source_descriptor,
        total_count,
        imported_count: 0,
        skipped_count: 0,
        errored_count: 0,
        error_reason: String::new(),
        scope,
        date_from,
    }
}

/// The honest `Rejected` the wasm `ImportSourceNest` stub returns — the web
/// IMAP transport (sans-io `rustls` over a blind byte relay) is user-gated +
/// POSTPONED (`mailbox-migration.md` § Where the IMAP client runs), same
/// relay-host blocker as Track C's web IMAP leg.
/// The nest half ([`MailImportNest`](crate::import::MailImportNest)) is real
/// on wasm too — only the foreign-source connection needs the relay.
#[allow(dead_code)] // Only the wasm module calls this.
fn import_source_unimpl(rpc: &str) -> NestError {
    unimplemented_rejection(
        rpc,
        "mailbox-migration.md § Where the IMAP client runs — web IMAP transport POSTPONED",
    )
}

/// The wizard's scope as the `scope_descriptor` blob `start_export_session`
/// carries — DAG-CBOR, **opaque to the nest**, which stores it verbatim and
/// hands it back (`mail-export.md` § Session row model). Only a client ever
/// reads it, so its shape can evolve without a wire change. Shared by the
/// native + wasm seams so both write one encoding.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn encode_export_scope(scope: &crate::export::ExportScope) -> Result<Vec<u8>, NestError> {
    fauna_protocol::encode_canonical(scope)
        .map(|b| b.to_vec())
        .map_err(|e| NestError::Rejected(format!("encode export scope: {e}")))
}

/// [`encode_export_scope`]'s inverse, for the `scope_descriptor` a session row
/// hands back. `None` when the bytes are absent or not a scope this client can
/// read — a cold resume refuses such a row rather than guess what it exported
/// (`mail-export.md` § Resume).
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn decode_export_scope(bytes: &[u8]) -> Option<crate::export::ExportScope> {
    fauna_protocol::decode_strict(bytes).ok()
}

/// Classify a failure of a generation-carrying export call: the typed
/// `export_stream_superseded` refusal becomes the machine's
/// [`ExportSeamError::Superseded`](crate::export::ExportSeamError::Superseded),
/// everything else keeps the seam's two classes. Keyed on the wire CODE, never
/// on message text (`mail-export.md` § Resume).
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn export_seam_error<E: fauna_protocol::RpcErrorClass + core::fmt::Display>(
    e: E,
) -> crate::export::ExportSeamError {
    use crate::export::ExportSeamError;
    let superseded = e
        .as_rpc_error()
        .is_some_and(|err| err.code == fauna_protocol::bridge_routing::EXPORT_STREAM_SUPERSEDED);
    if superseded {
        ExportSeamError::Superseded
    } else {
        ExportSeamError::Nest(crate::error::nest_error(e))
    }
}

/// Project one `export_sessions` wire row into the view the machine renders —
/// shared by the native + wasm seams (the wire→view mapping is written once).
/// `None` for a row whose format this client does not know: such a session's
/// frames bind a format token it cannot name, so it could neither drive nor
/// open it, and rendering it would offer controls that cannot work.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn project_export_session(
    info: fauna_protocol::bridge_routing::ExportSessionInfo,
) -> Option<crate::export::ExportSessionView> {
    use crate::export::{ExportFormat, ExportSessionState, ExportSessionView};
    let format = ExportFormat::from_wire_name(&info.format)?;
    let state = match info.state.as_str() {
        "running" => ExportSessionState::Running,
        "paused" => ExportSessionState::Paused,
        "completed" => ExportSessionState::Completed,
        "cancelled" => ExportSessionState::Cancelled,
        _ => ExportSessionState::Errored,
    };
    let to_u32 = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    let scope = info
        .scope_descriptor
        .as_deref()
        .and_then(|b| decode_export_scope(b));
    Some(ExportSessionView {
        session_id: info.session_id,
        state,
        format,
        exported_count: to_u32(info.exported_count),
        skipped_count: to_u32(info.skipped_count),
        errored_count: to_u32(info.errored_count),
        total_count: to_u32(info.total_count),
        error_reason: info.error_reason,
        // "Populated at completion" — a running session's byte count is the
        // partial blob's, which the Done summary must never show as the size.
        blob_bytes: (state == ExportSessionState::Completed).then_some(info.blob_bytes),
        download_url: info.download_url,
        // Carried through rather than dropped: the download leg unwraps it, and
        // the client that downloads is not necessarily the one that exported
        // (§ Download flow). The nest stores it verbatim for that reason.
        wrapped_session_key: info
            .wrapped_session_key
            .map(|b| b.into_vec())
            .unwrap_or_default(),
        stream_generation: info.stream_generation,
        scope,
    })
}

/// [`project_export_session`] for a reply to an action on a session this
/// client started — its format is one this client wrote, so an unknown one is
/// a protocol fault, not a row to hide.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn project_export_session_required(
    info: fauna_protocol::bridge_routing::ExportSessionInfo,
) -> Result<crate::export::ExportSessionView, NestError> {
    let format = info.format.clone();
    project_export_session(info)
        .ok_or_else(|| NestError::Rejected(format!("export session has unknown format {format:?}")))
}

/// `start_export_session`'s reply carries only the minted `session_id`, so the
/// view is assembled from what the caller already knows — the import twin's
/// [`fresh_import_session`] shape.
#[allow(dead_code)] // Each submodule imports it; neither target uses both paths.
fn fresh_export_session(
    session_id: String,
    format: crate::export::ExportFormat,
    total_count: u64,
    scope: crate::export::ExportScope,
) -> crate::export::ExportSessionView {
    crate::export::ExportSessionView {
        session_id,
        state: crate::export::ExportSessionState::Running,
        format,
        exported_count: 0,
        skipped_count: 0,
        errored_count: 0,
        total_count: u32::try_from(total_count).unwrap_or(u32::MAX),
        error_reason: String::new(),
        blob_bytes: None,
        download_url: String::new(),
        // The caller minted and wrapped it a moment ago; the machine holds it
        // for the run and re-reads the row's copy at download time, so the
        // freshly-assembled view does not need to carry it.
        wrapped_session_key: Vec::new(),
        // A freshly started session is stream generation 0 by definition.
        stream_generation: 0,
        scope: Some(scope),
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::Arc;

    use super::provision_backoff;

    use async_trait::async_trait;
    use ed25519_dalek::SigningKey;
    use fauna_client::NestClient;
    use fauna_client_bridges::{HolderInfo, MailAccountClient, MailAdminClient, discover_holders};
    use fauna_client_config::SuccessionLedgerStore;
    use fauna_client_pair::{
        LinkedNestsMachine, PairDispatchError, PairNestError, PostLinkHook,
        build_linked_nests_machine_with_hook, build_linked_nests_machine_with_hook_and_trust,
    };
    use fauna_core::identity::ActorKeypair;
    use fauna_mail::imap_client;
    use fauna_mls::wrapped_blob::{
        MlsSnapshotBlob, SubmissionToken, WrappedMsekBlob, WrappedSubmissionTokenBlob,
    };
    use fauna_protocol::ByteBuf;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::bridge_routing::{
        AddListMemberReply, AddListMemberRequest, AliasControls, AliasPolicy, AliasRow,
        BatchImportListMembersReply, BatchImportListMembersRequest, CreateAccountListReply,
        CreateAccountListRequest, DeleteAccountListReply, DeleteAccountListRequest, EpochSealKey,
        FetchConfigReply, GetSpamBaselineStateReply, ImportAliasOutcome, ListAccountAliasesReply,
        ListAccountAliasesRequest, ListAccountListsReply, ListAccountListsRequest,
        ListListMembersReply, ListListMembersRequest, MailDomainRenameRow, MailDomainRow,
        MailHealthReply, ProvisionRecipientMlsPubkeyRequest, PublishSpamBaselineReply,
        PutAliasPolicyRequest, PutAuthPolicyRequest, PutImapPolicyRequest,
        PutOutboundPolicyRequest, PutSpamPolicyRequest, PutSubmissionPolicyRequest,
        ResubscribeListMemberReply, ResubscribeListMemberRequest, SpamHistoryOp,
        UnsubscribeListMemberReply, UnsubscribeListMemberRequest, UpdateAccountListReply,
        UpdateAccountListRequest,
    };
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest, capability};
    use fauna_protocol::folders::{FoldersListReply, FoldersListRequest, KIND_FOLDERS_LIST};
    use fauna_protocol::wrapped_blob::{
        GetCaldavPortReply, GetCaldavPortRequest, GetMailServingEnabledReply,
        GetMailServingEnabledRequest, ProvisionMlsSnapshotBlobRequest, ProvisionReply,
        ProvisionWrappedMlsBlobRequest, ProvisionWrappedSubmissionTokenRequest,
        PutSpamModelOutcome, RevokeReply, RevokeWrappedMlsBlobRequest,
        RevokeWrappedSubmissionTokenRequest, ServiceUserInfo, SetMailEnabledReply,
        SetMailEnabledRequest, SetMailServingEnabledReply, SetMailServingEnabledRequest,
        SpamModelHolderCopy,
    };
    use tokio::sync::Mutex as AsyncMutex;

    use crate::admin_policy::{MailPolicyMachine, MailPolicyNest};
    use crate::aliases::{MailAliasesMachine, MailAliasesNest};
    use crate::bridge_approval::{BridgeApprovalAction, BridgeApprovalMachine, BridgeApprovalNest};
    use crate::caldav_policy::{CaldavPolicyMachine, CaldavPolicyNest};
    use crate::carddav_policy::{CarddavPolicyMachine, CarddavPolicyNest};
    use crate::error::DispatchError;
    use crate::error::{NestError, SignerError, nest_error};
    use crate::export::{
        ArchiveFileSink, ExportArchiveDelivery, ExportChunkProgress, ExportFetchPage, ExportFormat,
        ExportMailboxCount, ExportRecord, ExportScope, ExportSeamError, ExportSessionView,
        ExportUploadAck, MailExportKeyCustody, MailExportMachine, MailExportNest, SealedBlobStream,
    };
    use crate::forwarders::{ForwarderMachine, ForwarderNest};
    use crate::import::{
        ImportSourceNest, ImportTlsMode, MailImportMachine, MailImportNest, MailboxCursor,
        RetryClock, SourceConnectParams, SourceMailboxView,
    };
    use crate::lists::{
        ImportResult, ListDraft, ListMembers, ListView, MailListMembersMachine,
        MailListMembersNest, MailListsMachine, MailListsNest,
    };
    use crate::local_domains::{
        DomainDmarcPolicy, LocalDomainMachine, LocalDomainNest, RoleAddressKind,
    };
    use crate::machine::{
        FetchedSpamModel, IdentitySigner, MailSettingsMachine, MailStore,
        NestClient as MailNestClient, SealedModelWriter,
    };
    use crate::spam::{MailSpamMachine, MailSpamNest, SpamHistory};
    use crate::state::MuaInstructions;
    use crate::webdav_policy::{WebdavPolicyMachine, WebdavPolicyNest};
    use fauna_client_capabilities::rpc::CapabilitiesClient;
    use fauna_core::grant_event::GrantEvent;

    // ── LocalDomainNest (admin email-domains) ───────────────────────

    struct RpcLocalDomainNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl LocalDomainNest for RpcLocalDomainNest {
        async fn list_local_domains(
            &self,
        ) -> Result<(Vec<MailDomainRow>, Vec<MailDomainRow>), NestError> {
            let reply = self.admin.list_local_domains().await.map_err(nest_error)?;
            Ok((reply.active, reply.soft_deleted_within_30d))
        }

        async fn add_local_domain(
            &self,
            domain: String,
            mta_sts_cert_mode: String,
        ) -> Result<(MailDomainRow, bool), NestError> {
            let reply = self
                .admin
                .add_local_domain(domain, mta_sts_cert_mode, None, None)
                .await
                .map_err(nest_error)?;
            Ok((reply.domain, reply.skipped))
        }

        async fn remove_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .remove_local_domain(domain)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn restore_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .restore_local_domain(domain)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn update_local_domain_config(
            &self,
            domain: String,
            mta_sts_max_age_seconds: Option<i64>,
            mta_sts_cert_mode: Option<String>,
            spf_record: Option<String>,
            dmarc_policy: Option<DomainDmarcPolicy>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .update_local_domain_config(
                    domain,
                    mta_sts_max_age_seconds,
                    mta_sts_cert_mode,
                    spf_record,
                    dmarc_policy.map(Into::into),
                )
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn set_catch_all_actor(
            &self,
            domain: String,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .set_catch_all_actor(domain, actor_id)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn set_role_address(
            &self,
            domain: String,
            role: RoleAddressKind,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .set_role_address(domain, role.into(), actor_id)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn get_primary_domain_rename_status(
            &self,
        ) -> Result<Option<MailDomainRenameRow>, NestError> {
            Ok(self
                .admin
                .get_primary_domain_rename_status()
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn start_primary_domain_rename(
            &self,
            new_primary_domain_id: Vec<u8>,
            grace_days: Option<i64>,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .start_primary_domain_rename(new_primary_domain_id, grace_days)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn complete_primary_domain_rename(
            &self,
            rename_id: Vec<u8>,
            force: bool,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .complete_primary_domain_rename(rename_id, force)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn extend_primary_domain_rename_grace(
            &self,
            rename_id: Vec<u8>,
            additional_days: i64,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .extend_primary_domain_rename_grace(rename_id, additional_days)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn abort_primary_domain_rename(
            &self,
            rename_id: Vec<u8>,
            reason: Option<String>,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .abort_primary_domain_rename(rename_id, reason)
                .await
                .map_err(nest_error)?
                .rename)
        }
    }

    /// Build a [`LocalDomainMachine`] over a native WS-RPC handle (admin
    /// email-domains list). Replaces linux's `LinuxLocalDomainNest`.
    pub fn build_local_domains_machine(nest: Arc<NestClient>) -> LocalDomainMachine {
        LocalDomainMachine::new(Arc::new(RpcLocalDomainNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── BridgeApprovalNest (admin-bridges-pending) ──────────────────

    struct RpcBridgeApprovalNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl BridgeApprovalNest for RpcBridgeApprovalNest {
        async fn list_pending_bridges(&self) -> Result<Vec<ServiceUserInfo>, NestError> {
            Ok(self
                .admin
                .list_pending_bridges()
                .await
                .map_err(nest_error)?
                .bridges)
        }

        async fn list_service_users(
            &self,
            role: Option<String>,
            status: Option<String>,
        ) -> Result<Vec<ServiceUserInfo>, NestError> {
            Ok(self
                .admin
                .list_service_users(role, status)
                .await
                .map_err(nest_error)?
                .service_users)
        }

        async fn approve_pending_bridge(
            &self,
            ed25519_pubkey: Vec<u8>,
            role: String,
        ) -> Result<(), NestError> {
            self.admin
                .approve_pending_bridge(ed25519_pubkey, role)
                .await
                .map_err(nest_error)
        }

        async fn reject_pending_bridge(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .reject_pending_bridge(ed25519_pubkey)
                .await
                .map_err(nest_error)
        }

        async fn revoke_service_user(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .revoke_service_user(ed25519_pubkey)
                .await
                .map_err(nest_error)
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_mail_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_caldav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_carddav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_webdav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`BridgeApprovalMachine`] over a native WS-RPC handle
    /// (`admin-bridges-pending`). Replaces linux's `LinuxBridgeApprovalNest`.
    pub fn build_bridge_approval_machine(nest: Arc<NestClient>) -> BridgeApprovalMachine {
        BridgeApprovalMachine::new(Arc::new(RpcBridgeApprovalNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    /// Dispatch a bridge-approval action fire-and-forget, logging only on
    /// failure. The shape every native app's deployment-toggle glue shares —
    /// `set_{caldav,carddav,webdav}_enabled` each spawned this exact
    /// build-machine → dispatch → log-on-error sequence independently before
    /// this lift, differing only in which [`BridgeApprovalAction`] variant they
    /// dispatched.
    pub async fn dispatch_bridge_approval(nest: Arc<NestClient>, action: BridgeApprovalAction) {
        let machine = build_bridge_approval_machine(nest);
        let label = format!("{action:?}");
        if let Err(e) = machine.dispatch(action).await {
            tracing::error!("{label}: {e:?}");
        }
    }

    // ── ForwarderNest (admin-aliases external forwarders) ───────────

    struct RpcForwarderNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl ForwarderNest for RpcForwarderNest {
        async fn list_forwarders(&self) -> Result<Vec<AliasRow>, NestError> {
            Ok(self
                .admin
                .list_forwarders()
                .await
                .map_err(nest_error)?
                .forwarders)
        }

        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            Ok(self
                .admin
                .list_local_domains()
                .await
                .map_err(nest_error)?
                .active
                .into_iter()
                .map(|d| d.domain_name)
                .collect())
        }

        async fn create_forwarder(
            &self,
            local_domain: String,
            pattern: String,
            forward_target: String,
        ) -> Result<(), NestError> {
            self.admin
                .create_forwarder(local_domain, pattern, forward_target)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn delete_forwarder(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .delete_forwarder(alias_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }
    }

    /// Build a [`ForwarderMachine`] over a native WS-RPC handle (`admin-aliases`
    /// external forwarders, `admin.md` § 4).
    pub fn build_forwarders_machine(nest: Arc<NestClient>) -> ForwarderMachine {
        ForwarderMachine::new(Arc::new(RpcForwarderNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── MailPolicyNest (admin-mail policy form) ─────────────────────

    struct RpcMailPolicyNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl MailPolicyNest for RpcMailPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_mail_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn get_auto_enable_mail_for_new_users(&self) -> Result<bool, NestError> {
            self.admin
                .get_auto_enable_mail_for_new_users()
                .await
                .map_err(nest_error)
        }

        async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_auto_enable_mail_for_new_users(enabled)
                .await
                .map_err(nest_error)
        }

        async fn put_spam_policy(&self, req: PutSpamPolicyRequest) -> Result<(), NestError> {
            self.admin.put_spam_policy(req).await.map_err(nest_error)
        }

        async fn put_auth_policy(&self, req: PutAuthPolicyRequest) -> Result<(), NestError> {
            self.admin.put_auth_policy(req).await.map_err(nest_error)
        }

        async fn put_submission_policy(
            &self,
            req: PutSubmissionPolicyRequest,
        ) -> Result<(), NestError> {
            self.admin
                .put_submission_policy(req)
                .await
                .map_err(nest_error)
        }

        async fn put_imap_policy(&self, req: PutImapPolicyRequest) -> Result<(), NestError> {
            self.admin.put_imap_policy(req).await.map_err(nest_error)
        }

        async fn put_outbound_policy(
            &self,
            req: PutOutboundPolicyRequest,
        ) -> Result<(), NestError> {
            self.admin
                .put_outbound_policy(req)
                .await
                .map_err(nest_error)
        }

        async fn get_alias_policy(&self) -> Result<AliasPolicy, NestError> {
            self.admin.get_alias_policy().await.map_err(nest_error)
        }

        async fn put_alias_policy(&self, req: PutAliasPolicyRequest) -> Result<(), NestError> {
            self.admin.put_alias_policy(req).await.map_err(nest_error)
        }

        async fn get_spam_baseline_state(&self) -> Result<GetSpamBaselineStateReply, NestError> {
            self.admin
                .get_spam_baseline_state()
                .await
                .map_err(nest_error)
        }

        async fn publish_spam_baseline(&self) -> Result<PublishSpamBaselineReply, NestError> {
            self.admin.publish_spam_baseline().await.map_err(nest_error)
        }

        async fn mail_health(&self) -> Result<MailHealthReply, NestError> {
            self.admin.mail_health().await.map_err(nest_error)
        }

        async fn blocklist_self_check_run(&self) -> Result<(), NestError> {
            self.admin
                .blocklist_self_check_run()
                .await
                .map(drop)
                .map_err(nest_error)
        }

        async fn run_deliverability_diagnostics(&self) -> Result<(), NestError> {
            self.admin
                .run_deliverability_diagnostics()
                .await
                .map(drop)
                .map_err(nest_error)
        }

        async fn outbound_warmup_reset(&self) -> Result<(), NestError> {
            self.admin
                .outbound_warmup_reset()
                .await
                .map(drop)
                .map_err(nest_error)
        }
    }

    /// Build a [`MailPolicyMachine`] over a native WS-RPC handle (the flat
    /// `admin-mail` policy form, `admin.md` § Mail).
    pub fn build_mail_policy_machine(nest: Arc<NestClient>) -> MailPolicyMachine {
        MailPolicyMachine::new(Arc::new(RpcMailPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── CaldavPolicyNest (admin-calendar enable toggle) ─────────────

    struct RpcCaldavPolicyNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl CaldavPolicyNest for RpcCaldavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_caldav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_caldav_port(&self, port: u16) -> Result<(), NestError> {
            self.admin.set_caldav_port(port).await.map_err(nest_error)
        }
    }

    /// Build a [`CaldavPolicyMachine`] over a native WS-RPC handle (the flat
    /// `admin-calendar` CalDAV-enable toggle, `admin.md` § 8 Calendar).
    pub fn build_caldav_policy_machine(nest: Arc<NestClient>) -> CaldavPolicyMachine {
        CaldavPolicyMachine::new(Arc::new(RpcCaldavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── CarddavPolicyNest (admin-contacts enable toggle) ────────────

    struct RpcCarddavPolicyNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl CarddavPolicyNest for RpcCarddavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_carddav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`CarddavPolicyMachine`] over a native WS-RPC handle (the flat
    /// `admin-contacts` CardDAV-enable toggle, `admin.md` § Contacts).
    pub fn build_carddav_policy_machine(nest: Arc<NestClient>) -> CarddavPolicyMachine {
        CarddavPolicyMachine::new(Arc::new(RpcCarddavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── WebdavPolicyNest (admin-files enable toggle) ────────────────

    struct RpcWebdavPolicyNest {
        admin: MailAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl WebdavPolicyNest for RpcWebdavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_webdav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`WebdavPolicyMachine`] over a native WS-RPC handle (the flat
    /// `admin-files` WebDAV-enable toggle, `admin.md` § Files).
    pub fn build_webdav_policy_machine(nest: Arc<NestClient>) -> WebdavPolicyMachine {
        WebdavPolicyMachine::new(Arc::new(RpcWebdavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── MailAliasesNest (user-tier mail-aliases page) ───────────────

    struct RpcMailAliasesNest {
        account: MailAccountClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl MailAliasesNest for RpcMailAliasesNest {
        async fn list_account_aliases(&self) -> Result<Vec<AliasRow>, NestError> {
            self.account
                .list_account_aliases()
                .await
                .map_err(nest_error)
        }

        async fn create_account_alias(
            &self,
            kind: String,
            local_domain: String,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            self.account
                .create_account_alias(kind, local_domain, pattern, controls)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn update_account_alias(
            &self,
            alias_id: Vec<u8>,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            self.account
                .update_account_alias(alias_id, pattern, controls)
                .await
                .map_err(nest_error)
        }

        async fn revoke_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .revoke_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn enable_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .enable_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn delete_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .delete_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn generate_disposable_alias(
            &self,
            ttl_days: Option<u32>,
            uses: Option<u32>,
            label: String,
        ) -> Result<String, NestError> {
            Ok(self
                .account
                .generate_disposable_alias(ttl_days, uses, label)
                .await
                .map_err(nest_error)?
                .full_address)
        }

        async fn import_account_aliases(
            &self,
            lines: Vec<String>,
        ) -> Result<Vec<ImportAliasOutcome>, NestError> {
            self.account
                .import_account_aliases(lines)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`MailAliasesMachine`] over a native WS-RPC handle (the user
    /// `mail-aliases` page, `mail-aliases.md` § Aliases UX).
    pub fn build_mail_aliases_machine(nest: Arc<NestClient>) -> MailAliasesMachine {
        MailAliasesMachine::new(Arc::new(RpcMailAliasesNest {
            account: MailAccountClient::new(nest),
        }))
    }

    // ── MailSpamNest (user-tier mail-spam page) ─────────────────────
    //
    // The per-user Bayesian spam RPCs (`list_spam_training_history`,
    // `reset_spam_model`, `set_baseline_contribution`) are
    // live (`mail-spam.md` § Wire shapes; nest handlers in
    // `bins/fauna-nest/src/bridge_imap_handlers.rs`). All three are caller-scoped
    // (`User`-tier — the nest derives the subject from the authenticated
    // connection), so this seam wraps the same user-tier `MailAccountClient` the
    // aliases seam uses (priority #2/#4). The `SpamTrainingHistoryRow` →
    // `SpamTrainingView` wire→view projection is the shared
    // `super::project_spam_history`.

    struct RpcMailSpamNest {
        account: MailAccountClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl MailSpamNest for RpcMailSpamNest {
        async fn list_spam_training_history(&self) -> Result<SpamHistory, NestError> {
            let reply = self
                .account
                .list_spam_training_history()
                .await
                .map_err(nest_error)?;
            Ok(super::project_spam_history(reply))
        }
        async fn reset_spam_model(&self) -> Result<(), NestError> {
            self.account.reset_spam_model().await.map_err(nest_error)
        }
        async fn set_baseline_contribution(&self, contribute: bool) -> Result<(), NestError> {
            self.account
                .set_baseline_contribution(contribute)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`MailSpamMachine`] over a native WS-RPC handle (`mail-spam` page),
    /// wrapping the user-tier [`MailAccountClient`] — the same caller-scoped surface
    /// the aliases seam uses (priority #2/#4).
    ///
    /// Undo of a **client-written** (sealed) training row runs the reseal loop,
    /// which needs the actor's MSEK — so the page carries a [`MailSettingsMachine`]
    /// as its [`SealedModelWriter`], built from the same `keypair` so it derives the
    /// same MSEK (its MUA display block is never rendered here). A server-written
    /// (plaintext) row still undoes via the seam's server path. `ledger` is the
    /// host's grant-log seam: the page's contribute toggle mints (and its
    /// opt-out revokes) the spam-model baseline grant through that writer.
    pub fn build_mail_spam_machine(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        ledger: Arc<dyn SuccessionLedgerStore>,
    ) -> MailSpamMachine {
        let writer: Arc<dyn SealedModelWriter> = Arc::new(build_mail_settings_machine(
            Arc::clone(&nest),
            keypair,
            mail,
            node_url,
            ledger,
            // A writer / key custody only: it never hydrates the served state.
            None,
        ));
        MailSpamMachine::new(
            Arc::new(RpcMailSpamNest {
                account: MailAccountClient::new(nest),
            }),
            Some(writer),
        )
    }

    // ── MailExportNest (user-tier mail-export wizard) ───────────────────
    //
    // The twelve User-class kinds are live nest-side (`bridge_export_handlers.rs`,
    // 2026-09-21, `fail_export_session` 2026-09-22), so this seam is real: a thin projection over the shared
    // `fauna_mail::export::client::MailExportClient`, plus the one piece of
    // platform transport the loop must not see — resolving an over-frame
    // record's `body_ref` off the bulk-byte plane. Key custody is the
    // `MailSettingsMachine` built from the same keypair (§ Key material), the
    // composition `build_mail_spam_machine` already uses for its writer.

    struct RpcMailExportNest {
        client: fauna_mail::export::client::MailExportClient<Arc<NestClient>>,
        nest: Arc<NestClient>,
    }

    #[async_trait]
    impl MailExportNest for RpcMailExportNest {
        async fn list_own_mailboxes(&self) -> Result<Vec<ExportMailboxCount>, NestError> {
            Ok(self
                .client
                .list_own_mailboxes()
                .await
                .map_err(nest_error)?
                .mailboxes
                .into_iter()
                .map(|m| ExportMailboxCount {
                    name: m.name,
                    exists: m.exists,
                    uid_validity: m.uid_validity,
                })
                .collect())
        }
        async fn list_export_sessions(&self) -> Result<Vec<ExportSessionView>, NestError> {
            Ok(self
                .client
                .list_export_sessions()
                .await
                .map_err(nest_error)?
                .into_iter()
                .filter_map(super::project_export_session)
                .collect())
        }
        async fn start_export_session(
            &self,
            format: ExportFormat,
            scope: ExportScope,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, NestError> {
            let reply = self
                .client
                .start_export_session(
                    format.wire_name(),
                    super::encode_export_scope(&scope)?,
                    wrapped_session_key,
                    total_count,
                )
                .await
                .map_err(nest_error)?;
            Ok(super::fresh_export_session(
                reply.session_id,
                format,
                total_count,
                scope,
            ))
        }
        async fn fetch_export_chunk_ciphertext(
            &self,
            session_id: String,
            mailbox: String,
            after_uid: u32,
        ) -> Result<ExportFetchPage, NestError> {
            let reply = self
                .client
                .fetch_export_chunk_ciphertext(session_id, mailbox, after_uid)
                .await
                .map_err(nest_error)?;
            let mut records = Vec::with_capacity(reply.messages.len());
            for m in reply.messages {
                // An over-frame record crosses by reference; fetch it back into
                // the exact sealed bytes before it reaches the loop. A resolve
                // failure is a fetch failure — transient by construction — so
                // it surfaces as one, and the loop fails the session rather
                // than dropping the message (§ An unopenable record fails the
                // session).
                let sealed_body = match &m.body_ref {
                    Some(r) => fauna_mail::body_ref::resolve_referenced_mail_body(
                        &fauna_client::NestPublicChunkFetcher::new(&self.nest),
                        &r.chunk_hashes,
                        r.total_bytes,
                    )
                    .await
                    .map_err(|e| {
                        NestError::Transient(format!(
                            "resolve referenced body ({} uid {}): {e}",
                            m.mailbox, m.uid
                        ))
                    })?,
                    None => m.sealed_body,
                };
                records.push(ExportRecord {
                    mailbox: m.mailbox,
                    uid: m.uid,
                    flags: m.flags,
                    internal_date: m.internal_date,
                    stored_at: m.stored_at,
                    sealed_body,
                });
            }
            Ok(ExportFetchPage {
                records,
                next_after_uid: reply.next_after_uid,
                mailbox_done: reply.mailbox_done,
            })
        }
        async fn upload_export_chunk(
            &self,
            session_id: String,
            stream_generation: u64,
            chunk_idx: u64,
            sealed_chunk: Vec<u8>,
            progress: ExportChunkProgress,
        ) -> Result<ExportUploadAck, ExportSeamError> {
            let reply = self
                .client
                .upload_export_chunk(
                    session_id,
                    stream_generation,
                    chunk_idx,
                    sealed_chunk,
                    progress.exported_delta,
                    0,
                    0,
                    progress.last_processed_message_id,
                    progress.revised_total_count,
                )
                .await
                .map_err(super::export_seam_error)?;
            Ok(ExportUploadAck {
                blob_bytes: reply.blob_bytes,
                next_chunk_idx: reply.next_chunk_idx,
                exported_count: reply.exported_count,
            })
        }
        async fn pause_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .pause_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn resume_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .resume_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn restart_export_session(
            &self,
            session_id: String,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .restart_export_session(session_id, wrapped_session_key, total_count)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn cancel_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            self.client
                .cancel_export_session(session_id, as_driver_of)
                .await
                .map_err(super::export_seam_error)?;
            Ok(())
        }
        async fn finalize_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .finalize_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn fail_export_session(
            &self,
            session_id: String,
            reason: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            self.client
                .fail_export_session(session_id, reason, as_driver_of)
                .await
                .map_err(super::export_seam_error)?;
            Ok(())
        }
        async fn discard_export_blob(&self, session_id: String) -> Result<(), NestError> {
            // `existed = false` is the second discard of the same session —
            // idempotent from the client's side, deliberately not an error.
            self.client
                .discard_export_blob(session_id)
                .await
                .map_err(nest_error)?;
            Ok(())
        }
    }

    /// The native half of `mail-export.md` § Download flow: a streaming,
    /// actor-authenticated GET of the nest's per-session blob route, and the
    /// local file the archive is written into.
    ///
    /// **Streaming, not a convenience `get()`.** `NestContentApi::get` would be
    /// one line, and it buffers the whole body — which for an artifact
    /// § Quota composition caps at 10 GiB is the opener-in-memory shape the
    /// frame codec was built to avoid. So it goes through
    /// [`fauna_client::open_authed_stream`], which rides the session's own
    /// pinned client and shared bearer cache (opening no second connection and
    /// re-pinning no TLS) and hands the body back a slice at a time.
    pub struct NativeExportArchiveDelivery {
        auth: Arc<fauna_client::AuthClient>,
        /// Where a saved archive lands, chosen by the app's glue: a downloads
        /// directory on a desktop. Shared Rust must not pick it — this is the
        /// one genuinely per-platform half of the save.
        save_dir: std::path::PathBuf,
    }

    impl NativeExportArchiveDelivery {
        pub fn new(auth: Arc<fauna_client::AuthClient>, save_dir: std::path::PathBuf) -> Self {
            Self { auth, save_dir }
        }
    }

    #[async_trait]
    impl ExportArchiveDelivery for NativeExportArchiveDelivery {
        async fn open_download(
            &self,
            download_url: String,
        ) -> Result<Box<dyn SealedBlobStream>, DispatchError> {
            // The route answers 404 for a foreign actor, a still-running
            // session and a missing blob alike — deliberately (§ Cross-actor
            // isolation) — so the message carries the status and invents no
            // distinction the nest refused to make.
            let stream = fauna_client::open_authed_stream(&self.auth, &download_url)
                .await
                .map_err(|e| {
                    DispatchError::InvalidState(format!("download the export archive: {e:#}"))
                })?;
            Ok(Box::new(NativeBlobStream { stream }))
        }

        async fn create_archive(
            &self,
            file_name: String,
        ) -> Result<Box<dyn ArchiveFileSink>, DispatchError> {
            Ok(Box::new(
                NativeArchiveFile::create(&self.save_dir, &file_name).await?,
            ))
        }
    }

    struct NativeBlobStream {
        stream: fauna_client::NestAuthedByteStream,
    }

    #[async_trait]
    impl SealedBlobStream for NativeBlobStream {
        async fn next_slice(&mut self) -> Result<Option<Vec<u8>>, DispatchError> {
            self.stream.next_slice().await.map_err(|e| {
                DispatchError::InvalidState(format!("download the export archive: {e:#}"))
            })
        }
    }

    /// The local file a downloaded archive is written into.
    ///
    /// **Written under a `.part` name and renamed into place only by
    /// `finish()`.** § Download flow step 4 refuses a truncated or still-running
    /// archive, and the machine runs that refusal *before* it finishes the sink
    /// — but a refusal after the first frames were written would still leave
    /// those frames on disk, under exactly § Compression wrapper's
    /// `fauna-export-…zip.zst` name: a file that reads as the user's whole
    /// mailbox and is not, which is the half-restore the refusal exists to
    /// prevent. So the bytes land beside the final name, the rename is the one
    /// moment the archive comes to exist, and a sink dropped unfinished (a
    /// refused archive, a dropped stream, a failed write) deletes what it
    /// wrote. The same rename is why a failed re-download never truncates a
    /// good archive saved earlier the same day under the same name.
    struct NativeArchiveFile {
        /// The name the finished archive is shown under.
        path: std::path::PathBuf,
        /// Where the bytes accumulate until `finish()`; `None` once renamed.
        part: Option<std::path::PathBuf>,
        /// `None` only after `finish()` took it — an `Option` so `Drop` can
        /// close the handle before removing the file (Windows refuses to delete
        /// an open one).
        file: Option<tokio::fs::File>,
    }

    impl NativeArchiveFile {
        async fn create(
            save_dir: &std::path::Path,
            file_name: &str,
        ) -> Result<Self, DispatchError> {
            tokio::fs::create_dir_all(save_dir).await.map_err(|e| {
                DispatchError::InvalidState(format!("prepare {}: {e}", save_dir.display()))
            })?;
            let path = save_dir.join(file_name);
            let part = save_dir.join(format!("{file_name}.part"));
            let file = tokio::fs::File::create(&part).await.map_err(|e| {
                DispatchError::InvalidState(format!("create {}: {e}", part.display()))
            })?;
            Ok(Self {
                path,
                part: Some(part),
                file: Some(file),
            })
        }
    }

    impl Drop for NativeArchiveFile {
        fn drop(&mut self) {
            // Close first, then remove: an unfinished archive must not survive
            // as a file at all (the type's doc). Best-effort — a leftover
            // `.part` is at worst litter, and never carries the archive's name.
            drop(self.file.take());
            if let Some(part) = self.part.take() {
                let _ = std::fs::remove_file(part);
            }
        }
    }

    #[async_trait]
    impl ArchiveFileSink for NativeArchiveFile {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), DispatchError> {
            use tokio::io::AsyncWriteExt;
            let path = &self.path;
            let file = self.file.as_mut().ok_or_else(|| {
                DispatchError::InvalidState(format!("write {}: already closed", path.display()))
            })?;
            file.write_all(bytes)
                .await
                .map_err(|e| DispatchError::InvalidState(format!("write {}: {e}", path.display())))
        }

        async fn finish(mut self: Box<Self>) -> Result<String, DispatchError> {
            use tokio::io::AsyncWriteExt;
            let path = self.path.clone();
            let (Some(mut file), Some(part)) = (self.file.take(), self.part.clone()) else {
                return Err(DispatchError::InvalidState(format!(
                    "finish {}: already closed",
                    path.display()
                )));
            };
            // Flushed and synced before the path is shown: the user is about to
            // be told the archive is at this location, and on a crash a
            // buffered tail would make that a lie about their only copy.
            file.flush().await.map_err(|e| {
                DispatchError::InvalidState(format!("flush {}: {e}", path.display()))
            })?;
            file.sync_all().await.map_err(|e| {
                DispatchError::InvalidState(format!("sync {}: {e}", path.display()))
            })?;
            drop(file);
            tokio::fs::rename(&part, &path).await.map_err(|e| {
                DispatchError::InvalidState(format!("save {}: {e}", path.display()))
            })?;
            // Renamed: nothing is left for `Drop` to remove.
            self.part = None;
            Ok(path.display().to_string())
        }
    }

    /// Build a [`MailExportMachine`] over a native WS-RPC handle
    /// (`mail-export` page). `keypair` + `node_url` build the
    /// [`MailSettingsMachine`] that is the run's key custody (same keypair ⇒
    /// same MSEK ⇒ the export opens exactly what the inbox opens); `handle`
    /// names the archive's root directory; `save_dir` is where a downloaded
    /// archive lands (§ Download flow step 5 — the app's own downloads
    /// directory, which only the app can name).
    pub fn build_mail_export_machine(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        handle: &str,
        save_dir: std::path::PathBuf,
    ) -> MailExportMachine {
        let keys: Arc<dyn MailExportKeyCustody> = Arc::new(build_mail_settings_machine(
            Arc::clone(&nest),
            keypair,
            mail,
            node_url,
            // Key custody only (the MSEK) — this machine never touches the ledger.
            Arc::new(fauna_client_config::NoLedgerStore),
            // A writer / key custody only: it never hydrates the served state.
            None,
        ));
        let delivery: Arc<dyn ExportArchiveDelivery> = Arc::new(NativeExportArchiveDelivery::new(
            Arc::clone(nest.auth()),
            save_dir,
        ));
        MailExportMachine::new(
            Arc::new(RpcMailExportNest {
                client: fauna_mail::export::client::MailExportClient::new(Arc::clone(&nest)),
                nest,
            }),
            keys,
            delivery,
            handle,
        )
    }

    /// The seam-only build, for a caller that cannot drive
    /// [`MailExportMachine::run_export`] after `Start`/`Resume` — no secret to
    /// hold or no save directory to write to (linux and tui degrade to it, and
    /// the UniFFI seam-only builder hands it to android, windows and apple).
    ///
    /// Such a caller must not get key custody: `Start` would then open a real
    /// session that nothing ever drives — a Progress screen stuck at zero
    /// holding one of the user's three concurrency slots, which is exactly the
    /// fake-green Start the export track forbids. Without custody `Start`
    /// refuses honestly before any session exists, while the listing, the
    /// resume list and Cancel/Discard all work over the real seam. A caller
    /// that can drive the loop uses [`build_mail_export_machine`] instead.
    pub fn build_mail_export_machine_without_key_custody(
        nest: Arc<NestClient>,
    ) -> MailExportMachine {
        MailExportMachine::without_key_custody(Arc::new(RpcMailExportNest {
            client: fauna_mail::export::client::MailExportClient::new(Arc::clone(&nest)),
            nest,
        }))
    }

    // ── MailImportNest / ImportSourceNest (user-tier mail-import wizard) ────
    //
    // Two independent seams (`import.rs`'s module docs § Two seams, not one),
    // both real on native: the nest half (`import_sessions`, built since
    // 2026-07-08) wraps `fauna_mail::imap_client::MailImportClient`; the
    // foreign-server half wraps a live `fauna_mail::imap_client::ImapSession`
    // over the native transport (`tokio::net::TcpStream` + `tokio-rustls`, §
    // Where the IMAP client runs pins this row). The session-holding shape (an
    // async-mutexed `Option<ImapSession<NativeTlsTransport>>`) mirrors
    // `fauna-ffi::mail_import::FfiMailImportClient` — duplicated rather than
    // shared because this crate cannot depend on `fauna-ffi` (the dependency
    // runs the other way).

    struct RpcMailImportNest {
        client: imap_client::MailImportClient<Arc<NestClient>>,
        /// Kept beside `client` purely for label custody: `MailImportClient` is
        /// generic over its requester and deliberately holds no keypair, so the
        /// owner root has to come from the connection — the same split as
        /// `SyncClient::seal_device_label`.
        nest: Arc<NestClient>,
    }

    impl RpcMailImportNest {
        /// The owner custody this connection can seal and open import-source
        /// labels with, or `None` on a bearer-only connection (no keypair).
        ///
        /// Derived per call rather than cached: the connection's keypair is not
        /// fixed for the life of this struct, and a stale root fails *silently*
        /// (a wrong root renders `Omit`, never an error) — the failure mode
        /// this whole plane's funnels exist to make unrepresentable.
        fn owner_custody(&self) -> Option<fauna_core::file_download::FileDownloadKeys> {
            let keypair = self.nest.auth().keypair()?;
            Some(fauna_core::file_download::FileDownloadKeys::owner(
                fauna_core::crypto::BackupKey::derive(keypair.secret_bytes()),
            ))
        }

        /// Mint the sealed label for a descriptor about to be sent to the nest.
        /// `None` on a keyless connection or a seal error — both rest sealless,
        /// the ratified degrade (`file-sync.md` § Sealed names & paths):
        /// a session the user cannot label is worse than one resting unsealed
        /// until the boot scrub, and refusing the import outright would be worse
        /// than either.
        fn seal_source(&self, source_descriptor: &str) -> Option<Vec<u8>> {
            let keypair = self.nest.auth().keypair()?;
            let key = fauna_core::crypto::BackupKey::derive(keypair.secret_bytes());
            let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
            fauna_core::label_custody::seal_import_source(&root, source_descriptor).ok()
        }
    }

    #[async_trait]
    impl MailImportNest for RpcMailImportNest {
        async fn list_sessions(&self) -> Result<Vec<crate::import::ImportSessionView>, NestError> {
            // Derived once for the whole listing: a per-row derivation would
            // repeat the BackupKey KDF for every session on the resume screen.
            let custody = self.owner_custody();
            Ok(self
                .client
                .list_sessions()
                .await
                .map_err(nest_error)?
                .into_iter()
                .map(|info| super::project_import_session(info, custody.as_ref()))
                .collect())
        }

        async fn start_session(
            &self,
            source_descriptor: String,
            total_count: u64,
            scope: Vec<String>,
            date_from: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            let sealed = self.seal_source(&source_descriptor);
            let reply = self
                .client
                .start_session(
                    source_descriptor.clone(),
                    total_count,
                    scope.clone(),
                    date_from.clone(),
                    sealed,
                )
                .await
                .map_err(nest_error)?;
            Ok(super::fresh_import_session(
                reply.session_id,
                source_descriptor,
                total_count,
                scope,
                date_from,
            ))
        }

        async fn pause_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .pause_session(session_id)
                    .await
                    .map_err(nest_error)?,
                self.owner_custody().as_ref(),
            ))
        }

        async fn resume_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .resume_session(session_id)
                    .await
                    .map_err(nest_error)?,
                self.owner_custody().as_ref(),
            ))
        }

        async fn cancel_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .cancel_session(session_id)
                    .await
                    .map_err(nest_error)?,
                self.owner_custody().as_ref(),
            ))
        }

        async fn finalize_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .finalize_session(session_id)
                    .await
                    .map_err(nest_error)?,
                self.owner_custody().as_ref(),
            ))
        }

        async fn fail_session(
            &self,
            session_id: String,
            reason: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .fail_session(session_id, reason)
                    .await
                    .map_err(nest_error)?,
                self.owner_custody().as_ref(),
            ))
        }

        async fn send_unit(
            &self,
            session_id: String,
            unit: imap_client::ImportUnit,
            skip_dedup: bool,
            revised_total_count: Option<u64>,
        ) -> Result<crate::import::SendUnitReply, NestError> {
            Ok(super::project_import_unit_reply(
                self.client
                    .send_unit(session_id, unit, skip_dedup, revised_total_count)
                    .await
                    .map_err(nest_error)?,
            ))
        }
    }

    /// § Failure handling's TCP/TLS-fault classification, shared by every
    /// [`ImportSourceNest`] method: [`imap_client::ImapClientError::Transport`]/`Eof`
    /// are the retryable transport faults `run_import`'s backoff loop targets;
    /// every other variant is the source telling us something true (bad
    /// creds, a rejected command, a rebuilt mailbox) and is never retried.
    fn imap_error(e: imap_client::ImapClientError) -> NestError {
        use imap_client::ImapClientError;
        match e {
            ImapClientError::Transport(_) | ImapClientError::Eof => {
                NestError::Transient(e.to_string())
            }
            _ => NestError::Rejected(e.to_string()),
        }
    }

    /// [`imap_client::NativeImapConnector::connect`]'s pre-IMAP transport
    /// faults. A bad hostname or an unparseable trust anchor is a
    /// configuration mistake the user must fix (never retried); TCP/TLS-level
    /// failures are transient, same classification as [`imap_error`]'s
    /// `Transport`/`Eof`.
    fn native_transport_error(e: imap_client::NativeTransportError) -> NestError {
        use imap_client::NativeTransportError;
        match e {
            NativeTransportError::InvalidHostname(_) | NativeTransportError::TrustAnchor(_) => {
                NestError::Rejected(e.to_string())
            }
            NativeTransportError::Connect { .. }
            | NativeTransportError::Tls { .. }
            | NativeTransportError::Io(_) => NestError::Transient(e.to_string()),
        }
    }

    fn source_not_connected() -> NestError {
        NestError::Rejected("not connected to the source server".into())
    }

    /// One source connection, native transport. § Throttling's budget lives
    /// inside `ImapSession` itself (per source *server*, not per mailbox), so
    /// this seam holds exactly one session — mirrors
    /// `fauna-ffi::mail_import::FfiMailImportClient`'s internals.
    struct RpcImportSourceNest {
        session: AsyncMutex<Option<imap_client::ImapSession<imap_client::NativeTlsTransport>>>,
        clock: imap_client::TokioClock,
    }

    impl RpcImportSourceNest {
        fn new() -> Self {
            Self {
                session: AsyncMutex::new(None),
                clock: imap_client::TokioClock::new(),
            }
        }
    }

    #[async_trait]
    impl ImportSourceNest for RpcImportSourceNest {
        async fn connect(&self, params: SourceConnectParams) -> Result<(), NestError> {
            let mode = match params.tls_mode {
                ImportTlsMode::Implicit => imap_client::TlsMode::Implicit,
                ImportTlsMode::StartTls => imap_client::TlsMode::StartTls,
            };
            let connector = imap_client::NativeImapConnector::new(mode);
            let transport = connector
                .connect(&params.host, params.port)
                .await
                .map_err(native_transport_error)?;
            let mut session = match mode {
                imap_client::TlsMode::Implicit => {
                    imap_client::ImapSession::connect(transport).await
                }
                imap_client::TlsMode::StartTls => {
                    imap_client::ImapSession::connect_starttls(transport).await
                }
            }
            .map_err(imap_error)?;
            session
                .login(&params.username, params.password.as_str())
                .await
                .map_err(imap_error)?;
            *self.session.lock().await = Some(session);
            Ok(())
        }

        async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().ok_or_else(source_not_connected)?;
            let boxes = session.list_mailboxes().await.map_err(imap_error)?;
            Ok(boxes
                .into_iter()
                .map(|m| SourceMailboxView {
                    name: m.name,
                    selectable: m.selectable,
                    // `LIST` carries no per-mailbox count (only `EXAMINE`
                    // does); `SourceMailboxOption::message_count`'s own docs
                    // sanction 0 here — the Confirm estimate is refined once
                    // `run_import` `EXAMINE`s each selected mailbox.
                    message_count: 0,
                })
                .collect())
        }

        async fn examine(
            &self,
            mailbox: &str,
            cursor: Option<MailboxCursor>,
        ) -> Result<imap_client::MailboxStatus, NestError> {
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().ok_or_else(source_not_connected)?;
            session
                .examine(mailbox, cursor.map(|c| c.source_uid_validity))
                .await
                .map_err(imap_error)
        }

        async fn fetch_window(
            &self,
            // Not read: `ImapSession::enumerate_uids`/`fetch_messages` act on
            // whichever mailbox this session currently has `EXAMINE`d, and
            // `run_import_inner`'s calling contract always issues an
            // `examine(mailbox, cursor)` immediately before every
            // `fetch_window(mailbox, cursor, _)` for that same mailbox — so by
            // the time this runs the session is already on the right one.
            // `require_selected` inside the session is the backstop if a
            // future caller ever breaks that ordering.
            _mailbox: &str,
            cursor: Option<MailboxCursor>,
            max: usize,
        ) -> Result<Vec<imap_client::FetchOutcome>, NestError> {
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().ok_or_else(source_not_connected)?;
            let from_uid = cursor.map_or(1, |c| c.last_processed_source_uid + 1);
            // `enumerate_uids` has no cap of its own (§ its own docs), so a
            // large mailbox is re-enumerated from `from_uid` on every window —
            // correct (each call returns the right slice) but not the
            // cheapest possible re-fetch pattern; a future pass could cache
            // the enumeration per mailbox inside this seam if it becomes a
            // measured cost.
            let uids: Vec<u32> = session
                .enumerate_uids(from_uid)
                .await
                .map_err(imap_error)?
                .into_iter()
                .take(max)
                .map(|e| e.uid)
                .collect();
            let mut out = Vec::with_capacity(uids.len());
            session
                .fetch_messages(&uids, &self.clock, |o| out.push(o))
                .await
                .map_err(imap_error)?;
            Ok(out)
        }

        async fn logout(&self) -> Result<(), NestError> {
            imap_client::logout_and_clear(&self.session)
                .await
                .map_err(imap_error)
        }
    }

    /// § Failure handling's TCP/TLS backoff (5 s / 30 s / 2 min), on tokio —
    /// the real [`RetryClock`] `MailImportMachine::retrying` sleeps through
    /// between attempts.
    struct RpcRetryClock;

    #[async_trait]
    impl RetryClock for RpcRetryClock {
        async fn sleep_ms(&self, ms: u64) {
            fauna_sleep::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    /// Build a [`MailImportMachine`] over a native WS-RPC handle + a native
    /// foreign-IMAP-source connection (`mail-import` page). Both seams are
    /// real: the nest half (S9.4) wraps
    /// `fauna_mail::imap_client::MailImportClient`; the source half wraps a
    /// live `ImapSession` over `tokio::net::TcpStream` + `tokio-rustls` (§
    /// Where the IMAP client runs).
    pub fn build_mail_import_machine(nest: Arc<NestClient>) -> MailImportMachine {
        MailImportMachine::new(
            Arc::new(RpcMailImportNest {
                client: imap_client::MailImportClient::new(Arc::clone(&nest)),
                nest,
            }),
            Arc::new(RpcImportSourceNest::new()),
            Arc::new(RpcRetryClock),
        )
    }

    // ── MailLists / MailListMembers (user-tier mailing lists) ───────────
    //
    // The nine list RPCs are live nest-side (`bridge_list_handlers.rs`, registered
    // at `bins/fauna-nest/src/lib.rs:1344`, all `User`-class). Both seams talk the
    // WS-RPC connection directly (never `MailAccountClient`, whose `request()` is
    // documented NOT to auto-retry on disconnect) so every kind except
    // `create_account_list` — every one is nest-registered `forbid_replay: false`
    // — can ride the same `request_idempotent` retry the mail-provision seam below
    // already uses. Without this, a mid-flight WS drop on this heavily-loaded dev
    // fleet (a slower multi-address batch-import has a wider window to catch one)
    // surfaced immediately as "rpc disconnected" with zero resilience. `create_account_list` stays a bare, unretried
    // call: it is nest-registered `forbid_replay: true` because a replay would
    // surface a spurious UNIQUE(local_domain, pattern, kind) conflict.
    //
    // These were stubs returning `unimplemented` from the pages' first landing
    // until 2026-07-29: the backend shipped 2026-06-13/14 and nothing rewired the
    // seam, so six apps rendered two pages that could not do anything. The wire →
    // view projections are the shared `lists::{project_list_row,
    // project_member_row}`, never hand-rolled per app.

    struct RpcMailListsNest {
        nest: Arc<NestClient>,
    }

    #[async_trait]
    impl MailListsNest for RpcMailListsNest {
        async fn list_account_lists(&self) -> Result<Vec<ListView>, NestError> {
            let reply: ListAccountListsReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
                provision_backoff,
            )
            .await?;
            Ok(reply
                .lists
                .into_iter()
                .map(crate::lists::project_list_row)
                .collect())
        }

        /// The add-sheet domain picker. Lists are **user-tier**
        /// (`mail-mass-mailing.md` § Architectural rules), so this must not call
        /// the Admin-class `fauna.bridges.list_local_domains` — the options are
        /// derived from rows the caller already owns (shared
        /// `lists::derive_list_domains`).
        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            let lists: ListAccountListsReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
                provision_backoff,
            )
            .await?;
            let list_domains: Vec<String> =
                lists.lists.into_iter().map(|r| r.local_domain).collect();
            let aliases: ListAccountAliasesReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.list_account_aliases",
                ListAccountAliasesRequest {},
                provision_backoff,
            )
            .await?;
            let alias_domains: Vec<String> = aliases
                .aliases
                .into_iter()
                .map(|r| r.local_domain)
                .collect();
            Ok(crate::lists::derive_list_domains(
                &list_domains,
                &alias_domains,
            ))
        }

        async fn create_account_list(&self, draft: ListDraft) -> Result<(), NestError> {
            // NOT retried — see the module note above (`forbid_replay: true`).
            let _: CreateAccountListReply = self
                .nest
                .request(
                    "fauna.bridges.create_account_list",
                    CreateAccountListRequest {
                        local_part: draft.local_part,
                        local_domain: draft.local_domain,
                        friendly_name: super::opt_text(draft.friendly_name),
                        description: super::opt_text(draft.description),
                        list_help_url: super::opt_text(draft.list_help_url),
                        list_archive_url: super::opt_text(draft.list_archive_url),
                        recipients_per_send: draft.recipients_per_send.map(i64::from),
                    },
                )
                .await
                .map_err(nest_error)?;
            Ok(())
        }

        async fn update_account_list(
            &self,
            list_id: Vec<u8>,
            draft: ListDraft,
        ) -> Result<(), NestError> {
            let _: UpdateAccountListReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.update_account_list",
                UpdateAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                    friendly_name: super::opt_text(draft.friendly_name),
                    description: super::opt_text(draft.description),
                    list_help_url: super::opt_text(draft.list_help_url),
                    list_archive_url: super::opt_text(draft.list_archive_url),
                    recipients_per_send: draft.recipients_per_send.map(i64::from),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn delete_account_list(&self, list_id: Vec<u8>) -> Result<(), NestError> {
            let _: DeleteAccountListReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.delete_account_list",
                DeleteAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }
    }

    /// Build a [`MailListsMachine`] over a native WS-RPC handle (the user
    /// `mail-lists` page, `mail-mass-mailing.md` § `mail-lists` page UX).
    pub fn build_mail_lists_machine(nest: Arc<NestClient>) -> MailListsMachine {
        MailListsMachine::new(Arc::new(RpcMailListsNest { nest }))
    }

    struct RpcMailListMembersNest {
        nest: Arc<NestClient>,
    }

    #[async_trait]
    impl MailListMembersNest for RpcMailListMembersNest {
        async fn list_list_members(&self, list_id: Vec<u8>) -> Result<ListMembers, NestError> {
            // `include_unsubscribed` is always on: the page renders both halves
            // and its `mail-list-members-summary` needs both counts.
            let reply: ListListMembersReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.list_list_members",
                ListListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    include_unsubscribed: true,
                },
                provision_backoff,
            )
            .await?;
            Ok(ListMembers {
                members: reply
                    .members
                    .into_iter()
                    .map(crate::lists::project_member_row)
                    .collect(),
                subscribed_count: reply.subscribed_count.max(0) as u32,
                unsubscribed_count: reply.unsubscribed_count.max(0) as u32,
            })
        }

        async fn add_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: AddListMemberReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.add_list_member",
                AddListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn batch_import_list_members(
            &self,
            list_id: Vec<u8>,
            addresses: Vec<String>,
        ) -> Result<ImportResult, NestError> {
            let tally: BatchImportListMembersReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.batch_import_list_members",
                BatchImportListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    addresses,
                },
                provision_backoff,
            )
            .await?;
            Ok(ImportResult {
                added: tally.added,
                skipped_invalid: tally.skipped_invalid,
                skipped_duplicate: tally.skipped_duplicate,
            })
        }

        async fn unsubscribe_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: UnsubscribeListMemberReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.unsubscribe_list_member",
                UnsubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn resubscribe_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: ResubscribeListMemberReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.resubscribe_list_member",
                ResubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }
    }

    /// Build a [`MailListMembersMachine`] for `list_id_hex` (from a rendered
    /// [`ListView`]). Errors only on a malformed `list_id_hex`.
    pub fn build_mail_list_members_machine(
        nest: Arc<NestClient>,
        list_id_hex: String,
        list_name: String,
    ) -> Result<MailListMembersMachine, crate::error::DispatchError> {
        MailListMembersMachine::new(
            Arc::new(RpcMailListMembersNest { nest }),
            list_id_hex,
            list_name,
        )
    }

    // ── MailSettings seams (mail-credentials page) ──────────────────
    //
    // The seams `MailSettingsMachine` injects, all thin generic glue over
    // the same `Arc<NestClient>` WS-RPC handle (no per-app logic — lifted
    // verbatim from linux's old `mail_glue.rs`). Unlike the admin machines, the
    // machine takes four constructor args, so `build_mail_settings_machine`
    // takes the actor keypair + node URL alongside the transport.

    /// The machine's `NestClient` over the WS-RPC handle. Each method serializes
    /// the typed wrapped blob to its canonical bytes and `request`s the matching
    /// `fauna.bridges.*` kind. Mirrors `fauna_client_bridges::BridgesClient`.
    struct RpcMailNestClient {
        nest: Arc<NestClient>,
        /// The owner's folder-key custody the served state is read from.
        folder_keys: super::FolderKeys,
    }

    impl RpcMailNestClient {
        /// A client-side serialization fault on a fixed wire shape — practically
        /// unreachable. Treated as a non-retryable rejection.
        fn map_encode(e: impl std::fmt::Display) -> NestError {
            NestError::Rejected(format!("encode wrapped blob: {e}"))
        }
    }

    #[async_trait]
    impl MailNestClient for RpcMailNestClient {
        async fn provision_wrapped_mls_blob(&self, blob: WrappedMsekBlob) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.provision_wrapped_mls_blob",
                ProvisionWrappedMlsBlobRequest {
                    blob: ByteBuf::from(bytes),
                    // index = (actor_id, credential_id); nest keys the row on it.
                    credential_id: blob.index.1.clone(),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_mls_snapshot_blob(
            &self,
            blob: MlsSnapshotBlob,
        ) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.provision_mls_snapshot_blob",
                ProvisionMlsSnapshotBlobRequest {
                    blob: ByteBuf::from(bytes),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_wrapped_submission_token(
            &self,
            blob: WrappedSubmissionTokenBlob,
        ) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.provision_wrapped_submission_token",
                ProvisionWrappedSubmissionTokenRequest {
                    blob: ByteBuf::from(bytes),
                    // index = (actor_id, credential_id); nest keys the row on it.
                    credential_id: blob.index.1.clone(),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn revoke_wrapped_mls_blob(
            &self,
            actor_id: [u8; 32],
            credential_id: String,
        ) -> Result<(), NestError> {
            let _: RevokeReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.revoke_wrapped_mls_blob",
                RevokeWrappedMlsBlobRequest {
                    actor_id: actor_id.to_vec(),
                    credential_id,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn revoke_wrapped_submission_token(
            &self,
            actor_id: [u8; 32],
            credential_id: String,
        ) -> Result<(), NestError> {
            let _: RevokeReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.revoke_wrapped_submission_token",
                RevokeWrappedSubmissionTokenRequest {
                    actor_id: actor_id.to_vec(),
                    credential_id,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_recipient_mls_pubkey(
            &self,
            actor_id: [u8; 32],
            pubkey: [u8; 32],
            mlkem_ek: Vec<u8>,
            epoch_keys: Option<Vec<EpochSealKey>>,
        ) -> Result<(), NestError> {
            let _: ProvisionReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.provision_recipient_mls_pubkey",
                ProvisionRecipientMlsPubkeyRequest {
                    actor_id: ByteBuf::from(actor_id.to_vec()),
                    mls_pubkey: ByteBuf::from(pubkey.to_vec()),
                    // The post-quantum ML-KEM ek (1184 B) and the
                    // content-sealing-epoch schedule — the machine always
                    // publishes both.
                    mlkem_ek: ByteBuf::from(mlkem_ek),
                    epoch_keys,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn nest_supports(&self, token: &str) -> Result<bool, NestError> {
            let reply: NestInfoReply = super::request_idempotent(
                &*self.nest,
                "fauna.nest.info",
                NestInfoRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(capability::supports(&reply.capabilities, token))
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            let _: SetMailEnabledReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.set_mail_enabled",
                SetMailEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn set_mail_serving_enabled(&self, enabled: bool) -> Result<(), NestError> {
            let _: SetMailServingEnabledReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.set_mail_serving_enabled",
                SetMailServingEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn get_mail_serving_enabled(&self) -> Result<bool, NestError> {
            // Caller-scoped read: empty `actor_id` ⇒ the nest forces a `User`
            // caller to its own flag.
            let reply: GetMailServingEnabledReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.get_mail_serving_enabled",
                GetMailServingEnabledRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(reply.enabled)
        }

        async fn get_caldav_port(&self) -> Result<u16, NestError> {
            // Nest-wide singleton (not caller-scoped); the request is empty.
            let reply: GetCaldavPortReply = super::request_idempotent(
                &*self.nest,
                "fauna.bridges.get_caldav_port",
                GetCaldavPortRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(reply.port)
        }

        async fn serves_any_webdav_set(&self) -> Result<bool, NestError> {
            // Owner-scoped enumeration (the historic projection — never opt into
            // the member-visible union: a set *shared with* me is served by its
            // owner's blob, not mine).
            let reply: FoldersListReply = super::request_idempotent(
                &*self.nest,
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(super::custody_serves_any(reply.folders, &self.folder_keys).await)
        }

        async fn fetch_spam_model(
            &self,
            actor_id: [u8; 32],
        ) -> Result<FetchedSpamModel, NestError> {
            MailAccountClient::new(Arc::clone(&self.nest))
                .fetch_spam_model(actor_id.to_vec())
                .await
                .map(|reply| FetchedSpamModel {
                    blob: reply.blob.map(|b| b.into_vec()),
                    stored_sealed: reply.stored_sealed,
                    contribute_baseline: reply.contribute_baseline,
                    holder_seal_target: reply.holder_seal_target,
                })
                .map_err(|e| NestError::Rejected(format!("fetch_spam_model: {e}")))
        }

        async fn put_spam_model(
            &self,
            sealed_model: Vec<u8>,
            sample_count: u32,
            history_op: Option<SpamHistoryOp>,
            holder_copy: Option<SpamModelHolderCopy>,
        ) -> Result<PutSpamModelOutcome, NestError> {
            MailAccountClient::new(Arc::clone(&self.nest))
                .put_spam_model(sealed_model, sample_count, history_op, holder_copy)
                .await
                .map_err(|e| NestError::Rejected(format!("put_spam_model: {e}")))
        }

        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), NestError> {
            CapabilitiesClient::new(Arc::clone(&self.nest))
                .mint(grant_blob)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), NestError> {
            CapabilitiesClient::new(Arc::clone(&self.nest))
                .revoke(grant_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn renew_grant(
            &self,
            grant_id: [u8; 16],
            new_epoch_start: u64,
            new_epoch_end: u64,
            appended_keys: Vec<Vec<u8>>,
        ) -> Result<(), NestError> {
            CapabilitiesClient::new(Arc::clone(&self.nest))
                .renew(grant_id, new_epoch_start, new_epoch_end, appended_keys)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, NestError> {
            let admin = MailAdminClient::new(Arc::clone(&self.nest));
            discover_holders(&admin).await
        }

        async fn fetch_post_body_text(
            &self,
            content_id: &str,
        ) -> Result<Option<String>, NestError> {
            super::fetch_post_body_text(Arc::clone(&self.nest), content_id).await
        }

        async fn moderation_train(&self, content_id: &str, verdict: &str) -> Result<(), NestError> {
            super::moderation_train(Arc::clone(&self.nest), content_id, verdict).await
        }
    }

    /// Signs `SubmissionToken`s + capability `GrantEvent`s with the actor's Ed25519
    /// key. The machine hands a fully-populated token (placeholder `user_sig`) /
    /// unsigned grant event; the `sign` call replaces the placeholder signature —
    /// the raw key never crosses the FFI boundary.
    struct RpcIdentitySigner {
        signing_key: SigningKey,
    }

    impl IdentitySigner for RpcIdentitySigner {
        fn sign_submission_token(
            &self,
            token: SubmissionToken,
        ) -> Result<SubmissionToken, SignerError> {
            token
                .sign(&self.signing_key)
                .map_err(|e| SignerError::Sign(e.to_string()))
        }
        fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, SignerError> {
            event
                .sign(&self.signing_key)
                .map_err(|e| SignerError::GrantEventSign(e.to_string()))
        }
    }

    /// Build a [`MailSettingsMachine`] over a native WS-RPC handle, wiring all
    /// three shared seams + the [`MuaInstructions`] block derived from `node_url`.
    /// The per-app glue only supplies `(nest, keypair, node_url)` and its
    /// account-store `ledger` (the grant-event log) — every seam impl is shared
    /// (priority #2/#4). Replaces linux's `LinuxConfigStore` /
    /// `LinuxNestClient` / `LinuxIdentitySigner`.
    pub fn build_mail_settings_machine(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        ledger: Arc<dyn SuccessionLedgerStore>,
        folder_keys: super::FolderKeys,
    ) -> MailSettingsMachine {
        let actor_id = keypair.actor_id().0;
        let signing_key = keypair.signing_key().clone();
        let nest_seam: Arc<dyn MailNestClient> = Arc::new(RpcMailNestClient { nest, folder_keys });
        let signer: Arc<dyn IdentitySigner> = Arc::new(RpcIdentitySigner { signing_key });
        let mua = MuaInstructions::for_node_url(node_url);

        MailSettingsMachine::new(actor_id, nest_seam, ledger, mail, signer, mua)
    }

    /// [`build_mail_settings_machine`] over a signed-in connection's OWN
    /// identity and nest URL — what a host's store-ready edge hands the
    /// aftermath's mail burn (`fauna_client_recovery::ledger_aftermath`), where
    /// it holds the connection and the account store but no separate secret.
    /// `None` for a bearer-only connection, which holds no identity to sign
    /// with.
    pub fn build_mail_settings_machine_for_session(
        nest: Arc<NestClient>,
        mail: Arc<dyn MailStore>,
        ledger: Arc<dyn SuccessionLedgerStore>,
    ) -> Option<MailSettingsMachine> {
        let secret = *nest.auth().keypair()?.secret_bytes();
        let node_url = nest.nest_url();
        Some(build_mail_settings_machine(
            nest,
            ActorKeypair::from_secret(secret),
            mail,
            &node_url,
            ledger,
            // The aftermath's mail burn never hydrates the served state.
            None,
        ))
    }

    /// The post-claim serving enablement over the native transport — the ONE
    /// entry every native app's `LoggedIn` handoff calls with the four intents
    /// it read off the onboarding machine (`onboarding.md` § 3b *Mechanism*).
    /// Runs [`crate::serving_enablement::plan`] to the end and publishes the
    /// `{started, completed}` anchor
    /// ([`crate::serving_enablement::serving_enablement_json`]). The caller
    /// spawns the returned future; it resolves once every step has answered.
    pub async fn dispatch_post_claim_serving_enablement(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: String,
        ledger: Arc<dyn SuccessionLedgerStore>,
        intents: crate::serving_enablement::ServingEnablementIntents,
    ) {
        let actor_id_hex = keypair.actor_id_hex();
        let secret = zeroize::Zeroizing::new(*keypair.secret_bytes());
        let mail_nest = Arc::clone(&nest);
        let executor = crate::serving_enablement::RpcServingEnablement {
            nest: Arc::clone(&nest),
            build_mail_machine: move || {
                build_mail_settings_machine(
                    Arc::clone(&mail_nest),
                    ActorKeypair::from_secret(*secret),
                    Arc::clone(&mail),
                    &node_url,
                    Arc::clone(&ledger),
                    // Serving enablement never reads the served state.
                    None,
                )
            },
            bridges: build_bridge_approval_machine(nest),
        };
        crate::serving_enablement::apply_serving_enablement(actor_id_hex, intents, &executor).await
    }

    // ── Relay mailbox provisioning (home-with-public-relay auto-trigger) ──────

    /// Build a [`MailSettingsMachine`] over the account's **mail custody** (the
    /// MSEK source — `fauna.state.mail`, the same rows on every box), while its
    /// **nest seam targets the peer nest**
    /// (the provision target). The relay-provisioning twin of
    /// [`build_mail_settings_machine`] — the one split that makes
    /// `ProvisionRelayMailbox` reuse the account's MSEK and push the read recipe
    /// onto the peer's non-federating `bridge_*` store, so the home box's MDA
    /// derives the **same** recipient keypair the public relay seals inbound
    /// mail to (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § Pairing). `peer_node_url` only feeds the (provision-unused) MUA block.
    pub fn build_relay_provision_machine(
        peer_nest: Arc<NestClient>,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        peer_node_url: &str,
        ledger: Arc<dyn SuccessionLedgerStore>,
    ) -> MailSettingsMachine {
        let actor_id = keypair.actor_id().0;
        let signing_key = keypair.signing_key().clone();
        // The nest seam targets the PEER (the provision target).
        let nest_seam: Arc<dyn MailNestClient> = Arc::new(RpcMailNestClient {
            nest: peer_nest,
            // The provision target is the peer: its served state is never read.
            folder_keys: None,
        });
        let signer: Arc<dyn IdentitySigner> = Arc::new(RpcIdentitySigner { signing_key });
        let mua = MuaInstructions::for_node_url(peer_node_url);

        MailSettingsMachine::new(actor_id, nest_seam, ledger, mail, signer, mua)
    }

    /// [`PostLinkHook`] that auto-provisions the user's mailbox onto a just-linked
    /// peer nest (the home-with-public-relay home box), reusing the account's
    /// MSEK. Holds the **primary** connection, the user's secret and the mail
    /// custody; on `after_link_both` it gates on the account holding an MSEK (a
    /// no-op for content-only multi-homing), opens an authenticated connection to
    /// the peer with the same identity, and dispatches `ProvisionRelayMailbox` on
    /// a peer-bound machine carrying the same mail custody.
    /// `deployment-home-with-public-relay.md` § Pairing; `mail-credentials.md`
    /// § Trigger taxonomy.
    struct MailRelayProvisionHook {
        /// The user's 32-byte ed25519 secret. Stored raw (not as `ActorKeypair`,
        /// which is non-`Clone`) and reconstructed per use; `Zeroizing` inside
        /// `ActorKeypair` scrubs each copy on drop.
        secret: [u8; 32],
        /// The host's grant-event log seam, which the peer-bound machine
        /// carries like every other mail-settings machine.
        ledger: Arc<dyn SuccessionLedgerStore>,
        /// The account's mail custody — the MSEK source.
        mail: Arc<dyn MailStore>,
    }

    impl MailRelayProvisionHook {
        fn keypair(&self) -> ActorKeypair {
            ActorKeypair::from_secret(self.secret)
        }
    }

    #[async_trait]
    impl PostLinkHook for MailRelayProvisionHook {
        async fn after_link_both(&self, peer_url: &str) -> Result<(), PairDispatchError> {
            // Gate: only provision when the account holds a mailbox. Linking two
            // of the user's nests for content-only multi-homing is valid — there
            // is no mailbox to provision then, so the link succeeds untouched.
            let mail = self.mail.load().await.map_err(|e| {
                PairDispatchError::Nest(PairNestError::Transient(format!(
                    "read the account's mail custody: {e}"
                )))
            })?;
            if mail.msek.is_none() {
                return Ok(());
            }

            // Open a second authenticated connection to the peer (home box) with
            // the user's same identity (registered on both), then provision the
            // read recipe there reusing the account's MSEK.
            let peer = NestClient::new(peer_url.to_string(), self.keypair());
            peer.connect().await.map_err(|e| {
                PairDispatchError::Nest(PairNestError::Transient(format!(
                    "connect to home box {peer_url}: {e}"
                )))
            })?;
            let machine = build_relay_provision_machine(
                Arc::clone(&peer),
                self.keypair(),
                Arc::clone(&self.mail),
                peer_url,
                Arc::clone(&self.ledger),
            );
            machine
                .dispatch(crate::state::MailSettingsAction::ProvisionRelayMailbox)
                .await
                .map_err(|e| {
                    PairDispatchError::Nest(PairNestError::Transient(format!(
                        "provision mailbox on home box: {e}"
                    )))
                })?;
            // No per-box copy of the mail record: the custody is the account's
            // (`fauna.state.mail`, fleet-only rows every device of the account
            // reads, whichever box it is connected to), so a fauna app logged
            // into the home box already holds the read keys (`mail-credentials.md`
            // rule 8 — one MSEK per actor across every box).
            Ok(())
        }
    }

    /// Build a [`LinkedNestsMachine`] for the `linked-nests` page wired with the
    /// mail relay-provisioning post-link hook: after a both-ends `LinkBoth`, the
    /// user's mailbox auto-provisions onto the just-linked peer (home box) reusing
    /// the fleet MSEK — the one-action home-with-public-relay flow
    /// (`deployment-home-with-public-relay.md` § Pairing). The single shared
    /// constructor every **native** client calls (priority #2); `keypair` is the
    /// user's identity (registered on both nests) and the primary is the connected
    /// nest. Web stays on the hook-less `build_linked_nests_machine_with_peer` — its
    /// `LinkBoth` is blocked on the second-origin-WS seam, so there is nothing to
    /// trigger there yet.
    pub fn build_linked_nests_machine_with_mail_relay(
        primary_nest: Arc<NestClient>,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        mail: Arc<dyn MailStore>,
    ) -> LinkedNestsMachine {
        let secret = *keypair.secret_bytes();
        let hook: Arc<dyn PostLinkHook> = Arc::new(MailRelayProvisionHook {
            secret,
            ledger,
            mail,
        });
        build_linked_nests_machine_with_hook(primary_nest, hook)
    }

    /// Like [`build_linked_nests_machine_with_mail_relay`] but *also* wires the
    /// trust-facet seams (the Nests page): the machine keeps the mailbox
    /// auto-provision hook AND hydrates the home nest's trust facet + drives
    /// Mint/Renew/Revoke/SetLens (`nests.md` § Trust facet). The single shared
    /// constructor every **native** client (linux now; windows/macos/ios/android
    /// via `fauna-ffi`) calls for the trust-enabled Nests page (priority #2). The
    /// hook needs only the 32-byte secret (a copy), so `keypair` survives to build
    /// the trust seams — the raw identity key goes only into the signer seam,
    /// never across FFI. `blessings` is the seat's account-plane blessing door
    /// (`fauna_account_seams::blessed_nests::PlaneBlessedNests`); `backup_state`
    /// the account store's backup-destination state (`fauna.state.backup`).
    pub fn build_linked_nests_machine_with_mail_relay_and_trust(
        primary_nest: Arc<NestClient>,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn fauna_client_pair::BackupStateStore>,
        blessings: Arc<dyn fauna_client_pair::BlessedNestsStore>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        mail: Arc<dyn MailStore>,
    ) -> LinkedNestsMachine {
        let secret = *keypair.secret_bytes();
        let hook: Arc<dyn PostLinkHook> = Arc::new(MailRelayProvisionHook {
            secret,
            ledger: Arc::clone(&ledger),
            mail: Arc::clone(&mail),
        });
        build_linked_nests_machine_with_hook_and_trust(
            primary_nest,
            hook,
            keypair,
            ledger,
            backup_state,
            blessings,
            period_keys,
            mail,
        )
    }

    #[cfg(test)]
    mod archive_file_tests {
        use super::NativeArchiveFile;
        use crate::export::ArchiveFileSink;

        /// A fresh, empty directory per test under the system temp dir —
        /// unique per process and per call, and emptied first in case a
        /// crashed earlier run with a recycled pid left one behind.
        fn scratch_dir(tag: &str) -> std::path::PathBuf {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "fauna-export-sink-{tag}-{}-{n}",
                std::process::id(),
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn listing(dir: &std::path::Path) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        const NAME: &str = "fauna-export-alice-mbox-2026-09-25.zip.zst";

        #[tokio::test]
        async fn a_finished_archive_rests_under_its_name_and_nothing_else() {
            let dir = scratch_dir("finished");
            let mut sink: Box<dyn ArchiveFileSink> =
                Box::new(NativeArchiveFile::create(&dir, NAME).await.unwrap());
            sink.write(b"frame one ").await.unwrap();
            sink.write(b"frame two").await.unwrap();
            let shown = sink.finish().await.unwrap();
            assert_eq!(shown, dir.join(NAME).display().to_string());
            assert_eq!(listing(&dir), vec![NAME.to_string()]);
            assert_eq!(
                std::fs::read(dir.join(NAME)).unwrap(),
                b"frame one frame two"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }

        /// The refused-archive case: frames were written, then the terminator
        /// check failed and the machine dropped the sink unfinished. No file may
        /// remain that reads as the user's mailbox — under its name or any other.
        #[tokio::test]
        async fn an_archive_dropped_unfinished_leaves_no_file_behind() {
            let dir = scratch_dir("refused");
            let mut sink: Box<dyn ArchiveFileSink> =
                Box::new(NativeArchiveFile::create(&dir, NAME).await.unwrap());
            sink.write(b"the first frames of a truncated blob")
                .await
                .unwrap();
            drop(sink);
            assert_eq!(listing(&dir), Vec::<String>::new());
            std::fs::remove_dir_all(dir).unwrap();
        }

        /// A failed re-download the same day must not truncate the good archive
        /// an earlier download saved under the same name.
        #[tokio::test]
        async fn a_refused_redownload_keeps_the_earlier_archive_intact() {
            let dir = scratch_dir("redownload");
            std::fs::write(dir.join(NAME), b"the complete earlier archive").unwrap();
            let mut sink: Box<dyn ArchiveFileSink> =
                Box::new(NativeArchiveFile::create(&dir, NAME).await.unwrap());
            sink.write(b"partial").await.unwrap();
            drop(sink);
            assert_eq!(listing(&dir), vec![NAME.to_string()]);
            assert_eq!(
                std::fs::read(dir.join(NAME)).unwrap(),
                b"the complete earlier archive"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    build_bridge_approval_machine, build_caldav_policy_machine, build_carddav_policy_machine,
    build_forwarders_machine, build_linked_nests_machine_with_mail_relay,
    build_linked_nests_machine_with_mail_relay_and_trust, build_local_domains_machine,
    build_mail_aliases_machine, build_mail_export_machine,
    build_mail_export_machine_without_key_custody, build_mail_import_machine,
    build_mail_list_members_machine, build_mail_lists_machine, build_mail_policy_machine,
    build_mail_settings_machine, build_mail_settings_machine_for_session, build_mail_spam_machine,
    build_relay_provision_machine, build_webdav_policy_machine, dispatch_bridge_approval,
    dispatch_post_claim_serving_enablement,
};

// The wasm twin of `native`, over the browser `WsRpcClient` (gloo-net
// `RpcRequester`, `type Error = WsRpcError`). `WsRpcClient` is `Rc`-based and
// `!Send`, so each seam impl uses `#[async_trait(?Send)]` and the resulting
// machines are `!Send` (fine — wasm-bindgen is single-threaded). The reply→view
// projections + `nest_error` map are identical to native; only the stored
// transport type differs. The bodies are duplicated rather than shared because a
// single generic `impl<R> …Nest` is impossible (see the module-level note).
// `libs/fauna-wasm` calls these `build_*_machine` from its `#[wasm_bindgen]`
// wrappers.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::sync::Arc;

    use super::provision_backoff;
    use async_trait::async_trait;
    use ed25519_dalek::SigningKey;
    use fauna_client_bridges::{HolderInfo, MailAccountClient, MailAdminClient, discover_holders};
    use fauna_client_config::SuccessionLedgerStore;
    use fauna_core::identity::ActorKeypair;
    use fauna_mail::imap_client;
    use fauna_mls::wrapped_blob::{
        MlsSnapshotBlob, SubmissionToken, WrappedMsekBlob, WrappedSubmissionTokenBlob,
    };
    use fauna_protocol::ByteBuf;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::bridge_routing::{
        AddListMemberReply, AddListMemberRequest, AliasControls, AliasPolicy, AliasRow,
        BatchImportListMembersReply, BatchImportListMembersRequest, CreateAccountListReply,
        CreateAccountListRequest, DeleteAccountListReply, DeleteAccountListRequest, EpochSealKey,
        FetchConfigReply, GetSpamBaselineStateReply, ImportAliasOutcome, ListAccountAliasesReply,
        ListAccountAliasesRequest, ListAccountListsReply, ListAccountListsRequest,
        ListListMembersReply, ListListMembersRequest, MailDomainRenameRow, MailDomainRow,
        MailHealthReply, ProvisionRecipientMlsPubkeyRequest, PublishSpamBaselineReply,
        PutAliasPolicyRequest, PutAuthPolicyRequest, PutImapPolicyRequest,
        PutOutboundPolicyRequest, PutSpamPolicyRequest, PutSubmissionPolicyRequest,
        ResubscribeListMemberReply, ResubscribeListMemberRequest, SpamHistoryOp,
        UnsubscribeListMemberReply, UnsubscribeListMemberRequest, UpdateAccountListReply,
        UpdateAccountListRequest,
    };
    use fauna_protocol::discovery::{
        NestInfoReply, NestInfoRequest, SetupStatusReply, SetupStatusRequest, capability,
    };
    use fauna_protocol::folders::{FoldersListReply, FoldersListRequest, KIND_FOLDERS_LIST};
    use fauna_protocol::wrapped_blob::{
        GetCaldavPortReply, GetCaldavPortRequest, GetMailServingEnabledReply,
        GetMailServingEnabledRequest, ProvisionMlsSnapshotBlobRequest, ProvisionReply,
        ProvisionWrappedMlsBlobRequest, ProvisionWrappedSubmissionTokenRequest,
        PutSpamModelOutcome, RevokeReply, RevokeWrappedMlsBlobRequest,
        RevokeWrappedSubmissionTokenRequest, ServiceUserInfo, SetMailEnabledReply,
        SetMailEnabledRequest, SetMailServingEnabledReply, SetMailServingEnabledRequest,
        SpamModelHolderCopy,
    };
    use fauna_rpc_wasm::WsRpcClient;

    use crate::admin_policy::{MailPolicyMachine, MailPolicyNest};
    use crate::aliases::{MailAliasesMachine, MailAliasesNest};
    use crate::bridge_approval::{BridgeApprovalMachine, BridgeApprovalNest};
    use crate::caldav_policy::{CaldavPolicyMachine, CaldavPolicyNest};
    use crate::carddav_policy::{CarddavPolicyMachine, CarddavPolicyNest};
    use crate::error::{NestError, SignerError, nest_error};
    use crate::export::{
        ExportArchiveDelivery, ExportChunkProgress, ExportFetchPage, ExportFormat,
        ExportMailboxCount, ExportRecord, ExportScope, ExportSeamError, ExportSessionView,
        ExportUploadAck, MailExportKeyCustody, MailExportMachine, MailExportNest,
    };
    use crate::forwarders::{ForwarderMachine, ForwarderNest};
    use crate::import::{
        ImportSourceNest, MailImportMachine, MailImportNest, MailboxCursor, RetryClock,
        SourceConnectParams, SourceMailboxView,
    };
    use crate::lists::{
        ImportResult, ListDraft, ListMembers, ListView, MailListMembersMachine,
        MailListMembersNest, MailListsMachine, MailListsNest,
    };
    use crate::local_domains::{
        DomainDmarcPolicy, LocalDomainMachine, LocalDomainNest, RoleAddressKind,
    };
    use crate::machine::{
        FetchedSpamModel, IdentitySigner, MailSettingsMachine, MailStore,
        NestClient as MailNestClient, SealedModelWriter,
    };
    use crate::spam::{MailSpamMachine, MailSpamNest, SpamHistory};
    use crate::state::MuaInstructions;
    use crate::webdav_policy::{WebdavPolicyMachine, WebdavPolicyNest};
    use fauna_client_capabilities::rpc::CapabilitiesClient;
    use fauna_core::grant_event::GrantEvent;

    // ── LocalDomainNest (admin email-domains) ───────────────────────

    struct RpcLocalDomainNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl LocalDomainNest for RpcLocalDomainNest {
        async fn list_local_domains(
            &self,
        ) -> Result<(Vec<MailDomainRow>, Vec<MailDomainRow>), NestError> {
            let reply = self.admin.list_local_domains().await.map_err(nest_error)?;
            Ok((reply.active, reply.soft_deleted_within_30d))
        }

        async fn add_local_domain(
            &self,
            domain: String,
            mta_sts_cert_mode: String,
        ) -> Result<(MailDomainRow, bool), NestError> {
            let reply = self
                .admin
                .add_local_domain(domain, mta_sts_cert_mode, None, None)
                .await
                .map_err(nest_error)?;
            Ok((reply.domain, reply.skipped))
        }

        async fn remove_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .remove_local_domain(domain)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn restore_local_domain(&self, domain: String) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .restore_local_domain(domain)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn update_local_domain_config(
            &self,
            domain: String,
            mta_sts_max_age_seconds: Option<i64>,
            mta_sts_cert_mode: Option<String>,
            spf_record: Option<String>,
            dmarc_policy: Option<DomainDmarcPolicy>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .update_local_domain_config(
                    domain,
                    mta_sts_max_age_seconds,
                    mta_sts_cert_mode,
                    spf_record,
                    dmarc_policy.map(Into::into),
                )
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn set_catch_all_actor(
            &self,
            domain: String,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .set_catch_all_actor(domain, actor_id)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn set_role_address(
            &self,
            domain: String,
            role: RoleAddressKind,
            actor_id: Option<Vec<u8>>,
        ) -> Result<MailDomainRow, NestError> {
            Ok(self
                .admin
                .set_role_address(domain, role.into(), actor_id)
                .await
                .map_err(nest_error)?
                .domain)
        }

        async fn get_primary_domain_rename_status(
            &self,
        ) -> Result<Option<MailDomainRenameRow>, NestError> {
            Ok(self
                .admin
                .get_primary_domain_rename_status()
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn start_primary_domain_rename(
            &self,
            new_primary_domain_id: Vec<u8>,
            grace_days: Option<i64>,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .start_primary_domain_rename(new_primary_domain_id, grace_days)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn complete_primary_domain_rename(
            &self,
            rename_id: Vec<u8>,
            force: bool,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .complete_primary_domain_rename(rename_id, force)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn extend_primary_domain_rename_grace(
            &self,
            rename_id: Vec<u8>,
            additional_days: i64,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .extend_primary_domain_rename_grace(rename_id, additional_days)
                .await
                .map_err(nest_error)?
                .rename)
        }

        async fn abort_primary_domain_rename(
            &self,
            rename_id: Vec<u8>,
            reason: Option<String>,
        ) -> Result<MailDomainRenameRow, NestError> {
            Ok(self
                .admin
                .abort_primary_domain_rename(rename_id, reason)
                .await
                .map_err(nest_error)?
                .rename)
        }
    }

    /// Build a [`LocalDomainMachine`] over the browser WS-RPC handle (admin
    /// email-domains list). The wasm twin of native's `build_local_domains_machine`.
    pub fn build_local_domains_machine(nest: WsRpcClient) -> LocalDomainMachine {
        LocalDomainMachine::new(Arc::new(RpcLocalDomainNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── BridgeApprovalNest (admin-bridges-pending) ──────────────────

    struct RpcBridgeApprovalNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl BridgeApprovalNest for RpcBridgeApprovalNest {
        async fn list_pending_bridges(&self) -> Result<Vec<ServiceUserInfo>, NestError> {
            Ok(self
                .admin
                .list_pending_bridges()
                .await
                .map_err(nest_error)?
                .bridges)
        }

        async fn list_service_users(
            &self,
            role: Option<String>,
            status: Option<String>,
        ) -> Result<Vec<ServiceUserInfo>, NestError> {
            Ok(self
                .admin
                .list_service_users(role, status)
                .await
                .map_err(nest_error)?
                .service_users)
        }

        async fn approve_pending_bridge(
            &self,
            ed25519_pubkey: Vec<u8>,
            role: String,
        ) -> Result<(), NestError> {
            self.admin
                .approve_pending_bridge(ed25519_pubkey, role)
                .await
                .map_err(nest_error)
        }

        async fn reject_pending_bridge(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .reject_pending_bridge(ed25519_pubkey)
                .await
                .map_err(nest_error)
        }

        async fn revoke_service_user(&self, ed25519_pubkey: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .revoke_service_user(ed25519_pubkey)
                .await
                .map_err(nest_error)
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_mail_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_caldav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_carddav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_webdav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`BridgeApprovalMachine`] over the browser WS-RPC handle
    /// (`admin-bridges-pending`). The wasm twin of native's
    /// `build_bridge_approval_machine`.
    pub fn build_bridge_approval_machine(nest: WsRpcClient) -> BridgeApprovalMachine {
        BridgeApprovalMachine::new(Arc::new(RpcBridgeApprovalNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── ForwarderNest (admin-aliases external forwarders) ───────────

    struct RpcForwarderNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl ForwarderNest for RpcForwarderNest {
        async fn list_forwarders(&self) -> Result<Vec<AliasRow>, NestError> {
            Ok(self
                .admin
                .list_forwarders()
                .await
                .map_err(nest_error)?
                .forwarders)
        }

        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            Ok(self
                .admin
                .list_local_domains()
                .await
                .map_err(nest_error)?
                .active
                .into_iter()
                .map(|d| d.domain_name)
                .collect())
        }

        async fn create_forwarder(
            &self,
            local_domain: String,
            pattern: String,
            forward_target: String,
        ) -> Result<(), NestError> {
            self.admin
                .create_forwarder(local_domain, pattern, forward_target)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn delete_forwarder(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.admin
                .delete_forwarder(alias_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }
    }

    /// Build a [`ForwarderMachine`] over the browser WS-RPC handle
    /// (`admin-aliases` external forwarders, `admin.md` § 4). The wasm twin of
    /// native's `build_forwarders_machine`.
    pub fn build_forwarders_machine(nest: WsRpcClient) -> ForwarderMachine {
        ForwarderMachine::new(Arc::new(RpcForwarderNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── MailPolicyNest (admin-mail policy form) ─────────────────────

    struct RpcMailPolicyNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl MailPolicyNest for RpcMailPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_mail_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn get_auto_enable_mail_for_new_users(&self) -> Result<bool, NestError> {
            self.admin
                .get_auto_enable_mail_for_new_users()
                .await
                .map_err(nest_error)
        }

        async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_auto_enable_mail_for_new_users(enabled)
                .await
                .map_err(nest_error)
        }

        async fn put_spam_policy(&self, req: PutSpamPolicyRequest) -> Result<(), NestError> {
            self.admin.put_spam_policy(req).await.map_err(nest_error)
        }

        async fn put_auth_policy(&self, req: PutAuthPolicyRequest) -> Result<(), NestError> {
            self.admin.put_auth_policy(req).await.map_err(nest_error)
        }

        async fn put_submission_policy(
            &self,
            req: PutSubmissionPolicyRequest,
        ) -> Result<(), NestError> {
            self.admin
                .put_submission_policy(req)
                .await
                .map_err(nest_error)
        }

        async fn put_imap_policy(&self, req: PutImapPolicyRequest) -> Result<(), NestError> {
            self.admin.put_imap_policy(req).await.map_err(nest_error)
        }

        async fn put_outbound_policy(
            &self,
            req: PutOutboundPolicyRequest,
        ) -> Result<(), NestError> {
            self.admin
                .put_outbound_policy(req)
                .await
                .map_err(nest_error)
        }

        async fn get_alias_policy(&self) -> Result<AliasPolicy, NestError> {
            self.admin.get_alias_policy().await.map_err(nest_error)
        }

        async fn put_alias_policy(&self, req: PutAliasPolicyRequest) -> Result<(), NestError> {
            self.admin.put_alias_policy(req).await.map_err(nest_error)
        }

        async fn get_spam_baseline_state(&self) -> Result<GetSpamBaselineStateReply, NestError> {
            self.admin
                .get_spam_baseline_state()
                .await
                .map_err(nest_error)
        }

        async fn publish_spam_baseline(&self) -> Result<PublishSpamBaselineReply, NestError> {
            self.admin.publish_spam_baseline().await.map_err(nest_error)
        }

        async fn mail_health(&self) -> Result<MailHealthReply, NestError> {
            self.admin.mail_health().await.map_err(nest_error)
        }

        async fn blocklist_self_check_run(&self) -> Result<(), NestError> {
            self.admin
                .blocklist_self_check_run()
                .await
                .map(drop)
                .map_err(nest_error)
        }

        async fn run_deliverability_diagnostics(&self) -> Result<(), NestError> {
            self.admin
                .run_deliverability_diagnostics()
                .await
                .map(drop)
                .map_err(nest_error)
        }

        async fn outbound_warmup_reset(&self) -> Result<(), NestError> {
            self.admin
                .outbound_warmup_reset()
                .await
                .map(drop)
                .map_err(nest_error)
        }
    }

    /// Build a [`MailPolicyMachine`] over the browser WS-RPC handle (the flat
    /// `admin-mail` policy form, `admin.md` § Mail). The wasm twin of native's
    /// `build_mail_policy_machine`.
    pub fn build_mail_policy_machine(nest: WsRpcClient) -> MailPolicyMachine {
        MailPolicyMachine::new(Arc::new(RpcMailPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── CaldavPolicyNest (admin-calendar enable toggle) ─────────────

    struct RpcCaldavPolicyNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl CaldavPolicyNest for RpcCaldavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_caldav_enabled(enabled)
                .await
                .map_err(nest_error)
        }

        async fn set_caldav_port(&self, port: u16) -> Result<(), NestError> {
            self.admin.set_caldav_port(port).await.map_err(nest_error)
        }
    }

    /// Build a [`CaldavPolicyMachine`] over the browser WS-RPC handle (the flat
    /// `admin-calendar` CalDAV-enable toggle, `admin.md` § 8 Calendar). The wasm
    /// twin of native's `build_caldav_policy_machine`.
    pub fn build_caldav_policy_machine(nest: WsRpcClient) -> CaldavPolicyMachine {
        CaldavPolicyMachine::new(Arc::new(RpcCaldavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── CarddavPolicyNest (admin-contacts enable toggle) ────────────

    struct RpcCarddavPolicyNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl CarddavPolicyNest for RpcCarddavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_carddav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_carddav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`CarddavPolicyMachine`] over the browser WS-RPC handle (the flat
    /// `admin-contacts` CardDAV-enable toggle, `admin.md` § Contacts). The wasm
    /// twin of native's `build_carddav_policy_machine`.
    pub fn build_carddav_policy_machine(nest: WsRpcClient) -> CarddavPolicyMachine {
        CarddavPolicyMachine::new(Arc::new(RpcCarddavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── WebdavPolicyNest (admin-files enable toggle) ────────────────

    struct RpcWebdavPolicyNest {
        admin: MailAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl WebdavPolicyNest for RpcWebdavPolicyNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            self.admin.get_mail_config().await.map_err(nest_error)
        }

        async fn set_webdav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            self.admin
                .set_webdav_enabled(enabled)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`WebdavPolicyMachine`] over the browser WS-RPC handle (the flat
    /// `admin-files` WebDAV-enable toggle, `admin.md` § Files). The wasm twin of
    /// native's `build_webdav_policy_machine`.
    pub fn build_webdav_policy_machine(nest: WsRpcClient) -> WebdavPolicyMachine {
        WebdavPolicyMachine::new(Arc::new(RpcWebdavPolicyNest {
            admin: MailAdminClient::new(nest),
        }))
    }

    // ── MailAliasesNest (user-tier mail-aliases page) ───────────────

    struct RpcMailAliasesNest {
        account: MailAccountClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl MailAliasesNest for RpcMailAliasesNest {
        async fn list_account_aliases(&self) -> Result<Vec<AliasRow>, NestError> {
            self.account
                .list_account_aliases()
                .await
                .map_err(nest_error)
        }

        async fn create_account_alias(
            &self,
            kind: String,
            local_domain: String,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            self.account
                .create_account_alias(kind, local_domain, pattern, controls)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn update_account_alias(
            &self,
            alias_id: Vec<u8>,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            self.account
                .update_account_alias(alias_id, pattern, controls)
                .await
                .map_err(nest_error)
        }

        async fn revoke_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .revoke_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn enable_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .enable_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn delete_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            self.account
                .delete_account_alias(alias_id)
                .await
                .map_err(nest_error)
        }

        async fn generate_disposable_alias(
            &self,
            ttl_days: Option<u32>,
            uses: Option<u32>,
            label: String,
        ) -> Result<String, NestError> {
            Ok(self
                .account
                .generate_disposable_alias(ttl_days, uses, label)
                .await
                .map_err(nest_error)?
                .full_address)
        }

        async fn import_account_aliases(
            &self,
            lines: Vec<String>,
        ) -> Result<Vec<ImportAliasOutcome>, NestError> {
            self.account
                .import_account_aliases(lines)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`MailAliasesMachine`] over the browser WS-RPC handle (the user
    /// `mail-aliases` page). The wasm twin of native's `build_mail_aliases_machine`.
    pub fn build_mail_aliases_machine(nest: WsRpcClient) -> MailAliasesMachine {
        MailAliasesMachine::new(Arc::new(RpcMailAliasesNest {
            account: MailAccountClient::new(nest),
        }))
    }

    // ── MailSpamNest (user-tier mail-spam page) ─────────────────────
    //
    // wasm twin of the native seam: wraps the user-tier `MailAccountClient` over
    // the four live caller-scoped spam RPCs; the wire→view projection is the
    // shared `super::project_spam_history`. `libs/fauna-wasm` calls
    // `build_mail_spam_machine` from its `#[wasm_bindgen]` wrapper.

    struct RpcMailSpamNest {
        account: MailAccountClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl MailSpamNest for RpcMailSpamNest {
        async fn list_spam_training_history(&self) -> Result<SpamHistory, NestError> {
            let reply = self
                .account
                .list_spam_training_history()
                .await
                .map_err(nest_error)?;
            Ok(super::project_spam_history(reply))
        }
        async fn reset_spam_model(&self) -> Result<(), NestError> {
            self.account.reset_spam_model().await.map_err(nest_error)
        }
        async fn set_baseline_contribution(&self, contribute: bool) -> Result<(), NestError> {
            self.account
                .set_baseline_contribution(contribute)
                .await
                .map_err(nest_error)
        }
    }

    /// Build a [`MailSpamMachine`] over the browser `WsRpcClient` (`mail-spam`
    /// page). The wasm twin of native's `build_mail_spam_machine` — carries a
    /// [`MailSettingsMachine`] [`SealedModelWriter`] (same `keypair` ⇒ same MSEK) so
    /// a client-written (sealed) row's undo runs the reseal loop client-side.
    /// `ledger`: the contribute toggle's baseline grant records through it.
    pub fn build_mail_spam_machine(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        ledger: Arc<dyn SuccessionLedgerStore>,
    ) -> MailSpamMachine {
        let writer: Arc<dyn SealedModelWriter> = Arc::new(build_mail_settings_machine(
            nest.clone(),
            keypair,
            mail,
            node_url,
            ledger,
            // A writer / key custody only: it never hydrates the served state.
            None,
        ));
        MailSpamMachine::new(
            Arc::new(RpcMailSpamNest {
                account: MailAccountClient::new(nest),
            }),
            Some(writer),
        )
    }

    // ── MailExportNest (user-tier mail-export wizard) ───────────────────
    //
    // The twelve User-class kinds are live nest-side (`bridge_export_handlers.rs`,
    // 2026-09-21, `fail_export_session` 2026-09-22), so this seam is real: a thin projection over the shared
    // `fauna_mail::export::client::MailExportClient`, plus the one piece of
    // platform transport the loop must not see — resolving an over-frame
    // record's `body_ref` off the bulk-byte plane. Key custody is the
    // `MailSettingsMachine` built from the same keypair (§ Key material), the
    // composition `build_mail_spam_machine` already uses for its writer.

    struct RpcMailExportNest {
        client: fauna_mail::export::client::MailExportClient<WsRpcClient>,
        nest: WsRpcClient,
    }

    #[async_trait(?Send)]
    impl MailExportNest for RpcMailExportNest {
        async fn list_own_mailboxes(&self) -> Result<Vec<ExportMailboxCount>, NestError> {
            Ok(self
                .client
                .list_own_mailboxes()
                .await
                .map_err(nest_error)?
                .mailboxes
                .into_iter()
                .map(|m| ExportMailboxCount {
                    name: m.name,
                    exists: m.exists,
                    uid_validity: m.uid_validity,
                })
                .collect())
        }
        async fn list_export_sessions(&self) -> Result<Vec<ExportSessionView>, NestError> {
            Ok(self
                .client
                .list_export_sessions()
                .await
                .map_err(nest_error)?
                .into_iter()
                .filter_map(super::project_export_session)
                .collect())
        }
        async fn start_export_session(
            &self,
            format: ExportFormat,
            scope: ExportScope,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, NestError> {
            let reply = self
                .client
                .start_export_session(
                    format.wire_name(),
                    super::encode_export_scope(&scope)?,
                    wrapped_session_key,
                    total_count,
                )
                .await
                .map_err(nest_error)?;
            Ok(super::fresh_export_session(
                reply.session_id,
                format,
                total_count,
                scope,
            ))
        }
        async fn fetch_export_chunk_ciphertext(
            &self,
            session_id: String,
            mailbox: String,
            after_uid: u32,
        ) -> Result<ExportFetchPage, NestError> {
            let reply = self
                .client
                .fetch_export_chunk_ciphertext(session_id, mailbox, after_uid)
                .await
                .map_err(nest_error)?;
            let mut records = Vec::with_capacity(reply.messages.len());
            for m in reply.messages {
                // An over-frame record crosses by reference; fetch it back into
                // the exact sealed bytes before it reaches the loop. A resolve
                // failure is a fetch failure — transient by construction — so
                // it surfaces as one, and the loop fails the session rather
                // than dropping the message (§ An unopenable record fails the
                // session).
                let sealed_body = match &m.body_ref {
                    Some(r) => fauna_mail::body_ref::resolve_referenced_mail_body(
                        &fauna_core::file_download::WasmPublicChunkFetcher::new(
                            self.nest.nest_url().trim_end_matches('/'),
                        ),
                        &r.chunk_hashes,
                        r.total_bytes,
                    )
                    .await
                    .map_err(|e| {
                        NestError::Transient(format!(
                            "resolve referenced body ({} uid {}): {e}",
                            m.mailbox, m.uid
                        ))
                    })?,
                    None => m.sealed_body,
                };
                records.push(ExportRecord {
                    mailbox: m.mailbox,
                    uid: m.uid,
                    flags: m.flags,
                    internal_date: m.internal_date,
                    stored_at: m.stored_at,
                    sealed_body,
                });
            }
            Ok(ExportFetchPage {
                records,
                next_after_uid: reply.next_after_uid,
                mailbox_done: reply.mailbox_done,
            })
        }
        async fn upload_export_chunk(
            &self,
            session_id: String,
            stream_generation: u64,
            chunk_idx: u64,
            sealed_chunk: Vec<u8>,
            progress: ExportChunkProgress,
        ) -> Result<ExportUploadAck, ExportSeamError> {
            let reply = self
                .client
                .upload_export_chunk(
                    session_id,
                    stream_generation,
                    chunk_idx,
                    sealed_chunk,
                    progress.exported_delta,
                    0,
                    0,
                    progress.last_processed_message_id,
                    progress.revised_total_count,
                )
                .await
                .map_err(super::export_seam_error)?;
            Ok(ExportUploadAck {
                blob_bytes: reply.blob_bytes,
                next_chunk_idx: reply.next_chunk_idx,
                exported_count: reply.exported_count,
            })
        }
        async fn pause_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .pause_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn resume_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .resume_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn restart_export_session(
            &self,
            session_id: String,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .restart_export_session(session_id, wrapped_session_key, total_count)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn cancel_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            self.client
                .cancel_export_session(session_id, as_driver_of)
                .await
                .map_err(super::export_seam_error)?;
            Ok(())
        }
        async fn finalize_export_session(
            &self,
            session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            Ok(super::project_export_session_required(
                self.client
                    .finalize_export_session(session_id, as_driver_of)
                    .await
                    .map_err(super::export_seam_error)?,
            )?)
        }
        async fn fail_export_session(
            &self,
            session_id: String,
            reason: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            self.client
                .fail_export_session(session_id, reason, as_driver_of)
                .await
                .map_err(super::export_seam_error)?;
            Ok(())
        }
        async fn discard_export_blob(&self, session_id: String) -> Result<(), NestError> {
            // `existed = false` is the second discard of the same session —
            // idempotent from the client's side, deliberately not an error.
            self.client
                .discard_export_blob(session_id)
                .await
                .map_err(nest_error)?;
            Ok(())
        }
    }

    /// Build a [`MailExportMachine`] over a browser WS-RPC handle
    /// (`mail-export` page). `keypair` + `node_url` build the
    /// [`MailSettingsMachine`] that is the run's key custody (same keypair ⇒
    /// same MSEK ⇒ the export opens exactly what the inbox opens); `handle`
    /// names the archive's root directory.
    ///
    /// `delivery` is § Download flow's platform half, and on web it is passed
    /// in rather than built here: both of its ends are browser API (a streamed
    /// `fetch`, and a temporary file handed to the browser's own download),
    /// which lives with the `wasm-bindgen` glue in `fauna-wasm`
    /// (`mail_export_delivery.rs`), not in this crate.
    pub fn build_mail_export_machine(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        handle: &str,
        delivery: Arc<dyn ExportArchiveDelivery>,
    ) -> MailExportMachine {
        let keys: Arc<dyn MailExportKeyCustody> = Arc::new(build_mail_settings_machine(
            nest.clone(),
            keypair,
            mail,
            node_url,
            // Key custody only (the MSEK) — this machine never touches the ledger.
            Arc::new(fauna_client_config::NoLedgerStore),
            // A writer / key custody only: it never hydrates the served state.
            None,
        ));
        MailExportMachine::new(
            Arc::new(RpcMailExportNest {
                client: fauna_mail::export::client::MailExportClient::new(nest.clone()),
                nest,
            }),
            keys,
            delivery,
            handle,
        )
    }

    // ── MailImportNest (real) / ImportSourceNest (stub) — mail-import wizard ─
    //
    // wasm twin of native's `MailImportNest` (the nest half is real here too —
    // `fauna_mail::imap_client::MailImportClient` is wasm-clean, same
    // `imap-client` feature). `ImportSourceNest` stays a stub: the web IMAP
    // transport (sans-io `rustls` over a blind byte relay) is user-gated +
    // POSTPONED (`mailbox-migration.md` § Where the IMAP client runs).
    // Here it is the *transport* that is missing, not the nest backend.

    struct RpcMailImportNest {
        client: imap_client::MailImportClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl MailImportNest for RpcMailImportNest {
        async fn list_sessions(&self) -> Result<Vec<crate::import::ImportSessionView>, NestError> {
            Ok(self
                .client
                .list_sessions()
                .await
                .map_err(nest_error)?
                .into_iter()
                .map(|info| super::project_import_session(info, None))
                .collect())
        }

        async fn start_session(
            &self,
            source_descriptor: String,
            total_count: u64,
            scope: Vec<String>,
            date_from: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            let reply = self
                .client
                // No seal: this arm has no keypair to derive the owner root
                // from, and its `Connect` (starting a fresh import) is a stub
                // anyway — the § note above. Sealless is the ratified
                // keyless degrade, not a silent loss.
                .start_session(
                    source_descriptor.clone(),
                    total_count,
                    scope.clone(),
                    date_from.clone(),
                    None,
                )
                .await
                .map_err(nest_error)?;
            Ok(super::fresh_import_session(
                reply.session_id,
                source_descriptor,
                total_count,
                scope,
                date_from,
            ))
        }

        async fn pause_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .pause_session(session_id)
                    .await
                    .map_err(nest_error)?,
                None,
            ))
        }

        async fn resume_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .resume_session(session_id)
                    .await
                    .map_err(nest_error)?,
                None,
            ))
        }

        async fn cancel_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .cancel_session(session_id)
                    .await
                    .map_err(nest_error)?,
                None,
            ))
        }

        async fn finalize_session(
            &self,
            session_id: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .finalize_session(session_id)
                    .await
                    .map_err(nest_error)?,
                None,
            ))
        }

        async fn fail_session(
            &self,
            session_id: String,
            reason: String,
        ) -> Result<crate::import::ImportSessionView, NestError> {
            Ok(super::project_import_session(
                self.client
                    .fail_session(session_id, reason)
                    .await
                    .map_err(nest_error)?,
                None,
            ))
        }

        async fn send_unit(
            &self,
            session_id: String,
            unit: imap_client::ImportUnit,
            skip_dedup: bool,
            revised_total_count: Option<u64>,
        ) -> Result<crate::import::SendUnitReply, NestError> {
            Ok(super::project_import_unit_reply(
                self.client
                    .send_unit(session_id, unit, skip_dedup, revised_total_count)
                    .await
                    .map_err(nest_error)?,
            ))
        }
    }

    struct RpcImportSourceNest;

    #[async_trait(?Send)]
    impl ImportSourceNest for RpcImportSourceNest {
        async fn connect(&self, _params: SourceConnectParams) -> Result<(), NestError> {
            Err(super::import_source_unimpl("(web IMAP transport) connect"))
        }
        async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
            Err(super::import_source_unimpl(
                "(web IMAP transport) list_source_mailboxes",
            ))
        }
        async fn examine(
            &self,
            _mailbox: &str,
            _cursor: Option<MailboxCursor>,
        ) -> Result<imap_client::MailboxStatus, NestError> {
            Err(super::import_source_unimpl("(web IMAP transport) examine"))
        }
        async fn fetch_window(
            &self,
            _mailbox: &str,
            _cursor: Option<MailboxCursor>,
            _max: usize,
        ) -> Result<Vec<imap_client::FetchOutcome>, NestError> {
            Err(super::import_source_unimpl(
                "(web IMAP transport) fetch_window",
            ))
        }
        async fn logout(&self) -> Result<(), NestError> {
            Err(super::import_source_unimpl("(web IMAP transport) logout"))
        }

        // The web relay transport is unbuilt, not merely down — `connect` can
        // never succeed on this platform (§ Where the IMAP client runs), so
        // `MailImportMachine::connect` clears the password on failure instead
        // of retaining it for a retry that can never happen (
        // `mailbox-migration.md` § Credential handling).
        fn can_ever_connect(&self) -> bool {
            false
        }
    }

    struct RpcRetryClock;

    #[async_trait(?Send)]
    impl RetryClock for RpcRetryClock {
        async fn sleep_ms(&self, ms: u64) {
            fauna_sleep::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    /// Build a [`MailImportMachine`] over the browser `WsRpcClient`. The nest
    /// half is real; the source half is a stub (see the section note above) —
    /// `hydrate`/`list_sessions` (viewing a session a native app started) and
    /// resume/pause/cancel on an existing session still work, but `Connect`
    /// (starting a fresh import) surfaces the stub's `error-message`.
    pub fn build_mail_import_machine(nest: WsRpcClient) -> MailImportMachine {
        MailImportMachine::new(
            Arc::new(RpcMailImportNest {
                client: imap_client::MailImportClient::new(nest),
            }),
            Arc::new(RpcImportSourceNest),
            Arc::new(RpcRetryClock),
        )
    }

    // ── MailLists / MailListMembers (user-tier mailing lists) ───────────
    //
    // wasm twin of the native seams — see the native module's note for why
    // every kind except `create_account_list` rides `request_idempotent`
    // directly over `self.nest` rather than the unretried `MailAccountClient`.
    // Bodies are otherwise identical to native (including the shared
    // `lists::{project_list_row, project_member_row, derive_list_domains}`
    // projections); only the stored transport type differs.

    struct RpcMailListsNest {
        nest: WsRpcClient,
    }

    #[async_trait(?Send)]
    impl MailListsNest for RpcMailListsNest {
        async fn list_account_lists(&self) -> Result<Vec<ListView>, NestError> {
            let reply: ListAccountListsReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
                provision_backoff,
            )
            .await?;
            Ok(reply
                .lists
                .into_iter()
                .map(crate::lists::project_list_row)
                .collect())
        }

        /// User-tier by construction — see the native twin's note.
        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            let lists: ListAccountListsReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.list_account_lists",
                ListAccountListsRequest {},
                provision_backoff,
            )
            .await?;
            let list_domains: Vec<String> =
                lists.lists.into_iter().map(|r| r.local_domain).collect();
            let aliases: ListAccountAliasesReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.list_account_aliases",
                ListAccountAliasesRequest {},
                provision_backoff,
            )
            .await?;
            let alias_domains: Vec<String> = aliases
                .aliases
                .into_iter()
                .map(|r| r.local_domain)
                .collect();
            Ok(crate::lists::derive_list_domains(
                &list_domains,
                &alias_domains,
            ))
        }

        async fn create_account_list(&self, draft: ListDraft) -> Result<(), NestError> {
            // NOT retried — see the native twin's note (`forbid_replay: true`).
            let _: CreateAccountListReply = self
                .nest
                .request(
                    "fauna.bridges.create_account_list",
                    CreateAccountListRequest {
                        local_part: draft.local_part,
                        local_domain: draft.local_domain,
                        friendly_name: super::opt_text(draft.friendly_name),
                        description: super::opt_text(draft.description),
                        list_help_url: super::opt_text(draft.list_help_url),
                        list_archive_url: super::opt_text(draft.list_archive_url),
                        recipients_per_send: draft.recipients_per_send.map(i64::from),
                    },
                )
                .await
                .map_err(nest_error)?;
            Ok(())
        }

        async fn update_account_list(
            &self,
            list_id: Vec<u8>,
            draft: ListDraft,
        ) -> Result<(), NestError> {
            let _: UpdateAccountListReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.update_account_list",
                UpdateAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                    friendly_name: super::opt_text(draft.friendly_name),
                    description: super::opt_text(draft.description),
                    list_help_url: super::opt_text(draft.list_help_url),
                    list_archive_url: super::opt_text(draft.list_archive_url),
                    recipients_per_send: draft.recipients_per_send.map(i64::from),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn delete_account_list(&self, list_id: Vec<u8>) -> Result<(), NestError> {
            let _: DeleteAccountListReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.delete_account_list",
                DeleteAccountListRequest {
                    list_id: ByteBuf::from(list_id),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }
    }

    /// Build a [`MailListsMachine`] over the browser `WsRpcClient` (wasm twin of
    /// native's `build_mail_lists_machine`).
    pub fn build_mail_lists_machine(nest: WsRpcClient) -> MailListsMachine {
        MailListsMachine::new(Arc::new(RpcMailListsNest { nest }))
    }

    struct RpcMailListMembersNest {
        nest: WsRpcClient,
    }

    #[async_trait(?Send)]
    impl MailListMembersNest for RpcMailListMembersNest {
        async fn list_list_members(&self, list_id: Vec<u8>) -> Result<ListMembers, NestError> {
            let reply: ListListMembersReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.list_list_members",
                ListListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    include_unsubscribed: true,
                },
                provision_backoff,
            )
            .await?;
            Ok(ListMembers {
                members: reply
                    .members
                    .into_iter()
                    .map(crate::lists::project_member_row)
                    .collect(),
                subscribed_count: reply.subscribed_count.max(0) as u32,
                unsubscribed_count: reply.unsubscribed_count.max(0) as u32,
            })
        }

        async fn add_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: AddListMemberReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.add_list_member",
                AddListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn batch_import_list_members(
            &self,
            list_id: Vec<u8>,
            addresses: Vec<String>,
        ) -> Result<ImportResult, NestError> {
            let tally: BatchImportListMembersReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.batch_import_list_members",
                BatchImportListMembersRequest {
                    list_id: ByteBuf::from(list_id),
                    addresses,
                },
                provision_backoff,
            )
            .await?;
            Ok(ImportResult {
                added: tally.added,
                skipped_invalid: tally.skipped_invalid,
                skipped_duplicate: tally.skipped_duplicate,
            })
        }

        async fn unsubscribe_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: UnsubscribeListMemberReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.unsubscribe_list_member",
                UnsubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn resubscribe_list_member(
            &self,
            list_id: Vec<u8>,
            address: String,
        ) -> Result<(), NestError> {
            let _: ResubscribeListMemberReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.resubscribe_list_member",
                ResubscribeListMemberRequest {
                    list_id: ByteBuf::from(list_id),
                    recipient_address: address,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }
    }

    /// Build a [`MailListMembersMachine`] for `list_id_hex`. Errors only on a
    /// malformed `list_id_hex`.
    pub fn build_mail_list_members_machine(
        nest: WsRpcClient,
        list_id_hex: String,
        list_name: String,
    ) -> Result<MailListMembersMachine, crate::error::DispatchError> {
        MailListMembersMachine::new(
            Arc::new(RpcMailListMembersNest { nest }),
            list_id_hex,
            list_name,
        )
    }

    // ── MailSettings seams (mail-credentials page) ──────────────────
    //
    // The wasm twin of native's mail-settings seams, over the browser
    // `WsRpcClient` (`Rc`-based, `!Send` → `#[async_trait(?Send)]`, machines are
    // `!Send`). Bodies are identical to native; only the stored transport type
    // differs (a single generic `impl<R>` is impossible — see the module note).

    /// The machine's `NestClient` over the browser WS-RPC handle. Wasm twin of
    /// native's `RpcMailNestClient`.
    struct RpcMailNestClient {
        nest: WsRpcClient,
        /// The owner's folder-key custody the served state is read from.
        folder_keys: super::FolderKeys,
    }

    impl RpcMailNestClient {
        fn map_encode(e: impl std::fmt::Display) -> NestError {
            NestError::Rejected(format!("encode wrapped blob: {e}"))
        }
    }

    #[async_trait(?Send)]
    impl MailNestClient for RpcMailNestClient {
        async fn provision_wrapped_mls_blob(&self, blob: WrappedMsekBlob) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.provision_wrapped_mls_blob",
                ProvisionWrappedMlsBlobRequest {
                    blob: ByteBuf::from(bytes),
                    // index = (actor_id, credential_id); nest keys the row on it.
                    credential_id: blob.index.1.clone(),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_mls_snapshot_blob(
            &self,
            blob: MlsSnapshotBlob,
        ) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.provision_mls_snapshot_blob",
                ProvisionMlsSnapshotBlobRequest {
                    blob: ByteBuf::from(bytes),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_wrapped_submission_token(
            &self,
            blob: WrappedSubmissionTokenBlob,
        ) -> Result<(), NestError> {
            let bytes = blob.to_canonical_bytes().map_err(Self::map_encode)?;
            let _: ProvisionReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.provision_wrapped_submission_token",
                ProvisionWrappedSubmissionTokenRequest {
                    blob: ByteBuf::from(bytes),
                    // index = (actor_id, credential_id); nest keys the row on it.
                    credential_id: blob.index.1.clone(),
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn revoke_wrapped_mls_blob(
            &self,
            actor_id: [u8; 32],
            credential_id: String,
        ) -> Result<(), NestError> {
            let _: RevokeReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.revoke_wrapped_mls_blob",
                RevokeWrappedMlsBlobRequest {
                    actor_id: actor_id.to_vec(),
                    credential_id,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn revoke_wrapped_submission_token(
            &self,
            actor_id: [u8; 32],
            credential_id: String,
        ) -> Result<(), NestError> {
            let _: RevokeReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.revoke_wrapped_submission_token",
                RevokeWrappedSubmissionTokenRequest {
                    actor_id: actor_id.to_vec(),
                    credential_id,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn provision_recipient_mls_pubkey(
            &self,
            actor_id: [u8; 32],
            pubkey: [u8; 32],
            mlkem_ek: Vec<u8>,
            epoch_keys: Option<Vec<EpochSealKey>>,
        ) -> Result<(), NestError> {
            let _: ProvisionReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.provision_recipient_mls_pubkey",
                ProvisionRecipientMlsPubkeyRequest {
                    actor_id: ByteBuf::from(actor_id.to_vec()),
                    mls_pubkey: ByteBuf::from(pubkey.to_vec()),
                    // The post-quantum ML-KEM ek (1184 B) and the
                    // content-sealing-epoch schedule — the machine always
                    // publishes both.
                    mlkem_ek: ByteBuf::from(mlkem_ek),
                    epoch_keys,
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn nest_supports(&self, token: &str) -> Result<bool, NestError> {
            let reply: NestInfoReply = super::request_idempotent(
                &self.nest,
                "fauna.nest.info",
                NestInfoRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(capability::supports(&reply.capabilities, token))
        }

        async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
            let _: SetMailEnabledReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.set_mail_enabled",
                SetMailEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn set_mail_serving_enabled(&self, enabled: bool) -> Result<(), NestError> {
            let _: SetMailServingEnabledReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.set_mail_serving_enabled",
                SetMailServingEnabledRequest {
                    enabled,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(())
        }

        async fn get_mail_serving_enabled(&self) -> Result<bool, NestError> {
            // Caller-scoped read: empty `actor_id` ⇒ the nest forces a `User`
            // caller to its own flag.
            let reply: GetMailServingEnabledReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.get_mail_serving_enabled",
                GetMailServingEnabledRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(reply.enabled)
        }

        async fn get_caldav_port(&self) -> Result<u16, NestError> {
            // Nest-wide singleton (not caller-scoped); the request is empty.
            let reply: GetCaldavPortReply = super::request_idempotent(
                &self.nest,
                "fauna.bridges.get_caldav_port",
                GetCaldavPortRequest::default(),
                provision_backoff,
            )
            .await?;
            Ok(reply.port)
        }

        async fn serves_any_webdav_set(&self) -> Result<bool, NestError> {
            // Owner-scoped enumeration (the historic projection — never opt into
            // the member-visible union: a set *shared with* me is served by its
            // owner's blob, not mine).
            let reply: FoldersListReply = super::request_idempotent(
                &self.nest,
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
                provision_backoff,
            )
            .await?;
            Ok(super::custody_serves_any(reply.folders, &self.folder_keys).await)
        }

        async fn fetch_spam_model(
            &self,
            actor_id: [u8; 32],
        ) -> Result<FetchedSpamModel, NestError> {
            MailAccountClient::new(self.nest.clone())
                .fetch_spam_model(actor_id.to_vec())
                .await
                .map(|reply| FetchedSpamModel {
                    blob: reply.blob.map(|b| b.into_vec()),
                    stored_sealed: reply.stored_sealed,
                    contribute_baseline: reply.contribute_baseline,
                    holder_seal_target: reply.holder_seal_target,
                })
                .map_err(|e| NestError::Rejected(format!("fetch_spam_model: {e}")))
        }

        async fn put_spam_model(
            &self,
            sealed_model: Vec<u8>,
            sample_count: u32,
            history_op: Option<SpamHistoryOp>,
            holder_copy: Option<SpamModelHolderCopy>,
        ) -> Result<PutSpamModelOutcome, NestError> {
            MailAccountClient::new(self.nest.clone())
                .put_spam_model(sealed_model, sample_count, history_op, holder_copy)
                .await
                .map_err(|e| NestError::Rejected(format!("put_spam_model: {e}")))
        }

        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), NestError> {
            CapabilitiesClient::new(self.nest.clone())
                .mint(grant_blob)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), NestError> {
            CapabilitiesClient::new(self.nest.clone())
                .revoke(grant_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn renew_grant(
            &self,
            grant_id: [u8; 16],
            new_epoch_start: u64,
            new_epoch_end: u64,
            appended_keys: Vec<Vec<u8>>,
        ) -> Result<(), NestError> {
            CapabilitiesClient::new(self.nest.clone())
                .renew(grant_id, new_epoch_start, new_epoch_end, appended_keys)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, NestError> {
            let admin = MailAdminClient::new(self.nest.clone());
            discover_holders(&admin).await
        }

        async fn fetch_post_body_text(
            &self,
            content_id: &str,
        ) -> Result<Option<String>, NestError> {
            super::fetch_post_body_text(self.nest.clone(), content_id).await
        }

        async fn moderation_train(&self, content_id: &str, verdict: &str) -> Result<(), NestError> {
            super::moderation_train(self.nest.clone(), content_id, verdict).await
        }
    }

    /// Signs `SubmissionToken`s + capability `GrantEvent`s with the actor's Ed25519
    /// key. Wasm twin of native's `RpcIdentitySigner`.
    struct RpcIdentitySigner {
        signing_key: SigningKey,
    }

    impl IdentitySigner for RpcIdentitySigner {
        fn sign_submission_token(
            &self,
            token: SubmissionToken,
        ) -> Result<SubmissionToken, SignerError> {
            token
                .sign(&self.signing_key)
                .map_err(|e| SignerError::Sign(e.to_string()))
        }
        fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, SignerError> {
            event
                .sign(&self.signing_key)
                .map_err(|e| SignerError::GrantEventSign(e.to_string()))
        }
    }

    /// Build a [`MailSettingsMachine`] over the browser WS-RPC handle. The wasm
    /// twin of native's `build_mail_settings_machine`.
    pub fn build_mail_settings_machine(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: &str,
        ledger: Arc<dyn SuccessionLedgerStore>,
        folder_keys: super::FolderKeys,
    ) -> MailSettingsMachine {
        let actor_id = keypair.actor_id().0;
        let signing_key = keypair.signing_key().clone();
        let nest_seam: Arc<dyn MailNestClient> = Arc::new(RpcMailNestClient { nest, folder_keys });
        let signer: Arc<dyn IdentitySigner> = Arc::new(RpcIdentitySigner { signing_key });
        let mua = MuaInstructions::for_node_url(node_url);

        MailSettingsMachine::new(actor_id, nest_seam, ledger, mail, signer, mua)
    }

    /// The wasm twin of the native `dispatch_post_claim_serving_enablement` —
    /// the web app's `LoggedIn` handoff runs the identical
    /// [`crate::serving_enablement::apply_serving_enablement`] over the browser
    /// transport, so the four intents fire, and publish their anchor, exactly as
    /// on the native apps.
    pub async fn dispatch_post_claim_serving_enablement(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        mail: Arc<dyn MailStore>,
        node_url: String,
        ledger: Arc<dyn SuccessionLedgerStore>,
        intents: crate::serving_enablement::ServingEnablementIntents,
    ) {
        let actor_id_hex = keypair.actor_id_hex();
        let secret = zeroize::Zeroizing::new(*keypair.secret_bytes());
        let mail_nest = nest.clone();
        let executor = crate::serving_enablement::RpcServingEnablement {
            nest: nest.clone(),
            build_mail_machine: move || {
                build_mail_settings_machine(
                    mail_nest.clone(),
                    ActorKeypair::from_secret(*secret),
                    Arc::clone(&mail),
                    &node_url,
                    Arc::clone(&ledger),
                    // Serving enablement never reads the served state.
                    None,
                )
            },
            bridges: build_bridge_approval_machine(nest),
        };
        crate::serving_enablement::apply_serving_enablement(actor_id_hex, intents, &executor).await
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    build_bridge_approval_machine, build_caldav_policy_machine, build_carddav_policy_machine,
    build_forwarders_machine, build_local_domains_machine, build_mail_aliases_machine,
    build_mail_export_machine, build_mail_import_machine, build_mail_list_members_machine,
    build_mail_lists_machine, build_mail_policy_machine, build_mail_settings_machine,
    build_mail_spam_machine, build_webdav_policy_machine, dispatch_post_claim_serving_enablement,
};

/// Fault-injection coverage for [`request_idempotent`] — the regression guard
/// for the user-reported enable-mail freeze against a flaky remote nest
/// (example.com). A real GUI + WS-proxy reproduction would be timing-racy (the
/// drop has to coincide with a provision in flight) and is gated on the shared
/// AT-SPI bus; a scripted fake transport reproduces the *exact* failure shape —
/// a mid-flight disconnect mapped to [`NestError::Transient`] — deterministically
/// and in-process. The full-stack happy path is covered by the tier_3
/// `tests/e2e-unified/.../test_mail_enable_then_mua_round_trip.py`.
#[cfg(test)]
mod tests {
    use super::{MAIL_PROVISION_MAX_ATTEMPTS, request_idempotent};
    use crate::error::NestError;
    use fauna_protocol::{RpcErrorClass, RpcRequester};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    /// Scripted outcome for one `request` call — the fault injector's program.
    #[derive(Clone, Copy)]
    enum Outcome {
        /// The provision lands (server replied).
        Ok,
        /// A transport fault (mid-flight WS drop / timeout): `is_rejection()`
        /// is false, so `nest_error` maps it to `Transient` and the helper
        /// retries — the exact class of the example.com disconnect storm.
        Transient,
        /// A server rejection (e.g. permission denied): `is_rejection()` true →
        /// `Rejected`, returned without retry.
        Rejected,
    }

    #[derive(Debug)]
    struct FakeErr {
        rejection: bool,
    }
    impl core::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "fake {}",
                if self.rejection {
                    "rejection"
                } else {
                    "transport fault"
                }
            )
        }
    }
    impl RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            self.rejection
        }
    }

    /// A fake [`RpcRequester`] that returns a scripted sequence of outcomes and
    /// counts its calls — standing in for a real (flaky) transport.
    struct ScriptedRequester {
        script: RefCell<VecDeque<Outcome>>,
        calls: Cell<u32>,
    }
    impl ScriptedRequester {
        fn new(script: impl IntoIterator<Item = Outcome>) -> Self {
            Self {
                script: RefCell::new(script.into_iter().collect()),
                calls: Cell::new(0),
            }
        }
    }
    impl RpcRequester for ScriptedRequester {
        type Error = FakeErr;
        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, FakeErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.calls.set(self.calls.get() + 1);
            match self.script.borrow_mut().pop_front() {
                // Tests instantiate `Reply = ()`; produce it by round-tripping a
                // unit value through the canonical codec the real wire uses.
                Some(Outcome::Ok) => {
                    let bytes = fauna_protocol::encode_canonical(&()).expect("encode unit");
                    Ok(fauna_protocol::decode_strict(&bytes).expect("decode unit reply"))
                }
                Some(Outcome::Transient) => Err(FakeErr { rejection: false }),
                Some(Outcome::Rejected) => Err(FakeErr { rejection: true }),
                None => panic!("request() called more times than scripted"),
            }
        }
    }

    /// No-op backoff so the retry loop doesn't actually sleep in tests.
    async fn no_sleep(_ms: u32) {}

    async fn run(script: Vec<Outcome>) -> (Result<(), NestError>, u32) {
        let req = ScriptedRequester::new(script);
        let res: Result<(), NestError> =
            request_idempotent(&req, "fauna.bridges.provision_test", 0u32, no_sleep).await;
        (res, req.calls.get())
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        // Two mid-flight disconnects, then the provision lands — the user's
        // flaky-remote enable-mail failure, now recovered instead of aborting.
        let (res, calls) = run(vec![Outcome::Transient, Outcome::Transient, Outcome::Ok]).await;
        assert!(
            res.is_ok(),
            "transient drops must retry to success: {res:?}"
        );
        assert_eq!(calls, 3, "retried twice before succeeding");
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        // A connection that never recovers fails after exactly the bounded
        // attempt count — no infinite retry loop.
        let (res, calls) = run(vec![
            Outcome::Transient;
            MAIL_PROVISION_MAX_ATTEMPTS as usize + 2
        ])
        .await;
        assert!(
            matches!(res, Err(NestError::Transient(_))),
            "exhausted retries surface Transient: {res:?}"
        );
        assert_eq!(
            calls, MAIL_PROVISION_MAX_ATTEMPTS,
            "stops at the attempt cap"
        );
    }

    #[tokio::test]
    async fn rejection_is_not_retried() {
        // A server rejection is a real answer, not a transport fault — surfaced
        // immediately without burning retries.
        let (res, calls) = run(vec![Outcome::Rejected, Outcome::Ok]).await;
        assert!(
            matches!(res, Err(NestError::Rejected(_))),
            "rejection must not retry: {res:?}"
        );
        assert_eq!(calls, 1, "rejection returns on the first call");
    }

    /// The shared `mail-spam` wire→view projection both seam arms call. Pins
    /// every label/source arm + the hex-id + message carry-through, so a swapped
    /// enum arm fails here rather than rendering a wrong badge on six apps.
    #[test]
    fn project_spam_history_maps_every_enum_arm() {
        use crate::spam::{TrainingLabel, TrainingSource};
        use fauna_protocol::bridge_routing::{
            ListSpamTrainingHistoryReply, SpamLabel, SpamTrainingHistoryRow,
            TrainingSource as WireSource,
        };

        let reply = ListSpamTrainingHistoryReply {
            events: vec![
                SpamTrainingHistoryRow {
                    history_id: vec![0xABu8; 16],
                    message: "Cheap pills · INBOX".into(),
                    sealed_subject: Default::default(),
                    mailbox: "INBOX".into(),
                    label: SpamLabel::Spam,
                    source: WireSource::ImapJunkFlag,
                    created_at_ms: 1_700_000_000_000,
                    model_delta_applied: Default::default(),
                    extra: Default::default(),
                },
                SpamTrainingHistoryRow {
                    history_id: vec![0xCDu8; 16],
                    message: "Lunch? · INBOX".into(),
                    sealed_subject: Default::default(),
                    mailbox: "INBOX".into(),
                    label: SpamLabel::Ham,
                    source: WireSource::ImapJunkMove,
                    created_at_ms: 1_700_000_100_000,
                    model_delta_applied: Default::default(),
                    extra: Default::default(),
                },
                SpamTrainingHistoryRow {
                    history_id: vec![0xEFu8; 16],
                    message: "Receipt · Archive".into(),
                    sealed_subject: Default::default(),
                    mailbox: "Archive".into(),
                    label: SpamLabel::Ham,
                    source: WireSource::ManualOther,
                    created_at_ms: 1_700_000_200_000,
                    model_delta_applied: Default::default(),
                    extra: Default::default(),
                },
            ],
            contribute_baseline: true,
            extra: Default::default(),
        };

        let history = super::project_spam_history(reply);
        assert!(history.contribute_baseline);
        assert_eq!(history.events.len(), 3);

        // Row 0: Spam / ImapJunkFlag — hex id preserved + message carried verbatim.
        assert_eq!(history.events[0].history_id_hex, hex::encode([0xABu8; 16]));
        assert_eq!(history.events[0].message, "Cheap pills · INBOX");
        assert_eq!(history.events[0].label, TrainingLabel::Spam);
        assert_eq!(history.events[0].source, TrainingSource::ImapJunkFlag);
        assert_eq!(history.events[0].created_at_ms, 1_700_000_000_000);
        // Sealed-row display fields carried through for the machine's compose step
        // (a plaintext row has an empty `sealed_subject`).
        assert_eq!(history.events[0].mailbox, "INBOX");
        assert!(history.events[0].sealed_subject.is_empty());

        // Row 1: Ham / ImapJunkMove.
        assert_eq!(history.events[1].label, TrainingLabel::Ham);
        assert_eq!(history.events[1].source, TrainingSource::ImapJunkMove);

        // Row 2: `ManualOther` is the ratified first-party-button source
        // (`mail-spam.md` § Wire shapes), so it reads "Fauna app" — the lesson
        // says where it was given, never a bare "Other".
        assert_eq!(history.events[2].source, TrainingSource::ExplicitButton);
    }

    /// A label or source a newer nest wrote (`Unknown` on the wire) projects to
    /// the view's `Unknown` — a neutral badge, and the row stays in the list. It
    /// is never read as `Spam` or as the first-party button.
    #[test]
    fn project_spam_history_keeps_an_unknown_label_and_source_neutral() {
        use crate::spam::{TrainingLabel, TrainingSource};
        use fauna_protocol::bridge_routing::{
            ListSpamTrainingHistoryReply, SpamLabel, SpamTrainingHistoryRow,
            TrainingSource as WireSource,
        };

        let history = super::project_spam_history(ListSpamTrainingHistoryReply {
            events: vec![SpamTrainingHistoryRow {
                history_id: vec![0x11u8; 16],
                message: "Newer · INBOX".into(),
                sealed_subject: Default::default(),
                mailbox: "INBOX".into(),
                label: SpamLabel::Unknown,
                source: WireSource::Unknown,
                created_at_ms: 5,
                model_delta_applied: Default::default(),
                extra: Default::default(),
            }],
            contribute_baseline: false,
            extra: Default::default(),
        });
        assert_eq!(history.events.len(), 1);
        assert_eq!(history.events[0].label, TrainingLabel::Unknown);
        assert_eq!(history.events[0].source, TrainingSource::Unknown);
    }

    /// An import outcome a newer nest added counts as errored (logged, against
    /// the error budget) — never as imported or skipped.
    #[test]
    fn project_import_outcome_counts_an_unknown_outcome_as_errored() {
        use crate::import::SendOutcome;
        use fauna_protocol::bridge_routing::ImportMessageOutcome;
        assert!(matches!(
            super::project_import_outcome(ImportMessageOutcome::Unknown),
            SendOutcome::Errored { .. }
        ));
    }
}
