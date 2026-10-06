//! WS-RPC handlers for the I2b routing/data-plane surface that
//! isn't blob-shaped: `validate_recipient`, `fetch_recipient_mls_pubkey`,
//! `subscribe_config` (unary `fetch_config` for now), and
//! `report_session_close`. Wrapped-blob fetch/store/revoke handlers
//! stay in `bridge_blob_handlers.rs`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound, OutboundRow};
use crate::domain_hash::{HashField, write_fields};
use fauna_core::data::{MailIngress, MailVerdict, UnknownSenderMail, supervised_mail_verdict};
use fauna_mls::wrapped_blob::SealedRecordBytes;
use fauna_protocol::{
    RpcError, Value,
    bridge_routing::{
        AbortPrimaryDomainRenameReply, AbortPrimaryDomainRenameRequest, AddLocalDomainReply,
        AddLocalDomainRequest, AliasControls, AliasHitRow, AliasPolicy, AliasRow, ArcVerdict,
        BlocklistSelfCheckHistoryRow, BlocklistSelfCheckRunReply, BlocklistSelfCheckRunRequest,
        BlocklistServerResult, CheckGreylistReply, CheckGreylistRequest, CheckSubmissionQuotaReply,
        CheckSubmissionQuotaRequest, ClamavVerdict, CompletePrimaryDomainRenameReply,
        CompletePrimaryDomainRenameRequest, CreateAccountAliasReply, CreateAccountAliasRequest,
        CreateForwarderReply, CreateForwarderRequest, DecodeSrsBounceReply, DecodeSrsBounceRequest,
        DeleteAccountAliasReply, DeleteAccountAliasRequest, DeleteForwarderReply,
        DeleteForwarderRequest, DeliverSealedSchedulingReply, DeliverSealedSchedulingRequest,
        DiagnosticCheckResult, DiagnosticRunHistoryRow, DkimVerdict, DmarcPolicy, DmarcVerdict,
        DomainDkimSelector, EnableAccountAliasReply, EnableAccountAliasRequest,
        EnqueueOutboundMailReply, EnqueueOutboundMailRequest, ExtendPrimaryDomainRenameGraceReply,
        ExtendPrimaryDomainRenameGraceRequest, FetchConfigReply, FetchConfigRequest,
        FetchMtaStsPolicyReply, FetchMtaStsPolicyRequest, FetchOutboundDueReply,
        FetchOutboundDueRequest, FetchRecipientFiltersReply, FetchRecipientFiltersRequest,
        FetchRecipientForwardConfigReply, FetchRecipientForwardConfigRequest,
        FetchRecipientIndexKeyReply, FetchRecipientIndexKeyRequest, FetchRecipientMlsPubkeyReply,
        FetchRecipientMlsPubkeyRequest, FetchTlsaReply, FetchTlsaRequest, ForceRotateDkimReply,
        ForceRotateDkimRequest, ForwardMessageReply, ForwardMessageRequest,
        GenerateDisposableAliasReply, GenerateDisposableAliasRequest, GetAliasPolicyRequest,
        GetForwardAllToReply, GetForwardAllToRequest, GetForwardPerHourReply,
        GetForwardPerHourRequest, GetMailConfigRequest, GetPrimaryDomainRenameStatusReply,
        GetPrimaryDomainRenameStatusRequest, GetSpamThresholdOverrideReply,
        GetSpamThresholdOverrideRequest, ImportAccountAliasesReply, ImportAccountAliasesRequest,
        ImportAliasOutcome, ImportAliasStatus, IngestInboundMailReply, IngestInboundMailRequest,
        ListAccountAliasHitsReply, ListAccountAliasHitsRequest, ListAccountAliasesReply,
        ListAccountAliasesRequest, ListBlocklistSelfCheckHistoryReply,
        ListBlocklistSelfCheckHistoryRequest, ListDeliverabilityDiagnosticRunsReply,
        ListDeliverabilityDiagnosticRunsRequest, ListForwardersReply, ListForwardersRequest,
        ListLocalDomainsReply, ListLocalDomainsRequest, ListPrimaryDomainRenamesReply,
        ListPrimaryDomainRenamesRequest, MailDomainRenameRow, MailDomainRow, MailHealthCheck,
        MailHealthReply, MailHealthRequest, MarkOutboundBouncedReply, MarkOutboundBouncedRequest,
        MarkOutboundDeliveredReply, MarkOutboundDeliveredRequest, MarkOutboundFailedReply,
        MarkOutboundFailedRequest, MtaStsPolicyWire, MxHostWire, OutboundUnit,
        OutboundWarmupResetRequest, OutboundWarmupStatusReply, OutboundWarmupStatusRequest,
        ProvisionRecipientMlsPubkeyRequest, ProvisionSelfSignedCertReply,
        ProvisionSelfSignedCertRequest, PutAliasPolicyRequest, PutAuthPolicyRequest,
        PutImapPolicyRequest, PutOutboundPolicyRequest, PutPolicyReply, PutSpamPolicyRequest,
        PutSubmissionPolicyRequest, RecipientSealKeyHalves, RemoveLocalDomainReply,
        RemoveLocalDomainRequest, ReportRejectedScanReply, ReportRejectedScanRequest,
        ReportSessionCloseReply, ReportSessionCloseRequest, ReportTlsAttemptReply,
        ReportTlsAttemptRequest, ResolveMxReply, ResolveMxRequest, ResolveRecipientReply,
        ResolveRecipientRequest, RestoreLocalDomainReply, RestoreLocalDomainRequest,
        RestoreRealTlsCertReply, RestoreRealTlsCertRequest, RevokeAccountAliasReply,
        RevokeAccountAliasRequest, RoleAddressKind, RotateSrsSecretReply, RotateSrsSecretRequest,
        RspamdScore, RunDeliverabilityDiagnosticsReply, RunDeliverabilityDiagnosticsRequest,
        SealedBridgeInfo, SendAutoReplyReply, SendAutoReplyRequest, SetCatchAllActorReply,
        SetCatchAllActorRequest, SetDkimRotationDaysReply, SetDkimRotationDaysRequest,
        SetForwardAllToReply, SetForwardAllToRequest, SetForwardPerHourReply,
        SetForwardPerHourRequest, SetRoleAddressReply, SetRoleAddressRequest,
        SetSpamThresholdOverrideReply, SetSpamThresholdOverrideRequest, SpamDisposition,
        SpamPolicyThresholds, SpfVerdict, StampedHeader, StartPrimaryDomainRenameReply,
        StartPrimaryDomainRenameRequest, TlsaRecordWire, UpdateAccountAliasReply,
        UpdateAccountAliasRequest, UpdateLocalDomainConfigReply, UpdateLocalDomainConfigRequest,
        ValidateRecipientReply, ValidateRecipientRequest, WhoamiReply, WhoamiRequest,
        config_change_reason,
    },
    decode_strict as decode,
    wrapped_blob::ProvisionReply,
};
use serde_bytes::ByteBuf;

use crate::bridge_method_allowlist::CallerClass;
use crate::db::bridge_imap::{
    CreateMailboxDbOutcome, GUARDIAN_HELD_MAILBOX, is_reserved_mailbox, validate_mailbox_name,
};
use crate::db::bridge_routing::{InboundMailFields, ScanResultRow, SubmissionQuotaOutcome};
use crate::db::family::normalize_mail_address;
use crate::db::mail_policy::{
    AliasPolicyOverrides, AuthPolicyOverrides, ImapPolicyOverrides, OutboundPolicyOverrides,
    SpamPolicyOverrides, SubmissionPolicyOverrides,
};
use crate::db::now_epoch_secs;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use fauna_mail::segments::placement::MailPlacementRecord;

// ── Helpers (mirror bridge_blob_handlers.rs) ────────────────────

pub(crate) use crate::rpc_errors::{encode_reply, malformed};

pub(crate) fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("bridges", reason)
}

pub(crate) use crate::rpc_errors::internal;

pub(crate) use crate::rpc_errors::placement_journal_diverged;

pub(crate) fn not_found(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::not_found_ns("bridges", reason)
}

/// `force_rotate_dkim` was called but the domain's newest DKIM selector
/// already equals its active one — there is no *newer* key to flip to. The
/// rotation mint seats one once the domain is rotation-due
/// (`mail-multidomain.md` § Rotation); the rotate is re-issued after it.
pub(crate) fn no_dkim_selector_to_rotate(domain: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.no_dkim_selector_to_rotate",
        "error.bridges.no_dkim_selector_to_rotate",
    );
    e.details = Some(Box::new(Value::String(format!(
        "no newer DKIM selector provisioned for '{domain}' to rotate to; \
         provision a new selector's key first"
    ))));
    e
}

/// RFC 9208 over-quota on a mailbox write. `resource` is the quota-root
/// resource that would be exceeded — `"storage"` (RES-STORAGE) or
/// `"message"` (RES-MESSAGE). The Go MDA maps this code → `NO [OVERQUOTA]`
/// on the IMAP wire (APPEND/COPY/MOVE) and the Go MTA maps it →
/// `552 5.2.2 Mailbox full` on the SMTP wire (inbound delivery). The
/// signal is a *typed error*, not an outcome-enum reply (those write RPCs
/// carry a real success payload — uid/message_id — that an outcome variant
/// would muddy; contrast `CheckSubmissionQuotaReply` whose verdict IS its
/// whole result). See `imap-server.md` § Quota enforcement points.
pub(crate) fn over_quota(resource: &str) -> RpcError {
    let mut e = RpcError::new(
        RpcError::CODE_BRIDGES_OVER_QUOTA,
        "error.bridges.over_quota",
    );
    e.details = Some(Box::new(Value::String(format!(
        "{resource} quota exceeded"
    ))));
    e
}

/// `forward_message` did not park an over-cap forward (`mail-forwarding.md`
/// § Queue ceiling): a `redirect` forward refused because making room would
/// evict the only copy of other accepted mail (only `copy` rows are ever
/// evicted), or a `copy` forward that was itself the oldest copy dropped at the
/// ceiling. Nothing was enqueued, so it is an error rather than a `queued`
/// reply naming a row that does not exist: the MTA falls a per-rule redirect
/// back to local delivery and answers an admin forwarder's transaction `451`.
pub(crate) fn forward_queue_full() -> RpcError {
    RpcError::new(
        "fauna.bridges.forward_queue_full",
        "error.bridges.forward_queue_full",
    )
}

/// A ward's MUA tried to relocate a message out of the held mailbox while
/// its `guardian_mail_holds` sidecar row is live (`family-safety.md` § The
/// mail gate). The MDA maps this to a tagged `NO` — the message stays put
/// until the guardian releases or discards it. Reading is never gated.
pub(crate) fn held_for_review() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.held_for_review",
        "error.bridges.held_for_review",
    );
    e.details = Some(Box::new(Value::String(
        "message is held for guardian review".to_string(),
    )));
    e
}

pub(crate) fn pure_backup_destination() -> RpcError {
    crate::rpc_errors::pure_backup_destination_ns(
        "bridges",
        "this destination holds opaque chunks only; \
         IMAP serving requires local plaintext-framed segments",
    )
}

/// The shared "query [`is_pure_backup_destination`], then refuse" ceremony
/// behind every pure-backup gate in the fleet: `require_local_conv_serving`,
/// `require_local_mail_serving`, [`require_dav_caller_scope`] below, and the
/// inline mail/filesync/sync gates each hand-copied this same query-then-refuse
/// shape. `on_refuse` stays caller-supplied because the refusal text (and its
/// RPC namespace) genuinely differs per resource kind — this only collapses
/// the query ceremony, never the error identity.
///
/// [`is_pure_backup_destination`]: crate::db::sync_storage
pub(crate) async fn refuse_if_pure_backup(
    state: &AppState,
    resource_kind: &str,
    target: &[u8; 32],
    on_refuse: impl FnOnce() -> RpcError,
) -> Result<(), RpcError> {
    if state
        .db
        .is_pure_backup_destination(resource_kind, target)
        .await
        .map_err(internal)?
    {
        return Err(on_refuse());
    }
    Ok(())
}

/// The actor has turned IMAP/CalDAV serving OFF on this nest (per-actor,
/// user-set; `deployment-home-with-public-relay.md` § MUA reach). The MDA's
/// serving handlers reject for this actor while still serving every other
/// actor. Distinct from `pure_backup_destination` (a nest-storage-mode gate):
/// this is the user's own reading-location preference. Other users unaffected.
pub(crate) fn mail_serving_disabled() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.mail_serving_disabled",
        "error.bridges.mail_serving_disabled",
    );
    e.details = Some(Box::new(Value::String(
        "this actor has disabled IMAP/CalDAV serving on this nest; \
         they read their mail/calendar elsewhere"
            .into(),
    )));
    e
}

/// Per-actor IMAP/CalDAV-serving gate. Reads the actor's user-set serving flag
/// (default ON / absent ⇒ ON) and returns [`mail_serving_disabled`] when the
/// actor has turned serving OFF on this nest. Called by the nest-side MDA
/// serving gates: `require_local_mail_serving` (every IMAP handler) and the
/// `BridgeMda`-path in the CalDAV handlers. Pairs with — but is independent of —
/// `is_pure_backup_destination`: the pure-backup check is a storage-mode gate,
/// this is a user preference. Spec:
/// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
/// § MUA reach.
pub(crate) async fn ensure_actor_mail_serving_enabled(
    state: &Arc<AppState>,
    target: &[u8; 32],
) -> Result<(), RpcError> {
    let enabled = state
        .db
        .get_actor_mail_serving_enabled(target)
        .await
        .map_err(internal)?
        .unwrap_or(true);
    if !enabled {
        return Err(mail_serving_disabled());
    }
    Ok(())
}

/// Shared caller-scoping gate for the CalDAV and CardDAV bridge handlers:
/// only the mail bridge may act on another actor's data, a pure-backup
/// destination refuses to serve/mutate this kind for every caller class, and
/// a `BridgeMda` caller additionally respects the actor's own IMAP/CalDAV
/// serving preference (`ensure_actor_mail_serving_enabled`). The user's own
/// client path (`User`/`Admin`, `target == caller`) is never gated by the
/// serving flag — see the two DAV handler modules' own doc comments.
/// `resource_kind` is the `is_pure_backup_destination` storage-mode tag
/// (`"calendar"`/`"card"`); `resource_noun` is the denial message's noun
/// (`"calendar"`/`"address book"`).
pub(crate) async fn require_dav_caller_scope(
    state: &Arc<AppState>,
    class: CallerClass,
    target: &[u8; 32],
    actor_id: &[u8; 32],
    resource_kind: &str,
    resource_noun: &str,
) -> Result<(), RpcError> {
    if class != CallerClass::BridgeMda && target != actor_id {
        return Err(permission_denied(&format!(
            "only the mail bridge may access another actor's {resource_noun}"
        )));
    }
    refuse_if_pure_backup(state, resource_kind, target, pure_backup_destination).await?;
    if class == CallerClass::BridgeMda {
        ensure_actor_mail_serving_enabled(state, target).await?;
    }
    Ok(())
}

/// `UNIQUE (local_domain, pattern, kind)` violation on alias create/update
/// — two aliases of the same kind can't share a pattern on a local domain
/// (`mail-aliases.md` § Cross-user uniqueness).
pub(crate) fn conflicts_with_existing_alias() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.conflicts_with_existing_alias",
        "error.bridges.conflicts_with_existing_alias",
    );
    e.details = Some(Box::new(Value::String(
        "an alias with this pattern already exists on this domain".into(),
    )));
    e
}

/// User-tier create tried to claim a reserved local-part
/// (`mail-aliases.md` § Reserved local-parts — uncircumventable).
pub(crate) fn reserved_local_part(part: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.reserved_local_part",
        "error.bridges.reserved_local_part",
    );
    e.details = Some(Box::new(Value::String(format!(
        "'{part}' is a reserved local-part and cannot be claimed as an alias"
    ))));
    e
}

/// A wildcard prefix would shadow a reserved local-part (its glob `<prefix>*`
/// matches a reserved name — e.g. `dmarc-*` shadows `dmarc-report`).
/// `mail-aliases.md` § Kind 3 `:60`.
pub(crate) fn reserved_local_part_in_wildcard(prefix: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.reserved_local_part_in_wildcard",
        "error.bridges.reserved_local_part_in_wildcard",
    );
    e.details = Some(Box::new(Value::String(format!(
        "wildcard prefix '{prefix}' would shadow a reserved local-part"
    ))));
    e
}

/// The actor already owns a wildcard-prefix alias — one wildcard pattern per
/// actor (`mail-aliases.md` § Kind 3 `:52`).
pub(crate) fn actor_already_has_wildcard() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.actor_already_has_wildcard",
        "error.bridges.actor_already_has_wildcard",
    );
    e.details = Some(Box::new(Value::String(
        "an actor may own only one wildcard-prefix alias; delete the existing one first".into(),
    )));
    e
}

/// The actor is at the per-account exact-alias cap
/// (`mail.account.exact_aliases_max`, `mail-aliases.md` § Kind 1 Exact).
pub(crate) fn alias_cap_exceeded(max: u32) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.alias_cap_exceeded",
        "error.bridges.alias_cap_exceeded",
    );
    e.details = Some(Box::new(Value::String(format!(
        "per-account exact-alias cap reached ({max}); delete an alias before adding another"
    ))));
    e
}

/// The actor hit the per-day disposable-mint cap
/// (`mail.account.disposable_generate_per_day`, `mail-aliases.md` § Don't
/// `:327` — the anti-enumeration guard on the mint generator).
pub(crate) fn disposable_generate_rate_limited(cap: u32) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.disposable_generate_rate_limited",
        "error.bridges.disposable_generate_rate_limited",
    );
    e.details = Some(Box::new(Value::String(format!(
        "disposable-alias generation cap reached ({cap}/day); try again later"
    ))));
    e
}

/// The actor has no canonical address to derive the disposable `<handle>` +
/// `<domain>` from (`mail-aliases.md` § Kind 5 — the mint builds
/// `<handle>-temp-<token>@<domain>` from the actor's oldest exact alias).
pub(crate) fn no_canonical_address() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.no_canonical_address",
        "error.bridges.no_canonical_address",
    );
    e.details = Some(Box::new(Value::String(
        "create your canonical address (an exact alias) before minting a disposable".into(),
    )));
    e
}

/// The caller tried to disable or delete their **canonical** address — the
/// `<handle>@<domain>` exact alias written at mail-enable. It is the user's
/// primary mailbox + AUTH-login identity and is required for disposable mints
/// (`mail-aliases.md:34`/`:471`), so it must always be alive: disabling/deleting
/// it would strand inbound mail + login with no client-side recovery. Rejected.
pub(crate) fn canonical_alias_protected() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.canonical_alias_protected",
        "error.bridges.canonical_alias_protected",
    );
    e.details = Some(Box::new(Value::String(
        "your primary address (handle) can't be disabled or deleted".into(),
    )));
    e
}

// ── Primary-domain-rename refusals (mail-primary-domain-rename.md
//    § Wire shapes — RPC refusal codes) ─────────────────────────────

/// A rename is already in flight (`mail_domain_renames` has a non-terminal row).
/// At most one rename at a time (§ Concurrency).
pub(crate) fn rename_already_in_progress() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.rename_already_in_progress",
        "error.bridges.rename_already_in_progress",
    );
    e.details = Some(Box::new(Value::String(
        "a primary-domain rename is already in progress; complete or abort it first".into(),
    )));
    e
}

/// The rename target is not a current additional (`is_primary = false` and
/// `removed_at IS NULL`) — e.g. it doesn't exist, is the current primary, or was
/// removed. The admin must `add_local_domain(<new>)` first (§ two-step).
pub(crate) fn new_primary_must_be_additional() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.new_primary_must_be_additional",
        "error.bridges.new_primary_must_be_additional",
    );
    e.details = Some(Box::new(Value::String(
        "the new primary must already exist as an additional local domain; add it first".into(),
    )));
    e
}

/// The rename target's cert mode isn't `expand_primary` — the primary anchors the
/// cert chain the additionals' `mta-sts.` SANs piggyback on (§ Cert mode
/// interaction).
pub(crate) fn new_primary_cert_mode_must_be_expand_primary() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.new_primary_cert_mode_must_be_expand_primary",
        "error.bridges.new_primary_cert_mode_must_be_expand_primary",
    );
    e.details = Some(Box::new(Value::String(
        "the new primary must use the 'expand_primary' cert mode; change it first".into(),
    )));
    e
}

/// The rename target's TLS posture is weaker than the current primary's — the
/// rename must not regress MTA-STS posture (§ Goal #3 / TLS-posture monotonicity).
pub(crate) fn new_primary_tls_posture_weaker() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.new_primary_tls_posture_weaker",
        "error.bridges.new_primary_tls_posture_weaker",
    );
    e.details = Some(Box::new(Value::String(
        "the new primary's MTA-STS posture must match or exceed the current primary's".into(),
    )));
    e
}

/// `complete_primary_domain_rename` called on a `grace` row without `force`
/// (`mail-primary-domain-rename.md` § Wire shapes — the admin signs off on the
/// cache-flush completion once the grace window elapses; `force = true` overrides
/// it early, accepting the peer-cache-flush risk). A `ready_to_complete` row (the
/// grace watcher promoted it) completes without `force`.
pub(crate) fn grace_period_not_expired() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.grace_period_not_expired",
        "error.bridges.grace_period_not_expired",
    );
    e.details = Some(Box::new(Value::String(
        "the grace window has not elapsed; wait for the watcher to reach ready_to_complete, \
         or complete with force=true to override (accepting the peer-cache-flush risk)"
            .into(),
    )));
    e
}

/// `remove_local_domain` against a domain that is a participant (old or new
/// primary) of an in-flight, non-terminal rename (`mail-primary-domain-rename.md`
/// § Cross-table cascade). The admin must complete or abort the rename first — a
/// mid-rename removal would strand the flip with a missing anchor.
pub(crate) fn domain_in_rename_flight() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.domain_in_rename_flight",
        "error.bridges.domain_in_rename_flight",
    );
    e.details = Some(Box::new(Value::String(
        "this domain is participating in an in-flight primary-domain rename; \
         complete or abort the rename before removing it"
            .into(),
    )));
    e
}

/// The post-rename cert SAN graph would exceed Let's Encrypt's 100-SAN cap
/// (§ Behavior — cert chain re-issue ordering). The admin must drop some domain
/// to a non-`expand_primary` cert mode first.
pub(crate) fn cert_san_limit_exceeded(san_count: usize) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.cert_san_limit_exceeded",
        "error.bridges.cert_san_limit_exceeded",
    );
    e.details = Some(Box::new(Value::String(format!(
        "the post-rename certificate would carry {san_count} SANs, over the {} limit; \
         move a domain to a per-host or wildcard cert mode first",
        fauna_mail::LETSENCRYPT_SAN_LIMIT
    ))));
    e
}

/// A rename state-transition RPC (`complete`/`extend`/`abort`) was called on a
/// row that is not in a state that transition accepts (`mail-primary-domain-
/// rename.md` § Wire shapes — RPC refusal codes). Either the admin acted on a
/// stale view, or a concurrent transition (the cert-lifecycle loop or another
/// Admin RPC) changed the state between the admin's read and nest's
/// state-conditional UPDATE (the storage CAS matched 0 rows — a lost race,
/// refused rather than clobbering). A 409-class refusal, not a 500: the admin
/// re-reads `get_primary_domain_rename_status` and retries against the current
/// state. No info leak — the state is one the admin can already read.
pub(crate) fn rename_wrong_state() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.rename_wrong_state",
        "error.bridges.rename_wrong_state",
    );
    e.details = Some(Box::new(Value::String(
        "the rename is no longer in a state that accepts this action; \
         re-read its status and retry"
            .into(),
    )));
    e
}

/// `new_primary_domain_id == old_primary_domain_id` — a domain can't be renamed
/// to itself.
pub(crate) fn same_domain_for_rename() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.same_domain_for_rename",
        "error.bridges.same_domain_for_rename",
    );
    e.details = Some(Box::new(Value::String(
        "the new primary must differ from the current primary".into(),
    )));
    e
}

/// `grace_days` outside the accepted `[1, 30]` range.
pub(crate) fn invalid_grace_days() -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.invalid_grace_days",
        "error.bridges.invalid_grace_days",
    );
    e.details = Some(Box::new(Value::String(format!(
        "grace_days must be between {} and {}",
        fauna_mail::GRACE_DAYS_MIN,
        fauna_mail::GRACE_DAYS_MAX
    ))));
    e
}

// ── Verdict → DB-flat-string converters ─────────────────────────────
//
// The wire `AuthVerdicts` rides as nested enums (canonical UniFFI shape
// with adjacently-tagged serde). The DB layer (`InboundMailFields`)
// keeps the legacy RFC-canonical lowercase strings without separators:
// SPF/DKIM/DMARC/ARC names follow RFC 7208/6376/7489 verbatim.
//
// Mapping notes:
// * SPF `SoftFail` → `softfail` (RFC 7208 §2.6 spelling), not `soft_fail`.
// * Any `PermError/TempError` → `permerror/temperror` (same).
// * DKIM `Fail { reason }` discards the reason for the DB row — the
//   reason is diagnostic, not part of the result name.
// * DMARC returns two strings: the result name plus the policy name.
//   For non-fail verdicts we report policy `"none"` since there is no
//   published policy to honor.

fn spf_to_flat(v: &SpfVerdict) -> &'static str {
    match v {
        SpfVerdict::None => "none",
        SpfVerdict::Pass => "pass",
        SpfVerdict::Fail => "fail",
        SpfVerdict::SoftFail => "softfail",
        SpfVerdict::Neutral => "neutral",
        SpfVerdict::PermError => "permerror",
        SpfVerdict::TempError => "temperror",
    }
}

fn dkim_to_flat(v: &DkimVerdict) -> &'static str {
    match v {
        DkimVerdict::None => "none",
        DkimVerdict::Pass => "pass",
        DkimVerdict::Fail { .. } => "fail",
        DkimVerdict::Neutral => "neutral",
        DkimVerdict::PermError => "permerror",
        DkimVerdict::TempError => "temperror",
    }
}

fn dmarc_to_flat(v: &DmarcVerdict) -> (&'static str, &'static str) {
    match v {
        DmarcVerdict::None => ("none", "none"),
        DmarcVerdict::Pass => ("pass", "none"),
        DmarcVerdict::Fail { policy } => ("fail", dmarc_policy_to_flat(policy)),
        DmarcVerdict::PermError => ("permerror", "none"),
        DmarcVerdict::TempError => ("temperror", "none"),
    }
}

fn dmarc_policy_to_flat(p: &DmarcPolicy) -> &'static str {
    match p {
        DmarcPolicy::None => "none",
        DmarcPolicy::Quarantine => "quarantine",
        DmarcPolicy::Reject => "reject",
    }
}

fn arc_to_flat(v: &ArcVerdict) -> &'static str {
    match v {
        ArcVerdict::None => "none",
        ArcVerdict::Pass => "pass",
        ArcVerdict::Fail => "fail",
        ArcVerdict::PermError => "permerror",
        ArcVerdict::TempError => "temperror",
    }
}

pub(crate) async fn require_class(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<CallerClass, RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, internal).await
}

// ── validate_recipient ─────────────────────────────────────────

fn validate_recipient_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.validate_recipient").await?;
            let req: ValidateRecipientRequest = decode(&payload).map_err(malformed)?;
            // Resolve through `account_aliases` (kind='exact') — the
            // production source of truth per
            // `docs/goal/behavior/mail-aliases.md` § Storage. The
            // previous `recipient_routes` table had no production
            // writer; a member writes here through their own alias CRUD
            // (`fauna.bridges.create_account_alias`).
            // Kinds 2-5 + the fixed-order resolver are deferred.
            let resolved = state
                .db
                .lookup_exact_alias(&req.domain, &req.local_part)
                .await
                .map_err(internal)?;
            if let Some(actor) = resolved {
                return encode_reply(&ValidateRecipientReply::Resolved {
                    actor_id: actor.to_vec(),
                    // A normal alias hit — quota applies on inbound delivery.
                    is_role_address: false,
                });
            }
            // Alias miss. Handle-keyed local-auth fallback (Change A; design
            // tracked internally): a domainless / bare-IP nest reached by any locator has no
            // registered mail-domain, so no exact-alias row resolves. For the
            // AUTH-login path ONLY (`validate_recipient`), when the address
            // domain is NOT a registered mail-domain, resolve the local-part
            // against the unique handle→actor store instead. This is what makes
            // `test`, `test@<IP>`, `test@<any-name-that-reached-me>` all
            // authenticate to the single actor whose *handle* is the local-part.
            //
            // It is hijack-proof + unambiguous because handles are unique per
            // nest (`resolve_handle` returns a single Option) and aliases are a
            // different, Admin-only namespace — a colliding admin alias never
            // participates in this path (it resolves only via its explicit
            // `@<registered-domain>` form, kept above). Delivery
            // (`resolve_recipient` / `partition_recipients`) is untouched and
            // stays alias/domain-strict — this fallback is local-login only.
            //
            // The "is this a registered mail-domain?" signal is
            // `lookup_active_mail_domain` (Some ⇒ registered) — the same
            // per-domain row the delivery resolver reads for catch-all/role
            // overrides (`resolve_local_recipient`). An empty / non-matching
            // domain returns None and takes the handle fallback.
            let domain_is_registered = state
                .db
                .lookup_active_mail_domain(&req.domain)
                .await
                .map_err(internal)?
                .is_some();
            if !domain_is_registered
                && let Some(actor) = state
                    .db
                    .resolve_handle(&req.local_part)
                    .await
                    .map_err(internal)?
            {
                return encode_reply(&ValidateRecipientReply::Resolved {
                    actor_id: actor.to_vec(),
                    // A handle hit is a real mailbox owner — quota applies
                    // on inbound delivery, same as an exact-alias hit.
                    is_role_address: false,
                });
            }
            // Alias miss (and, on an unregistered domain, no matching handle
            // either). Before the unknown-recipient reject (→ 550 on the
            // SMTP wire), check whether this is a reserved role local-part
            // (RFC 2142 / RFC 5321 §4.5.1, smtp-server.md § abuse@/postmaster@
            // routing): those **never reject** even when no mailbox exists and
            // route to the deployment admin's mailbox. The recognition set and
            // routing classifier are shared Rust (T2.5 T0) so the same call
            // carries over when the Go MTA cuts over to `resolve_recipient`.
            let reply = match fauna_mail::aliases::classify_role_address(
                &req.local_part,
                fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS,
            ) {
                fauna_mail::aliases::RoleAddressRoute::NotReserved => {
                    ValidateRecipientReply::Reject {
                        reason: "no such recipient".into(),
                    }
                }
                // AdminMailbox today; tlsrpt@/dmarc-report@ classify as their
                // report processors but fall back here until T2.5 T4 wires the
                // processor dispatch (coordinate with the mail-outbound
                // work's `ingest_tlsrpt_report`). The never-reject
                // invariant (smtp-server.md :201) is what binds for all of
                // them; per-domain re-routing (:204) is deferred (T2.5 T3).
                _route => {
                    // Primary admin = the box's claimer (lowest `added_at`).
                    let admins = state.db.list_admin_actors().await.map_err(internal)?;
                    match admins.first() {
                        Some((admin_actor, _added_at)) => ValidateRecipientReply::Resolved {
                            actor_id: admin_actor.clone(),
                            // Role-address route → bypasses per-mailbox quota
                            // on inbound delivery (smtp-server.md :204). The Go
                            // MTA echoes this on the ingest request so an
                            // over-quota admin mailbox still receives
                            // postmaster/abuse/security mail.
                            is_role_address: true,
                        },
                        // No admin claimed: unreachable in production (an MTA
                        // bridge can only be enrolled by an authenticated
                        // admin, so admin_actor_ids is non-empty whenever this
                        // handler is callable). Still, defend the never-reject
                        // invariant — surface an internal error so the Go MTA
                        // tempfails 451 ("retry later"), never a hard 550. The
                        // wire reply can only express Resolved / Reject→550, so
                        // 451 has to come from an RPC-level error here.
                        None => {
                            return Err(internal(format!(
                                "role address {}@{} has no admin mailbox to route to \
                                 (deployment not yet claimed)",
                                req.local_part, req.domain
                            )));
                        }
                    }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── check_greylist (nest-side greylist, MTA-class) ─────────────
//
// Greylist STATE lives nest-side (`docs/goal/behavior/smtp-server.md` §
// Greylisting, `:172`): the Go MTA forwards `(from, to, client_ip)` and holds
// no local map, so behavior is uniform across bridge restart. The pure
// tuple-key + defer/pass decision are shared `fauna_mail::greylist`; this
// handler does the I/O (policy read, row read/upsert) around them. A disabled
// policy ⇒ always Pass (the e2e fixture sets `greylist_enabled=false` so the
// existing inbound round-trips don't have to wait out a hold).

fn check_greylist_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.check_greylist").await?;
            let req: CheckGreylistRequest = decode(&payload).map_err(malformed)?;

            // Effective policy = spam-policy override overlaid on the catalog
            // default — the same source `fetch_config` projects
            // `greylist_enabled` / `greylist_delay_secs` from. The retry-window
            // (4 h) + whitelist (30 d) knobs are not yet admin-writable
            // ("Bucket C", `mail-policy-config.md:327`) → compile-time catalog
            // defaults via `GreylistPolicy::default()`.
            let default = SpamPolicyThresholds::default();
            let spam = state.db.get_spam_policy().await.map_err(internal)?;
            if !spam.greylist_enabled.unwrap_or(default.greylist_enabled) {
                return encode_reply(&CheckGreylistReply { pass: true });
            }
            // Role-address recipients bypass greylisting (smtp-server.md:205):
            // postmaster@/abuse@/noc@/security@/tlsrpt@/dmarc-report@ traffic is
            // transactional + low-volume, and greylisting it would delay
            // critical postmaster-to-postmaster mail. Classify the RCPT local-part
            // via the shared set used by `validate_recipient`'s role routing
            // (T2.5) — same nest-side `classify_role_address`, no Go change.
            let recipient_local = req.to.rsplit_once('@').map_or(req.to.as_str(), |(l, _)| l);
            if !matches!(
                fauna_mail::aliases::classify_role_address(
                    recipient_local,
                    fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS,
                ),
                fauna_mail::aliases::RoleAddressRoute::NotReserved
            ) {
                return encode_reply(&CheckGreylistReply { pass: true });
            }
            let min_hold = spam
                .greylist_delay_secs
                .unwrap_or(default.greylist_delay_secs) as i64;
            let policy = fauna_mail::greylist::GreylistPolicy {
                min_hold_seconds: min_hold,
                ..fauna_mail::greylist::GreylistPolicy::default()
            };

            let tuple = fauna_mail::greylist::tuple_key(&req.from, &req.to, &req.client_ip);
            let row = state.db.get_greylist_row(&tuple).await.map_err(internal)?;
            let outcome = fauna_mail::greylist::decide(row, state.greylist_now(), policy);
            state
                .db
                .upsert_greylist_row(&tuple, &outcome.next_row)
                .await
                .map_err(internal)?;

            let pass = matches!(outcome.verdict, fauna_mail::greylist::GreylistVerdict::Pass);
            encode_reply(&CheckGreylistReply { pass })
        })
    })
}

// ── resolve_recipient (the fixed-order RCPT-TO resolver, MTA-class) ──
//
// The richer superset of `validate_recipient`: resolves all alias kinds in
// the fixed order (exact → +suffix → disposable → wildcard → catch-all →
// 550; `mail-aliases.md` § Resolution order), honors `disabled`, and returns
// the `X-Fauna-Address-*` headers to stamp + the per-alias control overrides.
// This handler does the I/O (catch-all from `mail_domains`, the exact /
// +suffix-base / wildcard candidate reads) and delegates the *order* to the
// pure `fauna_mail::aliases::resolve_recipient` matcher. Disposable resolution
// + mint are A2.3; the bridge cutover off `validate_recipient` is a separate
// follow-up (A2.2 scope decision 7).

/// db `AliasRecord` → the matcher's native `ExactCandidate`.
fn record_to_exact_candidate(
    r: &crate::db::mail_aliases::AliasRecord,
) -> fauna_mail::aliases::ExactCandidate {
    fauna_mail::aliases::ExactCandidate {
        alias_id: r.alias_id,
        actor_id: r.actor_id,
        disabled: r.disabled,
        controls: alias_record_controls(r),
    }
}

/// `kind='forwarder'` row → the matcher's [`ForwarderCandidate`]. A row whose
/// `forward_target` is unexpectedly NULL (data corruption — the column is
/// always set on insert) maps to an empty target; the matcher still yields a
/// `Forward`, and the bridge surfaces the bad target on first dispatch attempt.
fn record_to_forwarder_candidate(
    r: &crate::db::mail_aliases::AliasRecord,
) -> fauna_mail::aliases::ForwarderCandidate {
    fauna_mail::aliases::ForwarderCandidate {
        alias_id: r.alias_id,
        forward_target: r.forward_target.clone().unwrap_or_default(),
        forwarder_actor_id: r.actor_id,
        disabled: r.disabled,
    }
}

fn alias_record_controls(
    r: &crate::db::mail_aliases::AliasRecord,
) -> fauna_mail::aliases::ResolvedControls {
    fauna_mail::aliases::ResolvedControls {
        spam_threshold_override: r.spam_threshold_override,
        rate_limit_per_hour: r.rate_limit_per_hour,
        rate_limit_per_day: r.rate_limit_per_day,
    }
}

/// The outcome of resolving one local-domain recipient through the full,
/// fixed-order alias resolver — the shared core behind both
/// `fauna.bridges.resolve_recipient` (the MTA's RCPT-TO resolver) and the
/// in-domain partition on `fauna.bridges.enqueue_outbound_mail` (so an
/// in-domain submission / auto-schedule recipient delivers locally instead of
/// self-looping onto the MX-relay queue). Any resolve-time side effects the
/// matcher signals — a disposable `uses_remaining` decrement, the `alias_hits`
/// log — are already applied before this returns.
pub(crate) enum LocalRecipientOutcome {
    /// Resolved to a deliverable local mailbox actor (exact / +suffix /
    /// wildcard / catch-all / disposable / RFC 2142 role-address).
    Mailbox {
        actor_id: [u8; 32],
        stamped_headers: Vec<(String, String)>,
        controls: fauna_mail::aliases::ResolvedControls,
        is_role_address: bool,
    },
    /// An admin external-forwarder (mail-aliases.md § Kind 7) — redirect to an
    /// external target, no local copy.
    Forward {
        forward_target: String,
        forwarder_actor_id: [u8; 32],
    },
    /// The resolver rejected this recipient (e.g. an expired disposable).
    Reject { smtp_code: u16, reason: String },
}

/// Resolve `local_part@domain` and, on a deliverable mailbox, fold the three
/// spam-threshold tiers into the one number that rides out with the message.
///
/// **This is where "resolution at delivery time" happens** (`mail-aliases.md`
/// § Spam-threshold override, ruled 2026-08-17): per-alias override >
/// per-account override > admin-tier default collapse HERE, nest-side, at the
/// one RCPT that knows which alias matched — and the result is appended to
/// `stamped_headers` as [`fauna_mail::aliases::HEADER_SPAM_THRESHOLD`], so the
/// MDA's SELECT-time scorer reads a number instead of re-resolving a chain it
/// would have to be told the user's whole alias taxonomy to walk.
///
/// The fold sits in this wrapper rather than in each `Mailbox` arm of
/// [`resolve_local_recipient_unstamped`] so that a future arm cannot ship an
/// unstamped delivery: every deliverable outcome passes through this one spot,
/// role-address route included.
pub(crate) async fn resolve_local_recipient(
    state: &Arc<AppState>,
    domain: &str,
    local_part: &str,
    sender_domain: &str,
) -> Result<LocalRecipientOutcome, RpcError> {
    let outcome =
        resolve_local_recipient_unstamped(state, domain, local_part, sender_domain).await?;
    let LocalRecipientOutcome::Mailbox {
        actor_id,
        mut stamped_headers,
        controls,
        is_role_address,
    } = outcome
    else {
        // Forward / Reject never deliver a local copy, so there is no message
        // to stamp. In particular the admin-forwarder arm must NOT leak the
        // recipient's threshold to an external destination.
        return Ok(outcome);
    };
    let admin_default = state
        .db
        .get_spam_policy()
        .await
        .map_err(internal)?
        .effective()
        .max_score_before_spam_folder;
    let account_override = state
        .db
        .get_spam_threshold_override(&actor_id)
        .await
        .map_err(internal)?;
    let resolved = fauna_mail::aliases::resolve_delivery_spam_threshold(
        controls.spam_threshold_override,
        account_override,
        admin_default,
    );
    stamped_headers.push((
        fauna_mail::aliases::HEADER_SPAM_THRESHOLD.to_string(),
        resolved.to_string(),
    ));
    Ok(LocalRecipientOutcome::Mailbox {
        actor_id,
        stamped_headers,
        controls,
        is_role_address,
    })
}

/// Resolve `local_part@domain` through the full fixed-order alias resolver
/// (`fauna_mail::aliases::resolve_recipient`): gather the DB-backed candidates,
/// run the pure matcher, and apply the side effects it signals. `domain` and
/// `local_part` are lowercased here; `sender_domain` rides into the alias-hit
/// log (the MTA's MAIL FROM domain; empty when unknown).
///
/// Delivery callers want [`resolve_local_recipient`], which additionally
/// applies the delivery-time spam-threshold stamp. The one non-delivery
/// caller is the app door's sender-ownership gate
/// (`email_handlers::send_handler`), which asks only *who owns this address*
/// — the same question the MTA asks over `fauna.bridges.resolve_recipient`
/// for its `MAIL FROM` and `From:` (`mail-multidomain.md` § From: header
/// ownership) — and must not stamp anything.
pub(crate) async fn resolve_local_recipient_unstamped(
    state: &Arc<AppState>,
    domain: &str,
    local_part: &str,
    sender_domain: &str,
) -> Result<LocalRecipientOutcome, RpcError> {
    let local_part = local_part.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();

    // Effective alias policy (admin overrides over the
    // `fauna_mail::aliases` const defaults) — gates the +suffix and
    // wildcard resolution steps (`put_alias_policy`). Read per-call: no
    // hot-reload, but
    // also no cache to invalidate.
    let alias_policy = state
        .db
        .get_alias_policy()
        .await
        .map_err(internal)?
        .effective();

    // The per-domain `mail_domains` row carries both the catch-all designated
    // actor (set at `add_local_domain`; not an `account_aliases` row) and the
    // role-address override map — one lookup feeds both (no extra query).
    let mail_domain = state
        .db
        .lookup_active_mail_domain(&domain)
        .await
        .map_err(internal)?;
    let catch_all_actor = mail_domain.as_ref().and_then(|d| d.catch_all_actor_id);
    let role_address_overrides = fauna_mail::aliases::role_overrides::parse_stored(
        mail_domain
            .as_ref()
            .and_then(|d| d.role_address_overrides_json.as_deref()),
    );

    // Exact + (+suffix base) candidates, via the exact index.
    let exact = state
        .db
        .lookup_exact_alias_record(&domain, &local_part)
        .await
        .map_err(internal)?
        .as_ref()
        .map(record_to_exact_candidate);

    // Forwarder candidate (resolver step 2) — on the same exact key,
    // fetched only when exact missed (exact and forwarder are mutually
    // exclusive on a key, and exact wins the tier; no wasted query when
    // exact won). Present ⇒ the matcher yields a `Forward` outcome.
    let forwarder = if exact.is_none() {
        state
            .db
            .lookup_forwarder_alias_record(&domain, &local_part)
            .await
            .map_err(internal)?
            .as_ref()
            .map(record_to_forwarder_candidate)
    } else {
        None
    };

    // List candidate (resolver step 2, cont.) — a list's posting address on
    // the same exact key, looked up only when exact and forwarder both missed.
    // Present ⇒ the matcher refuses: a list only sends
    // (`mail-mass-mailing.md` § Pattern).
    let list_address = exact.is_none()
        && forwarder.is_none()
        && state
            .db
            .lookup_list_id_for_address(&domain, &local_part)
            .await
            .map_err(internal)?
            .is_some();

    let subaddressing_enabled = alias_policy.subaddressing_enabled;
    let subaddress_base = match (subaddressing_enabled, &exact) {
        // Only look up the base when we have a valid sub-address and
        // no exact match already won.
        (true, None) => match fauna_mail::aliases::split_subaddress(&local_part) {
            fauna_mail::aliases::SubaddressSplit::Valid { base, .. } => state
                .db
                .lookup_exact_alias_record(&domain, &base)
                .await
                .map_err(internal)?
                .as_ref()
                .map(record_to_exact_candidate),
            _ => None,
        },
        _ => None,
    };

    // Disposable candidate (resolver step 3) — only when exact missed
    // (exact wins, no decrement) and the local-part decodes to a token
    // that matches a row. Aliveness is computed handler-side (it owns
    // the clock); the matcher orders + signals the consume.
    let disposable = if exact.is_none() {
        match fauna_mail::aliases::split_disposable(&local_part) {
            Some(token) => state
                .db
                .lookup_disposable_alias_record(&domain, &token)
                .await
                .map_err(internal)?
                .map(|rec| fauna_mail::aliases::DisposableCandidate {
                    alias_id: rec.alias_id,
                    actor_id: rec.actor_id,
                    token,
                    disabled: rec.disabled,
                    alive: fauna_mail::aliases::disposable_alive(
                        rec.expires_at,
                        rec.uses_remaining,
                        crate::db::now_epoch_millis(),
                    ),
                    controls: alias_record_controls(&rec),
                }),
            None => None,
        }
    } else {
        None
    };

    // Wildcard candidates for the domain (bounded).
    let wildcard_prefix_enabled = alias_policy.wildcard_prefix_enabled;
    let wildcards: Vec<fauna_mail::aliases::WildcardCandidate> = if wildcard_prefix_enabled {
        state
            .db
            .list_wildcard_aliases_for_domain(&domain)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|r| fauna_mail::aliases::WildcardCandidate {
                alias_id: r.alias_id,
                actor_id: r.actor_id,
                prefix: r.pattern.clone(),
                disabled: r.disabled,
                controls: alias_record_controls(&r),
            })
            .collect()
    } else {
        Vec::new()
    };

    // Role-address fallback (resolver step 6) — an RFC 2142 reserved
    // local-part (postmaster@/abuse@/…) with no explicit alias never
    // rejects; it routes to the deployment admin's mailbox ahead of
    // catch-all (`smtp-server.md` § abuse@/postmaster@ routing). This is
    // the T2.5 routing `validate_recipient` carries, brought into the
    // resolver superset. Only computed on an exact miss (an admin who
    // registered `postmaster@` as a real mailbox wins at step 1, and the
    // lookup costs nothing then); a reserved name with **no** admin
    // claimed surfaces an internal error so the Go MTA tempfails 451 —
    // never a hard 550 — holding the never-reject invariant exactly as
    // `validate_recipient` does. The matcher only orders it (step 6); it
    // does no I/O.
    let role_route = fauna_mail::aliases::classify_role_address(
        &local_part,
        fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS,
    );
    let role_address_target =
        if exact.is_none() && role_route != fauna_mail::aliases::RoleAddressRoute::NotReserved {
            // Resolve the deployment admin — the fallback for an unset/invalid
            // override, and the deployment-wide processor target for tlsrpt/dmarc.
            let admins = state.db.list_admin_actors().await.map_err(internal)?;
            let admin = match admins.first() {
                Some((admin_actor, _added_at)) => {
                    let mut id = [0u8; 32];
                    id.copy_from_slice(admin_actor);
                    id
                }
                None => {
                    return Err(internal(format!(
                        "role address {local_part}@{domain} has no admin mailbox to route to \
                             (deployment not yet claimed)"
                    )));
                }
            };
            Some(match role_route {
                // postmaster/abuse/noc/security → the per-domain override actor if
                // set + valid, else the deployment admin (`mail-multidomain.md`
                // § Per-domain override). A malformed/absent override degrades to
                // admin (never 550 — the never-reject invariant).
                fauna_mail::aliases::RoleAddressRoute::AdminMailbox => {
                    role_address_overrides.resolve(&local_part).unwrap_or(admin)
                }
                // tlsrpt/dmarc-report → the deployment-wide processor (the admin
                // mailbox today, until a separate processor actor exists); the
                // per-domain override is ignored for these.
                _ => admin,
            })
        } else {
            None
        };

    let input = fauna_mail::aliases::ResolverInput {
        local_part: &local_part,
        domain: &domain,
        subaddressing_enabled,
        wildcard_prefix_enabled,
        catch_all_actor,
        exact,
        forwarder,
        subaddress_base,
        disposable,
        wildcards: &wildcards,
        role_address_target,
        list_address,
    };
    match fauna_mail::aliases::resolve_recipient(&input) {
        fauna_mail::aliases::RecipientResolution::Resolved {
            actor_id,
            stamped_headers,
            controls,
            matched,
        } => {
            // Resolve-time side effects the matcher signalled (it does
            // no I/O): the rate-cap gate, the disposable decrement, then
            // the hit log.
            if let Some(m) = matched {
                // 0. Per-alias rate cap (`mail-aliases.md` § Per-alias
                // rate-cap). Enforced HERE — nest-side, inside the resolve
                // the MTA already round-trips for every RCPT — rather than
                // in the Go MTA over the `control_overrides` it receives:
                // the count lives in `alias_hits` (durable across a bridge
                // restart, where a bridge-local bucket would reset), all
                // three RCPT call sites (inbound MX + both submission
                // paths) go through this one function, and the reply costs
                // no extra round trip. The bridge needs no new code — its
                // `rejectFromResolver` already renders a 4xx resolver
                // Reject as `451 4.7.0 <reason>`, the exact wire string
                // § Per-alias rate-cap ratifies.
                //
                // Ordering is load-bearing: the gate runs BEFORE the
                // disposable decrement and BEFORE the hit log, so a
                // tempfailed message burns no `uses_remaining` and leaves
                // no `alias_hits` row. Were the hit logged first, a
                // retrying sender would keep re-arming their own cap and
                // the window could never drain.
                if controls.has_rate_cap() {
                    let now = crate::db::now_epoch_millis();
                    let (hour_hits, day_hits) = state
                        .db
                        .count_alias_hits_in_windows(
                            &m.alias_id,
                            now - fauna_mail::aliases::RATE_CAP_HOUR_WINDOW_MS,
                            now - fauna_mail::aliases::RATE_CAP_DAY_WINDOW_MS,
                        )
                        .await
                        .map_err(internal)?;
                    if fauna_mail::aliases::rate_cap_exceeded(&controls, hour_hits, day_hits) {
                        tracing::info!(
                            target: "mail_aliases",
                            hour_hits,
                            day_hits,
                            per_hour = ?controls.rate_limit_per_hour,
                            per_day = ?controls.rate_limit_per_day,
                            "alias over its rate cap; tempfailing the recipient"
                        );
                        return Ok(LocalRecipientOutcome::Reject {
                            smtp_code: fauna_mail::aliases::RATE_CAP_REJECT_CODE,
                            reason: fauna_mail::aliases::RATE_CAP_REJECT_REASON.into(),
                        });
                    }
                }
                // 1. A live disposable match owes an atomic
                // `uses_remaining` decrement. Losing the last-use race
                // (or a row deleted between lookup and decrement) →
                // reject as expired, logging **no** hit (the route
                // failed).
                if m.consume_disposable {
                    use crate::db::mail_aliases::ConsumeDisposableOutcome::*;
                    match state
                        .db
                        .consume_disposable_use(&m.alias_id)
                        .await
                        .map_err(internal)?
                    {
                        Decremented | Unlimited => {}
                        Exhausted | NotFound => {
                            return Ok(LocalRecipientOutcome::Reject {
                                smtp_code: 550,
                                reason: "Address expired".into(),
                            });
                        }
                    }
                }
                // 2. Log the hit against the matched stored row + bump
                // its `last_hit_at`/`hit_count` (catch-all has no row,
                // so `matched` is None there → no hit). `sender_domain`
                // rides the request from the MTA's MAIL FROM (empty
                // until the Go-MTA cutover passes it).
                let matched_address = format!("{local_part}@{domain}");
                state
                    .db
                    .log_alias_hit(
                        &m.alias_id,
                        &matched_address,
                        sender_domain,
                        crate::db::now_epoch_millis(),
                    )
                    .await
                    .map_err(internal)?;
            }
            Ok(LocalRecipientOutcome::Mailbox {
                actor_id,
                stamped_headers,
                controls,
                // A normal alias/exact/+suffix/disposable/wildcard/
                // catch-all match — per-mailbox quota applies on inbound
                // delivery. Only the RoleAddress arm below sets this.
                is_role_address: false,
            })
        }
        fauna_mail::aliases::RecipientResolution::RoleAddress { target_actor_id } => {
            // RFC 2142 reserved local-part with no explicit alias — the
            // never-reject route to the resolved target (the per-domain role
            // override actor if set, else the deployment admin; resolver step
            // 6, ahead of catch-all). No headers, no `alias_hits` row
            // (there is no stored alias — the route is the RFC default),
            // and `is_role_address=true` so the Go MTA echoes the quota +
            // greylist bypass onto ingest (`smtp-server.md`
            // § abuse@/postmaster@ routing).
            Ok(LocalRecipientOutcome::Mailbox {
                actor_id: target_actor_id,
                stamped_headers: Vec::new(),
                controls: fauna_mail::aliases::ResolvedControls::default(),
                is_role_address: true,
            })
        }
        fauna_mail::aliases::RecipientResolution::Forward {
            forward_target,
            forwarder_actor_id,
        } => {
            // No local delivery, no hit logged this slice (no admin
            // forwarder-hit-list RPC yet). The MTA hands this to the
            // forward dispatch once the RCPT-TO cutover lands.
            Ok(LocalRecipientOutcome::Forward {
                forward_target,
                forwarder_actor_id,
            })
        }
        fauna_mail::aliases::RecipientResolution::Reject { smtp_code, reason } => {
            Ok(LocalRecipientOutcome::Reject { smtp_code, reason })
        }
    }
}

/// Recognize an RFC 8058 mailto one-click-unsubscribe RCPT local-part
/// (`unsubscribe@` / `unsubscribe+<token>@`, `mail-mass-mailing.md`
/// § The mailto handler). The outer `Option` is `Some` iff the base local-part
/// (before the first `+`) is `unsubscribe` (case-insensitive); the inner
/// `Option<&str>` is `Some(<token>)` for the `+<token>` form (preserved
/// verbatim — base64url is case-sensitive, unlike the lowercased base) and
/// `None` for the bare form. Any other local-part → `None`. Splits on the
/// FIRST `+`, matching the +suffix convention (`split_subaddress`); a token
/// containing further `+` keeps them.
fn match_unsubscribe_local_part(local_part: &str) -> Option<Option<&str>> {
    match local_part.split_once('+') {
        Some((base, token)) if base.eq_ignore_ascii_case("unsubscribe") => Some(Some(token)),
        Some(_) => None,
        None if local_part.eq_ignore_ascii_case("unsubscribe") => Some(None),
        None => None,
    }
}

/// `fauna.bridges.resolve_recipient` — the MTA's RCPT-TO recipient resolver.
/// A thin wire wrapper over [`resolve_local_recipient`]; maps its outcome onto
/// the `ResolveRecipientReply` shape. Ahead of the resolver it intercepts the
/// RFC 8058 mailto one-click-unsubscribe address (`match_unsubscribe_local_part`).
fn resolve_recipient_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.resolve_recipient").await?;
            let req: ResolveRecipientRequest = decode(&payload).map_err(malformed)?;

            // RFC 8058 mailto one-click unsubscribe (`mail-mass-mailing.md`
            // § The mailto handler), intercepted ahead of the alias resolver so
            // it sits in front of catch-all + role-address routing and never
            // delivers to a mailbox. The local-part base is matched
            // case-insensitively, but the `+<token>` suffix is preserved
            // verbatim — it is a case-sensitive base64url value (the same token
            // the HTTPS endpoint resolves). This interception is inbound-only
            // (here, not in the shared `resolve_local_recipient`), so a stray
            // outbound submission to `unsubscribe+…@own-domain` does not fire
            // the flip via the in-domain partition.
            if let Some(token) = match_unsubscribe_local_part(&req.local_part) {
                let reply = match token {
                    // `unsubscribe+<token>@` — flip the member by the cached
                    // token index, fire-and-forget: `250`-discard regardless of
                    // whether the token matched (per the goal doc — the success
                    // state is "the recipient is unsubscribed"; an unknown token
                    // is a no-op, not a bounce, since most MUAs ignore mailto
                    // bounces anyway). The body is never parsed.
                    Some(token) => {
                        if let Err(e) = state.db.unsubscribe_member_by_token(token).await {
                            tracing::warn!(
                                target: "mail_lists",
                                error = %e,
                                "mailto one-click unsubscribe flip failed"
                            );
                        }
                        ResolveRecipientReply::Discard
                    }
                    // Bare `unsubscribe@` with no token — a confused human, not
                    // an MUA one-click. Reject with a plain-language `550`
                    // (`mail-mass-mailing.md` § Reserved local-part).
                    None => ResolveRecipientReply::Reject {
                        smtp_code: 550,
                        reason: "unsubscribe@ requires a list-unsubscribe token".into(),
                    },
                };
                return encode_reply(&reply);
            }

            let reply = match resolve_local_recipient(
                &state,
                &req.domain,
                &req.local_part,
                &req.sender_domain,
            )
            .await?
            {
                LocalRecipientOutcome::Mailbox {
                    actor_id,
                    stamped_headers,
                    controls,
                    is_role_address,
                } => {
                    // Guardian mail gate, `reject` arm (`family-safety.md`
                    // § The mail gate). This is the ONE stage at which a mail
                    // refusal can be per-recipient: SMTP resolves recipients one
                    // at a time at `RCPT TO` but emits a single reply for `DATA`,
                    // so refusing at ingest would bounce the message for every
                    // recipient — a stranger mailing both the ward and a parent
                    // would deny the parent their copy too. The nest decides;
                    // the bridge only relays the verdict.
                    // An empty `sender_address` — the null reverse-path — can
                    // never Reject here: DSN-ness is undecidable before DATA,
                    // so `from_envelope` routes it to the null-path arm whose
                    // strictest verdict is Hold, applied at ingest instead.
                    if guardian_mail_verdict(
                        &state,
                        &actor_id,
                        MailIngress::from_envelope(&req.sender_address, None),
                    )
                    .await?
                    .verdict
                        == MailVerdict::Reject
                    {
                        // Refuse with the SAME reply a nonexistent address gets
                        // (`aliases::resolve_recipient`'s `550 "User unknown"`,
                        // routed through `LocalRecipientOutcome::Reject` above) —
                        // never a distinct "approved senders only" text. A
                        // sharper reject would reveal to any off-allowlist
                        // stranger that this address exists AND is a
                        // policy-restricted (typically a minor's) ward — a
                        // metadata leak about a vulnerable user. Recipient
                        // existence stays exactly as (in)visible as SMTP already
                        // makes it; the ward is not *additionally*
                        // distinguishable. (network-exposure.md § Rulings F5;
                        // family-safety.md § The mail gate.)
                        return encode_reply(&ResolveRecipientReply::Reject {
                            smtp_code: 550,
                            reason: "User unknown".into(),
                        });
                    }
                    ResolveRecipientReply::Resolved {
                        actor_id: ByteBuf::from(actor_id.to_vec()),
                        headers_to_stamp: stamped_headers
                            .into_iter()
                            .map(|(name, value)| StampedHeader { name, value })
                            .collect(),
                        control_overrides: AliasControls {
                            label: String::new(),
                            spam_threshold_override: controls.spam_threshold_override,
                            rate_limit_per_hour: controls.rate_limit_per_hour,
                            rate_limit_per_day: controls.rate_limit_per_day,
                        },
                        is_role_address,
                    }
                }
                LocalRecipientOutcome::Forward {
                    forward_target,
                    forwarder_actor_id,
                } => ResolveRecipientReply::Forward {
                    forward_target,
                    forwarder_actor_id: ByteBuf::from(forwarder_actor_id.to_vec()),
                },
                LocalRecipientOutcome::Reject { smtp_code, reason } => {
                    ResolveRecipientReply::Reject { smtp_code, reason }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── Per-account alias user surface (A2.1) ──
//
// User-class CRUD over `account_aliases`, wire shapes per
// `mail-aliases.md` § Wire shapes. The owning actor is the authenticated
// caller (a user manages their *own* aliases only); every write is
// owner-scoped at the DB layer. Slice 1 is exact-kind; wildcard/+suffix
// (A2.2), disposable (A2.3), and the alias-hit audit (A2.4) follow.

/// db `AliasRecord` → wire `AliasRow`.
fn alias_record_to_row(r: crate::db::mail_aliases::AliasRecord) -> AliasRow {
    AliasRow {
        alias_id: ByteBuf::from(r.alias_id.to_vec()),
        actor_id: ByteBuf::from(r.actor_id.to_vec()),
        local_domain: r.local_domain,
        kind: r.kind,
        pattern: r.pattern,
        forward_target: r.forward_target,
        label: r.label,
        disabled: r.disabled,
        spam_threshold_override: r.spam_threshold_override,
        rate_limit_per_hour: r.rate_limit_per_hour,
        rate_limit_per_day: r.rate_limit_per_day,
        uses_remaining: r.uses_remaining,
        expires_at: r.expires_at,
        created_at: r.created_at,
        last_hit_at: r.last_hit_at,
        hit_count: r.hit_count,
        // Set per-row by `list_account_aliases_handler` from the runtime
        // canonical address; the bare projection defaults it false.
        is_canonical: false,
    }
}

/// wire `AliasControls` → db `AliasControlsInput`.
fn controls_to_input(c: AliasControls) -> crate::db::mail_aliases::AliasControlsInput {
    crate::db::mail_aliases::AliasControlsInput {
        label: c.label,
        spam_threshold_override: c.spam_threshold_override,
        rate_limit_per_hour: c.rate_limit_per_hour,
        rate_limit_per_day: c.rate_limit_per_day,
    }
}

/// Validate an exact-alias local-part via the shared `fauna_mail::aliases`
/// predicates, mapping `Reserved` to the dedicated wire code and the rest
/// (empty / too-long / bad-char) to `fauna.protocol.malformed`. `reserved`
/// is the admin-tunable reserved-local-part list (`put_alias_policy`
/// effective value), defaulting to `DEFAULT_RESERVED_LOCAL_PARTS`.
pub(crate) fn validate_alias_pattern(pattern: &str, reserved: &[&str]) -> Result<(), RpcError> {
    use fauna_mail::aliases::{AliasValidationError, validate_exact_local_part};
    match validate_exact_local_part(pattern, reserved) {
        Ok(()) => Ok(()),
        Err(AliasValidationError::Reserved(p)) => Err(reserved_local_part(&p)),
        Err(other) => Err(malformed(other)),
    }
}

/// Validate a **wildcard_prefix** pattern via the shared
/// `fauna_mail::aliases::validate_wildcard_prefix`, mapping the reserved-glob
/// violation to its dedicated wire code and the structural ones (empty /
/// too-long / bad-char / missing-dash / too-short) to `fauna.protocol.malformed`.
/// `reserved` is the admin-tunable reserved-local-part list (so a wildcard
/// can't shadow an admin-added reserved name either).
fn validate_wildcard_alias_pattern(prefix: &str, reserved: &[&str]) -> Result<(), RpcError> {
    use fauna_mail::aliases::{AliasValidationError, validate_wildcard_prefix};
    match validate_wildcard_prefix(prefix, reserved) {
        Ok(()) => Ok(()),
        Err(AliasValidationError::ReservedInWildcard(p)) => {
            Err(reserved_local_part_in_wildcard(&p))
        }
        Err(other) => Err(malformed(other)),
    }
}

/// Label cap (`mail-aliases.md` § Label — max 64 UTF-8 chars).
pub(crate) fn validate_label(label: &str) -> Result<(), RpcError> {
    if label.chars().count() > 64 {
        return Err(malformed("alias label exceeds 64 characters"));
    }
    Ok(())
}

/// Validate a whole wire [`AliasControls`] bundle — the label length above plus
/// the **non-negative** rate caps.
///
/// `mail-aliases.md` § Per-alias rate-cap gives `rate_limit_per_hour` /
/// `rate_limit_per_day` the semantics `None` = unlimited and an over-quota
/// message tempfailed `451 4.7.0 Rate limit exceeded`; a *negative* cap is
/// meaningless under that contract, and the shared client-side parser
/// (`fauna_core::format::parse_count_i64`, § Where the override-input parse
/// lives) already refuses one. The nest is the trust boundary, so it refuses one
/// too — a non-conforming client can still send one,
/// and this door writes a column the enforcement
/// gate is still being built against, so the value must
/// be sane *before* a consumer starts trusting it. Same `0`-is-meaningful
/// boundary as the admin tier caps: the refusal is `< 0`, never `< 1`.
///
/// One validator for both the create and update doors so they cannot drift
/// (priority #1) — `AliasControls` is the shared input bundle for both.
pub(crate) fn validate_alias_controls(c: &AliasControls) -> Result<(), RpcError> {
    validate_label(&c.label)?;
    for (field, value) in [
        ("rate_limit_per_hour", c.rate_limit_per_hour),
        ("rate_limit_per_day", c.rate_limit_per_day),
    ] {
        if let Some(v) = value
            && v < 0
        {
            return Err(malformed(format!(
                "{field} must be non-negative (got {v}); omit it for unlimited"
            )));
        }
    }
    Ok(())
}

fn map_alias_write_err_rpc(e: crate::db::mail_aliases::AliasWriteError) -> RpcError {
    match e {
        crate::db::mail_aliases::AliasWriteError::Conflict
        | crate::db::mail_aliases::AliasWriteError::KeyHeldByOtherKind => {
            conflicts_with_existing_alias()
        }
        crate::db::mail_aliases::AliasWriteError::Db(err) => internal(err),
    }
}

fn alias_id_from_wire(b: &ByteBuf) -> Result<[u8; 16], RpcError> {
    b.as_ref()
        .try_into()
        .map_err(|_| malformed("alias_id must be 16 bytes"))
}

fn list_account_aliases_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_account_aliases").await?;
            let _req: ListAccountAliasesRequest = decode(&payload).map_err(malformed)?;
            let records = state
                .db
                .list_aliases_for_actor(&actor_id)
                .await
                .map_err(internal)?;
            // The canonical `<handle>@<domain>` exact alias is resolved from the
            // runtime primary mail domain (the same source the revoke/delete/
            // update guards use) — never stored — so clients can render it
            // read-only (`mail-aliases.md` § Aliases UX). `None` (handle-less
            // actor / no mail domain) ⇒ no row is canonical.
            let canonical = canonical_address_for_actor(&state, &actor_id).await?;
            let aliases = records
                .into_iter()
                .map(|r| {
                    let is_canonical = canonical.as_ref().is_some_and(|(domain, localpart)| {
                        r.kind == fauna_mail::aliases::ALIAS_KIND_EXACT
                            && r.local_domain.eq_ignore_ascii_case(domain)
                            && r.pattern.eq_ignore_ascii_case(localpart)
                    });
                    let mut row = alias_record_to_row(r);
                    row.is_canonical = is_canonical;
                    row
                })
                .collect();
            encode_reply(&ListAccountAliasesReply { aliases })
        })
    })
}

fn create_account_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.create_account_alias").await?;
            let req: CreateAccountAliasRequest = decode(&payload).map_err(malformed)?;
            use crate::db::mail_aliases::{ALIAS_KIND_EXACT, ALIAS_KIND_WILDCARD_PREFIX};
            if req.local_domain.trim().is_empty() {
                return Err(malformed("local_domain must not be empty"));
            }
            validate_alias_controls(&req.controls)?;
            // Effective alias policy: the admin-tunable reserved-local-part
            // list + exact-alias cap (`put_alias_policy`) over the
            // `fauna_mail::aliases` const
            // defaults. `reserved_local_parts` here is the single tunable
            // list the future admin-forwarder create-time check also reads
            // (§ AF).
            let alias_policy = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective();
            let reserved: Vec<&str> = alias_policy
                .reserved_local_parts
                .iter()
                .map(String::as_str)
                .collect();
            // Per-kind validation + conflict checks. Exact and wildcard_prefix
            // are user-creatable (A2.1 / A2.2); disposable mints via the
            // separate `generate_disposable_alias` RPC (A2.3); catch-all is
            // admin policy, never a user row. The `(local_domain, pattern,
            // kind)` UNIQUE constraint is the cross-user same-pattern guard
            // (incl. wildcard-vs-wildcard) below.
            match req.kind.as_str() {
                ALIAS_KIND_EXACT => {
                    validate_alias_pattern(&req.pattern, &reserved)?;
                    // Exact, forwarder and list are mutually exclusive on a
                    // key (`mail-aliases.md` § Resolution order): the insert
                    // below refuses a key another tier kind holds
                    // (`exact_key_held_by_other_kind`).
                    // Per-account exact-alias cap (per actor, across all domains).
                    let count = state
                        .db
                        .count_exact_aliases_for_actor(&actor_id)
                        .await
                        .map_err(internal)?;
                    if count >= alias_policy.exact_aliases_max {
                        return Err(alias_cap_exceeded(alias_policy.exact_aliases_max));
                    }
                }
                ALIAS_KIND_WILDCARD_PREFIX => {
                    validate_wildcard_alias_pattern(&req.pattern, &reserved)?;
                    // One wildcard pattern per actor (§ Kind 3 :52).
                    if state
                        .db
                        .actor_has_wildcard(&actor_id)
                        .await
                        .map_err(internal)?
                    {
                        return Err(actor_already_has_wildcard());
                    }
                    // A wildcard must not shadow another user's exact alias
                    // (§ Cross-user uniqueness :284) — exact wins at resolution.
                    if state
                        .db
                        .exact_alias_exists_under_prefix(&req.local_domain, &req.pattern)
                        .await
                        .map_err(internal)?
                    {
                        return Err(conflicts_with_existing_alias());
                    }
                }
                other => {
                    return Err(malformed(format!(
                        "kind '{other}' is not user-creatable: 'disposable' is minted via \
                         generate_disposable_alias (A2.3), 'catchall' is admin policy"
                    )));
                }
            }
            let alias_id = state
                .db
                .create_account_alias(
                    &actor_id,
                    &req.local_domain,
                    &req.kind,
                    &req.pattern,
                    &controls_to_input(req.controls),
                )
                .await
                .map_err(map_alias_write_err_rpc)?;
            encode_reply(&CreateAccountAliasReply {
                alias_id: ByteBuf::from(alias_id.to_vec()),
            })
        })
    })
}

/// `fauna.bridges.import_account_aliases` (User) — bulk-create **exact**
/// aliases from a list of full addresses (one per `lines` entry). Best-effort
/// per line: a malformed / domain-not-local / over-cap / duplicate line is
/// *reported* as an `ImportAliasOutcome`, never aborting the batch; only a
/// genuine DB fault aborts. Idempotent — re-importing an existing address is
/// `SkippedDuplicate`. This is the recipient-whitelist import path
/// (`docs/goal/behavior/mail-aliases.md` § Bulk import).
fn import_account_aliases_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.import_account_aliases").await?;
            let req: ImportAccountAliasesRequest = decode(&payload).map_err(malformed)?;
            use crate::db::mail_aliases::{ALIAS_KIND_EXACT, AliasControlsInput, AliasWriteError};

            // Load the owned-domain set + alias policy ONCE for the whole batch.
            // Unlike single `create_account_alias` (which trusts the domain from
            // a UI dropdown of owned domains), bulk import validates each line's
            // domain against `mail_domains` — a pasted list can name anything.
            let owned: std::collections::HashSet<String> = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .into_iter()
                .map(|d| d.domain_name.to_ascii_lowercase())
                .collect();
            let alias_policy = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective();
            let reserved: Vec<&str> = alias_policy
                .reserved_local_parts
                .iter()
                .map(String::as_str)
                .collect();
            // Per-actor exact-alias cap (across all domains) — count once, then
            // track locally as the batch inserts.
            let mut count = state
                .db
                .count_exact_aliases_for_actor(&actor_id)
                .await
                .map_err(internal)?;

            let invalid = |idx: u32, addr: &str, reason: &str| ImportAliasOutcome {
                line_index: idx,
                address: addr.to_string(),
                status: ImportAliasStatus::Invalid,
                reason: Some(reason.to_string()),
            };

            let mut results: Vec<ImportAliasOutcome> = Vec::new();
            for (i, raw_line) in req.lines.iter().enumerate() {
                let line = raw_line.trim();
                if line.is_empty() {
                    continue; // blank lines produce no outcome
                }
                let idx = i as u32;

                // Split "<local-part>@<domain>" from the right.
                let (local_part, domain) = match line.rsplit_once('@') {
                    Some((lp, dom)) if !lp.is_empty() && !dom.is_empty() => {
                        (lp.to_string(), dom.to_ascii_lowercase())
                    }
                    _ => {
                        results.push(invalid(idx, line, "malformed address"));
                        continue;
                    }
                };

                if !owned.contains(&domain) {
                    results.push(invalid(idx, line, "domain not local"));
                    continue;
                }
                if validate_alias_pattern(&local_part, &reserved).is_err() {
                    results.push(invalid(idx, line, "invalid local part"));
                    continue;
                }
                if count >= alias_policy.exact_aliases_max {
                    results.push(invalid(idx, line, "alias cap reached"));
                    continue;
                }

                match state
                    .db
                    .create_account_alias(
                        &actor_id,
                        &domain,
                        ALIAS_KIND_EXACT,
                        &local_part,
                        &AliasControlsInput::default(),
                    )
                    .await
                {
                    Ok(_alias_id) => {
                        count += 1;
                        results.push(ImportAliasOutcome {
                            line_index: idx,
                            address: line.to_string(),
                            status: ImportAliasStatus::Created,
                            reason: None,
                        });
                    }
                    // The UNIQUE(local_domain,pattern,kind) index rejected a
                    // same-address row already present → report, don't abort.
                    Err(AliasWriteError::Conflict) => {
                        results.push(ImportAliasOutcome {
                            line_index: idx,
                            address: line.to_string(),
                            status: ImportAliasStatus::SkippedDuplicate,
                            reason: None,
                        });
                    }
                    // An admin forwarder or another user's list already holds
                    // the key (the exact-key tier's one-holder rule, which the
                    // UNIQUE index cannot see across kinds) → report, don't abort.
                    Err(AliasWriteError::KeyHeldByOtherKind) => {
                        results.push(ImportAliasOutcome {
                            line_index: idx,
                            address: line.to_string(),
                            status: ImportAliasStatus::SkippedDuplicate,
                            reason: Some("conflicts with existing alias".into()),
                        });
                    }
                    // A genuine DB fault is not a per-line outcome — surface it.
                    Err(AliasWriteError::Db(e)) => return Err(internal(e)),
                }
            }

            encode_reply(&ImportAccountAliasesReply { results })
        })
    })
}

fn update_account_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.update_account_alias").await?;
            let req: UpdateAccountAliasRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            // The canonical `<handle>@<domain>` exact alias's localpart is the
            // AUTH-login identity (`validate_recipient`) + primary mailbox.
            // `update` is full-overwrite, so a *pattern* change would rewrite
            // the canonical address and strand login — the same unrecoverable
            // class the revoke/delete guards reject (`mail-aliases.md` §
            // Disable / § Aliases UX). Editing its label/spam/rate is fine;
            // only renaming the localpart is rejected. Clients also render the
            // canonical row read-only, but this guard makes the protection
            // uniform across every (incl. non-conforming) client.
            if is_canonical_alias(&state, &actor_id, &alias_id).await? {
                let keeps_pattern = canonical_address_for_actor(&state, &actor_id)
                    .await?
                    .is_some_and(|(_, localpart)| {
                        req.pattern.trim().eq_ignore_ascii_case(&localpart)
                    });
                if !keeps_pattern {
                    return Err(canonical_alias_protected());
                }
            }
            // Re-validate the (editable) pattern against the admin-tunable
            // reserved list (`put_alias_policy`).
            let reserved_owned: Vec<String> = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective()
                .reserved_local_parts;
            let reserved: Vec<&str> = reserved_owned.iter().map(String::as_str).collect();
            validate_alias_pattern(&req.pattern, &reserved)?;
            validate_alias_controls(&req.controls)?;
            let updated = state
                .db
                .update_alias_controls(
                    &alias_id,
                    &actor_id,
                    &req.pattern,
                    &controls_to_input(req.controls),
                )
                .await
                .map_err(map_alias_write_err_rpc)?;
            if !updated {
                return Err(not_found("alias not found or not owned by caller"));
            }
            encode_reply(&UpdateAccountAliasReply { ok: true })
        })
    })
}

fn revoke_account_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.revoke_account_alias").await?;
            let req: RevokeAccountAliasRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            // The canonical `<handle>@<domain>` alias is the primary mailbox +
            // AUTH identity — disabling it strands mail + login unrecoverably.
            if is_canonical_alias(&state, &actor_id, &alias_id).await? {
                return Err(canonical_alias_protected());
            }
            let ok = state
                .db
                .set_alias_disabled(&alias_id, &actor_id, true)
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("alias not found or not owned by caller"));
            }
            encode_reply(&RevokeAccountAliasReply { ok: true })
        })
    })
}

/// The reverse of `revoke` — flip `disabled = false` on an owned alias so a
/// soft-off alias becomes live again (`mail-aliases.md:156`: "the user can
/// re-enable later"). Without it, `revoke` is a one-way trap.
fn enable_account_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.enable_account_alias").await?;
            let req: EnableAccountAliasRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            let ok = state
                .db
                .set_alias_disabled(&alias_id, &actor_id, false)
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("alias not found or not owned by caller"));
            }
            encode_reply(&EnableAccountAliasReply { ok: true })
        })
    })
}

fn delete_account_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.delete_account_alias").await?;
            let req: DeleteAccountAliasRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            // Deleting the canonical address is as destructive as disabling it
            // (loses the primary mailbox + AUTH identity) — and irreversible.
            if is_canonical_alias(&state, &actor_id, &alias_id).await? {
                return Err(canonical_alias_protected());
            }
            let ok = state
                .db
                .delete_account_alias(&alias_id, &actor_id)
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("alias not found or not owned by caller"));
            }
            encode_reply(&DeleteAccountAliasReply { ok: true })
        })
    })
}

// ── Admin external forwarders (mail-aliases.md § Kind 7 / § AF) ──────
//
// Admin-class CRUD over `account_aliases` `kind='forwarder'` rows. Unlike the
// User `*_account_alias` handlers, the owning actor is the **admin caller**
// (forwarders are deployment routing config, attributed to the managing admin
// for SRS / rate-cap / NDR — `mail-aliases.md` § Kind 7). The `Forward`
// resolver outcome is wired in `resolve_recipient_handler` above. The forward
// *dispatch* (SRS / loop / rate-cap / NDR) is `mail-forwarding.md`'s; it fires
// once the Go-MTA RCPT-TO cutover consumes `resolve_recipient`.

fn create_forwarder_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.create_forwarder").await?;
            let req: CreateForwarderRequest = decode(&payload).map_err(malformed)?;
            if req.local_domain.trim().is_empty() {
                return Err(malformed("local_domain must not be empty"));
            }
            let local_domain = req.local_domain.to_ascii_lowercase();

            // The deployment's hosted domains: the forwarder must live on one
            // (`mail-aliases.md` § Kind 7 `:114`), and `validate_forward_target`
            // rejects a target on any of them (`mail-forwarding.md:244` — that
            // would be a same-deployment alias, not an external forward).
            let hosted: Vec<String> = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .into_iter()
                .map(|d| d.domain_name)
                .collect();
            if !hosted.iter().any(|d| d == &local_domain) {
                return Err(malformed(format!(
                    "local_domain '{local_domain}' is not a hosted mail domain; add it first"
                )));
            }

            // The pattern obeys the same exact-alias rules + the admin-tunable
            // reserved-local-part list (`put_alias_policy` — the AF check reads
            // the tunable value, not a hardcoded const).
            let reserved_owned: Vec<String> = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective()
                .reserved_local_parts;
            let reserved: Vec<&str> = reserved_owned.iter().map(String::as_str).collect();
            validate_alias_pattern(&req.pattern, &reserved)?;

            // The external destination: RFC-5321 syntactic + must-not-be-local.
            let hosted_refs: Vec<&str> = hosted.iter().map(String::as_str).collect();
            if let Err(e) = fauna_mail::validate_forward_target(&req.forward_target, &hosted_refs) {
                return Err(malformed(format!("invalid forward_target: {e}")));
            }

            // Insert attributed to the admin caller. A forwarder-forwarder
            // collision on the key surfaces as the UNIQUE conflict; a key an
            // exact alias or list already holds (it would never resolve —
            // exact wins, `mail-aliases.md` § Resolution order) is refused by
            // the insert's exact-key tier check.
            let alias_id = state
                .db
                .create_forwarder_alias(&actor_id, &local_domain, &req.pattern, &req.forward_target)
                .await
                .map_err(map_alias_write_err_rpc)?;
            encode_reply(&CreateForwarderReply {
                alias_id: ByteBuf::from(alias_id.to_vec()),
            })
        })
    })
}

fn list_forwarders_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_forwarders").await?;
            let _req: ListForwardersRequest = decode(&payload).map_err(malformed)?;
            let records = state.db.list_forwarders().await.map_err(internal)?;
            let forwarders = records.into_iter().map(alias_record_to_row).collect();
            encode_reply(&ListForwardersReply { forwarders })
        })
    })
}

fn delete_forwarder_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.delete_forwarder").await?;
            let req: DeleteForwarderRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            let ok = state
                .db
                .delete_forwarder(&alias_id)
                .await
                .map_err(internal)?;
            if !ok {
                return Err(not_found("forwarder not found"));
            }
            encode_reply(&DeleteForwarderReply { ok: true })
        })
    })
}

/// Page-size ceiling for `list_account_alias_hits`. The audit UI shows the
/// last 100 (`mail-aliases.md` § Per-alias-hit audit list `:245`); cap at 500
/// so a client can't request an unbounded scan.
const MAX_ALIAS_HITS_PAGE: u32 = 500;

/// db `AliasHitRecord` → wire `AliasHitRow`.
fn alias_hit_record_to_row(r: crate::db::mail_aliases::AliasHitRecord) -> AliasHitRow {
    AliasHitRow {
        hit_id: ByteBuf::from(r.hit_id.to_vec()),
        matched_address: r.matched_address,
        sender_domain: r.sender_domain,
        received_at: r.received_at,
    }
}

fn list_account_alias_hits_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_account_alias_hits").await?;
            let req: ListAccountAliasHitsRequest = decode(&payload).map_err(malformed)?;
            let alias_id = alias_id_from_wire(&req.alias_id)?;
            let before_hit_id = req
                .before_hit_id
                .as_ref()
                .map(alias_id_from_wire)
                .transpose()?;
            let limit = req.limit.clamp(1, MAX_ALIAS_HITS_PAGE) as i64;
            // Owner-scoped: `None` = the alias is unknown or not the caller's
            // (never leak another user's hits — same isolation as the CRUD).
            let records = state
                .db
                .list_alias_hits_for_owner(&actor_id, &alias_id, limit, before_hit_id.as_ref())
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("alias not found or not owned by caller"))?;
            let hits = records.into_iter().map(alias_hit_record_to_row).collect();
            encode_reply(&ListAccountAliasHitsReply { hits })
        })
    })
}

/// One day in epoch-millis — the disposable TTL unit + the per-day mint
/// window.
const DISPOSABLE_DAY_MILLIS: i64 = 86_400_000;

/// Mint a fresh lowercase base32 token (the RNG is scoped out of any `.await`
/// — `ThreadRng` is `!Send`).
fn mint_disposable_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..fauna_mail::aliases::DISPOSABLE_TOKEN_LEN)
        .map(|_| {
            let i = rng.gen_range(0..fauna_mail::aliases::DISPOSABLE_TOKEN_ALPHABET.len());
            fauna_mail::aliases::DISPOSABLE_TOKEN_ALPHABET[i] as char
        })
        .collect()
}

/// `fauna.bridges.generate_disposable_alias` (User, `mail-aliases.md` § Kind
/// 5). Derives `<handle>` + `<domain>` from the actor's canonical exact alias,
/// enforces the per-day mint cap, mints a collision-free 6-char base32 token,
/// inserts a `kind='disposable'` row (`uses_remaining` + `expires_at`), and
/// returns `{alias_id, full_address, token}`.
fn generate_disposable_alias_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.generate_disposable_alias").await?;
            let req: GenerateDisposableAliasRequest = decode(&payload).map_err(malformed)?;
            validate_label(&req.label)?;
            let ttl_days = req
                .ttl_days
                .unwrap_or(fauna_mail::aliases::DISPOSABLE_DEFAULT_TTL_DAYS);
            if ttl_days == 0 {
                return Err(malformed("ttl_days must be >= 1"));
            }
            // `uses = 0` → unlimited (stored NULL); `Some(n)` → n finite uses.
            let uses = req
                .uses
                .unwrap_or(fauna_mail::aliases::DISPOSABLE_DEFAULT_USES);
            let uses_remaining = if uses == 0 { None } else { Some(uses as i64) };

            let now = crate::db::now_epoch_millis();

            // Per-day mint cap (anti-enumeration; rolling 24 h window).
            let cap = fauna_mail::aliases::DISPOSABLE_GENERATE_PER_DAY_DEFAULT;
            let recent = state
                .db
                .count_recent_disposable_mints(&actor_id, now - DISPOSABLE_DAY_MILLIS)
                .await
                .map_err(internal)?;
            if recent >= cap {
                return Err(disposable_generate_rate_limited(cap));
            }

            // `<handle>` + `<domain>` come from the actor's canonical address
            // (oldest exact alias) — the disposable is minted on that domain.
            let (local_domain, handle) = state
                .db
                .oldest_exact_alias_for_actor(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(no_canonical_address)?;

            let expires_at = now + (ttl_days as i64) * DISPOSABLE_DAY_MILLIS;

            // Mint a collision-free token. The UNIQUE(local_domain, pattern,
            // kind) constraint is the race-safe collision check — re-mint on a
            // duplicate token (32^6 space makes >1 retry vanishingly rare).
            let mut minted = None;
            for _ in 0..8 {
                let token = mint_disposable_token();
                match state
                    .db
                    .create_disposable_alias(
                        &actor_id,
                        &local_domain,
                        &token,
                        &req.label,
                        uses_remaining,
                        expires_at,
                        fauna_mail::aliases::DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT,
                    )
                    .await
                {
                    Ok(alias_id) => {
                        minted = Some((alias_id, token));
                        break;
                    }
                    // A disposable token is outside the exact-key tier, so
                    // `KeyHeldByOtherKind` cannot fire; a fresh token is the
                    // answer to either collision.
                    Err(
                        crate::db::mail_aliases::AliasWriteError::Conflict
                        | crate::db::mail_aliases::AliasWriteError::KeyHeldByOtherKind,
                    ) => continue,
                    Err(crate::db::mail_aliases::AliasWriteError::Db(e)) => {
                        return Err(internal(e));
                    }
                }
            }
            let (alias_id, token) =
                minted.ok_or_else(|| internal("disposable token collision retries exhausted"))?;

            let full_address = format!(
                "{}@{}",
                fauna_mail::aliases::disposable_local_part(&handle, &token),
                local_domain
            );
            encode_reply(&GenerateDisposableAliasReply {
                alias_id: ByteBuf::from(alias_id.to_vec()),
                full_address,
                token,
            })
        })
    })
}

// ── local-domain admin (multidomain) ────────────────────────────
//
// Admin-class WS-RPC surface over the `mail_domains` DB API
// (`db::mail_domains::*`). Wire
// shapes + scope decisions: `docs/goal/behavior/mail-multidomain.md`
// § Wire shapes (§ A1). No HTTP: the linux
// app (and all 6) drive these over WS-RPC via the shared
// `fauna_client_bridges::MailAdminClient`.

/// DB row → wire row. Parses the JSON-text `dkim_algorithms` column into
/// a real list for the wire (falls back to empty on a malformed column —
/// pre-prod, the column always carries valid JSON from the schema default).
fn mail_domain_to_row(d: &crate::db::mail_domains::MailDomain) -> MailDomainRow {
    MailDomainRow {
        domain_id: ByteBuf::from(d.domain_id.to_vec()),
        domain_name: d.domain_name.clone(),
        is_primary: d.is_primary,
        added_at: d.added_at,
        removed_at: d.removed_at,
        restored_at: d.restored_at,
        dkim_selector: d.dkim_selector.clone(),
        dkim_rotation_days: d.dkim_rotation_days,
        dkim_algorithms: serde_json::from_str(&d.dkim_algorithms_json).unwrap_or_default(),
        mta_sts_mode: d.mta_sts_mode.clone(),
        mta_sts_max_age_seconds: d.mta_sts_max_age_seconds,
        mta_sts_cert_mode: d.mta_sts_cert_mode.clone(),
        catch_all_actor_id: d.catch_all_actor_id.map(|a| ByteBuf::from(a.to_vec())),
        // The JSON object is the at-rest encoding only; the wire carries the
        // typed shape. A stored value that does not decode projects as no
        // override, so one corrupt column never fails the reply
        // (`mail-multidomain.md` § Wire shape + storage).
        role_address_overrides: fauna_mail::aliases::role_overrides::parse_stored(
            d.role_address_overrides_json.as_deref(),
        ),
        dmarc_overrides: fauna_mail::dmarc_publish::parse_stored_overrides(
            d.dmarc_overrides_json.as_deref(),
        ),
        spf_record: d.spf_record.clone(),
        dkim_selector_activated_at: d.dkim_selector_activated_at,
        // Due-ness is "as of now" by definition, so read the clock at projection
        // time (the pure helper is unit-tested deterministically). The
        // deployment-wide default lives nest-side, so the client can't compute
        // this itself. `mail-multidomain.md` § Rotation.
        dkim_rotation_due: d.is_dkim_rotation_due(
            crate::db::now_epoch_millis(),
            crate::db::mail_domains::DEFAULT_DKIM_ROTATION_DAYS,
        ),
        catch_all_cleared_by_succession_at: d.catch_all_cleared_by_succession_at,
    }
}

fn mail_domain_rename_to_row(
    r: &crate::db::mail_domain_renames::MailDomainRename,
) -> MailDomainRenameRow {
    MailDomainRenameRow {
        rename_id: ByteBuf::from(r.rename_id.to_vec()),
        old_primary_domain_id: ByteBuf::from(r.old_primary_domain_id.to_vec()),
        new_primary_domain_id: ByteBuf::from(r.new_primary_domain_id.to_vec()),
        state: r.state.clone(),
        started_at: r.started_at,
        grace_days: r.grace_days,
        cert_acquired_at: r.cert_acquired_at,
        new_cert_fingerprint: r.new_cert_fingerprint.clone(),
        flipped_at: r.flipped_at,
        grace_started_at: r.grace_started_at,
        grace_ends_at: r.grace_ends_at,
        ready_to_complete_at: r.ready_to_complete_at,
        completed_at: r.completed_at,
        aborted_at: r.aborted_at,
        abort_reason: r.abort_reason.clone(),
        initiated_by_actor_id: ByteBuf::from(r.initiated_by_actor_id.to_vec()),
    }
}

// ── Per-account forward-all (N1) ──
//
// User-class get/set over `mail_account_settings.forward_all_to`. User forward
// config rests at the **plaintext routing-metadata floor in BOTH modes** — the
// same tier as `local_domains`, aliases, and admin forwarders
// (`mail-forwarding.md` § Where the forward config lives at rest;
// `encryption-at-rest.md` § Per-content-kind conformance). The
// separate-bridge-sealed alternative (wrapped-to-bridge, the DKIM/TLS class) is
// the documented future upgrade (§ N1b),
// deferred because its only real protection is against *nest-disk* theft in a
// *separate-bridge* deployment — co-resident-theater for the single-box case —
// and it costs a per-app wrap surface not worth paying yet. Nothing reads
// the value yet — the forward-all delivery trigger is N2 (which must obtain it
// through a single chokepoint so the N1b sealed-fetch swap stays one-spot).

fn get_forward_all_to_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_forward_all_to").await?;
            let _req: GetForwardAllToRequest = decode(&payload).map_err(malformed)?;
            let forward_all_to = state
                .db
                .get_forward_all_to(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&GetForwardAllToReply { forward_all_to })
        })
    })
}

fn set_forward_all_to_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_forward_all_to").await?;
            let req: SetForwardAllToRequest = decode(&payload).map_err(malformed)?;
            // Normalize: a blank/whitespace value clears (disables forward-all).
            let target = req
                .forward_all_to
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());

            // Validate a non-empty target: RFC-5321 syntax + reject a hosted
            // `local_domains` address — that should be an alias, not a forward
            // (`mail-forwarding.md:31,:244`). The hosted case has its own code,
            // whose localized sentence names the alias remedy: an app renders
            // the code, never `details`, so `malformed` could not say it.
            if let Some(addr) = target {
                let domains = state
                    .db
                    .list_active_mail_domains()
                    .await
                    .map_err(internal)?;
                let domain_refs: Vec<&str> =
                    domains.iter().map(|d| d.domain_name.as_str()).collect();
                match fauna_mail::validate_forward_target(addr, &domain_refs) {
                    Ok(()) => {}
                    Err(fauna_mail::forward_config::ForwardTargetError::IsLocalDomain) => {
                        return Err(RpcError::new(
                            "fauna.bridges.forward_target_on_local_domain",
                            "error.bridges.forward_target_on_local_domain",
                        ));
                    }
                    Err(other) => return Err(malformed(other)),
                }
            }

            // Plaintext routing-metadata floor in both modes — no storage-mode
            // branch (the sealed wrapped-to-bridge upgrade is N1b, deferred).
            state
                .db
                .set_forward_all_to(&actor_id, target)
                .await
                .map_err(internal)?;
            encode_reply(&SetForwardAllToReply {})
        })
    })
}

/// The caller's own hourly forward cap (`mail.account.forward_per_hour`,
/// `mail-forwarding.md` § Per-account forward rate-limit), with the admin
/// ceiling it is bounded by — the one value `forward_message`'s rate cap reads.
fn get_forward_per_hour_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_forward_per_hour").await?;
            let _req: GetForwardPerHourRequest = decode(&payload).map_err(malformed)?;
            let forward_per_hour = state
                .db
                .get_forward_per_hour(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&GetForwardPerHourReply {
                forward_per_hour,
                forward_per_hour_ceiling: fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING,
                ..Default::default()
            })
        })
    })
}

fn set_forward_per_hour_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_forward_per_hour").await?;
            let req: SetForwardPerHourRequest = decode(&payload).map_err(malformed)?;
            // Lower than the admin ceiling, never higher, and never zero
            // (which would park every forward until eviction).
            fauna_mail::validate_forward_per_hour(
                req.forward_per_hour,
                fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING,
            )
            .map_err(malformed)?;
            state
                .db
                .set_forward_per_hour(&actor_id, req.forward_per_hour)
                .await
                .map_err(internal)?;
            encode_reply(&SetForwardPerHourReply {})
        })
    })
}

fn get_spam_threshold_override_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.get_spam_threshold_override",
            )
            .await?;
            let _req: GetSpamThresholdOverrideRequest = decode(&payload).map_err(malformed)?;
            let spam_threshold_override = state
                .db
                .get_spam_threshold_override(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&GetSpamThresholdOverrideReply {
                spam_threshold_override,
            })
        })
    })
}

fn set_spam_threshold_override_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.set_spam_threshold_override",
            )
            .await?;
            let req: SetSpamThresholdOverrideRequest = decode(&payload).map_err(malformed)?;
            // No normalization and no range check: the value is a `u32` (so the
            // negative the alias doors refuse is unrepresentable here), `None`
            // is the clear, and `Some(0)` is the deliberate "auto-Junk off for
            // this account" setting — collapsing it into the clear would make
            // the account tier unable to express what the alias tier can.
            state
                .db
                .set_spam_threshold_override(&actor_id, req.spam_threshold_override)
                .await
                .map_err(internal)?;
            encode_reply(&SetSpamThresholdOverrideReply {})
        })
    })
}

fn add_local_domain_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.add_local_domain").await?;
            let req: AddLocalDomainRequest = decode(&payload).map_err(malformed)?;
            if req.domain.trim().is_empty() {
                return Err(malformed("domain must not be empty"));
            }
            // A domain carrying userinfo/a path/a query/a fragment/whitespace
            // must never reach `mail_domains` at all — downstream, `resolve_
            // handle_domain(&row.domain_name).is_public_dns_name` below is a
            // negative test that would still read it as "public" and promote
            // it to the identity cache (`security.md` § Transport trust). Stays permissive on FQDN-shape/IP-literal (a
            // `localhost`/IP/`.local` add is tolerated but never promoted,
            // per the identity guard below).
            if !fauna_core::web::is_hostname_syntax(&req.domain) {
                return Err(malformed("domain must be a well-formed hostname"));
            }
            // Idempotent on domain_name: an already-active domain is a
            // no-op re-add (mail-multidomain.md § add :349).
            if let Some(existing) = state
                .db
                .lookup_active_mail_domain(&req.domain)
                .await
                .map_err(internal)?
            {
                return encode_reply(&AddLocalDomainReply {
                    domain: mail_domain_to_row(&existing),
                    skipped: true,
                });
            }
            // First domain claimed is the deployment's primary
            // (mail-multidomain.md § The primary domain). Auto-determined
            // here rather than taken from the client — can never create a
            // second primary via this path.
            let is_primary = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .is_empty();
            let catch_all: Option<[u8; 32]> = match req.catch_all_actor.as_ref() {
                None => None,
                Some(b) => Some(
                    crate::rpc_errors::require_bytes32("catch_all_actor", b.as_ref())
                        .map_err(malformed)?,
                ),
            };
            let row = state
                .db
                .add_mail_domain(
                    &req.domain,
                    is_primary,
                    // Every domain starts in `testing`; the nest's own advance
                    // is the mode's only writer after this
                    // (`mta_sts_advance`, `mail-multidomain.md` § The advance).
                    fauna_mail::outbound::mta_sts::MtaStsMode::Testing.as_str(),
                    &req.mta_sts_cert_mode,
                    catch_all.as_ref(),
                    req.dkim_selector_override.as_deref(),
                )
                .await
                .map_err(internal)?;
            // The primary `mail_domains` row IS the deployment identity: when this
            // add registered the FIRST (primary) domain — the domainless-boot →
            // claim-by-bare-handle → add-domain-from-a-client flow
            // (`domains-and-tls-bootstrap.md` § Claim) — point the sync identity
            // cache at it + self-heal TLS, so `handle_domain()` / discovery / web /
            // the ACME apex follow the added domain with no restart. This is the
            // exact step `mail_enable::ensure_mail_domain_registered` runs for the
            // claim / boot paths; the add-domain path can't delegate to that helper
            // (it hard-codes MTA-STS / drops catch-all + DKIM overrides, which this
            // handler carries from the client), so it calls the shared identity
            // primitive directly. Guarded on `is_primary` (a 2nd+ added domain never
            // reassigns identity) and on a real domain — `apply_primary_identity`
            // requires a non-local apex, and unlike `claim_core` /
            // `ensure_primary_mail_domain` this handler's caller (the admin client)
            // does not pre-gate on the host class, so a `localhost`/IP/`.local` add
            // (mail on which is nonsense) must not become the identity.
            if is_primary
                && fauna_provisioning::probe::resolve_handle_domain(&row.domain_name)
                    .is_public_dns_name
            {
                crate::identity_domain_core::apply_primary_identity(&state, &row.domain_name);
            }
            notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS).await;
            encode_reply(&AddLocalDomainReply {
                domain: mail_domain_to_row(&row),
                skipped: false,
            })
        })
    })
}

fn remove_local_domain_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.remove_local_domain").await?;
            let req: RemoveLocalDomainRequest = decode(&payload).map_err(malformed)?;
            // Refuse removing a domain that is a participant of an in-flight rename
            // (`mail-primary-domain-rename.md` § Cross-table cascade) — a mid-rename
            // removal would strand the flip with a missing anchor. The lookup is by
            // name → id, compared against the active rename's old/new participants.
            if let Some(rename) = state.db.get_active_rename().await.map_err(internal)?
                && let Some(dom) = state
                    .db
                    .lookup_active_mail_domain(&req.domain)
                    .await
                    .map_err(internal)?
                && (dom.domain_id == rename.old_primary_domain_id
                    || dom.domain_id == rename.new_primary_domain_id)
            {
                return Err(domain_in_rename_flight());
            }
            match state.db.soft_delete_mail_domain(&req.domain).await {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&RemoveLocalDomainReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

fn restore_local_domain_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.restore_local_domain").await?;
            let req: RestoreLocalDomainRequest = decode(&payload).map_err(malformed)?;
            match state.db.restore_mail_domain(&req.domain).await {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&RestoreLocalDomainReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

fn list_local_domains_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_local_domains").await?;
            let _req: ListLocalDomainsRequest = decode(&payload).map_err(malformed)?;
            let active = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?;
            let soft = state
                .db
                .list_soft_deleted_within_30d_mail_domains()
                .await
                .map_err(internal)?;
            encode_reply(&ListLocalDomainsReply {
                active: active.iter().map(mail_domain_to_row).collect(),
                soft_deleted_within_30d: soft.iter().map(mail_domain_to_row).collect(),
            })
        })
    })
}

fn update_local_domain_config_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.update_local_domain_config",
            )
            .await?;
            let req: UpdateLocalDomainConfigRequest = decode(&payload).map_err(malformed)?;
            // Plain Option<T> = leave/set; a knob needing a genuine clear has
            // its own dedicated setter (`serialization.md` § Tri-state fields).
            let update = crate::db::mail_domains::MailDomainUpdate {
                mta_sts_max_age_seconds: req.mta_sts_max_age_seconds,
                mta_sts_cert_mode: req.mta_sts_cert_mode,
                spf_record: req.spf_record,
                ..Default::default()
            };
            let result = state
                .db
                .update_mail_domain_config(&req.domain, update)
                .await;
            // The DMARC policy merges into the stored override partial (other
            // keys kept; the default clears both policy keys), so it is its own
            // read-merge-write rather than a column overwrite.
            let result = match (result, req.dmarc_policy_mode) {
                (Ok(_), Some(mode)) => state.db.set_dmarc_policy_mode(&req.domain, mode).await,
                (result, _) => result,
            };
            match result {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&UpdateLocalDomainConfigReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

// ── Primary-domain rename (mail-primary-domain-rename.md) — SLICE 1 + 2:
//    `start` validates every precondition, inserts a `requested` row, then (SLICE
//    2) auto-advances it to `cert_issuance` and wakes the cert-lifecycle loop
//    (`acme_retry_notify`) so the loop widens the single managed cert with the
//    resolve-gated `mail.<new>` SAN and, once covered, stamps `cert_ready`. No
//    cert/DNS side effect fires *in the handler* — the loop performs the widening
//    idempotently (crash-safe), and the DNS assembler advertises `mail.<new> A →
//    mail-host` while the rename is non-terminal. `abort` from any pre-flip state
//    (`requested`/`cert_issuance`/`cert_ready`) is a pure state change with
//    nothing remote to unwind. No `config_changed` push — nothing the bridge
//    fetches changes until anchor flip (a later slice). ─────────────────────────

/// `fauna.bridges.start_primary_domain_rename` (Admin) — begin a rename by
/// validating every precondition (§ Wire shapes — RPC refusal codes) and
/// inserting a `mail_domain_renames` row at `requested`. SLICE 1 stops there.
fn start_primary_domain_rename_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.start_primary_domain_rename",
            )
            .await?;
            let req: StartPrimaryDomainRenameRequest = decode(&payload).map_err(malformed)?;

            // 1. grace_days range [1, 30] (default 7 when absent).
            let grace_days = req.grace_days.unwrap_or(fauna_mail::DEFAULT_GRACE_DAYS);
            if !(fauna_mail::GRACE_DAYS_MIN..=fauna_mail::GRACE_DAYS_MAX).contains(&grace_days) {
                return Err(invalid_grace_days());
            }

            // 2. Parse the 16-byte target id.
            let new_id: [u8; 16] = req
                .new_primary_domain_id
                .as_ref()
                .try_into()
                .map_err(|_| malformed("new_primary_domain_id must be 16 bytes"))?;

            // 3. The target must exist (as some mail_domains row).
            let new = state
                .db
                .lookup_mail_domain_by_id(&new_id)
                .await
                .map_err(internal)?
                .ok_or_else(new_primary_must_be_additional)?;

            // 4. There must be a current primary to rename from. Unreachable on a
            //    claimed nest (the primary row IS the deployment identity); a guard.
            let old = state
                .db
                .lookup_primary_mail_domain()
                .await
                .map_err(internal)?
                .ok_or_else(|| malformed("no primary domain to rename"))?;

            // 5. Can't rename a domain to itself (also catches new == the primary).
            if new_id == old.domain_id {
                return Err(same_domain_for_rename());
            }

            // 6. At most one rename in flight (§ Concurrency).
            if state
                .db
                .get_active_rename()
                .await
                .map_err(internal)?
                .is_some()
            {
                return Err(rename_already_in_progress());
            }

            // 7. The target must be a live additional (not removed).
            if new.is_primary || new.removed_at.is_some() {
                return Err(new_primary_must_be_additional());
            }

            // 8. The target must anchor the cert chain (`expand_primary`).
            if new.mta_sts_cert_mode != "expand_primary" {
                return Err(new_primary_cert_mode_must_be_expand_primary());
            }

            // 9. TLS-posture monotonicity: the new primary must match or exceed
            //    the old primary's MTA-STS posture (§ Goal #3).
            if fauna_mail::tls_posture_rank(&new.mta_sts_mode)
                < fauna_mail::tls_posture_rank(&old.mta_sts_mode)
            {
                return Err(new_primary_tls_posture_weaker());
            }

            // 10. Cert SAN cap: the post-rename graph carries `2 + 2 × N` SANs
            //     (N = active local domains; the rename adds one `mail.<new>`).
            //     Refuse over Let's Encrypt's 100-SAN limit (§ cert re-issue
            //     ordering — the spec's formula + N ≤ 49 boundary).
            let n = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .len();
            let post_rename_sans = 2 + 2 * n;
            if post_rename_sans > fauna_mail::LETSENCRYPT_SAN_LIMIT {
                return Err(cert_san_limit_exceeded(post_rename_sans));
            }

            // All preconditions hold → insert at `requested`, then advance to
            // `cert_issuance` (SLICE 2) and wake the cert-lifecycle loop so it
            // widens the single managed cert with the `mail.<new>` SAN promptly
            // rather than on the next steady poll. No cert/DNS side effects fire
            // here — the loop performs the resolve-gated SAN widening idempotently
            // (crash-safe; `mail-primary-domain-rename.md` § Crash recovery), and
            // the DNS assembler advertises `mail.<new> A → mail-host` as soon as
            // the row is non-terminal so the client can publish it.
            let inserted = state
                .db
                .insert_domain_rename(&old.domain_id, &new_id, grace_days, &actor_id)
                .await
                .map_err(map_rename_err)?;
            let rename_id = inserted.rename_id;
            // Auto-advance `requested → cert_issuance`. The steady-poll cert-
            // lifecycle loop drives the identical transition for crash recovery
            // (`acme_http01.rs`), so a loop tick can win the state-conditional CAS
            // in the sub-ms gap between this call's precondition read and its
            // UPDATE — refusing with a spurious `WrongState` even though the rename
            // was created and IS correctly in `cert_issuance`. Mirror the loop's
            // tolerance (it swallows the error and re-derives next tick): on
            // refusal, re-read and treat an already-`cert_issuance` row as the
            // idempotent success it is, rather than surfacing a `409` for an op
            // that actually succeeded.
            let row = match state.db.advance_rename_to_cert_issuance(&rename_id).await {
                Ok(row) => row,
                Err(advance_err) => {
                    let reread = state
                        .db
                        .lookup_rename_by_id(&rename_id)
                        .await
                        .map_err(internal)?;
                    reconcile_concurrent_cert_issuance_advance(advance_err, reread)
                        .map_err(map_rename_err)?
                }
            };
            state.acme_retry_notify.notify_one();
            encode_reply(&StartPrimaryDomainRenameReply {
                rename: mail_domain_rename_to_row(&row),
            })
        })
    })
}

/// `fauna.bridges.get_primary_domain_rename_status` (Admin) — the in-flight
/// rename, or `None`.
fn get_primary_domain_rename_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.get_primary_domain_rename_status",
            )
            .await?;
            let _req: GetPrimaryDomainRenameStatusRequest = decode(&payload).map_err(malformed)?;
            let active = state.db.get_active_rename().await.map_err(internal)?;
            encode_reply(&GetPrimaryDomainRenameStatusReply {
                rename: active.as_ref().map(mail_domain_rename_to_row),
            })
        })
    })
}

/// `fauna.bridges.list_primary_domain_renames` (Admin) — all renames, newest
/// first, for the audit surface.
fn list_primary_domain_renames_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.list_primary_domain_renames",
            )
            .await?;
            let _req: ListPrimaryDomainRenamesRequest = decode(&payload).map_err(malformed)?;
            let rows = state.db.list_domain_renames().await.map_err(internal)?;
            encode_reply(&ListPrimaryDomainRenamesReply {
                renames: rows.iter().map(mail_domain_rename_to_row).collect(),
            })
        })
    })
}

/// `fauna.bridges.abort_primary_domain_rename` (Admin) — unwind a rename from any
/// non-terminal state (`mail-primary-domain-rename.md` § Lifecycle; § Architectural
/// rules — abort from a post-flip state is expensive but always valid). Routed by
/// state:
/// - **Pre-flip** (`requested`/`cert_issuance`/`cert_ready`) / not-found /
///   already-terminal: a pure state change via `mark_rename_aborted`. The only
///   side effect a pre-flip rename has is the widened managed-cert SAN + the
///   advertised `mail.<new>` DNS row, both of which simply stop being re-derived
///   once the row is terminal (the widened cert stays valid until its next
///   renewal narrows it back).
/// - **Post-flip** (`grace`/`ready_to_complete`): the atomic **inverse** re-flip
///   (SLICE 4) — `abort_rename_from_grace` demotes the new primary + re-promotes
///   the old in ONE transaction + marks `aborted`; then the handler swaps the
///   runtime identity projection back to the old primary (`apply_primary_identity`)
///   and pushes `config_changed`/`local_domains` so bridges re-fetch + the admin
///   client re-publishes the re-targeted DNS (the mirror of the SLICE-3 flip
///   driver's reconcile, for the old primary). Once the row is terminal, the
///   DNS `mail.<old>` keep-alive + the cert SAN keep stop, so the deployment
///   converges back on the old primary with the new domain a plain secondary.
fn abort_primary_domain_rename_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.abort_primary_domain_rename",
            )
            .await?;
            let req: AbortPrimaryDomainRenameRequest = decode(&payload).map_err(malformed)?;
            let rename_id: [u8; 16] = req
                .rename_id
                .as_ref()
                .try_into()
                .map_err(|_| malformed("rename_id must be 16 bytes"))?;
            let existing = state
                .db
                .lookup_rename_by_id(&rename_id)
                .await
                .map_err(internal)?;
            if existing
                .as_ref()
                .and_then(|r| r.parsed_state())
                .is_some_and(|s| s.is_post_flip_active())
            {
                // Atomic inverse re-flip, then reconcile the runtime identity back
                // to the old primary. The identity swap is a *sync* call
                // immediately after the awaited transaction — no await-gap where a
                // task cancel could leave the DB primary = old but the projection =
                // new (and a restart re-derives the identity from the DB regardless,
                // so the DB stays the source of truth).
                let row = state
                    .db
                    .abort_rename_from_grace(&rename_id, req.abort_reason)
                    .await
                    .map_err(map_rename_err)?;
                if let Ok(Some(old_dom)) = state
                    .db
                    .lookup_mail_domain_by_id(&row.old_primary_domain_id)
                    .await
                {
                    crate::identity_domain_core::apply_primary_identity(
                        &state,
                        &old_dom.domain_name,
                    );
                }
                notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS).await;
                return encode_reply(&AbortPrimaryDomainRenameReply {
                    rename: mail_domain_rename_to_row(&row),
                });
            }
            match state
                .db
                .mark_rename_aborted(&rename_id, req.abort_reason)
                .await
            {
                Ok(row) => encode_reply(&AbortPrimaryDomainRenameReply {
                    rename: mail_domain_rename_to_row(&row),
                }),
                Err(e) => Err(map_rename_err(e)),
            }
        })
    })
}

/// `fauna.bridges.complete_primary_domain_rename` (Admin) — finalize a rename
/// (`mail-primary-domain-rename.md` § Lifecycle; § Behavior — cert chain re-issue
/// ordering, Post-complete). Valid from `ready_to_complete` (the grace watcher
/// promoted it) without `force`; from `grace` only with `force = true` (else
/// `grace_period_not_expired`). `complete_rename` stamps the terminal `completed`
/// state; then `config_changed`/`local_domains` prompts the admin client to
/// re-fetch `list_records`, which no longer carries the grace-window `mail.<old>`
/// A row (the assembler's keep-alive stops once the rename is terminal). The cert
/// narrows to drop `mail.<old>` on its **next natural renewal** — not a forced
/// re-issue here (keeping `mail.<old>` as an extra SAN until then is harmless: the
/// old domain stays a local domain; a forced narrow would burn an ACME issuance
/// against Let's Encrypt's success-side limits for no functional gain). No
/// identity swap — the primary is already the new domain (flipped at SLICE 3).
fn complete_primary_domain_rename_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.complete_primary_domain_rename",
            )
            .await?;
            let req: CompletePrimaryDomainRenameRequest = decode(&payload).map_err(malformed)?;
            let rename_id: [u8; 16] = req
                .rename_id
                .as_ref()
                .try_into()
                .map_err(|_| malformed("rename_id must be 16 bytes"))?;
            let existing = state
                .db
                .lookup_rename_by_id(&rename_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("rename not found"))?;
            // Force-gate: a `grace` row (window not yet elapsed) needs `force=true`;
            // a `ready_to_complete` row completes without it. Other states fall
            // through to `complete_rename`, which refuses them.
            if existing.parsed_state() == Some(fauna_mail::RenameState::Grace)
                && req.force != Some(true)
            {
                return Err(grace_period_not_expired());
            }
            let row = state
                .db
                .complete_rename(&rename_id)
                .await
                .map_err(map_rename_err)?;
            // Drop the grace-window `mail.<old>` A row promptly: the assembler stops
            // emitting it now the rename is terminal; the push tells the admin client
            // to re-fetch + reconcile the zone to the post-rename steady state.
            notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS).await;
            encode_reply(&CompletePrimaryDomainRenameReply {
                rename: mail_domain_rename_to_row(&row),
            })
        })
    })
}

/// `fauna.bridges.extend_primary_domain_rename_grace` (Admin) — push `grace_ends_at`
/// out by `additional_days × 1 day` (`mail-primary-domain-rename.md` § Wire shapes).
/// Valid from `grace` / `ready_to_complete` (a `ready_to_complete` row reverts to
/// `grace`); `additional_days` range `[1, 30]` per call. No `config_changed` push —
/// nothing a bridge fetches changes (the grace-window bindings stay put); only the
/// wall-clock deadline the watcher reads moves.
fn extend_primary_domain_rename_grace_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.extend_primary_domain_rename_grace",
            )
            .await?;
            let req: ExtendPrimaryDomainRenameGraceRequest = decode(&payload).map_err(malformed)?;
            let rename_id: [u8; 16] = req
                .rename_id
                .as_ref()
                .try_into()
                .map_err(|_| malformed("rename_id must be 16 bytes"))?;
            // Bound the per-call extension to the same [1, 30] range as grace_days.
            if !(fauna_mail::GRACE_DAYS_MIN..=fauna_mail::GRACE_DAYS_MAX)
                .contains(&req.additional_days)
            {
                return Err(invalid_grace_days());
            }
            let row = state
                .db
                .extend_rename_grace(&rename_id, req.additional_days)
                .await
                .map_err(map_rename_err)?;
            encode_reply(&ExtendPrimaryDomainRenameGraceReply {
                rename: mail_domain_rename_to_row(&row),
            })
        })
    })
}

/// `fauna.bridges.set_catch_all_actor` (Admin) — designate or clear a domain's
/// per-domain catch-all actor (`mail_domains.catch_all_actor_id`;
/// `mail-aliases.md` § Kind 4, `mail-multidomain.md` § Per-domain catch-all,
/// surfaced on `admin-dns` per `admin.md` § 4). The DB already supports the
/// set/clear via the `MailDomainUpdate { catch_all_actor_id: Option<Option<_>> }`
/// tri-state; this is a dedicated setter because that tri-state is not
/// DAG-CBOR-round-trippable on the wire (so `UpdateLocalDomainConfigRequest`
/// omits it). `actor_id = Some(32 bytes)` designates; `None` clears — the outer
/// `Some` (always present) means "write this column".
fn set_catch_all_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_catch_all_actor").await?;
            let req: SetCatchAllActorRequest = decode(&payload).map_err(malformed)?;
            let catch_all: Option<[u8; 32]> = match req.actor_id.as_ref() {
                None => None,
                Some(b) => Some(
                    crate::rpc_errors::require_bytes32("actor_id", b.as_ref())
                        .map_err(malformed)?,
                ),
            };
            // Outer `Some` => write the column; inner `catch_all` => set / clear.
            let update = crate::db::mail_domains::MailDomainUpdate {
                catch_all_actor_id: Some(catch_all),
                ..Default::default()
            };
            match state
                .db
                .update_mail_domain_config(&req.domain, update)
                .await
            {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&SetCatchAllActorReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

/// `fauna.bridges.set_role_address` (Admin) — designate or clear the per-domain
/// override actor for one overridable role address (`mail_domains.role_address_overrides`;
/// `mail-multidomain.md` § Per-domain role-address routing, surfaced on `admin-dns`).
/// A dedicated setter mirroring `set_catch_all_actor`: the per-key set/clear is a
/// tri-state not DAG-CBOR-round-trippable, so it can't ride `UpdateLocalDomainConfigRequest`.
/// `actor_id = Some(32 bytes)` designates the override; `None` clears it (the role
/// falls back to the deployment admin). The nest does an atomic read-merge-write so
/// the domain's other role overrides are preserved. Only the four overridable roles
/// are settable (the wire `RoleAddressKind` enum); `tlsrpt`/`dmarc-report` always
/// route to the deployment-wide processor and have no override.
fn set_role_address_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_role_address").await?;
            let req: SetRoleAddressRequest = decode(&payload).map_err(malformed)?;
            // `Some(32 bytes)` → designate (stored as 64-char lowercase actor hex,
            // the crate-canonical actor string form); `None` → clear.
            let actor_hex: Option<String> = match req.actor_id.as_ref() {
                None => None,
                Some(b) => {
                    let bytes: [u8; 32] =
                        crate::rpc_errors::require_bytes32("actor_id", b.as_ref())
                            .map_err(malformed)?;
                    Some(hex::encode(bytes))
                }
            };
            let role_key = RoleAddressKind::as_storage_key(req.role);
            match state
                .db
                .set_role_address_override(&req.domain, role_key, actor_hex)
                .await
            {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&SetRoleAddressReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

/// `fauna.bridges.set_dkim_rotation_days` (Admin) — set or clear a domain's
/// per-domain DKIM rotation interval (`mail_domains.dkim_rotation_days`;
/// `mail-multidomain.md` § Selector — the accelerated-rotation admin lever).
/// A dedicated setter mirroring `set_catch_all_actor`/`set_role_address`: the
/// set/clear tri-state can't ride `UpdateLocalDomainConfigRequest`. The outer
/// `Some` (always present) means "write the column"; `rotation_days` set/clear is
/// the inner value. Setting a shorter interval makes a domain due sooner, which
/// the scheduled rotation-mint then acts on (`run_scheduled_dkim_rotation_mint`).
fn set_dkim_rotation_days_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.set_dkim_rotation_days").await?;
            let req: SetDkimRotationDaysRequest = decode(&payload).map_err(malformed)?;
            // Outer `Some` => write the column; inner `rotation_days` => set / clear.
            let update = crate::db::mail_domains::MailDomainUpdate {
                dkim_rotation_days: Some(req.rotation_days),
                ..Default::default()
            };
            match state
                .db
                .update_mail_domain_config(&req.domain, update)
                .await
            {
                Ok(row) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&SetDkimRotationDaysReply {
                        domain: mail_domain_to_row(&row),
                    })
                }
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

/// `fauna.bridges.force_rotate_dkim` (Admin) — emergency DKIM rotation: flip a
/// domain's **active** selector (`mail_domains.dkim_selector`) to its newest
/// selector, skipping the scheduled 24 h peer-cache wait
/// (`mail-multidomain.md` § Rotation, "Emergency rotation"). A newer selector
/// must already exist — the scheduled rotation mint's key; this only flips
/// the active pointer, and the next outbound hand-out is signed `s=<newest>`.
/// DNS publish/unpublish of the selector's TXT records is client-side
/// (`dns-management.md`), not nest's job. DKIM is an automatic concern (no
/// manual selector knob), so this is the only public surface that mutates
/// `dkim_selector`. Errors when there is no *newer* selector to rotate to.
fn force_rotate_dkim_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.force_rotate_dkim").await?;
            let req: ForceRotateDkimRequest = decode(&payload).map_err(malformed)?;

            // The active row. lookup lowercases the name; use its canonical
            // `domain_name` for the flip so casing always agrees.
            let row = state
                .db
                .lookup_active_mail_domain(&req.domain)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found(format!("no active local domain '{}'", req.domain)))?;

            // Flip to the newest selector via the shared primitive, skipping
            // the peer-cache wait (`min_age = 0`: the admin accepts the
            // stale-cache DMARC risk that defines emergency rotation). `None` =
            // the rotation mint has seated nothing newer. On a flip the nest
            // signs `s=<newest>` at the next outbound hand-out.
            match state.db.flip_to_newest_dkim_selector(&row, 0).await {
                Ok(Some(updated)) => {
                    notify_bridges_config_changed(&state, config_change_reason::LOCAL_DOMAINS)
                        .await;
                    encode_reply(&ForceRotateDkimReply {
                        domain: mail_domain_to_row(&updated),
                    })
                }
                Ok(None) => Err(no_dkim_selector_to_rotate(&row.domain_name)),
                Err(e) => Err(map_mail_domain_err(e)),
            }
        })
    })
}

/// Whether an Admin `provision_self_signed_cert` should wake the ACME
/// cert-lifecycle task to re-heal the freshly-clobbered listener cert back to a
/// trusted one.
///
/// Only a **real→self-signed transition** warrants a wake — the handler just
/// overwrote a CA-issued cert with a stopgap self-signed one, so ACME should
/// re-issue a trusted cert now. A repeat provision over a cert that was
/// **already self-signed** is a no-op clobber; re-waking each such call fires a
/// fresh *successful* ACME re-issue every time, which can approach Let's
/// Encrypt's **success-side** Duplicate-Certificate / Certificates-per-Domain
/// weekly limits. The wake gate the
/// lifecycle task applies (`next_attempt_delay`) only paces **failed**-validation
/// attempts, so it does NOT cap successful re-issuance — hence this transition
/// gate is what coalesces repeated provisions to the single in-flight self-heal.
///
/// A missing / unreadable / unparseable prior cert is **not** "already
/// self-signed" (`pem_is_self_signed` returns `false` for those, erring toward
/// "real"), so it wakes — harmless: on an ACME-on box the lifecycle task issues
/// once; on an ACME-off box the wake is a no-op.
fn provision_should_wake_acme(prior_cert_pem: Option<&[u8]>) -> bool {
    match prior_cert_pem {
        Some(pem) => !crate::acme::pem_is_self_signed(pem),
        None => true,
    }
}

// ── self-signed TLS cert provisioning (Admin) ─────────────────
//
// The sole surface for self-signed cert provisioning (the transitional HTTP
// twin `POST /api/admin/local_domains/{domain}/self_signed_cert` was retired
// once its consumers migrated — no-HTTP directive). Calls the shared
// `crate::self_signed_cert::synthesize_and_seal_self_signed_cert` — synthesize
// via rcgen → seal+fan-out a wrapped `TlsCertBlob` to every approved bridge
// with an x25519 pubkey → write the on-disk PEM. Per
// `mail-bridge-lifecycle.md` § TLS provisioning, "Admin-synthesized
// (self-signed)". TLS cert blobs are not part of `fetch_config`, but the
// handler still fires a `config_changed` push with the `"tls"` reason (the
// realized prompt-refresh-on-provision half, § TLS provisioning): the bridge
// re-runs `fetch_tls_cert_blob` on it, so the freshly-provisioned cert is
// served promptly instead of on the bridge's own 12 h TLS timer.

fn provision_self_signed_cert_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_self_signed_cert",
            )
            .await?;
            let req: ProvisionSelfSignedCertRequest = decode(&payload).map_err(malformed)?;
            if req.domain.trim().is_empty() {
                return Err(malformed("domain must not be empty"));
            }
            use crate::self_signed_cert::{
                SelfSignedCertError, synthesize_and_seal_self_signed_cert,
            };
            // Read the on-disk listener cert BEFORE the synthesize+seal
            // overwrites it, so we can distinguish a real→self-signed transition
            // (wake ACME to re-heal) from a repeat provision over an already
            // self-signed cert (skip the wake — `provision_should_wake_acme`). `None` on missing/unreadable.
            let prior_cert_pem = tokio::fs::read(state.acme_dir.join(crate::acme::CERT_FILENAME))
                .await
                .ok();
            let outcome =
                synthesize_and_seal_self_signed_cert(&state, &req.domain, req.additional_dns_sans)
                    .await
                    .map_err(|e| match e {
                        SelfSignedCertError::DomainNotActive => {
                            not_found(format!("no active local mail domain named {}", req.domain))
                        }
                        SelfSignedCertError::Synthesis(m) => internal(m),
                        SelfSignedCertError::Db(m) => internal(m),
                        SelfSignedCertError::Storage(s) => internal(s.reason),
                    })?;
            // Prompt-refresh-on-provision (`mail-bridge-lifecycle.md` § TLS
            // provisioning): the synthesize+seal above overwrote the on-disk PEM,
            // so nudge every approved bridge to re-run `fetch_tls_cert_blob` now
            // (seal-on-read hands it the freshly-synthesized cert) instead of
            // waiting out its 12 h TLS timer. On an ACME-**off** box (localhost /
            // LAN / air-gapped — the deployments that provision self-signed to
            // *keep* it) this is the only prompt; the lifecycle task isn't running,
            // so the self-signed cert stays.
            notify_bridges_config_changed(&state, config_change_reason::TLS).await;
            // On an ACME-**on** box a self-signed cert is a stopgap that
            // auto-upgrades (§ Self-healing): wake the lifecycle task so it
            // re-issues a trusted cert *now* rather than on its next steady poll
            // (it then fires another `"tls"` nudge on success) — but ONLY on a
            // genuine real→self-signed transition (`provision_should_wake_acme`).
            // The lifecycle task's wake gate paces only *failed*-validation
            // attempts, so a wake on *every* call — including a repeat provision
            // over an already-self-signed cert — would fire a fresh *successful*
            // ACME re-issue each time and can approach Let's Encrypt's success-side
            // Duplicate-Certificate weekly limit.
            // Gating on the transition coalesces repeated provisions to the single
            // in-flight self-heal. On an ACME-off box the task isn't running and
            // this is a harmless no-op. Same accelerator `restore_real_tls_cert` uses.
            if provision_should_wake_acme(prior_cert_pem.as_deref()) {
                state.acme_retry_notify.notify_one();
            }
            let to_info = |b: crate::self_signed_cert::SealedBridge| SealedBridgeInfo {
                role: b.role,
                bridge_id: b.bridge_id,
            };
            encode_reply(&ProvisionSelfSignedCertReply {
                bridges_sealed_to: outcome.bridges_sealed_to.into_iter().map(to_info).collect(),
                bridges_skipped_no_x25519: outcome
                    .bridges_skipped_no_x25519
                    .into_iter()
                    .map(to_info)
                    .collect(),
                expires_at_unix: outcome.expires_at_unix,
            })
        })
    })
}

/// `fauna.bridges.restore_real_tls_cert` (Admin) — undo a self-signed override
/// of the nest's own TLS listener. If `provision_self_signed_cert` previously
/// clobbered a real (CA-issued) cert, the real cert was preserved to a backup
/// slot (`write_acme_pem_atomic`); this restores it and the cert watcher
/// hot-reloads it. If no backup exists (e.g. the real cert was overwritten
/// before this feature shipped), the live self-signed cert is removed so the
/// ACME lifecycle re-obtains a real one on its next poll. Either way the
/// admin's "switch back to a real cert" intent is honoured without a manual
/// ACME run.
///
/// Either branch then **wakes the ACME cert-lifecycle task**
/// ([`acme_retry_notify`](crate::routes::AppState::acme_retry_notify)) so it
/// re-evaluates issuance *now* rather than after its steady poll — the "retry
/// issuance now" accelerator. The task still gates each attempt on the
/// failed-validation budget, so the wake expedites the self-heal (e.g. right
/// after port 80 opens) without ever blowing Let's Encrypt's rate limit.
fn restore_real_tls_cert_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.restore_real_tls_cert").await?;
            let _req: RestoreRealTlsCertRequest = decode(&payload).map_err(malformed)?;
            let method = state
                .storage()
                .restore_real_tls_cert()
                .await
                .map_err(|e| internal(e.reason))?;
            // Nudge the cert-lifecycle task to act immediately (within the rate
            // budget). For FromBackup it harmlessly re-confirms the restored real
            // cert; for NoBackupSelfHeals it triggers the self-heal attempt now
            // instead of on the next steady poll / backoff window.
            state.acme_retry_notify.notify_one();
            let reply = match method {
                crate::storage::RestoreTlsMethod::FromBackup => RestoreRealTlsCertReply {
                    restored_immediately: true,
                    method: "backup".to_string(),
                    message: "Restored the preserved real TLS certificate; it is serving now."
                        .to_string(),
                },
                crate::storage::RestoreTlsMethod::NoBackupSelfHeals => RestoreRealTlsCertReply {
                    restored_immediately: false,
                    method: "self_heal".to_string(),
                    message: "No preserved real certificate to restore, so nothing was changed. \
                              If ACME is enabled the nest now attempts to obtain a real \
                              certificate immediately (within Let's Encrypt's rate budget) and \
                              self-heals the self-signed one — no further action needed."
                        .to_string(),
                },
            };
            encode_reply(&reply)
        })
    })
}

/// Map a `db::mail_domains` error to an `RpcError`. Precondition
/// failures (primary-refused, past-window) map to `malformed` — the
/// codebase idiom for "request can't be honored, fix the inputs";
/// missing rows to
/// `fauna.bridges.not_found`; the rest to `internal`.
fn map_mail_domain_err(e: anyhow::Error) -> RpcError {
    let msg = format!("{e:#}");
    if msg.contains("cannot remove primary domain") {
        malformed("cannot_remove_primary_domain")
    } else if msg.contains("past the 30-day recovery window") {
        malformed("past_recovery_window")
    } else if msg.contains("no active mail_domains row")
        || msg.contains("no mail_domains row")
        || msg.contains("is not soft-deleted")
    {
        not_found(msg)
    } else {
        internal(msg)
    }
}

/// Map a `mail_domain_renames` storage error to a wire `RpcError`
/// (`mail-primary-domain-rename.md` § Wire shapes — RPC refusal codes). The
/// storage layer returns a typed [`RenameTransitionError`] for every refusal, so
/// this **downcasts** rather than string-matching the anyhow message (the
/// fragility that let the slice-2/3/4 wrong-state errors fall through to
/// a `500 internal`). A wrong-state refusal — the admin called `complete`/
/// `extend`/`abort` on a row not in an accepted state, or a concurrent
/// transition changed the state under a CAS — is a 409-class `rename_wrong_state`,
/// never a 500. Anything that is *not* a typed refusal (a "row vanished"
/// re-read, a participant-missing rollback, a rusqlite/context error) is a
/// genuine `internal`.
fn map_rename_err(e: anyhow::Error) -> RpcError {
    use crate::db::mail_domain_renames::RenameTransitionError;
    match e.downcast_ref::<RenameTransitionError>() {
        Some(RenameTransitionError::NotFound) => not_found("rename not found"),
        Some(RenameTransitionError::AlreadyInProgress) => rename_already_in_progress(),
        Some(RenameTransitionError::AlreadyTerminal) => malformed("rename_already_terminal"),
        Some(RenameTransitionError::WrongState { .. }) => rename_wrong_state(),
        None => internal(format!("{e:#}")),
    }
}

/// Reconcile a raced `requested → cert_issuance` auto-advance for the
/// `start_primary_domain_rename` handler.
///
/// `start` inserts a `requested` rename row and immediately auto-advances it to
/// `cert_issuance`. The steady-poll cert-lifecycle loop drives that *identical*
/// transition for crash recovery (`acme_http01.rs`;
/// `mail-primary-domain-rename.md` § Crash recovery). If a loop tick wins the
/// advance in the sub-millisecond gap between the handler's precondition read and
/// its state-conditional (CAS) UPDATE, `advance_rename_to_cert_issuance`'s CAS
/// matches 0 rows and refuses with a [`RenameTransitionError::WrongState`] — yet
/// the rename **was** created and **is** correctly in `cert_issuance`. Surfacing
/// that as a `409 rename_wrong_state` for an operation that actually succeeded is a
/// spurious refusal (the admin's retry then returns `rename_already_in_progress`).
///
/// Given the advance error and a fresh re-read of the row, treat the refusal as
/// the idempotent success it is **iff** the row is now exactly `cert_issuance` (a
/// coincident loop tick won the same advance). Any other observed state — or a
/// vanished row — propagates the original error unchanged, so a genuinely
/// divergent race (e.g. a concurrent abort of the just-inserted row) is never
/// masked. This gives one-shot `start` the same tolerance the loop already has:
/// the loop swallows the error and re-derives next tick; `start` re-reads.
fn reconcile_concurrent_cert_issuance_advance(
    advance_err: anyhow::Error,
    reread: Option<crate::db::mail_domain_renames::MailDomainRename>,
) -> anyhow::Result<crate::db::mail_domain_renames::MailDomainRename> {
    match reread {
        Some(row) if row.parsed_state() == Some(fauna_mail::RenameState::CertIssuance) => Ok(row),
        _ => Err(advance_err),
    }
}

// ── config_changed hot-reload push ───────────────────────────
//
// Subscribe-and-hot-reload (`mail-bridge-lifecycle.md` § Running /
// § Architectural rules): when an admin mutates a knob the bridge reads via
// `fauna.bridges.fetch_config`, nest fans a `fauna.bridges.config_changed`
// push to every approved bridge so the running bridge re-fetches + re-applies
// in-process — no SIGHUP, no restart. Without it a change is only picked up on
// the bridge's next reconnect / cold boot.

/// Fan a `fauna.bridges.config_changed` push to every approved bridge.
///
/// `reason` is a `fauna_protocol::bridge_routing::config_change_reason::*`
/// token (advisory; the bridge re-fetches the full `fetch_config` regardless).
/// Best-effort by design: a bridge with no live WS connection is a silent
/// `notify_push` no-op (it re-reads config on its next reconnect), and a DB
/// error listing bridges is logged rather than failing the admin mutation —
/// which has already committed, and the bridge converges on its next
/// `fetch_config` either way. Call it *after* the mutating write succeeds.
pub(crate) async fn notify_bridges_config_changed(state: &Arc<AppState>, reason: &str) {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                reason,
                error = %format!("{e:#}"),
                "config_changed: failed to list approved bridges; skipping push (bridges converge on next fetch_config)"
            );
            return;
        }
    };
    for bridge in bridges {
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeConfigChanged(
                fauna_protocol::bridge_routing::BridgeConfigChangedPush {
                    reason: reason.to_string(),
                },
            ),
        );
    }
}

/// One scheduled DKIM-rotation pass (`mail-multidomain.md` § Rotation,
/// "Scheduled rotation"). For every domain whose active selector is **due**
/// (`dkim_rotation_due_domains`), auto-flip to its newest-provisioned selector
/// **iff** that selector has aged past the peer-cache warmup window
/// (`DKIM_ROTATION_CACHE_WARMUP_MS`) — the same flip primitive
/// `force_rotate_dkim` uses, here gated on the 24 h wait instead of skipping it.
/// A due domain with no newer selector provisioned yet is logged but **not**
/// flipped (the rotation waits for `run_scheduled_dkim_rotation_mint` to mint
/// the new key and for its record to be published). A flip pushes `config_changed`
/// once so the MTA rebuilds its per-domain DKIM set live. Called from the nest's
/// background-expiry tick; exposed as the lib entry point so `main.rs` needn't
/// reach the internal push helper. Best-effort: errors are logged, never fatal
/// (the next tick retries; the flip is idempotent).
pub async fn run_scheduled_dkim_autoflip(state: &Arc<AppState>) {
    let due = match state.db.dkim_rotation_due_domains().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("DKIM rotation due-check error: {e}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    tracing::info!(
        "{} mail domain(s) due for DKIM rotation: {:?}",
        due.len(),
        due.iter()
            .map(|d| d.domain_name.as_str())
            .collect::<Vec<_>>()
    );
    let mut flipped: Vec<String> = Vec::new();
    for row in &due {
        match state
            .db
            .flip_to_newest_dkim_selector(
                row,
                crate::db::mail_domains::DKIM_ROTATION_CACHE_WARMUP_MS,
            )
            .await
        {
            Ok(Some(updated)) => flipped.push(updated.domain_name),
            Ok(None) => {} // no newer key provisioned yet, or not aged past the window
            Err(e) => tracing::warn!(domain = %row.domain_name, "DKIM auto-flip error: {e}"),
        }
    }
    if !flipped.is_empty() {
        tracing::info!(
            "auto-flipped DKIM selector for {} due domain(s) past the cache window: {flipped:?}",
            flipped.len()
        );
        notify_bridges_config_changed(state, config_change_reason::LOCAL_DOMAINS).await;
    }
}

/// One scheduled DKIM rotation-**mint** pass (`mail-multidomain.md` § Rotation,
/// "Scheduled rotation"). The producer half that feeds `run_scheduled_dkim_
/// autoflip`: for every **due** domain (`dkim_rotation_due_domains`) that has no
/// rotation already in flight, mint a fresh Ed25519 DKIM key under a `<YYYYMM>`
/// selector, so the auto-flip then activates it after the 24 h peer-cache
/// warmup.
///
/// A deliberate mint, never a read: the cache-warmup model needs the new
/// selector's TXT published *before* it becomes active, so the key is minted
/// while the **old** selector is still signing.
///
/// Every mint is **nest-held** (`mail-bridge-lifecycle.md` § DKIM provisioning
/// (automatic) → *Custody moves to the nest*): the key is sealed under the
/// nest's own key-encryption key and no approved MTA is needed first.
///
/// Mints exactly once per rotation: the gate is "newest selector == the active
/// selector" (no newer selector pending a flip), so re-running across the 24 h
/// warmup window — even across a month boundary, where the `<YYYYMM>` name
/// would otherwise advance — is a no-op until the auto-flip lands and the next
/// window opens. DNS publish of the new selector's TXT stays client-side
/// (`admin-dns` renders `list_dkim_selectors`). Best-effort: errors are logged
/// and never abort the pass; the next tick retries.
pub async fn run_scheduled_dkim_rotation_mint(state: &Arc<AppState>) {
    let due = match state.db.dkim_rotation_due_domains().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("DKIM rotation-mint due-check error: {e}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }

    let selector = crate::db::mail_domains::dkim_rotation_selector(crate::db::now_epoch_millis());
    let mut minted: Vec<String> = Vec::new();
    for row in &due {
        let active = row
            .dkim_selector
            .as_deref()
            .unwrap_or(DEFAULT_DKIM_SELECTOR);
        // Idempotency gate: only mint when no newer selector is already pending a
        // flip (newest == active). `list_dkim_selectors` orders by `created_at`
        // ascending → newest is last, matching `flip_to_newest_dkim_selector`'s
        // notion of "newest". A domain with no key at all is the boot step's.
        let provisioned = match state.db.list_dkim_selectors(Some(&row.domain_name)).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(domain = %row.domain_name, "DKIM rotation-mint list error: {e}");
                continue;
            }
        };
        let Some(newest) = provisioned.last() else {
            continue;
        };
        if newest.selector != active || newest.selector == selector {
            continue; // a rotation is already in flight (or already this selector)
        }

        match state
            .db
            .mint_mail_dkim_key(&row.domain_name, &selector)
            .await
        {
            Ok(true) => minted.push(row.domain_name.clone()),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(domain = %row.domain_name, "DKIM rotation-mint error: {e:#}")
            }
        }
    }
    if !minted.is_empty() {
        tracing::info!(
            "minted DKIM rotation selector {selector} for {} due domain(s): {minted:?}; \
             the auto-flip activates it after the cache-warmup window",
            minted.len()
        );
    }
}

/// Nudge the **MTA-role** bridge to drain its outbound queue promptly via
/// a `fauna.bridges.outbound_ready` push, instead of waiting for its next
/// `fauna.bridges.fetch_outbound_due` poll. The outbound twin of the
/// inbound `crate::segments::notify_mail_received` arrival push.
///
/// Called from the interactive `fauna.email.send` path after it enqueues a
/// remote-recipient row — the one outbound-enqueue site whose ≤30 s poll
/// latency is user-observable (a user watching their sent mail relay). The
/// many *background* enqueue paths (bounces/DSNs, forwards, TLSRPT reports,
/// security mail) deliberately do **not** call this: nobody watches their
/// drain latency, and the `fetch_outbound_due` poll is the universal
/// correctness backstop for *every* enqueue path. A future track that wants
/// one of those to drain promptly can call this helper there too.
///
/// Filtered to `BridgeRole::Mta` (unlike `notify_bridges_config_changed`,
/// which fans to every approved bridge) because only the MTA runs the
/// outbound worker — the MDA has no `Trigger()`. Best-effort: a
/// disconnected MTA's emit is a no-op (`notify_push` drops it); it drains
/// on its next poll.
///
/// Returns the number of approved MTA-role bridges the push was addressed to.
/// Production ignores it (the nudge is advisory either way); the `test-hooks`
/// poke endpoint reports it so a test can tell "nudged nobody, no MTA is
/// enrolled" from "nudged the MTA" instead of silently waiting out the poll.
pub(crate) async fn notify_bridges_outbound_ready(state: &Arc<AppState>) -> usize {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "outbound_ready: failed to list approved bridges; skipping push (MTA drains on next fetch_outbound_due poll)"
            );
            return 0;
        }
    };
    let mut nudged = 0usize;
    for bridge in bridges {
        if bridge.role != crate::db::bridge_service_users::BridgeRole::Mta {
            continue;
        }
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeOutboundReady(
                fauna_protocol::bridge_routing::BridgeOutboundReadyPush::default(),
            ),
        );
        nudged += 1;
    }
    nudged
}

/// Nudge the **MDA-role / content-processor** bridge to run its re-score drain
/// worker promptly via a `fauna.bridges.rescore_ready` push, instead of waiting
/// for its next startup / `config_changed` / 12 h backstop trigger. The re-score
/// twin of the inbound `crate::segments::notify_mail_received` arrival push and
/// the outbound `notify_bridges_outbound_ready` nudge.
///
/// Called from the inbound-mail ingest core after a genuinely-new delivery has
/// seeded a per-user re-score obligation (`seed_new_mail_labeler_obligations`
/// returned > 0). Filtered to the two grant-holding roles that run the drain
/// (`BridgeRole::{Mda, ContentProcessor}`); the MTA has no drain. Best-effort:
/// a disconnected holder's emit is a no-op (`notify_push` drops it); it drains
/// on its next trigger.
pub(crate) async fn notify_bridges_rescore_ready(state: &Arc<AppState>) {
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "bridge_rpc",
                error = %format!("{e:#}"),
                "rescore_ready: failed to list approved bridges; skipping push (drain fires on its next config_changed / 12h backstop)"
            );
            return;
        }
    };
    for bridge in bridges {
        match bridge.role {
            crate::db::bridge_service_users::BridgeRole::Mda
            | crate::db::bridge_service_users::BridgeRole::ContentProcessor => {}
            _ => continue,
        }
        state.ws.notify_push(
            &bridge.ed25519_pubkey,
            fauna_protocol::PushEvent::BridgeRescoreReady(
                fauna_protocol::bridge_routing::BridgeRescoreReadyPush::default(),
            ),
        );
    }
}

/// Poke the **MDA-role / content-processor** holder to run the spam-baseline
/// publish drain for pending run `run_id` via a
/// `fauna.bridges.spam_baseline_publish` push (`mail-spam.md` § Encrypted-mode
/// interaction, ratified 2026-07-13 — the third holder-pull drain instance,
/// beside [`notify_bridges_rescore_ready`]). The holder answers by pulling
/// `fauna.capabilities.spam_baseline_worklist` for this run and submitting its
/// merged half via `fauna.capabilities.submit_spam_baseline`.
///
/// Called from `publish_spam_baseline_handler` after it registered the pending
/// run, with the run's bound `holder` — the box's single aggregation holder, as
/// resolved by `bridge_imap_handlers::resolve_content_processor_holder`. The
/// poke goes to **that holder alone**: it is the identity the contributor copies
/// are sealed to, so it is the only one whose worklist can be non-empty
/// (`mail-spam.md` § Encrypted-mode interaction — aggregation runs at *the*
/// granted holder, singular).
///
/// Do **not** widen this back to a role filter. `BridgeRole::{Mda,
/// ContentProcessor}` both pass the drain's coarse method gate, and a standard
/// box always runs an MDA — but no copy or grant is ever directed to it, so its
/// drain answers with an empty half having merged nothing, and the publish's
/// first-submit-wins oneshot would hand the run to it near-deterministically
/// while the real holder is still unsealing. A disconnected holder's emit is a
/// no-op (`notify_push` drops it) — the publish handler's bounded await then
/// elapses and the publish proceeds from the nest-readable half with the
/// unmerged count reported honestly.
pub(crate) async fn notify_bridges_spam_baseline_publish(
    state: &Arc<AppState>,
    run_id: &[u8],
    holder: &[u8; 32],
) {
    state.ws.notify_push(
        holder,
        fauna_protocol::PushEvent::BridgeSpamBaselinePublish(
            fauna_protocol::bridge_routing::BridgeSpamBaselinePublishPush {
                run_id: serde_bytes::ByteBuf::from(run_id.to_vec()),
                extra: Default::default(),
            },
        ),
    );
}

// ── fetch_recipient_mls_pubkey ────────────────────────────────

fn fetch_recipient_mls_pubkey_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.fetch_recipient_mls_pubkey",
            )
            .await?;
            let req: FetchRecipientMlsPubkeyRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            recipient_mls_pubkey_reply(&state, &target, req.mail_new_ingest).await
        })
    })
}

/// `fetch_recipient_mls_pubkey` for a conversation-bridge **principal**
/// (`apps/bridges.md` § Bridge-kind catalogue → Phase G): the key each
/// deposit is sealed to, for the principal's own account only — never another
/// actor's, and never the mail seam's epoch key.
pub(crate) fn principal_fetch_recipient_mls_pubkey_handler()
-> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: FetchRecipientMlsPubkeyRequest = decode(&payload).map_err(malformed)?;
            if req.actor_id.as_slice() != caller.account {
                return Err(crate::rpc_errors::permission_denied_ns(
                    "bridges",
                    "a principal reads its own account's recipient key only",
                ));
            }
            recipient_mls_pubkey_reply(&state, &caller.account, false).await
        })
    })
}

/// The recipient-key reply for `target`, shared by both doors.
async fn recipient_mls_pubkey_reply(
    state: &crate::routes::AppState,
    target: &[u8; 32],
    mail_new_ingest: bool,
) -> Result<bytes::Bytes, fauna_protocol::RpcError> {
    // Phase-3 D2: this reply feeds every Go-bridge seal site, but NOT
    // every caller is mail-new-ingest — the same RPC also serves the
    // MDA session's cached pubkey for CalDAV/CardDAV PUT, several
    // collection-metadata seals, and the IMAP spam-model re-seal,
    // none of which have an epoch opener. So the content-sealing-epochs mail seam
    // (design § 3/§ 6) applies ONLY when the caller self-identifies
    // as the genuine per-delivery mail resolution
    // (`req.mail_new_ingest` — additive, defaults false); every
    // other caller always gets the standing key, even with the
    // write gate forced on. `mlkem_ek` is the post-quantum sibling
    // (S3c), on file for every recipient the provision door wrote.
    let seal_key = state
        .db
        .get_recipient_mail_seal_key(target, mail_new_ingest && state.epoch_sealing_enabled())
        .await
        .map_err(internal)?;
    let (key, succession_pending) = match seal_key {
        None => {
            // No key on file. Distinguish "this actor is a
            // succession's successor and will provision one within
            // one sign-in" from "never onboarded" — the caller
            // (the MTA) tempfails the former and permanently
            // rejects the latter (`smtp-server.md` § Error /
            // tempfail strategy).
            let succession_pending = state
                .db
                .is_succession_new_actor(target)
                .await
                .map_err(internal)?;
            (None, succession_pending)
        }
        Some(k) => (
            Some(RecipientSealKeyHalves {
                mls_pubkey: ByteBuf::from(k.mls_pubkey.to_vec()),
                mlkem_ek: ByteBuf::from(k.mlkem_ek),
                extra: Default::default(),
            }),
            false,
        ),
    };
    encode_reply(&FetchRecipientMlsPubkeyReply {
        key,
        succession_pending,
    })
}

/// Byte length of an ML-KEM-768 encapsulation key (= the standard's fixed
/// `ek` size, mirrored by `fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`). Hard-coded
/// here so the nest's provision-time length guard does not pull the heavy
/// `libcrux-ml-kem` dependency into `fauna-nest` for a single constant; the
/// value is fixed by FIPS 203 and cannot drift.
const ML_KEM_768_ENCAPS_KEY_LEN: usize = 1184;

// ── provision_recipient_mls_pubkey ────────────────────────────
//
// Writer for `actor_mls_pubkeys` — the production counterpart to
// `fetch_recipient_mls_pubkey` (which the MTA reads on every inbound
// DATA to seal `encrypted_body` to the recipient). The recipient
// pubkey is the user's MSEK-derived standing HPKE key
// (`docs/goal/architecture/key-material-hierarchy.md` § Path
// B-sibling-2); the user self-registers their OWN pubkey at enable-mail
// (`mail-credentials.md` § Partial-state-during-minting step 2). A
// `User` caller may register only `target == caller`; an `Admin` may
// register any actor's (bridge-perimeter / migration). Allowlist gate
// in `bridge_method_allowlist.rs`.

fn provision_recipient_mls_pubkey_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(
                &state,
                &actor_id,
                "fauna.bridges.provision_recipient_mls_pubkey",
            )
            .await?;
            let req: ProvisionRecipientMlsPubkeyRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            // A user may register only their own recipient pubkey;
            // admin (and bridge classes, were they ever permitted) may
            // target any actor.
            if class == CallerClass::User && target != actor_id {
                return Err(permission_denied(
                    "a user may only register their own recipient mls pubkey",
                ));
            }
            let pubkey: [u8; 32] =
                crate::rpc_errors::require_bytes32("mls_pubkey", req.mls_pubkey.as_slice())
                    .map_err(malformed)?;
            // Post-quantum sibling (S3c): the ML-KEM-768 encapsulation key the
            // client publishes alongside the X25519 pubkey. Length-checked here
            // (a structural guard, mirroring the 32-byte `mls_pubkey` check) so
            // a malformed ek is rejected at provision time rather than failing
            // later when the MTA assembles the X-Wing key. Both halves land in
            // one write, so no recipient row rests without its ek.
            if req.mlkem_ek.len() != ML_KEM_768_ENCAPS_KEY_LEN {
                return Err(malformed("mlkem_ek must be 1184 bytes (ML-KEM-768)"));
            }
            state
                .db
                .put_actor_recipient_seal_key(&target, &pubkey, &req.mlkem_ek)
                .await
                .map_err(internal)?;
            // Content-sealing-epoch schedule (design 2026-07-18 § 3): store
            // the published horizon of per-epoch PUBLIC seal keys. Additive;
            // absent leaves any prior schedule untouched. Length-checked like the
            // standing halves; count-capped so one request can't grow the
            // table unboundedly (the honest horizon is 26 epochs — twice
            // that plus slack is generous for any legitimate publisher).
            if let Some(epoch_keys) = req.epoch_keys.as_ref() {
                const MAX_EPOCH_KEYS_PER_PROVISION: usize = 64;
                // Sanity window for the untrusted `epoch` index: an honest publisher writes
                // `[e_now, e_now + MAIL_EPOCH_PUBLISH_HORIZON]`; twice the
                // horizon each side tolerates clock skew and a stale-but-
                // honest republish while rejecting the adversarial
                // distinct-epoch table-growth write. Provision-path only —
                // ingest never touches this bound (never-bounce).
                const EPOCH_SANITY_SLACK: u64 =
                    2 * fauna_mls::wrapped_blob::MAIL_EPOCH_PUBLISH_HORIZON;
                if epoch_keys.len() > MAX_EPOCH_KEYS_PER_PROVISION {
                    return Err(malformed("epoch_keys: at most 64 epochs per provision"));
                }
                let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(
                    crate::db::now_epoch_secs() as u64,
                );
                let mut rows = Vec::with_capacity(epoch_keys.len());
                for k in epoch_keys {
                    if k.epoch < e_now.saturating_sub(EPOCH_SANITY_SLACK)
                        || k.epoch > e_now + EPOCH_SANITY_SLACK
                    {
                        return Err(malformed(format!(
                            "epoch_keys[].epoch {} outside the plausible sealing window \
                             [{}, {}]",
                            k.epoch,
                            e_now.saturating_sub(EPOCH_SANITY_SLACK),
                            e_now + EPOCH_SANITY_SLACK,
                        )));
                    }
                    let pk: [u8; 32] = crate::rpc_errors::require_bytes32(
                        "epoch_keys[].mls_pubkey",
                        k.mls_pubkey.as_slice(),
                    )
                    .map_err(malformed)?;
                    if k.mlkem_ek.len() != ML_KEM_768_ENCAPS_KEY_LEN {
                        return Err(malformed(
                            "epoch_keys[].mlkem_ek must be 1184 bytes (ML-KEM-768)",
                        ));
                    }
                    rows.push((k.epoch, pk, k.mlkem_ek.to_vec()));
                }
                state
                    .db
                    .put_actor_epoch_seal_keys(&target, &rows)
                    .await
                    .map_err(internal)?;
            }
            // The user's registration handle IS their email address: enabling
            // mail creates the canonical `<handle-localpart>@<domain>` exact
            // alias so inbound to the handle routes to this actor with zero
            // alias management (`mail-aliases.md` § Kind 1 — Exact). Idempotent;
            // a no-op when the actor has no handle or no mail domain exists yet;
            // never clobbers a localpart another actor already owns.
            ensure_canonical_handle_alias(&state, &target).await?;
            encode_reply(&ProvisionReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Create the canonical exact alias `<handle-localpart>@<domain>` for `target`
/// so the user's registration handle is their routable email address
/// (`mail-aliases.md` § Kind 1 — Exact: "the canonical address each user gets
/// at signup"). Called when a user enables mail (provisions their recipient
/// MLS pubkey) — the mail domain must exist by then, so enable-time is the
/// realized timing for the spec's "at signup" intent on a fresh nest.
///
/// Idempotent and defensive:
/// - no-op if the actor has no handle, or no mail domain is registered yet;
/// - the address domain is the handle's own domain when the nest serves mail
///   for it, else the primary mail domain (the canonical `<our-domain>`);
/// - never clobbers a `(local_domain, pattern)` another actor already owns
///   (the uniqueness invariant in `mail-aliases.md` § Kind 1).
pub(crate) async fn ensure_canonical_handle_alias(
    state: &AppState,
    target: &[u8; 32],
) -> Result<(), RpcError> {
    let Some((domain, localpart)) = canonical_address_for_actor(state, target).await? else {
        return Ok(());
    };
    // Don't steal a localpart another actor already owns (uniqueness invariant).
    match state
        .db
        .lookup_exact_alias(&domain, &localpart)
        .await
        .map_err(internal)?
    {
        Some(existing) if existing == *target => {} // already ours — idempotent
        Some(_) => {}                               // owned by another actor — leave it
        None => match state
            .db
            .put_exact_alias(&domain, &localpart, "exact", target)
            .await
        {
            Ok(()) => {}
            // A forwarder or list holds the key — leave it too: an exact row
            // there would shadow it (exact resolves first).
            Err(crate::db::mail_aliases::AliasWriteError::KeyHeldByOtherKind) => {}
            Err(e) => return Err(map_alias_write_err_rpc(e)),
        },
    }
    Ok(())
}

/// The actor's canonical email address `(local_domain, localpart)` — the
/// `<handle-localpart>@<domain>` that mail-enable writes as the canonical exact
/// alias. `None` when the actor has no handle or no servable mail domain exists.
///
/// **Single source of truth** shared by the canonical *writer*
/// (`ensure_canonical_handle_alias`) and the canonical-protection *guard*
/// (`is_canonical_alias`) so the two can never disagree on which row is
/// canonical (a drift bug would re-open the disable-trap on the wrong row).
/// Both returned strings are lower-cased to match how `put_exact_alias` /
/// `create_account_alias` store them.
pub(crate) async fn canonical_address_for_actor(
    state: &AppState,
    target: &[u8; 32],
) -> Result<Option<(String, String)>, RpcError> {
    let Some(handle) = state.db.get_handle(target).await.map_err(internal)? else {
        return Ok(None);
    };
    let handle = handle.trim();
    let (localpart, handle_domain) = match handle.split_once('@') {
        Some((lp, dom)) => (lp.trim(), Some(dom.trim().to_ascii_lowercase())),
        None => (handle, None),
    };
    if localpart.is_empty() {
        return Ok(None);
    }
    // Address domain: the handle's own domain when the nest serves mail for it,
    // else the primary mail domain. No mail domain at all → nothing routable.
    let domain = match handle_domain {
        Some(d)
            if state
                .db
                .lookup_active_mail_domain(&d)
                .await
                .map_err(internal)?
                .is_some() =>
        {
            d
        }
        _ => match state
            .db
            .lookup_primary_mail_domain()
            .await
            .map_err(internal)?
        {
            Some(p) => p.domain_name,
            None => return Ok(None),
        },
    };
    Ok(Some((domain, localpart.to_ascii_lowercase())))
}

/// Whether `alias_id` (owned by `actor_id`) is the actor's canonical exact
/// alias — its primary `<handle>@<domain>` address. Such a row must never be
/// disabled or deleted (`canonical_alias_protected`). Returns `Ok(false)` if the
/// alias isn't found/owned (the caller's own not-found path then handles it).
async fn is_canonical_alias(
    state: &AppState,
    actor_id: &[u8; 32],
    alias_id: &[u8; 16],
) -> Result<bool, RpcError> {
    let Some((domain, localpart)) = canonical_address_for_actor(state, actor_id).await? else {
        return Ok(false);
    };
    let aliases = state
        .db
        .list_aliases_for_actor(actor_id)
        .await
        .map_err(internal)?;
    Ok(aliases.iter().any(|r| {
        &r.alias_id == alias_id
            && r.kind == fauna_mail::aliases::ALIAS_KIND_EXACT
            && r.local_domain.eq_ignore_ascii_case(&domain)
            && r.pattern.eq_ignore_ascii_case(&localpart)
    }))
}

// ── fetch_recipient_index_key ─────────────────────────────────

fn fetch_recipient_index_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_recipient_index_key").await?;
            let req: FetchRecipientIndexKeyRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            let pubkey = state
                .db
                .get_actor_index_pubkey(&target)
                .await
                .map_err(internal)?
                .map(|pk| ByteBuf::from(pk.to_vec()));
            encode_reply(&FetchRecipientIndexKeyReply { pubkey })
        })
    })
}

// ── fetch_config ──────────────────────────────────────────────

/// The static-baseline reply. Catalog defaults live on the wire types'
/// `Default` impls in `libs/fauna-protocol/src/bridge_routing.rs` (sourced
/// from `docs/goal/behavior/mail-policy-config.md` § Inbound hardening,
/// § Outbound delivery, § IMAP server policy). Operator-tunable knobs
/// land in a follow-up that adds a `[bridges.policy]` config section;
/// until then nest is the source of truth and bridges may cache the reply.
///
/// `mail_enabled` (Phase E; Stage-5 default-off landed) is the persisted
/// `mail_enabled` DB toggle in its effective form
/// (`CacheDb::effective_mail_enabled`): set by the admin via
/// `fauna.bridges.set_mail_enabled`, and **off** when never set (a
/// freshly-claimed nest — `mail-policy-config.md` § Default-off on first
/// claim). There is no unset⇒on "approved ⇒ enabled" fallback. See
/// `bridge_routing.rs::FetchConfigReply::mail_enabled` for the field's
/// semantics.
fn default_config_reply() -> FetchConfigReply {
    FetchConfigReply::default()
}

/// Default DKIM selector *label* applied when a `mail_domains` row's
/// `dkim_selector` is NULL. The DNS record lives at
/// `<selector>._domainkey.<domain>` and the DKIM-Signature `s=` tag
/// carries this label; "default" is the conventional first selector
/// (matches `docs/goal/behavior/mail-multidomain.md` § Selector and the
/// blob the admin-uploaded path provisions under for a fresh domain).
const DEFAULT_DKIM_SELECTOR: &str = crate::mail_dkim_key::DEFAULT_SELECTOR;

/// Overlay each `Some(field)` of a `<X>PolicyOverrides` onto the matching
/// field of a `FetchConfigReply` sub-struct, leaving `None` fields at the
/// catalog default. The override-struct field names match the sub-struct
/// field names by construction (A3 Bucket B), so one macro covers all five
/// — and a typo'd or forgotten field is a compile error, not a silent
/// half-overlay. Each `$src.$field` is a partial move (read once).
macro_rules! overlay_policy {
    ($src:expr, $dst:expr, $($field:ident),+ $(,)?) => {
        $( if let Some(v) = $src.$field { $dst.$field = v; } )+
    };
}

/// Assemble the **overlaid effective** `FetchConfigReply` — catalog defaults
/// with every `put_<substruct>_policy` admin override applied, plus the derived
/// `mail_domains` projection and the `mail_enabled` / `caldav_enabled` toggles.
/// Shared by `fetch_config_handler` (the bridge read, `BridgeMta | BridgeMda`)
/// and `get_mail_config_handler` (the admin read, `Admin`) so both see byte-for
/// -byte the same effective config — the admin form edits exactly what the
/// bridge will apply. The two callers differ only in caller-class gating and
/// (for the bridge) the forward-compat `scope` validation.
async fn assemble_fetch_config_reply(
    state: &Arc<crate::routes::AppState>,
) -> Result<FetchConfigReply, RpcError> {
    // Project `mail.local_domains` + `primary_domain` from
    // the `mail_domains` table per
    // docs/goal/behavior/mail-multidomain.md § Architectural
    // rules → "The `local_domains` list is a derived projection".
    let active_domains = state
        .db
        .list_active_mail_domains()
        .await
        .map_err(internal)?;
    let local_domains: Vec<String> = active_domains
        .iter()
        .map(|d| d.domain_name.clone())
        .collect();
    let primary_row = active_domains.iter().find(|d| d.is_primary);
    let primary_domain = primary_row
        .map(|d| d.domain_name.clone())
        .unwrap_or_default();
    // Per-domain DKIM selectors — one entry per active domain (the `s=`
    // tag / `<selector>._domainkey.<domain>` DNS label), defaulting to
    // DEFAULT_DKIM_SELECTOR when the row's dkim_selector is NULL (a
    // freshly-claimed domain). The MTA bridge builds one signing registry
    // per entry and routes signing by the From: header domain (RFC 6376
    // §3.6, docs/goal/behavior/mail-multidomain.md § Signing-key selection).
    // An empty list (no active domains) keeps the bridge's DKIM degraded
    // (unsigned DATA). Ordered like `local_domains` (primary first, then
    // added_at ASC — list_active_mail_domains' ordering).
    let dkim_selectors = active_domains
        .iter()
        .map(|d| DomainDkimSelector {
            domain: d.domain_name.clone(),
            selector: d
                .dkim_selector
                .clone()
                .unwrap_or_else(|| DEFAULT_DKIM_SELECTOR.to_string()),
            // Forward-compat catch-all (transport.md rule 4); nest-minted.
            extra: Default::default(),
        })
        .collect();

    let mut reply = FetchConfigReply {
        local_domains,
        primary_domain,
        dkim_selectors,
        ..default_config_reply()
    };

    // Phase E + the Stage-5 default-off flip (landed): the deployment-wide
    // mail-enable toggle, effective form — an unset toggle (freshly-claimed
    // nest) reads OFF (`mail-policy-config.md` § Default-off on first claim).
    // The admin sets it via `fauna.bridges.set_mail_enabled`, normally fired
    // by the launched client's claim-time § 3b glue on a real-domain public
    // claim. `CacheDb::effective_mail_enabled` is the default's single owner.
    reply.mail_enabled = state.db.effective_mail_enabled().await.map_err(internal)?;

    // CalDAV gates independently of email (it needs only the HTTPS
    // surface, not the full MX stack). When the admin has never set the
    // CalDAV toggle, fall back to `mail_enabled` ("enabling email also
    // enables CalDAV" out of the box). The MDA binds its CalDAV
    // listener iff this is true, independently of the IMAP listeners.
    // Per `caldav-server.md` § Independent enablement.
    reply.caldav_enabled = state
        .db
        .get_caldav_enabled()
        .await
        .map_err(internal)?
        .unwrap_or(reply.mail_enabled);

    // CardDAV gates independently too (contacts twin of `caldav_enabled`). It
    // needs only the HTTPS surface and rides the SAME DAV listener as CalDAV
    // (no separate port). When the admin has never set the CardDAV toggle, fall
    // back to `mail_enabled` so a fresh real-domain deployment gets a contacts
    // surface out of the box. The MDA registers its `/carddav` path handler iff
    // this is true (CardDAV server design; tracked internally).
    reply.carddav_enabled = state
        .db
        .get_carddav_enabled()
        .await
        .map_err(internal)?
        .unwrap_or(reply.mail_enabled);

    // WebDAV gates independently too (files twin of `carddav_enabled`). It needs
    // only the HTTPS surface and rides the SAME DAV listener as CalDAV/CardDAV
    // (no separate port). When the admin has never set the WebDAV toggle, fall
    // back to `mail_enabled` so a fresh real-domain deployment gets a files
    // surface out of the box (harmless-on — nothing is served until a set is
    // individually flagged `folders.webdav_enabled`). The MDA registers its
    // `/webdav` path handler iff this is true. Per
    // `docs/goal/behavior/webdav-server.md` § Independent enablement.
    reply.webdav_enabled = state
        .db
        .get_webdav_enabled()
        .await
        .map_err(internal)?
        .unwrap_or(reply.mail_enabled);

    // The admin-set CalDAV listener port (`fauna.bridges.set_caldav_port`),
    // falling back to the hard-coded `DEFAULT_CALDAV_PORT` (8443) when the admin
    // has never set it. The MDA binds this on a bare-IP / desktop / domainless
    // box that serves CalDAV directly (no SNI router); a router-fronted domain
    // box's loopback-IPC hatch wins over it. Per `caldav-server.md`
    // § Network exposure.
    reply.caldav_port = state
        .db
        .get_caldav_port()
        .await
        .map_err(internal)?
        .unwrap_or(fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT);

    // A3 Bucket B — overlay the per-sub-struct admin policy
    // overrides onto the catalog defaults before encoding. Each
    // `Some(...)` field replaces; each `None` (and a row that
    // doesn't exist yet) keeps the catalog default. The bridge
    // already reads all these fields; this is purely the
    // write-path projection (`put_<substruct>_policy` →
    // `mail_<substruct>_policy` → here). Subscribe-and-hot-reload
    // (`config_changed` push) is still deferred; the bridge picks
    // a change up on its next `fetch_config`. Spec:
    // `docs/goal/behavior/mail-policy-config.md` § Policy catalog.
    let spam = state.db.get_spam_policy().await.map_err(internal)?;
    overlay_policy!(
        spam,
        reply.spam,
        max_score_before_spam_folder,
        max_score_before_reject,
        dnsbl_servers,
        reject_no_rdns,
        greylist_enabled,
        greylist_delay_secs,
        max_conn_per_min,
        fcrdns_mode,
        helo_identity_required,
        reject_fcrdns_fail,
        max_message_bytes,
        bayesian_weight_milli,
        bayesian_min_samples,
        bayesian_full_confidence_samples,
        training_history_retention_days,
        unlisted_recipient_penalty,
        baseline_standing_publish,
    );
    let auth = state.db.get_auth_policy().await.map_err(internal)?;
    overlay_policy!(
        auth,
        reply.auth,
        enforce_dmarc,
        enforce_dmarc_quarantine,
        enforce_spf_hardfail,
        enforce_dkim,
        log_only,
        max_auth_failures_per_minute,
        max_conn_per_ip,
    );
    let submission = state.db.get_submission_policy().await.map_err(internal)?;
    overlay_policy!(
        submission,
        reply.submission,
        max_per_day,
        max_recipients_per_message,
    );
    let imap = state.db.get_imap_policy().await.map_err(internal)?;
    overlay_policy!(
        imap,
        reply.imap,
        idle_timeout_secs,
        tombstone_retention_days,
        delete_nonempty,
        bodystructure_cache_max,
        storage_bytes_default,
        message_count_default,
    );
    let outbound = state.db.get_outbound_policy().await.map_err(internal)?;
    overlay_policy!(
        outbound,
        reply.outbound,
        retry_schedule_seconds,
        permanent_failure_timeout_hours,
        delay_warning_at_hours,
        ndr_rate_limit_days,
        suppress_ndr_spf_hardfail,
        suppress_ndr_dmarc_reject,
        postmaster_cc_bounces,
        tlsrpt_send_reports,
        ipv6_enabled,
        treat_5xx_as_transient,
    );

    Ok(reply)
}

fn fetch_config_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_config").await?;
            let req: FetchConfigRequest = decode(&payload).map_err(malformed)?;
            if req.scope != "all" {
                return Err(malformed(format!("unknown scope: {}", req.scope)));
            }
            let reply = assemble_fetch_config_reply(&state).await?;
            encode_reply(&reply)
        })
    })
}

/// `fauna.bridges.get_mail_config` — the **admin read twin** of
/// `fetch_config`. Returns the same overlaid effective `FetchConfigReply` the
/// bridge would see, so the admin client's `admin-mail` policy form hydrates
/// from real persisted state (catalog defaults + the `put_<substruct>_policy`
/// overrides) before edit. Admin-class only (allowlist gate); read-only (no
/// `config_changed` fan-out). The nest-side **alias** policy is *not* in this
/// reply — see `get_alias_policy` (its own admin read twin). Spec:
/// `docs/goal/behavior/mail-policy-config.md` § Implementation status today.
fn get_mail_config_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_mail_config").await?;
            let _req: GetMailConfigRequest = decode(&payload).map_err(malformed)?;
            let reply = assemble_fetch_config_reply(&state).await?;
            encode_reply(&reply)
        })
    })
}

// ── put_<substruct>_policy (A3 Bucket B) ──────────────────────
//
// Admin-class only (allowlist gate). Five uniform kinds, one per
// `FetchConfigReply` sub-struct, each upserting its single-row
// `mail_<substruct>_policy` table; `fauna.bridges.fetch_config` reads
// them back and overlays each `Some(...)` onto the wire-type catalog
// default before encoding (NULL / absent ⇒ catalog default). The bridge
// already reads every one of these fields — this is purely the write
// path. Subscribe-and-hot-reload (`config_changed` push) is still
// deferred per `docs/goal/behavior/mail-policy-config.md` § Impl status;
// a write takes effect on the bridge's next `fetch_config`.
//
// Construct a `<X>PolicyOverrides` from a `Put<X>PolicyRequest`. The wire
// request and DB override structs are field-identical by construction; a
// missing or misnamed field is a compile error.
macro_rules! overrides_from_req {
    ($req:expr, $ty:ident, $($field:ident),+ $(,)?) => {
        $ty { $( $field: $req.$field ),+ }
    };
    // A required (non-`Option`) request field, stored as a stated override.
    ($req:expr, $ty:ident, $($field:ident),+ ; required $($req_field:ident),+ $(,)?) => {
        $ty { $( $field: $req.$field, )+ $( $req_field: Some($req.$req_field) ),+ }
    };
}

pub(crate) fn put_spam_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_spam_policy").await?;
            let req: PutSpamPolicyRequest = decode(&payload).map_err(malformed)?;
            // The bridge builds a `fauna_mail::SpamPolicy` directly and
            // `decide_spam_disposition` debug-asserts the ordering of the
            // two tiers when both are enabled; reject an override whose
            // *effective* (override-or-default) thresholds violate it so a bad
            // write can't crash/misbehave the MTA. `0 = disabled` for a tier —
            // the permissive auto-Junk default is folder=5, reject=0, and an
            // admin opts into the reject tier by setting a non-zero value;
            // with both non-zero, spam_folder < reject must hold.
            let d = SpamPolicyThresholds::default();
            let folder = req
                .max_score_before_spam_folder
                .unwrap_or(d.max_score_before_spam_folder);
            let reject = req
                .max_score_before_reject
                .unwrap_or(d.max_score_before_reject);
            if folder != 0 && reject != 0 && folder >= reject {
                return Err(malformed(format!(
                    "spam thresholds: when both are non-zero, spam_folder < reject must hold \
                     (0 = disabled); effective values were spam_folder={folder}, \
                     reject={reject}"
                )));
            }
            // The confidence ramp is `clamp((n - min)/(full - min), 0, 1)`
            // (`mail-spam.md` § Combined-score formula): an effective
            // `full_confidence_samples <= min_samples` inverts/collapses the
            // ramp (the scorer would treat anything past the floor as full
            // confidence — a config error, not a smooth ramp). Reject it on
            // the effective (override-or-default) values so a bad write can't
            // misconfigure the MDA/nest scorer.
            let eff_min = req.bayesian_min_samples.unwrap_or(d.bayesian_min_samples);
            let eff_full = req
                .bayesian_full_confidence_samples
                .unwrap_or(d.bayesian_full_confidence_samples);
            if eff_full <= eff_min {
                return Err(malformed(format!(
                    "spam bayesian knobs: bayesian_full_confidence_samples must be strictly \
                     greater than bayesian_min_samples (the confidence ramp spans \
                     [min, full]); effective values were min_samples={eff_min}, \
                     full_confidence_samples={eff_full}"
                )));
            }
            // The product ceiling has an upper bound: MAX_MESSAGE_BYTES_CEILING
            // (`mail-message-size.md` § Message size limits, ruled 2026-08-26).
            // The bound is not cosmetic — the ClamAV gate's cap is *derived*
            // from this knob (`mail-content-scanning.md` § Oversize messages),
            // and the scan sidecar's stream/scan/file limits are shipped by the
            // deployment bundle sized for the bound. A knob above it would name
            // a ceiling the scanner cannot be configured to cover, so refuse the
            // write rather than accept a ceiling we cannot honour. Checked on the
            // effective (override-or-default) value, like the Bayesian ramp above.
            // This refusal is the knob's only bound: no stored knob exceeds it.
            let eff_max_message_bytes = req.max_message_bytes.unwrap_or(d.max_message_bytes);
            if eff_max_message_bytes > fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING {
                return Err(malformed(format!(
                    "spam max_message_bytes: must not exceed {ceiling} bytes (the scan sidecar's \
                     limits are shipped for that bound, and the malware gate scans everything the \
                     perimeter accepts); requested value was {eff_max_message_bytes}",
                    ceiling = fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING,
                )));
            }
            let overrides = overrides_from_req!(
                req,
                SpamPolicyOverrides,
                max_score_before_spam_folder,
                max_score_before_reject,
                dnsbl_servers,
                reject_no_rdns,
                greylist_enabled,
                greylist_delay_secs,
                max_conn_per_min,
                fcrdns_mode,
                helo_identity_required,
                reject_fcrdns_fail,
                max_message_bytes,
                bayesian_weight_milli,
                bayesian_min_samples,
                bayesian_full_confidence_samples,
                training_history_retention_days,
                unlisted_recipient_penalty;
                required baseline_standing_publish,
            );
            let stored = state.db.get_spam_policy().await.map_err(internal)?;
            let was_standing = stored.effective().baseline_standing_publish;
            let now_standing = req.baseline_standing_publish;
            state
                .db
                .put_spam_policy(overrides)
                .await
                .map_err(internal)?;
            // Off is an unpublish: the nest stops running publishes AND
            // withdraws the standing baseline, whoever contributed to it
            // (`mail-spam.md` § Cold start Path 2 → *Standing publish*).
            if was_standing && !now_standing {
                state.db.withdraw_spam_baseline().await.map_err(internal)?;
            }
            notify_bridges_config_changed(&state, config_change_reason::SPAM_POLICY).await;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn put_auth_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_auth_policy").await?;
            let req: PutAuthPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = overrides_from_req!(
                req,
                AuthPolicyOverrides,
                enforce_dmarc,
                enforce_dmarc_quarantine,
                enforce_spf_hardfail,
                enforce_dkim,
                log_only,
                max_auth_failures_per_minute,
                max_conn_per_ip,
            );
            state
                .db
                .put_auth_policy(overrides)
                .await
                .map_err(internal)?;
            notify_bridges_config_changed(&state, config_change_reason::AUTH_POLICY).await;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn put_submission_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_submission_policy").await?;
            let req: PutSubmissionPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = overrides_from_req!(
                req,
                SubmissionPolicyOverrides,
                max_per_day,
                max_recipients_per_message,
            );
            state
                .db
                .put_submission_policy(overrides)
                .await
                .map_err(internal)?;
            notify_bridges_config_changed(&state, config_change_reason::SUBMISSION_POLICY).await;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn put_imap_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_imap_policy").await?;
            let req: PutImapPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = overrides_from_req!(
                req,
                ImapPolicyOverrides,
                idle_timeout_secs,
                tombstone_retention_days,
                delete_nonempty,
                bodystructure_cache_max,
                storage_bytes_default,
                message_count_default,
            );
            state
                .db
                .put_imap_policy(overrides)
                .await
                .map_err(internal)?;
            notify_bridges_config_changed(&state, config_change_reason::IMAP_POLICY).await;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn put_outbound_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_outbound_policy").await?;
            let req: PutOutboundPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = overrides_from_req!(
                req,
                OutboundPolicyOverrides,
                retry_schedule_seconds,
                permanent_failure_timeout_hours,
                delay_warning_at_hours,
                ndr_rate_limit_days,
                suppress_ndr_spf_hardfail,
                suppress_ndr_dmarc_reject,
                postmaster_cc_bounces,
                tlsrpt_send_reports,
                ipv6_enabled,
                treat_5xx_as_transient,
            );
            state
                .db
                .put_outbound_policy(overrides)
                .await
                .map_err(internal)?;
            notify_bridges_config_changed(&state, config_change_reason::OUTBOUND_POLICY).await;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Admin write path for the four nest-side alias-policy knobs
/// (`mail-policy-config.md` § Inbound perimeter). Unlike the five
/// `put_<substruct>_policy` handlers above, the values are **not**
/// projected to the bridge via `fetch_config` — they are read nest-side by
/// `resolve_recipient_handler` (the `+suffix` / wildcard gates) and the
/// alias CRUD handlers (the exact-alias cap + the reserved-local-part
/// check). `reserved_local_parts` set here is the single tunable list the
/// future admin-forwarder create-time reserved-local-part check also reads
/// (§ AF).
fn put_alias_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.put_alias_policy").await?;
            let req: PutAliasPolicyRequest = decode(&payload).map_err(malformed)?;
            let overrides = overrides_from_req!(
                req,
                AliasPolicyOverrides,
                exact_aliases_max,
                reserved_local_parts,
                subaddressing_enabled,
                wildcard_prefix_enabled,
            );
            state
                .db
                .put_alias_policy(overrides)
                .await
                .map_err(internal)?;
            encode_reply(&PutPolicyReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.bridges.get_alias_policy` — the **admin read twin** of
/// `put_alias_policy`. Returns the **effective** (override-or-default) nest-side
/// alias policy so the `admin-mail` form hydrates before edit. Unlike the five
/// projected `put_<substruct>_policy` kinds, the four alias knobs are *not* in
/// `FetchConfigReply` (they are consumed nest-side by the alias resolver +
/// alias CRUD), so they read back through this dedicated kind — mirroring the
/// write-path split. Admin-class only (allowlist gate); read-only (no
/// `config_changed` fan-out). Spec: `docs/goal/behavior/mail-policy-config.md`
/// § Implementation status today.
fn get_alias_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.get_alias_policy").await?;
            let _req: GetAliasPolicyRequest = decode(&payload).map_err(malformed)?;
            // `.effective()` resolves each unset override to its
            // `fauna_mail::aliases` const default, so every wire field is
            // concrete (the same `None ⇒ default` overlay the resolver applies).
            let eff = state
                .db
                .get_alias_policy()
                .await
                .map_err(internal)?
                .effective();
            let reply = AliasPolicy {
                exact_aliases_max: eff.exact_aliases_max,
                reserved_local_parts: eff.reserved_local_parts,
                subaddressing_enabled: eff.subaddressing_enabled,
                wildcard_prefix_enabled: eff.wildcard_prefix_enabled,
                // Forward-compat catch-all (transport.md rule 4); nest-minted.
                extra: Default::default(),
            };
            encode_reply(&reply)
        })
    })
}

// ── whoami ────────────────────────────────────────────────────
//
// Returns the calling bridge's role + identity. Replaces trial-and-
// error role discovery (the bridge previously could only learn its
// role by getting `permission_denied` on the wrong allowlist half);
// B.8's `main.go` calls this after the WS handshake completes and
// dispatches `mta.Run` vs `mda.Run` from the reply.
//
// Lookup is against the bridge service-user table; non-bridge actors
// (regular users, admins) get `fauna.bridges.permission_denied` from
// the allowlist gate before we even decode the body.

fn whoami_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.whoami").await?;
            // Body is an empty struct; decode to enforce wire shape
            // (rejecting trailing junk) without using the value.
            let _req: WhoamiRequest = decode(&payload).map_err(malformed)?;
            let row = state
                .db
                .lookup_bridge_service_user(&actor_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    // Defence-in-depth — the allowlist gate above
                    // should have already rejected this path. Fail
                    // closed if the row was deleted between the
                    // class-resolution call and the body decode.
                    permission_denied("actor is not an enrolled bridge service user")
                })?;
            let x25519_hex = row.x25519_pubkey.map(hex::encode).unwrap_or_default();
            // Per-bridge domain is NOT on this reply — see
            // docs/goal/behavior/mail-bridge-lifecycle.md § Wire shapes.
            // The bridge reads `local_domains` + `primary_domain` from
            // `fauna.bridges.fetch_config`'s reply instead (those
            // project the `mail_domains` table).
            let reply = WhoamiReply {
                role: row.role.as_str().to_string(),
                bridge_id: row.bridge_id,
                status: row.status.as_str().to_string(),
                ed25519_pubkey_hex: hex::encode(actor_id),
                x25519_pubkey_hex: x25519_hex,
                // The nest's NAT axis (now client-set, not the boot seed) so
                // the MDA can default its IMAP/CalDAV bind to loopback on a
                // private nest (deployment-home-with-public-relay.md
                // § Plaintext-mode behavior). Per-request read of the live
                // `AppState.node_mode` RwLock — cheap, no DB round-trip; the
                // admin-panel toggle flips it and the next `whoami` follows.
                node_mode: state.node_mode.read().await.as_str().to_string(),
            };
            encode_reply(&reply)
        })
    })
}

// ── check_submission_quota ────────────────────────────────────

fn check_submission_quota_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.check_submission_quota").await?;
            let req: CheckSubmissionQuotaRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if req.recipient_count == 0 {
                return Err(malformed("recipient_count must be > 0"));
            }
            let day_bucket = now_epoch_secs() / 86_400;
            // `.effective()` resolves an unset admin override to the
            // wire-catalog default — an admin-written `put_submission_policy`
            // must actually be enforced here, not just advertised via
            // `fetch_config` (mail-policy-config.md § Submission policy).
            let effective = state
                .db
                .get_submission_policy()
                .await
                .map_err(internal)?
                .effective();
            // The per-message cap is nest's authoritative path, not the
            // submission token's `MaxRecipients` fast-path — the client mints
            // that token from its own hard-coded constant, never from this
            // admin knob (mail-policy-config.md § Implementation status
            // today, sweep-169 finding). Checked before the daily quota so a
            // rejected over-cap message never consumes the actor's allowance.
            if req.recipient_count > effective.max_recipients_per_message {
                return encode_reply(&CheckSubmissionQuotaReply::OverQuota {
                    remaining: effective.max_recipients_per_message,
                });
            }
            // The charging rule (smtp-server.md § Architectural rules, ruled
            // 2026-09-25): the bridge calls once per accepted RCPT, and one
            // call costs one unit — for a recipient that leaves the
            // deployment. `recipient_count` is the RUNNING count and feeds
            // only the per-message cap above; debiting it charged a
            // k-recipient message k(k+1)/2 units. A recipient the bridge
            // resolved to a mailbox on this deployment costs nothing — the
            // line `fauna.email.send` already draws with its `remote_addrs`
            // (`email_handlers.rs`). An absent `recipient_is_local` is the serde
            // default `false` (remote), so such a recipient counts.
            if req.recipient_is_local {
                return encode_reply(&CheckSubmissionQuotaReply::Allowed);
            }
            let max_per_day = effective.max_per_day;
            let outcome = state
                .db
                .try_consume_submission_quota(&target, day_bucket, 1, max_per_day)
                .await
                .map_err(internal)?;
            let reply = match outcome {
                SubmissionQuotaOutcome::Allowed => CheckSubmissionQuotaReply::Allowed,
                SubmissionQuotaOutcome::OverQuota { remaining } => {
                    CheckSubmissionQuotaReply::OverQuota { remaining }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── content-scan row helpers (T1.4) ─────────────────────────

/// Project an optional rspamd score onto the four `message_scan_results`
/// rspamd columns: `rspamd_score_raw` / `rspamd_score_scaled` (milli-ints),
/// `rspamd_flagged_rules` (JSON array of rule names), `rspamd_score_breakdown`
/// (JSON object `{rule: score_milli}`). Shared by the `ingest_inbound_mail`
/// scan-row write and the `report_rejected_scan` forensic path so both paths
/// serialize the score identically.
fn rspamd_row_fields(
    score: Option<&RspamdScore>,
) -> (Option<i64>, Option<i64>, Option<String>, Option<String>) {
    match score {
        Some(s) => {
            let rules =
                serde_json::to_string(&s.flagged_rules).unwrap_or_else(|_| "[]".to_string());
            let breakdown: std::collections::BTreeMap<&str, i32> = s
                .breakdown
                .iter()
                .map(|c| (c.rule.as_str(), c.score_milli))
                .collect();
            let breakdown_json =
                serde_json::to_string(&breakdown).unwrap_or_else(|_| "{}".to_string());
            (
                Some(i64::from(s.raw_milli)),
                Some(i64::from(s.scaled_milli)),
                Some(rules),
                Some(breakdown_json),
            )
        }
        None => (None, None, None, None),
    }
}

// ── report_rejected_scan ─────────────────────────────────────
//
// T1.4 forensic report for a reject-at-perimeter ClamAV hit. A `reject`-action
// infected message 554s at SMTP DATA and is never stored, so it makes no
// `ingest_inbound_mail` call — but the admin still wants the audit row ("we
// rejected this") per `mail-content-scanning.md` § Actions (the row is inserted
// with `delivered=false`). The nest derives a synthetic, deterministic
// `message_id` (there is no ingest id for a never-stored message) and inserts
// via the shared `insert_scan_result` with `delivered_to_actor = NULL`,
// `action_taken = 'rejected_malware'`. Metadata only — the rejected bytes never
// cross to the nest (`content-scoring.md` § The scoring-metadata bus).

/// Domain-separation tag for the synthetic forensic-row id.
const REPORT_REJECTED_SCAN_DST: &[u8] = b"fauna.bridges.report_rejected_scan.v1";

/// Deterministic id for a rejected-message forensic row. Length-prefixed
/// domain tag + signature, then the timestamp + sender domain — the
/// length-prefix framing keeps unrelated inputs from aliasing. (This is a
/// forensic-row key, not a record identity: stored mail records file under
/// the content hash of their envelope bytes — `message-segment-store.md`
/// § Record identity per kind.) Deterministic ⇒ a retried report is
/// idempotent (`INSERT OR REPLACE`).
fn rejected_scan_message_id(signature: &str, received_at: i64, sender_domain: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    write_fields(
        REPORT_REJECTED_SCAN_DST,
        &[
            HashField::LenPrefixed(signature.as_bytes()),
            HashField::I64(received_at),
            HashField::Trailing(sender_domain.as_bytes()),
        ],
        |b| {
            h.update(b);
        },
    );
    *h.finalize().as_bytes()
}

async fn report_rejected_scan(
    state: Arc<AppState>,
    bridge_actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    require_class(&state, &bridge_actor, "fauna.bridges.report_rejected_scan").await?;
    let req: ReportRejectedScanRequest = decode(&payload).map_err(malformed)?;
    if req.clamav_signature.trim().is_empty() {
        return Err(malformed("clamav_signature must not be empty"));
    }
    let message_id =
        rejected_scan_message_id(&req.clamav_signature, req.received_at, &req.sender_domain);
    let (rspamd_score_raw, rspamd_score_scaled, rspamd_flagged_rules, rspamd_score_breakdown) =
        rspamd_row_fields(req.rspamd_score.as_ref());
    state
        .db
        .insert_scan_result(&ScanResultRow {
            message_id,
            received_at: req.received_at,
            scanned_at: req.received_at,
            clamav_verdict: "infected".to_string(),
            clamav_signature: Some(req.clamav_signature),
            rspamd_score_raw,
            rspamd_score_scaled,
            rspamd_flagged_rules,
            rspamd_score_breakdown,
            action_taken: "rejected_malware".to_string(),
            // Never stored to an actor — this message was rejected at the
            // perimeter. NULL `delivered_to_actor` is what scopes it out of the
            // per-user "my scan results" query and into the admin forensic view.
            delivered_to_actor: None,
        })
        .await
        .map_err(internal)?;
    encode_reply(&ReportRejectedScanReply {
        message_id: message_id.to_vec(),
    })
}

fn report_rejected_scan_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| Box::pin(report_rejected_scan(state, actor_id, payload)))
}

// ── ingest_inbound_mail / submit_inbound_mail ────────────────

/// Shared validation + persistence for the two encrypted-mail-write
/// kinds. `ingest_inbound_mail` (B.2) is external-MX inbound and sets
/// `is_own_submission=false`; `submit_inbound_mail` (B.3) is
/// authenticated-MTA submission and sets it to `true`. The flag is
/// server-set per kind so a malicious MTA can't claim external traffic
/// is "own-submission" by lying on the wire.
async fn persist_inbound_mail_request(
    state: Arc<AppState>,
    bridge_actor: [u8; 32],
    payload: Bytes,
    kind: &'static str,
    is_own_submission: bool,
) -> Result<Bytes, RpcError> {
    require_class(&state, &bridge_actor, kind).await?;
    let mut req: IngestInboundMailRequest = decode(&payload).map_err(malformed)?;
    // The sealed body either rode the frame inline, or — being over the inline
    // budget — crossed on the bulk-byte plane and is named here by reference
    // (`smtp-server.md` § Message size limits). Resolving the reference yields the
    // identical byte string the producer sealed, which then takes exactly the path
    // below that an inline body takes: same seal gate, same `append_record`, same
    // quota charge. Only the transport differed.
    //
    // Exactly one of the two must be present. Rejecting "neither" is what stops a
    // version-skewed producer — a new MTA staging a reference at an older nest that
    // drops the unknown key — from ever storing an *empty* message: the failure
    // surfaces as a typed error the sender is told about, not as silent mail loss.
    let body_bytes = match req.body_ref.take() {
        Some(r) => {
            if !req.encrypted_body.is_empty() {
                return Err(malformed(
                    "encrypted_body must be empty when body_ref is set",
                ));
            }
            // The product ceiling's upper bound, not the live knob: the MTA's
            // perimeter already admitted this message against the knob, and a
            // knob lowered in between must not refuse mail it accepted. Bounds
            // the reference before any chunk is read
            // (`mail-message-size.md` § Message size limits).
            crate::mail_body_plane::resolve_body_ref(
                &state,
                &r,
                u64::from(fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING)
                    + u64::from(fauna_mail::transport_limits::SEAL_ENVELOPE_ALLOWANCE_BYTES),
            )
            .await?
        }
        None => {
            if req.encrypted_body.is_empty() {
                return Err(malformed(
                    "ingest carries neither an inline encrypted_body nor a body_ref",
                ));
            }
            std::mem::take(&mut req.encrypted_body)
        }
    };
    // S6.12b structural seal gate: prove the payload halves are sealed
    // recipient envelopes at the wire edge (the Go MTA seals both,
    // unconditionally, in both modes) — nothing unsealed can reach the
    // backup-eligible `__mail` segment store. `mem::take` so the typed values
    // are the only copy that flows onward.
    let body = SealedRecordBytes::verify(body_bytes)
        .map_err(|_| malformed("encrypted_body is not a sealed recipient envelope"))?;
    let hint = SealedRecordBytes::verify(std::mem::take(&mut req.encrypted_index_hint))
        .map_err(|_| malformed("encrypted_index_hint is not a sealed recipient envelope"))?;
    // The envelope sender the MTA carried. Empty = the SMTP null reverse-path
    // `<>` (or a bridge that sent no sender): gated unless the report proves it bounces a
    // message the ward actually sent — anyone on the internet can claim `<>`, so
    // it must never read as a known sender (`family-safety.md` § The mail gate).
    // The correlation is `dsn_original_msgid`, NOT `dsn_recipient`: an address
    // the ward mailed is often public, an id its client minted is not. A
    // message with no such id fails *closed* for supervised recipients (held,
    // never lost).
    let sender_address = std::mem::take(&mut req.sender_address);
    // The escalation-break pre-check (`family-safety.md` § The mail gate): a
    // null-path report may take the consuming correlation probe only when its
    // address-header set is *plausible* — non-empty (a genuine DSN always
    // carries `From: MAILER-DAEMON@…`; empty means a hostile
    // costume dodging the reply-seed suppression) and small (the set is what
    // the suppression matches, so a stuffed Cc: must not bloat it; no real
    // DSN approaches the cap). Outside that window the probe is skipped
    // entirely — msgid `None` → held — so the report burns no budget.
    let dsn_original_msgid = std::mem::take(&mut req.dsn_original_msgid)
        .filter(|_| (1..=MAX_DSN_REPORT_ADDRESSES).contains(&req.dsn_report_addresses.len()));
    let message_id = persist_decoded_inbound_mail(
        state,
        req,
        body,
        hint,
        is_own_submission,
        MailIngress::from_envelope(&sender_address, dsn_original_msgid.as_deref()),
    )
    .await?;
    encode_reply(&IngestInboundMailReply {
        message_id: message_id.to_vec(),
    })
}

/// Seal `plaintext` to `recipient_pubkey`, selecting the post-quantum X-Wing
/// suite iff `mlkem_ek` is a valid-length (1184-B) ML-KEM ek; otherwise the
/// classical X25519 seal. A recipient's seal key always carries its ek, so
/// `None` is passed only for a key no ek pairs with — the index hint sealed
/// to a recipient's separate index key. Both the
/// in-domain body and the companion index hint route through this one selector
/// so they share a single suite-selection path (PQ-6 brought the hint under
/// surface A — `post-quantum.md`). Degrades to the classical seal on an X-Wing
/// seal *error* (PQ-4b) rather than failing closed: the ek is length-gated but
/// FIPS-203-validated only at encaps, so a right-length-but-invalid ek would
/// otherwise self-DoS the recipient's inbound mail (publish is authz-bound to
/// self → no cross-user DoS). `what` labels the warn-log so a *systemic* "PQ
/// silently off" regression stays observable.
pub(crate) fn seal_recipient_blob(
    plaintext: &[u8],
    recipient_pubkey: &[u8; 32],
    mlkem_ek: Option<&[u8]>,
    what: &str,
) -> Result<Vec<u8>, RpcError> {
    let xwing_ek = mlkem_ek.filter(|ek| ek.len() == ML_KEM_768_ENCAPS_KEY_LEN);
    if let Some(ek) = xwing_ek {
        let mut ek_arr = [0u8; ML_KEM_768_ENCAPS_KEY_LEN];
        ek_arr.copy_from_slice(ek);
        let xwing_pk =
            fauna_mls::wrapped_blob::XWingPublicKey::from_parts(ek_arr, *recipient_pubkey);
        match fauna_mls::wrapped_blob::seal_to_recipient_xwing(plaintext, &xwing_pk) {
            Ok(env) => return env.to_canonical_bytes().map_err(internal),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    what,
                    "X-Wing in-domain seal failed; degrading to classical X25519 seal (PQ-4b)"
                );
            }
        }
    }
    fauna_mls::wrapped_blob::seal_to_recipient(plaintext, recipient_pubkey)
        .map_err(internal)?
        .to_canonical_bytes()
        .map_err(internal)
}

/// The nest-side composer for the **guardian mail gate** (`family-safety.md`
/// § The mail gate) — the mail sibling of `routes::reach_floor`.
///
/// Reads only facts the recipient's own nest **stored** — the `guardian_policies`
/// row, the `guardian_mail_allowlist`, the ward's sent-Message-ID set — and hands
/// the verdict to the shared pure primitive [`supervised_mail_verdict`]. It never
/// consults a *decision* the bridge declares: an MTA that skipped the `RCPT TO`
/// gate, or invented a spam disposition, still meets the same verdict here. That
/// is what makes the pillar hold against **external senders and non-conforming
/// clients**.
///
/// It does **not** hold against a compromised MTA bridge, and does not claim to:
/// the bridge is inside the mail TCB — it supplies the recipient actor, the sealed
/// body, and the envelope facts this verdict keys on — so the gate can never be
/// stronger than the bridge feeding it, exactly as the reach floor can never be
/// stronger than the client that authenticates.
///
/// An unsupervised recipient, a known correspondent, or system-generated mail all
/// resolve to `Deliver` without the caller needing to pre-check the link.
pub(crate) async fn guardian_mail_verdict(
    state: &AppState,
    target: &[u8; 32],
    ingress: MailIngress<'_>,
) -> Result<GuardianMailOutcome, RpcError> {
    if ingress == MailIngress::System {
        return Ok(GuardianMailOutcome::plain(MailVerdict::Deliver));
    }
    let Some(policy) = state
        .db
        .get_guardian_policy(target)
        .await
        .map_err(internal)?
    else {
        return Ok(GuardianMailOutcome::plain(MailVerdict::Deliver));
    };
    let knob = UnknownSenderMail::from_wire(&policy.unknown_sender_mail);
    // When the knob is inert (`allow`), the verdict is `Deliver` no matter
    // what the probe would say — short-circuit *before* probing, because the
    // null-path probe below is consuming: it spends a unit of the sent-msgid
    // correlation budget, and an inert gate must not drain the budget a later,
    // stricter setting will rely on.
    if supervised_mail_verdict(Some(knob), false, ingress) == MailVerdict::Deliver {
        return Ok(GuardianMailOutcome::plain(MailVerdict::Deliver));
    }
    // The correlation probe — a *different* stored fact per ingress class:
    //
    // - A named envelope sender is known iff the ward's allowlist holds the
    //   address (the outbound auto-seed put it there, so replies always flow).
    // - The null reverse-path is known iff the report names the **original
    //   Message-ID of a message the ward actually sent**, within the retention
    //   window, **with correlation budget left** — the probe consumes a unit.
    //   NOT the address the report claims to bounce: that only establishes
    //   "the ward once mailed that address", which anyone may
    //   claim. And not a *durable*
    //   id match either: threading leaks the id in `References:` to every
    //   later thread participant, so an unconsumed correlation was an open
    //   delivery channel to exactly the population the gate
    //   excludes.
    //   The seed side bounds who can name an id at all: only ids a Fauna path
    //   minted (128 random bits) are ever recorded.
    //
    // A null-path message with no extractable id — no report, an unparseable
    // one, or a bridge that sent no id — probes nothing and is never known.
    let mut spent_correlation = false;
    let known = match ingress {
        MailIngress::Sender(addr) => state
            .db
            .is_known_mail_sender(target, addr)
            .await
            .map_err(internal)?,
        MailIngress::NullReversePath {
            dsn_original_msgid: Some(msgid),
        } => {
            spent_correlation = state
                .db
                .consume_sent_msgid_correlation(target, msgid)
                .await
                .map_err(internal)?;
            spent_correlation
        }
        MailIngress::NullReversePath {
            dsn_original_msgid: None,
        } => false,
        MailIngress::System => unreachable!("handled above"),
    };
    Ok(GuardianMailOutcome {
        verdict: supervised_mail_verdict(Some(knob), known, ingress),
        spent_correlation,
    })
}

/// What [`guardian_mail_verdict`] decided, plus the one fact the placement
/// path needs beyond the verdict: whether this delivery was *authorized by
/// the consumed sent-Message-ID correlation*. Exactly those deliveries get
/// their report's address set recorded
/// ([`CacheDb::add_correlated_delivery_origins`]) so the outbound auto-seed
/// declines them — the escalation break
/// (`family-safety.md` § The mail gate). An `allow`-knob short-circuit is
/// deliberately NOT one: the gate is inert there, cold mail flows regardless,
/// and the correlation neither ran nor spent budget.
pub(crate) struct GuardianMailOutcome {
    pub verdict: MailVerdict,
    /// True iff a unit of the ward's sent-Message-ID correlation budget was
    /// spent producing this verdict — which implies `verdict == Deliver`
    /// ([`supervised_mail_verdict`] is monotone in `known`).
    pub spent_correlation: bool,
}

/// Plausibility cap on a null-path report's address-header set
/// (`dsn_report_addresses`). A genuine DSN carries `From: MAILER-DAEMON@…`
/// and rarely anything beyond the ward's own address; a set past this cap is
/// header-stuffing aimed at bloating the correlated-origins suppression
/// table, and the report is held without probing (budget unburnt). The Go
/// MTA truncates its extraction at cap + 1, so an over-stuffed list still
/// arrives as over-stuffed rather than disguised at the cap.
pub(crate) const MAX_DSN_REPORT_ADDRESSES: usize = 16;

impl GuardianMailOutcome {
    fn plain(verdict: MailVerdict) -> Self {
        Self {
            verdict,
            spent_correlation: false,
        }
    }
}

/// The typed refusal both `reject` enforcement points share
/// (`family-safety.md` § The mail gate). Mirrors
/// `fauna.inbox.guardian_approval_required` on the contact-reach side.
pub(crate) fn mail_guardian_approval_required(recipient: &str) -> RpcError {
    crate::rpc_errors::guardian_approval_required_ns(
        "email",
        format!("{recipient} only accepts mail from senders their guardian has approved"),
    )
}

/// Seal a sender-supplied plaintext RFC 5322 message to `target`'s MSEK-derived
/// pubkey and persist it through the shared sealed-ingest core
/// ([`persist_decoded_inbound_mail`]). `is_own_submission` selects the
/// destination (and the quota/filter semantics — see that core): `false` →
/// `target`'s INBOX (a *delivery*, the in-domain / role / bounce paths); `true`
/// → `target`'s own `Sent` mailbox, `\Seen`, quota-exempt, unfiltered (an
/// own-submission copy of mail `target` itself sent). The nest seals here
/// because the caller holds plaintext (the same trust model as a
/// Thunderbird→MTA submission) — **in both storage modes** (Phase-3 D1,
/// `2026-07-07-phase-3-sealed-both-modes-design.md`: the design-(b)
/// no-seal-at-ingest branch is deleted; one at-rest byte shape, the nest core
/// never holds a content key). `target` must have provisioned an MSEK-derived
/// MLS pubkey or this errors (never a silent drop). The index hint is tokenized
/// exactly as the MTA does (`fauna_recipient.go` → `Tokenize` over "subject
/// body") so the mail is searchable by the same MDA SEARCH path.
///
/// `dedup` is the pair the caller minted from the message as it was sent —
/// **before** any per-recipient delivery stamp went on, which is why it is a
/// parameter and not computed from `raw_rfc5322` here: this function may see
/// stamped bytes, and a key over those would differ per recipient
/// (`mailbox-migration.md` § The envelope key confirms a Message-ID hit →
/// *Producers send the pair, always*).
async fn seal_and_persist_local(
    state: &Arc<AppState>,
    target: &[u8; 32],
    raw_rfc5322: &[u8],
    dedup: fauna_mail::MailDedupKeyPair,
    sender_domain: &str,
    is_own_submission: bool,
    ingress: MailIngress<'_>,
) -> Result<(), RpcError> {
    // Phase-3 D2: this is a genuine new-mail-ingest seal site (in-domain
    // delivery), so it resolves through the content-sealing-epochs mail seam
    // — never ad-hoc column reads, and never the shared (non-mail-aware)
    // `get_recipient_seal_key` directly.
    let seal_key = state
        .db
        .get_recipient_mail_seal_key(target, state.epoch_sealing_enabled())
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            crate::email_handlers::invalid_params("recipient has no encryption key on file")
        })?;
    let parsed = mail_parser::MessageParser::default().parse(raw_rfc5322);
    let subject = parsed.as_ref().and_then(|p| p.subject()).unwrap_or("");
    let body_text = parsed
        .as_ref()
        .and_then(|p| p.body_text(0))
        .unwrap_or_default();
    let index_hint =
        fauna_mail::tokenizer::tokenize(&format!("{subject} {body_text}")).canonical_bytes;
    // Canonical report-hash over the same parsed fields the MTA hashes
    // (report-sharing.md § Content identity) — this in-domain path is the same
    // trust position as an MTA submission (the caller holds plaintext), so
    // in-domain deliveries aggregate like external ones.
    let report_hash = fauna_mail::report_hash::report_hash(subject, &body_text);
    // Seal the body AND the companion index hint with the post-quantum
    // X-Wing suite: a recipient's seal key always carries its ML-KEM ek
    // (the provision door requires it). PQ-6 brought the hint under the same hybrid surface as
    // the body so the at-rest hint no longer leaks the plaintext body
    // word-set under HNDL (`post-quantum.md` § surface A). Both seal to the
    // recipient's MSEK-derived key — and as of the 2026-08-03 Phase-E ruling
    // that is the DESIGN, not a fallback awaiting replacement: Plan 5b's
    // mail/calendar index-segment key is MSEK-derived and symmetric, so there
    // is no dedicated index keypair to re-point at and no reader that holds
    // one without also holding MSEK (`post-quantum.md` § surface-A scope →
    // Phase-E hand-off). The recipient's reader opens either suite (self-describing
    // blob); `seal_recipient_blob` degrades to classical on a seal *error*
    // (PQ-4b), never failing closed.
    let (body, hint) = {
        let pubkey = &seal_key.mls_pubkey;
        let mlkem_ek = Some(seal_key.mlkem_ek.as_slice());
        let sealed_body = seal_recipient_blob(raw_rfc5322, pubkey, mlkem_ek, "body")?;
        let sealed_hint = seal_recipient_blob(&index_hint, pubkey, mlkem_ek, "index-hint")?;
        (sealed_body, sealed_hint)
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    // S6.12b: mint the typed halves from our own seal output (`verify` is a
    // cheap strict decode of bytes we just sealed — a failure here is a bug in
    // the seal path, surfaced as internal, never a client error).
    let body_typed = SealedRecordBytes::verify(body).map_err(internal)?;
    let hint_typed = SealedRecordBytes::verify(hint).map_err(internal)?;
    let ingest = IngestInboundMailRequest {
        actor_id: target.to_vec(),
        // The wire fields stay empty — persist_decoded_inbound_mail reads only
        // the typed halves.
        public_metadata: fauna_protocol::bridge_routing::PublicMailMetadata {
            timestamp: now,
            ciphertext_size: body_typed.len() as u32,
            sender_domain: sender_domain.to_string(),
        },
        report_hash,
        dedup_key: dedup.dedup_key,
        envelope_key: dedup.envelope_key,
        ..Default::default()
    };
    persist_decoded_inbound_mail(
        state.clone(),
        ingest,
        body_typed,
        hint_typed,
        is_own_submission,
        ingress,
    )
    .await
    .map(|_message_id| ())
}

/// Seal a sender-supplied plaintext RFC 5322 message to one resolved in-domain
/// recipient and **deliver** it into that recipient's `__mail/<actor>` segment
/// store + `bridge_imap_messages` INBOX (readable via `fauna.email.inbox.fetch`
/// /IMAP — the same path the Go MTA's local-recipient delivery takes).
///
/// Shared by `fauna.email.send` (first-party client submission, via
/// `email_handlers::deliver_in_domain`) and the in-domain partition on
/// `fauna.bridges.enqueue_outbound_mail` (MTA submission + MDA auto-schedule),
/// plus the synchronous in-domain bounce / security / forwarder-NDR paths —
/// one seal-and-ingest home (priority #2). The caller resolves `target` first
/// (exact alias / the full `resolve_local_recipient`).
///
/// `ingress` decides whether the guardian mail gate looks at this delivery at
/// all. The bounce / NDR / security-notification callers pass
/// [`MailIngress::System`] — a held delivery-failure notice would strand a ward
/// (`family-safety.md` § The mail gate → *"System-generated mail is never
/// gated"*). The two user-mail callers pass the envelope sender.
///
/// `stamped_headers` are the resolver's per-recipient delivery stamps
/// (`X-Fauna-Address-*` + the `X-Fauna-Spam-Threshold` fold), prepended before
/// the seal exactly as the Go MTA does on its own two delivery paths — so the
/// stamps ride INSIDE the sealed body and reach the agent that unwraps it. It is
/// a required parameter rather than an option because forgetting it is precisely
/// how the nest-side paths diverged from the Go ones; a
/// system-generated sender passes `&[]` and pays nothing.
pub(crate) async fn seal_and_ingest_local(
    state: &Arc<AppState>,
    target: &[u8; 32],
    raw_rfc5322: &[u8],
    sender_domain: &str,
    ingress: MailIngress<'_>,
    stamped_headers: &[(String, String)],
) -> Result<(), RpcError> {
    // The dedup pair is minted from the UNSTAMPED message — the same choice the
    // Go MTA makes for external-MX delivery — so every recipient of one message
    // records the same pair, whatever stamps its own delivery carries.
    let dedup = fauna_mail::mail_dedup_keys_from_slice(raw_rfc5322);
    let stamped = fauna_mail::aliases::prepend_stamped_headers(raw_rfc5322, stamped_headers);
    seal_and_persist_local(
        state,
        target,
        &stamped,
        dedup,
        sender_domain,
        false,
        ingress,
    )
    .await?;
    // An invitation from a sender on this nest's own domain never crosses the
    // MTA, so the nest places it on the calendar itself (caldav-server.md §
    // Server-side auto-schedule, "Inbound invite") — after the mail is safely
    // delivered, and never failing that delivery.
    crate::bridge_caldav_handlers::place_local_invite(state, target, raw_rfc5322, ingress).await;
    Ok(())
}

/// Store a durable server-side **Sent** copy of `raw_rfc5322` for its `sender` —
/// sealed to the sender's OWN MSEK-derived read key and placed in the sender's
/// `Sent` mailbox, so a message composed in a Fauna app survives a client
/// restart and appears on every device (it reloads via `fauna.email.sent.fetch`,
/// the Sent sibling of `inbox.fetch`). This mirrors the server-side Sent copy an
/// external-MUA SMTP submission already leaves (`submit_inbound_mail`), giving
/// `fauna.email.send` and the external-MUA path one uniform Sent-copy home
/// (`smtp-server.md` § Inbound client receive). The sender reads their own Sent
/// mailbox, so the seal target IS the sender (the same `recipient_secret` the
/// client uses for INBOX opens it).
pub(crate) async fn seal_and_store_sent_copy(
    state: &Arc<AppState>,
    sender: &[u8; 32],
    raw_rfc5322: &[u8],
    sender_domain: &str,
) -> Result<(), RpcError> {
    // A Sent copy of the sender's own mail is never gated: the mail gate is
    // about who may reach the ward, not about what the ward sends. It lands in
    // `Sent`, not INBOX, so it never passes the placement branch either.
    seal_and_persist_local(
        state,
        sender,
        raw_rfc5322,
        fauna_mail::mail_dedup_keys_from_slice(raw_rfc5322),
        sender_domain,
        true,
        MailIngress::System,
    )
    .await
}

/// The deployment's primary mail domain — the domain the in-domain↔external
/// recipient partition (`fauna.email.send` + `fauna.bridges.enqueue_outbound_mail`)
/// splits against.
///
/// Resolved solely from the runtime `local_domains` table (`add_local_domain`;
/// `is_primary` nest-derived = first active domain — mail-multidomain.md § The
/// primary domain), which is how a client provisions the mail domain (product
/// invariant: nest config comes from clients, not boot args). `None` when no
/// domain is claimed yet — a nest that can't send/route mail.
///
/// There is no boot-time fallback: the legacy `state.email.domain` field was
/// removed once the last in-process test seam migrated to a real `mail_domains`
/// row. Keying the partition on that field had previously left the in-domain
/// short-circuit DEAD in the shipped binary (every recipient classified remote →
/// in-domain attendees MX-self-loop-relayed and bounced/greylisted), since the
/// field was always `None` on the Go-MTA build while the tier_3 conformance
/// tests seeded it in-process and so never exercised the production path — a gap
/// only the real-image tier_4 acceptance surfaces (`test_caldav_autoschedule_imip.py`).
pub(crate) async fn primary_mail_domain(state: &AppState) -> Result<Option<String>, RpcError> {
    Ok(state
        .db
        .lookup_primary_mail_domain()
        .await
        .map_err(internal)?
        .map(|row| row.domain_name))
}

/// Persist an already-decoded inbound-mail request: validate the sealed
/// payload, insert into the recipient's `__mail` segment store, record the
/// content-scan verdict, bootstrap mailboxes, apply the perimeter
/// filter-placement verdict, place into the IMAP mailbox, and append the
/// placement-journal record. Returns the server-assigned 32-byte
/// `message_id`.
///
/// Shared by the bridge-facing `fauna.bridges.{ingest,submit}_inbound_mail`
/// wrapper (`persist_inbound_mail_request`, which adds `require_class` +
/// decode + reply-encode + the S6.12b wire-edge `SealedRecordBytes::verify`),
/// the in-domain branch of `fauna.email.send` (`email_handlers`, which seals
/// the sender-supplied plaintext to the resolved local recipient and mints
/// from its own seal output), and the `test-hooks` raw-inject hook (which
/// mints `carried_at_rest_unchecked` to simulate a pre-Phase-3 legacy
/// record) — one sealed-ingest path (priority #2). The caller is responsible
/// for caller authorization and for minting `body`/`hint`; `req`'s own
/// `encrypted_body`/`encrypted_index_hint` fields are IGNORED here — the
/// typed halves are the only payload this core reads.
pub(crate) async fn persist_decoded_inbound_mail(
    state: Arc<AppState>,
    req: IngestInboundMailRequest,
    body: SealedRecordBytes,
    hint: SealedRecordBytes,
    is_own_submission: bool,
    ingress: MailIngress<'_>,
) -> Result<[u8; 32], RpcError> {
    let target: [u8; 32] = crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
        .map_err(malformed)?;
    // Taken before `req` is partially moved below (`extra_flags`,
    // `target_mailbox`); consumed at the very end, after the message is durably
    // placed.
    let req_dedup_key = req.dedup_key.clone();
    let req_envelope_key = req.envelope_key.clone();
    // Refused before anything is stored: a delivery that cannot be indexed is
    // a producer that skipped the shared key function, not a message to keep
    // unindexed (`mailbox-migration.md` § *There is no absent key*).
    fauna_mail::require_dedup_pair(&req_dedup_key, &req_envelope_key).map_err(malformed)?;
    if body.is_empty() {
        return Err(malformed("encrypted_body must not be empty"));
    }
    if hint.is_empty() {
        return Err(malformed("encrypted_index_hint must not be empty"));
    }
    if req.public_metadata.sender_domain.trim().is_empty() {
        return Err(malformed("sender_domain must not be empty"));
    }
    if (req.public_metadata.ciphertext_size as usize) != body.len() {
        return Err(malformed(format!(
            "ciphertext_size mismatch: metadata={} body_bytes={}",
            req.public_metadata.ciphertext_size,
            body.len()
        )));
    }
    // The recipient must be a provisioned local actor (has an MSEK-derived MLS
    // pubkey on file) — the key the caller sealed the body to (S1: both modes,
    // unconditionally). Also guards against a misrouted write landing dead
    // bytes for a non-existent recipient.
    if state
        .db
        .get_actor_mls_pubkey(&target)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(malformed("recipient has not provisioned an MLS pubkey"));
    }
    // Inbound-delivery quota enforcement point (imap-server.md § Quota
    // enforcement points → "Inbound mail delivery"). A normal external-MX
    // inbound over the recipient's per-mailbox quota root is rejected with the
    // typed `over_quota` error the Go MTA maps to `552 5.2.2 Mailbox full`.
    // Shares `enforce_imap_storage_quota` with the APPEND/COPY/MOVE write paths
    // (pre-check, not transactional — Dovecot parity, slight over-admission
    // under concurrency is acceptable). Two exemptions:
    //   - **own-submission** (`submit_inbound_mail` → Sent): submission is NOT
    //     an enforcement point — § Quota enforcement points lists APPEND /
    //     COPY / MOVE / inbound-delivery; § Composition (:278) confirms
    //     per-mailbox quota stops APPEND + inbound delivery, while submission
    //     has its own separate (rate-based) quota.
    //   - **role-address deliveries** (postmaster@/abuse@/security@/tlsrpt@/…):
    //     an over-quota admin mailbox still receives postmaster mail
    //     (smtp-server.md § Architectural rules :204). The Go MTA carries
    //     `is_role_address` from `resolve_recipient` onto the ingest request
    //     (both call sites — inbound-MX and submission; `validate_recipient`
    //     is auth-only and discards its own `isRoleAddress` return).
    if !is_own_submission && !req.is_role_address {
        crate::bridge_imap_handlers::enforce_imap_storage_quota(
            &state,
            &target,
            req.public_metadata.ciphertext_size as u64,
            1,
        )
        .await?;
    }
    // ── Guardian mail gate (`family-safety.md` § The mail gate) ──
    //
    // Recomputed here from the recipient's stored policy, never from a flag the
    // bridge carries: this is the choke point every sealed inbound delivery
    // passes — the Go MTA's `ingest_inbound_mail`, the in-domain twin, and the
    // MDA auto-schedule partition alike.
    //
    // `Reject` should already have been refused per-recipient at `RCPT TO` (or
    // at the in-domain twin's sender). Meeting one *here* means a bridge skipped
    // that gate, so we fail closed rather than deliver. `Hold` is a placement
    // decision, applied below. Own-submission (the sender's own `Sent` copy) is
    // outside the gate entirely.
    let mail_outcome = if is_own_submission {
        GuardianMailOutcome::plain(MailVerdict::Deliver)
    } else {
        guardian_mail_verdict(&state, &target, ingress).await?
    };
    let mail_verdict = mail_outcome.verdict;
    if mail_verdict == MailVerdict::Reject {
        return Err(mail_guardian_approval_required("this recipient"));
    }
    // A delivery authorized by the consumed sent-Message-ID correlation gets
    // the report's address-header set recorded, so the ward's *reply* to it
    // cannot auto-seed the allowlist — the report's author chose those
    // addresses, not the ward (§ The mail gate; the escalation break).
    // Recorded BEFORE placement: a crash between the two holds the recording
    // without the delivery (the retry re-records — idempotent), never the
    // delivery without the recording. Hard-fail, not best-effort: an
    // unrecorded correlated delivery silently re-opens the escalation.
    // The correlation budget was already spent one step earlier, at verdict
    // time (the spend IS the probe that decides `known`), so a recording error
    // here fails the delivery with that unit gone and the MTA retry
    // re-decrements. That ordering is inherent — the verdict can't know it's a
    // `Deliver` worth recording until after the probe has run — and it fails
    // toward *holding* (a drained budget only ever over-holds), so it is
    // accepted rather than a reorder target.
    if mail_outcome.spent_correlation && mail_verdict == MailVerdict::Deliver {
        state
            .db
            .add_correlated_delivery_origins(&target, &req.dsn_report_addresses)
            .await
            .map_err(internal)?;
    }
    let disposition_str = match req.spam_disposition {
        SpamDisposition::Accept => "accept",
        SpamDisposition::AcceptToSpamFolder => "accept_to_spam_folder",
        SpamDisposition::PolicyJunk => "policy_junk",
    };
    // The uniform scoring-metadata bus rows (content-scoring.md § The
    // scoring-metadata bus, the contract phase): the perimeter minted them
    // (`fauna_core::scoring::perimeter_mail_score_rows`, called by the Go MTA
    // per recipient) and the nest stores exactly what it was sent — nothing
    // is derived from the per-kind fields, which are the detail record beside
    // the rows, not their source. Empty is a real answer: the own-submission
    // Sent copy was never perimeter-scored and gets no rows.
    let scores = req.scores.clone();
    // Canonical report-hash (report-sharing.md § Content identity): empty =
    // absent (e.g. an IMAP APPEND stores none — the message cannot aggregate, graceful);
    // non-empty must be exactly 32 bytes.
    if !req.report_hash.is_empty() && req.report_hash.len() != 32 {
        return Err(malformed(format!(
            "report_hash must be 32 bytes when present, got {}",
            req.report_hash.len()
        )));
    }
    // DB-layer keeps the legacy RFC-canonical lowercase strings (RFC
    // 7208/6376/7489: no underscores — "softfail" not "soft_fail",
    // "permerror" not "perm_error"). Flatten the nested-enum wire
    // verdicts into those strings here.
    let (dmarc_str, dmarc_policy_str) = dmarc_to_flat(&req.verdicts.dmarc);
    let fields = InboundMailFields {
        actor_id: target,
        timestamp: req.public_metadata.timestamp,
        ciphertext_size: req.public_metadata.ciphertext_size,
        encrypted_body: body,
        encrypted_index_hint: hint,
        sender_domain: req.public_metadata.sender_domain,
        spf: spf_to_flat(&req.verdicts.spf).into(),
        dkim: dkim_to_flat(&req.verdicts.dkim).into(),
        dmarc: dmarc_str.into(),
        dmarc_policy: dmarc_policy_str.into(),
        arc: arc_to_flat(&req.verdicts.arc).into(),
        spam_score: req.spam_score,
        spam_disposition: disposition_str.to_string(),
        is_own_submission,
        scores: scores.clone(),
        report_hash: req.report_hash,
    };
    let insert_outcome = state
        .db
        .insert_inbound_mail(&state.mail_segments, &fields)
        .await
        .map_err(internal)?;
    let message_id = insert_outcome.message_id;
    let inserted = insert_outcome.inserted;
    // The nest's own receipt instant, in seconds — the same reading that
    // became this record's `MailFloorMetadata::received_at`. Every
    // arrival-time surface below keys on THIS, never on
    // `public_metadata.timestamp`: that field is the MTA's parse of the
    // sender's own RFC 5322 `Date:` header, which no sender is obliged to
    // fill in truthfully, so anything it orders, filters, or ages out is the
    // sender's to steer (`imap-server.md` § SEARCH → *INTERNALDATE is the
    // nest's own receipt time*). The header value stays on the record as the
    // floor's `timestamp`, for display and for anyone who genuinely wants the
    // sender's claim.
    let received_at_secs = insert_outcome.received_at / 1000;

    // T1.4 — record the perimeter content-scan verdict (deployment-data
    // plaintext, both storage modes; the scanned body never reaches the nest,
    // see content-scoring.md). The wire carries only the verdict fields; the
    // `action_taken` is derived here. Idempotent on retry (deterministic
    // message_id → INSERT OR REPLACE).
    //
    // A row exists only for a message that reached the scan pipeline
    // (mail-content-scanning.md § Per-message scan-result storage). A door that
    // never invokes the gate — the submission twin, the sender's own Sent copy
    // — sends `NotScanned` and no rspamd score, and gets no row: recording
    // `'clean'` there was a verdict nobody computed. `NotScanned` beside an
    // rspamd score (ClamAV disabled, rspamd on) keeps the row for rspamd's
    // detail record and says so in the column.
    let reached_scan_pipeline =
        !(matches!(req.clamav_verdict, ClamavVerdict::NotScanned) && req.rspamd_score.is_none());
    if reached_scan_pipeline {
        let (clamav_verdict, clamav_signature) = match &req.clamav_verdict {
            ClamavVerdict::Clean => ("clean", None),
            ClamavVerdict::Infected { signature } => ("infected", Some(signature.clone())),
            ClamavVerdict::Error { .. } => ("error", None),
            ClamavVerdict::BypassedOversize => ("bypassed_oversize", None),
            ClamavVerdict::NotScanned => ("not_scanned", None),
        };
        // Ingest only happens for non-reject actions; an infected message that
        // was nonetheless ingested was filed to Junk by the `junk` action (Go
        // routes it as SpamDisposition::PolicyJunk) or tagged (headers only). Clean /
        // oversize / not-scanned → delivered. (Reject-at-perimeter records its
        // forensic row via a separate report path — T3 — and never reaches
        // this handler.)
        let action_taken = match (&req.clamav_verdict, req.spam_disposition) {
            (ClamavVerdict::Infected { .. }, SpamDisposition::PolicyJunk) => "junked",
            (ClamavVerdict::Infected { .. }, _) => "tagged",
            _ => "delivered",
        };
        let (rspamd_score_raw, rspamd_score_scaled, rspamd_flagged_rules, rspamd_score_breakdown) =
            rspamd_row_fields(req.rspamd_score.as_ref());
        state
            .db
            .insert_scan_result(&ScanResultRow {
                message_id,
                // Both are nest-clock facts about this delivery — when we
                // received it and when the perimeter scanned it — and the
                // forensic row's retention prunes on `received_at`
                // (`prune_scan_results`), so sourcing either from the sender's
                // `Date:` header would let a sender expire their own scan
                // record on demand (backdated) or keep it forever
                // (future-dated).
                received_at: received_at_secs,
                scanned_at: received_at_secs,
                clamav_verdict: clamav_verdict.to_string(),
                clamav_signature,
                rspamd_score_raw,
                rspamd_score_scaled,
                rspamd_flagged_rules,
                rspamd_score_breakdown,
                action_taken: action_taken.to_string(),
                delivered_to_actor: Some(target),
            })
            .await
            .map_err(internal)?;
    }

    // The uniform bus rows (computed above, also carried in the segment
    // footer). Idempotent on retry, same as the scan-result row.
    state
        .db
        .insert_content_scores(
            &message_id,
            "mail",
            Some(&target),
            // `scored_at` is when the scoring ran — the perimeter pass that
            // produced these rows ran at delivery. Every other writer of this
            // column already uses the nest clock (`db/labelers.rs`,
            // `db/model_versions.rs`); this path was the one feeding it a
            // sender-supplied value, which also decided the row's place in the
            // `(actor_id, scored_at)` index.
            received_at_secs,
            &scores,
        )
        .await
        .map_err(internal)?;

    // Ingest-time report-aggregate join (report-sharing.md § The aggregate):
    // a late copy of content whose k-anonymized report aggregate is already
    // live gets its tier-3 `report:spam` row on arrival. The gated read is
    // the k-anonymity choke point; below k nothing is written. Best-effort —
    // never fails the ingest.
    if let Ok(hash) = <[u8; 32]>::try_from(fields.report_hash.as_slice()) {
        use fauna_core::scoring::{ScoreEntry, TIER_COMMUNITY, factor, scorer_version};
        match state
            .db
            .gated_report_score(&hash, factor::REPORT_SPAM)
            .await
        {
            Ok(Some(score)) => {
                if let Err(e) = state
                    .db
                    .insert_content_scores(
                        &message_id,
                        "mail",
                        Some(&target),
                        // Same `scored_at` column, same nest-clock rule as the
                        // bus rows above — this join runs at delivery too.
                        received_at_secs,
                        &[ScoreEntry {
                            factor: factor::REPORT_SPAM.to_string(),
                            score,
                            tier: TIER_COMMUNITY,
                            scorer_version: scorer_version::REPORT,
                        }],
                    )
                    .await
                {
                    tracing::warn!("report-aggregate ingest join failed (ingest succeeded): {e}");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("report-aggregate gate read failed (ingest succeeded): {e}"),
        }
    }

    // Plan 5 T6: emit `fauna.segments.changed { Finalized }` when this
    // append rotated a previously-open segment closed. The data
    // owner's custodian pull listens for this kind to wake its
    // 5-s debounce window and back up the just-finalized segment.
    if let Some(closed_seg_id) = insert_outcome.finalized {
        crate::segments::notify_segments_changed(
            &state.ws,
            &target,
            "mail",
            closed_seg_id,
            fauna_protocol::push_events::SegmentChange::Finalized,
        );
    }
    // Bootstrap the six standard IMAP mailboxes if this is the actor's
    // first delivery; emit one `MailPlacementRecord::Create` per
    // newly-seeded mailbox BEFORE the `Append` below, so the manifest
    // has the destination mailbox row by the time the Append record
    // applies (spec § D2 record-table ordering invariant; spec § D6
    // (ε) atomic-with-SQL note — same crash-window deferral to Plan 2
    // T9's divergence detection as T7's APPEND/STORE/EXPUNGE wiring).
    let newly_seeded = state
        .db
        .ensure_bridge_imap_mailboxes(&target)
        .await
        .map_err(internal)?;
    for seeded in newly_seeded {
        let record = MailPlacementRecord::Create {
            mailbox: seeded.name,
            uid_validity: seeded.uid_validity,
            attrs: seeded.attrs,
        };
        state
            .mail_placement
            .append_event(&target, &record)
            .await
            .map_err(internal)?;
    }
    // Filter-rule placement override (T3.3, `smtp-server.md` § Email filter
    // rules). The Go MTA perimeter evaluated the recipient's stored filter rules
    // (`fauna_mail::filter`) against the plaintext envelope / headers / spam-score
    // and may have resolved a `FileInto`/`Allow` target mailbox (`target_mailbox`)
    // and/or `AddLabel` keywords (`extra_flags`). nest does NOT evaluate filters —
    // it only applies the perimeter's verdict to *placement*, leaving the recorded
    // `spam_disposition` (above) truthful. Own-submission (the Sent copy) is never
    // filtered.
    let filter_extra_flags = req.extra_flags;
    let mut filter_target_mailbox = req.target_mailbox;
    // A custom (non-standard) FileInto folder must exist — with its own `Create`
    // placement record — before the `Append` below, so the manifest stays
    // consistent (spec § D2 ordering; same shape as the standard-mailbox bootstrap
    // above and the IMAP `CREATE` handler). The six standard mailboxes are already
    // seeded by `ensure_bridge_imap_mailboxes`. A malformed target name, or one
    // inbound mail must never enter (`validate_file_into_mailbox`: `Sent` and
    // `Drafts` hold only this account's own writing, and a `Sent` record reads as
    // the user's own send; the guardian's held mailbox holds only holds), falls
    // back to disposition placement rather than failing an already-accepted SMTP
    // transaction — the perimeter must never trust the rule store to keep delivery
    // safe. Create-time validation refuses both, but a rule stored before it, or
    // carried across a succession, still arrives here.
    // A held message never consults the recipient's filter rules: the guardian's
    // routing floor outranks a rule the ward wrote, or the message would be one
    // `FileInto INBOX` rule away from escaping the hold.
    if !is_own_submission
        && mail_verdict != MailVerdict::Hold
        && let Some(name) = filter_target_mailbox.as_deref()
    {
        if fauna_protocol::email::validate_file_into_mailbox(name).is_err()
            || validate_mailbox_name(name).is_err()
        {
            filter_target_mailbox = None;
        } else if !is_reserved_mailbox(name)
            && let CreateMailboxDbOutcome::Created {
                uid_validity: created_uid_validity,
            } = state
                .db
                .create_bridge_imap_mailbox(
                    &target,
                    name,
                    crate::bridge_imap_handlers::fresh_uid_validity(),
                )
                .await
                .map_err(internal)?
        {
            let record = MailPlacementRecord::Create {
                mailbox: name.to_string(),
                uid_validity: created_uid_validity,
                attrs: Vec::new(),
            };
            state
                .mail_placement
                .append_event(&target, &record)
                .await
                .map_err(internal)?;
        }
    }
    let (mailbox, initial_flags): (String, String) = if is_own_submission {
        ("Sent".to_string(), "\\Seen".to_string())
    } else if mail_verdict == MailVerdict::Hold {
        // Never `Junk` — the ward must be able to tell "your guardian is
        // reviewing this" from "this looked like spam", and the MDA's
        // SELECT-time spam scorer re-files INBOX→Junk independently and would
        // race a release. Unread, and the spam disposition recorded above stays
        // truthful: this is a *placement* decision layered over it.
        (GUARDIAN_HELD_MAILBOX.to_string(), String::new())
    } else {
        let mb = match filter_target_mailbox {
            Some(name) => name,
            None => match req.spam_disposition {
                SpamDisposition::Accept => "INBOX",
                SpamDisposition::AcceptToSpamFolder => "Junk",
                SpamDisposition::PolicyJunk => "Junk",
            }
            .to_string(),
        };
        // Defense-in-depth (EF-1): the perimeter must never trust the rule store
        // to keep delivery safe (see the validate_mailbox_name fallback above).
        // Each `AddLabel` keyword is validated at create time
        // (`email_handlers::validate_action`), but a pre-existing or hand-edited
        // rule could still carry a system flag (`\Deleted` → EXPUNGE-eligible,
        // `\Seen` → silently read) or a whitespace value that splits into several
        // flags; drop any token that is not a single RFC 3501 keyword `atom`
        // before it becomes an IMAP flag on the placed message.
        let safe_flags = filter_extra_flags
            .iter()
            .filter(|l| fauna_protocol::email::validate_label(l).is_ok())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        (mb, safe_flags)
    };
    // `sender_domain` populates `bridge_imap_messages.from_norm` for the
    // SEARCH header axis (imap-server.md § SEARCH).  In encrypted mode
    // this is the only `*_norm` column the MTA can populate — to/cc/
    // subject_norm remain empty and degrade to no-match per § SEARCH.
    // Read off `fields` (req has been moved into it above).
    let placement_outcome = if mail_verdict == MailVerdict::Hold {
        // The placement and its `guardian_mail_holds` envelope sidecar commit
        // together — the message's presence in the held mailbox IS the hold, so
        // neither may exist without the other.
        let sender_address = match ingress {
            MailIngress::Sender(addr) => normalize_mail_address(addr),
            // A held null-path message truthfully records no sender — the
            // guardian's queue shows the hold with an empty address.
            MailIngress::NullReversePath { .. } => String::new(),
            // Unreachable: `guardian_mail_verdict` returns `Deliver` for
            // `System`, so a `Hold` never comes from it.
            MailIngress::System => String::new(),
        };
        state
            .db
            .place_held_inbound_mail(
                &target,
                &message_id,
                received_at_secs,
                &fields.sender_domain,
                &sender_address,
                inserted,
            )
            .await
            .map_err(internal)?
    } else {
        state
            .db
            .place_inbound_mail(
                &target,
                &message_id,
                &mailbox,
                received_at_secs,
                &initial_flags,
                &fields.sender_domain,
                inserted,
            )
            .await
            .map_err(internal)?
    };

    // Spec § D6 (ε): placement-journal append for the MTA ingest path
    // (`ingest_inbound_mail` for external-MX inbound, `submit_inbound_mail`
    // for authenticated own-submission). The SQLite UID/modseq allocation +
    // INSERT already committed inside `place_inbound_mail` above; the
    // placement append below happens after that commit. The crash window
    // between the two is closed by Plan 2 T9's divergence detection at
    // SELECT / QRESYNC / sync-collection time.
    //
    // `placement_outcome` is `None` when this is an idempotent-retry of an
    // inbound the MTA already delivered (same `message_id` already placed,
    // via `content_was_new = false`). The original ingest already emitted
    // its `Append` record, so we must NOT emit a duplicate one here.
    if let Some((uid, modseq)) = placement_outcome {
        let flags: Vec<String> = initial_flags.split_whitespace().map(String::from).collect();
        let record = MailPlacementRecord::Append {
            mailbox: mailbox.clone(),
            uid,
            modseq: modseq as u64,
            flags,
            content_record_id: message_id.to_vec(),
            // Must be the same instant the SQL row above took: the journal is
            // the authoritative rebuild source, so a divergence here would
            // silently rewrite every replayed message's INTERNALDATE.
            internal_date: received_at_secs,
        };
        state
            .mail_placement
            .append_event(&target, &record)
            .await
            .map_err(internal)?;

        // Per-record arrival push (`fauna.mail.received`): the genuinely-new
        // placement is now both segment-stored AND visible to the recipient's
        // INBOX-scoped `fauna.email.inbox.fetch` (which joins INBOX placement
        // rows → segment envelopes), so the client can fetch immediately
        // without the up-to-30 s poll. Emitted here (after the placement
        // append, gated by `placement_outcome.is_some()`) rather than after
        // the segment insert above so the placement row exists when the
        // client reacts — closing the push→fetch-finds-nothing race. Also
        // wakes the data owner's custodian pull. Best-effort; the
        // periodic poll is the correctness backstop.
        crate::segments::notify_mail_received(&state.ws, &target);

        // S5 Arm 1 (design spec D5; `content-scoring.md` § Timing → *Delivery-
        // time fast path*): a mail delivered AFTER the owner subscribed a per-
        // user community-labeler scorer owes drain work the subscribe-time
        // backlog seed never covered. Create that obligation now — one
        // behind-version-0 `labeler:<hex>` placeholder per subscribed WASM/mail
        // labeler — and, only if any was seeded, nudge the co-resident MDA /
        // content-processor holder to drain it moments after delivery (else it
        // waits for startup / config_changed / the 12 h backstop). No subscribed
        // labeler → 0 rows → no push. Best-effort; the drain's periodic triggers
        // are the correctness backstop.
        let rescore_obligations = state
            .db
            .seed_new_mail_labeler_obligations(&message_id, &target)
            .await
            .map_err(internal)?;
        if rescore_obligations > 0 {
            notify_bridges_rescore_ready(&state).await;
        }
    }

    // `mailbox-migration.md` § Dedup key persistence: every delivery populates
    // the index, so a later import of the same account from a foreign IMAP
    // server dedup-hits the copy already delivered here.
    //
    // Record-only. A dedup hit must NEVER suppress a delivery — a legitimate
    // resend, or a second copy the user is `Cc:`'d on, would vanish. Only
    // `import_message` skips on a hit. `insert_dedup_key` is INSERT OR IGNORE,
    // so an idempotent MTA retry of the same `message_id` is a no-op.
    //
    // Covers every caller: external-MX `ingest_inbound_mail`, authenticated
    // own-submission `submit_inbound_mail` (the Sent copy is the user's mail
    // too, and re-importing it should dedup), and the nest's own in-domain
    // delivery, which mints the pair from the plaintext it seals.
    //
    // The envelope key rides beside it because the Message-ID is the SENDER's
    // choice: a stranger's delivery reusing a Message-ID from a mailbox the
    // user has not imported yet writes this row first, and only a disagreeing
    // envelope key stops it from making the real message skip at import
    // (`mailbox-migration.md` § The envelope key confirms a Message-ID hit).
    state
        .db
        .insert_dedup_key(
            &target,
            &req_dedup_key,
            &req_envelope_key,
            &hex::encode(message_id),
        )
        .await
        .map_err(internal)?;

    Ok(message_id)
}

fn ingest_inbound_mail_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let reply = persist_inbound_mail_request(
                state.clone(),
                actor_id,
                payload,
                "fauna.bridges.ingest_inbound_mail",
                false,
            )
            .await?;
            // The mail health readout's "last received" heartbeat
            // (`mail-deliverability.md` § The mail health readout). Best-effort:
            // a stamp failure never fails an accepted message.
            if let Err(e) = state.db.stamp_inbound_accepted(now_epoch_secs()).await {
                tracing::warn!(target: "mail_deliverability", "inbound heartbeat stamp failed: {e}");
            }
            Ok(reply)
        })
    })
}

fn submit_inbound_mail_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(persist_inbound_mail_request(
            state,
            actor_id,
            payload,
            "fauna.bridges.submit_inbound_mail",
            true,
        ))
    })
}

// ── I4 Phase D.5 outbound queue handlers ───────────────────────
//
// Thin wrappers over the `db/outbound.rs` helpers. The MTA bridge
// drives the worker loop on the Go side; nest's role is to hand out
// due rows + record terminal status. Catalog ceiling for batch size
// and retry-backoff ride here so the bridge can't smuggle past them.

/// Hard ceiling on the per-poll batch size. The bridge's worker is one
/// goroutine; bigger batches just queue up CPU work on the bridge
/// without buying nest any extra concurrency. Keep small — extra polls
/// are cheap on an authenticated WS-RPC connection.
const FETCH_OUTBOUND_DUE_MAX_BATCH: u32 = 64;

/// Hard ceiling on `retry_after_seconds` accepted from the bridge.
/// 24 hours matches the longest step in goal doc
/// `docs/goal/behavior/smtp-server.md` § Outbound delivery's retry
/// schedule (the post-12h plateau); longer values would let a
/// misbehaving bridge stall retries indefinitely. Nest clamps
/// silently.
const MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER: u32 = 86_400;

fn fetch_outbound_due_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_outbound_due").await?;
            let req: FetchOutboundDueRequest = decode(&payload).map_err(malformed)?;
            if req.max == 0 {
                return Err(malformed("max must be > 0"));
            }
            if req.lease_seconds == 0 {
                return Err(malformed("lease_seconds must be > 0"));
            }
            let limit = req.max.min(FETCH_OUTBOUND_DUE_MAX_BATCH);
            let now = state.outbound_now();
            // N5: promote any parked forwards whose actor is back under the
            // hourly rate cap into the outbound queue, so this poll picks them
            // up. Nest-internal (no separate timer / RPC); a promotion failure
            // must not block normal outbound dispatch.
            if let Err(e) = promote_due_forwards(&state).await {
                tracing::warn!(
                    target: "mail_forward",
                    error = %e,
                    "forward-queue promotion failed"
                );
            }
            let rows = state
                .db
                .fetch_due_outbound(now, limit)
                .await
                .map_err(internal)?;
            // SRS-rewrite forwarded rows' envelope MAIL FROM at queue-out,
            // nest-side, so the per-deployment SRS secret never leaves nest
            // (`mail-forwarding.md` § SRS on outbound). Fetched once per batch;
            // normal (non-forwarded) rows are untouched.
            let srs_secret = state.db.get_active_srs_secret().await.map_err(internal)?;
            let primary_domain = state
                .db
                .lookup_primary_mail_domain()
                .await
                .map_err(internal)?
                .map(|d| d.domain_name);
            let now_day = (now.max(0) as u64) / 86_400;
            // Reply byte budget (smtp-server.md § Message size limits, the
            // staged-envelope rule; transport.md § Max frame corollary). A body
            // over the inline reply budget is sealed under a one-shot AEAD key
            // and STAGED on the byte plane (S9.3) — the unit then carries only a
            // tiny reference, so the historic wedge (two ~1.2 MiB rows summing
            // past the frame, or one over-frame row that could never be served)
            // is gone: the reply always fits and a body up to the product ceiling
            // delivers. The budget still bounds the reply for the INLINE rows —
            // rows are independent (a due-set poll, not a cursor walk), so the
            // remainder rides the next poll.
            const OUTBOUND_REPLY_BUDGET_BYTES: usize =
                fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE - 16 * 1024;
            // Covers msgid + addresses (SRS-rewritten sender included) + CBOR
            // framing per unit, generously — a conservative page cut, never an
            // over-frame reply.
            const OUTBOUND_UNIT_OVERHEAD_BYTES: usize = 4096;
            let inline_budget =
                fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize;
            // THE DKIM SIGN SITE (`mail-bridge-lifecycle.md` § DKIM provisioning
            // (automatic) → *Custody moves to the nest*): every outbound
            // message, from every door, leaves through this hand-out, so it is
            // signed here and nowhere else in the nest. The keys are looked up
            // once per batch — never minted — and dropped with it. A lookup
            // failure signs nothing and ships the batch: signing never holds
            // the queue.
            let signer = if rows.is_empty() {
                None
            } else {
                match crate::mail_dkim_key::OutboundSigner::load(&*state.db.conn().await) {
                    Ok(signer) => Some(signer),
                    Err(e) => {
                        tracing::error!(
                            target: "mail_dkim",
                            "DKIM key lookup failed; this batch leaves unsigned: {e:#}"
                        );
                        None
                    }
                }
            };
            let mut units = Vec::new();
            let mut used = 0usize;
            for mut r in rows {
                // Signed afresh at each hand-out (the row rests unsigned), so a
                // retry after a selector flip carries the new selector; and
                // signed BEFORE the staging decision, so a staged body is the
                // signed bytes. A unit the nest does not sign leaves as it
                // rests; nothing downstream signs it.
                if let Some(signed) = signer.as_ref().and_then(|s| s.sign(&r.raw_message)) {
                    r.raw_message = signed;
                }
                let original_sender = srs_rewrite_outbound_sender(
                    srs_secret.as_deref(),
                    primary_domain.as_deref(),
                    now_day,
                    &r,
                );
                // Over the inline reply budget ⇒ seal + stage on the byte plane
                // and serve a reference; the worker GETs the chunks over the open
                // route and opens the AEAD. Re-staging per serve is stateless —
                // a fresh key each time, so the superseded chunks are disposable
                // GC orphans (no cross-serve dedup, by design: the fresh key
                // rules out a blake3-of-plaintext correlation channel). At or
                // under the budget the body rides inline exactly as before.
                let over_inline = r.raw_message.len() > inline_budget;
                let staged_body = if over_inline {
                    match crate::mail_body_plane::stage_staged_body(&state, &r.raw_message).await {
                        Ok(sref) => Some(sref),
                        Err(e) => {
                            // A nest-local staging failure (blob-store write
                            // error / full disk) is transient infrastructure, not
                            // a per-message condition — skip the row this poll and
                            // let it ride the next, never permfail deliverable
                            // mail or wedge the batch.
                            tracing::error!(
                                id = r.id,
                                error = ?e,
                                "failed to stage over-frame outbound body; \
                                 will retry on the next poll"
                            );
                            continue;
                        }
                    }
                } else {
                    None
                };
                // Wire cost of the unit as it will be sent: a staged unit carries
                // only the reference (per-hash + the 32-byte key + framing); an
                // inline unit carries its raw_message.
                let ref_cost = staged_body
                    .as_ref()
                    .map(|s| s.chunk_hashes.len() * 34 + 64)
                    .unwrap_or(0);
                let inline_cost = if over_inline { 0 } else { r.raw_message.len() };
                let cost = inline_cost + ref_cost + OUTBOUND_UNIT_OVERHEAD_BYTES;
                if cost > OUTBOUND_REPLY_BUDGET_BYTES {
                    // Unreachable now that over-frame bodies stage (inline rows
                    // are ≤ the inline budget, a staged reference is tiny, and the
                    // enqueue admission caps at the product ceiling) — retained
                    // belt-and-braces: a row whose served form still cannot fit a
                    // reply fails with a permanent DSN rather than wedging the
                    // poll. `r` is intact here (not yet moved).
                    tracing::error!(
                        id = r.id,
                        raw_bytes = r.raw_message.len(),
                        staged = over_inline,
                        "outbound row cannot fit a reply even after staging; \
                         failing it with a permanent DSN instead of wedging the poll"
                    );
                    if let Err(e) = crate::outbound_bounce::generate_permfail_bounce(
                        &state,
                        &r,
                        "message too large to hand to the outbound dispatcher",
                        now,
                    )
                    .await
                    {
                        tracing::error!(
                            id = r.id,
                            error = %e,
                            "permfail NDR generation failed; marking bounced without a DSN"
                        );
                        state
                            .db
                            .mark_outbound_bounced_with_reason(
                                r.id,
                                "over the outbound dispatch frame budget",
                            )
                            .await
                            .map_err(internal)?;
                    }
                    continue;
                }
                if used + cost > OUTBOUND_REPLY_BUDGET_BYTES {
                    // Page closes early; the remaining due rows ride the next
                    // poll (the bridge polls on a short interval and the Data
                    // hook nudges it).
                    break;
                }
                used += cost;
                let raw_message = if over_inline {
                    Vec::new()
                } else {
                    r.raw_message
                };
                units.push(OutboundUnit {
                    id: r.id,
                    message_id: r.original_msgid,
                    original_sender,
                    recipient: r.recipient,
                    raw_message,
                    attempt_count: r.attempt_count,
                    staged_body,
                });
            }
            encode_reply(&FetchOutboundDueReply { units })
        })
    })
}

/// Draw one unit of `forwarder`'s daily recipients allowance for a forward
/// about to dispatch; `false` (nothing drawn) when today's allowance is spent.
///
/// A forward is the forwarding actor's outbound, so it consumes the same
/// nest-side recipients/day counter both submission doors debit
/// (`smtp-server.md` § Architectural rules), composed with the hourly forward
/// cap (`mail-forwarding.md` § Architectural rules — "a forward that would
/// exceed either gets queued"). A forward always leaves through the outbound
/// queue, so it is a remote recipient under the one charging rule and costs
/// exactly one. Both dispatch points call this — `forward_message`'s under-cap
/// arm and [`promote_due_forwards`] — and a forward it refuses is parked, never
/// dropped, so a later poll promotes it once a new day opens the allowance.
/// The draw lands before the outbound row is written: a failed enqueue costs
/// the unit, never the reverse (a dispatch the counter did not see).
async fn draw_forward_daily_unit(
    state: &Arc<AppState>,
    forwarder: &[u8; 32],
    max_per_day: u32,
) -> anyhow::Result<bool> {
    let day_bucket = now_epoch_secs() / 86_400;
    let outcome = state
        .db
        .try_consume_submission_quota(forwarder, day_bucket, 1, max_per_day)
        .await?;
    Ok(matches!(outcome, SubmissionQuotaOutcome::Allowed))
}

/// Promote parked forwards (`forward_queue`) into `outbound_mail_queue` up to
/// each actor's remaining hourly rate allowance, and no further than the day's
/// recipients allowance covers ([`draw_forward_daily_unit`]) (`mail-forwarding.md` § Queue
/// ceiling — "pops happen ... at the rate-cap cadence" `:185`). Folded into the
/// MTA's `fetch_outbound_due` poll so there is no separate nest timer and no
/// second dispatch path: a promoted forward becomes an ordinary `is_forwarded`
/// outbound row, SRS-rewritten at queue-out exactly like a directly-dispatched
/// one (the shipped N2 decision — forwards dispatch via the unified outbound
/// queue). This realizes the doc's `fetch_outbound_forwards_due` ("nest applies
/// the per-actor rate-cap window before returning") as an in-`fetch_outbound_due`
/// step. `allowance = min(cap, ceiling) - window`; promoting exactly that many
/// oldest rows refills the window to the cap, so the next poll promotes the
/// next batch once the earlier ones age out of the hour.
async fn promote_due_forwards(state: &Arc<AppState>) -> anyhow::Result<()> {
    let actors = state.db.distinct_forward_queue_actors().await?;
    if actors.is_empty() {
        return Ok(());
    }
    let max_per_day = state
        .db
        .get_submission_policy()
        .await?
        .effective()
        .max_per_day;
    for actor in actors {
        let per_account = state.db.get_forward_per_hour(&actor).await?;
        let cap = per_account.min(fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING);
        let window = state
            .db
            .count_forward_dispatched_window(&actor, 3600)
            .await?;
        let allowance = cap.saturating_sub(window);
        if allowance == 0 {
            continue;
        }
        let parked = state
            .db
            .fetch_forward_queue_oldest(&actor, allowance)
            .await?;
        for row in parked {
            // The day's allowance is spent → the rest stay parked, oldest
            // first, until a later day's poll.
            if !draw_forward_daily_unit(state, &actor, max_per_day).await? {
                break;
            }
            // One outbound row per parked forward (single destination); the
            // SRS rewrite happens at queue-out keyed on this new row's id.
            state
                .db
                .enqueue_outbound(NewOutbound {
                    original_msgid: &row.source_message_id,
                    original_sender: &row.original_sender,
                    recipients: &[row.destination_address.as_str()],
                    raw_message: &row.raw_message,
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: true,
                    forward_actor_id: Some(&actor),
                    forward_rule_id: Some(&row.rule_id_or_forward_all),
                    // Carried across promotion as-is: an unknown (unrecognised or NULL)
                    // mode stays unknown rather than being guessed `copy`.
                    forward_copy_mode: row.copy_mode,
                    submit_actor_id: None,
                })
                .await?;
            state.db.delete_forward_queue(row.id).await?;
        }
    }
    Ok(())
}

/// Rewrite a forwarded outbound row's envelope MAIL FROM under SRS for
/// queue-out dispatch (`mail-forwarding.md` § SRS on outbound). Done
/// **nest-side** here, not in the Go bridge, so the per-deployment SRS secret
/// never leaves nest — the bridge dispatches the envelope nest hands back in
/// the [`OutboundUnit`]. The stored row keeps its *original* sender (this only
/// rewrites the returned unit), so a retry re-fetch re-derives the address
/// cleanly rather than double-rewriting.
///
/// Non-forwarded rows pass through unchanged. A forwarded row in a deployment
/// missing the SRS secret or a primary mail domain also passes through (it
/// will SPF-fail at the downstream MX — a misconfiguration — but we never drop
/// the row).
///
/// The SRS payload's forwarder "short-id" (`mail-forwarding.md:115`) is the
/// **outbound row id**: bounded, so the rewritten local-part stays within the
/// RFC 5321 limit, and looked up by `decode_srs_bounce` to recover the real
/// forwarder actor and the bounced destination. The HMAC covers it, so it
/// cannot be swapped to misroute a bounce.
fn srs_rewrite_outbound_sender(
    secret: Option<&[u8]>,
    primary_domain: Option<&str>,
    now_day: u64,
    row: &OutboundRow,
) -> String {
    if !(row.is_forwarded && row.forward_actor_id.is_some()) {
        return row.original_sender.clone();
    }
    let (Some(secret), Some(domain)) = (secret, primary_domain) else {
        tracing::warn!(
            id = row.id,
            "forwarded outbound row but no SRS secret / primary mail domain; \
             dispatching the original envelope (will SPF-fail at the downstream MX)"
        );
        return row.original_sender.clone();
    };
    match fauna_mail::srs::srs_forward(
        secret,
        domain,
        now_day,
        &row.id.to_string(),
        &row.original_sender,
    ) {
        Ok(rewritten) => rewritten,
        Err(e) => {
            tracing::warn!(
                id = row.id,
                error = %e,
                "SRS rewrite failed; dispatching the original envelope"
            );
            row.original_sender.clone()
        }
    }
}

/// Map a shared [`MtaStsLookup`] onto the wire [`FetchMtaStsPolicyReply`].
///
/// Factored out of the handler so the four-outcome mapping is unit-testable
/// without standing up a full `AppState`. Both vocabularies are asked of their
/// owner in `fauna-mail` — `MtaStsOutcome::as_wire` for the RFC 8461 §5 /
/// RFC 8460 §4.3 lookup outcome the Go bridge tells apart for enforcement +
/// (T2.4) TLSRPT attribution, and `MtaStsMode::as_str` for the RFC 8461 §3.2
/// mode. Until 2026-08-23 both were hand-written here (and hand-written again,
/// inverted, in `mta_sts_lookup_from_wire` below, which reads the same tokens
/// back when the bridge echoes them on `report_tls_attempt`); the mode copy
/// carried a comment saying `as_str` was private, which it no longer is.
fn lookup_to_reply(lookup: fauna_mail::outbound::mta_sts::MtaStsLookup) -> FetchMtaStsPolicyReply {
    use fauna_mail::outbound::mta_sts::MtaStsLookup;
    let outcome = lookup.outcome().as_wire().to_string();
    match lookup {
        MtaStsLookup::NotPublished | MtaStsLookup::FetchError | MtaStsLookup::Invalid => {
            FetchMtaStsPolicyReply {
                outcome,
                policy: None,
            }
        }
        MtaStsLookup::Found(fp) => FetchMtaStsPolicyReply {
            outcome,
            policy: Some(MtaStsPolicyWire {
                id: fp.id,
                mode: fp.policy.mode.as_str().into(),
                mx: fp.policy.mx,
                max_age_secs: fp.policy.max_age_secs,
            }),
        },
    }
}

/// `fauna.bridges.fetch_mta_sts_policy` (T2.1a). nest performs the MTA-STS
/// network fetch (`_mta-sts.<domain>` TXT + `.well-known/mta-sts.txt`) behind
/// a 24 h cache (the shared `CachingMtaStsFetcher`) and returns the parsed
/// policy so the Go MTA bridge can apply the per-host enforce/testing
/// decision locally (`docs/goal/behavior/smtp-server.md` § MX resolution).
/// MTA-only.
fn fetch_mta_sts_policy_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_mta_sts_policy").await?;
            let req: FetchMtaStsPolicyRequest = decode(&payload).map_err(malformed)?;
            if req.domain.is_empty() {
                return Err(malformed("domain must be non-empty"));
            }
            // A scripted test override (test-hooks builds only) short-circuits
            // the real network fetch so a tier_3 e2e can drive each outcome.
            #[cfg(feature = "test-hooks")]
            if let Some(scripted) = state.test_mta_sts_override(&req.domain) {
                return encode_reply(&lookup_to_reply(scripted));
            }
            let lookup = state
                .mta_sts_fetcher
                .lookup(&req.domain)
                .await
                .map_err(internal)?;
            encode_reply(&lookup_to_reply(lookup))
        })
    })
}

/// Map lifted [`fauna_mail::outbound::dane::TlsaRecord`]s onto the wire
/// [`FetchTlsaReply`], keeping **only** SMTP-usable records (DANE-TA /
/// DANE-EE — RFC 7672 §3.1). PKIX-TA/EE (usage 0/1) are dropped here so the
/// bridge can treat a non-empty `records` list as "DANE applies to this
/// host" with a single matcher call: a host publishing only PKIX-class TLSA
/// records becomes an empty reply (→ MTA-STS / opportunistic fallback)
/// rather than an undeliverable hard-fail. Factored out so the filter is
/// unit-testable without an `AppState`.
fn tlsa_to_reply(records: Vec<fauna_mail::outbound::dane::TlsaRecord>) -> FetchTlsaReply {
    let records = records
        .into_iter()
        .filter(|r| r.is_smtp_dane())
        .map(|r| TlsaRecordWire {
            usage: r.usage,
            selector: r.selector,
            matching: r.matching,
            data: r.data,
        })
        .collect();
    FetchTlsaReply { records }
}

/// `fauna.bridges.fetch_tlsa` (T2.1b). nest performs the
/// `_25._tcp.<mx_host>` DNSSEC-validating TLSA lookup so the Go MTA bridge
/// (which can't do DNSSEC in the Go stdlib) can pin the outbound TLS
/// handshake against the published records via the `dane_chain_matches`
/// UniFFI decision (`docs/goal/behavior/smtp-server.md` § Architectural
/// rules). MTA-only.
fn fetch_tlsa_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_tlsa").await?;
            let req: FetchTlsaRequest = decode(&payload).map_err(malformed)?;
            if req.mx_host.is_empty() {
                return Err(malformed("mx_host must be non-empty"));
            }
            // A scripted test override (test-hooks builds only) short-circuits
            // the real DNSSEC lookup so a tier_3 e2e can drive pin match /
            // mismatch / no-DANE without a real DNSSEC-signed zone.
            #[cfg(feature = "test-hooks")]
            if let Some(scripted) = state.test_tlsa_override(&req.mx_host) {
                return encode_reply(&tlsa_to_reply(scripted));
            }
            let records = state
                .tlsa_resolver
                .lookup(&req.mx_host)
                .await
                .map_err(internal)?;
            encode_reply(&tlsa_to_reply(records))
        })
    })
}

/// `fauna.bridges.resolve_mx` — the MX leg of the same nest/bridge split
/// `fetch_tlsa` sits on. nest resolves the recipient domain's MX RRset with
/// DNSSEC validation and reports the hosts **plus** whether that RRset
/// proved `Secure`, because outbound DANE may only bind to a name that came
/// out of a validated RRset (RFC 7672 §2.2). Without the provenance bit the
/// bridge's TLSA-side DNSSEC validation authenticates whatever name a
/// DNS-spoofing attacker put in the MX answer
/// (`docs/goal/behavior/smtp-server.md` § Architectural rules, outbound
/// DANE). MTA-only.
fn resolve_mx_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.resolve_mx").await?;
            let req: ResolveMxRequest = decode(&payload).map_err(malformed)?;
            if req.domain.is_empty() {
                return Err(malformed("domain must be non-empty"));
            }
            let answer = state
                .mx_resolver
                .lookup(&req.domain)
                .await
                .map_err(internal)?;
            encode_reply(&ResolveMxReply {
                hosts: answer
                    .hosts
                    .into_iter()
                    .map(|(priority, hostname)| MxHostWire { priority, hostname })
                    .collect(),
                secure: answer.secure,
            })
        })
    })
}

fn mark_outbound_delivered_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.mark_outbound_delivered").await?;
            let req: MarkOutboundDeliveredRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .mark_outbound_sent(req.id)
                .await
                .map_err(internal)?;
            // The mail health readout's "last delivered" heartbeat
            // (`mail-deliverability.md` § The mail health readout). Best-effort:
            // a stamp failure never fails a delivery report.
            if let Err(e) = state.db.stamp_outbound_delivered(now_epoch_secs()).await {
                tracing::warn!(target: "mail_deliverability", "outbound heartbeat stamp failed: {e}");
            }
            encode_reply(&MarkOutboundDeliveredReply { ok: true })
        })
    })
}

fn mark_outbound_failed_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.mark_outbound_failed").await?;
            let req: MarkOutboundFailedRequest = decode(&payload).map_err(malformed)?;
            let now = state.outbound_now();
            // nest owns the retry curve — `smtp-server.md` § Outbound
            // delivery: "nest reschedules per the retry schedule below".
            // The bridge reports a *temporary* failure; nest applies the
            // shared `RetryPolicy`, emits the once-per-message 4 h delay-
            // warning DSN, and on give-up promotes the row to a permanent-
            // failure bounce (the same NDR path as a permanent 5xx, T1.2).
            // A missing row (raced with a terminal mark) is a no-op.
            let row = match state
                .db
                .fetch_outbound_by_id(req.id)
                .await
                .map_err(internal)?
            {
                Some(r) => r,
                None => {
                    tracing::warn!(id = req.id, "mark_outbound_failed: row not found");
                    return encode_reply(&MarkOutboundFailedReply { ok: true });
                }
            };
            // The bridge no longer computes the backoff curve; `retry_
            // after_seconds` is a server Retry-After *hint* (0 = none),
            // honoured as a floor under the curve delay (clamped).
            let hint = req
                .retry_after_seconds
                .min(MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER) as i64;
            let outbound = state.db.get_outbound_policy().await.map_err(internal)?;
            let policy = crate::outbound_retry::retry_policy_from_outbound(&outbound.effective());
            match crate::outbound_retry::schedule_after_failure(&row, hint, now, &policy) {
                crate::outbound_retry::FailedDecision::GiveUp => {
                    if let Err(e) = crate::outbound_bounce::generate_permfail_bounce(
                        &state,
                        &row,
                        &req.last_error,
                        now,
                    )
                    .await
                    {
                        tracing::error!(
                            id = req.id,
                            error = %e,
                            "permfail NDR generation failed on give-up; marking bounced without a DSN"
                        );
                        state
                            .db
                            .mark_outbound_bounced_with_reason(req.id, &req.last_error)
                            .await
                            .map_err(internal)?;
                    }
                }
                crate::outbound_retry::FailedDecision::Retry {
                    next_attempt_at,
                    warn,
                } => {
                    if warn {
                        // Mark `delay_warned_at` whenever the warning was
                        // emitted *or* deliberately suppressed (backscatter)
                        // — both mean "don't re-warn this message". Only a
                        // transient enqueue error leaves it unset so the
                        // warning re-tries on the next attempt.
                        match crate::outbound_retry::enqueue_delay_warning(
                            &state.db,
                            &row,
                            &req.last_error,
                            now,
                        )
                        .await
                        {
                            Ok(_) => {
                                state
                                    .db
                                    .mark_outbound_delay_warned(req.id, now)
                                    .await
                                    .map_err(internal)?;
                            }
                            Err(e) => tracing::error!(
                                id = req.id,
                                error = %e,
                                "delay-warning enqueue failed; will retry warning next attempt"
                            ),
                        }
                    }
                    state
                        .db
                        .mark_outbound_attempt(
                            req.id,
                            next_attempt_at,
                            Some(req.last_error.as_str()),
                            None,
                        )
                        .await
                        .map_err(internal)?;
                }
            }
            encode_reply(&MarkOutboundFailedReply { ok: true })
        })
    })
}

fn mark_outbound_bounced_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.mark_outbound_bounced").await?;
            let req: MarkOutboundBouncedRequest = decode(&payload).map_err(malformed)?;
            let now = state.outbound_now();
            // Generate the RFC 3464 NDR (backscatter → NDR rate-limit →
            // DSN build → enqueue null-sender back to the sender) and own
            // the terminal status transition — `smtp-server.md`
            // §§ Permanent-failure bounce / Backscatter / NDR rate-limit.
            // Falls back to a status-only `bounced` mark if the row is gone
            // or NDR generation errors, so the row is always terminal and
            // the bridge never re-attempts an already-given-up send.
            match state.db.fetch_outbound_by_id(req.id).await {
                Ok(Some(row)) => {
                    if let Err(e) = crate::outbound_bounce::generate_permfail_bounce(
                        &state,
                        &row,
                        &req.reason,
                        now,
                    )
                    .await
                    {
                        tracing::error!(
                            id = req.id,
                            error = %e,
                            "permfail NDR generation failed; marking bounced without a DSN"
                        );
                        state
                            .db
                            .mark_outbound_bounced_with_reason(req.id, &req.reason)
                            .await
                            .map_err(internal)?;
                    }
                }
                Ok(None) => {
                    tracing::warn!(id = req.id, "mark_outbound_bounced: row not found");
                }
                Err(e) => {
                    tracing::error!(id = req.id, error = %e, "fetch_outbound_by_id failed");
                    state
                        .db
                        .mark_outbound_bounced_with_reason(req.id, &req.reason)
                        .await
                        .map_err(internal)?;
                }
            }
            encode_reply(&MarkOutboundBouncedReply { ok: true })
        })
    })
}

/// Rebuild the [`fauna_mail::outbound::mta_sts::MtaStsLookup`] the bridge saw
/// from the echoed `outcome` string + optional policy, so
/// [`fauna_mail::outbound::tlsrpt::policy_for_attempt`] derives the same
/// RFC 8460 §4.4 bucket nest would have produced on the legacy in-nest path.
/// `version` is hard-coded `"STSv1"` — the wire `MtaStsPolicyWire` omits it
/// (RFC 8461 §3.2 mandates it; the bridge only forwards policies that parsed
/// as STSv1).
///
/// This is the read half of the round trip `lookup_to_reply` writes, and both
/// halves now ask the same owner (`MtaStsOutcome::from_wire`,
/// `MtaStsMode::from_str`) rather than each carrying its own inverted table.
/// An unknown token stays a `malformed` protocol error rather than a guess:
/// every candidate guess is a delivery decision, and RFC 8461 §5's fail-safe
/// direction is not the same for all four outcomes.
fn mta_sts_lookup_from_wire(
    outcome: &str,
    policy: Option<&MtaStsPolicyWire>,
) -> Result<fauna_mail::outbound::mta_sts::MtaStsLookup, RpcError> {
    use fauna_mail::outbound::mta_sts::{
        FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsOutcome, MtaStsPolicy,
    };
    let parsed = MtaStsOutcome::from_wire(outcome)
        .ok_or_else(|| malformed(format!("unknown mta_sts_outcome {outcome:?}")))?;
    if let Some(payload_free) = MtaStsLookup::from_outcome(parsed) {
        return Ok(payload_free);
    }
    // The one outcome that carries a body.
    let p = policy.ok_or_else(|| malformed("mta_sts_outcome=found requires mta_sts_policy"))?;
    let mode: MtaStsMode = p
        .mode
        .parse()
        .map_err(|_| malformed(format!("unknown mta_sts mode {:?}", p.mode)))?;
    Ok(MtaStsLookup::Found(FetchedPolicy {
        id: p.id.clone(),
        policy: MtaStsPolicy {
            version: "STSv1".to_string(),
            mode,
            mx: p.mx.clone(),
            max_age_secs: p.max_age_secs,
        },
    }))
}

/// Pure reconstruction of one TLSRPT [`AttemptOutcome`] from a
/// `report_tls_attempt` request: format the TLSA records → policy-strings,
/// rebuild the MTA-STS lookup, and run the shared `policy_for_attempt`
/// bucketer. Factored out of the handler so the bucket attribution (the
/// `tlsa`>`sts`>`no-policy-found` precedence) is unit-testable without an
/// `AppState` or an approved-bridge actor.
fn report_to_attempt_outcome(
    req: ReportTlsAttemptRequest,
) -> Result<fauna_mail::outbound::tlsrpt::AttemptOutcome, RpcError> {
    let lookup = mta_sts_lookup_from_wire(&req.mta_sts_outcome, req.mta_sts_policy.as_ref())?;
    let tlsa_records: Vec<fauna_mail::outbound::dane::TlsaRecord> = req
        .tlsa_records
        .iter()
        .map(|r| fauna_mail::outbound::dane::TlsaRecord {
            usage: r.usage,
            selector: r.selector,
            matching: r.matching,
            data: r.data.clone(),
        })
        .collect();
    let tlsa_strings = fauna_mail::outbound::dane::tlsa_policy_strings(&tlsa_records);
    let policy = fauna_mail::outbound::tlsrpt::policy_for_attempt(
        &req.recipient_domain,
        &req.mx_host,
        &tlsa_strings,
        &lookup,
    );
    Ok(fauna_mail::outbound::tlsrpt::AttemptOutcome {
        recipient_domain: req.recipient_domain,
        policy,
        failure_type: req.result_type,
    })
}

/// `fauna.bridges.report_tls_attempt` (T2.4). The Go MTA bridge reports one
/// outbound delivery attempt's TLS outcome; nest reconstructs the RFC 8460
/// §4.4 policy bucket via the shared pure `policy_for_attempt` and records it
/// into the daily TLSRPT aggregator (`docs/goal/behavior/smtp-server.md`
/// § TLSRPT outbound reporter). Recording is best-effort: a malformed report
/// is rejected, but a well-formed one always succeeds (the aggregator is an
/// in-memory `HashMap` insert). MTA-only.
fn report_tls_attempt_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.report_tls_attempt").await?;
            let req: ReportTlsAttemptRequest = decode(&payload).map_err(malformed)?;
            if req.recipient_domain.is_empty() {
                return Err(malformed("recipient_domain must be non-empty"));
            }
            let outcome = report_to_attempt_outcome(req)?;
            use fauna_mail::outbound::tlsrpt::OutboundTlsrptRecorder;
            state.email.tlsrpt_aggregator.record(outcome);
            encode_reply(&ReportTlsAttemptReply { ok: true })
        })
    })
}

/// Hard ceiling on the number of recipients accepted in one enqueue.
/// Matches the submission listener's per-message MaxRecipients fast-
/// path (the MTA bridge can't have validated more than this many at
/// RCPT TO time). Defends nest against a malicious MTA submitting a
/// 100k-recipient envelope.
const ENQUEUE_OUTBOUND_MAX_RECIPIENTS: usize = 100;

/// Hard ceiling on the raw message size accepted by `enqueue_outbound_
/// mail`. Mirrors `SpamPolicyThresholds::max_message_bytes` default
/// (50 MiB) — submissions larger than that would have been rejected at
/// SMTP DATA before reaching nest. The cap defends nest against a
/// rogue bridge dumping arbitrary blobs into outbound_mail_queue.
const ENQUEUE_OUTBOUND_MAX_BYTES: usize = 50_000_000;

/// Caller-scope a `BridgeMda` call to the AUTH'd organizer it is acting for
/// (caldav-server.md § Server-side auto-schedule). Shared by the two server-side
/// auto-schedule gateway RPCs: `enqueue_outbound_mail` (the email-reachable leg)
/// and `deliver_sealed_scheduling` (the mailbox-less leg).
///
/// The MDA MUST carry `on_behalf_of_actor`; nest rejects the call unless
/// `original_sender` resolves — through the *same* exact-alias path AUTH uses
/// (`lookup_exact_alias`, the `validate_recipient` login resolver) — to that
/// actor. So a (compromised or buggy) MDA can only ever act as the single
/// local organizer whose encrypted PUT session it is inside: never an arbitrary
/// local actor, never an external / non-existent From. This is the same
/// `target == actor` shape `require_caller_scope` enforces for the calendar
/// storage RPCs — the MDA's grant is strictly narrower than the MTA's
/// (sender-unconstrained) submission path. Returns the validated 32-byte
/// organizer actor on success.
async fn enforce_mda_sender_scope(
    state: &Arc<AppState>,
    on_behalf_of_actor: Option<&[u8]>,
    original_sender: &str,
) -> Result<[u8; 32], RpcError> {
    let claimed = on_behalf_of_actor
        .ok_or_else(|| malformed("BridgeMda call requires on_behalf_of_actor"))?;
    let claimed: [u8; 32] =
        crate::rpc_errors::require_bytes32("on_behalf_of_actor", claimed).map_err(malformed)?;
    let (local, domain) = original_sender
        .rsplit_once('@')
        .ok_or_else(|| permission_denied("original_sender must be a local@domain address"))?;
    let resolved = state
        .db
        .lookup_exact_alias(domain, local)
        .await
        .map_err(internal)?;
    match resolved {
        Some(actor) if actor == claimed => Ok(claimed),
        _ => Err(permission_denied(
            "BridgeMda may act only as the organizer it AUTH'd \
             (original_sender must resolve to on_behalf_of_actor)",
        )),
    }
}

/// THE chokepoint for putting mail onto the outbound path.
///
/// Partitions `fields.recipients` by the deployment's primary mail domain and
/// guarantees the in-domain invariant for **every** caller: an in-domain
/// recipient that resolves to a local mailbox (the full alias resolver — exact /
/// `+suffix` / wildcard / catch-all / disposable / role-address) is **sealed to
/// that recipient's MSEK-derived pubkey and delivered locally** through the
/// shared sealed-ingest path (`__mail/<actor>` segment + `bridge_imap_messages`
/// INBOX); everything else (external recipients, and in-domain recipients that
/// resolve to Forward / Reject / nothing) goes onto `outbound_mail_queue`. An
/// in-domain recipient is **never** enqueued for MX relay — that self-loops back
/// through the box's own inbound listener and, on a containerized deploy,
/// hairpins through the docker bridge (`172.18.0.1`) and `554`-bounces at the
/// inbound HELO-identity check.
///
/// The partition lives at the **queue-insert**, not in any one handler, so the
/// invariant holds for the submission handler (`enqueue_outbound_mail`), the MDA
/// auto-schedule gateway, **and** the nest-internal direct-enqueue callers
/// (DSN/NDR generation, vacation auto-reply). See smtp-server.md § Outbound
/// submission flow. Returns one queue row id per MX-relayed recipient
/// (in-domain recipients delivered locally produce no row).
///
/// Note: `fauna.email.send` keeps its own in-domain path (the *exact-match*
/// alias resolver, mail-aliases.md § exact), so it does **not** route through
/// here; security notifications seal directly via [`seal_and_ingest_local`]
/// (they always target a known in-domain actor and are never "outbound").
pub(crate) async fn submit_outbound(
    state: &Arc<AppState>,
    fields: NewOutbound<'_>,
    now: i64,
    apply_warmup: bool,
) -> Result<Vec<i64>, RpcError> {
    let recipients_owned: Vec<String> = fields.recipients.iter().map(|r| r.to_string()).collect();
    // The split is against the deployment's PRIMARY mail domain, resolved from the
    // runtime `local_domains` table (`primary_mail_domain`). The legacy boot-time
    // `state.email.domain` fallback is **removed**; keying on it (always `None` in
    // the shipped binary) had left this short-circuit DEAD (every recipient
    // classified remote → in-domain recipients MX-self-loop-relayed), a gap only
    // the real-image tier_4 acceptance surfaced (`test_caldav_autoschedule_imip`).
    let primary_domain = primary_mail_domain(state).await?;
    let (local_parts, remote_addrs) =
        fauna_mail::routing::partition_recipients(&recipients_owned, primary_domain.as_deref())
            .map_err(|addr| malformed(format!("invalid recipient address: {addr}")))?;

    // sender_domain rides into the sealed in-domain copy's metadata + the
    // alias-hit log (the submission's MAIL FROM domain). A null sender
    // (DSN / auto-reply) has no domain → "unknown", matching the handler.
    let sender_domain = fields
        .original_sender
        .rsplit_once('@')
        .map(|(_, d)| d.to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let domain = primary_domain.clone().unwrap_or_default();

    // The authenticated-sender stamp this door writes into each in-domain
    // sealed copy (`smtp-server.md` § Architectural rules → *The `X-Fauna-*`
    // namespace*; consumer `caldav-server.md` § Who may mutate … → *The mail
    // rail*): the envelope sender the caller presents — for the MTA submission
    // door the address it validated as owned by the authenticated actor, for the
    // MDA auto-schedule gateway the organizer `enforce_mda_sender_scope` bound
    // to `on_behalf_of_actor`, for the list fan-out the nest's own list address.
    // The nest-internal null-sender callers (DSN / NDR / auto-reply) pass `""`,
    // which the builder refuses → no stamp. Prepended ahead of the body, so it
    // is the first occurrence the reader takes; local copies only — never an
    // outbound-queue row.
    let sender_stamp = fauna_mail::sender_auth::authenticated_sender_stamp(fields.original_sender);

    // Resolve + locally deliver each in-domain recipient. Only a Mailbox
    // resolution short-circuits to local delivery; Forward / Reject / unresolved
    // falls through to the outbound queue (status quo). A per-recipient seal
    // failure is logged and dropped — never silently re-queued onto MX.
    //
    // The Reject arm is uniform across all five reject reasons (unknown
    // address, disabled, expired, invalid sub-address, rate-capped) and is the
    // twin of `email_handlers`'s `fauna.email.send` in-domain branch: the two
    // doors move together or not at all (mail-aliases.md § Per-alias rate-cap
    // -> *Second consumer, deliberately left uniform*).
    let mut queued_addrs: Vec<String> = remote_addrs;
    for local in &local_parts {
        match resolve_local_recipient(state, &domain, local, &sender_domain).await {
            Ok(LocalRecipientOutcome::Mailbox {
                actor_id,
                mut stamped_headers,
                ..
            }) => {
                stamped_headers.extend(sender_stamp.clone());
                // Real user mail (MTA submission / MDA auto-schedule), so the
                // guardian mail gate applies: `original_sender` is the envelope
                // FROM. A null reverse-path here carries no DSN correlation
                // (this path re-ingests a message the nest already holds
                // decoded), so it is gated like any other cold null-path mail.
                if let Err(e) = seal_and_ingest_local(
                    state,
                    &actor_id,
                    fields.raw_message,
                    &sender_domain,
                    MailIngress::from_envelope(fields.original_sender, None),
                    &stamped_headers,
                )
                .await
                {
                    tracing::warn!(
                        recipient = %format!("{local}@{domain}"),
                        error = %e.code,
                        "in-domain local delivery failed on submit_outbound"
                    );
                }
            }
            Ok(_) => queued_addrs.push(format!("{local}@{domain}")),
            Err(e) => {
                tracing::warn!(
                    recipient = %format!("{local}@{domain}"),
                    error = %e.code,
                    "in-domain recipient resolution failed on submit_outbound"
                );
                queued_addrs.push(format!("{local}@{domain}"));
            }
        }
    }

    if queued_addrs.is_empty() {
        return Ok(Vec::new());
    }
    // Fresh-IP warm-up gate (`mail-deliverability.md` § Enforcement at
    // submission time). ONLY the authenticated 465/587 submission path
    // (`apply_warmup`, set from `class == BridgeMta` at the handler) is
    // warm-up-rate-capped — the MDA calendar-auto-schedule gateway, the
    // auto-reply, and the NDR/bounce path all submit through here too and must
    // NEVER be deferred. The cap is deployment-wide over the EXTERNAL recipients
    // that actually leave via the outbound MX (`queued_addrs`, post in-domain
    // partition); in-domain sealed deliveries never touch the outbound IP.
    // Over today's cap → the whole message is queued for tomorrow
    // (`next_attempt_at = next_utc_midnight`), the MTA still returns 250 OK (the
    // enqueue succeeds), never bounced.
    let next_attempt_at = if apply_warmup {
        match state
            .db
            .try_consume_warmup(now, queued_addrs.len() as u32)
            .await
            .map_err(internal)?
        {
            crate::db::mail_warmup::WarmupDecision::Allowed => now,
            crate::db::mail_warmup::WarmupDecision::Deferred => {
                fauna_mail::warmup::next_utc_midnight(now)
            }
        }
    } else {
        now
    };
    let recipient_refs: Vec<&str> = queued_addrs.iter().map(|s| s.as_str()).collect();
    state
        .db
        .enqueue_outbound_split(
            NewOutbound {
                recipients: &recipient_refs,
                ..fields
            },
            now,
            next_attempt_at,
        )
        .await
        .map_err(internal)
}

fn enqueue_outbound_mail_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class =
                require_class(&state, &actor_id, "fauna.bridges.enqueue_outbound_mail").await?;
            let mut req: EnqueueOutboundMailRequest = decode(&payload).map_err(malformed)?;
            if req.original_msgid.trim().is_empty() {
                return Err(malformed("original_msgid must not be empty"));
            }
            if req.original_sender.trim().is_empty() {
                return Err(malformed("original_sender must not be empty"));
            }
            // #6: a `kind='list'` address must NOT be submitted over raw SMTP —
            // per-recipient RFC 8058 unsubscribe stamping is structurally
            // impossible for a single-body submission (mail-mass-mailing.md
            // § How the per-list cap separates). The canonical (and only) list
            // path is `fauna.bridges.send_list_message`, where the nest fans out
            // one-per-member, stamps each recipient's List-* headers, and flags
            // the rows for delivery-time DKIM signing. Reject here so the
            // list-mode discriminator is the explicit RPC, not a MAIL-FROM match.
            // This handler is the bridge submission entry; the nest-internal
            // direct-enqueue callers (DSN/auto-reply/the list fan-out itself)
            // call `submit_outbound` directly and are unaffected.
            if let Some((local, domain)) = req.original_sender.rsplit_once('@')
                && state
                    .db
                    .lookup_list_id_for_address(domain, local)
                    .await
                    .map_err(internal)?
                    .is_some()
            {
                return Err(RpcError::new(
                    "fauna.bridges.list_submission_requires_send_rpc",
                    "error.bridges.list_submission_requires_send_rpc",
                )
                .with_details_text(
                    "MAIL FROM is a mailing-list address; use fauna.bridges.send_list_message \
                     (per-recipient RFC 8058 unsubscribe stamping requires the nest fan-out)",
                ));
            }
            // Caller-scope the MDA (server-side `calendar-auto-schedule`
            // gateway): it may enqueue only as the organizer it AUTH'd, never an
            // arbitrary From (caldav-server.md § Server-side auto-schedule). The
            // MTA path is exempt HERE — nest-side it is the sender-unconstrained
            // submission gateway and leaves `on_behalf_of_actor` unset — because
            // the MTA holds both sender identities to the ownership rule itself,
            // before enqueueing: the envelope at MAIL FROM and the From: header
            // at DATA (mail-multidomain.md § From: header ownership).
            if class == CallerClass::BridgeMda {
                enforce_mda_sender_scope(
                    &state,
                    req.on_behalf_of_actor.as_deref(),
                    &req.original_sender,
                )
                .await?;
            }
            if req.recipients.is_empty() {
                return Err(malformed("recipients must not be empty"));
            }
            if req.recipients.len() > ENQUEUE_OUTBOUND_MAX_RECIPIENTS {
                return Err(malformed(format!(
                    "recipients exceeds ceiling ({} > {})",
                    req.recipients.len(),
                    ENQUEUE_OUTBOUND_MAX_RECIPIENTS
                )));
            }
            // A DKIM-signed body over the inline request budget arrives as a
            // staged envelope (smtp-server.md § Message size limits, the
            // staged-envelope rule): the MTA sealed it under a one-shot key,
            // staged the ciphertext on the byte plane, and sent a reference in
            // place of an inline `raw_message`. Resolve it from the local blob
            // store into the plaintext body and proceed exactly as an inline
            // submission — quota, product ceiling, SRS rewrite and the retry
            // curve all charge the resolved bytes. Exactly-one-of with a
            // non-empty `raw_message`; the resolve fails closed (join pinned on
            // the sealed total, then the AEAD tag authenticates the whole join).
            if let Some(staged) = req.staged_body.take() {
                if !req.raw_message.is_empty() {
                    return Err(malformed(
                        "raw_message must be empty when staged_body is set",
                    ));
                }
                req.raw_message = crate::mail_body_plane::resolve_staged_body(
                    &state,
                    &staged,
                    ENQUEUE_OUTBOUND_MAX_BYTES as u64
                        + fauna_mail::staged_envelope::STAGED_ENVELOPE_OVERHEAD_BYTES,
                )
                .await?;
            }
            if req.raw_message.is_empty() {
                return Err(malformed("raw_message must not be empty"));
            }
            if req.raw_message.len() > ENQUEUE_OUTBOUND_MAX_BYTES {
                return Err(malformed(format!(
                    "raw_message exceeds ceiling ({} > {})",
                    req.raw_message.len(),
                    ENQUEUE_OUTBOUND_MAX_BYTES
                )));
            }
            // (The interim inline-budget backstop that lived here is retired: an
            // over-inline-budget body now legitimately enters the queue via the
            // staged envelope above, and `fetch_outbound_due` stages it back down
            // on serve. The product ceiling `ENQUEUE_OUTBOUND_MAX_BYTES` is now
            // the only outbound size bound — smtp-server.md § Message size
            // limits, the staged-envelope rule, S9.3 guard-lift.)
            // The in-domain partition + local-sealed delivery (NEVER the MX-relay
            // queue, which self-loops and `554`-bounces on a containerized deploy)
            // lives at the queue-insert chokepoint, `submit_outbound`, so the
            // invariant holds identically for this handler, the MDA auto-schedule
            // gateway (which passes ALL attendees through this RPC), and the
            // nest-internal direct-enqueue callers (smtp-server.md § Outbound
            // submission; caldav-server.md § Server-side auto-schedule). The
            // partition there resolves the primary domain from the runtime
            // `local_domains` table (`primary_mail_domain`); the legacy boot-time
            // `state.email.domain` it once fell back to — always `None` in the
            // shipped binary, which had shipped the short-circuit dead — is removed.
            // `created_at` rides the same clock seam as the retry math
            // (`AppState::outbound_now`). The submission path has no inbound
            // verdicts to carry forward → the "none / none / none" triple.
            let recipient_refs: Vec<&str> = req.recipients.iter().map(|s| s.as_str()).collect();

            // Guardian mail gate — outbound auto-seed, path B of two
            // (`family-safety.md` § Wire & data shape). Path A is
            // `fauna.email.send` (the native Conversations client, authenticated
            // as the ward). Here the caller is the *bridge* service-user, so the
            // ward is resolved from the envelope sender the MTA verified it owns.
            // Seeded in the handler rather than in `submit_outbound`, because
            // that core also serves DSN / auto-reply / NDR callers whose sender
            // is the box itself, not a ward — and over `req.recipients`, not the
            // post-partition `queued_addrs`, since same-nest recipients never
            // enter the outbound queue. Best-effort; never fails a submission.
            //
            // Two facts, two uses: the recipient ADDRESSES let their replies
            // through, and the message's own MESSAGE-ID lets a remote MTA's
            // *bounce* of this message through — consumed against a budget
            // sized by the remote-recipient count, so the partition (the same
            // one `submit_outbound` runs internally) is recomputed here first;
            // an in-domain-only submission seeds no correlation, since nothing
            // can ever bounce it (§ The mail gate — an address is guessable,
            // an id is not, and even a real id must not correlate forever).
            // Both are no-ops for an unsupervised sender.
            if let Some((local, domain)) = req.original_sender.rsplit_once('@')
                && let Ok(Some(sender_actor)) = state.db.lookup_exact_alias(domain, local).await
            {
                for rcpt in &recipient_refs {
                    if let Err(e) = state
                        .db
                        .add_mail_allowlist_entry(&sender_actor, rcpt, "outbound")
                        .await
                    {
                        tracing::warn!("guardian mail allowlist seed failed: {e}");
                    }
                }
                let remote_recipients = match primary_mail_domain(&state).await {
                    Ok(primary) => fauna_mail::routing::partition_recipients(
                        &req.recipients,
                        primary.as_deref(),
                    )
                    .map(|(_, remote)| remote.len())
                    // An unpartitionable recipient list fails the submission in
                    // `submit_outbound` below; seed nothing (fail-closed).
                    .unwrap_or(0),
                    Err(_) => 0,
                };
                if let Err(e) = state
                    .db
                    .add_sent_msgid(&sender_actor, &req.original_msgid, remote_recipients)
                    .await
                {
                    tracing::warn!("guardian sent-msgid seed failed: {e}");
                }
            }

            let ids = submit_outbound(
                &state,
                NewOutbound {
                    original_msgid: &req.original_msgid,
                    original_sender: &req.original_sender,
                    recipients: &recipient_refs,
                    raw_message: &req.raw_message,
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    // Deliberately unstamped, not an oversight: the raw-SMTP
                    // submission path has its OWN per-actor metering, applied
                    // before this point at RCPT TO — the per-message recipient
                    // cap and the authoritative recipients/day quota, both via
                    // `check_submission_quota` (`smtp-server.md`
                    // § Architectural rules). `submit_actor_id` is the
                    // `fauna.email.send` per-hour ceiling's key; stamping it
                    // here would silently make that ceiling bind this path too,
                    // which is a behaviour change to a different surface and
                    // wants its own track, not a side effect of.
                    submit_actor_id: None,
                },
                state.outbound_now(),
                // Warm-up-rate-cap the authenticated submission gateway only;
                // the MDA auto-schedule path (BridgeMda) is exempt — its
                // calendar invites must not be deferred to tomorrow.
                class == CallerClass::BridgeMta,
            )
            .await?;
            encode_reply(&EnqueueOutboundMailReply { ids })
        })
    })
}

/// `fauna.bridges.deliver_sealed_scheduling` — the MDA server-side
/// `calendar-auto-schedule` gateway's **mailbox-less** delivery rail
/// (caldav-server.md § Server-side auto-schedule, C3). The email-reachable leg
/// rides `enqueue_outbound_mail`; this delivers the iMIP to a mailbox-less Fauna
/// attendee (CalDAV enabled, email disabled) over the WS-RPC sealed MLS
/// scheduling rail instead.
///
/// The MDA has already sealed the one-off MLS welcome + iMIP **itself**
/// (`mailfauna.BuildSchedulingDelivery`, an ephemeral signer — nest sees
/// ciphertext only) and ships the opaque bytes here. This handler:
///   1. class-gates BridgeMda (the allowlist also restricts it — NOT the
///      User gate the conversation RPCs carry), then
///   2. caller-scopes to the AUTH'd organizer (the same `on_behalf_of_actor` +
///      `original_sender`-resolves-to-it pattern as `enqueue_outbound_mail`),
///      then
///   3. runs `welcome.deliver`(tagged `Scheduling`) + `channel.send` **as the
///      organizer** by REUSING the conversations cores
///      (`deliver_scheduling_as_organizer`) — membership/inbox bind to the
///      organizer, not the MDA, and we never replicate the cross-nest
///      federation relay.
///
/// `peer_domain` carries the rail across nests: the handler derives the relay
/// base URL via `resolve_handle_domain` (the same mapping the client rail's
/// `peer_nest_url` uses) so the Welcome relays to a recipient on a foreign nest.
fn deliver_sealed_scheduling_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let _class =
                require_class(&state, &actor_id, "fauna.bridges.deliver_sealed_scheduling").await?;
            let req: DeliverSealedSchedulingRequest = decode(&payload).map_err(malformed)?;

            // Caller-scope to the AUTH'd organizer (returns the validated 32-byte
            // organizer). The allowlist already restricts this kind to BridgeMda,
            // so every reaching caller is the MDA acting for one organizer.
            let organizer = enforce_mda_sender_scope(
                &state,
                Some(req.on_behalf_of_actor.as_slice()),
                &req.original_sender,
            )
            .await?;

            // `Some(domain)` ⇒ a recipient on a foreign nest; derive the relay
            // base URL exactly like the client rail's `peer_nest_url`. Empty /
            // absent ⇒ same-nest (`None`).
            let nest_url = req
                .peer_domain
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| fauna_provisioning::probe::resolve_handle_domain(d).base_url);

            let (inbox_id, seq) = crate::conversations_handlers::deliver_scheduling_as_organizer(
                &state,
                &organizer,
                &req.recipient_actor_id,
                &req.channel_id,
                nest_url,
                req.welcome_bytes,
                req.app_envelope,
            )
            .await?;

            encode_reply(&DeliverSealedSchedulingReply { inbox_id, seq })
        })
    })
}

/// Floor/ceiling for the vacation `interval_hours` — clamp a client-supplied
/// value so `0` (auto-reply to every message → a loop amplifier) or an absurd
/// interval can't slip through. RFC 5230's default is 7 days; we allow 1 h .. 1 y.
const AUTO_REPLY_MIN_INTERVAL_HOURS: u32 = 1;
const AUTO_REPLY_MAX_INTERVAL_HOURS: u32 = 24 * 365;

/// `send_auto_reply` (BridgeMta) — atomically claim the vacation rate-limit for
/// `(recipient, envelope-sender)` and, if the slot is free, enqueue the bridge's
/// composed + DKIM-signed reply with a null envelope-from. The MTA calls this
/// only after the perimeter loop guard (`fauna_mail::filter::auto_reply_decision`)
/// passed; `sent == true` means the reply is now queued for delivery.
fn send_auto_reply_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.send_auto_reply").await?;
            let req: SendAutoReplyRequest = decode(&payload).map_err(malformed)?;
            let recipient_id: [u8; 32] = crate::rpc_errors::require_bytes32(
                "recipient_actor_id",
                req.recipient_actor_id.as_slice(),
            )
            .map_err(malformed)?;
            let from = req.envelope_from.trim();
            if from.is_empty() {
                // A null envelope-from never auto-replies (the perimeter loop
                // guard already suppresses this — defensive double-check).
                return encode_reply(&SendAutoReplyReply { sent: false });
            }
            if req.raw_message.is_empty() {
                return Err(malformed("raw_message must not be empty"));
            }
            if req.raw_message.len() > ENQUEUE_OUTBOUND_MAX_BYTES {
                return Err(malformed(format!(
                    "raw_message exceeds ceiling ({} > {})",
                    req.raw_message.len(),
                    ENQUEUE_OUTBOUND_MAX_BYTES
                )));
            }
            let interval = req
                .interval_hours
                .clamp(AUTO_REPLY_MIN_INTERVAL_HOURS, AUTO_REPLY_MAX_INTERVAL_HOURS);
            // Rate-limit key = BLAKE3 of the lowercased envelope-from. Hashed
            // here so the convention lives in exactly one place and the raw
            // sender address never sits in `auto_reply_log`.
            let sender_hash: [u8; 32] = *blake3::hash(from.to_lowercase().as_bytes()).as_bytes();
            let claimed = state
                .db
                .try_claim_auto_reply(&recipient_id, &sender_hash, interval)
                .await
                .map_err(internal)?;
            if !claimed {
                return encode_reply(&SendAutoReplyReply { sent: false });
            }
            // Submit with a NULL envelope-from (RFC 3834) through the
            // `submit_outbound` chokepoint: an auto-reply addressed back to an
            // in-domain sender delivers locally (sealed INBOX) instead of
            // MX-self-looping; an external sender enqueues for relay. (The
            // submission `enqueue_outbound_mail` handler forbids an empty sender,
            // so the auto-reply submits here nest-side, like the NDR path.)
            submit_outbound(
                &state,
                NewOutbound {
                    original_msgid: &req.original_msgid,
                    original_sender: "",
                    recipients: &[from],
                    raw_message: &req.raw_message,
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                state.outbound_now(),
                // Auto-replies (RFC 3834) are not warm-up-deferred.
                false,
            )
            .await?;
            encode_reply(&SendAutoReplyReply { sent: true })
        })
    })
}

// ── Forward delivery trigger (MTA perimeter, N2) ───────────────
//
// The forward DECISION is an MTA-perimeter decision: only the Go MTA
// holds the inbound plaintext during DATA. The nest stores only the
// recipient-sealed `encrypted_body` (it cannot decrypt it), so it cannot
// produce the plaintext copy a downstream MX needs — a nest-side hook in
// `persist_inbound_mail_request` is infeasible. After the local mailbox
// write commits, the MTA reads the recipient's forward config via
// `fetch_recipient_forward_config` and (if set, non-null-sender, loop-checks
// pass) enqueues the forward via `forward_message`. See
// `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all" +
// § Storage-mode interaction + § N2.

/// `fetch_recipient_forward_config` (BridgeMta) — the MTA's single
/// chokepoint for reading a recipient's forward config at the perimeter
/// (mirrors the per-recipient `fetch_recipient_mls_pubkey`). The request
/// carries the *recipient* actor (not the caller). This is the one spot the
/// N1b sealed-fetch upgrade swaps (plaintext read → sealed-blob fetch +
/// bridge-side unseal), so the MTA must not read forward config elsewhere.
fn fetch_recipient_forward_config_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.fetch_recipient_forward_config",
            )
            .await?;
            let req: FetchRecipientForwardConfigRequest = decode(&payload).map_err(malformed)?;
            let recipient: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            let forward_all_to = state
                .db
                .get_forward_all_to(&recipient)
                .await
                .map_err(internal)?;
            encode_reply(&FetchRecipientForwardConfigReply { forward_all_to })
        })
    })
}

/// `fetch_recipient_filters` (BridgeMta) — the MTA's chokepoint for reading a
/// recipient's stored email filter rules at the perimeter, so the pure
/// `fauna_mail::filter::evaluate` engine can run pre-seal on plaintext
/// (`mail-forwarding.md` § Where rule eval runs — eval is an MTA-perimeter
/// decision; the nest only stores + serves rules, never evaluates on sealed
/// ciphertext). Reuses the user-facing `fauna.email.filters.list` projection
/// (`email_handlers::email_filters_for_actor`) so the perimeter and the client
/// see byte-identical filter shapes. The request carries the *recipient* actor
/// (not the caller).
fn fetch_recipient_filters_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fetch_recipient_filters").await?;
            let req: FetchRecipientFiltersRequest = decode(&payload).map_err(malformed)?;
            let recipient: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            let filters = crate::email_handlers::email_filters_for_actor(&state.db, &recipient)
                .await
                .map_err(internal)?;
            encode_reply(&FetchRecipientFiltersReply { filters })
        })
    })
}

/// `forward_message` (BridgeMta) — the MTA enqueues a forward of an inbound
/// message it has already locally delivered. One `outbound_mail_queue` row
/// (`is_forwarded = 1`, carrying the forwarder actor + rule) so the SRS
/// rewrite at queue-out (N3) and NDR routing (N4) can read them off the row.
/// The SRS envelope rewrite is NOT applied here — this stores the original
/// envelope; `fetch_outbound_due` (N3) rewrites at dispatch.
fn forward_message_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.forward_message").await?;
            let req: ForwardMessageRequest = decode(&payload).map_err(malformed)?;
            let forwarder: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if req.original_msgid.trim().is_empty() {
                return Err(malformed("original_msgid must not be empty"));
            }
            // A forward of a null-sender (bounce) message is the backscatter
            // problem (`mail-forwarding.md:231,:254`); the MTA skips forwarding
            // null-sender messages, and nest rejects an empty sender here as
            // defense-in-depth against a rogue bridge.
            if req.original_sender.trim().is_empty() {
                return Err(malformed(
                    "original_sender must not be empty (null-sender forwards are skipped at the MTA)",
                ));
            }
            if req.destination.trim().is_empty() {
                return Err(malformed("destination must not be empty"));
            }
            if req.rule_id_or_forward_all.trim().is_empty() {
                return Err(malformed("rule_id_or_forward_all must not be empty"));
            }
            if req.raw_message.is_empty() {
                return Err(malformed("raw_message must not be empty"));
            }
            if req.raw_message.len() > ENQUEUE_OUTBOUND_MAX_BYTES {
                return Err(malformed(format!(
                    "raw_message exceeds ceiling ({} > {})",
                    req.raw_message.len(),
                    ENQUEUE_OUTBOUND_MAX_BYTES
                )));
            }
            // Interim inline-budget backstop — same rationale as the
            // `enqueue_outbound_mail` arm (smtp-server.md § Message size limits,
            // the staged-envelope rule); the MTA's `dispatchForward` suppresses
            // over-budget forwards before ever calling this.
            if req.raw_message.len()
                > fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize
            {
                return Err(malformed(format!(
                    "raw_message exceeds the inline outbound budget ({} > {}); the staged-envelope leg is not built",
                    req.raw_message.len(),
                    fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES
                )));
            }
            // The local-delivery skip for `Redirect` is the MTA's decision,
            // already made before this call. Nest persists the mode on the
            // queued row (both arms below), because it decides what a burn
            // would destroy: a succession burns a queued `Copy` — a second
            // copy — and carries a `Redirect`, the only copy of mail already
            // answered 250 (`successions.rs::rule_the_forwarding_family`).
            // N5 per-account forward rate-cap (`mail-forwarding.md` § Per-account
            // forward rate-limit `:176-179`). The effective cap is min(the
            // account's chosen `forward_per_hour`, the admin ceiling). If
            // dispatching now would exceed it, park the forward in
            // `forward_queue` — promoted at the rate-cap cadence by the
            // promotion step in `fetch_outbound_due` — instead of enqueuing it
            // outbound. The sliding window counts this actor's forwarded rows
            // already on the outbound queue in the last hour.
            let per_account = state
                .db
                .get_forward_per_hour(&forwarder)
                .await
                .map_err(internal)?;
            let cap = per_account.min(fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING);
            let window = state
                .db
                .count_forward_dispatched_window(&forwarder, 3600)
                .await
                .map_err(internal)?;
            // The daily recipients allowance composes with the hourly cap: it
            // is drawn only for a forward the hourly cap would dispatch now,
            // and a forward it cannot cover parks exactly like an over-cap one
            // ([`draw_forward_daily_unit`]).
            let over_daily = window < cap && {
                let max_per_day = state
                    .db
                    .get_submission_policy()
                    .await
                    .map_err(internal)?
                    .effective()
                    .max_per_day;
                !draw_forward_daily_unit(&state, &forwarder, max_per_day)
                    .await
                    .map_err(internal)?
            };
            if window + 1 > cap || over_daily {
                // Over either cap → park. Ceiling = min(cap,ceiling)*24; over it the
                // oldest parked `copy` forwards are FIFO-evicted, and a
                // `redirect` with no copy left to evict is refused (§ Queue
                // ceiling — a `redirect` row is the only copy of the mail).
                let ceiling = cap.saturating_mul(fauna_mail::FORWARD_QUEUE_CEILING_MULTIPLIER);
                let outcome = state
                    .db
                    .enqueue_forward_queue(
                        crate::db::forward_queue::NewForwardQueueEntry {
                            actor_id: &forwarder,
                            source_message_id: &req.original_msgid,
                            original_sender: &req.original_sender,
                            destination_address: &req.destination,
                            rule_id_or_forward_all: &req.rule_id_or_forward_all,
                            raw_message: &req.raw_message,
                            copy_mode: req.copy_mode,
                        },
                        ceiling,
                    )
                    .await
                    .map_err(internal)?;
                // FIFO newest-evicts-oldest at the ceiling → notify the forwarder
                // IN-APP (never by email, `:183,:277`) that forwards are being
                // dropped.
                if !outcome.evicted_destinations.is_empty() {
                    notify_forward_queue_evicted(
                        &state,
                        &forwarder,
                        &outcome.evicted_destinations,
                        cap,
                    )
                    .await;
                }
                let Some(id) = outcome.id else {
                    return Err(forward_queue_full());
                };
                return encode_reply(&ForwardMessageReply { id, queued: true });
            }

            // Under cap → dispatch now. A forward has a single destination →
            // exactly one queue row.
            let ids = state
                .db
                .enqueue_outbound(NewOutbound {
                    original_msgid: &req.original_msgid,
                    original_sender: &req.original_sender,
                    recipients: &[req.destination.as_str()],
                    raw_message: &req.raw_message,
                    // The forward's own SPF/DMARC is set by the SRS rewrite at
                    // queue-out (we become the sender); record the "none"
                    // triple like the submission path.
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: true,
                    forward_actor_id: Some(&forwarder),
                    forward_rule_id: Some(&req.rule_id_or_forward_all),
                    forward_copy_mode: Some(req.copy_mode),
                    submit_actor_id: None,
                })
                .await
                .map_err(internal)?;
            let id = ids.first().copied().unwrap_or_default();
            encode_reply(&ForwardMessageReply { id, queued: false })
        })
    })
}

/// Notify a forwarding actor IN-APP (never by email — `mail-forwarding.md:277`)
/// that their forward queue is full and the oldest forwards are being dropped
/// (`:183`). `insert_notification` dedups on `(actor, notif_type)`, so this
/// produces a single standing "queue full" notification per actor rather than
/// one per dropped forward — the right product shape for a 2400-deep queue
/// (flooding the user with thousands of eviction notices would be hostile; the
/// admin investigates per `:189`). The summary names a representative dropped
/// destination + the configured rate.
async fn notify_forward_queue_evicted(
    state: &Arc<AppState>,
    forwarder: &[u8; 32],
    evicted_destinations: &[String],
    cap: u32,
) {
    let dest = evicted_destinations
        .first()
        .map(String::as_str)
        .unwrap_or("(unknown)");
    let text = crate::db::notifications::NotificationText::localized(
        fauna_protocol::LocalizedText::new("notifications.row_forward_queue_evicted")
            .with_arg("dest", dest)
            .with_arg("cap", cap.to_string()),
    );
    // Micros — the notifications column unit; seconds sort into 1970.
    let now = fauna_core::data::Timestamp::now().as_i64();
    match state
        .db
        .insert_notification(
            forwarder,
            &fauna_protocol::notifications::NotifType::MailForwardQueueEvicted,
            "fauna",
            None,
            None,
            None,
            &text,
            now,
        )
        .await
    {
        Ok(Some(notif_id)) => {
            state.ws.notify_push(
                forwarder,
                fauna_protocol::PushEvent::Notification(
                    fauna_protocol::push_events::NotificationPayload {
                        notification_id: notif_id,
                        notif_type:
                            fauna_protocol::notifications::NotifType::MailForwardQueueEvicted,
                        source: "fauna".into(),
                        sender_id: None,
                        content_id: None,
                        summary: text.summary().to_string(),
                        body: text.body().cloned(),
                        timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                        extra: std::collections::BTreeMap::new(),
                    },
                ),
            );
        }
        // Already-standing notification (deduped) → nothing to push.
        Ok(None) => {}
        Err(e) => tracing::warn!(
            target: "mail_forward",
            error = %e,
            "failed to record forward-queue eviction notification"
        ),
    }
}

/// The verified result of trying every stored SRS secret against an inbound
/// `SRS0=`/`SRS1=` local-part (`mail-forwarding.md` § Bounce decode). Pure +
/// secret-set-agnostic so the multi-secret rotation-overlap logic is unit-
/// testable without an `AppState`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SrsBounceDecode {
    /// MAC-verified under some stored secret. `row_id` is the SRS short-id (the
    /// outbound-queue row id, as a string); `original_sender` is the rewritten
    /// envelope's original `MAIL FROM`.
    Verified {
        row_id: String,
        original_sender: String,
    },
    /// Not an `SRS0=`/`SRS1=` address — a normal recipient, not a bounce.
    NotSrs,
    /// Structurally invalid SRS → `550`.
    Malformed,
    /// No stored secret verified the MAC → `550 5.1.1`, no retry.
    MacFail,
    /// MAC verified but the `TT` age exceeds the max → `550 5.4.4`.
    Expired,
}

/// Try each stored SRS secret in turn (`mail-forwarding.md:97,:256` — the N5
/// rotation holds a 2-secret overlap so an in-flight bounce minted under the
/// prior secret still verifies). `NotSrs` / `Malformed` are secret-independent
/// and short-circuit; otherwise `Verified` wins over `Expired` wins over
/// `MacFail`. An empty secret set decodes as `MacFail` (we can't verify).
fn srs_decode_over_secrets(
    secrets: &[Vec<u8>],
    now_day: u64,
    max_age_days: u64,
    local_part: &str,
) -> SrsBounceDecode {
    use fauna_mail::srs::{SrsError, srs_decode};
    let mut saw_expired = false;
    for secret in secrets {
        match srs_decode(secret, now_day, max_age_days, local_part) {
            Ok(d) => {
                return SrsBounceDecode::Verified {
                    row_id: d.forwarder_actor_id,
                    original_sender: d.original_sender,
                };
            }
            // Prefix / structure are secret-independent — decide immediately.
            Err(SrsError::NotSrs) => return SrsBounceDecode::NotSrs,
            Err(SrsError::Malformed) => return SrsBounceDecode::Malformed,
            // MAC passed under this secret but the bounce is too old.
            Err(SrsError::Expired) => saw_expired = true,
            // Wrong secret — keep trying the rest of the overlap window.
            Err(SrsError::MacFail) => {}
        }
    }
    if saw_expired {
        SrsBounceDecode::Expired
    } else {
        SrsBounceDecode::MacFail
    }
}

/// `decode_srs_bounce` (BridgeMta) — decode + verify an inbound `SRS0=`/`SRS1=`
/// recipient local-part the MTA saw at RCPT-TO (`mail-forwarding.md`
/// § Bounce decode). On a MAC-verified, in-age bounce, nest maps the SRS
/// short-id (the outbound-queue row id) back to the forwarder actor + the
/// bounced destination so N4 can route the NDR to the forwarder's mailbox
/// (NOT the original sender). A verified bounce whose row is gone (account
/// deleted / pruned) is an `orphan` — dropped with a counter, never the admin
/// mailbox (`:104,:117`). The actual NDR delivery + the Go RCPT-TO recognition
/// that calls this are N4.
fn decode_srs_bounce_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.decode_srs_bounce").await?;
            let req: DecodeSrsBounceRequest = decode(&payload).map_err(malformed)?;
            let secrets = state.db.list_srs_secrets().await.map_err(internal)?;
            let now_day = (state.outbound_now().max(0) as u64) / 86_400;
            let decoded = srs_decode_over_secrets(
                &secrets,
                now_day,
                fauna_mail::srs::DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS,
                &req.local_part,
            );
            // Every `outcome` token comes from `SrsBounceOutcome`, the owner —
            // the Go MTA switches on these six and its `default:` arm tempfails
            // (`451`), so a token spelled here that the bridge does not know
            // tempfails EVERY srs bounce forever rather than failing loudly.
            use fauna_mail::srs::SrsBounceOutcome;
            let no_payload = |o: SrsBounceOutcome| DecodeSrsBounceReply {
                outcome: o.as_wire().into(),
                ..Default::default()
            };
            let reply = match decoded {
                SrsBounceDecode::NotSrs => no_payload(SrsBounceOutcome::NotSrs),
                SrsBounceDecode::Malformed => no_payload(SrsBounceOutcome::Malformed),
                SrsBounceDecode::MacFail => no_payload(SrsBounceOutcome::MacFail),
                SrsBounceDecode::Expired => no_payload(SrsBounceOutcome::Expired),
                SrsBounceDecode::Verified {
                    row_id,
                    original_sender,
                } => {
                    // The short-id is the outbound row id; look it up to recover
                    // the forwarder actor + the bounced destination. A bad parse
                    // or a missing/non-forward row is an orphan.
                    let row = match row_id.parse::<i64>() {
                        Ok(id) => state.db.fetch_outbound_by_id(id).await.map_err(internal)?,
                        Err(_) => None,
                    };
                    match row {
                        Some(r) if r.is_forwarded && r.forward_actor_id.is_some() => {
                            DecodeSrsBounceReply {
                                outcome: SrsBounceOutcome::Ok.as_wire().into(),
                                forwarder_actor_id: r.forward_actor_id.unwrap().to_vec(),
                                original_sender,
                                original_destination: r.recipient,
                            }
                        }
                        _ => no_payload(SrsBounceOutcome::Orphan),
                    }
                }
            };
            encode_reply(&reply)
        })
    })
}

// ── rotate_srs_secret (N5) ─────────────────────────────────────

/// `fauna.bridges.rotate_srs_secret` (Admin) — mint a fresh SRS secret held
/// alongside the prior one as the 2-secret rotation overlap. Admin-class (the
/// SRS secret is deployment crypto material, rotated by the admin);
/// `rotate_srs_secret` generates the random bytes nest-side and the admin never
/// supplies or reads them (`mail-forwarding.md:279`). Replay-safe is *false* —
/// each call mints a new secret, so a replayed frame must not silently rotate
/// twice; the router's idempotency cache covers a genuine duplicate.
fn rotate_srs_secret_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.rotate_srs_secret").await?;
            // Empty request. It is app-callable (Admin-class), so it is
            // client↔nest wire and TOLERATES a newer app's added field
            // (transport.md rule 4) rather than refusing the call.
            let _req: RotateSrsSecretRequest = decode(&payload).map_err(malformed)?;
            state.db.rotate_srs_secret().await.map_err(internal)?;
            encode_reply(&RotateSrsSecretReply {
                rotated_at: now_epoch_secs(),
                // Forward-compat catch-all (transport.md rule 4); nest-minted.
                extra: Default::default(),
            })
        })
    })
}

// ── report_session_close ──────────────────────────────────────

fn report_session_close_handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            require_class(&state, &bridge_actor, "fauna.bridges.report_session_close").await?;
            let req: ReportSessionCloseRequest = decode(&payload).map_err(malformed)?;
            let target: [u8; 32] =
                crate::rpc_errors::require_bytes32("actor_id", req.actor_id.as_slice())
                    .map_err(malformed)?;
            if req.reason.trim().is_empty() {
                return Err(malformed("reason must not be empty"));
            }
            state
                .db
                .append_bridge_session_close(
                    &bridge_actor,
                    &target,
                    &req.credential_id,
                    &req.reason,
                    req.occurred_at,
                )
                .await
                .map_err(internal)?;
            encode_reply(&ReportSessionCloseReply { ok: true })
        })
    })
}

// ── Deliverability diagnostics + blocklist self-check (Admin) ──────
// docs/goal/behavior/mail-deliverability.md § Symptom diagnostics +
// § Blocklist self-check. Admin-class (admin-pane-only); the orchestration
// lives in `crate::mail_deliverability` over nest's DNS / STARTTLS seams.

/// Gather the diagnostic's expected-record input from nest state: the primary
/// mail domain + HELO name, the resolved outbound IP (A/AAAA of `mail.<primary>`),
/// the active DKIM selectors, and the MTA-STS policy-file fetch result. `None`
/// when no primary mail domain is configured (mail not enabled).
async fn gather_diagnostic_input(
    state: &Arc<AppState>,
) -> Result<Option<crate::mail_deliverability::DiagnosticInput>, RpcError> {
    let domains = state
        .db
        .list_active_mail_domains()
        .await
        .map_err(internal)?;
    let Some(primary) = domains.into_iter().find(|d| d.is_primary) else {
        return Ok(None);
    };
    let primary_domain = primary.domain_name;
    let helo_name = format!("mail.{primary_domain}");
    let resolver = state.dns_verifier.resolver();
    let outbound_ip =
        crate::mail_deliverability::resolve_outbound_ip(resolver.as_ref(), &helo_name).await;
    let dkim_selectors = state
        .db
        .list_dkim_selectors(Some(&primary_domain))
        .await
        .map_err(internal)?
        .into_iter()
        .map(|s| crate::mail_deliverability::DkimExpectation {
            selector: s.selector,
            public_dns_value: s.public_dns_value,
        })
        .collect();
    use fauna_mail::outbound::mta_sts::MtaStsLookup;
    let mta_sts_policy_fetch = match state.mta_sts_fetcher.lookup(&primary_domain).await {
        Ok(MtaStsLookup::Found(_)) => Ok(()),
        Ok(MtaStsLookup::NotPublished) => {
            Err("MTA-STS not published (no _mta-sts TXT)".to_string())
        }
        Ok(MtaStsLookup::FetchError) => Err("policy file fetch failed (DNS/network)".to_string()),
        Ok(MtaStsLookup::Invalid) => Err("policy file invalid (RFC 8461 §3.2)".to_string()),
        Err(e) => Err(format!("policy fetch error: {e}")),
    };
    Ok(Some(crate::mail_deliverability::DiagnosticInput {
        primary_domain,
        helo_name,
        outbound_ip,
        dkim_selectors,
        mta_sts_policy_fetch,
    }))
}

fn run_deliverability_diagnostics_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.run_deliverability_diagnostics",
            )
            .await?;
            let _req: RunDeliverabilityDiagnosticsRequest = decode(&payload).map_err(malformed)?;
            let reply = run_and_record_diagnostics(&state, &actor_id).await?;
            encode_reply(&reply)
        })
    })
}

/// The `ran_by_actor_id` a timer-driven diagnostics run records: all zeros, the
/// nest itself rather than an admin.
const SCHEDULED_DIAGNOSTICS_RAN_BY: [u8; 32] = [0u8; 32];

/// Run the deliverability diagnostics and persist the audit row — the one path
/// both the admin's `run_deliverability_diagnostics` and the 24 h self-check
/// tick take, so the mail health readout's `records_failing` input always has a
/// fresh row (`mail-deliverability.md` § The mail health readout).
async fn run_and_record_diagnostics(
    state: &Arc<AppState>,
    ran_by: &[u8; 32],
) -> Result<RunDeliverabilityDiagnosticsReply, RpcError> {
    let ran_at = now_epoch_secs();
    let checks = match gather_diagnostic_input(state).await? {
        Some(input) => {
            let resolver = state.dns_verifier.resolver();
            crate::mail_deliverability::run_diagnostics(
                resolver.as_ref(),
                state.starttls_prober.as_ref(),
                &input,
            )
            .await
        }
        None => vec![DiagnosticCheckResult {
            name: "Mail enabled".into(),
            status: "fail".into(),
            detail: "no primary mail domain configured (enable mail first)".into(),
        }],
    };
    // Audit row (best-effort: a persist failure must not fail the run).
    if let Ok(json) = serde_json::to_string(&checks)
        && let Err(e) = state.db.record_diagnostic_run(ran_at, &json, ran_by).await
    {
        tracing::warn!(target: "mail_deliverability", "diagnostic run: persist failed: {e}");
    }
    Ok(RunDeliverabilityDiagnosticsReply { checks, ran_at })
}

fn blocklist_self_check_run_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.blocklist_self_check_run").await?;
            let req: BlocklistSelfCheckRunRequest = decode(&payload).map_err(malformed)?;
            let checked_at = now_epoch_secs();

            // Outbound IP = A/AAAA of mail.<primary>.
            let primary = state
                .db
                .list_active_mail_domains()
                .await
                .map_err(internal)?
                .into_iter()
                .find(|d| d.is_primary);
            let ip = match &primary {
                Some(p) => {
                    let helo = format!("mail.{}", p.domain_name);
                    let resolver = state.dns_verifier.resolver();
                    crate::mail_deliverability::resolve_outbound_ip(resolver.as_ref(), &helo).await
                }
                None => None,
            };
            let ip_str = ip.map(|x| x.to_string()).unwrap_or_default();

            // Default DNSBL set, filtered to a single server on the force-refresh
            // path; each server is force-refresh rate-limited (1/min).
            let requested: Vec<String> =
                fauna_mail::deliverability::DEFAULT_BLOCKLIST_SELF_CHECK_SERVERS
                    .iter()
                    .map(|s| s.to_string())
                    .filter(|s| req.server.as_deref().is_none_or(|only| only == s))
                    .collect();
            let mut allowed = Vec::new();
            let mut limited = Vec::new();
            for s in requested {
                if crate::mail_deliverability::force_refresh_allowed(&s, checked_at) {
                    allowed.push(s);
                } else {
                    limited.push(s);
                }
            }
            let resolver = state.dns_verifier.resolver();
            let mut results = crate::mail_deliverability::run_blocklist_self_check(
                resolver.as_ref(),
                ip,
                &allowed,
                None,
            )
            .await;
            for server in limited {
                results.push(BlocklistServerResult {
                    server,
                    listed: false,
                    reason: String::new(),
                    error: "rate-limited (max once per minute per DNSBL)".into(),
                });
            }
            if let Ok(json) = serde_json::to_string(&results) {
                let _ = state
                    .db
                    .record_blocklist_self_check(checked_at, &json)
                    .await;
            }
            encode_reply(&BlocklistSelfCheckRunReply {
                checked_at,
                outbound_ip: ip_str,
                results,
            })
        })
    })
}

/// Map the db-layer warm-up snapshot onto the wire reply (identical fields;
/// shared by the status + reset handlers).
fn warmup_status_reply(s: crate::db::mail_warmup::WarmupStatus) -> OutboundWarmupStatusReply {
    OutboundWarmupStatusReply {
        current_day: s.current_day,
        today_used: s.today_used,
        today_max: s.today_max,
        ramp_end_date: s.ramp_end_date,
        lifetime_total: s.lifetime_total,
        first_outbound_at: s.first_outbound_at,
        last_reset_at: s.last_reset_at,
    }
}

/// `fauna.bridges.outbound_warmup_status` (Admin) — read the deployment-wide
/// fresh-IP warm-up state (`mail-deliverability.md` § Fresh-IP warm-up
/// § Wire shapes). Read-only; uses the same `outbound_now` clock seam as the
/// submission-time enforcement so `current_day` agrees.
fn outbound_warmup_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.outbound_warmup_status").await?;
            let _req: OutboundWarmupStatusRequest = decode(&payload).map_err(malformed)?;
            let status = state
                .db
                .read_warmup_status(state.outbound_now())
                .await
                .map_err(internal)?;
            encode_reply(&warmup_status_reply(status))
        })
    })
}

/// `fauna.bridges.outbound_warmup_reset` (Admin) — restart the warm-up ramp at
/// day 1 after a deployment IP change (`mail-deliverability.md` § Manual reset).
/// Preserves the lifetime counter; returns the post-reset state.
fn outbound_warmup_reset_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.outbound_warmup_reset").await?;
            let _req: OutboundWarmupResetRequest = decode(&payload).map_err(malformed)?;
            let status = state
                .db
                .reset_warmup(state.outbound_now())
                .await
                .map_err(internal)?;
            encode_reply(&warmup_status_reply(status))
        })
    })
}

/// Gather the nest facts the mail health fold reads
/// (`mail-deliverability.md` § The mail health readout — the states table's
/// Source column): `mail_enabled`, the bridge-connection projection, the latest
/// self-check and diagnostics rows, the stalled-queue count and the warm-up
/// state.
async fn gather_mail_health_inputs(
    state: &Arc<AppState>,
) -> anyhow::Result<fauna_mail::health::HealthInputs> {
    use fauna_mail::health::{BlocklistEntry, DiagnosticEntry, HealthInputs, queue_row_stalled};

    // The bridge-connection projection: every approved out-of-process MTA/MDA
    // service user, and whether it holds a live WS connection (the raw fact
    // `WsState::has_connections` holds for its ed25519 actor id).
    let mail_bridges: Vec<_> = state
        .db
        .list_approved_bridge_service_users()
        .await?
        .into_iter()
        .filter(|b| {
            use crate::db::bridge_service_users::BridgeRole;
            matches!(b.role, BridgeRole::Mta | BridgeRole::Mda) && !b.in_process
        })
        .collect();
    let connected = mail_bridges
        .iter()
        .filter(|b| state.ws.has_connections(&b.ed25519_pubkey))
        .count();

    // Stored `results_json` is always the serialized verdict `Vec`; a malformed
    // row degrades to an empty list (which reads as nothing listed / failing).
    let blocklist = state
        .db
        .latest_blocklist_self_check()
        .await?
        .map(|(_, json)| {
            serde_json::from_str::<Vec<BlocklistServerResult>>(&json)
                .unwrap_or_default()
                .into_iter()
                .map(|r| BlocklistEntry {
                    errored: !r.error.is_empty(),
                    server: r.server,
                    listed: r.listed,
                })
                .collect()
        });
    let diagnostics = state.db.latest_diagnostic_run().await?.map(|(_, json)| {
        serde_json::from_str::<Vec<DiagnosticCheckResult>>(&json)
            .unwrap_or_default()
            .into_iter()
            .map(|c| DiagnosticEntry {
                name: c.name,
                status: c.status,
            })
            .collect()
    });

    // `queue_stalled` reuses the outbound policy's delayed-delivery warning
    // age — no new threshold, no new knob — on the same clock the queue's
    // `created_at` was stamped with.
    let now = state.outbound_now();
    let delay_warning_at_hours = state
        .db
        .get_outbound_policy()
        .await?
        .effective()
        .delay_warning_at_hours;
    let stalled_outbound = state
        .db
        .list_failed_pending_outbound()
        .await?
        .into_iter()
        .filter(|(attempts, created_at)| {
            queue_row_stalled(*attempts, *created_at, now, delay_warning_at_hours)
        })
        .count();

    let warmup = state.db.read_warmup_status(now).await?;

    Ok(HealthInputs {
        mail_enabled: state.db.effective_mail_enabled().await?,
        mail_bridges_approved: mail_bridges.len() as u32,
        mail_bridges_connected: connected as u32,
        blocklist,
        stalled_outbound: stalled_outbound as u32,
        diagnostics,
        warmup_day: warmup.current_day.max(1) as u32,
        warmup_capped: warmup.today_max.is_some(),
    })
}

/// `fauna.bridges.mail_health` (Admin) — the mail health readout
/// (`mail-deliverability.md` § The mail health readout): gathers the nest facts,
/// folds them through the shared `fauna_mail::health::evaluate`, and returns the
/// categorical state, the seven check rows, the two heartbeat stamps and the
/// de-listing URL. Read-only.
fn mail_health_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.mail_health").await?;
            let _req: MailHealthRequest = decode(&payload).map_err(malformed)?;
            let inputs = gather_mail_health_inputs(&state).await.map_err(internal)?;
            let report = fauna_mail::health::evaluate(&inputs);
            let heartbeats = state.db.read_mail_heartbeats().await.map_err(internal)?;
            encode_reply(&MailHealthReply {
                state: report.state.as_str().to_string(),
                checks: report
                    .checks
                    .into_iter()
                    .map(|c| MailHealthCheck {
                        label_key: c.label_key.to_string(),
                        state: c.state.as_str().to_string(),
                        detail: c.detail,
                        ..Default::default()
                    })
                    .collect(),
                last_outbound_delivered_at: heartbeats.last_outbound_delivered_at,
                last_inbound_accepted_at: heartbeats.last_inbound_accepted_at,
                delist_url: report.delist_url.map(str::to_string),
                ..Default::default()
            })
        })
    })
}

/// `fauna.bridges.list_blocklist_self_check_history` (Admin) — the persisted
/// blocklist-self-check history within `window_days` (≤ 0 ⇒ the full 90-day
/// retention), newest first (`mail-deliverability.md` § Wire shapes
/// § Admin-visible audit). Each row's stored `results_json` is parsed back
/// into the structured per-DNSBL verdicts so the client renders typed rows.
fn list_blocklist_self_check_history_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.list_blocklist_self_check_history",
            )
            .await?;
            let req: ListBlocklistSelfCheckHistoryRequest = decode(&payload).map_err(malformed)?;
            let window_days = if req.window_days <= 0 {
                90
            } else {
                req.window_days
            };
            let since = now_epoch_secs() - window_days * 86_400;
            let raw = state
                .db
                .list_blocklist_self_check_history(since)
                .await
                .map_err(internal)?;
            let rows = raw
                .into_iter()
                .map(|(checked_at, json)| {
                    // Stored `results_json` is always
                    // `serde_json::to_string(&Vec<BlocklistServerResult>)`; a
                    // malformed row degrades to an empty verdict list (the
                    // audit timestamp is load-bearing — never drop the row).
                    let results = serde_json::from_str(&json).unwrap_or_default();
                    BlocklistSelfCheckHistoryRow {
                        checked_at,
                        results,
                    }
                })
                .collect();
            encode_reply(&ListBlocklistSelfCheckHistoryReply { rows })
        })
    })
}

/// `fauna.bridges.list_deliverability_diagnostic_runs` (Admin) — the diagnostic-
/// run audit (`mail-deliverability.md` § Admin-visible audit), newest first,
/// capped at `limit` (≤ 0 ⇒ 100, max 1000). Stored `results_json` is parsed back
/// into the structured checklist.
fn list_deliverability_diagnostic_runs_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.list_deliverability_diagnostic_runs",
            )
            .await?;
            let req: ListDeliverabilityDiagnosticRunsRequest =
                decode(&payload).map_err(malformed)?;
            let limit = if req.limit <= 0 {
                100
            } else {
                req.limit.min(1000)
            };
            let raw = state
                .db
                .list_deliverability_diagnostic_runs(limit)
                .await
                .map_err(internal)?;
            let rows = raw
                .into_iter()
                .map(|(ran_at, json, actor)| {
                    // Stored `results_json` is always
                    // `serde_json::to_string(&Vec<DiagnosticCheckResult>)`;
                    // a malformed row degrades to an empty checklist.
                    let checks = serde_json::from_str(&json).unwrap_or_default();
                    DiagnosticRunHistoryRow {
                        ran_at,
                        checks,
                        ran_by_actor_id: ByteBuf::from(actor.to_vec()),
                    }
                })
                .collect();
            encode_reply(&ListDeliverabilityDiagnosticRunsReply { rows })
        })
    })
}

/// The 24h-timer blocklist self-check (`mail-deliverability.md` § Blocklist
/// self-check — default 03:00 UTC ± offset, default-on). Resolves the outbound
/// IP, sweeps the default DNSBL set (not force-refresh-rate-limited — this IS
/// the cadence), persists the row, and prunes history older than 90 days.
pub async fn run_scheduled_blocklist_self_check(state: &Arc<AppState>) {
    let checked_at = now_epoch_secs();
    let primary = match state.db.list_active_mail_domains().await {
        Ok(domains) => domains.into_iter().find(|d| d.is_primary),
        Err(e) => {
            tracing::warn!(target: "mail_deliverability", "blocklist self-check: list domains failed: {e}");
            return;
        }
    };
    let Some(primary) = primary else {
        // Mail not enabled — nothing to check.
        return;
    };
    let helo = format!("mail.{}", primary.domain_name);
    let resolver = state.dns_verifier.resolver();
    let ip = crate::mail_deliverability::resolve_outbound_ip(resolver.as_ref(), &helo).await;
    let servers: Vec<String> = fauna_mail::deliverability::DEFAULT_BLOCKLIST_SELF_CHECK_SERVERS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let results =
        crate::mail_deliverability::run_blocklist_self_check(resolver.as_ref(), ip, &servers, None)
            .await;
    if let Ok(json) = serde_json::to_string(&results)
        && let Err(e) = state
            .db
            .record_blocklist_self_check(checked_at, &json)
            .await
    {
        tracing::warn!(target: "mail_deliverability", "blocklist self-check: persist failed: {e}");
    }
    // 90-day retention for both blocklist + diagnostic history.
    let cutoff = checked_at - 90 * 24 * 3600;
    if let Err(e) = state.db.prune_deliverability_history(cutoff).await {
        tracing::warn!(target: "mail_deliverability", "deliverability history prune failed: {e}");
    }
    let listed: Vec<&str> = results
        .iter()
        .filter(|r| r.listed)
        .map(|r| r.server.as_str())
        .collect();
    if !listed.is_empty() {
        tracing::warn!(target: "mail_deliverability", "outbound IP listed on DNSBL(s): {}", listed.join(", "));
    }
    // One diagnostics run piggybacks on this same daily tick — no separate or
    // more frequent timer — so the mail health readout's `records_failing`
    // input always has a fresh row (`mail-deliverability.md` § The mail health
    // readout). Runs after the prune so today's row is never the one pruned.
    if let Err(e) = run_and_record_diagnostics(state, &SCHEDULED_DIAGNOSTICS_RAN_BY).await {
        tracing::warn!(target: "mail_deliverability", "scheduled diagnostics run failed: {e:?}");
    }
}

/// The daily `spam_training_history` retention sweep (`mail-spam.md`
/// § Training-sample retention). Prunes per-message training-audit rows older
/// than the admin-effective Tier-2 `mail.spam.training_history_retention_days`
/// (default 30) — the learned n-gram weights persist in `spam_models`; only
/// the per-event undo trail ages out, so after the window a user can no longer
/// undo that specific event (reset-and-retrain is their resort). Reads the
/// effective retention from the overlaid spam policy, so an admin's override
/// takes effect on the next sweep. No-ops cheaply (a `DELETE … WHERE
/// created_at < cutoff`) when nothing is stale or no history exists.
pub async fn run_spam_training_history_gc(state: &Arc<AppState>) {
    let retention_days = match state.db.get_spam_policy().await {
        Ok(o) => o.effective().training_history_retention_days,
        Err(e) => {
            tracing::warn!(target: "mail_spam", "spam history GC: read retention policy failed: {e}");
            return;
        }
    };
    // `created_at` is `now_epoch_millis()`; the retention is in days.
    let cutoff_ms = crate::db::now_epoch_millis() - (retention_days as i64) * 24 * 3600 * 1000;
    match state.db.gc_spam_training_history(cutoff_ms).await {
        Ok(pruned) if pruned > 0 => {
            tracing::info!(target: "mail_spam", "spam history GC: pruned {pruned} rows older than {retention_days}d");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(target: "mail_spam", "spam history GC: prune failed: {e}");
        }
    }
}

// ── Registration entry point ──────────────────────────────────

pub fn register_bridge_routing_handlers(b: &mut RpcRouterBuilder) {
    // Deliverability diagnostics + blocklist self-check (Admin, on-demand).
    b.add(
        "fauna.bridges.run_deliverability_diagnostics",
        RpcKindMeta {
            // Synchronous DNS + STARTTLS probes (5–30s); read-only, replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(45),
            handler: run_deliverability_diagnostics_handler(),
        },
    );
    b.add(
        "fauna.bridges.blocklist_self_check_run",
        RpcKindMeta {
            // Force-refresh DNSBL sweep; rate-limited per-DNSBL in the handler.
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: blocklist_self_check_run_handler(),
        },
    );
    b.add(
        "fauna.bridges.outbound_warmup_status",
        RpcKindMeta {
            // Read-only deployment-wide warm-up state; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: outbound_warmup_status_handler(),
        },
    );
    b.add(
        "fauna.bridges.outbound_warmup_reset",
        RpcKindMeta {
            // Idempotent reset (sets first_outbound_at = now, day = 1);
            // replay-safe (a double-reset within the same second is a no-op-ish
            // re-stamp, never destructive — lifetime_total is preserved).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: outbound_warmup_reset_handler(),
        },
    );
    b.add(
        "fauna.bridges.mail_health",
        RpcKindMeta {
            // Read-only fold over nest state; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mail_health_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_blocklist_self_check_history",
        RpcKindMeta {
            // Read-only history over the persisted blocklist-self-check rows.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_blocklist_self_check_history_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_deliverability_diagnostic_runs",
        RpcKindMeta {
            // Read-only diagnostic-run audit history; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_deliverability_diagnostic_runs_handler(),
        },
    );
    b.add(
        "fauna.bridges.validate_recipient",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: validate_recipient_handler(),
        },
    );
    b.add(
        "fauna.bridges.resolve_recipient",
        RpcKindMeta {
            // Read-only resolve (decrements/hits are A2.3+); replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: resolve_recipient_handler(),
        },
    );
    b.add(
        "fauna.bridges.check_greylist",
        RpcKindMeta {
            // Idempotent-on-retry envelope check (the SMTP retry IS the
            // mechanism); replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: check_greylist_handler(),
        },
    );
    // Per-account alias user surface (A2.1).
    b.add(
        "fauna.bridges.list_account_aliases",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_account_aliases_handler(),
        },
    );
    b.add(
        "fauna.bridges.create_account_alias",
        RpcKindMeta {
            // Server-enforced UNIQUE(local_domain, pattern, kind): an
            // auto-retry replay surfaces as a spurious conflict, so the
            // caller must re-decide. Mirrors `add_follow`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: create_account_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.import_account_aliases",
        RpcKindMeta {
            // Inserts new rows under UNIQUE(local_domain, pattern, kind); an
            // auto-retry replay would surface spurious conflicts — same as
            // create_account_alias.
            forbid_replay: true,
            // A batch can be ~100 rows; give it more headroom than the single
            // create (5s).
            default_deadline: Duration::from_secs(10),
            handler: import_account_aliases_handler(),
        },
    );
    b.add(
        "fauna.bridges.update_account_alias",
        RpcKindMeta {
            // Idempotent full-overwrite (same payload twice → same state).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: update_account_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.revoke_account_alias",
        RpcKindMeta {
            // Idempotent (already-disabled is a no-op).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: revoke_account_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.enable_account_alias",
        RpcKindMeta {
            // Idempotent (already-enabled is a no-op).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: enable_account_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_account_alias",
        RpcKindMeta {
            // Idempotent (already-deleted → not_found, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_account_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.generate_disposable_alias",
        RpcKindMeta {
            // Each mint creates a new row with a fresh token (not idempotent);
            // an auto-retry replay would mint a duplicate against the per-day
            // cap, so the caller re-decides. Mirrors `create_account_alias`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: generate_disposable_alias_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_account_alias_hits",
        RpcKindMeta {
            // Read-only audit list (idempotent, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_account_alias_hits_handler(),
        },
    );
    // Admin external forwarders (mail-aliases.md § Kind 7 / § AF).
    b.add(
        "fauna.bridges.create_forwarder",
        RpcKindMeta {
            // Each create inserts a new row (not idempotent); a replay would
            // hit the UNIQUE conflict, so the caller re-decides. Mirrors
            // `create_account_alias`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: create_forwarder_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_forwarders",
        RpcKindMeta {
            // Read-only enumeration (idempotent, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_forwarders_handler(),
        },
    );
    b.add(
        "fauna.bridges.delete_forwarder",
        RpcKindMeta {
            // Idempotent (already-deleted → not_found, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_forwarder_handler(),
        },
    );
    // Per-account forward-all user surface (N1).
    b.add(
        "fauna.bridges.get_forward_all_to",
        RpcKindMeta {
            // Read-only (idempotent, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_forward_all_to_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_forward_all_to",
        RpcKindMeta {
            // Idempotent full-overwrite (same payload twice → same state).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_forward_all_to_handler(),
        },
    );
    // Per-account forward rate cap — Tier 3 under the admin ceiling
    // (`mail-forwarding.md` § Per-account forward rate-limit).
    b.add(
        "fauna.bridges.get_forward_per_hour",
        RpcKindMeta {
            // Read-only (idempotent, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_forward_per_hour_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_forward_per_hour",
        RpcKindMeta {
            // Idempotent full-overwrite (same payload twice → same state).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_forward_per_hour_handler(),
        },
    );
    // Per-account spam-threshold override — Tier 3's middle tier
    // (`mail-aliases.md` § Spam-threshold override).
    b.add(
        "fauna.bridges.get_spam_threshold_override",
        RpcKindMeta {
            // Read-only (idempotent, replay-safe).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_spam_threshold_override_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_spam_threshold_override",
        RpcKindMeta {
            // Idempotent full-overwrite (same payload twice → same state).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_spam_threshold_override_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_recipient_mls_pubkey",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_recipient_mls_pubkey_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_recipient_mls_pubkey",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: provision_recipient_mls_pubkey_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_recipient_index_key",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_recipient_index_key_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_config",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_config_handler(),
        },
    );
    b.add(
        // Admin read twin of `fetch_config` — hydrates the `admin-mail`
        // policy form with the overlaid effective config.
        "fauna.bridges.get_mail_config",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_mail_config_handler(),
        },
    );
    b.add(
        "fauna.bridges.whoami",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(2),
            handler: whoami_handler(),
        },
    );
    b.add(
        "fauna.bridges.report_session_close",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(2),
            handler: report_session_close_handler(),
        },
    );
    b.add(
        // `forbid_replay = true` (79th pass). The name reads like a query and
        // it inherited a *read* deadline constant, but the handler's only
        // effect is a consuming debit: `try_consume_submission_quota` does a
        // bare read-modify-write (`used = used + recipient_count`) keyed on
        // (actor, day_bucket) with **no dedup key**, so nothing collapses a
        // repeat. A replay charges the actor's daily allowance twice for one
        // outbound message, and the user's own legitimate mail is then refused
        // as over-quota at a volume they never sent. The day bucket is a
        // counter key, not a replay guard.
        //
        // This is deliberately NOT the `ingest_inbound_mail` shape: that kind
        // is content-addressed and dedups (see its declaration below), so its
        // retry is safe and forbidding it would trade a duplicate for mail
        // loss. Here there is nothing to dedup against, and the caller can
        // re-check safely once it knows the outcome is ambiguous — which is
        // exactly what `RpcDisconnected { was_in_flight: true }` tells it.
        // Hazard pin: `a_replayed_submission_quota_check_debits_the_allowance_twice`.
        "fauna.bridges.check_submission_quota",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(2),
            handler: check_submission_quota_handler(),
        },
    );
    b.add(
        "fauna.bridges.ingest_inbound_mail",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: ingest_inbound_mail_handler(),
        },
    );
    b.add(
        "fauna.bridges.submit_inbound_mail",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: submit_inbound_mail_handler(),
        },
    );
    b.add(
        "fauna.bridges.report_rejected_scan",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: report_rejected_scan_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_outbound_due",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_outbound_due_handler(),
        },
    );
    b.add(
        "fauna.bridges.mark_outbound_delivered",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mark_outbound_delivered_handler(),
        },
    );
    b.add(
        "fauna.bridges.mark_outbound_failed",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mark_outbound_failed_handler(),
        },
    );
    b.add(
        "fauna.bridges.mark_outbound_bounced",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mark_outbound_bounced_handler(),
        },
    );
    b.add(
        "fauna.bridges.enqueue_outbound_mail",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: enqueue_outbound_mail_handler(),
        },
    );
    // The MDA mailbox-less auto-schedule rail (caldav-server.md § Server-side
    // auto-schedule, C3). `forbid_replay: true` mirrors the reused
    // `welcome.deliver` / `channel.send` kinds — a recovered connection must not
    // auto-resend the sealed Welcome + iMIP (the inbox dedup makes a manual
    // retry idempotent, but the auto-retry path is suppressed for parity). 30 s
    // deadline matches `welcome.deliver` (the dominant cost — a same-nest push
    // or a cross-nest federation relay dial).
    b.add(
        "fauna.bridges.deliver_sealed_scheduling",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: deliver_sealed_scheduling_handler(),
        },
    );
    // N2 forward delivery trigger (MTA perimeter).
    b.add(
        "fauna.bridges.fetch_recipient_forward_config",
        RpcKindMeta {
            // Read-only per-recipient config lookup; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_recipient_forward_config_handler(),
        },
    );
    // T3.3 — per-recipient filter-rule fetch at the MTA perimeter.
    b.add(
        "fauna.bridges.fetch_recipient_filters",
        RpcKindMeta {
            // Read-only per-recipient rule lookup; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_recipient_filters_handler(),
        },
    );
    b.add(
        "fauna.bridges.forward_message",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: forward_message_handler(),
        },
    );
    // AutoReply (Sieve vacation): atomic rate-limit claim + null-sender enqueue.
    b.add(
        "fauna.bridges.send_auto_reply",
        RpcKindMeta {
            // forbid_replay:false so a retried send re-runs the atomic claim and
            // returns sent=false (slot already consumed) — the safe idempotent
            // outcome for vacation (skip-on-retry beats a replayed sent=true that
            // would enqueue a second reply).
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: send_auto_reply_handler(),
        },
    );
    b.add(
        "fauna.bridges.decode_srs_bounce",
        RpcKindMeta {
            // Read-only decode + verify of an inbound SRS recipient; replay-safe.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: decode_srs_bounce_handler(),
        },
    );
    // N5 — admin rotates the SRS secret (2-secret overlap). Mints a new secret
    // each call, so a replayed frame must not rotate twice.
    b.add(
        "fauna.bridges.rotate_srs_secret",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: rotate_srs_secret_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_mta_sts_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_mta_sts_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.fetch_tlsa",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: fetch_tlsa_handler(),
        },
    );
    b.add(
        "fauna.bridges.resolve_mx",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: resolve_mx_handler(),
        },
    );
    b.add(
        "fauna.bridges.report_tls_attempt",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: report_tls_attempt_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_spam_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_spam_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_auth_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_auth_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_submission_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_submission_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_imap_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_imap_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_outbound_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_outbound_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.put_alias_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: put_alias_policy_handler(),
        },
    );
    b.add(
        // Admin read twin of `put_alias_policy` — hydrates the `admin-mail`
        // alias-policy group (these knobs are not in `FetchConfigReply`).
        "fauna.bridges.get_alias_policy",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_alias_policy_handler(),
        },
    );
    b.add(
        "fauna.bridges.add_local_domain",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: add_local_domain_handler(),
        },
    );
    b.add(
        "fauna.bridges.remove_local_domain",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: remove_local_domain_handler(),
        },
    );
    b.add(
        "fauna.bridges.restore_local_domain",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: restore_local_domain_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_local_domains",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_local_domains_handler(),
        },
    );
    b.add(
        "fauna.bridges.update_local_domain_config",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: update_local_domain_config_handler(),
        },
    );
    // Primary-domain rename (mail-primary-domain-rename.md) — SLICE 1.
    b.add(
        "fauna.bridges.start_primary_domain_rename",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: start_primary_domain_rename_handler(),
        },
    );
    b.add(
        "fauna.bridges.get_primary_domain_rename_status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_primary_domain_rename_status_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_primary_domain_renames",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_primary_domain_renames_handler(),
        },
    );
    b.add(
        "fauna.bridges.abort_primary_domain_rename",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: abort_primary_domain_rename_handler(),
        },
    );
    b.add(
        "fauna.bridges.complete_primary_domain_rename",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: complete_primary_domain_rename_handler(),
        },
    );
    b.add(
        "fauna.bridges.extend_primary_domain_rename_grace",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: extend_primary_domain_rename_grace_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_catch_all_actor",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_catch_all_actor_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_role_address",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_role_address_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_dkim_rotation_days",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_dkim_rotation_days_handler(),
        },
    );
    b.add(
        "fauna.bridges.force_rotate_dkim",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: force_rotate_dkim_handler(),
        },
    );
    b.add(
        "fauna.bridges.provision_self_signed_cert",
        RpcKindMeta {
            forbid_replay: false,
            // Synthesis (rcgen keygen) + seal/fan-out to N approved bridges
            // (HPKE) — matches the 30 s provision-family deadline rather than
            // the 5 s read deadline.
            default_deadline: Duration::from_secs(30),
            handler: provision_self_signed_cert_handler(),
        },
    );
    b.add(
        "fauna.bridges.restore_real_tls_cert",
        RpcKindMeta {
            forbid_replay: false,
            // Local filesystem cert restore (+ optional cert removal to trigger
            // ACME) — fast; the 5 s read deadline is plenty.
            default_deadline: Duration::from_secs(5),
            handler: restore_real_tls_cert_handler(),
        },
    );
}

#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_approval_test_support::approve_bridge;
    use crate::db::mail_domain_renames::RenameTransitionError;
    use crate::db::{CacheDb, bridge_service_users::BridgeRole};
    use fauna_protocol::bridge_routing::ForwardCopyMode;
    use fauna_protocol::bridge_routing::SubmissionPolicyThresholds;
    use fauna_protocol::bridge_routing::{DmarcMode, DmarcOverrides};

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    async fn fixture_state_with_node_mode(mode: crate::config::NodeMode) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        Arc::new(AppState::for_test_with_node_mode(db, mode))
    }

    async fn add_admin(db: &CacheDb, pk: &[u8; 32]) {
        db.add_admin_actor(&pk[..]).await.unwrap();
    }

    // ── DKIM emergency rotation (force_rotate_dkim) ──────────────────
    // mail-multidomain.md § Rotation: flip a domain's active selector
    // (mail_domains.dkim_selector) to its newest-provisioned selector, skipping
    // the scheduled wait. The bridge then rebuilds its DKIM set on config_changed
    // (proven in the Go bridge tests) and signs with the new selector.

    #[tokio::test]
    async fn force_rotate_dkim_flips_active_selector_to_newest_provisioned() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain(
                "fauna.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();

        // The current "default" key, then (after the clock advances) a
        // freshly-rotated "202606" key — the newest-provisioned selector.
        state
            .db
            .seat_dkim_selector_for_test("fauna.test", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        state
            .db
            .seat_dkim_selector_for_test("fauna.test", "202606", "v=DKIM1; k=ed25519; p=NEW")
            .await;

        let req = ForceRotateDkimRequest {
            domain: "fauna.test".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = force_rotate_dkim_handler()(state.clone(), admin, payload)
            .await
            .expect("force_rotate ok");
        let reply: ForceRotateDkimReply = decode(&reply_bytes).unwrap();

        // The active selector flipped to the newest-provisioned one — both on the
        // reply row and persisted (what the dkim_selectors projection reads).
        assert_eq!(reply.domain.dkim_selector.as_deref(), Some("202606"));
        let row = state
            .db
            .lookup_active_mail_domain("fauna.test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.dkim_selector.as_deref(), Some("202606"));
    }

    #[tokio::test]
    async fn force_rotate_dkim_errors_when_no_newer_selector() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain(
                "fauna.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        // Only the active selector is provisioned → nothing newer to rotate to.
        state
            .db
            .seat_dkim_selector_for_test("fauna.test", "default", "v=DKIM1; k=ed25519; p=X")
            .await;

        let req = ForceRotateDkimRequest {
            domain: "fauna.test".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = force_rotate_dkim_handler()(state.clone(), admin, payload)
            .await
            .expect_err("must error when no newer selector is provisioned");
        assert_eq!(err.code, "fauna.bridges.no_dkim_selector_to_rotate");
        // Active selector unchanged.
        let row = state
            .db
            .lookup_active_mail_domain("fauna.test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.dkim_selector.as_deref(), Some("default"));
    }

    #[tokio::test]
    async fn force_rotate_dkim_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        state
            .db
            .add_mail_domain(
                "fauna.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        let req = ForceRotateDkimRequest {
            domain: "fauna.test".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = force_rotate_dkim_handler()(state.clone(), non_admin, payload)
            .await
            .expect_err("non-admin must be rejected");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── DKIM scheduled-rotation due-detection signal (mail-multidomain.md
    //    § Rotation): the flip stamps the activation time + the list projection
    //    surfaces `dkim_rotation_due` to the admin client.

    #[tokio::test]
    async fn force_rotate_dkim_reply_carries_activation_stamp() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain(
                "fauna.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        state
            .db
            .seat_dkim_selector_for_test("fauna.test", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        state
            .db
            .seat_dkim_selector_for_test("fauna.test", "202606", "v=DKIM1; k=ed25519; p=NEW")
            .await;

        let req = ForceRotateDkimRequest {
            domain: "fauna.test".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: ForceRotateDkimReply = decode(
            &force_rotate_dkim_handler()(state.clone(), admin, payload)
                .await
                .expect("force_rotate ok"),
        )
        .unwrap();
        // The flip stamped the activation time, projected onto the wire row.
        assert!(
            reply.domain.dkim_selector_activated_at.is_some(),
            "the selector flip must stamp dkim_selector_activated_at"
        );
    }

    #[tokio::test]
    async fn list_local_domains_projects_dkim_rotation_due() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("fresh.test", true, "testing", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .add_mail_domain("due.test", false, "testing", "expand_primary", None, None)
            .await
            .unwrap();
        // Accelerate `due.test` to rotation_days = 0 via the DB-API tri-state
        // (no wire surface exposes it; DKIM rotation is automatic, no manual UI).
        state
            .db
            .update_mail_domain_config(
                "due.test",
                crate::db::mail_domains::MailDomainUpdate {
                    dkim_rotation_days: Some(Some(0)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let payload = Bytes::from(
            encode_canonical(&ListLocalDomainsRequest {})
                .unwrap()
                .to_vec(),
        );
        let reply: ListLocalDomainsReply = decode(
            &list_local_domains_handler()(state.clone(), admin, payload)
                .await
                .expect("list ok"),
        )
        .unwrap();

        let due = reply
            .active
            .iter()
            .find(|r| r.domain_name == "due.test")
            .expect("due.test present");
        let fresh = reply
            .active
            .iter()
            .find(|r| r.domain_name == "fresh.test")
            .expect("fresh.test present");
        assert!(due.dkim_rotation_due, "accelerated domain must signal due");
        assert!(
            !fresh.dkim_rotation_due,
            "a freshly-added domain must not be due under the quarterly default"
        );
    }

    #[tokio::test]
    async fn run_scheduled_dkim_autoflip_respects_24h_cache_window() {
        let state = fixture_state().await;
        state.db.add_admin_actor(&[7u8; 32]).await.unwrap();
        state
            .db
            .add_mail_domain(
                "due.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        // Make it due (rotation_days = 0 via the DB-API tri-state).
        state
            .db
            .update_mail_domain_config(
                "due.test",
                crate::db::mail_domains::MailDomainUpdate {
                    dkim_rotation_days: Some(Some(0)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // Provision the active key + a newer selector, both just now (< 24h).
        state
            .db
            .seat_dkim_selector_for_test("due.test", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        state
            .db
            .seat_dkim_selector_for_test("due.test", "202606", "v=DKIM1; k=ed25519; p=NEW")
            .await;

        // Scheduled pass: the domain is due, but the newer selector has NOT aged
        // past the 24h peer-cache window → no auto-flip; the active selector stays.
        // (The aged-past-window flip itself is covered by the
        // flip_to_newest_dkim_selector unit test via min_age, without backdating.)
        run_scheduled_dkim_autoflip(&state).await;
        let row = state
            .db
            .lookup_active_mail_domain("due.test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.dkim_selector.as_deref(),
            Some("default"),
            "a due domain whose newer selector is < 24h old must not auto-flip yet"
        );
    }

    /// Set up an approved MTA service-user with an attested X25519 — the seal
    /// target the rotation-mint (and provision-on-read) require. Returns the
    /// X25519 *secret* so a test can unseal the minted blob to prove custody.
    async fn approve_mta_with_x25519(state: &Arc<AppState>) -> [u8; 32] {
        use crate::db::bridge_service_users::BridgeRole;
        use fauna_mls::wrapped_blob::generate_x25519_keypair;
        let (x_secret, x_public) = generate_x25519_keypair();
        let ed = [0x55u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&ed, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        state.db.upsert_bridge_x25519(&ed, &x_public).await.unwrap();
        state
            .db
            .approve_bridge_service_user(&ed, Some(&[7u8; 32]))
            .await
            .unwrap();
        x_secret
    }

    /// A due domain gets a fresh `<YYYYMM>` key beside its active selector's,
    /// once per rotation.
    #[tokio::test]
    async fn run_scheduled_dkim_rotation_mint_mints_nest_held_for_due_domain() {
        let state = fixture_state().await;
        state.db.add_admin_actor(&[7u8; 32]).await.unwrap();
        approve_mta_with_x25519(&state).await;

        // A due domain: newest == active.
        state
            .db
            .add_mail_domain(
                "mint.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        state
            .db
            .seat_dkim_selector_for_test("mint.test", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        state
            .db
            .update_mail_domain_config(
                "mint.test",
                crate::db::mail_domains::MailDomainUpdate {
                    dkim_rotation_days: Some(Some(0)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        run_scheduled_dkim_rotation_mint(&state).await;

        let expected =
            crate::db::mail_domains::dkim_rotation_selector(crate::db::now_epoch_millis());
        assert_ne!(
            expected, "default",
            "the rotation selector must be a fresh <YYYYMM>"
        );
        let selectors = state
            .db
            .list_dkim_selectors(Some("mint.test"))
            .await
            .unwrap();
        let minted = selectors
            .last()
            .filter(|s| s.selector == expected)
            .expect("a fresh <YYYYMM> selector must be minted, and listed newest-last");
        assert!(
            minted.public_dns_value.starts_with("v=DKIM1; k=ed25519;"),
            "minted selector carries an Ed25519 public DNS value: {}",
            minted.public_dns_value
        );

        // Idempotent across the warmup window: a second pass mints nothing new
        // (newest is now the fresh selector ≠ active `default`).
        run_scheduled_dkim_rotation_mint(&state).await;
        let after = state
            .db
            .list_dkim_selectors(Some("mint.test"))
            .await
            .unwrap();
        assert_eq!(
            after.len(),
            2,
            "re-running the mint must not provision a second rotation key"
        );
    }

    /// The rotation mint needs no approved MTA: the key is the nest's own, so
    /// the new selector's record exists whether or not a bridge is enrolled.
    #[tokio::test]
    async fn run_scheduled_dkim_rotation_mint_needs_no_mta() {
        let state = fixture_state().await;
        // Due domain, holding the nest-held `default` key minted when it was
        // added — and NO bridge enrolled at all.
        state
            .db
            .add_mail_domain(
                "nomta.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        state
            .db
            .update_mail_domain_config(
                "nomta.test",
                crate::db::mail_domains::MailDomainUpdate {
                    dkim_rotation_days: Some(Some(0)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        run_scheduled_dkim_rotation_mint(&state).await;
        let selectors: Vec<String> = state
            .db
            .list_dkim_selectors(Some("nomta.test"))
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.selector)
            .collect();
        assert_eq!(
            selectors,
            [
                "default".to_string(),
                crate::db::mail_domains::dkim_rotation_selector(crate::db::now_epoch_millis()),
            ],
            "with no MTA enrolled the rotation still mints its nest-held selector"
        );
    }

    #[tokio::test]
    async fn run_scheduled_dkim_rotation_mint_then_flip_composes() {
        let state = fixture_state().await;
        state.db.add_admin_actor(&[7u8; 32]).await.unwrap();
        approve_mta_with_x25519(&state).await;
        state
            .db
            .add_mail_domain(
                "compose.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        state
            .db
            .seat_dkim_selector_for_test("compose.test", "default", "v=DKIM1; k=ed25519; p=OLD")
            .await;
        state
            .db
            .update_mail_domain_config(
                "compose.test",
                crate::db::mail_domains::MailDomainUpdate {
                    dkim_rotation_days: Some(Some(0)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Mint the rotation selector, then flip to it (min_age 0 = the same
        // emergency/force-rotate path; the 24 h-gated scheduled flip is covered by
        // `run_scheduled_dkim_autoflip_respects_24h_cache_window`). Proves the
        // minted selector is a valid flip target → the active signing selector
        // advances to the nest-minted key.
        run_scheduled_dkim_rotation_mint(&state).await;
        let expected =
            crate::db::mail_domains::dkim_rotation_selector(crate::db::now_epoch_millis());
        let row = state
            .db
            .lookup_active_mail_domain("compose.test")
            .await
            .unwrap()
            .unwrap();
        let flipped = state
            .db
            .flip_to_newest_dkim_selector(&row, 0)
            .await
            .unwrap();
        assert_eq!(
            flipped
                .expect("flip to the minted selector")
                .dkim_selector
                .as_deref(),
            Some(expected.as_str()),
            "the active selector flips onto the nest-minted rotation key"
        );

        // After the flip the activation stamp is fresh → the domain is no longer
        // due, so a subsequent mint pass is a no-op (no churn).
        run_scheduled_dkim_rotation_mint(&state).await;
        let selectors = state
            .db
            .list_dkim_selectors(Some("compose.test"))
            .await
            .unwrap();
        assert_eq!(
            selectors.len(),
            2,
            "no further selector minted once rotation completed"
        );
    }

    // ── MTA-STS lookup → wire mapping (T2.1a) ───────────────────────

    #[test]
    fn lookup_to_reply_not_published() {
        use fauna_mail::outbound::mta_sts::MtaStsLookup;
        let reply = lookup_to_reply(MtaStsLookup::NotPublished);
        assert_eq!(reply.outcome, "not_published");
        assert!(reply.policy.is_none());
    }

    #[test]
    fn lookup_to_reply_fetch_error() {
        use fauna_mail::outbound::mta_sts::MtaStsLookup;
        let reply = lookup_to_reply(MtaStsLookup::FetchError);
        assert_eq!(reply.outcome, "fetch_error");
        assert!(reply.policy.is_none());
    }

    #[test]
    fn lookup_to_reply_invalid() {
        use fauna_mail::outbound::mta_sts::MtaStsLookup;
        let reply = lookup_to_reply(MtaStsLookup::Invalid);
        assert_eq!(reply.outcome, "invalid");
        assert!(reply.policy.is_none());
    }

    #[test]
    fn lookup_to_reply_found_maps_all_fields() {
        use fauna_mail::outbound::mta_sts::{
            FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsPolicy,
        };
        let reply = lookup_to_reply(MtaStsLookup::Found(FetchedPolicy {
            id: "20240101T000000".into(),
            policy: MtaStsPolicy {
                version: "STSv1".into(),
                mode: MtaStsMode::Enforce,
                mx: vec!["mx1.example.com".into(), "*.example.net".into()],
                max_age_secs: 604800,
            },
        }));
        assert_eq!(reply.outcome, "found");
        let policy = reply.policy.expect("found ⇒ policy present");
        assert_eq!(policy.id, "20240101T000000");
        assert_eq!(policy.mode, "enforce");
        assert_eq!(policy.mx, vec!["mx1.example.com", "*.example.net"]);
        assert_eq!(policy.max_age_secs, 604800);
    }

    #[test]
    fn lookup_to_reply_found_testing_and_none_modes() {
        use fauna_mail::outbound::mta_sts::{
            FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsPolicy,
        };
        for (mode, expected) in [(MtaStsMode::Testing, "testing"), (MtaStsMode::None, "none")] {
            let reply = lookup_to_reply(MtaStsLookup::Found(FetchedPolicy {
                id: "id1".into(),
                policy: MtaStsPolicy {
                    version: "STSv1".into(),
                    mode,
                    mx: vec!["mx.example.com".into()],
                    max_age_secs: 86400,
                },
            }));
            assert_eq!(reply.policy.expect("policy present").mode, expected);
        }
    }

    /// The MTA-STS outcome makes a **round trip through the other binary**:
    /// `lookup_to_reply` writes it on `fetch_mta_sts_policy`, the Go bridge
    /// carries it, and `mta_sts_lookup_from_wire` reads it back on
    /// `report_tls_attempt` to rebuild the RFC 8460 §4.4 TLSRPT policy bucket.
    ///
    /// Until 2026-08-23 the two halves were two hand-written, independently
    /// inverted tables and **nothing asserted they agreed** — each half's own
    /// tests spelled the literals themselves, so a rename on one side plus its
    /// own tests was a green commit that silently mis-attributed every TLSRPT
    /// bucket. Both halves now ask `MtaStsOutcome` / `MtaStsMode`; this pins
    /// that they compose, which is the property those tables were supposed to
    /// have. The cross-*language* half of the same claim lives in
    /// `libs/fauna-mail/tests/go_wire_outcome_contract.rs`.
    #[test]
    fn every_mta_sts_lookup_survives_the_wire_round_trip() {
        use fauna_mail::outbound::mta_sts::{
            FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsPolicy,
        };
        let found = |mode: MtaStsMode| {
            MtaStsLookup::Found(FetchedPolicy {
                id: "20240101T000000".into(),
                policy: MtaStsPolicy {
                    version: "STSv1".into(),
                    mode,
                    mx: vec!["mx1.example.com".into(), "*.example.net".into()],
                    max_age_secs: 604800,
                },
            })
        };
        for lookup in [
            MtaStsLookup::NotPublished,
            MtaStsLookup::FetchError,
            MtaStsLookup::Invalid,
            found(MtaStsMode::Enforce),
            found(MtaStsMode::Testing),
            found(MtaStsMode::None),
        ] {
            let reply = lookup_to_reply(lookup.clone());
            let back = mta_sts_lookup_from_wire(&reply.outcome, reply.policy.as_ref())
                .unwrap_or_else(|e| {
                    panic!("{lookup:?} serialized to {reply:?}, which nest rejects on the way back: {e:?}")
                });
            assert_eq!(
                back, lookup,
                "the write and read halves of the MTA-STS wire vocabulary disagree"
            );
        }
    }

    // ── T2.1b tlsa_to_reply mapping tests ──────────────────────────

    #[test]
    fn tlsa_to_reply_empty_is_empty() {
        let reply = tlsa_to_reply(vec![]);
        assert!(reply.records.is_empty());
    }

    #[test]
    fn tlsa_to_reply_maps_dane_record_fields() {
        use fauna_mail::outbound::dane::TlsaRecord;
        let reply = tlsa_to_reply(vec![TlsaRecord {
            usage: 3,
            selector: 1,
            matching: 1,
            data: vec![0xde, 0xad, 0xbe, 0xef],
        }]);
        assert_eq!(reply.records.len(), 1);
        let r = &reply.records[0];
        assert_eq!((r.usage, r.selector, r.matching), (3, 1, 1));
        assert_eq!(r.data, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn tlsa_to_reply_drops_pkix_class_records() {
        use fauna_mail::outbound::dane::TlsaRecord;
        // Usage 0 (PKIX-TA) + 1 (PKIX-EE) are unsupported → filtered out, so
        // a host publishing only those yields an empty reply (MTA-STS /
        // opportunistic fallback, not an undeliverable hard-fail). The
        // DANE-EE (3) record survives.
        let reply = tlsa_to_reply(vec![
            TlsaRecord {
                usage: 0,
                selector: 0,
                matching: 1,
                data: vec![1; 32],
            },
            TlsaRecord {
                usage: 1,
                selector: 0,
                matching: 1,
                data: vec![2; 32],
            },
            TlsaRecord {
                usage: 3,
                selector: 0,
                matching: 1,
                data: vec![3; 32],
            },
        ]);
        assert_eq!(reply.records.len(), 1);
        assert_eq!(reply.records[0].usage, 3);
    }

    fn add_req(domain: &str) -> Bytes {
        let req = AddLocalDomainRequest {
            domain: domain.into(),
            mta_sts_cert_mode: "expand_primary".into(),
            catch_all_actor: None,
            dkim_selector_override: None,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    /// Drain a bridge WS receiver and collect the `reason` of every
    /// `fauna.bridges.config_changed` push currently queued. Shared by the
    /// config_changed hot-reload tests.
    pub(crate) fn drain_config_changed_reasons(
        rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) -> Vec<String> {
        let mut reasons = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            if let Ok(fauna_protocol::Frame::Push(p)) = fauna_protocol::decode_frame(&bytes)
                && p.kind == fauna_protocol::bridge_routing::PUSH_KIND_BRIDGE_CONFIG_CHANGED
            {
                let payload_bytes = encode_canonical(&p.payload).unwrap();
                let push: fauna_protocol::bridge_routing::BridgeConfigChangedPush =
                    decode(&payload_bytes).unwrap();
                reasons.push(push.reason);
            }
        }
        reasons
    }

    // ── config_changed hot-reload push ─────────
    // The fetch_config-surface admin mutations (local-domain edits + the
    // put_<substruct>_policy edits) emit a `fauna.bridges.config_changed`
    // push to every approved bridge so a running bridge re-fetches
    // fetch_config without a restart. Per
    // `docs/goal/behavior/mail-bridge-lifecycle.md` § Running.

    #[tokio::test]
    async fn put_spam_policy_emits_config_changed_to_approved_bridges() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        // Two approved bridges (MTA + MDA), each holding a live WS connection.
        let mta = [0x11u8; 32];
        let mda = [0x12u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let (_cm, mut rx_mta) = state.ws.subscribe(mta);
        let (_cd, mut rx_mda) = state.ws.subscribe(mda);

        let req = PutSpamPolicyRequest {
            max_score_before_spam_folder: Some(6),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        put_spam_policy_handler()(state.clone(), admin, payload)
            .await
            .expect("put_spam_policy ok");

        assert_eq!(
            drain_config_changed_reasons(&mut rx_mta),
            vec!["spam_policy".to_string()],
            "MTA bridge must receive a config_changed push"
        );
        assert_eq!(
            drain_config_changed_reasons(&mut rx_mda),
            vec!["spam_policy".to_string()],
            "MDA bridge must receive the same push (it re-fetches its own role config)"
        );
    }

    #[tokio::test]
    async fn each_put_policy_emits_its_reason() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let mta = [0x21u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let (_c, mut rx) = state.ws.subscribe(mta);

        let cases: Vec<(&str, Bytes)> = vec![
            (
                "auth_policy",
                Bytes::from(
                    encode_canonical(&PutAuthPolicyRequest::default())
                        .unwrap()
                        .to_vec(),
                ),
            ),
            (
                "submission_policy",
                Bytes::from(
                    encode_canonical(&PutSubmissionPolicyRequest::default())
                        .unwrap()
                        .to_vec(),
                ),
            ),
            (
                "imap_policy",
                Bytes::from(
                    encode_canonical(&PutImapPolicyRequest::default())
                        .unwrap()
                        .to_vec(),
                ),
            ),
            (
                "outbound_policy",
                Bytes::from(
                    encode_canonical(&PutOutboundPolicyRequest::default())
                        .unwrap()
                        .to_vec(),
                ),
            ),
        ];
        let handlers: Vec<(&str, RpcHandler)> = vec![
            ("auth_policy", put_auth_policy_handler()),
            ("submission_policy", put_submission_policy_handler()),
            ("imap_policy", put_imap_policy_handler()),
            ("outbound_policy", put_outbound_policy_handler()),
        ];
        for ((reason, payload), (_, handler)) in cases.into_iter().zip(handlers) {
            handler(state.clone(), admin, payload)
                .await
                .unwrap_or_else(|e| panic!("{reason} handler failed: {e:?}"));
            assert_eq!(
                drain_config_changed_reasons(&mut rx),
                vec![reason.to_string()],
                "{reason} must emit config_changed with reason={reason}"
            );
        }
    }

    #[tokio::test]
    async fn get_mail_config_reads_back_overlaid_overrides() {
        // The admin read twin returns the same overlaid effective config the
        // bridge's `fetch_config` would — an admin's `put_spam_policy` override
        // is visible on the very next `get_mail_config`, so the `admin-mail`
        // form hydrates from real persisted state (not blind catalog defaults).
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        // Fresh nest: get_mail_config returns the catalog defaults verbatim.
        let fresh = get_mail_config_handler()(state.clone(), admin, empty_get_mail_config())
            .await
            .expect("get_mail_config ok (fresh)");
        let fresh: FetchConfigReply = decode(&fresh).expect("decode FetchConfigReply");
        assert_eq!(
            fresh.spam.max_score_before_spam_folder,
            SpamPolicyThresholds::default().max_score_before_spam_folder,
            "a fresh nest reads back the catalog default"
        );

        // Admin writes a spam override (raise the auto-Junk threshold to 6).
        let put = PutSpamPolicyRequest {
            max_score_before_spam_folder: Some(6),
            ..Default::default()
        };
        let put_payload = Bytes::from(encode_canonical(&put).unwrap().to_vec());
        put_spam_policy_handler()(state.clone(), admin, put_payload)
            .await
            .expect("put_spam_policy ok");

        // get_mail_config now reflects the override; untouched fields stay default.
        let after = get_mail_config_handler()(state.clone(), admin, empty_get_mail_config())
            .await
            .expect("get_mail_config ok (after put)");
        let after: FetchConfigReply = decode(&after).expect("decode FetchConfigReply");
        assert_eq!(
            after.spam.max_score_before_spam_folder, 6,
            "the override must be visible on the next admin read"
        );
        assert_eq!(
            after.spam.max_score_before_reject,
            SpamPolicyThresholds::default().max_score_before_reject,
            "a field the admin did not set stays at the catalog default"
        );
    }

    #[tokio::test]
    async fn get_mail_config_reads_back_bayesian_overrides() {
        // The Tier-2 per-user `mail.spam.bayesian_*` + retention knobs overlay
        // onto the effective config the same way the perimeter tiers do, so
        // the MDA scorer (via fetch_config) + nest (via get_spam_policy) see an
        // admin's override. Catalog rows in `mail-policy-config.md` § Spam.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        let put = PutSpamPolicyRequest {
            bayesian_weight_milli: Some(900),
            bayesian_min_samples: Some(40),
            bayesian_full_confidence_samples: Some(300),
            training_history_retention_days: Some(14),
            ..Default::default()
        };
        let put_payload = Bytes::from(encode_canonical(&put).unwrap().to_vec());
        put_spam_policy_handler()(state.clone(), admin, put_payload)
            .await
            .expect("put_spam_policy ok");

        let after = get_mail_config_handler()(state.clone(), admin, empty_get_mail_config())
            .await
            .expect("get_mail_config ok");
        let after: FetchConfigReply = decode(&after).expect("decode FetchConfigReply");
        assert_eq!(after.spam.bayesian_weight_milli, 900);
        assert_eq!(after.spam.bayesian_min_samples, 40);
        assert_eq!(after.spam.bayesian_full_confidence_samples, 300);
        assert_eq!(after.spam.training_history_retention_days, 14);
        // The nest-side effective read agrees with the projection.
        let eff = state.db.get_spam_policy().await.unwrap().effective();
        assert_eq!(eff.bayesian_full_confidence_samples, 300);
        assert_eq!(eff.training_history_retention_days, 14);
    }

    #[tokio::test]
    async fn put_spam_policy_rejects_inverted_confidence_ramp() {
        // The confidence ramp spans [min, full]; an effective
        // `full_confidence_samples <= min_samples` collapses/inverts it — a
        // config error the handler rejects on the effective values.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        // full (100) < min (150) → rejected.
        let bad = PutSpamPolicyRequest {
            bayesian_min_samples: Some(150),
            bayesian_full_confidence_samples: Some(100),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&bad).unwrap().to_vec());
        let err = put_spam_policy_handler()(state.clone(), admin, payload)
            .await
            .expect_err("inverted ramp must be rejected");
        assert!(
            format!("{err:?}").contains("bayesian"),
            "error should name the bayesian knobs, got {err:?}"
        );

        // full == min is also rejected (zero-width ramp). min defaults to 50.
        let equal = PutSpamPolicyRequest {
            bayesian_full_confidence_samples: Some(50),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&equal).unwrap().to_vec());
        put_spam_policy_handler()(state.clone(), admin, payload)
            .await
            .expect_err("equal min==full must be rejected");

        // A valid raise (full 300 > min 40) is accepted.
        let ok = PutSpamPolicyRequest {
            bayesian_min_samples: Some(40),
            bayesian_full_confidence_samples: Some(300),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&ok).unwrap().to_vec());
        put_spam_policy_handler()(state.clone(), admin, payload)
            .await
            .expect("valid ramp accepted");
    }

    #[tokio::test]
    async fn run_spam_training_history_gc_prunes_by_configured_retention() {
        // The daily sweep reads the admin-effective
        // `mail.spam.training_history_retention_days` and prunes older rows.
        // A fresh row survives a 30-day retention; lowering retention to 0
        // (cutoff = now) prunes it on the next sweep (`mail-spam.md`
        // § Training-sample retention).
        let state = fixture_state().await;
        let actor = [55u8; 32];
        let msg = [56u8; 32];
        state
            .db
            .put_spam_model_with_history(
                &actor,
                &[0xEEu8; 300],
                Some(crate::db::moderation::SpamHistoryDbOp::Insert {
                    message_id: &msg,
                    mailbox: "INBOX",
                    sealed_subject: &[0x22u8; 16],
                    sealed_delta: &[0x33u8; 16],
                    label: "spam",
                    source: "manual_other",
                }),
                None,
            )
            .await
            .expect("insert history row");
        assert_eq!(
            state
                .db
                .list_spam_training_history(&actor, 10, None)
                .await
                .unwrap()
                .len(),
            1
        );

        // Default 30-day retention: the fresh row survives.
        run_spam_training_history_gc(&state).await;
        assert_eq!(
            state
                .db
                .list_spam_training_history(&actor, 10, None)
                .await
                .unwrap()
                .len(),
            1,
            "a row well inside the 30-day window must not be pruned"
        );

        // Lower retention to 0 (cutoff = now). A tiny sleep guarantees the row's
        // created_at is strictly older than the sweep instant.
        state
            .db
            .put_spam_policy(crate::db::mail_policy::SpamPolicyOverrides {
                training_history_retention_days: Some(0),
                ..Default::default()
            })
            .await
            .expect("set retention=0");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        run_spam_training_history_gc(&state).await;
        assert_eq!(
            state
                .db
                .list_spam_training_history(&actor, 10, None)
                .await
                .unwrap()
                .len(),
            0,
            "retention=0 prunes any row created before the sweep"
        );
    }

    fn empty_get_mail_config() -> Bytes {
        Bytes::from(
            encode_canonical(&GetMailConfigRequest::default())
                .unwrap()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn local_domain_mutations_emit_config_changed() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let mta = [0x31u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let (_c, mut rx) = state.ws.subscribe(mta);

        // add → emits.
        add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        assert_eq!(
            drain_config_changed_reasons(&mut rx),
            vec!["local_domains".to_string()]
        );

        // idempotent re-add → NO emit (nothing changed).
        add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        assert!(
            drain_config_changed_reasons(&mut rx).is_empty(),
            "a skipped idempotent re-add must not emit config_changed"
        );

        // add a removable second domain, then remove → emits.
        add_local_domain_handler()(state.clone(), admin, add_req("removable.example"))
            .await
            .unwrap();
        let _ = drain_config_changed_reasons(&mut rx); // clear the add's push
        let rm = Bytes::from(
            encode_canonical(&RemoveLocalDomainRequest {
                domain: "removable.example".into(),
            })
            .unwrap()
            .to_vec(),
        );
        remove_local_domain_handler()(state.clone(), admin, rm)
            .await
            .unwrap();
        assert_eq!(
            drain_config_changed_reasons(&mut rx),
            vec!["local_domains".to_string()]
        );
    }

    #[tokio::test]
    async fn config_changed_noop_when_no_bridge_connected() {
        // An approved bridge with no live WS connection (and a deployment
        // with zero bridges) must not error or panic on a mutation — the
        // push is best-effort.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        put_spam_policy_handler()(
            state,
            admin,
            Bytes::from(
                encode_canonical(&PutSpamPolicyRequest::default())
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("mutation succeeds even with no connected bridge");
    }

    // ── local-domain admin WS-RPC surface (§ A1) ──

    #[tokio::test]
    async fn add_local_domain_creates_primary_then_lists() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        // A domainless-booted box (fixture default) has the `localhost` identity
        // fallback until a domain is registered.
        assert_eq!(
            state.handle_domain(),
            "localhost",
            "a domainless box's identity is the localhost fallback before any add"
        );

        let reply_bytes = add_local_domain_handler()(state.clone(), admin, add_req("example.com"))
            .await
            .expect("add ok");
        let reply: AddLocalDomainReply = decode(&reply_bytes).unwrap();
        assert!(!reply.skipped);
        assert_eq!(reply.domain.domain_name, "example.com");
        assert!(reply.domain.is_primary, "first domain must be primary");
        assert_eq!(reply.domain.dkim_algorithms, vec!["ed25519", "rsa-2048"]);

        // Adding the FIRST (primary) domain from a client sets the deployment
        // identity apex (the domainless-then-add-domain flow → `handle_domain()`
        // follows it → ACME orders for it). Regression pin for the gap where
        // `add_local_domain` wrote the primary row but never called
        // `apply_primary_identity` (domains-and-tls-bootstrap.md § Claim).
        assert_eq!(
            state.handle_domain(),
            "example.com",
            "adding the first (primary) domain must set the deployment identity apex"
        );

        // Second domain is non-primary.
        let r2 = add_local_domain_handler()(state.clone(), admin, add_req("two.example"))
            .await
            .expect("add ok");
        let r2: AddLocalDomainReply = decode(&r2).unwrap();
        assert!(!r2.domain.is_primary);

        // A 2nd, non-primary add must NOT reassign the identity.
        assert_eq!(
            state.handle_domain(),
            "example.com",
            "a non-primary add must not reassign the deployment identity"
        );

        let list_req = Bytes::from(
            encode_canonical(&ListLocalDomainsRequest {})
                .unwrap()
                .to_vec(),
        );
        let list_bytes = list_local_domains_handler()(state, admin, list_req)
            .await
            .expect("list ok");
        let list: ListLocalDomainsReply = decode(&list_bytes).unwrap();
        assert_eq!(list.active.len(), 2);
        // Primary first per the DB ORDER BY.
        assert_eq!(list.active[0].domain_name, "example.com");
        assert!(list.soft_deleted_within_30d.is_empty());
    }

    #[tokio::test]
    async fn add_local_domain_idempotent_returns_skipped() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        let _ = add_local_domain_handler()(state.clone(), admin, add_req("example.com"))
            .await
            .unwrap();
        let again = add_local_domain_handler()(state, admin, add_req("example.com"))
            .await
            .unwrap();
        let again: AddLocalDomainReply = decode(&again).unwrap();
        assert!(again.skipped, "re-add of an active domain must be skipped");
        assert_eq!(again.domain.domain_name, "example.com");
    }

    #[tokio::test]
    async fn add_local_domain_requires_admin() {
        let state = fixture_state().await;
        let stranger = [200u8; 32]; // not admin, not a service user → User
        let err = add_local_domain_handler()(state, stranger, add_req("example.com"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn add_local_domain_refuses_userinfo_and_never_stores_it() {
        // Storage happened before the identity-only guard
        // (`resolve_handle_domain(...).is_public_dns_name`), so a malformed
        // domain reached `mail_domains` even though it could never become the
        // identity. The syntax check must run before `add_mail_domain`.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        let err = add_local_domain_handler()(
            state.clone(),
            admin,
            add_req("nest.example.com@attacker.example"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        let list = state.db.list_active_mail_domains().await.unwrap();
        assert!(
            list.is_empty(),
            "the userinfo-carrying domain must never reach mail_domains"
        );
    }

    // ── primary-domain rename (mail-primary-domain-rename.md) — SLICE 1 ──

    /// Seed a `mail_domains` row directly (side-effect-free, unlike the
    /// `add_local_domain` handler) and return its 16-byte id.
    async fn seed_domain(
        db: &CacheDb,
        name: &str,
        is_primary: bool,
        mta_sts_mode: &str,
        cert_mode: &str,
    ) -> [u8; 16] {
        db.add_mail_domain(name, is_primary, mta_sts_mode, cert_mode, None, None)
            .await
            .unwrap()
            .domain_id
    }

    fn start_req_bytes(new_id: &[u8; 16], grace_days: Option<i64>) -> Bytes {
        Bytes::from(
            encode_canonical(&StartPrimaryDomainRenameRequest {
                new_primary_domain_id: ByteBuf::from(new_id.to_vec()),
                grace_days,
            })
            .unwrap()
            .to_vec(),
        )
    }

    fn status_req_bytes() -> Bytes {
        Bytes::from(
            encode_canonical(&GetPrimaryDomainRenameStatusRequest {})
                .unwrap()
                .to_vec(),
        )
    }

    async fn get_status(state: &Arc<AppState>, admin: [u8; 32]) -> Option<MailDomainRenameRow> {
        let bytes =
            get_primary_domain_rename_status_handler()(state.clone(), admin, status_req_bytes())
                .await
                .expect("status ok");
        decode::<GetPrimaryDomainRenameStatusReply>(&bytes)
            .unwrap()
            .rename
    }

    #[tokio::test]
    async fn start_happy_path_advances_to_cert_issuance() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;

        // Default grace (None → 7).
        let bytes = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&new_id, None),
        )
        .await
        .expect("start ok");
        let reply: StartPrimaryDomainRenameReply = decode(&bytes).unwrap();
        // SLICE 2: `start` inserts `requested` then auto-advances to
        // `cert_issuance` (and wakes the cert-lifecycle loop). No cert/DNS side
        // effect fires in the handler itself.
        assert_eq!(reply.rename.state, "cert_issuance");
        assert_eq!(reply.rename.grace_days, 7);
        assert_eq!(reply.rename.new_primary_domain_id.as_ref(), &new_id[..]);
        assert_eq!(reply.rename.initiated_by_actor_id.as_ref(), &admin[..]);

        // get_status now returns the in-flight rename.
        let status = get_status(&state, admin).await.expect("status present");
        assert_eq!(status.rename_id, reply.rename.rename_id);
    }

    #[tokio::test]
    async fn start_honors_explicit_grace_days() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let bytes = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&new_id, Some(14)),
        )
        .await
        .expect("start ok");
        let reply: StartPrimaryDomainRenameReply = decode(&bytes).unwrap();
        assert_eq!(reply.rename.grace_days, 14);
    }

    #[tokio::test]
    async fn start_rejects_invalid_grace_days() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        for bad in [0i64, 31] {
            let err = start_primary_domain_rename_handler()(
                state.clone(),
                admin,
                start_req_bytes(&new_id, Some(bad)),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.bridges.invalid_grace_days", "grace={bad}");
        }
    }

    #[tokio::test]
    async fn start_rejects_same_domain_for_rename() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let primary_id =
            seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&primary_id, None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.same_domain_for_rename");
    }

    #[tokio::test]
    async fn start_rejects_nonexistent_target() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&[42u8; 16], None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.new_primary_must_be_additional");
    }

    #[tokio::test]
    async fn start_rejects_removed_target() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let new_id = state
            .db
            .soft_delete_mail_domain("new.example")
            .await
            .unwrap()
            .domain_id;
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&new_id, None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.new_primary_must_be_additional");
    }

    #[tokio::test]
    async fn start_rejects_non_expand_primary_cert_mode() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id = seed_domain(&state.db, "new.example", false, "enforce", "per_host").await;
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&new_id, None),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code,
            "fauna.bridges.new_primary_cert_mode_must_be_expand_primary"
        );
    }

    #[tokio::test]
    async fn start_rejects_weaker_tls_posture() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        // New primary still in its `testing` window < old's `enforce` → refuse.
        let new_id =
            seed_domain(&state.db, "new.example", false, "testing", "expand_primary").await;
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&new_id, None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.new_primary_tls_posture_weaker");
    }

    #[tokio::test]
    async fn start_rejects_second_rename_in_flight() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let a = seed_domain(&state.db, "a.example", false, "enforce", "expand_primary").await;
        let b = seed_domain(&state.db, "b.example", false, "enforce", "expand_primary").await;
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&a, None))
            .await
            .expect("first start ok");
        let err =
            start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&b, None))
                .await
                .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.rename_already_in_progress");
    }

    #[tokio::test]
    async fn start_cert_san_limit_boundary() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        // 49 active domains (1 primary + 48 additionals) → post-rename SANs
        // 2 + 2×49 = 100 (== limit, allowed).
        seed_domain(
            &state.db,
            "primary.example",
            true,
            "enforce",
            "expand_primary",
        )
        .await;
        let mut ids = Vec::new();
        for i in 0..48u32 {
            ids.push(
                seed_domain(
                    &state.db,
                    &format!("d{i}.example"),
                    false,
                    "enforce",
                    "expand_primary",
                )
                .await,
            );
        }
        assert_eq!(state.db.list_active_mail_domains().await.unwrap().len(), 49);
        // At the boundary (n=49): allowed.
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&ids[0], None))
            .await
            .expect("start allowed at 100-SAN boundary");
        // Abort so a second start isn't blocked by the in-flight one.
        let active = get_status(&state, admin).await.unwrap();
        let abort_req = Bytes::from(
            encode_canonical(&AbortPrimaryDomainRenameRequest {
                rename_id: active.rename_id.clone(),
                abort_reason: None,
            })
            .unwrap()
            .to_vec(),
        );
        abort_primary_domain_rename_handler()(state.clone(), admin, abort_req)
            .await
            .expect("abort ok");
        // Add a 50th active domain → post-rename SANs 2 + 2×50 = 102 (> limit).
        seed_domain(&state.db, "d48.example", false, "enforce", "expand_primary").await;
        assert_eq!(state.db.list_active_mail_domains().await.unwrap().len(), 50);
        let err = start_primary_domain_rename_handler()(
            state.clone(),
            admin,
            start_req_bytes(&ids[0], None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.cert_san_limit_exceeded");
    }

    /// Start a rename and drive it (via the storage transitions) to `grace`,
    /// returning the 16-byte rename id. `old.example` is the primary,
    /// `new.example` the (already-seeded) new primary.
    async fn drive_to_grace(state: &Arc<AppState>, admin: [u8; 32], new_id: &[u8; 16]) -> [u8; 16] {
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(new_id, None))
            .await
            .expect("start ok");
        let rid = state
            .db
            .get_active_rename()
            .await
            .unwrap()
            .unwrap()
            .rename_id;
        state
            .db
            .mark_rename_cert_ready(&rid, "fp", 111)
            .await
            .unwrap();
        state.db.advance_rename_to_grace(&rid).await.unwrap();
        rid
    }

    fn complete_req_bytes(rename_id: &[u8; 16], force: Option<bool>) -> Bytes {
        Bytes::from(
            encode_canonical(&CompletePrimaryDomainRenameRequest {
                rename_id: ByteBuf::from(rename_id.to_vec()),
                force,
            })
            .unwrap()
            .to_vec(),
        )
    }

    fn extend_req_bytes(rename_id: &[u8; 16], additional_days: i64) -> Bytes {
        Bytes::from(
            encode_canonical(&ExtendPrimaryDomainRenameGraceRequest {
                rename_id: ByteBuf::from(rename_id.to_vec()),
                additional_days,
            })
            .unwrap()
            .to_vec(),
        )
    }

    #[tokio::test]
    async fn abort_from_grace_reflips_is_primary_back_and_aborts() {
        // SLICE 4: post-flip abort runs the atomic inverse — re-flips `is_primary`
        // back to the old primary + marks the rename `aborted`.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let old_id = seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let rid = drive_to_grace(&state, admin, &new_id).await;
        // Precondition: the flip happened — new.example is the primary.
        assert_eq!(
            state
                .db
                .lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            new_id
        );

        let abort_req = Bytes::from(
            encode_canonical(&AbortPrimaryDomainRenameRequest {
                rename_id: ByteBuf::from(rid.to_vec()),
                abort_reason: Some("rolling back".into()),
            })
            .unwrap()
            .to_vec(),
        );
        let reply: AbortPrimaryDomainRenameReply = decode(
            &abort_primary_domain_rename_handler()(state.clone(), admin, abort_req)
                .await
                .expect("abort from grace ok"),
        )
        .unwrap();
        assert_eq!(reply.rename.state, "aborted");

        // The inverse re-flip landed: old.example is the primary again; no active rename.
        assert_eq!(
            state
                .db
                .lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            old_id,
            "old primary restored"
        );
        assert!(
            state.db.get_active_rename().await.unwrap().is_none(),
            "rename is terminal"
        );
    }

    #[tokio::test]
    async fn complete_from_ready_to_complete_terminal() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let rid = drive_to_grace(&state, admin, &new_id).await;
        state
            .db
            .advance_rename_to_ready_to_complete(&rid)
            .await
            .unwrap();
        // No force needed from ready_to_complete.
        let reply: CompletePrimaryDomainRenameReply = decode(
            &complete_primary_domain_rename_handler()(
                state.clone(),
                admin,
                complete_req_bytes(&rid, None),
            )
            .await
            .expect("complete ok"),
        )
        .unwrap();
        assert_eq!(reply.rename.state, "completed");
        assert!(state.db.get_active_rename().await.unwrap().is_none());
        // Complete doesn't touch is_primary — new stays primary.
        assert_eq!(
            state
                .db
                .lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_id,
            new_id
        );
    }

    #[tokio::test]
    async fn complete_from_grace_without_force_refused_with_force_ok() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let rid = drive_to_grace(&state, admin, &new_id).await;

        // Grace window not elapsed + no force → refused.
        let err = complete_primary_domain_rename_handler()(
            state.clone(),
            admin,
            complete_req_bytes(&rid, None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.grace_period_not_expired");
        // Still in grace.
        assert_eq!(
            state
                .db
                .get_active_rename()
                .await
                .unwrap()
                .unwrap()
                .parsed_state(),
            Some(fauna_mail::RenameState::Grace)
        );

        // force=true overrides.
        let reply: CompletePrimaryDomainRenameReply = decode(
            &complete_primary_domain_rename_handler()(
                state.clone(),
                admin,
                complete_req_bytes(&rid, Some(true)),
            )
            .await
            .expect("forced complete ok"),
        )
        .unwrap();
        assert_eq!(reply.rename.state, "completed");
    }

    #[tokio::test]
    async fn complete_wrong_state_returns_409_not_500() {
        // a `complete` on a pre-flip
        // (`cert_issuance`) row is a wrong-state refusal — it must surface as the
        // 409-class `rename_wrong_state`, never a `500 internal`. The typed store
        // error carries the mapping (no string match).
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        // `start` auto-advances the row to `cert_issuance` (pre-flip).
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&new_id, None))
            .await
            .expect("start ok");
        let rid = state
            .db
            .get_active_rename()
            .await
            .unwrap()
            .unwrap()
            .rename_id;
        assert_eq!(
            state
                .db
                .get_active_rename()
                .await
                .unwrap()
                .unwrap()
                .parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance),
        );

        let err = complete_primary_domain_rename_handler()(
            state.clone(),
            admin,
            complete_req_bytes(&rid, None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.rename_wrong_state");

        // Same for `extend` on a pre-flip row.
        let err = extend_primary_domain_rename_grace_handler()(
            state.clone(),
            admin,
            extend_req_bytes(&rid, 3),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.rename_wrong_state");
    }

    // `start`'s own `requested →
    // cert_issuance` auto-advance can lose the state-conditional CAS to a
    // coincident cert-lifecycle loop tick driving the identical transition. The
    // handler must treat that spurious `WrongState` as the idempotent success it
    // is (the rename IS in `cert_issuance`) rather than returning a 409 for an op
    // that succeeded. The natural sub-ms race isn't deterministically reproducible
    // here (the advance's precondition read and its CAS use the *same* `requested`
    // predicate, so — unlike the pre-flip-set abort CAS — no seam-free state
    // passes the read but fails the UPDATE), so we exercise the reconciliation
    // decision logic directly against real rows.

    /// A CAS-race `WrongState` reconciles to idempotent success when the re-read
    /// shows the row already advanced to `cert_issuance` (the coincident loop tick
    /// won the same advance).
    #[tokio::test]
    async fn start_autoadvance_reconciles_raced_cert_issuance_to_success() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let old_id = seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        // A real row a coincident loop tick would leave: inserted + advanced.
        let inserted = state
            .db
            .insert_domain_rename(&old_id, &new_id, 7, &admin)
            .await
            .unwrap();
        state
            .db
            .advance_rename_to_cert_issuance(&inserted.rename_id)
            .await
            .unwrap();
        let reread = state
            .db
            .lookup_rename_by_id(&inserted.rename_id)
            .await
            .unwrap();
        let raced = anyhow::Error::new(RenameTransitionError::WrongState {
            attempted: "advance to cert_issuance",
            actual: "changed concurrently".into(),
        });
        let row = reconcile_concurrent_cert_issuance_advance(raced, reread)
            .expect("raced advance reconciles to idempotent success");
        assert_eq!(row.rename_id, inserted.rename_id);
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance)
        );
    }

    /// A genuinely divergent race is NOT masked: if the row left `requested` for
    /// any state other than `cert_issuance` (here: a concurrent abort), the
    /// original `WrongState` propagates and still maps to a 409.
    #[tokio::test]
    async fn start_autoadvance_propagates_when_row_diverged() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let old_id = seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let inserted = state
            .db
            .insert_domain_rename(&old_id, &new_id, 7, &admin)
            .await
            .unwrap();
        // The row was aborted concurrently, not advanced.
        state
            .db
            .mark_rename_aborted(&inserted.rename_id, None)
            .await
            .unwrap();
        let reread = state
            .db
            .lookup_rename_by_id(&inserted.rename_id)
            .await
            .unwrap();
        let raced = anyhow::Error::new(RenameTransitionError::WrongState {
            attempted: "advance to cert_issuance",
            actual: "changed concurrently".into(),
        });
        let err = reconcile_concurrent_cert_issuance_advance(raced, reread)
            .expect_err("a diverged (aborted) row must not be masked as success");
        assert!(
            matches!(
                err.downcast_ref::<RenameTransitionError>(),
                Some(RenameTransitionError::WrongState { .. })
            ),
            "the original WrongState propagates (→ 409 rename_wrong_state); got: {err:#}"
        );
        // A vanished row likewise propagates rather than fabricating success.
        let gone = anyhow::Error::new(RenameTransitionError::WrongState {
            attempted: "advance to cert_issuance",
            actual: "changed concurrently".into(),
        });
        assert!(reconcile_concurrent_cert_issuance_advance(gone, None).is_err());
    }

    #[tokio::test]
    async fn extend_grace_pushes_deadline_via_handler() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let rid = drive_to_grace(&state, admin, &new_id).await;
        let before = state.db.get_active_rename().await.unwrap().unwrap();

        let reply: ExtendPrimaryDomainRenameGraceReply = decode(
            &extend_primary_domain_rename_grace_handler()(
                state.clone(),
                admin,
                extend_req_bytes(&rid, 4),
            )
            .await
            .expect("extend ok"),
        )
        .unwrap();
        assert_eq!(reply.rename.state, "grace");
        assert_eq!(
            reply.rename.grace_ends_at,
            Some(before.grace_ends_at.unwrap() + 4 * 86_400_000)
        );
    }

    #[tokio::test]
    async fn extend_grace_rejects_invalid_additional_days() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        let rid = drive_to_grace(&state, admin, &new_id).await;
        for bad in [0i64, 31] {
            let err = extend_primary_domain_rename_grace_handler()(
                state.clone(),
                admin,
                extend_req_bytes(&rid, bad),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.bridges.invalid_grace_days", "days={bad}");
        }
    }

    #[tokio::test]
    async fn remove_local_domain_refused_for_rename_participant() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let new_id =
            seed_domain(&state.db, "new.example", false, "enforce", "expand_primary").await;
        // Rename in flight (pre-flip cert_issuance) referencing new.example.
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&new_id, None))
            .await
            .expect("start ok");

        let remove_req = Bytes::from(
            encode_canonical(&RemoveLocalDomainRequest {
                domain: "new.example".into(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = remove_local_domain_handler()(state.clone(), admin, remove_req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.domain_in_rename_flight");
        // Still active.
        assert!(
            state
                .db
                .lookup_active_mail_domain("new.example")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn complete_requires_admin() {
        let state = fixture_state().await;
        let stranger = [200u8; 32];
        let err = complete_primary_domain_rename_handler()(
            state,
            stranger,
            complete_req_bytes(&[1u8; 16], None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn get_status_null_when_none() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        assert!(get_status(&state, admin).await.is_none());
    }

    #[tokio::test]
    async fn list_returns_all_including_aborted() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        seed_domain(&state.db, "old.example", true, "enforce", "expand_primary").await;
        let a = seed_domain(&state.db, "a.example", false, "enforce", "expand_primary").await;
        // Start then abort, then start again → 2 rows total (1 aborted, 1 active).
        let r1 = decode::<StartPrimaryDomainRenameReply>(
            &start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&a, None))
                .await
                .unwrap(),
        )
        .unwrap();
        let abort_req = Bytes::from(
            encode_canonical(&AbortPrimaryDomainRenameRequest {
                rename_id: r1.rename.rename_id.clone(),
                abort_reason: Some("oops".into()),
            })
            .unwrap()
            .to_vec(),
        );
        let aborted = decode::<AbortPrimaryDomainRenameReply>(
            &abort_primary_domain_rename_handler()(state.clone(), admin, abort_req)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(aborted.rename.state, "aborted");
        assert_eq!(aborted.rename.abort_reason.as_deref(), Some("oops"));
        start_primary_domain_rename_handler()(state.clone(), admin, start_req_bytes(&a, None))
            .await
            .expect("second start ok after abort");

        let list_bytes = list_primary_domain_renames_handler()(
            state.clone(),
            admin,
            Bytes::from(
                encode_canonical(&ListPrimaryDomainRenamesRequest {})
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("list ok");
        let list: ListPrimaryDomainRenamesReply = decode(&list_bytes).unwrap();
        assert_eq!(list.renames.len(), 2, "aborted + active both listed");
    }

    #[tokio::test]
    async fn abort_unknown_id_not_found() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let abort_req = Bytes::from(
            encode_canonical(&AbortPrimaryDomainRenameRequest {
                rename_id: ByteBuf::from(vec![9u8; 16]),
                abort_reason: None,
            })
            .unwrap()
            .to_vec(),
        );
        let err = abort_primary_domain_rename_handler()(state.clone(), admin, abort_req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn start_requires_admin() {
        let state = fixture_state().await;
        let stranger = [200u8; 32];
        let err = start_primary_domain_rename_handler()(
            state,
            stranger,
            start_req_bytes(&[1u8; 16], None),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── self-signed TLS cert provisioning ──
    // `AppState::for_test` installs `SealedStorage`, so `store_acme_material`
    // (the seal/fan-out + on-disk PEM write) succeeds in-process; these unit
    // tests cover the gate + partition logic, and the tier_3
    // `tests/api/test_mail_self_signed_cert.py` covers the kind over the real
    // socket with a committed storage mode.

    fn ssc_req(domain: &str, sans: &[&str]) -> Bytes {
        let req = ProvisionSelfSignedCertRequest {
            domain: domain.into(),
            additional_dns_sans: sans.iter().map(|s| s.to_string()).collect(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    // `provision_self_signed_cert` may wake
    // the ACME lifecycle only on a real→self-signed *transition*. The wake gate
    // paces only failed validations, so waking on every call would fire a fresh
    // *successful* re-issue each time and can approach LE's success-side
    // Duplicate-Certificate weekly limit.
    #[test]
    fn provision_should_wake_acme_only_on_real_to_self_signed_transition() {
        use crate::self_signed_cert::synthesize_self_signed_pem;
        // No prior cert on disk (missing/unreadable) → not "already self-signed"
        // → wake (ACME issues once on an ACME-on box; a no-op if ACME is off).
        assert!(provision_should_wake_acme(None));
        // Prior cert already self-signed → SKIP the wake: a repeat provision is a
        // no-op clobber and re-waking would fire a redundant successful re-issue.
        let (self_signed_pem, _key) =
            synthesize_self_signed_pem("example.com", vec!["example.com".into()]).unwrap();
        assert!(!provision_should_wake_acme(Some(
            self_signed_pem.as_bytes()
        )));
        // Unparseable prior cert → `pem_is_self_signed` errs toward "real", so we
        // wake (a real cert we just clobbered warrants a re-heal).
        assert!(provision_should_wake_acme(Some(b"not a pem at all")));
    }

    #[tokio::test]
    async fn provision_self_signed_cert_requires_admin() {
        let state = fixture_state().await;
        let stranger = [200u8; 32];
        let err =
            provision_self_signed_cert_handler()(state, stranger, ssc_req("example.com", &[]))
                .await
                .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn provision_self_signed_cert_unknown_domain_not_found() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let err = provision_self_signed_cert_handler()(state, admin, ssc_req("nope.example", &[]))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn provision_self_signed_cert_no_bridges_succeeds_with_90d_expiry() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        add_local_domain_handler()(state.clone(), admin, add_req("example.com"))
            .await
            .expect("add domain");

        let now = fauna_core::data::Timestamp::now_secs();
        let reply_bytes = provision_self_signed_cert_handler()(
            state,
            admin,
            ssc_req("example.com", &["mail.example.com"]),
        )
        .await
        .expect("provision ok");
        let reply: ProvisionSelfSignedCertReply = decode(&reply_bytes).unwrap();
        assert!(
            reply.bridges_sealed_to.is_empty(),
            "no approved bridges yet"
        );
        assert!(reply.bridges_skipped_no_x25519.is_empty());
        // ~90 days out (allow generous slack for the test clock).
        assert!(reply.expires_at_unix > now + 89 * 24 * 3600);
        assert!(reply.expires_at_unix < now + 91 * 24 * 3600);
    }

    #[tokio::test]
    async fn provision_self_signed_cert_partitions_sealed_and_skipped() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        add_local_domain_handler()(state.clone(), admin, add_req("example.com"))
            .await
            .expect("add domain");

        // An MTA approved WITH x25519 → sealable; an MDA approved WITHOUT one
        // → skipped (it hasn't attested its x25519 pubkey yet).
        let mta = [0x11u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let mda = [0x12u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&mda, BridgeRole::Mda, "mda-noattest")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&mda, None)
            .await
            .unwrap();

        let reply_bytes =
            provision_self_signed_cert_handler()(state, admin, ssc_req("example.com", &[]))
                .await
                .expect("provision ok");
        let reply: ProvisionSelfSignedCertReply = decode(&reply_bytes).unwrap();
        assert_eq!(
            reply.bridges_sealed_to.len(),
            1,
            "the x25519-attested MTA is sealed to"
        );
        assert_eq!(reply.bridges_sealed_to[0].role, "mta");
        assert_eq!(
            reply.bridges_skipped_no_x25519.len(),
            1,
            "the un-attested MDA is skipped"
        );
        assert_eq!(reply.bridges_skipped_no_x25519[0].role, "mda");
    }

    #[tokio::test]
    async fn remove_local_domain_refuses_primary() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();

        let req = Bytes::from(
            encode_canonical(&RemoveLocalDomainRequest {
                domain: "primary.example".into(),
            })
            .unwrap()
            .to_vec(),
        );
        let err = remove_local_domain_handler()(state, admin, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let details = format!("{:?}", err.details);
        assert!(
            details.contains("primary"),
            "details mention primary: {details}"
        );
    }

    #[tokio::test]
    async fn remove_then_restore_round_trip() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("removable.example"))
            .await
            .unwrap();

        let rm = Bytes::from(
            encode_canonical(&RemoveLocalDomainRequest {
                domain: "removable.example".into(),
            })
            .unwrap()
            .to_vec(),
        );
        let rm_reply = remove_local_domain_handler()(state.clone(), admin, rm)
            .await
            .expect("remove ok");
        let rm_reply: RemoveLocalDomainReply = decode(&rm_reply).unwrap();
        assert!(rm_reply.domain.removed_at.is_some());

        let restore = Bytes::from(
            encode_canonical(&RestoreLocalDomainRequest {
                domain: "removable.example".into(),
            })
            .unwrap()
            .to_vec(),
        );
        let restore_reply = restore_local_domain_handler()(state, admin, restore)
            .await
            .expect("restore ok");
        let restore_reply: RestoreLocalDomainReply = decode(&restore_reply).unwrap();
        assert!(restore_reply.domain.removed_at.is_none());
        assert!(restore_reply.domain.restored_at.is_some());
    }

    #[tokio::test]
    async fn update_local_domain_config_partial() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("cfg.example"))
            .await
            .unwrap();

        let req = Bytes::from(
            encode_canonical(&UpdateLocalDomainConfigRequest {
                domain: "cfg.example".into(),
                mta_sts_max_age_seconds: Some(604800),
                spf_record: Some("v=spf1 mx -all".into()),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        let reply = update_local_domain_config_handler()(state, admin, req)
            .await
            .expect("update ok");
        let reply: UpdateLocalDomainConfigReply = decode(&reply).unwrap();
        // The mode is no request field: an added domain is stored `testing`.
        assert_eq!(reply.domain.mta_sts_mode, "testing");
        assert_eq!(reply.domain.mta_sts_max_age_seconds, 604800);
        assert_eq!(reply.domain.spf_record, "v=spf1 mx -all");
        // Untouched field stays at its default.
        assert_eq!(reply.domain.mta_sts_cert_mode, "expand_primary");
    }

    #[tokio::test]
    async fn update_local_domain_config_unknown_domain_not_found() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let req = Bytes::from(
            encode_canonical(&UpdateLocalDomainConfigRequest {
                domain: "ghost.example".into(),
                dmarc_policy_mode: Some(DmarcMode::None),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        );
        let err = update_local_domain_config_handler()(state, admin, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn update_local_domain_config_softens_one_domains_dmarc_policy() {
        // dmarc-reporting.md § Multi-domain deployments: the select sets
        // `policy_mode` + `subdomain_policy_mode` together; the default clears
        // both; other domains and other override keys are untouched.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        for d in ["primary.example", "cfg.example"] {
            let _ = add_local_domain_handler()(state.clone(), admin, add_req(d))
                .await
                .unwrap();
        }
        let call = |mode: DmarcMode| {
            let state = state.clone();
            async move {
                let req = Bytes::from(
                    encode_canonical(&UpdateLocalDomainConfigRequest {
                        domain: "cfg.example".into(),
                        dmarc_policy_mode: Some(mode),
                        ..Default::default()
                    })
                    .unwrap()
                    .to_vec(),
                );
                let reply = update_local_domain_config_handler()(state, admin, req)
                    .await
                    .expect("update ok");
                decode::<UpdateLocalDomainConfigReply>(&reply)
                    .unwrap()
                    .domain
            }
        };

        let row = call(DmarcMode::Quarantine).await;
        assert_eq!(row.dmarc_overrides.policy_mode, Some(DmarcMode::Quarantine));
        assert_eq!(
            row.dmarc_overrides.subdomain_policy_mode,
            Some(DmarcMode::Quarantine)
        );
        let primary = state
            .db
            .lookup_active_mail_domain("primary.example")
            .await
            .unwrap()
            .unwrap();
        assert!(primary.dmarc_overrides_json.is_none());

        let row = call(DmarcMode::Reject).await;
        assert_eq!(row.dmarc_overrides, DmarcOverrides::default());
        let stored = state
            .db
            .lookup_active_mail_domain("cfg.example")
            .await
            .unwrap()
            .unwrap();
        assert!(stored.dmarc_overrides_json.is_none());
    }

    #[tokio::test]
    async fn set_catch_all_actor_designates_then_clears() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("cat.example"))
            .await
            .unwrap();

        let actor = [9u8; 32];
        // Designate the catch-all actor.
        let set = Bytes::from(
            encode_canonical(&SetCatchAllActorRequest {
                domain: "cat.example".into(),
                actor_id: Some(ByteBuf::from(actor.to_vec())),
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_catch_all_actor_handler()(state.clone(), admin, set)
            .await
            .expect("set ok");
        let reply: SetCatchAllActorReply = decode(&reply).unwrap();
        assert_eq!(reply.domain.domain_name, "cat.example");
        assert_eq!(
            reply.domain.catch_all_actor_id.as_deref(),
            Some(&actor.to_vec())
        );

        // Clearing (None) is distinct from setting — the field is a single
        // Option, so no Option<Option> null-collision.
        let clear = Bytes::from(
            encode_canonical(&SetCatchAllActorRequest {
                domain: "cat.example".into(),
                actor_id: None,
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_catch_all_actor_handler()(state.clone(), admin, clear)
            .await
            .expect("clear ok");
        let reply: SetCatchAllActorReply = decode(&reply).unwrap();
        assert!(reply.domain.catch_all_actor_id.is_none());
    }

    #[tokio::test]
    async fn set_dkim_rotation_days_sets_then_clears() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("rot.example"))
            .await
            .unwrap();

        // Set an accelerated per-domain override (0 = always due).
        let set = Bytes::from(
            encode_canonical(&SetDkimRotationDaysRequest {
                domain: "rot.example".into(),
                rotation_days: Some(0),
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_dkim_rotation_days_handler()(state.clone(), admin, set)
            .await
            .expect("set ok");
        let reply: SetDkimRotationDaysReply = decode(&reply).unwrap();
        assert_eq!(reply.domain.dkim_rotation_days, Some(0));
        // The set override makes the domain due (the lever the rotation-mint reads).
        let due = state.db.dkim_rotation_due_domains().await.unwrap();
        assert!(due.iter().any(|d| d.domain_name == "rot.example"));

        // Clearing (None) restores inherit-default → no longer due under quarterly.
        let clear = Bytes::from(
            encode_canonical(&SetDkimRotationDaysRequest {
                domain: "rot.example".into(),
                rotation_days: None,
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_dkim_rotation_days_handler()(state.clone(), admin, clear)
            .await
            .expect("clear ok");
        let reply: SetDkimRotationDaysReply = decode(&reply).unwrap();
        assert!(reply.domain.dkim_rotation_days.is_none());
        let due = state.db.dkim_rotation_due_domains().await.unwrap();
        assert!(!due.iter().any(|d| d.domain_name == "rot.example"));
    }

    #[tokio::test]
    async fn set_catch_all_actor_rejects_non_32_byte_actor() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let bad = Bytes::from(
            encode_canonical(&SetCatchAllActorRequest {
                domain: "primary.example".into(),
                actor_id: Some(ByteBuf::from(vec![1u8; 16])),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_catch_all_actor_handler()(state, admin, bad)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn set_catch_all_actor_unknown_domain_not_found() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let req = Bytes::from(
            encode_canonical(&SetCatchAllActorRequest {
                domain: "ghost.example".into(),
                actor_id: Some(ByteBuf::from(vec![9u8; 32])),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_catch_all_actor_handler()(state, admin, req)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn validate_recipient_resolves_known_local_part() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let actor = [42u8; 32];
        state
            .db
            .put_exact_alias(
                "example.com",
                "alice",
                crate::db::mail_aliases::ALIAS_KIND_EXACT,
                &actor,
            )
            .await
            .unwrap();

        let req = ValidateRecipientRequest {
            local_part: "alice".into(),
            domain: "example.com".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = validate_recipient_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
        match reply {
            ValidateRecipientReply::Resolved {
                actor_id,
                is_role_address,
            } => {
                assert_eq!(actor_id, actor.to_vec());
                assert!(!is_role_address, "a normal alias hit is not a role address");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_recipient_rejects_unknown_local_part() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = ValidateRecipientRequest {
            local_part: "ghost".into(),
            domain: "example.com".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = validate_recipient_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
        assert!(matches!(reply, ValidateRecipientReply::Reject { .. }));
    }

    // ── validate_recipient handle-keyed local-auth fallback (Change A) ──
    //
    // On a domainless / bare-IP nest the login address domain matches no
    // registered mail-domain, so there is no exact-alias row to resolve.
    // The AUTH-login resolver (`validate_recipient` ONLY) falls back to the
    // unique handle→actor store so `test`, `test@<IP>`, `test@<any-name>`
    // all authenticate to the actor whose *handle* is `test`. Delivery
    // (`resolve_recipient`/`partition_recipients`) stays alias/domain-strict.
    // Change A of the any-locator CalDAV/IMAP design (tracked internally).

    #[tokio::test]
    async fn validate_recipient_resolves_by_handle_when_domain_unregistered() {
        let state = fixture_state().await;
        let mda = [1u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // An actor with handle `test`, but NO registered mail-domain on this nest
        // (the bare-IP / domainless box).
        let actor = [42u8; 32];
        state
            .db
            .create_user(&actor, "free", "the box owner")
            .await
            .unwrap();
        state.db.set_handle(&actor, "test").await.unwrap();

        // Login as `test@192.168.1.57` — the IP isn't a registered mail-domain.
        let req = ValidateRecipientRequest {
            local_part: "test".into(),
            domain: "192.168.1.57".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = validate_recipient_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
        match reply {
            ValidateRecipientReply::Resolved {
                actor_id,
                is_role_address,
            } => {
                assert_eq!(actor_id, actor.to_vec(), "resolves to the handle owner");
                assert!(!is_role_address, "a handle hit is not a role address");
            }
            other => panic!("expected Resolved via handle fallback, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_recipient_handle_fallback_ignores_admin_alias() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let mda = [1u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        // Actor B owns the handle `test` on this nest.
        let actor_b = [0xBBu8; 32];
        state
            .db
            .create_user(&actor_b, "free", "handle owner B")
            .await
            .unwrap();
        state.db.set_handle(&actor_b, "test").await.unwrap();

        // Admin creates a registered mail-domain `domain-x` and a colliding
        // exact alias `test@domain-x → actor A` (a DIFFERENT actor).
        let actor_a = [0xAAu8; 32];
        state
            .db
            .add_mail_domain("domain-x", true, "testing", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .put_exact_alias(
                "domain-x",
                "test",
                crate::db::mail_aliases::ALIAS_KIND_EXACT,
                &actor_a,
            )
            .await
            .unwrap();

        // Bare-handle login on an UNREGISTERED domain → resolves to B (the
        // handle owner), NOT A (the admin alias). Hijack-proof.
        let req_unreg = ValidateRecipientRequest {
            local_part: "test".into(),
            domain: "192.168.1.57".into(),
        };
        let payload = Bytes::from(encode_canonical(&req_unreg).unwrap().to_vec());
        let reply: ValidateRecipientReply = decode(
            &validate_recipient_handler()(state.clone(), mda, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        match reply {
            ValidateRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(
                    actor_id,
                    actor_b.to_vec(),
                    "unregistered-domain login resolves to the handle owner B, not the alias target A"
                );
            }
            other => panic!("expected Resolved to handle owner, got {other:?}"),
        }

        // The same local-part on the REGISTERED domain still resolves to A via
        // the alias path (today's exact behavior — multi-identity-safe).
        let req_reg = ValidateRecipientRequest {
            local_part: "test".into(),
            domain: "domain-x".into(),
        };
        let payload = Bytes::from(encode_canonical(&req_reg).unwrap().to_vec());
        let reply: ValidateRecipientReply = decode(
            &validate_recipient_handler()(state, mda, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        match reply {
            ValidateRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(
                    actor_id,
                    actor_a.to_vec(),
                    "registered-domain login still resolves via the alias to A"
                );
            }
            other => panic!("expected Resolved via alias, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_recipient_rejects_unknown_handle_on_unregistered_domain() {
        let state = fixture_state().await;
        let mda = [1u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // No handle `ghost`, no registered mail-domain.
        let req = ValidateRecipientRequest {
            local_part: "ghost".into(),
            domain: "192.168.1.57".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: ValidateRecipientReply = decode(
            &validate_recipient_handler()(state, mda, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(
            matches!(reply, ValidateRecipientReply::Reject { .. }),
            "no matching handle on an unregistered domain → reject"
        );
    }

    // ── check_greylist (smtp-server.md § Greylisting) ──

    fn greylist_payload() -> Bytes {
        Bytes::from(
            encode_canonical(&CheckGreylistRequest {
                from: "sender@remote.test".into(),
                to: "bob@example.com".into(),
                client_ip: "203.0.113.9".into(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    #[tokio::test]
    async fn check_greylist_defers_first_then_passes_when_enabled() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Enable greylist with a zero min-hold so the retry passes immediately
        // (the min-hold / window / whitelist timing is unit-tested in
        // fauna_mail::greylist::decide); here we exercise the handler's
        // read → decide → upsert round-trip + cross-call persistence.
        state
            .db
            .put_spam_policy(crate::db::mail_policy::SpamPolicyOverrides {
                greylist_enabled: Some(true),
                greylist_delay_secs: Some(0),
                ..Default::default()
            })
            .await
            .unwrap();

        // First contact → defer (no row yet).
        let r1: CheckGreylistReply = decode(
            &check_greylist_handler()(state.clone(), mta, greylist_payload())
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(!r1.pass, "first contact must defer (451)");

        // Retry the same tuple → pass (row aged ≥ 0 min-hold). Proves the row
        // persisted nest-side between the two calls.
        let r2: CheckGreylistReply = decode(
            &check_greylist_handler()(state.clone(), mta, greylist_payload())
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(r2.pass, "retry after the (zero) hold must pass (250)");
    }

    #[tokio::test]
    async fn check_greylist_disabled_always_passes() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        state
            .db
            .put_spam_policy(crate::db::mail_policy::SpamPolicyOverrides {
                greylist_enabled: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();

        let r: CheckGreylistReply = decode(
            &check_greylist_handler()(state, mta, greylist_payload())
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(r.pass, "disabled greylist passes even on first contact");
    }

    #[tokio::test]
    async fn check_greylist_role_address_bypasses_even_when_enabled() {
        // smtp-server.md:205 — role-address recipients bypass greylisting.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // A real 60 s hold so a non-role first contact WOULD defer.
        state
            .db
            .put_spam_policy(crate::db::mail_policy::SpamPolicyOverrides {
                greylist_enabled: Some(true),
                greylist_delay_secs: Some(60),
                ..Default::default()
            })
            .await
            .unwrap();

        let mk = |to: &str| {
            Bytes::from(
                encode_canonical(&CheckGreylistRequest {
                    from: "ops@remote.test".into(),
                    to: to.into(),
                    client_ip: "203.0.113.9".into(),
                })
                .unwrap()
                .to_vec(),
            )
        };

        // postmaster@ — reserved role address → bypass → pass on first contact.
        let role: CheckGreylistReply = decode(
            &check_greylist_handler()(state.clone(), mta, mk("postmaster@example.com"))
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(
            role.pass,
            "role address (postmaster@) must bypass greylisting"
        );

        // Sanity: a non-role first contact under the same 60 s hold defers.
        let non_role: CheckGreylistReply = decode(
            &check_greylist_handler()(state, mta, mk("alice@example.com"))
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(!non_role.pass, "non-role first contact must defer");
    }

    // ── role-address routing (T2.5: smtp-server.md § abuse@/postmaster@) ──

    #[tokio::test]
    async fn validate_recipient_routes_role_address_to_admin() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Single admin = the box's claimer; `list_admin_actors().first()`.
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;

        // None of these reserved local-parts can be created as an alias
        // (validate_exact_local_part rejects them), so the exact lookup
        // always misses — role-address routing must resolve them to the
        // admin instead of 550-ing (smtp-server.md :201 never-reject).
        // tlsrpt@/dmarc-report@ classify as their report processors but
        // fall back to the admin until T4 wires the processor dispatch;
        // the never-reject invariant is what binds here.
        for local in [
            "postmaster",
            "abuse",
            "noc",
            "security",
            "tlsrpt",
            "dmarc-report",
        ] {
            let req = ValidateRecipientRequest {
                local_part: local.into(),
                domain: "example.com".into(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let reply_bytes = validate_recipient_handler()(state.clone(), mta, payload)
                .await
                .expect("handler ok");
            let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
            match reply {
                ValidateRecipientReply::Resolved {
                    actor_id,
                    is_role_address,
                } => {
                    assert_eq!(actor_id, admin.to_vec(), "{local}@ → admin mailbox");
                    assert!(
                        is_role_address,
                        "{local}@ resolved via the role-address route → bypasses quota"
                    );
                }
                other => panic!("expected Resolved for {local}@, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn validate_recipient_role_address_no_admin_is_internal_not_reject() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // No admin claimed. Unreachable in production (MTA enrollment
        // requires an authenticated admin), but the handler must defend
        // the never-reject invariant: surface an internal error so the
        // Go MTA tempfails 451 rather than hard-550 (validate_recipient's
        // reply can only express Resolved / Reject→550).
        let req = ValidateRecipientRequest {
            local_part: "postmaster".into(),
            domain: "example.com".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = validate_recipient_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.internal");
    }

    #[tokio::test]
    async fn validate_recipient_permitted_for_mda_class() {
        // The MDA resolves the MUA-AUTH username → actor before fetching that
        // actor's wrapped-MSEK blob (mail-mda-7 / `internal/mda/imap/auth.go`),
        // so an MDA bridge must NOT be permission-denied. With no alias for
        // "alice" the handler returns a normal Reject *reply* (not an RPC error),
        // which is exactly the resolve-miss the MDA maps to an auth-fail.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ValidateRecipientRequest {
            local_part: "alice".into(),
            domain: "example.com".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply = validate_recipient_handler()(state, mda, payload)
            .await
            .expect("MDA must be permitted to call validate_recipient");
        let decoded: ValidateRecipientReply = decode(&reply).unwrap();
        assert!(
            matches!(decoded, ValidateRecipientReply::Reject { .. }),
            "unaliased non-role local-part must resolve to Reject, got {decoded:?}"
        );
    }

    #[tokio::test]
    async fn report_session_close_persists_and_dedupes() {
        let state = fixture_state().await;
        let mda = [55u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ReportSessionCloseRequest {
            actor_id: vec![44u8; 32],
            credential_id: "cred-x".into(),
            reason: "logout".into(),
            occurred_at: 1_700_000_000,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        for _ in 0..2 {
            let _ = report_session_close_handler()(state.clone(), mda, payload.clone())
                .await
                .expect("ok");
        }
        let target = [44u8; 32];
        let rows = state
            .db
            .list_bridge_session_close_for_actor(&target, 5)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "duplicate calls must collapse");
        assert_eq!(rows[0].reason, "logout");
    }

    #[tokio::test]
    async fn report_session_close_rejects_empty_reason() {
        let state = fixture_state().await;
        let mda = [55u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = ReportSessionCloseRequest {
            actor_id: vec![44u8; 32],
            credential_id: "cred-x".into(),
            reason: "   ".into(),
            occurred_at: 1_700_000_000,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = report_session_close_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn report_session_close_user_class_denied() {
        let state = fixture_state().await;
        let stranger = [101u8; 32];
        let req = ReportSessionCloseRequest {
            actor_id: vec![44u8; 32],
            credential_id: "cred-x".into(),
            reason: "logout".into(),
            occurred_at: 1_700_000_000,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = report_session_close_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── provision_recipient_mls_pubkey: User self-registration ──

    #[tokio::test]
    async fn provision_recipient_mls_pubkey_user_self_permitted() {
        // A regular user registering their OWN recipient pubkey (the
        // enable-mail path) succeeds and writes the row.
        let state = fixture_state().await;
        let user = [70u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let pubkey = [71u8; 32];
        let req = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(user.to_vec()),
            mls_pubkey: ByteBuf::from(pubkey.to_vec()),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        provision_recipient_mls_pubkey_handler()(state.clone(), user, payload)
            .await
            .expect("user self-registration ok");
        assert_eq!(
            state.db.get_actor_mls_pubkey(&user).await.unwrap(),
            Some(pubkey)
        );
    }

    #[tokio::test]
    async fn provision_recipient_mls_pubkey_user_other_denied() {
        // A user must NOT be able to register someone else's pubkey
        // (would let them redirect another actor's inbound mail).
        let state = fixture_state().await;
        let user = [70u8; 32];
        let victim = [99u8; 32];
        let req = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(victim.to_vec()),
            mls_pubkey: ByteBuf::from([71u8; 32].to_vec()),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = provision_recipient_mls_pubkey_handler()(state.clone(), user, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        assert_eq!(state.db.get_actor_mls_pubkey(&victim).await.unwrap(), None);
    }

    #[tokio::test]
    async fn provision_recipient_mls_pubkey_admin_other_permitted() {
        // An admin may register any actor's pubkey (bridge-perimeter /
        // migration), not just their own.
        let state = fixture_state().await;
        let admin = [80u8; 32];
        add_admin(&state.db, &admin).await;
        let target = [81u8; 32];
        let pubkey = [82u8; 32];
        let req = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(target.to_vec()),
            mls_pubkey: ByteBuf::from(pubkey.to_vec()),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        provision_recipient_mls_pubkey_handler()(state.clone(), admin, payload)
            .await
            .expect("admin cross-actor registration ok");
        assert_eq!(
            state.db.get_actor_mls_pubkey(&target).await.unwrap(),
            Some(pubkey)
        );
    }

    #[tokio::test]
    async fn validate_recipient_unknown_actor_denied() {
        let state = fixture_state().await;
        let stranger = [123u8; 32];

        let req = ValidateRecipientRequest {
            local_part: "alice".into(),
            domain: "example.com".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        // Stranger isn't admin and isn't a service user, so caller_class
        // resolves to User, which the allowlist rejects for this kind.
        let err = validate_recipient_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_returns_some_when_provisioned() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let pubkey = [9u8; 32];
        state
            .db
            .put_actor_recipient_seal_key(&target, &pubkey, &[7u8; 1184])
            .await
            .unwrap();

        let req = FetchRecipientMlsPubkeyRequest {
            actor_id: target.to_vec(),
            mail_new_ingest: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_mls_pubkey_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchRecipientMlsPubkeyReply = decode(&reply_bytes).unwrap();
        let got = reply.key.expect("expected a key on file");
        assert_eq!(got.mls_pubkey.as_slice(), &pubkey[..]);
        assert_eq!(got.mlkem_ek.as_slice(), &[7u8; 1184][..]);
    }

    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_returns_none_when_unprovisioned() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [77u8; 32];

        let req = FetchRecipientMlsPubkeyRequest {
            actor_id: target.to_vec(),
            mail_new_ingest: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_mls_pubkey_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchRecipientMlsPubkeyReply = decode(&reply_bytes).unwrap();
        assert!(reply.key.is_none());
        assert!(!reply.succession_pending);
    }

    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_flags_succession_pending_for_an_unprovisioned_successor() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let old_actor = [88u8; 32];
        let new_actor = [89u8; 32];
        state
            .db
            .create_user_with_handle(&old_actor, "free", "succeeded-user", None)
            .await
            .unwrap();
        state
            .db
            .record_succession(&old_actor, &new_actor, b"statement", 1)
            .await
            .unwrap()
            .expect("succession applies");
        // new_actor never provisioned a key — the succession leg hasn't run
        // yet (it fires at the successor's first sign-in).

        let req = FetchRecipientMlsPubkeyRequest {
            actor_id: new_actor.to_vec(),
            mail_new_ingest: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_mls_pubkey_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchRecipientMlsPubkeyReply = decode(&reply_bytes).unwrap();
        assert!(reply.key.is_none());
        assert!(reply.succession_pending);
    }

    #[cfg(feature = "test-hooks")]
    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_seals_under_epoch_key_for_mail_new_ingest_only() {
        // B4 (updated at the 2026-07-19 write flip): epoch sealing is on by
        // default (`MAIL_EPOCH_SEALING_WRITE_DEFAULT = true`), and this
        // proves the plumbing from the RPC handler down to the DB resolver
        // connects — for the genuine mail-new-ingest caller ONLY.
        // `AppState::epoch_sealing_test_override` (driven in a real e2e by
        // `POST /api/v1/test/content/epoch_sealing`) is force-on-only and
        // now redundant-but-harmless; it is exercised below so the e2e hook
        // path stays pinned.
        //
        // this same reply also
        // feeds the MDA session's cached pubkey for CalDAV/CardDAV PUT,
        // collection-metadata seals, and the IMAP spam-model re-seal — none
        // of which have an epoch opener. `mail_new_ingest` scopes the epoch
        // gate to the genuine MTA per-delivery caller ONLY; every other
        // caller (which never sets it — it defaults false) must keep
        // getting the standing key even with the write gate forced on.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let standing_pubkey = [9u8; 32];
        state
            .db
            .put_actor_recipient_seal_key(&target, &standing_pubkey, &[7u8; 1184])
            .await
            .unwrap();
        let e_now =
            fauna_mls::wrapped_blob::mail_sealing_epoch_of(crate::db::now_epoch_secs() as u64);
        let epoch_pubkey = [0x77u8; 32];
        state
            .db
            .put_actor_epoch_seal_keys(&target, &[(e_now, epoch_pubkey, vec![7u8; 1184])])
            .await
            .unwrap();

        let fetch = |state: Arc<AppState>, mail_new_ingest: bool| {
            let req = FetchRecipientMlsPubkeyRequest {
                actor_id: target.to_vec(),
                mail_new_ingest,
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            async move {
                let reply: FetchRecipientMlsPubkeyReply = decode(
                    &fetch_recipient_mls_pubkey_handler()(state, mta, payload)
                        .await
                        .unwrap(),
                )
                .unwrap();
                reply.key.unwrap().mls_pubkey
            }
        };

        // Gate on by default since the 2026-07-19 write flip
        // (`MAIL_EPOCH_SEALING_WRITE_DEFAULT = true`): ONLY the
        // mail-new-ingest caller gets the current epoch's key.
        assert!(
            state.epoch_sealing_enabled(),
            "the 2026-07-19 write flip made epoch sealing the default"
        );
        assert_eq!(
            fetch(state.clone(), true).await.as_slice(),
            &epoch_pubkey[..]
        );
        assert_eq!(
            fetch(state.clone(), false).await.as_slice(),
            &standing_pubkey[..]
        );

        // The force-on e2e test hook remains a no-op-safe override
        // (force-on-only; it can never turn the gate off).
        state
            .epoch_sealing_test_override
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(state.epoch_sealing_enabled());
        assert_eq!(
            fetch(state.clone(), true).await.as_slice(),
            &epoch_pubkey[..],
            "the genuine MTA mail-new-ingest caller gets the epoch key"
        );
        assert_eq!(
            fetch(state.clone(), false).await.as_slice(),
            &standing_pubkey[..],
            "every other caller (DAV/IMAP session pubkey caching, feeding \
             CalDAV/CardDAV PUT + collection-metadata + spam-model reseal) \
             must still get the standing key"
        );
    }

    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_permits_mda_class() {
        // I5 Phase D.5: the MDA needs the recipient's MLS pubkey for
        // APPEND seal-to-self (IMAP write surface) — same primitive the
        // MTA uses to seal inbound mail.  The recipient's MLS pubkey is
        // a public key, so widening this gate adds no privacy surface;
        // it just unblocks the seal-to-self path.  Phase C.3 wired the
        // MDA's AUTH-time fetch of this pubkey but the allowlist row
        // was MTA-only — D.5 widens both this RPC and the index-key
        // RPC to BridgeMta | BridgeMda.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = FetchRecipientMlsPubkeyRequest {
            actor_id: vec![42u8; 32],
            mail_new_ingest: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_mls_pubkey_handler()(state, mda, payload)
            .await
            .expect("MDA must be permitted to fetch the recipient MLS pubkey");
        let reply: FetchRecipientMlsPubkeyReply = decode(&reply_bytes).unwrap();
        // No pubkey provisioned for actor [42u8; 32] in the fixture →
        // None.  The test asserts caller-class permission, not the
        // provisioning state.
        assert!(reply.key.is_none());
    }

    #[tokio::test]
    async fn fetch_recipient_index_key_returns_some_when_provisioned() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let pubkey = [11u8; 32];
        state
            .db
            .put_actor_index_pubkey(&target, &pubkey)
            .await
            .unwrap();

        let req = FetchRecipientIndexKeyRequest {
            actor_id: target.to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_index_key_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchRecipientIndexKeyReply = decode(&reply_bytes).unwrap();
        let got = reply.pubkey.expect("expected Some pubkey");
        assert_eq!(got.as_slice(), &pubkey[..]);
    }

    #[tokio::test]
    async fn fetch_recipient_index_key_returns_none_when_unprovisioned() {
        // Phase C.9 ships the wire surface but no production provisioning
        // RPC exists yet (Phase E concern). Until then `None` is the
        // expected reply for any actor without a hand-seeded index pubkey;
        // the MTA bridge falls back to the MLS pubkey for the index hint
        // when this returns None, with a Phase E follow-up to wire real
        // index pubkeys end-to-end.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [88u8; 32];

        let req = FetchRecipientIndexKeyRequest {
            actor_id: target.to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_index_key_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchRecipientIndexKeyReply = decode(&reply_bytes).unwrap();
        assert!(reply.pubkey.is_none());
    }

    #[tokio::test]
    async fn fetch_recipient_index_key_permits_mda_class() {
        // I5 Phase D.5 — see the parallel test on fetch_recipient_mls_pubkey
        // above.  APPEND seals the search-index hint to the actor's own
        // index pubkey, so the MDA fetches its own index key just as the
        // MTA fetches the recipient's for inbound mail.
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = FetchRecipientIndexKeyRequest {
            actor_id: vec![42u8; 32],
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_recipient_index_key_handler()(state, mda, payload)
            .await
            .expect("MDA must be permitted to fetch the recipient index key");
        let reply: FetchRecipientIndexKeyReply = decode(&reply_bytes).unwrap();
        assert!(reply.pubkey.is_none());
    }

    #[tokio::test]
    async fn fetch_config_returns_defaults_for_mta() {
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = FetchConfigRequest {
            scope: "all".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_config_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchConfigReply = decode(&reply_bytes).unwrap();
        // Defaults must be deterministic so bridges can cache them.
        // Stage-5 default-off (mail-policy-config.md § Default-off on first
        // claim): an unset toggle reports mail OFF; the launched client's § 3b
        // glue (or the admin-mail toggle) is what enables it explicitly.
        assert!(
            !reply.mail_enabled,
            "unset-toggle fetch_config must report mail_enabled=false (default-off)"
        );
        // Permissive auto-Junk policy (T3.1): auto-Junk on at score
        // 5, but reject on the content score is disabled by default
        // (admin opt-in). `0` = disabled per `SpamPolicyThresholds` field docs.
        assert_eq!(reply.spam.max_score_before_spam_folder, 5);
        assert_eq!(reply.spam.max_score_before_reject, 0);
        assert!(!reply.spam.dnsbl_servers.is_empty());
        // Inbound-hardening defaults per mail-policy-config.md § Inbound hardening.
        assert_eq!(reply.spam.max_conn_per_min, 10);
        assert_eq!(reply.spam.fcrdns_mode, "score_signal");
        assert!(reply.spam.helo_identity_required);
        assert!(!reply.spam.reject_fcrdns_fail);
        assert!(reply.auth.enforce_dmarc);
        // DKIM enforce-on-fail is off by default (C.6); admins opt in.
        assert!(!reply.auth.enforce_dkim);
        assert!(reply.submission.max_per_day > 0);
        assert!(reply.imap.idle_timeout_secs > 0);
        // I5 Phase A MDA knobs — defaults match the catalog rows in
        // docs/goal/behavior/mail-policy-config.md § IMAP server policy.
        assert_eq!(reply.imap.tombstone_retention_days, 30);
        assert_eq!(reply.imap.delete_nonempty, "forbidden");
        assert_eq!(reply.imap.bodystructure_cache_max, 4096);
        // I5 Phase C.8 QUOTA defaults — `imap-server.md` § QUOTA.
        assert_eq!(reply.imap.storage_bytes_default, 1 << 30);
        assert_eq!(reply.imap.message_count_default, 50_000);
        // Outbound delivery defaults per mail-policy-config.md.
        assert_eq!(reply.outbound.retry_schedule_seconds.len(), 10);
        assert_eq!(reply.outbound.retry_schedule_seconds[1], 300); // 5m
        assert_eq!(reply.outbound.permanent_failure_timeout_hours, 120);
        assert_eq!(reply.outbound.delay_warning_at_hours, 4);
        assert_eq!(reply.outbound.ndr_rate_limit_days, 7);
        assert!(reply.outbound.suppress_ndr_spf_hardfail);
        assert!(reply.outbound.suppress_ndr_dmarc_reject);
        assert!(!reply.outbound.postmaster_cc_bounces);
        assert!(reply.outbound.tlsrpt_send_reports);
        assert!(reply.outbound.ipv6_enabled);
        assert!(reply.outbound.treat_5xx_as_transient.is_empty());
        // T2.6 graceful-shutdown drain budget per mail-policy-config.md:57.
        assert_eq!(reply.bridge.shutdown_grace_seconds, 30);
    }

    /// Phase E — `fetch_config` honors the persisted `mail_enabled` toggle, and
    /// falls back to the derived "approved ⇒ enabled" default when unset.
    #[tokio::test]
    async fn fetch_config_reflects_mail_enabled_toggle() {
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mta = [3u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let fetch = |state: Arc<AppState>, mta: [u8; 32]| async move {
            let req = FetchConfigRequest {
                scope: "all".into(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let reply_bytes = fetch_config_handler()(state, mta, payload)
                .await
                .expect("handler ok");
            let reply: FetchConfigReply = decode(&reply_bytes).unwrap();
            reply.mail_enabled
        };

        // Unset toggle → OFF (Stage-5 default-off; the retired approved⇒on
        // derivation survives only as the upgrade materialization, which runs
        // at DB open — before this fixture's bridge existed).
        assert!(!fetch(state.clone(), mta).await);

        // Admin enables → the toggle wins.
        state.db.set_mail_enabled(true).await.unwrap();
        assert!(fetch(state.clone(), mta).await);

        // Admin disables → the toggle wins again.
        state.db.set_mail_enabled(false).await.unwrap();
        assert!(!fetch(state.clone(), mta).await);
    }

    /// `set_webdav_enabled` flips the deployment-wide WebDAV toggle projected on
    /// `FetchConfigReply.webdav_enabled`, and the projection falls back to
    /// `mail_enabled` when unset (the same default posture as CalDAV/CardDAV —
    /// harmless-on, since nothing is served until a set is individually flagged).
    /// Files twin of `fetch_config_reflects_mail_enabled_toggle`.
    #[tokio::test]
    async fn fetch_config_reflects_webdav_enabled_toggle() {
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mda = [11u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let fetch = |state: Arc<AppState>, mda: [u8; 32]| async move {
            let req = FetchConfigRequest {
                scope: "all".into(),
            };
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            let reply_bytes = fetch_config_handler()(state, mda, payload)
                .await
                .expect("handler ok");
            let reply: FetchConfigReply = decode(&reply_bytes).unwrap();
            reply.webdav_enabled
        };

        // Both toggles unset → WebDAV follows the effective mail default, which
        // is OFF (Stage-5 default-off).
        assert!(!fetch(state.clone(), mda).await);

        // Admin enables mail but has never touched WebDAV → WebDAV follows mail.
        state.db.set_mail_enabled(true).await.unwrap();
        assert!(fetch(state.clone(), mda).await);

        // Mail explicitly off, WebDAV still unset → follows mail off.
        state.db.set_mail_enabled(false).await.unwrap();
        assert!(!fetch(state.clone(), mda).await);

        // Admin explicitly enables WebDAV → the WebDAV toggle wins over mail-off
        // (files gate independently; a files-only deployment is valid).
        state.db.set_webdav_enabled(true).await.unwrap();
        assert!(fetch(state.clone(), mda).await);

        // Admin explicitly disables WebDAV → off regardless of mail.
        state.db.set_mail_enabled(true).await.unwrap();
        state.db.set_webdav_enabled(false).await.unwrap();
        assert!(!fetch(state.clone(), mda).await);
    }

    #[tokio::test]
    async fn fetch_config_returns_defaults_for_mda() {
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = FetchConfigRequest {
            scope: "all".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_config_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: FetchConfigReply = decode(&reply_bytes).unwrap();
        // I5 Phase A MDA knobs.
        assert_eq!(reply.imap.idle_timeout_secs, 1740);
        assert_eq!(reply.imap.tombstone_retention_days, 30);
        assert_eq!(reply.imap.delete_nonempty, "forbidden");
        assert_eq!(reply.imap.bodystructure_cache_max, 4096);
        // I5 Phase C.8 QUOTA defaults — the MDA reads these to size
        // GETQUOTA replies without round-tripping a separate RPC per
        // session.
        assert_eq!(reply.imap.storage_bytes_default, 1 << 30);
        assert_eq!(reply.imap.message_count_default, 50_000);
    }

    #[tokio::test]
    async fn fetch_config_returns_local_domains_and_primary_from_mail_domains_table() {
        // Per docs/goal/behavior/mail-multidomain.md § The mail_domains
        // model + § The primary domain: `mail.local_domains` is the
        // derived projection of `domain_name` rows where removed_at IS
        // NULL; `primary_domain` is the active `is_primary = true` row.
        // Both are surfaced on FetchConfigReply so the bridge's
        // mta.Run iterates RCPT TO over the full list and anchors
        // TLS/DKIM/EHLO on the primary.
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        state
            .db
            .add_mail_domain(
                "primary.example",
                /* is_primary */ true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        state
            .db
            .add_mail_domain(
                "secondary.example",
                /* is_primary */ false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let req = FetchConfigRequest {
            scope: "all".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_config_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchConfigReply = decode(&reply_bytes).unwrap();

        // Primary first (`ORDER BY is_primary DESC, added_at ASC`).
        assert_eq!(
            reply.local_domains,
            vec![
                "primary.example".to_string(),
                "secondary.example".to_string()
            ]
        );
        assert_eq!(reply.primary_domain, "primary.example");
        // Per-domain DKIM: every active row
        // projects its own selector entry, ordered like local_domains
        // (primary first). The NULL column (dkim_selector_override = None
        // above) defaults to "default" so each domain's DKIM registry has a
        // selector to fetch its admin-provisioned key under (DATA would
        // otherwise stay unsigned).
        assert_eq!(
            reply.dkim_selectors,
            vec![
                fauna_protocol::bridge_routing::DomainDkimSelector {
                    domain: "primary.example".to_string(),
                    selector: "default".to_string(),
                    extra: Default::default(),
                },
                fauna_protocol::bridge_routing::DomainDkimSelector {
                    domain: "secondary.example".to_string(),
                    selector: "default".to_string(),
                    extra: Default::default(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn fetch_config_projects_dkim_selector_override_and_empties_with_no_primary() {
        // A domain's explicit dkim_selector projects verbatim into its
        // dkim_selectors entry; with no mail_domains rows the list is empty
        // (registry degraded, unsigned DATA). Per-domain DKIM.
        use fauna_protocol::bridge_routing::{
            DomainDkimSelector, FetchConfigReply, FetchConfigRequest,
        };
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = FetchConfigRequest {
            scope: "all".into(),
        };

        // No mail_domains rows → empty selector list (whole bridge degraded).
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: FetchConfigReply = decode(
            &fetch_config_handler()(state.clone(), mta, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(reply.dkim_selectors.is_empty());

        // Claim a primary with an explicit selector override → verbatim.
        state
            .db
            .add_mail_domain(
                "primary.example",
                /* is_primary */ true,
                "enforce",
                "expand_primary",
                None,
                Some("sel2026"),
            )
            .await
            .unwrap();
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: FetchConfigReply = decode(
            &fetch_config_handler()(state, mta, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert_eq!(
            reply.dkim_selectors,
            vec![DomainDkimSelector {
                domain: "primary.example".to_string(),
                selector: "sel2026".to_string(),
                extra: Default::default(),
            }]
        );
    }

    #[tokio::test]
    async fn fetch_config_returns_empty_local_domains_when_table_is_empty() {
        // Fresh nest: no mail_domains rows yet. Bridge's mta.Run
        // idles on empty list per
        // docs/goal/behavior/smtp-server.md § Implementation status today.
        use fauna_protocol::bridge_routing::{FetchConfigReply, FetchConfigRequest};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = FetchConfigRequest {
            scope: "all".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = fetch_config_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchConfigReply = decode(&reply_bytes).unwrap();
        assert!(reply.local_domains.is_empty());
        assert!(reply.primary_domain.is_empty());
    }

    #[tokio::test]
    async fn fetch_config_rejects_unknown_scope() {
        use fauna_protocol::bridge_routing::FetchConfigRequest;
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = FetchConfigRequest {
            scope: "spam".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_config_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_config_user_class_denied() {
        use fauna_protocol::bridge_routing::FetchConfigRequest;
        let state = fixture_state().await;
        let stranger = [101u8; 32];

        let req = FetchConfigRequest {
            scope: "all".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_config_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_recipient_mls_pubkey_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = FetchRecipientMlsPubkeyRequest {
            actor_id: vec![1u8; 16], // wrong length
            mail_new_ingest: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_recipient_mls_pubkey_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn check_submission_quota_allowed_within_limit() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        let req = CheckSubmissionQuotaRequest {
            actor_id: target.to_vec(),
            recipient_count: 5,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = check_submission_quota_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: CheckSubmissionQuotaReply = decode(&bytes).unwrap();
        assert_eq!(reply, CheckSubmissionQuotaReply::Allowed);
    }

    /// One RCPT, one quota call, one call = the check for one recipient: the
    /// payload names the running count `rc` and the locality; the test
    /// helpers below build it.
    fn quota_call(target: &[u8; 32], rc: u32, local: bool) -> Bytes {
        let req = CheckSubmissionQuotaRequest {
            actor_id: target.to_vec(),
            recipient_count: rc,
            recipient_is_local: local,
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    async fn quota_reply(
        state: &Arc<AppState>,
        mta: [u8; 32],
        payload: Bytes,
    ) -> CheckSubmissionQuotaReply {
        let bytes = check_submission_quota_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        decode(&bytes).unwrap()
    }

    /// How many units of a `limit`-sized day the actor has spent, read back
    /// through the counter itself: the largest debit that still fits.
    async fn quota_used(state: &Arc<AppState>, target: &[u8; 32], limit: u32) -> u32 {
        let day = now_epoch_secs() / 86_400;
        match state
            .db
            .try_consume_submission_quota(target, day, limit + 1, limit)
            .await
            .unwrap()
        {
            SubmissionQuotaOutcome::OverQuota { remaining } => limit - remaining,
            SubmissionQuotaOutcome::Allowed => unreachable!("limit + 1 never fits"),
        }
    }

    #[tokio::test]
    async fn check_submission_quota_refuses_once_the_day_is_spent() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        // Pre-fill via the db method to one unit under the daily limit.
        let day = now_epoch_secs() / 86_400;
        let limit = SubmissionPolicyThresholds::default().max_per_day;
        let _ = state
            .db
            .try_consume_submission_quota(&target, day, limit - 1, limit)
            .await
            .unwrap();

        // The first RCPT spends the last unit; the second finds none left.
        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 1, false)).await,
            CheckSubmissionQuotaReply::Allowed
        );
        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 2, false)).await,
            CheckSubmissionQuotaReply::OverQuota { remaining: 0 }
        );
    }

    /// Hazard pin for `fauna.bridges.check_submission_quota`'s
    /// `forbid_replay = true` (79th pass). Asserts the **harm**, not the flag,
    /// so it stays meaningful if the handler is ever made idempotent: the
    /// handler's only effect is a bare read-modify-write debit
    /// (`used = used + 1`, `try_consume_submission_quota`) with no dedup key
    /// of any kind, so a replayed call charges the actor's daily submission
    /// allowance a **second** time for one recipient. The visible consequence
    /// is the user's own legitimate mail being refused as over-quota at a
    /// volume they never sent.
    ///
    /// If a future change makes the debit idempotent (a per-submission dedup
    /// key), this test goes red and the flag may be reconsidered — that is the
    /// intended signal.
    #[tokio::test]
    async fn a_replayed_submission_quota_check_debits_the_allowance_twice() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let limit = 3;
        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_per_day: Some(limit),
                ..Default::default()
            })
            .await
            .unwrap();

        // One RCPT's check, delivered twice — exactly what a post-reconnect
        // auto-retry of the same request would do (the nest's idempotency
        // cache is per-connection and starts empty on the new connection, so
        // it cannot collapse this).
        let payload = quota_call(&target, 1, false);
        for _ in 0..2 {
            assert_eq!(
                quota_reply(&state, mta, payload.clone()).await,
                CheckSubmissionQuotaReply::Allowed
            );
        }

        // The harm: 2 charged for one recipient. Of a 3-unit day, one more
        // recipient fits, and the one after it — which a single debit would
        // have left room for — is refused.
        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 1, false)).await,
            CheckSubmissionQuotaReply::Allowed
        );
        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 1, false)).await,
            CheckSubmissionQuotaReply::OverQuota { remaining: 0 },
            "a replayed quota check double-debited: the actor was charged 2 \
             for one recipient"
        );
    }

    #[tokio::test]
    async fn check_submission_quota_respects_admin_override() {
        // An admin-set `max_per_day` override must actually be enforced —
        // not silently ignored in favor of the compile-time catalog default
        // (mail-policy-config.md § Submission policy, sweep-41 finding).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_per_day: Some(3),
                ..Default::default()
            })
            .await
            .unwrap();

        // Three recipients fit the admin's day; the fourth — well inside the
        // compile-time default of 1000 — does not.
        for rc in 1..=3 {
            assert_eq!(
                quota_reply(&state, mta, quota_call(&target, rc, false)).await,
                CheckSubmissionQuotaReply::Allowed,
                "recipient {rc} of the admin's 3"
            );
        }
        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 4, false)).await,
            CheckSubmissionQuotaReply::OverQuota { remaining: 0 }
        );
    }

    /// The charging rule (`smtp-server.md` § Architectural rules, ruled
    /// 2026-09-25): one unit per accepted RCPT, never `recipient_count`. The
    /// bridge sends the RUNNING count on every RCPT — it is the per-message
    /// cap's input — so debiting it charged a k-recipient message k(k+1)/2
    /// units: 10 for a 4-recipient message, 55 for a 10-recipient one.
    #[tokio::test]
    async fn check_submission_quota_charges_one_remote_recipient_per_call() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let limit = 10;
        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_per_day: Some(limit),
                ..Default::default()
            })
            .await
            .unwrap();

        // A 4-recipient message to the outside: four RCPTs, running counts 1..=4.
        for rc in 1..=4 {
            assert_eq!(
                quota_reply(&state, mta, quota_call(&target, rc, false)).await,
                CheckSubmissionQuotaReply::Allowed,
                "RCPT {rc} of 4"
            );
        }
        assert_eq!(
            quota_used(&state, &target, limit).await,
            4,
            "a 4-recipient message costs 4 units, not 1+2+3+4"
        );
    }

    /// A recipient placed in a mailbox on this deployment never leaves it and
    /// consumes none of the allowance — the app door's rule since 2026-08-23
    /// (`mail-app-surface.md` § Outbound metering), the submission door's
    /// since 2026-09-25.
    #[tokio::test]
    async fn check_submission_quota_does_not_charge_a_local_recipient() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        let limit = 10;
        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_per_day: Some(limit),
                ..Default::default()
            })
            .await
            .unwrap();

        // local, remote, local: every RCPT is accepted, one unit is spent.
        for (rc, local) in [(1, true), (2, false), (3, true)] {
            assert_eq!(
                quota_reply(&state, mta, quota_call(&target, rc, local)).await,
                CheckSubmissionQuotaReply::Allowed,
                "RCPT {rc} (local = {local})"
            );
        }
        assert_eq!(
            quota_used(&state, &target, limit).await,
            1,
            "only the recipient that leaves the deployment is charged"
        );
    }

    /// The per-message cap counts every recipient, local ones included — it
    /// bounds the message's shape, not what leaves the deployment.
    #[tokio::test]
    async fn check_submission_quota_still_caps_a_local_recipient_per_message() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_recipients_per_message: Some(3),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(
            quota_reply(&state, mta, quota_call(&target, 4, true)).await,
            CheckSubmissionQuotaReply::OverQuota { remaining: 3 },
            "the 4th recipient of a 3-recipient cap is refused even when local"
        );
        let limit = SubmissionPolicyThresholds::default().max_per_day;
        assert_eq!(quota_used(&state, &target, limit).await, 0);
    }

    #[tokio::test]
    async fn check_submission_quota_respects_max_recipients_per_message_override() {
        // An admin-set `max_recipients_per_message` override must actually be
        // enforced nest-side — the submission token's `MaxRecipients` fast-
        // path is a client-self-asserted floor, never fed by this admin
        // knob (mail-policy-config.md § Implementation status today,
        // sweep-169 finding).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];

        state
            .db
            .put_submission_policy(SubmissionPolicyOverrides {
                max_recipients_per_message: Some(3),
                ..Default::default()
            })
            .await
            .unwrap();

        let req = CheckSubmissionQuotaRequest {
            actor_id: target.to_vec(),
            recipient_count: 5,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = check_submission_quota_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: CheckSubmissionQuotaReply = decode(&bytes).unwrap();
        assert_eq!(reply, CheckSubmissionQuotaReply::OverQuota { remaining: 3 });

        // The rejected over-cap call must not have consumed any of the
        // actor's daily allowance — the full daily allowance must still fit.
        let day = now_epoch_secs() / 86_400;
        let limit = SubmissionPolicyThresholds::default().max_per_day;
        let outcome = state
            .db
            .try_consume_submission_quota(&target, day, limit, limit)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SubmissionQuotaOutcome::Allowed,
            "a per-message-cap rejection must not debit the daily quota"
        );
    }

    #[tokio::test]
    async fn check_submission_quota_requires_mta_class() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = CheckSubmissionQuotaRequest {
            actor_id: vec![42u8; 32],
            recipient_count: 1,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = check_submission_quota_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn check_submission_quota_rejects_zero_recipient_count() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = CheckSubmissionQuotaRequest {
            actor_id: vec![42u8; 32],
            recipient_count: 0,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = check_submission_quota_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn check_submission_quota_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = CheckSubmissionQuotaRequest {
            actor_id: vec![1u8; 16],
            recipient_count: 1,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = check_submission_quota_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn check_submission_quota_unknown_actor_denied() {
        let state = fixture_state().await;
        let stranger = [123u8; 32];

        let req = CheckSubmissionQuotaRequest {
            actor_id: vec![42u8; 32],
            recipient_count: 1,
            recipient_is_local: false,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = check_submission_quota_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// Seal `plaintext` as a genuine recipient envelope — the shape the Go MTA
    /// produces for every `ingest`/`submit`. S6.12b makes
    /// `persist_inbound_mail_request` refuse (at the wire edge) anything
    /// `SealedRecordBytes::verify` rejects, so success-path request bodies/hints
    /// must be real seals, not byte literals. NOT deterministic (HPKE
    /// encapsulation is randomized) — build a request once and reuse its bytes
    /// when a test needs the same message twice (idempotency/retry paths).
    fn sealed(plaintext: &[u8]) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};
        let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
        seal_to_recipient(plaintext, &pubkey)
            .expect("seal test fixture")
            .to_canonical_bytes()
            .expect("canonical test fixture")
    }

    fn sample_ingest_request(target: &[u8; 32], body: &[u8]) -> IngestInboundMailRequest {
        use fauna_protocol::bridge_routing::{AuthVerdicts, PublicMailMetadata};
        let sealed_body = sealed(body);
        let body_len = sealed_body.len() as u32;
        // What a producer does: the pair of the plaintext it is about to seal.
        let dedup = fauna_mail::mail_dedup_keys_from_slice(body);
        IngestInboundMailRequest {
            actor_id: target.to_vec(),
            dedup_key: dedup.dedup_key,
            envelope_key: dedup.envelope_key,
            encrypted_body: sealed_body,
            encrypted_index_hint: sealed(b"index-hint"),
            public_metadata: PublicMailMetadata {
                timestamp: 1_700_000_000,
                ciphertext_size: body_len,
                sender_domain: "example.com".into(),
            },
            verdicts: AuthVerdicts {
                dkim: DkimVerdict::Pass,
                spf: SpfVerdict::Pass,
                dmarc: DmarcVerdict::Pass,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Lower the deployment storage ceiling so the next inbound body alone
    /// trips it. Quota usage is now sized through the CARv2 index by record_cid
    /// (imap-server.md § QUOTA), so it can no longer be faked with a synthetic
    /// `byte_length` row; the inbound quota tests instead set a ceiling below
    /// the message body and rely on the pre-check arithmetic.
    async fn lower_storage_ceiling(state: &crate::routes::AppState, bytes: u64) {
        state
            .db
            .put_imap_policy(crate::db::mail_policy::ImapPolicyOverrides {
                storage_bytes_default: Some(bytes),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// The mail health readout's "last received" heartbeat
    /// (`mail-deliverability.md` § The mail health readout) advances on an
    /// accepted inbound delivery.
    #[tokio::test]
    async fn ingest_inbound_mail_stamps_the_inbound_heartbeat() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [43u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        assert_eq!(
            state
                .db
                .read_mail_heartbeats()
                .await
                .unwrap()
                .last_inbound_accepted_at,
            None
        );

        let req = sample_ingest_request(&target, b"inbound-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("inbound delivery ok");

        let hb = state.db.read_mail_heartbeats().await.unwrap();
        assert!(hb.last_inbound_accepted_at.is_some());
        assert_eq!(hb.last_outbound_delivered_at, None);
    }

    /// `mailbox-migration.md` § Dedup key persistence — every inbound MTA
    /// delivery populates `actor_message_dedup`, so a later import of the same
    /// account from a foreign IMAP server dedup-hits the copy delivered here.
    #[tokio::test]
    async fn ingest_inbound_mail_with_dedup_key_populates_the_index() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [43u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let key = "msgid:v1:inbound@example.com";
        let mut req = sample_ingest_request(&target, b"inbound-body");
        req.dedup_key = key.into();
        req.envelope_key = "env:v1:inbound".into();
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("inbound delivery ok");

        assert!(
            state.db.has_dedup_key(&target, key).await.unwrap(),
            "inbound MTA delivery must record its dedup key"
        );
        // …and the envelope key beside it: the sender chose the Message-ID, so
        // only this content-bound key can confirm a later import's hit
        // (§ The envelope key confirms a Message-ID hit).
        assert_eq!(
            state.db.dedup_envelope_key(&target, key).await.unwrap(),
            Some("env:v1:inbound".to_string())
        );
        let other = [44u8; 32];
        assert!(
            !state.db.has_dedup_key(&other, key).await.unwrap(),
            "dedup index must not leak across actors"
        );
    }

    /// A dedup hit must NEVER suppress a delivery. A legitimate resend, or a
    /// second copy the user is `Cc:`'d on, arrives with the same Message-ID —
    /// dropping it would be silent mail loss. Only `import_message` skips.
    #[tokio::test]
    async fn ingest_inbound_mail_is_never_suppressed_on_a_dedup_hit() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [45u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let key = "msgid:v1:resend@example.com";
        let mut first = sample_ingest_request(&target, b"first-copy");
        first.dedup_key = key.into();
        let p1 = Bytes::from(encode_canonical(&first).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, p1)
            .await
            .expect("first delivery ok");

        // Same dedup key, different bytes → a genuinely different message.
        let mut second = sample_ingest_request(&target, b"second-copy-different-bytes");
        second.dedup_key = key.into();
        let p2 = Bytes::from(encode_canonical(&second).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, p2)
            .await
            .expect("a dedup hit must not suppress the delivery");

        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "both copies delivered; neither dropped");
    }

    /// There is no absent key: an empty half of the pair is refused before
    /// anything is stored, rather than delivered unindexed.
    #[tokio::test]
    async fn ingest_inbound_mail_with_an_empty_key_is_refused() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [46u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        for strip_dedup in [true, false] {
            let mut req = sample_ingest_request(&target, b"unkeyed-body");
            if strip_dedup {
                req.dedup_key.clear();
            } else {
                req.envelope_key.clear();
            }
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            assert!(
                ingest_inbound_mail_handler()(state.clone(), mta, payload)
                    .await
                    .is_err(),
                "an empty key must be refused (dedup_key stripped: {strip_dedup})"
            );
        }
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert!(rows.is_empty(), "a refused delivery stores nothing");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_over_storage_quota_returns_over_quota() {
        // A normal (non-role) inbound recipient over the storage cap: the
        // delivery is rejected with the typed `over_quota` error the Go MTA
        // maps to `552 5.2.2 Mailbox full` (imap-server.md § Quota enforcement
        // points → "Inbound mail delivery"). A 10-byte ceiling sits below the
        // 12-byte body, so the pre-check (used + added vs ceiling) trips.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        lower_storage_ceiling(&state, 10).await;

        // The 12-byte body exceeds the 10-byte ceiling.
        let req = sample_ingest_request(&target, b"over-the-cap");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect_err("inbound delivery over the storage quota must be rejected");
        assert_eq!(err.code, "fauna.bridges.over_quota");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_role_address_bypasses_storage_quota() {
        // Role-address bypass invariant (smtp-server.md :204): an over-quota
        // admin mailbox still receives postmaster/abuse/security mail.
        // `is_role_address = true` on the ingest request skips the pre-check,
        // so the delivery succeeds despite a ceiling the body would exceed.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        lower_storage_ceiling(&state, 10).await;

        let mut req = sample_ingest_request(&target, b"postmaster-mail");
        req.is_role_address = true;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("role-address inbound delivery bypasses the quota pre-check");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.message_id.len(), 32);
    }

    #[tokio::test]
    async fn ingest_inbound_mail_filter_target_mailbox_files_into_custom_folder() {
        // T3.3: a matched filter rule's `FileInto { mailbox }` verdict rides the
        // ingest `target_mailbox` field and overrides the spam-disposition →
        // folder map. A *custom* (non-standard) folder is auto-created — with a
        // LIST-visible state row — so the message lands there, not in INBOX,
        // even though the spam disposition was `Accept` (→ INBOX).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"quarterly numbers");
        req.spam_disposition = SpamDisposition::Accept; // would be INBOX
        req.target_mailbox = Some("Reports".to_string());
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("filter FileInto delivery succeeds");

        let inbox = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert!(
            inbox.is_empty(),
            "FileInto must divert the message away from INBOX"
        );
        let reports = state
            .db
            .list_bridge_imap_messages(&target, "Reports")
            .await
            .unwrap();
        assert_eq!(
            reports.len(),
            1,
            "message lands in the filter's FileInto folder"
        );
        // The custom folder is LIST-visible (state row exists) — so the MDA's
        // LIST/SELECT can reach it.
        let mailboxes: Vec<String> = state
            .db
            .list_bridge_imap_mailbox_state(&target)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            mailboxes.iter().any(|m| m == "Reports"),
            "custom FileInto folder must be LIST-visible; got {mailboxes:?}"
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_file_into_refused_mailbox_falls_back_to_disposition() {
        // A `FileInto` verdict naming a mailbox inbound mail must never enter
        // (`email-filters.md` § Email filter rules) places by the spam
        // disposition instead, the same fallback a malformed name takes.
        // `Sent` and `Drafts` hold only this account's own writing: the SMTP
        // rail reads a `Sent` record as the user's own send and hides
        // mark-as-spam for it (`SmtpBackend::bucket_inbound`). A message in the
        // guardian's held mailbox IS a hold, placed only with its sidecar.
        // `fauna.email.filters.create` refuses such a rule; this covers one
        // stored before that check, or carried across a succession.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        for (i, refused) in ["Sent", "Drafts", GUARDIAN_HELD_MAILBOX]
            .into_iter()
            .enumerate()
        {
            let target = [42u8 + i as u8; 32];
            crate::test_support::seed_recipient_seal_key(
                &state.db,
                &target,
                &crate::test_support::FIXTURE_MSEK,
            )
            .await;

            let body = format!("from a stranger {i}");
            let mut req = sample_ingest_request(&target, body.as_bytes());
            req.spam_disposition = SpamDisposition::Accept; // → INBOX
            req.target_mailbox = Some(refused.to_string());
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
            ingest_inbound_mail_handler()(state.clone(), mta, payload)
                .await
                .expect("delivery succeeds on the disposition placement");

            let placed = state
                .db
                .list_bridge_imap_messages(&target, refused)
                .await
                .unwrap();
            assert!(
                placed.is_empty(),
                "a FileInto {refused:?} verdict must never place inbound mail there"
            );
            let inbox = state
                .db
                .list_bridge_imap_messages(&target, "INBOX")
                .await
                .unwrap();
            assert_eq!(
                inbox.len(),
                1,
                "FileInto {refused:?} falls back to the Accept disposition's INBOX"
            );
        }
        // Nor is the held mailbox created for an account no hold reached: the
        // name is reserved for a first hold.
        let mailboxes: Vec<String> = state
            .db
            .list_bridge_imap_mailbox_state(&[44u8; 32])
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(
            !mailboxes.iter().any(|m| m == GUARDIAN_HELD_MAILBOX),
            "a FileInto verdict must not create the held mailbox; got {mailboxes:?}"
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_filter_extra_flags_set_label_keyword() {
        // T3.3: a matched filter rule's `AddLabel { label }` verdict rides the
        // ingest `extra_flags` field and is set as an IMAP keyword on the placed
        // message; placement stays on the spam disposition (INBOX here).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"newsletter");
        req.spam_disposition = SpamDisposition::Accept;
        req.extra_flags = vec!["Newsletter".to_string()];
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("filter AddLabel delivery succeeds");

        let inbox = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1, "AddLabel delivers normally (to INBOX)");
        assert!(
            inbox[0].1.split_whitespace().any(|f| f == "Newsletter"),
            "the AddLabel keyword must be set on the placed message; flags = {:?}",
            inbox[0].1
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_drops_unsafe_extra_flags() {
        // EF-1 defense-in-depth: a system-flag / whitespace `extra_flags` token
        // (which create-time validation now rejects, but a pre-existing or
        // hand-edited rule could still carry) is dropped before it becomes an
        // IMAP flag — only the valid keyword survives. Guards against `\Deleted`
        // (EXPUNGE-eligible) / `\Seen` (silently read) ever reaching placement.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"newsletter");
        req.spam_disposition = SpamDisposition::Accept;
        req.extra_flags = vec![
            "\\Deleted".to_string(),
            "Newsletter".to_string(),
            "two words".to_string(),
        ];
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("delivery succeeds with the unsafe flags filtered out");

        let inbox = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1, "message still delivers");
        let flags: Vec<&str> = inbox[0].1.split_whitespace().collect();
        assert!(
            flags.contains(&"Newsletter"),
            "the valid keyword survives; flags = {:?}",
            inbox[0].1
        );
        assert!(
            !flags.contains(&"\\Deleted") && !flags.iter().any(|f| f == &"two" || f == &"words"),
            "no system flag / split-whitespace token reaches placement; flags = {:?}",
            inbox[0].1
        );
    }

    #[tokio::test]
    async fn submit_inbound_mail_over_storage_quota_still_delivers() {
        // Own-submission (the Sent copy) is NOT a quota enforcement point —
        // imap-server.md § Quota enforcement points lists APPEND/COPY/MOVE/
        // inbound-delivery, not submission; § Composition (:278) confirms
        // per-mailbox quota stops APPEND + inbound delivery, while submission
        // has its own separate (rate-based) quota. A user at their storage cap
        // can still send: the Sent copy is admitted (the pre-check's documented
        // slight over-admission).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        // A ceiling the submission body would exceed if it were checked — it
        // isn't (submission is not a quota enforcement point), so it delivers.
        lower_storage_ceiling(&state, 10).await;

        let req = sample_ingest_request(&target, b"my-own-sent-copy");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("own-submission Sent copy is exempt from the per-mailbox quota");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.message_id.len(), 32);
    }

    #[tokio::test]
    async fn ingest_inbound_mail_writes_scan_result_row() {
        use fauna_protocol::bridge_routing::{RspamdRuleContribution, RspamdScore};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        // An infected message filed to Junk by the `junk` action, carrying an rspamd score.
        let mut req = sample_ingest_request(&target, b"scanned-body");
        req.clamav_verdict = ClamavVerdict::Infected {
            signature: "Eicar-Test-Signature".into(),
        };
        req.spam_disposition = SpamDisposition::PolicyJunk;
        req.rspamd_score = Some(RspamdScore {
            raw_milli: 2400,
            scaled_milli: 1200,
            flagged_rules: vec!["BAYES_HAM".into(), "URIBL_BLACK".into()],
            breakdown: vec![
                RspamdRuleContribution {
                    rule: "BAYES_HAM".into(),
                    score_milli: -2900,
                },
                RspamdRuleContribution {
                    rule: "URIBL_BLACK".into(),
                    score_milli: 5400,
                },
            ],
        });
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        let row = state
            .db
            .get_scan_result(&mid)
            .await
            .unwrap()
            .expect("scan-result row written on ingest");
        assert_eq!(row.clamav_verdict, "infected");
        assert_eq!(
            row.clamav_signature.as_deref(),
            Some("Eicar-Test-Signature")
        );
        // Infected + PolicyJunk disposition → junked (derived nest-side).
        assert_eq!(row.action_taken, "junked");
        assert_eq!(row.rspamd_score_raw, Some(2400));
        assert_eq!(row.rspamd_score_scaled, Some(1200));
        assert_eq!(
            row.rspamd_flagged_rules.as_deref(),
            Some(r#"["BAYES_HAM","URIBL_BLACK"]"#)
        );
        assert_eq!(row.delivered_to_actor, Some(target));
    }

    #[tokio::test]
    async fn ingest_inbound_mail_derives_nothing_from_the_per_kind_fields() {
        // The contract phase (content-scoring.md § The scoring-metadata bus):
        // the bus rows are what the perimeter sent, full stop. A request that
        // carries scored per-kind fields but no `scores` lands NO bus rows —
        // the per-kind fields are the detail record beside the rows, never
        // their source. Re-adding a nest-side derivation reds this.
        use fauna_protocol::bridge_routing::RspamdScore;
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"scored-body");
        req.spam_score = 875;
        req.spam_disposition = SpamDisposition::AcceptToSpamFolder;
        req.rspamd_score = Some(RspamdScore {
            raw_milli: 2400,
            scaled_milli: 1200,
            flagged_rules: vec![],
            breakdown: vec![],
        });
        assert!(req.scores.is_empty(), "per-kind-only wire shape under test");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        // The detail record landed from the per-kind fields …
        let scan = state
            .db
            .get_scan_result(&mid)
            .await
            .unwrap()
            .expect("scan-result row written from the per-kind fields");
        assert_eq!(scan.rspamd_score_scaled, Some(1200));
        // … and the bus holds exactly what the wire carried: nothing.
        let rows = state.db.get_content_scores(&mid).await.unwrap();
        assert!(rows.is_empty(), "nest-side derivation is retired: {rows:?}");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_honors_wire_supplied_scores() {
        // The wire `scores` array is stored verbatim — a factor with no
        // per-kind field rides it like any other, and the nest overlays
        // nothing from the per-kind fields beside it.
        use fauna_core::scoring::{ScoreEntry, TIER_COMMUNITY};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"wire-scored-body");
        req.scores = vec![ScoreEntry {
            factor: "phishing".into(),
            score: 990,
            tier: TIER_COMMUNITY,
            scorer_version: 3,
        }];
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        let rows = state.db.get_content_scores(&mid).await.unwrap();
        assert_eq!(rows.len(), 1, "wire rows used as-is, no merge: {rows:?}");
        assert_eq!(rows[0].factor, "phishing");
        assert_eq!(rows[0].score, 990);
        assert_eq!(rows[0].tier, TIER_COMMUNITY);
        assert_eq!(rows[0].scorer_version, 3);
    }

    #[tokio::test]
    async fn submit_inbound_mail_writes_no_content_scores() {
        // An own-submission Sent copy was never perimeter-scored; recording
        // all-default rows would be noise — the bus stays empty for it.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"my-own-sent-copy");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        assert!(
            state.db.get_content_scores(&mid).await.unwrap().is_empty(),
            "own-submission gets no bus rows"
        );
    }

    #[tokio::test]
    async fn submit_inbound_mail_not_scanned_writes_no_scan_result_row() {
        // The sender's own Sent copy never reaches the scan pipeline
        // (submission is not ClamAV-scanned): the wire carries `NotScanned`,
        // and the nest records no `message_scan_results` row — until
        // 2026-09-28 it recorded a `'clean'` verdict nobody computed.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"my-own-sent-copy");
        assert_eq!(req.clamav_verdict, ClamavVerdict::NotScanned);
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        assert!(
            state.db.get_scan_result(&mid).await.unwrap().is_none(),
            "a message no scanner touched gets no scan-result row"
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_not_scanned_twin_records_no_clamav_verdict() {
        // The submission twin — a colleague's copy of a locally submitted
        // message (`fauna_recipient.go`) — arrives over `ingest_inbound_mail`
        // with `NotScanned`, no rspamd score, and the bus rows the ONE shared
        // mapping mints from exactly those inputs. Neither plane may claim a
        // ClamAV scan: no scan-result row, no `clamav` bus row.
        use fauna_core::scoring::perimeter_mail_score_rows;
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"colleague-twin");
        req.scores = perimeter_mail_score_rows(0, &req.clamav_verdict, None, &req.verdicts);
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        assert!(
            state.db.get_scan_result(&mid).await.unwrap().is_none(),
            "the twin never reached the scan pipeline: no scan-result row"
        );
        let rows = state.db.get_content_scores(&mid).await.unwrap();
        assert!(
            !rows.is_empty(),
            "the twin still carries its spam + auth rows"
        );
        assert!(
            rows.iter().all(|r| r.factor != "clamav"),
            "a scorer that did not run emits no row: {rows:?}"
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_not_scanned_beside_rspamd_keeps_the_row() {
        // ClamAV off, rspamd on: the message DID reach the scan pipeline, so
        // rspamd's detail record is kept and the ClamAV column says honestly
        // that it did not run.
        use fauna_protocol::bridge_routing::RspamdScore;
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"rspamd-only");
        req.rspamd_score = Some(RspamdScore {
            raw_milli: 2400,
            scaled_milli: 1200,
            flagged_rules: vec!["BAYES_HAM".into()],
            breakdown: vec![],
        });
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        let row = state
            .db
            .get_scan_result(&mid)
            .await
            .unwrap()
            .expect("rspamd ran: the detail record is kept");
        assert_eq!(row.clamav_verdict, "not_scanned");
        assert_eq!(row.clamav_signature, None);
        assert_eq!(row.action_taken, "delivered");
        assert_eq!(row.rspamd_score_scaled, Some(1200));
    }

    #[tokio::test]
    async fn ingest_inbound_mail_persists_and_returns_message_id() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"encrypted-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.message_id.len(), 32);

        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        assert!(
            state
                .db
                .segment_records_lookup_record(
                    &target,
                    "mail",
                    &fauna_cbor::Cid::from_digest_dag_cbor(mid)
                )
                .await
                .unwrap()
                .is_some(),
            "segment_records row in the owning scope"
        );
        let (envelope, floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("record present");
        // Stored verbatim at rest: the outer envelope carries the exact sealed
        // wire body (S6.12b — the ingest handler stores what `verify` accepted).
        assert_eq!(envelope.encrypted_body, req.encrypted_body);
        assert_eq!(floor.spam_disposition, "accept");
        assert!(!floor.is_own_submission);
    }

    /// Phase-3 D1 (sealed at rest in BOTH modes): even a committed
    /// **plaintext-mode** nest seals an in-domain mail delivery at ingest —
    /// `seal_and_persist_local`'s design-(b) no-seal branch is deleted, so the
    /// stored payload is `seal_recipient_blob` output in both storage modes
    /// (one at-rest byte shape). Authority: `encryption-at-rest.md`
    /// § Plaintext ceiling per mode (Phase 3, S1);
    /// `2026-07-07-phase-3-sealed-both-modes-design.md` D1. (This test pinned
    /// the retired no-seal-at-ingest behavior until 2026-07-08; the Phase-3
    /// commit deleted the branch but missed this lib pin — silent red. It was
    /// fixed independently twice the same day; this is the merged
    /// superset of both assertions.)
    #[tokio::test]
    async fn in_domain_delivery_seals_body() {
        let state = fixture_state().await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let raw: &[u8] =
            b"From: a@ex.com\r\nTo: b@fauna.test\r\nSubject: hi\r\n\r\nplaintext body bytes\r\n";
        seal_and_ingest_local(
            &state,
            &target,
            raw,
            "ex.com",
            MailIngress::Sender("a@ex.com"),
            &[],
        )
        .await
        .expect("in-domain delivery");

        // Read the actor's single stored record back; its payload is a sealed
        // `MailRecordEnvelope` (one at-rest byte shape, both modes) — never
        // the literal plaintext RFC-5322.
        let records =
            crate::segments::mail::read_after_seq(&state.mail_segments, &state.db, &target, 0, 10)
                .await
                .unwrap();
        assert_eq!(records.len(), 1, "one record delivered");
        let mid = records[0].1;
        let (envelope, _floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("record present");
        assert_ne!(
            envelope.encrypted_body.as_slice(),
            raw,
            "plaintext mode seals the body at ingest (Phase-3 D1) — never verbatim"
        );
        assert!(
            !envelope
                .encrypted_body
                .windows(b"plaintext body bytes".len())
                .any(|w| w == b"plaintext body bytes"),
            "the plaintext body must not appear anywhere in the sealed payload"
        );
        assert!(
            fauna_mls::wrapped_blob::is_sealed_mail_record(&envelope.encrypted_body),
            "stored payload is a sealed mail record (per-record strict-shape discrimination)"
        );
    }

    /// `mailbox-migration.md` § The envelope key confirms a Message-ID hit →
    /// *Producers send the pair, always — all four of them*: the nest's own
    /// in-domain delivery is the fourth producer. It mints the pair from the
    /// UNSTAMPED raw message — the per-recipient delivery stamps are prepended
    /// after — so every recipient of one message records the same pair, exactly
    /// as the Go MTA does for external-MX delivery. Record-only, like every
    /// delivery door: an identical second send is stored too, and the index
    /// keeps its one first-writer row.
    #[tokio::test]
    async fn in_domain_delivery_mints_the_dedup_pair_from_the_unstamped_raw() {
        let state = fixture_state().await;
        let target = [47u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let raw: &[u8] = b"From: a@fauna.test\r\nTo: b@fauna.test\r\nSubject: hi\r\n\
            Date: Thu, 01 Oct 2026 10:00:00 +0000\r\n\
            Message-ID: <in-domain-1@fauna.test>\r\n\r\nbody\r\n";
        let want = fauna_mail::dedup_key::mail_dedup_keys_from_slice(raw);
        let stamps = vec![(
            "X-Fauna-Address-Used".to_string(),
            "b@fauna.test".to_string(),
        )];
        for _ in 0..2 {
            seal_and_ingest_local(
                &state,
                &target,
                raw,
                "fauna.test",
                MailIngress::Sender("a@fauna.test"),
                &stamps,
            )
            .await
            .expect("in-domain delivery");
        }

        assert_eq!(
            state
                .db
                .dedup_envelope_key(&target, &want.dedup_key)
                .await
                .unwrap(),
            Some(want.envelope_key),
            "the in-domain delivery records the pair of the unstamped message"
        );
        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "both sends delivered; a hit never suppresses"
        );
    }

    /// The sender's server-side Sent copy goes through the same builder and
    /// mints the same pair, so re-importing the account the mail was sent from
    /// dedup-hits the copy already held.
    #[tokio::test]
    async fn sent_copy_mints_the_dedup_pair() {
        let state = fixture_state().await;
        let sender = [48u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &sender,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let raw: &[u8] = b"From: a@fauna.test\r\nTo: x@remote.example\r\nSubject: out\r\n\
            Message-ID: <sent-1@fauna.test>\r\n\r\nbody\r\n";
        let want = fauna_mail::dedup_key::mail_dedup_keys_from_slice(raw);
        seal_and_store_sent_copy(&state, &sender, raw, "fauna.test")
            .await
            .expect("sent copy");

        assert_eq!(
            state
                .db
                .dedup_envelope_key(&sender, &want.dedup_key)
                .await
                .unwrap(),
            Some(want.envelope_key)
        );
    }

    #[tokio::test]
    async fn ingest_inbound_mail_is_idempotent_on_retry() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"encrypted-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let r1 = ingest_inbound_mail_handler()(state.clone(), mta, payload.clone())
            .await
            .unwrap();
        let r2 = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .unwrap();
        let id1: IngestInboundMailReply = decode(&r1).unwrap();
        let id2: IngestInboundMailReply = decode(&r2).unwrap();
        assert_eq!(id1.message_id, id2.message_id);
    }

    // ── T7 completion: MTA-path placement-journal wiring tests ──────
    //
    // Spec § D2 / § D6 (ε): both `ingest_inbound_mail` (MX inbound) and
    // `submit_inbound_mail` (own submission) must emit a
    // `MailPlacementRecord::Append` after the placement row is written,
    // mirroring the IMAP-APPEND handler wired in T7.

    #[tokio::test]
    async fn ingest_inbound_mail_produces_append_placement_record() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Unique actor per test — MailPlacementSegmentManager keeps a
        // per-actor manifest, and the fixture uses a process-unique
        // tempdir; fresh actor keeps tests from bleeding state.
        let target = [0xC0u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"inbound-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.placements.len(),
            1,
            "exactly one placement after one MTA ingest"
        );
        let p = &manifest.placements[0];
        assert_eq!(p.mailbox, "INBOX", "Accept disposition routes to INBOX");
        assert_eq!(p.content_record_id, reply.message_id);
        assert!(p.modseq >= 1, "modseq is at least 1");
        // No flags on default Accept ingest (initial_flags == "").
        assert!(p.flags.is_empty(), "no initial flags on inbound Accept");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_idempotent_retry_does_not_duplicate_placement_record() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [0xC1u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"inbound-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        // Two identical RPCs — the second hits the `content_was_new == false`
        // branch in `place_inbound_mail` and returns `Ok(None)`, so the
        // handler must NOT emit a second Append record.
        let r1 = ingest_inbound_mail_handler()(state.clone(), mta, payload.clone())
            .await
            .unwrap();
        let r2 = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .unwrap();
        let id1: IngestInboundMailReply = decode(&r1).unwrap();
        let id2: IngestInboundMailReply = decode(&r2).unwrap();
        assert_eq!(id1.message_id, id2.message_id, "same message_id on retry");

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.placements.len(),
            1,
            "idempotent retry must not duplicate the Append record"
        );
    }

    #[tokio::test]
    async fn submit_inbound_mail_produces_append_in_sent_with_seen_flag() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [0xC2u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"submit-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();

        let manifest = state
            .mail_placement
            .current_manifest(&target)
            .await
            .expect("placement manifest");
        assert_eq!(
            manifest.placements.len(),
            1,
            "exactly one placement after one own-submission"
        );
        let p = &manifest.placements[0];
        assert_eq!(p.mailbox, "Sent", "own-submission routes to Sent");
        assert_eq!(p.content_record_id, reply.message_id);
        assert_eq!(
            p.flags,
            vec!["\\Seen".to_string()],
            "own-submission seeds the \\Seen flag"
        );
    }

    #[tokio::test]
    async fn report_rejected_scan_writes_forensic_row_and_is_idempotent() {
        use fauna_protocol::bridge_routing::{
            ReportRejectedScanReply, ReportRejectedScanRequest, RspamdScore,
        };
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = ReportRejectedScanRequest {
            clamav_signature: "Eicar-Test-Signature".into(),
            rspamd_score: Some(RspamdScore {
                raw_milli: 9000,
                scaled_milli: 4500,
                flagged_rules: vec!["VIRUS".into()],
                breakdown: vec![],
            }),
            received_at: 1_700_000_000,
            sender_domain: "evil.example".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = report_rejected_scan_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: ReportRejectedScanReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();

        let row = state
            .db
            .get_scan_result(&mid)
            .await
            .unwrap()
            .expect("forensic row written");
        assert_eq!(row.clamav_verdict, "infected");
        assert_eq!(
            row.clamav_signature.as_deref(),
            Some("Eicar-Test-Signature")
        );
        // Reject-at-perimeter: never delivered to an actor (scopes it into the
        // admin forensic view, out of any per-user "my scan results" query).
        assert_eq!(row.action_taken, "rejected_malware");
        assert_eq!(row.delivered_to_actor, None);
        assert_eq!(row.rspamd_score_scaled, Some(4500));

        // Idempotent on retry — deterministic synthetic id ⇒ same id back, and
        // `insert_scan_result` is INSERT OR REPLACE on that key (so no
        // duplicate row; the db-layer test `insert_scan_result_round_trips_and_
        // is_idempotent` covers the no-duplicate guarantee directly).
        let payload2 = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes2 = report_rejected_scan_handler()(state.clone(), mta, payload2)
            .await
            .expect("retry ok");
        let reply2: ReportRejectedScanReply = decode(&bytes2).unwrap();
        assert_eq!(reply2.message_id, reply.message_id);
    }

    #[tokio::test]
    async fn report_rejected_scan_requires_mta_class() {
        use fauna_protocol::bridge_routing::ReportRejectedScanRequest;
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = ReportRejectedScanRequest {
            clamav_signature: "X".into(),
            rspamd_score: None,
            received_at: 1,
            sender_domain: "d.example".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = report_rejected_scan_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_requires_mta_class() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_unknown_actor_denied() {
        let state = fixture_state().await;
        let stranger = [123u8; 32];
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_malformed_actor_id() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let mut req = sample_ingest_request(&[0u8; 32], b"body");
        req.actor_id = vec![1u8; 16];
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_empty_body() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let mut req = sample_ingest_request(&target, b"x");
        req.encrypted_body = vec![];
        req.public_metadata.ciphertext_size = 0;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// S6.12b structural seal gate (mail ingest): `persist_inbound_mail_request`
    /// proves both payload halves are sealed recipient envelopes at the wire
    /// edge. A raw RFC 5322 body — with a genuinely sealed hint — is refused as
    /// malformed, so nothing unsealed reaches the backup-eligible `__mail`
    /// segment store via the ingest RPC.
    #[tokio::test]
    async fn ingest_rejects_an_unsealed_body() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        // A genuine sealed hint from the helper; only the body is raw, so the
        // body verify (not the hint verify) is the check that fires.
        let mut req = sample_ingest_request(&target, b"placeholder");
        let raw_body = b"From: a@b.example\r\n\r\nnot a sealed envelope\r\n".to_vec();
        req.public_metadata.ciphertext_size = raw_body.len() as u32;
        req.encrypted_body = raw_body;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .expect_err("an unsealed ingest body must be rejected");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_empty_index_hint() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let mut req = sample_ingest_request(&target, b"body");
        req.encrypted_index_hint = vec![];
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_size_mismatch() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let mut req = sample_ingest_request(&target, b"actual-bytes");
        req.public_metadata.ciphertext_size = 9999;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_empty_sender_domain() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let mut req = sample_ingest_request(&target, b"body");
        req.public_metadata.sender_domain = "   ".into();
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_rejects_recipient_without_pubkey() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [99u8; 32]; // no seal key seeded

        let req = sample_ingest_request(&target, b"body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = ingest_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn ingest_inbound_mail_records_policy_junk_disposition() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"junk-body");
        req.spam_disposition = SpamDisposition::PolicyJunk;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .unwrap();
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        let (_env, floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("record present");
        assert_eq!(floor.spam_disposition, "policy_junk");
    }

    #[tokio::test]
    async fn submit_inbound_mail_persists_with_own_submission_flag() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"submit-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        assert!(
            state
                .db
                .segment_records_lookup_record(
                    &target,
                    "mail",
                    &fauna_cbor::Cid::from_digest_dag_cbor(mid)
                )
                .await
                .unwrap()
                .is_some(),
            "segment_records row in the owning scope"
        );
        let (envelope, floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("record present");
        // Stored verbatim at rest — the exact sealed wire body (S6.12b).
        assert_eq!(envelope.encrypted_body, req.encrypted_body);
        assert!(floor.is_own_submission, "submit must flip the flag to true");
    }

    #[tokio::test]
    async fn submit_inbound_mail_is_idempotent_on_retry() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"submit-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let r1 = submit_inbound_mail_handler()(state.clone(), mta, payload.clone())
            .await
            .unwrap();
        let r2 = submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .unwrap();
        let id1: IngestInboundMailReply = decode(&r1).unwrap();
        let id2: IngestInboundMailReply = decode(&r2).unwrap();
        assert_eq!(id1.message_id, id2.message_id);
    }

    #[tokio::test]
    async fn submit_inbound_mail_requires_mta_class() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_inbound_mail_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn submit_inbound_mail_passes_through_shared_validation() {
        // Smoke test that the shared persist helper's validation fires
        // for submit too. (B.2's ingest tests cover every validation
        // branch; this one confirms the submit path reaches the same
        // code via the shared helper.)
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        let mut req = sample_ingest_request(&target, b"x");
        req.encrypted_body = vec![];
        req.public_metadata.ciphertext_size = 0;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = submit_inbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn submit_inbound_mail_cross_kind_collision_keeps_first_writer_flag() {
        // If submit and ingest both fire with byte-identical fields (a
        // theoretical edge case — MLS encryption nonces make this
        // astronomically unlikely in practice), INSERT OR IGNORE on the
        // PK keeps the first writer's row. Document the behaviour: the
        // second call's `is_own_submission` is dropped silently.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"shared-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        // submit first → row written with is_own_submission=true.
        let _ = submit_inbound_mail_handler()(state.clone(), mta, payload.clone())
            .await
            .unwrap();
        // ingest second with the same bytes → INSERT OR IGNORE skips,
        // row unchanged.
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .unwrap();
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        let mid: [u8; 32] = reply.message_id.as_slice().try_into().unwrap();
        let (_env, floor) = crate::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &target,
            &mid,
        )
        .await
        .unwrap()
        .expect("record present");
        assert!(floor.is_own_submission, "first writer (submit) wins");
    }

    // ── C.0 IMAP placement tests ────────────────────────────────────────────

    #[tokio::test]
    async fn ingest_accept_message_lands_in_inbox() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [42u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"ingest-accept-body");
        let ts = req.public_metadata.timestamp;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: IngestInboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.message_id.len(), 32);

        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one placement row in INBOX");
        let (uid, flags, internal_date) = &rows[0];
        assert_eq!(*uid, 1u32);
        assert_eq!(flags, "", "Accept: no flags");
        // INTERNALDATE is the nest's own receipt instant, NOT the sender's
        // `Date:` header that `public_metadata.timestamp` carries (the sample
        // request's is 2023-11-14). See
        // `internaldate_is_the_nest_receipt_clock_not_the_senders_date_header`.
        assert!(
            *internal_date > ts,
            "internal_date ({internal_date}) must be the nest's own clock, \
             not the sender-reported timestamp ({ts})"
        );
    }

    /// **INTERNALDATE is nest-authenticated, never sender-supplied**
    /// (`imap-server.md` § SEARCH → *INTERNALDATE is the nest's own receipt
    /// time*). A sender writes their own RFC 5322 `Date:` header and nothing
    /// in SMTP requires it to be truthful; the MTA bridge parses it into
    /// `PublicMailMetadata.timestamp`. If that value reached
    /// `bridge_imap_messages.internal_date`, a backdated header would hide a
    /// just-arrived message from `SEARCH SINCE` and a future-dated one would
    /// surface an old message as recent — the sender, not the server, would be
    /// deciding what "mail from the last week" means in every MUA.
    ///
    /// Both directions are exercised from one call each, and the assertion is
    /// a **causal bracket** rather than a wall-clock budget (e2e convention
    /// 14): the placement must land inside the `[before, after]` window this
    /// test itself observed around the handler call, whatever the machine load.
    #[tokio::test]
    async fn internaldate_is_the_nest_receipt_clock_not_the_senders_date_header() {
        for (label, header_date) in [
            // Well before the nest's clock — the message an honest `SEARCH
            // SINCE last-week` would wrongly miss.
            ("far-past", 1_000_000_000_i64),
            // Well after it — the message that would wrongly pin itself to the
            // top of an INTERNALDATE-sorted mailbox forever.
            ("far-future", 4_000_000_000_i64),
        ] {
            let state = fixture_state().await;
            let mta = [1u8; 32];
            approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
            let target = [77u8; 32];
            crate::test_support::seed_recipient_seal_key(
                &state.db,
                &target,
                &crate::test_support::FIXTURE_MSEK,
            )
            .await;

            let mut req = sample_ingest_request(&target, label.as_bytes());
            req.public_metadata.timestamp = header_date;
            let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

            let before = crate::db::now_epoch_secs();
            ingest_inbound_mail_handler()(state.clone(), mta, payload)
                .await
                .expect("handler ok");
            let after = crate::db::now_epoch_secs();

            let rows = state
                .db
                .list_bridge_imap_messages(&target, "INBOX")
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "{label}: one placement row in INBOX");
            let internal_date = rows[0].2;
            assert!(
                (before..=after).contains(&internal_date),
                "{label}: internal_date {internal_date} must sit in the receipt \
                 window [{before}, {after}] this test bracketed the handler \
                 call with — the sender said {header_date}"
            );

            // The consequence that matters to a MUA: SEARCH SINCE/BEFORE
            // bracket on the nest's clock, so a window that contains the
            // delivery finds the message and one that contains only the
            // sender's claimed date does not.
            use crate::db::bridge_imap::SearchTermDb;
            let since_receipt = state
                .db
                .search_bridge_imap_messages(
                    &target,
                    "INBOX",
                    &[SearchTermDb::SinceInternalDate(before)],
                )
                .await
                .unwrap();
            assert_eq!(
                since_receipt.len(),
                1,
                "{label}: SINCE the receipt instant must match the delivery"
            );
            let before_receipt = state
                .db
                .search_bridge_imap_messages(
                    &target,
                    "INBOX",
                    &[SearchTermDb::BeforeInternalDate(before)],
                )
                .await
                .unwrap();
            assert_eq!(
                before_receipt.len(),
                0,
                "{label}: BEFORE the receipt instant must not match it"
            );
            let before_claimed = state
                .db
                .search_bridge_imap_messages(
                    &target,
                    "INBOX",
                    &[SearchTermDb::BeforeInternalDate(header_date + 1)],
                )
                .await
                .unwrap();
            assert_eq!(
                before_claimed.len(),
                usize::from(header_date > after),
                "{label}: the sender's claimed date decides nothing — the \
                 window matches only when it happens to contain the real \
                 receipt instant too"
            );
        }
    }

    #[tokio::test]
    async fn ingest_accept_to_spam_folder_lands_in_junk() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [43u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let mut req = sample_ingest_request(&target, b"spam-body");
        req.spam_disposition = SpamDisposition::AcceptToSpamFolder;
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let inbox_rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(
            inbox_rows.len(),
            0,
            "AcceptToSpamFolder must NOT land in INBOX"
        );

        let junk_rows = state
            .db
            .list_bridge_imap_messages(&target, "Junk")
            .await
            .unwrap();
        assert_eq!(junk_rows.len(), 1, "AcceptToSpamFolder lands in Junk");
        assert_eq!(junk_rows[0].0, 1u32, "uid=1 in Junk");
    }

    #[tokio::test]
    async fn submit_lands_in_sent_with_seen_flag() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [44u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"submit-sent-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        submit_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let sent_rows = state
            .db
            .list_bridge_imap_messages(&target, "Sent")
            .await
            .unwrap();
        assert_eq!(sent_rows.len(), 1, "submit must land in Sent");
        assert_eq!(sent_rows[0].0, 1u32, "uid=1 in Sent");
        assert!(
            sent_rows[0].1.contains("\\Seen"),
            "Sent message must have \\Seen flag; got: {:?}",
            sent_rows[0].1
        );
    }

    #[tokio::test]
    async fn ingest_retry_does_not_duplicate_placement_row() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let target = [45u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &target,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        let req = sample_ingest_request(&target, b"retry-body");
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

        // First ingest.
        ingest_inbound_mail_handler()(state.clone(), mta, payload.clone())
            .await
            .expect("first call ok");

        // Retry (byte-identical).
        ingest_inbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("retry ok");

        let rows = state
            .db
            .list_bridge_imap_messages(&target, "INBOX")
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "retry must NOT create a second placement row"
        );

        // uid_next should still be 2 (first ingest got uid 1, retry was no-op).
        let conn = state.db.conn().await;
        let uid_next: i64 = conn
            .query_row(
                "SELECT uid_next FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&target[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            uid_next, 2,
            "uid_next must be 2 after one successful ingest"
        );
    }

    // ── whoami handler ────────────────────────────────────────

    #[tokio::test]
    async fn whoami_returns_role_and_identity_for_approved_mta() {
        let state = fixture_state().await;
        let mta = [7u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = whoami_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: WhoamiReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.role, "mta");
        assert_eq!(reply.bridge_id, "b1");
        assert_eq!(reply.status, "approved");
        assert_eq!(reply.ed25519_pubkey_hex, hex::encode(mta));
        // approve_bridge writes [9u8; 32] as the x25519 pubkey.
        assert_eq!(reply.x25519_pubkey_hex, hex::encode([9u8; 32]));
        // No `domain` / `dkim_selector` fields on this reply — the
        // multi-domain track moved per-bridge accept-list to
        // fetch_config's `local_domains` projection.
    }

    #[tokio::test]
    async fn whoami_reply_does_not_carry_domain_field() {
        // Pin the wire-shape drift removal per
        // docs/goal/behavior/mail-bridge-lifecycle.md § Wire shapes
        // (target shape has no `domain` / `dkim_selector` keys).
        // Sibling tests cover the positive role/status/pubkey paths;
        // this one inspects the encoded bytes directly so a future
        // session that re-adds either field by mistake fails loudly.
        let state = fixture_state().await;
        let mta = [7u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = whoami_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        // CBOR encodes struct field names as text strings; if the
        // `domain` field were on the struct, the bytes "domain" would
        // appear verbatim in the output. The other reply fields
        // (`role`, `bridge_id`, etc.) contain no such substring.
        assert!(
            !reply_bytes.windows(6).any(|w| w == b"domain"),
            "WhoamiReply wire bytes must not contain the substring `domain`"
        );
        assert!(
            !reply_bytes.windows(13).any(|w| w == b"dkim_selector"),
            "WhoamiReply wire bytes must not contain the substring `dkim_selector`"
        );
    }

    #[tokio::test]
    async fn whoami_returns_mda_role_for_approved_mda() {
        let state = fixture_state().await;
        let mda = [8u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = whoami_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: WhoamiReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.role, "mda");
        assert_eq!(reply.status, "approved");
    }

    #[tokio::test]
    async fn whoami_reports_private_node_mode() {
        // Slice 3 (deployment-home-with-public-relay.md § Plaintext-mode
        // behavior): the nest's NAT axis must reach the bridge so the MDA
        // can pick a LAN-only (loopback) bind default for a private nest.
        // `whoami` is the channel — it's the bootstrap "ask nest who I am"
        // RPC the bridge already calls before binding any listener.
        let state = fixture_state_with_node_mode(crate::config::NodeMode::Private).await;
        let mda = [8u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = whoami_handler()(state, mda, payload)
            .await
            .expect("handler ok");
        let reply: WhoamiReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.node_mode, "private");
    }

    #[tokio::test]
    async fn whoami_reports_public_node_mode_by_default() {
        // The public NAT axis (fixture_state's default) keeps the current
        // all-interfaces MDA bind behavior — the public nest legitimately
        // serves IMAP/CalDAV publicly (subject to the Slice-2 per-actor
        // serving toggle).
        let state = fixture_state().await;
        let mta = [7u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = whoami_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: WhoamiReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.node_mode, "public");
    }

    #[tokio::test]
    async fn whoami_denies_non_bridge_actor() {
        // A stranger that's neither a service user nor an admin
        // resolves to CallerClass::User; the allowlist denies whoami
        // for that class.
        let state = fixture_state().await;
        let stranger = [201u8; 32];
        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = whoami_handler()(state, stranger, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn whoami_denies_pending_bridge() {
        // A bridge whose row is `pending` (not yet admin-approved)
        // resolves to None in caller_class_for_actor → permission
        // denied. The bridge will retry whoami after approval; we
        // intentionally do NOT leak status="pending" to a non-
        // authenticated peer here — only the regular HTTP enrollment
        // poll path surfaces that.
        let state = fixture_state().await;
        let pending_pk = [44u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&pending_pk, BridgeRole::Mta, "pending-1")
            .await
            .unwrap();
        let req = WhoamiRequest {};
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = whoami_handler()(state, pending_pk, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── I4 Phase D.5 outbound-queue handler tests ─────────────────

    use crate::db::outbound::OutboundStatus;

    async fn enqueue_one(state: &Arc<AppState>, msgid: &str, recipient: &str, raw: &[u8]) -> i64 {
        let recipients = [recipient];
        let ids = state
            .db
            .enqueue_outbound(NewOutbound {
                original_msgid: msgid,
                original_sender: "alice@example.com",
                recipients: &recipients,
                raw_message: raw,
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();
        ids[0]
    }

    #[tokio::test]
    async fn fetch_outbound_due_returns_due_rows_for_mta() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw\r\n").await;

        let req = FetchOutboundDueRequest {
            max: 8,
            lease_seconds: 30,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_outbound_due_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchOutboundDueReply = decode(&bytes).unwrap();
        assert_eq!(reply.units.len(), 1);
        assert_eq!(reply.units[0].id, id);
        assert_eq!(reply.units[0].recipient, "bob@dest.test");
        assert_eq!(reply.units[0].original_sender, "alice@example.com");
        assert_eq!(reply.units[0].raw_message, b"raw\r\n");
        assert_eq!(reply.units[0].attempt_count, 0);
    }

    #[tokio::test]
    async fn fetch_outbound_due_clamps_batch_to_ceiling() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Enqueue more than the ceiling so we can verify clamping.
        for i in 0..(FETCH_OUTBOUND_DUE_MAX_BATCH as i64 + 5) {
            enqueue_one(
                &state,
                &format!("msg-{i}"),
                &format!("r{i}@dest.test"),
                b"raw",
            )
            .await;
        }

        let req = FetchOutboundDueRequest {
            max: 9999,
            lease_seconds: 30,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_outbound_due_handler()(state, mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchOutboundDueReply = decode(&bytes).unwrap();
        assert_eq!(reply.units.len() as u32, FETCH_OUTBOUND_DUE_MAX_BATCH);
    }

    #[tokio::test]
    async fn fetch_outbound_due_rejects_zero_max() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = FetchOutboundDueRequest {
            max: 0,
            lease_seconds: 30,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_outbound_due_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_outbound_due_rejects_zero_lease_seconds() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = FetchOutboundDueRequest {
            max: 8,
            lease_seconds: 0,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_outbound_due_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn fetch_outbound_due_denies_mda_class() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = FetchOutboundDueRequest {
            max: 8,
            lease_seconds: 30,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = fetch_outbound_due_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── N3 SRS rewrite at queue-out ───────────────────────────────

    fn forwarded_row(id: i64, original_sender: &str) -> OutboundRow {
        OutboundRow {
            id,
            original_msgid: "<m@sender.test>".into(),
            original_sender: original_sender.into(),
            recipient: "bob@example.net".into(),
            raw_message: b"raw\r\n".to_vec(),
            attempt_count: 0,
            next_attempt_at: 0,
            delay_warned_at: None,
            status: OutboundStatus::Pending,
            last_error: None,
            last_enhanced: None,
            inbound_verdicts: InboundVerdictsSnapshot {
                spf: "none".into(),
                dmarc: "none".into(),
                dmarc_policy: "none".into(),
            },
            is_forwarded: true,
            forward_actor_id: Some([7u8; 32]),
            forward_rule_id: Some("forward-all".into()),
            created_at: 0,
        }
    }

    #[test]
    fn srs_rewrite_round_trips_to_row_id_and_original_sender() {
        let secret = [0xABu8; 32];
        let row = forwarded_row(42, "alice@example.net");
        let rewritten = srs_rewrite_outbound_sender(Some(&secret), Some("fauna.test"), 100, &row);
        assert!(rewritten.starts_with("SRS0="), "got {rewritten}");
        assert!(rewritten.ends_with("@fauna.test"), "got {rewritten}");
        // The payload short-id is the outbound row id; decode recovers it +
        // the original sender, so N4 can map row → forwarder actor + dest.
        let local_part = rewritten.strip_suffix("@fauna.test").unwrap();
        let dec = fauna_mail::srs::srs_decode(
            &secret,
            100,
            fauna_mail::srs::DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS,
            local_part,
        )
        .unwrap();
        assert_eq!(dec.forwarder_actor_id, "42");
        assert_eq!(dec.original_sender, "alice@example.net");
    }

    #[test]
    fn srs_rewrite_leaves_non_forwarded_and_misconfig_rows_untouched() {
        let secret = [0xABu8; 32];
        // A normal (non-forwarded) row: never rewritten.
        let mut normal = forwarded_row(1, "carol@x.test");
        normal.is_forwarded = false;
        normal.forward_actor_id = None;
        assert_eq!(
            srs_rewrite_outbound_sender(Some(&secret), Some("fauna.test"), 1, &normal),
            "carol@x.test"
        );
        // A forwarded row but the deployment has no SRS secret / no primary
        // domain → pass through (SPF-fails downstream, but never dropped).
        let fwd = forwarded_row(2, "dave@x.test");
        assert_eq!(
            srs_rewrite_outbound_sender(None, Some("fauna.test"), 1, &fwd),
            "dave@x.test"
        );
        assert_eq!(
            srs_rewrite_outbound_sender(Some(&secret), None, 1, &fwd),
            "dave@x.test"
        );
    }

    #[tokio::test]
    async fn fetch_outbound_due_srs_rewrites_only_forwarded_rows() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        state
            .db
            .add_mail_domain("example.com", true, "testing", "none", None, None)
            .await
            .unwrap();

        // One normal submission row + one forwarded row.
        let normal_id = enqueue_one(&state, "msg-normal", "bob@dest.test", b"raw\r\n").await;
        let forwarder = [7u8; 32];
        let fwd_recipients = ["downstream@ext.test"];
        let fwd_id = state
            .db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<fwd@sender.test>",
                original_sender: "alice@example.net",
                recipients: &fwd_recipients,
                raw_message: b"raw\r\n",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: true,
                forward_actor_id: Some(&forwarder),
                forward_rule_id: Some("forward-all"),
                forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                submit_actor_id: None,
            })
            .await
            .unwrap()[0];

        let req = FetchOutboundDueRequest {
            max: 16,
            lease_seconds: 30,
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = fetch_outbound_due_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: FetchOutboundDueReply = decode(&bytes).unwrap();

        let normal = reply.units.iter().find(|u| u.id == normal_id).unwrap();
        assert_eq!(
            normal.original_sender, "alice@example.com",
            "non-forwarded row's envelope is untouched"
        );
        let fwd = reply.units.iter().find(|u| u.id == fwd_id).unwrap();
        assert!(
            fwd.original_sender.starts_with("SRS0=")
                && fwd.original_sender.ends_with("@example.com"),
            "forwarded row dispatches under SRS at the primary domain: {}",
            fwd.original_sender
        );
        // The stored row keeps the original envelope (rewrite is queue-out only).
        let row = state
            .db
            .fetch_outbound_by_id(fwd_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.original_sender, "alice@example.net");
    }

    #[tokio::test]
    async fn mark_outbound_delivered_transitions_to_sent() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;

        let req = MarkOutboundDeliveredRequest { id };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = mark_outbound_delivered_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: MarkOutboundDeliveredReply = decode(&bytes).unwrap();
        assert!(reply.ok);
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, OutboundStatus::Sent);
    }

    #[tokio::test]
    async fn mark_outbound_failed_records_backoff_and_last_error() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;
        let before = now_epoch_secs();

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 600,
            last_error: "451 temporary".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: MarkOutboundFailedReply = decode(&bytes).unwrap();
        assert!(reply.ok);
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, OutboundStatus::Pending);
        assert_eq!(rows[0].attempt_count, 1);
        assert!(rows[0].next_attempt_at >= before + 600);
        assert_eq!(rows[0].last_error.as_deref(), Some("451 temporary"));
    }

    #[tokio::test]
    async fn mark_outbound_failed_clamps_retry_after() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;
        let before = now_epoch_secs();
        let huge = MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER * 100;

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: huge,
            last_error: "stall attempt".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        // Clamped to the ceiling; the row's next_attempt_at must not
        // exceed `before + MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER` plus a
        // small wallclock fudge.
        assert!(
            rows[0].next_attempt_at <= before + MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER as i64 + 5,
            "next_attempt_at={} should be clamped near {}",
            rows[0].next_attempt_at,
            before + MARK_OUTBOUND_FAILED_MAX_RETRY_AFTER as i64
        );
    }

    #[tokio::test]
    async fn mark_outbound_failed_respects_admin_retry_schedule_override() {
        // An admin-set `retry_schedule_seconds` override must actually
        // govern the reschedule delay — the handler's only production call
        // site used to pass the compile-time catalog default regardless of
        // any stored override (mail-policy-config.md § Implementation
        // status today, sweep-169 finding).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;
        let before = now_epoch_secs();

        state
            .db
            .put_outbound_policy(OutboundPolicyOverrides {
                retry_schedule_seconds: Some(vec![0, 60]),
                ..Default::default()
            })
            .await
            .unwrap();

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 0,
            last_error: "451 temporary".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        // The catalog default's first-retry delay is 300s (±10% jitter =
        // 270..330); the override's is 60s (±10% jitter = 54..66). Landing
        // in the override's window proves the admin's stored value reached
        // the scheduler, not just the struct.
        let delay = rows[0].next_attempt_at - before;
        assert!(
            (54..=66).contains(&delay),
            "next_attempt_at should honor the 60s override (delay={delay}s), \
             not the 300s catalog default"
        );
    }

    #[tokio::test]
    async fn mark_outbound_failed_zero_permanent_failure_timeout_falls_back_to_default() {
        // `permanent_failure_timeout_hours = 0` is a timing window, not an
        // allowance (mail-policy-config.md ruling 2): a literal 0 would
        // permanently fail every outbound message on its first attempt.
        // The effective() resolver must treat a stored 0 as unset and fall
        // back to the catalog's 120h ceiling.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;

        state
            .db
            .put_outbound_policy(OutboundPolicyOverrides {
                permanent_failure_timeout_hours: Some(0),
                ..Default::default()
            })
            .await
            .unwrap();

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 0,
            last_error: "451 temporary".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(
            rows[0].status,
            OutboundStatus::Pending,
            "a stored 0 must not permanently fail the message on its first \
             attempt — it must fall back to the catalog's default budget"
        );
        assert_eq!(rows[0].attempt_count, 1);
    }

    #[tokio::test]
    async fn mark_outbound_failed_emits_delay_warning_past_4h_once() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Row whose attempt history is 4 h+ old → the next reported failure
        // must emit the once-per-message 4.4.7 delay-warning DSN.
        let now = now_epoch_secs();
        let ids = state
            .db
            .enqueue_outbound_at(
                NewOutbound {
                    original_msgid: "warn-1",
                    original_sender: "alice@example.com",
                    recipients: &["bob@dest.test"],
                    raw_message:
                        b"From: alice@example.com\r\nTo: bob@dest.test\r\nSubject: hi\r\n\r\nbody",
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "pass".into(),
                        dmarc: "pass".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now - 4 * 3600 - 1,
            )
            .await
            .unwrap();
        let id = ids[0];

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 0,
            last_error: "451 4.7.1 greylisted".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        let orig = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(
            orig.status,
            OutboundStatus::Pending,
            "row reschedules, not terminal"
        );
        assert_eq!(orig.attempt_count, 1);
        assert!(
            orig.delay_warned_at.is_some(),
            "delay_warned_at set after warning"
        );
        let dsn = rows
            .iter()
            .find(|r| r.original_sender.is_empty() && r.recipient == "alice@example.com")
            .expect("null-sender delay-warning DSN enqueued");
        let body = String::from_utf8_lossy(&dsn.raw_message);
        assert!(body.contains("Action: delayed"), "{body}");
        assert!(body.contains("Status: 4.4.7"), "{body}");

        // A second failure on the same message must NOT re-warn.
        let req2 = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 0,
            last_error: "451 4.7.1 still greylisted".into(),
        };
        let payload2 = Bytes::from(encode_canonical(&req2).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload2)
            .await
            .expect("handler ok");
        let after = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(
            after
                .iter()
                .filter(|r| r.original_sender.is_empty())
                .count(),
            1,
            "must not enqueue a second delay-warning DSN (once-per-message)"
        );
    }

    #[tokio::test]
    async fn mark_outbound_failed_gives_up_and_bounces_past_budget() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // A row past the 5-day permanent-failure ceiling → the next reported
        // temporary failure must promote to a permanent-failure bounce, not
        // reschedule. nest (not the bridge) owns the give-up decision.
        let now = now_epoch_secs();
        let ids = state
            .db
            .enqueue_outbound_at(
                NewOutbound {
                    original_msgid: "giveup-1",
                    original_sender: "alice@example.com",
                    recipients: &["bob@dest.test"],
                    raw_message:
                        b"From: alice@example.com\r\nTo: bob@dest.test\r\nSubject: hi\r\n\r\nbody",
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "pass".into(),
                        dmarc: "pass".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now - 6 * 86_400,
            )
            .await
            .unwrap();
        let id = ids[0];

        let req = MarkOutboundFailedRequest {
            id,
            retry_after_seconds: 0,
            last_error: "451 4.7.0 try later".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_failed_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        let orig = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(
            orig.status,
            OutboundStatus::Bounced,
            "give-up bounces, not reschedules"
        );
        let dsn = rows
            .iter()
            .find(|r| r.original_sender.is_empty() && r.recipient == "alice@example.com")
            .expect("NDR row enqueued on give-up");
        let body = String::from_utf8_lossy(&dsn.raw_message);
        assert!(body.contains("Action: failed"), "{body}");
        // Give-up reason was a 4xx → generic permanent enhanced status.
        assert!(body.contains("Status: 5.0.0"), "{body}");
    }

    #[tokio::test]
    async fn mark_outbound_bounced_transitions_with_reason() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let id = enqueue_one(&state, "msg-1", "bob@dest.test", b"raw").await;

        let req = MarkOutboundBouncedRequest {
            id,
            reason: "550 5.1.1 mailbox unknown".into(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = mark_outbound_bounced_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: MarkOutboundBouncedReply = decode(&bytes).unwrap();
        assert!(reply.ok);
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows[0].status, OutboundStatus::Bounced);
        assert_eq!(
            rows[0].last_error.as_deref(),
            Some("550 5.1.1 mailbox unknown")
        );
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_inserts_one_row_per_recipient() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = EnqueueOutboundMailRequest {
            original_msgid: "abc@ex.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec!["bob@dest.test".into(), "carol@other.test".into()],
            raw_message: b"body\r\n".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = enqueue_outbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: EnqueueOutboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.ids.len(), 2);
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows.len(), 2);
        let recipients: Vec<String> = rows.iter().map(|r| r.recipient.clone()).collect();
        assert!(recipients.contains(&"bob@dest.test".to_string()));
        assert!(recipients.contains(&"carol@other.test".to_string()));
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_rejects_empty_recipients() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = EnqueueOutboundMailRequest {
            original_msgid: "abc@ex.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec![],
            raw_message: b"body".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_rejects_too_many_recipients() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let mut rcpts = Vec::with_capacity(ENQUEUE_OUTBOUND_MAX_RECIPIENTS + 1);
        for i in 0..=ENQUEUE_OUTBOUND_MAX_RECIPIENTS {
            rcpts.push(format!("r{i}@dest.test"));
        }
        let req = EnqueueOutboundMailRequest {
            original_msgid: "abc@ex.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: rcpts,
            raw_message: b"body".to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_rejects_empty_body() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = EnqueueOutboundMailRequest {
            original_msgid: "abc@ex.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: vec![],
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mta, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    // ── BridgeMda caller-scope (caldav-server.md § Server-side auto-schedule).
    // The MDA reaches `enqueue_outbound_mail` for the server-side auto-schedule
    // gateway, but only as the AUTH'd organizer it is fanning an iMIP out for:
    // `original_sender` must resolve (exact alias) to `on_behalf_of_actor`.

    #[tokio::test]
    async fn enqueue_outbound_mail_mda_permitted_for_own_organizer() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // The organizer the MDA is serving: their address resolves (the same
        // exact-alias path AUTH uses) to their actor.
        let organizer = [3u8; 32];
        state
            .db
            .put_exact_alias("example.com", "organizer", "exact", &organizer)
            .await
            .unwrap();
        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "organizer@example.com".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".to_vec(),
            on_behalf_of_actor: Some(organizer.to_vec()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = enqueue_outbound_mail_handler()(state.clone(), mda, payload)
            .await
            .expect("MDA enqueue as own organizer ok");
        let reply: EnqueueOutboundMailReply = decode(&bytes).unwrap();
        assert_eq!(reply.ids.len(), 1);
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recipient, "bob@dest.test");
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_mda_requires_on_behalf_of_actor() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "organizer@example.com".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: b"body".to_vec(),
            on_behalf_of_actor: None,
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_mda_denied_when_sender_actor_mismatch() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // `original_sender` resolves to the organizer actor, but the MDA claims
        // to be acting for a *different* actor → permission denied (a buggy or
        // compromised MDA cannot spoof another local user's From).
        let organizer = [3u8; 32];
        let other = [4u8; 32];
        state
            .db
            .put_exact_alias("example.com", "organizer", "exact", &organizer)
            .await
            .unwrap();
        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "organizer@example.com".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: b"body".to_vec(),
            on_behalf_of_actor: Some(other.to_vec()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_mda_denied_when_sender_not_local() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        // An external / non-existent From never resolves to a local actor, so
        // the MDA cannot enqueue as it however it sets on_behalf_of_actor.
        let claimed = [3u8; 32];
        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "stranger@external.test".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: b"body".to_vec(),
            on_behalf_of_actor: Some(claimed.to_vec()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = enqueue_outbound_mail_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── deliver_sealed_scheduling — the MDA mailbox-less auto-schedule rail
    //    (caldav-server.md § Server-side auto-schedule, C3). Same BridgeMda
    //    caller-scope as enqueue_outbound_mail; the sealed welcome + iMIP bytes
    //    are OPAQUE to the nest (it never decrypts them), so these handler-level
    //    tests pass arbitrary bytes — the MLS crypto round-trip (join + drain +
    //    apply) is proven separately by the tier_3
    //    `conformance_caldav_scheduling_mailbox_less.rs`.

    /// A BridgeMda-class actor + an organizer whose `local@domain` resolves
    /// (exact alias) to its actor — the precondition every reaching call meets.
    async fn mda_and_organizer(state: &Arc<AppState>) -> ([u8; 32], [u8; 32]) {
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let organizer = [3u8; 32];
        state
            .db
            .put_exact_alias("example.com", "organizer", "exact", &organizer)
            .await
            .unwrap();
        (mda, organizer)
    }

    fn deliver_sealed_scheduling_req(
        organizer: &[u8; 32],
        recipient: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> DeliverSealedSchedulingRequest {
        DeliverSealedSchedulingRequest {
            on_behalf_of_actor: organizer.to_vec(),
            original_sender: "organizer@example.com".into(),
            recipient_actor_id: hex::encode(recipient),
            peer_domain: None,
            channel_id: hex::encode(channel_id),
            welcome_bytes: b"opaque-sealed-welcome".to_vec(),
            // A real MDA seals the iMIP into a dag-cbor `ChannelEnvelope` whose
            // inner bytes are AEAD output; the strict ingest verifier (now the
            // only arm — Phase 4) decodes the envelope and enforces the AEAD
            // shape floor. An opaque byte-string fixture only ever passed because
            // `AppState::for_test` used to install the permissive plaintext arm.
            app_envelope: fauna_mls::types::ChannelEnvelope::Application(vec![0xC7u8; 32])
                .to_bytes()
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn deliver_sealed_scheduling_lands_scheduling_welcome_in_recipient_inbox() {
        let state = fixture_state().await;
        let (mda, organizer) = mda_and_organizer(&state).await;
        let bob = [42u8; 32];
        let channel_id = [7u8; 32];

        let req = deliver_sealed_scheduling_req(&organizer, &bob, &channel_id);
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let bytes = deliver_sealed_scheduling_handler()(state.clone(), mda, payload)
            .await
            .expect("MDA delivers a sealed scheduling iMIP for its own organizer");
        let reply: DeliverSealedSchedulingReply = decode(&bytes).unwrap();
        assert!(
            reply.inbox_id > 0,
            "the same-nest welcome pushes an inbox row"
        );
        assert_eq!(reply.seq, 1, "the iMIP is the channel's first message");

        // The recipient's inbox carries exactly one Welcome, tagged `scheduling`
        // (so the recipient routes it to calendar-apply, NOT the chat UI),
        // wrapping the opaque welcome bytes verbatim.
        let inbox = state.db.list_inbox_all(&bob).await.unwrap();
        assert_eq!(inbox.len(), 1, "exactly one scheduling welcome to bob");
        let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1).unwrap();
        assert_eq!(env.kind, fauna_protocol::inbox::InboxKind::Welcome);
        let w = env.decode_welcome().unwrap();
        assert_eq!(w.channel_type.as_deref(), Some("scheduling"));
        assert_eq!(
            w.channel_id.as_deref(),
            Some(hex::encode(channel_id).as_str())
        );
        assert_eq!(w.welcome_bytes, b"opaque-sealed-welcome");

        // The iMIP application message landed on the channel log AS THE ORGANIZER
        // (membership binds to the organizer, not the MDA).
        let rows = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel_id,
            0,
            100,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "one iMIP app message on the channel");
        assert_eq!(
            rows[0].1, req.app_envelope,
            "the sealed envelope is stored byte-for-byte as the MDA sent it"
        );
        let members = state.db.list_channel_actors(&channel_id).await.unwrap();
        assert!(
            members.contains(&organizer),
            "the organizer is the channel sender"
        );
        assert!(
            members.contains(&bob),
            "the recipient auto-registered via the welcome"
        );
    }

    #[tokio::test]
    async fn deliver_sealed_scheduling_denied_for_non_owned_organizer() {
        let state = fixture_state().await;
        let (mda, organizer) = mda_and_organizer(&state).await;
        let bob = [42u8; 32];
        let channel_id = [7u8; 32];
        // The MDA claims to act for a *different* actor than `original_sender`
        // resolves to → permission denied (no spoofing another user's organizer).
        let mut req = deliver_sealed_scheduling_req(&organizer, &bob, &channel_id);
        req.on_behalf_of_actor = [4u8; 32].to_vec();
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = deliver_sealed_scheduling_handler()(state.clone(), mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        // Nothing delivered on the denied path.
        assert!(state.db.list_inbox_all(&bob).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deliver_sealed_scheduling_requires_on_behalf_of_actor() {
        let state = fixture_state().await;
        let (mda, organizer) = mda_and_organizer(&state).await;
        let mut req = deliver_sealed_scheduling_req(&organizer, &[42u8; 32], &[7u8; 32]);
        req.on_behalf_of_actor = Vec::new();
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = deliver_sealed_scheduling_handler()(state, mda, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn deliver_sealed_scheduling_denied_for_non_bridge_caller() {
        // A User-class actor cannot reach this gateway RPC (allowlist
        // BridgeMda-only) — it's the server-side gateway, distinct from the
        // User-class conversation RPCs it reuses internally.
        let state = fixture_state().await;
        let user = [5u8; 32];
        state
            .db
            .create_user_with_handle(&user, "free", "eve", None)
            .await
            .unwrap();
        let req = deliver_sealed_scheduling_req(&user, &[42u8; 32], &[7u8; 32]);
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = deliver_sealed_scheduling_handler()(state, user, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── In-domain local delivery on the bridge-enqueue path ─────────────
    //
    // `enqueue_outbound_mail` must short-circuit an in-domain recipient to
    // local delivery (seal-to-recipient + the shared sealed-ingest path),
    // exactly as `fauna.email.send` and the Go MTA submission path do —
    // never enqueue it onto the MX-relay queue. Routing it to MX self-loops:
    // on a real deploy the loop hairpins back through the docker bridge and
    // the inbound HELO-identity check `554`-bounces it (the live
    // CalDAV-auto-schedule self-loop bug). The MDA auto-schedule gateway
    // passes ALL attendees (in-domain + external) through this RPC, so the
    // partition must live here (smtp-server.md § Outbound submission;
    // caldav-server.md § Server-side auto-schedule).

    /// Build a test AppState whose deployment mail domain is `domain`, so the
    /// enqueue handler can classify in-domain vs external recipients.
    async fn fixture_state_with_email_domain(domain: &str) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.add_mail_domain(domain, true, "testing", "self_signed", None, None)
            .await
            .unwrap();
        let st = AppState::for_test(db);
        Arc::new(st)
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_locally_delivers_in_domain_recipient() {
        let state = fixture_state_with_email_domain("example.com").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // A real local mailbox: bob@example.com with an MLS pubkey + exact alias.
        let bob = [3u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &bob,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
            .db
            .put_exact_alias("example.com", "bob", "exact", &bob)
            .await
            .unwrap();

        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec!["bob@example.com".into(), "ext@external.test".into()],
            raw_message:
                b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: hi\r\n\r\nbody\r\n"
                    .to_vec(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        enqueue_outbound_mail_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");

        // Only the external recipient hit the MX-relay queue — bob did NOT
        // (the self-loop bug enqueues bob too).
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        let recips: Vec<String> = rows.iter().map(|r| r.recipient.clone()).collect();
        assert_eq!(
            recips,
            vec!["ext@external.test".to_string()],
            "in-domain bob@example.com must not be enqueued for MX relay"
        );

        // bob received the message locally (sealed into his INBOX).
        let inbox = state
            .db
            .query_bridge_imap_messages(&bob, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "in-domain recipient bob must receive the message locally"
        );
    }

    #[tokio::test]
    async fn enqueue_outbound_mail_locally_delivers_in_domain_subaddress() {
        // The live CalDAV-auto-schedule case: organizer test@example.com invites
        // its own sub-address test+autosched@example.com. Default-on
        // subaddressing resolves it back to test's mailbox, so it must deliver
        // locally, never MX-self-loop.
        let state = fixture_state_with_email_domain("example.com").await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        let test_actor = [3u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &test_actor,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
            .db
            .put_exact_alias("example.com", "test", "exact", &test_actor)
            .await
            .unwrap();

        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "test@example.com".into(),
            recipients: vec!["test+autosched@example.com".into()],
            raw_message:
                b"From: test@example.com\r\nTo: test+autosched@example.com\r\nSubject: invite\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n"
                    .to_vec(),
            on_behalf_of_actor: Some(test_actor.to_vec()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        enqueue_outbound_mail_handler()(state.clone(), mda, payload)
            .await
            .expect("handler ok");

        // Nothing queued for MX relay — the in-domain sub-address delivered locally.
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            rows.is_empty(),
            "in-domain sub-address must not be enqueued for MX relay; got {:?}",
            rows.iter().map(|r| &r.recipient).collect::<Vec<_>>()
        );
        let inbox = state
            .db
            .query_bridge_imap_messages(&test_actor, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "the sub-addressed in-domain attendee must receive the iMIP locally"
        );
    }

    #[tokio::test]
    async fn auto_reply_to_in_domain_sender_delivers_locally_no_relay() {
        // A vacation auto-reply addressed back to an in-domain envelope-from must
        // deliver into that sender's sealed INBOX, never the MX-relay queue
        // (which self-loops and `554`-bounces on a containerized deploy). The
        // `send_auto_reply` handler routes through the `submit_outbound`
        // chokepoint (smtp-server.md § Outbound submission flow).
        let state = fixture_state_with_email_domain("example.com").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // bob@example.com sent mail to a vacationing user; the auto-reply goes
        // back to bob (in-domain) → seals into bob's INBOX.
        let bob = [4u8; 32];
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &bob,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        state
            .db
            .put_exact_alias("example.com", "bob", "exact", &bob)
            .await
            .unwrap();

        let vacationer = [5u8; 32];
        let req = SendAutoReplyRequest {
            recipient_actor_id: vacationer.to_vec(),
            envelope_from: "bob@example.com".into(),
            interval_hours: 168,
            original_msgid: "<ar-1@example.com>".into(),
            raw_message:
                b"From: vac@example.com\r\nTo: bob@example.com\r\nSubject: Out of office\r\n\r\nAway until Monday.\r\n"
                    .to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = send_auto_reply_handler()(state.clone(), mta, payload)
            .await
            .expect("handler ok");
        let reply: SendAutoReplyReply = decode(&reply_bytes).unwrap();
        assert!(reply.sent, "auto-reply must be sent (rate-limit slot free)");

        // Nothing queued for MX relay — the in-domain auto-reply delivered locally.
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            rows.is_empty(),
            "in-domain auto-reply must not be MX-relayed; got {:?}",
            rows.iter().map(|r| &r.recipient).collect::<Vec<_>>()
        );

        // bob received the auto-reply locally (sealed into his INBOX).
        let inbox = state
            .db
            .query_bridge_imap_messages(&bob, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "the in-domain auto-reply recipient must receive it locally"
        );
    }

    // ── The authenticated-sender stamp on the in-domain partition
    //    (smtp-server.md § Architectural rules → *The `X-Fauna-*` namespace*;
    //    consumer caldav-server.md § Who may mutate … → *The mail rail*). ──

    /// Provision a REAL MSEK-derived recipient key for `actor` (the in-domain
    /// seal needs one) plus its exact alias, and hand the MSEK back so the
    /// test can open what the partition sealed.
    async fn provision_openable_mailbox(
        state: &Arc<AppState>,
        actor: &[u8; 32],
        domain: &str,
        local: &str,
        msek: [u8; 32],
    ) -> [u8; 32] {
        crate::test_support::seed_recipient_seal_key(&state.db, actor, &msek).await;
        state
            .db
            .put_exact_alias(domain, local, "exact", actor)
            .await
            .unwrap();
        msek
    }

    /// Open the ONE message in `actor`'s INBOX — the same segment read the
    /// client feed (`fauna.email.inbox.fetch`) ships, unsealed client-side.
    async fn open_only_inbox_copy(
        state: &Arc<AppState>,
        actor: &[u8; 32],
        msek: &[u8; 32],
    ) -> Vec<u8> {
        let inbox = state
            .db
            .query_bridge_imap_messages(actor, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1, "exactly one locally delivered copy");
        let (body, _hint, _floor) = crate::segments::mail::read_sealed_body_with_floor(
            &state.mail_segments,
            &state.db,
            actor,
            &inbox[0].message_id,
        )
        .await
        .unwrap()
        .expect("the placement row has a sealed body");
        crate::test_support::open_recipient_record(&body, msek)
    }

    /// The MDA auto-schedule gateway's in-domain leg: `original_sender` is the
    /// organizer `enforce_mda_sender_scope` bound to `on_behalf_of_actor`, so
    /// the attendee's sealed copy names it as the authenticated sender — the
    /// stamp a mailed `REPLY`/`REQUEST` consumer binds to.
    #[tokio::test]
    async fn enqueue_outbound_mail_mda_stamps_the_bound_organizer_on_the_local_copy() {
        let state = fixture_state_with_email_domain("example.com").await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let organizer = [3u8; 32];
        state
            .db
            .put_exact_alias("example.com", "organizer", "exact", &organizer)
            .await
            .unwrap();
        let attendee = [6u8; 32];
        let attendee_secret =
            provision_openable_mailbox(&state, &attendee, "example.com", "attendee", [0x61; 32])
                .await;

        let req = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "organizer@example.com".into(),
            recipients: vec!["attendee@example.com".into()],
            raw_message: b"From: organizer@example.com\r\nTo: attendee@example.com\r\n\
                           Subject: invite\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n\
                           END:VCALENDAR\r\n"
                .to_vec(),
            on_behalf_of_actor: Some(organizer.to_vec()),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        enqueue_outbound_mail_handler()(state.clone(), mda, payload)
            .await
            .expect("MDA enqueue as own organizer ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert!(
            rows.is_empty(),
            "the in-domain attendee is never MX-relayed"
        );
        let opened = open_only_inbox_copy(&state, &attendee, &attendee_secret).await;
        assert_eq!(
            fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened).as_deref(),
            Some("organizer@example.com"),
            "the local copy names the bound organizer as the authenticated sender:\n{}",
            String::from_utf8_lossy(&opened)
        );
    }

    /// A nest-internal null-sender submission (the RFC 3834 auto-reply) through
    /// the same partition authenticates nobody, so its local copy is unstamped.
    #[tokio::test]
    async fn a_null_sender_submission_leaves_the_local_copy_unstamped() {
        let state = fixture_state_with_email_domain("example.com").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let bob = [4u8; 32];
        let bob_secret =
            provision_openable_mailbox(&state, &bob, "example.com", "bob", [0x62; 32]).await;

        let req = SendAutoReplyRequest {
            recipient_actor_id: [5u8; 32].to_vec(),
            envelope_from: "bob@example.com".into(),
            interval_hours: 168,
            original_msgid: "<ar-2@example.com>".into(),
            raw_message:
                b"From: vac@example.com\r\nTo: bob@example.com\r\nSubject: Out of office\r\n\r\nAway.\r\n"
                    .to_vec(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply: SendAutoReplyReply = decode(
            &send_auto_reply_handler()(state.clone(), mta, payload)
                .await
                .expect("handler ok"),
        )
        .unwrap();
        assert!(reply.sent);
        let opened = open_only_inbox_copy(&state, &bob, &bob_secret).await;
        assert_eq!(
            fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened),
            None,
            "a null-sender submission stamps nobody:\n{}",
            String::from_utf8_lossy(&opened)
        );
    }

    #[tokio::test]
    async fn mark_outbound_methods_deny_mda_class() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;

        for (kind, payload_bytes) in [
            (
                "delivered",
                encode_canonical(&MarkOutboundDeliveredRequest { id: 1 })
                    .unwrap()
                    .to_vec(),
            ),
            (
                "failed",
                encode_canonical(&MarkOutboundFailedRequest {
                    id: 1,
                    retry_after_seconds: 1,
                    last_error: "x".into(),
                })
                .unwrap()
                .to_vec(),
            ),
            (
                "bounced",
                encode_canonical(&MarkOutboundBouncedRequest {
                    id: 1,
                    reason: "x".into(),
                })
                .unwrap()
                .to_vec(),
            ),
        ] {
            let handler = match kind {
                "delivered" => mark_outbound_delivered_handler(),
                "failed" => mark_outbound_failed_handler(),
                "bounced" => mark_outbound_bounced_handler(),
                _ => unreachable!(),
            };
            let payload = Bytes::from(payload_bytes);
            let err = handler(state.clone(), mda, payload).await.unwrap_err();
            assert_eq!(err.code, "fauna.bridges.permission_denied", "kind={kind}");
        }
    }

    // ── Per-account alias user surface (A2.1) ──
    //
    // User-class CRUD over `account_aliases` (exact kind this slice). The
    // owning actor is the authenticated caller; any non-bridge/non-admin
    // actor resolves to `CallerClass::User`, so a bare `[u8; 32]` is a
    // User caller in these tests.

    fn create_alias_req(kind: &str, domain: &str, pattern: &str) -> Bytes {
        let req = CreateAccountAliasRequest {
            kind: kind.into(),
            local_domain: domain.into(),
            pattern: pattern.into(),
            controls: AliasControls::default(),
        };
        Bytes::from(encode_canonical(&req).unwrap().to_vec())
    }

    async fn create_alias(state: &Arc<AppState>, actor: [u8; 32], pattern: &str) -> ByteBuf {
        let bytes = create_account_alias_handler()(
            state.clone(),
            actor,
            create_alias_req("exact", "example.com", pattern),
        )
        .await
        .unwrap_or_else(|e| panic!("create {pattern} failed: {}", e.code));
        decode::<CreateAccountAliasReply>(&bytes).unwrap().alias_id
    }

    async fn list_aliases(state: &Arc<AppState>, actor: [u8; 32]) -> Vec<AliasRow> {
        let req = Bytes::from(
            encode_canonical(&ListAccountAliasesRequest {})
                .unwrap()
                .to_vec(),
        );
        let bytes = list_account_aliases_handler()(state.clone(), actor, req)
            .await
            .expect("list ok");
        decode::<ListAccountAliasesReply>(&bytes).unwrap().aliases
    }

    #[tokio::test]
    async fn create_then_list_exact_alias_owned_by_caller_and_resolves() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob.smith").await;
        assert_eq!(alias_id.len(), 16);

        let aliases = list_aliases(&state, user).await;
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].pattern, "bob.smith");
        assert_eq!(aliases[0].kind, "exact");
        assert_eq!(aliases[0].actor_id.as_ref(), &user[..]);
        assert!(!aliases[0].disabled);

        // F1 end-to-end: the user-created exact alias resolves through the
        // live MTA `validate_recipient` (exact-lookup).
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let vr = ValidateRecipientRequest {
            local_part: "bob.smith".into(),
            domain: "example.com".into(),
        };
        let vr_bytes = validate_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&vr).unwrap().to_vec()),
        )
        .await
        .expect("validate ok");
        match decode::<ValidateRecipientReply>(&vr_bytes).unwrap() {
            ValidateRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id, user.to_vec())
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_non_user_creatable_kind() {
        // exact (A2.1) + wildcard_prefix (A2.2) are user-creatable; disposable
        // mints via a separate RPC (A2.3), catch-all is admin policy, and any
        // unknown kind value is malformed.
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        for kind in ["disposable", "catchall", "subaddress"] {
            let err = create_account_alias_handler()(
                state.clone(),
                user,
                create_alias_req(kind, "example.com", "bob"),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed", "kind={kind}");
        }
        assert!(list_aliases(&state, user).await.is_empty());
    }

    #[tokio::test]
    async fn create_rejects_reserved_local_part() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        for reserved in [
            "postmaster",
            "abuse",
            "noc",
            "security",
            "dmarc-report",
            "tlsrpt",
            "POSTMASTER", // case-insensitive
        ] {
            let err = create_account_alias_handler()(
                state.clone(),
                user,
                create_alias_req("exact", "example.com", reserved),
            )
            .await
            .unwrap_err();
            assert_eq!(
                err.code, "fauna.bridges.reserved_local_part",
                "reserved={reserved}"
            );
        }
    }

    #[tokio::test]
    async fn create_rejects_bad_charclass_and_overlong() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        for bad in ["bob smith", "bob@x", "bob+work", "bobünïcode", ""] {
            let err = create_account_alias_handler()(
                state.clone(),
                user,
                create_alias_req("exact", "example.com", bad),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed", "bad={bad:?}");
        }
        // 65 chars > RFC-5321 §4.5.3.1.1 64-char local-part ceiling.
        let long = "a".repeat(65);
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_alias_req("exact", "example.com", &long),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn create_duplicate_conflicts() {
        let state = fixture_state().await;
        let alice = [5u8; 32];
        let bob = [6u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        create_alias(&state, alice, "shared").await;
        // Same actor re-claims → conflict.
        let err = create_account_alias_handler()(
            state.clone(),
            alice,
            create_alias_req("exact", "example.com", "shared"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");
        // Different actor claims the same exact alias on the same domain →
        // conflict (two users can't share — § Cross-user uniqueness).
        let err = create_account_alias_handler()(
            state.clone(),
            bob,
            create_alias_req("exact", "example.com", "shared"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");
    }

    // ── Per-account forward-all user surface (N1) ──
    //
    // User-class get/set over the `mail_account_settings.forward_all_to` column.
    // A bare `[u8; 32]` resolves to `CallerClass::User`, so it is a User caller
    // here. Plaintext mode stores the address in the column; encrypted mode
    // rejects (the user-private value must be sealed to the bridge, not stored
    // plaintext — N1b). See `mail-forwarding.md` § Per-account "forward all" +
    // § Where the forward config lives at rest.

    async fn set_forward_all_to(
        state: &Arc<AppState>,
        actor: [u8; 32],
        addr: Option<&str>,
    ) -> std::result::Result<(), RpcError> {
        let req = SetForwardAllToRequest {
            forward_all_to: addr.map(str::to_string),
        };
        set_forward_all_to_handler()(
            state.clone(),
            actor,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .map(|_| ())
    }

    async fn get_forward_all_to(state: &Arc<AppState>, actor: [u8; 32]) -> Option<String> {
        let bytes = get_forward_all_to_handler()(
            state.clone(),
            actor,
            Bytes::from(
                encode_canonical(&GetForwardAllToRequest {})
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("get ok");
        decode::<GetForwardAllToReply>(&bytes)
            .unwrap()
            .forward_all_to
    }

    #[tokio::test]
    async fn forward_all_to_set_get_roundtrip_plaintext() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        state
            .db
            .create_user(&[8u8; 32], "free", "test")
            .await
            .unwrap();
        // Unset by default.
        assert_eq!(get_forward_all_to(&state, user).await, None);
        // Set a valid external address.
        set_forward_all_to(&state, user, Some("alice@example.net"))
            .await
            .unwrap();
        assert_eq!(
            get_forward_all_to(&state, user).await,
            Some("alice@example.net".into())
        );
        // Overwrite.
        set_forward_all_to(&state, user, Some("alice@other.example"))
            .await
            .unwrap();
        assert_eq!(
            get_forward_all_to(&state, user).await,
            Some("alice@other.example".into())
        );
        // Per-actor scoping: a different actor is unaffected.
        assert_eq!(get_forward_all_to(&state, [8u8; 32]).await, None);
    }

    #[tokio::test]
    async fn forward_all_to_clear_disables() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        set_forward_all_to(&state, user, Some("alice@example.net"))
            .await
            .unwrap();
        // None clears.
        set_forward_all_to(&state, user, None).await.unwrap();
        assert_eq!(get_forward_all_to(&state, user).await, None);
        // An empty/whitespace address also clears ("cleared the field" in the UI).
        set_forward_all_to(&state, user, Some("alice@example.net"))
            .await
            .unwrap();
        set_forward_all_to(&state, user, Some("   ")).await.unwrap();
        assert_eq!(get_forward_all_to(&state, user).await, None);
    }

    #[tokio::test]
    async fn forward_all_to_rejects_local_domain() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // Seed example.com as a hosted local domain.
        state
            .db
            .add_mail_domain("example.com", true, "testing", "none", None, None)
            .await
            .unwrap();
        // Forwarding to a hosted domain is an alias, not a forward
        // (`mail-forwarding.md:244,:265`) — refused with its own code, whose
        // localized sentence names the alias remedy. The generic `malformed`
        // cannot say that: an app renders the code, never `details`.
        let err = set_forward_all_to(&state, user, Some("bob@example.com"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.forward_target_on_local_domain");
        assert!(err.localized().contains("alias"), "{}", err.localized());
        // Nothing persisted.
        assert_eq!(get_forward_all_to(&state, user).await, None);
    }

    #[tokio::test]
    async fn forward_all_to_rejects_syntactic_garbage() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        for bad in [
            "not-an-email",
            "two@@at.com",
            "bob@",
            "@nodomain",
            "bob@nodot",
        ] {
            let err = set_forward_all_to(&state, user, Some(bad))
                .await
                .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed", "bad={bad:?}");
        }
        assert_eq!(get_forward_all_to(&state, user).await, None);
    }

    // ── Per-account forward rate cap (mail.account.forward_per_hour) ──

    async fn set_forward_per_hour(
        state: &Arc<AppState>,
        actor: [u8; 32],
        value: u32,
    ) -> std::result::Result<(), RpcError> {
        let req = SetForwardPerHourRequest {
            forward_per_hour: value,
            ..Default::default()
        };
        set_forward_per_hour_handler()(
            state.clone(),
            actor,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .map(|_| ())
    }

    async fn get_forward_per_hour(
        state: &Arc<AppState>,
        actor: [u8; 32],
    ) -> GetForwardPerHourReply {
        let bytes = get_forward_per_hour_handler()(
            state.clone(),
            actor,
            Bytes::from(
                encode_canonical(&GetForwardPerHourRequest {})
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("get ok");
        decode::<GetForwardPerHourReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn forward_per_hour_set_get_is_what_the_rate_cap_reads() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        let other = [8u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        state.db.create_user(&other, "free", "test").await.unwrap();
        // Unset: the default, and the ceiling the app bounds its field by.
        assert_eq!(
            get_forward_per_hour(&state, user).await,
            GetForwardPerHourReply {
                forward_per_hour: fauna_mail::FORWARD_PER_HOUR_DEFAULT,
                forward_per_hour_ceiling: fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING,
                ..Default::default()
            }
        );
        set_forward_per_hour(&state, user, 7).await.unwrap();
        assert_eq!(get_forward_per_hour(&state, user).await.forward_per_hour, 7);
        // The value the forward rate cap reads is the one the user set.
        assert_eq!(state.db.get_forward_per_hour(&user).await.unwrap(), 7);
        // Per-actor: another user keeps the default.
        assert_eq!(
            get_forward_per_hour(&state, other).await.forward_per_hour,
            fauna_mail::FORWARD_PER_HOUR_DEFAULT
        );
        // The ceiling itself is a legal setting.
        set_forward_per_hour(
            &state,
            user,
            fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING,
        )
        .await
        .unwrap();
        assert_eq!(
            state.db.get_forward_per_hour(&user).await.unwrap(),
            fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING
        );
    }

    #[tokio::test]
    async fn forward_per_hour_refuses_zero_and_above_the_ceiling_and_keeps_the_old_value() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        set_forward_per_hour(&state, user, 12).await.unwrap();
        for bad in [0, fauna_mail::FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING + 1] {
            let err = set_forward_per_hour(&state, user, bad).await.unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed", "value {bad}");
            assert_eq!(state.db.get_forward_per_hour(&user).await.unwrap(), 12);
        }
    }

    #[tokio::test]
    async fn forward_per_hour_and_forward_all_to_leave_each_other_alone() {
        let state = fixture_state().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        set_forward_per_hour(&state, user, 9).await.unwrap();
        set_forward_all_to(&state, user, Some("alice@example.net"))
            .await
            .unwrap();
        assert_eq!(state.db.get_forward_per_hour(&user).await.unwrap(), 9);
        set_forward_per_hour(&state, user, 10).await.unwrap();
        assert_eq!(
            get_forward_all_to(&state, user).await,
            Some("alice@example.net".into())
        );
    }

    #[tokio::test]
    async fn forward_all_to_rests_at_the_plaintext_floor() {
        // User forward config rests at the plaintext routing-metadata FLOOR (the
        // same tier as `local_domains`/aliases/admin-forwarders) — readable by
        // the box on every nest, by design, because routing needs it. The
        // separate-bridge-sealed alternative is the documented N1b upgrade
        // (`mail-forwarding.md` § Where the forward config lives at rest).
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        set_forward_all_to(&state, user, Some("alice@example.net"))
            .await
            .expect("set ok — the forward config is floor data");
        assert_eq!(
            get_forward_all_to(&state, user).await,
            Some("alice@example.net".into())
        );
    }

    // ── N2 forward delivery trigger (MTA perimeter) ───────────────

    fn forward_all_req(forwarder: [u8; 32], dest: &str) -> ForwardMessageRequest {
        ForwardMessageRequest {
            actor_id: forwarder.to_vec(),
            original_msgid: "<orig@sender.test>".into(),
            original_sender: "carol@sender.test".into(),
            destination: dest.into(),
            raw_message: b"From: carol@sender.test\r\nSubject: hi\r\n\r\nbody\r\n".to_vec(),
            rule_id_or_forward_all: "forward-all".into(),
            copy_mode: ForwardCopyMode::Copy,
        }
    }

    async fn call_forward_message(
        state: &Arc<AppState>,
        caller: [u8; 32],
        req: ForwardMessageRequest,
    ) -> std::result::Result<i64, RpcError> {
        let bytes = forward_message_handler()(
            state.clone(),
            caller,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await?;
        Ok(decode::<ForwardMessageReply>(&bytes).unwrap().id)
    }

    async fn call_fetch_forward_config(
        state: &Arc<AppState>,
        mta: [u8; 32],
        recipient: [u8; 32],
    ) -> Option<String> {
        let req = FetchRecipientForwardConfigRequest {
            actor_id: recipient.to_vec(),
        };
        let bytes = fetch_recipient_forward_config_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("fetch ok");
        decode::<FetchRecipientForwardConfigReply>(&bytes)
            .unwrap()
            .forward_all_to
    }

    async fn call_fetch_filters(
        state: &Arc<AppState>,
        mta: [u8; 32],
        recipient: [u8; 32],
    ) -> Vec<fauna_protocol::email::EmailFilter> {
        let req = FetchRecipientFiltersRequest {
            actor_id: recipient.to_vec(),
        };
        let bytes = fetch_recipient_filters_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("fetch ok");
        decode::<FetchRecipientFiltersReply>(&bytes)
            .unwrap()
            .filters
    }

    #[tokio::test]
    async fn fetch_recipient_filters_returns_actor_rules_in_priority_order() {
        use fauna_protocol::email::{EmailFilterAction, EmailFilterRule};
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let recipient = [42u8; 32];
        let other = [99u8; 32];
        let rules = |r: Vec<EmailFilterRule>| fauna_core::encoding::canonical_encode(&r).unwrap();

        // Created out of priority order to prove the DB's `ORDER BY priority ASC`
        // (single-sourced through `email_filters_for_actor`) wins.
        state
            .db
            .create_email_filter(
                &recipient,
                "low",
                &rules(vec![EmailFilterRule::SenderDomain {
                    domain: "example.com".into(),
                }]),
                "all",
                "fileinto:Newsletters",
                5,
                true, // continue_on_match — must flow through to the perimeter fetch
                false,
            )
            .await
            .unwrap();
        state
            .db
            .create_email_filter(
                &recipient,
                "high",
                &rules(vec![EmailFilterRule::SpamScoreAtLeast { milli: 8000 }]),
                "all",
                "fileinto:Junk",
                0,
                false,
                false,
            )
            .await
            .unwrap();
        // An unrelated actor's filter must NOT leak into the recipient's reply.
        state
            .db
            .create_email_filter(
                &other,
                "other",
                &rules(vec![EmailFilterRule::SenderIs {
                    address: "x@y.z".into(),
                }]),
                "any",
                "discard",
                0,
                false,
                false,
            )
            .await
            .unwrap();

        let filters = call_fetch_filters(&state, mta, recipient).await;
        assert_eq!(filters.len(), 2, "only the recipient's two filters");
        // priority 0 ("Junk") first, then priority 5 ("Newsletters").
        assert_eq!(filters[0].priority, 0);
        assert_eq!(
            filters[0].action,
            EmailFilterAction::FileInto {
                mailbox: "Junk".into()
            }
        );
        assert_eq!(
            filters[0].rules,
            vec![EmailFilterRule::SpamScoreAtLeast { milli: 8000 }]
        );
        assert_eq!(filters[1].priority, 5);
        assert_eq!(
            filters[1].action,
            EmailFilterAction::FileInto {
                mailbox: "Newsletters".into()
            }
        );
        // The Sieve `continue` bit must survive the DB round-trip + the
        // perimeter projection so the Go evaluator sees the multi-action flag.
        assert!(
            filters[1].continue_on_match,
            "continue must flow to the MTA"
        );
        assert!(!filters[0].continue_on_match);
    }

    #[tokio::test]
    async fn fetch_recipient_filters_rejects_non_mta_caller() {
        let state = fixture_state().await;
        let user = [3u8; 32];
        // A non-enrolled actor (no bridge approval) is not MTA-class.
        let req = FetchRecipientFiltersRequest {
            actor_id: [42u8; 32].to_vec(),
        };
        let err = fetch_recipient_filters_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn forward_message_enqueues_forwarded_outbound_row() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];

        let id = call_forward_message(&state, mta, forward_all_req(forwarder, "bob@example.net"))
            .await
            .expect("forward_message ok");

        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        let row = rows.iter().find(|r| r.id == id).expect("row enqueued");
        assert!(row.is_forwarded, "row is marked forwarded");
        assert_eq!(row.recipient, "bob@example.net");
        // The ORIGINAL envelope is stored — the SRS rewrite happens at
        // queue-out (N3), not here.
        assert_eq!(row.original_sender, "carol@sender.test");
        assert_eq!(row.forward_actor_id, Some(forwarder));
        assert_eq!(row.forward_rule_id.as_deref(), Some("forward-all"));
        assert_eq!(row.status, OutboundStatus::Pending);
        assert!(String::from_utf8_lossy(&row.raw_message).contains("body"));
    }

    #[tokio::test]
    async fn forward_message_redirect_still_enqueues_with_rule_id() {
        // The local-delivery skip for Redirect is the MTA's decision; nest
        // enqueues either mode. A per-rule forward records its rule.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let mut req = forward_all_req([7u8; 32], "bob@example.net");
        req.copy_mode = ForwardCopyMode::Redirect;
        req.rule_id_or_forward_all = "rule-42".into();
        let id = call_forward_message(&state, mta, req).await.expect("ok");
        let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
        let row = rows.iter().find(|r| r.id == id).unwrap();
        assert!(row.is_forwarded);
        assert_eq!(row.forward_rule_id.as_deref(), Some("rule-42"));
    }

    async fn outbound_copy_mode(state: &Arc<AppState>, id: i64) -> Option<String> {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT forward_copy_mode FROM outbound_mail_queue WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// The copy mode is persisted on the queued row on BOTH arms — dispatched
    /// under the cap, parked over it — and survives promotion unchanged,
    /// because a succession reads it off the row to tell a second copy (burn)
    /// from the only copy (carry).
    #[tokio::test]
    async fn forward_message_persists_the_copy_mode_on_both_arms_and_through_promotion() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        seed_forward_per_hour(&state, &forwarder, 1).await;
        let req = |mode, rule: &str| {
            let mut r = forward_all_req(forwarder, "bob@example.net");
            r.copy_mode = mode;
            r.rule_id_or_forward_all = rule.into();
            r
        };

        // Under the cap: dispatched straight to the outbound queue.
        let reply = call_forward_message_reply(&state, mta, req(ForwardCopyMode::Redirect, "43"))
            .await
            .unwrap();
        assert!(!reply.queued);
        assert_eq!(
            outbound_copy_mode(&state, reply.id).await.as_deref(),
            Some("redirect")
        );

        // Over the cap: parked, one of each mode.
        for (mode, rule) in [
            (ForwardCopyMode::Copy, "42"),
            (ForwardCopyMode::Redirect, "43"),
        ] {
            let reply = call_forward_message_reply(&state, mta, req(mode, rule))
                .await
                .unwrap();
            assert!(reply.queued);
        }
        let parked = state
            .db
            .fetch_forward_queue_oldest(&forwarder, 10)
            .await
            .unwrap();
        assert_eq!(
            parked.iter().map(|r| r.copy_mode).collect::<Vec<_>>(),
            vec![Some(ForwardCopyMode::Copy), Some(ForwardCopyMode::Redirect)]
        );

        // Raise the cap and promote: each promoted row keeps its mode.
        seed_forward_per_hour(&state, &forwarder, 10).await;
        promote_due_forwards(&state).await.unwrap();
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 0);
        let mut promoted = Vec::new();
        for row in state.db.fetch_all_outbound_for_test().await.unwrap() {
            if row.id != reply.id {
                promoted.push((
                    row.forward_rule_id.clone().unwrap(),
                    outbound_copy_mode(&state, row.id).await,
                ));
            }
        }
        promoted.sort();
        assert_eq!(
            promoted,
            vec![
                ("42".to_string(), Some("copy".to_string())),
                ("43".to_string(), Some("redirect".to_string())),
            ]
        );
    }

    #[tokio::test]
    async fn forward_message_rejects_null_sender_and_empty_fields() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // Null-sender (empty original_sender) → rejected (backscatter floor,
        // `mail-forwarding.md:231,:254`).
        let mut req = forward_all_req([7u8; 32], "bob@example.net");
        req.original_sender = "".into();
        let err = call_forward_message(&state, mta, req).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Empty destination → rejected.
        let mut req = forward_all_req([7u8; 32], "x");
        req.destination = "".into();
        let err = call_forward_message(&state, mta, req).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Empty body → rejected.
        let mut req = forward_all_req([7u8; 32], "bob@example.net");
        req.raw_message = vec![];
        let err = call_forward_message(&state, mta, req).await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");

        // None of the three rejects persisted a row.
        assert!(
            state
                .db
                .fetch_all_outbound_for_test()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn forward_message_denies_non_mta_caller() {
        let state = fixture_state().await;
        // A bare actor resolves to User class; forward_message is MTA-only.
        let user = [7u8; 32];
        let err = call_forward_message(&state, user, forward_all_req([7u8; 32], "bob@example.net"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
        assert!(
            state
                .db
                .fetch_all_outbound_for_test()
                .await
                .unwrap()
                .is_empty()
        );
    }

    // ── N5 rate-cap ───────────────────────────────────────────────

    /// Seed the actor's per-account forward cap (`forward_per_hour`); there is
    /// no write-path RPC yet (N5 enforces the cap; the policy write-path track
    /// wires the setter), so the test seeds the column directly.
    async fn seed_forward_per_hour(state: &Arc<AppState>, actor: &[u8; 32], cap: u32) {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO mail_account_settings (actor_id, forward_per_hour, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET forward_per_hour = excluded.forward_per_hour",
            rusqlite::params![actor.as_slice(), cap, 0i64],
        )
        .unwrap();
    }

    async fn call_forward_message_reply(
        state: &Arc<AppState>,
        caller: [u8; 32],
        req: ForwardMessageRequest,
    ) -> std::result::Result<ForwardMessageReply, RpcError> {
        let bytes = forward_message_handler()(
            state.clone(),
            caller,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await?;
        Ok(decode::<ForwardMessageReply>(&bytes).unwrap())
    }

    #[tokio::test]
    async fn forward_message_dispatches_under_cap_and_queues_over_cap() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        // Cap of 2/hour for this actor.
        seed_forward_per_hour(&state, &forwarder, 2).await;

        // First two forwards dispatch immediately (one outbound row each).
        for i in 0..2 {
            let reply = call_forward_message_reply(
                &state,
                mta,
                forward_all_req(forwarder, "bob@example.net"),
            )
            .await
            .unwrap_or_else(|e| panic!("forward {i} ok: {e:?}"));
            assert!(!reply.queued, "forward {i} dispatched under cap");
        }
        let outbound = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(outbound.len(), 2, "two forwarded rows dispatched");
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 0);

        // The third forward exceeds the cap → parked, not dispatched.
        let reply =
            call_forward_message_reply(&state, mta, forward_all_req(forwarder, "bob@example.net"))
                .await
                .unwrap();
        assert!(reply.queued, "third forward parked over cap");
        assert_eq!(
            state.db.fetch_all_outbound_for_test().await.unwrap().len(),
            2,
            "no new outbound row for the parked forward"
        );
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 1);
    }

    /// Today's `bridge_submission_quota.used` for `actor` — the daily
    /// recipients counter both submission doors and every forward draw on.
    async fn submission_quota_used_today(state: &Arc<AppState>, actor: &[u8; 32]) -> i64 {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT used FROM bridge_submission_quota WHERE actor_id = ?1 AND day_bucket = ?2",
            rusqlite::params![actor.as_slice(), now_epoch_secs() / 86_400],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) // no row yet today = nothing spent
    }

    /// Spend `units` of `actor`'s daily recipients allowance up front.
    async fn spend_submission_quota(state: &Arc<AppState>, actor: &[u8; 32], units: u32) {
        let max = state
            .db
            .get_submission_policy()
            .await
            .unwrap()
            .effective()
            .max_per_day;
        let outcome = state
            .db
            .try_consume_submission_quota(actor, now_epoch_secs() / 86_400, units, max)
            .await
            .unwrap();
        assert!(matches!(outcome, SubmissionQuotaOutcome::Allowed));
    }

    /// A forward is the forwarding actor's outbound, so it draws one unit of
    /// their daily recipients allowance — composed with the hourly forward cap
    /// (`mail-forwarding.md` § Architectural rules). A forward the day's
    /// allowance cannot cover is parked, not refused, and charges nothing.
    #[tokio::test]
    async fn forward_message_debits_the_daily_allowance_and_parks_past_it() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        seed_forward_per_hour(&state, &forwarder, 10).await;
        let max = state
            .db
            .get_submission_policy()
            .await
            .unwrap()
            .effective()
            .max_per_day;

        let reply =
            call_forward_message_reply(&state, mta, forward_all_req(forwarder, "bob@example.net"))
                .await
                .unwrap();
        assert!(!reply.queued, "under both caps → dispatched");
        assert_eq!(
            submission_quota_used_today(&state, &forwarder).await,
            1,
            "a dispatched forward debits exactly one unit"
        );

        // Spend the rest of the day: the hourly cap still has room, the daily
        // allowance does not.
        spend_submission_quota(&state, &forwarder, max - 1).await;
        let reply =
            call_forward_message_reply(&state, mta, forward_all_req(forwarder, "bob@example.net"))
                .await
                .unwrap();
        assert!(reply.queued, "over the day's allowance → parked");
        assert_eq!(
            state.db.fetch_all_outbound_for_test().await.unwrap().len(),
            1,
            "the parked forward is not dispatched"
        );
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 1);
        assert_eq!(
            submission_quota_used_today(&state, &forwarder).await,
            i64::from(max),
            "a parked forward charges nothing"
        );
    }

    /// Promotion is the second dispatch point: it too draws one daily unit per
    /// promoted forward, and stops at the day's remaining allowance, leaving
    /// the rest parked for a later poll.
    #[tokio::test]
    async fn promote_due_forwards_debits_and_stops_at_the_daily_allowance() {
        let state = fixture_state().await;
        let forwarder = [7u8; 32];
        seed_forward_per_hour(&state, &forwarder, 10).await;
        for i in 0..3 {
            state
                .db
                .enqueue_forward_queue(
                    crate::db::forward_queue::NewForwardQueueEntry {
                        actor_id: &forwarder,
                        source_message_id: &format!("<m{i}@s.test>"),
                        original_sender: "carol@sender.test",
                        destination_address: &format!("d{i}@example.net"),
                        rule_id_or_forward_all: "forward-all",
                        raw_message: b"body",
                        copy_mode: ForwardCopyMode::Copy,
                    },
                    1000,
                )
                .await
                .unwrap();
        }
        let max = state
            .db
            .get_submission_policy()
            .await
            .unwrap()
            .effective()
            .max_per_day;
        // Two units left today; the hourly allowance (10) would take all three.
        spend_submission_quota(&state, &forwarder, max - 2).await;

        promote_due_forwards(&state).await.unwrap();
        let outbound = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(outbound.len(), 2, "promoted only what the day covers");
        assert_eq!(
            state.db.count_forward_queue(&forwarder).await.unwrap(),
            1,
            "the third stays parked for tomorrow"
        );
        assert_eq!(
            submission_quota_used_today(&state, &forwarder).await,
            i64::from(max),
            "one unit per promoted forward"
        );

        // Nothing left today → a further poll promotes nothing.
        promote_due_forwards(&state).await.unwrap();
        assert_eq!(
            state.db.fetch_all_outbound_for_test().await.unwrap().len(),
            2
        );
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 1);
    }

    /// The handler-level shape of the ceiling rule: a queue full of
    /// `redirect` rows — each the only copy of accepted mail — survives any
    /// further forward. A further `redirect` is refused with an error (the MTA
    /// then keeps the mail), and the parked redirects all remain.
    #[tokio::test]
    async fn forward_queue_ceiling_never_evicts_a_parked_redirect() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        // cap 1 → ceiling 24.
        seed_forward_per_hour(&state, &forwarder, 1).await;
        let ceiling = fauna_mail::FORWARD_QUEUE_CEILING_MULTIPLIER;
        let req = |mode| {
            let mut r = forward_all_req(forwarder, "bob@example.net");
            r.copy_mode = mode;
            r.rule_id_or_forward_all = "43".into();
            r
        };
        // The first forward dispatches under the cap; the rest park.
        let reply = call_forward_message_reply(&state, mta, req(ForwardCopyMode::Redirect))
            .await
            .unwrap();
        assert!(!reply.queued);
        for i in 0..ceiling {
            let reply = call_forward_message_reply(&state, mta, req(ForwardCopyMode::Redirect))
                .await
                .unwrap_or_else(|e| panic!("redirect {i} parks: {e:?}"));
            assert!(reply.queued);
        }
        let parked_before = state
            .db
            .fetch_forward_queue_oldest(&forwarder, 100)
            .await
            .unwrap();
        assert_eq!(parked_before.len() as u32, ceiling);

        // Past the ceiling: a redirect is refused, a copy is the one dropped.
        let err = call_forward_message_reply(&state, mta, req(ForwardCopyMode::Redirect))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.forward_queue_full");
        let err = call_forward_message_reply(&state, mta, req(ForwardCopyMode::Copy))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.forward_queue_full");

        let parked_after = state
            .db
            .fetch_forward_queue_oldest(&forwarder, 100)
            .await
            .unwrap();
        assert_eq!(
            parked_after.iter().map(|r| r.id).collect::<Vec<_>>(),
            parked_before.iter().map(|r| r.id).collect::<Vec<_>>(),
            "every parked redirect survives the ceiling"
        );
    }

    #[tokio::test]
    async fn forward_queue_eviction_fires_in_app_notification() {
        // cap=0 ⇒ effective ceiling 0: a parked copy forward is immediately
        // FIFO-evicted, so this exercises the handler's eviction + in-app
        // notification path in one call (forwarding-disabled-by-cap edge) —
        // and the reply is an error, never `queued: true` naming the row just
        // deleted.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        seed_forward_per_hour(&state, &forwarder, 0).await;

        let err =
            call_forward_message_reply(&state, mta, forward_all_req(forwarder, "bob@example.net"))
                .await
                .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.forward_queue_full");
        // Evicted immediately → nothing left in the queue, nothing dispatched.
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 0);
        assert!(
            state
                .db
                .fetch_all_outbound_for_test()
                .await
                .unwrap()
                .is_empty()
        );
        // An in-app eviction notification was recorded for the forwarder.
        let notifs = state
            .db
            .list_notifications(&forwarder, None, 10)
            .await
            .unwrap();
        let n = notifs
            .iter()
            .find(|n| {
                n.notif_type == fauna_protocol::notifications::NotifType::MailForwardQueueEvicted
            })
            .expect("eviction notification recorded");
        assert!(n.summary.contains("bob@example.net"));
        assert!(n.summary.contains("forward queue is full"));
        // Micros, not seconds — the notifications column unit. A seconds value
        // sorts the row into 1970, burying it below every correctly stamped
        // row (the unit drift this pins against re-appearing).
        assert!(
            n.created_at > 1_000_000_000_000_000,
            "created_at must be microseconds, got {}",
            n.created_at
        );
    }

    #[tokio::test]
    async fn promote_due_forwards_drains_up_to_the_remaining_allowance() {
        let state = fixture_state().await;
        let forwarder = [7u8; 32];
        seed_forward_per_hour(&state, &forwarder, 2).await;

        // Park three forwards directly (a generous ceiling so none evict).
        for i in 0..3 {
            state
                .db
                .enqueue_forward_queue(
                    crate::db::forward_queue::NewForwardQueueEntry {
                        actor_id: &forwarder,
                        source_message_id: &format!("<m{i}@s.test>"),
                        original_sender: "carol@sender.test",
                        destination_address: &format!("d{i}@example.net"),
                        rule_id_or_forward_all: "forward-all",
                        raw_message: b"body",
                        copy_mode: ForwardCopyMode::Copy,
                    },
                    1000,
                )
                .await
                .unwrap();
        }

        // Window is empty → allowance == cap (2): promote the two oldest.
        promote_due_forwards(&state).await.unwrap();
        let outbound = state.db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(outbound.len(), 2, "promoted up to the cap");
        assert!(outbound.iter().all(|r| r.is_forwarded));
        let promoted_dests: std::collections::BTreeSet<_> =
            outbound.iter().map(|r| r.recipient.clone()).collect();
        assert_eq!(
            promoted_dests,
            ["d0@example.net".to_string(), "d1@example.net".to_string()]
                .into_iter()
                .collect(),
            "FIFO oldest-first promotion"
        );
        assert_eq!(
            state.db.count_forward_queue(&forwarder).await.unwrap(),
            1,
            "the third stays parked until the window opens"
        );

        // Window now full (2 forwarded rows == cap) → next promotion is a no-op.
        promote_due_forwards(&state).await.unwrap();
        assert_eq!(
            state.db.fetch_all_outbound_for_test().await.unwrap().len(),
            2,
            "no further promotion while at the cap"
        );
        assert_eq!(state.db.count_forward_queue(&forwarder).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn rotate_srs_secret_handler_rotates_for_admin_and_denies_others() {
        let state = fixture_state().await;
        let admin = [2u8; 32];
        add_admin(&state.db, &admin).await;
        // The auto-seeded secret before rotation.
        let before = state.db.get_active_srs_secret().await.unwrap().unwrap();

        let bytes = rotate_srs_secret_handler()(
            state.clone(),
            admin,
            Bytes::from(
                encode_canonical(&RotateSrsSecretRequest::default())
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("admin rotate ok");
        let reply = decode::<RotateSrsSecretReply>(&bytes).unwrap();
        assert!(reply.rotated_at > 0, "reply carries a rotation timestamp");

        let secrets = state.db.list_srs_secrets().await.unwrap();
        assert_eq!(secrets.len(), 2, "2-secret overlap retained");
        assert_ne!(
            state.db.get_active_srs_secret().await.unwrap().unwrap(),
            before,
            "a fresh active secret was minted"
        );
        assert!(
            secrets.contains(&before),
            "the prior secret still verifies in-flight bounces"
        );

        // A bare actor resolves to User class → denied (Admin-only).
        let user = [9u8; 32];
        let err = rotate_srs_secret_handler()(
            state.clone(),
            user,
            Bytes::from(
                encode_canonical(&RotateSrsSecretRequest::default())
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── N3 decode_srs_bounce ──────────────────────────────────────

    #[test]
    fn srs_decode_over_secrets_handles_overlap_expiry_and_empty() {
        // A bounce minted under an OLD secret still verifies once nest lists
        // both (the N5 rotation overlap). list_srs_secrets is newest-first.
        let old = vec![0x11u8; 32];
        let new = vec![0x22u8; 32];
        let srs = fauna_mail::srs::srs_forward(&old, "x.test", 50, "42", "a@b.test").unwrap();
        let lp = srs.strip_suffix("@x.test").unwrap();
        assert_eq!(
            srs_decode_over_secrets(
                &[new.clone(), old.clone()],
                50,
                fauna_mail::srs::DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS,
                lp,
            ),
            SrsBounceDecode::Verified {
                row_id: "42".into(),
                original_sender: "a@b.test".into(),
            }
        );
        // Only the new (wrong) secret → mac_fail.
        assert_eq!(
            srs_decode_over_secrets(
                &[new],
                50,
                fauna_mail::srs::DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS,
                lp,
            ),
            SrsBounceDecode::MacFail,
        );
        // MAC verifies under the right secret but the bounce is too old.
        assert_eq!(
            srs_decode_over_secrets(&[old], 100, 28, lp),
            SrsBounceDecode::Expired,
        );
        // No secrets at all → cannot verify → mac_fail.
        assert_eq!(
            srs_decode_over_secrets(&[], 50, 28, lp),
            SrsBounceDecode::MacFail,
        );
    }

    async fn call_decode_srs(
        state: &Arc<AppState>,
        mta: [u8; 32],
        local_part: &str,
    ) -> DecodeSrsBounceReply {
        let req = DecodeSrsBounceRequest {
            local_part: local_part.into(),
        };
        let bytes = decode_srs_bounce_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("decode handler ok");
        decode::<DecodeSrsBounceReply>(&bytes).unwrap()
    }

    /// Build the SRS local-part the queue-out rewrite would emit for `row_id`,
    /// under nest's active secret — the inverse of `srs_rewrite_outbound_sender`.
    async fn srs_local_part_for(
        state: &Arc<AppState>,
        secret: &[u8],
        row_id: &str,
        original_sender: &str,
    ) -> String {
        let now_day = (state.outbound_now().max(0) as u64) / 86_400;
        let srs =
            fauna_mail::srs::srs_forward(secret, "example.com", now_day, row_id, original_sender)
                .unwrap();
        srs.strip_suffix("@example.com").unwrap().to_string()
    }

    #[tokio::test]
    async fn decode_srs_bounce_ok_maps_row_to_forwarder_and_destination() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let forwarder = [7u8; 32];
        let recipients = ["downstream@ext.test"];
        let fwd_id = state
            .db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<f@s.test>",
                original_sender: "alice@example.net",
                recipients: &recipients,
                raw_message: b"raw\r\n",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: true,
                forward_actor_id: Some(&forwarder),
                forward_rule_id: Some("forward-all"),
                forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                submit_actor_id: None,
            })
            .await
            .unwrap()[0];

        let secret = state.db.get_active_srs_secret().await.unwrap().unwrap();
        let lp =
            srs_local_part_for(&state, &secret, &fwd_id.to_string(), "alice@example.net").await;

        let reply = call_decode_srs(&state, mta, &lp).await;
        assert_eq!(reply.outcome, "ok");
        assert_eq!(reply.forwarder_actor_id, forwarder.to_vec());
        assert_eq!(reply.original_sender, "alice@example.net");
        assert_eq!(reply.original_destination, "downstream@ext.test");
    }

    #[tokio::test]
    async fn decode_srs_bounce_orphans_when_row_missing() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let secret = state.db.get_active_srs_secret().await.unwrap().unwrap();
        // A well-formed SRS for a row id that was never enqueued → orphan
        // (drop + counter, never the admin mailbox — `:117`).
        let lp = srs_local_part_for(&state, &secret, "999999", "alice@example.net").await;
        let reply = call_decode_srs(&state, mta, &lp).await;
        assert_eq!(reply.outcome, "orphan");
        assert!(reply.forwarder_actor_id.is_empty());
    }

    #[tokio::test]
    async fn decode_srs_bounce_mac_fail_on_foreign_secret() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        // Encoded under a secret nest does not hold → mac_fail (hard-reject).
        let lp = srs_local_part_for(&state, &[0xFFu8; 32], "1", "alice@example.net").await;
        let reply = call_decode_srs(&state, mta, &lp).await;
        assert_eq!(reply.outcome, "mac_fail");
    }

    #[tokio::test]
    async fn decode_srs_bounce_not_srs_for_plain_localpart() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        assert_eq!(
            call_decode_srs(&state, mta, "alice").await.outcome,
            "not_srs"
        );
    }

    #[tokio::test]
    async fn decode_srs_bounce_denies_non_mta() {
        let state = fixture_state().await;
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = DecodeSrsBounceRequest {
            local_part: "alice".into(),
        };
        let err = decode_srs_bounce_handler()(
            state,
            mda,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn fetch_recipient_forward_config_returns_stored_target() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let recipient = [7u8; 32];
        state
            .db
            .create_user(&recipient, "free", "test")
            .await
            .unwrap();

        // Unset → None.
        assert_eq!(
            call_fetch_forward_config(&state, mta, recipient).await,
            None
        );

        // The recipient sets their own forward-all (User-class set); the MTA
        // reads it back through the perimeter chokepoint.
        set_forward_all_to(&state, recipient, Some("alice@example.net"))
            .await
            .unwrap();
        assert_eq!(
            call_fetch_forward_config(&state, mta, recipient).await,
            Some("alice@example.net".into())
        );

        // Per-recipient scoping: a different recipient is unaffected.
        assert_eq!(
            call_fetch_forward_config(&state, mta, [8u8; 32]).await,
            None
        );
    }

    #[tokio::test]
    async fn update_overwrites_pattern_and_controls() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;

        let upd = UpdateAccountAliasRequest {
            alias_id: alias_id.clone(),
            pattern: "bob.smith".into(),
            controls: AliasControls {
                label: "work".into(),
                spam_threshold_override: Some(9),
                rate_limit_per_hour: Some(50),
                rate_limit_per_day: None,
            },
        };
        update_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&upd).unwrap().to_vec()),
        )
        .await
        .expect("update ok");

        let aliases = list_aliases(&state, user).await;
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].pattern, "bob.smith");
        assert_eq!(aliases[0].label, "work");
        assert_eq!(aliases[0].spam_threshold_override, Some(9));
        assert_eq!(aliases[0].rate_limit_per_hour, Some(50));
        assert_eq!(aliases[0].rate_limit_per_day, None);
    }

    /// A negative rate cap is meaningless under `mail-aliases.md`
    /// § Per-alias rate-cap (`None` = unlimited; over-quota tempfails), and the
    /// shared client-side parser already refuses one — but the nest is the trust
    /// boundary and a non-conforming client can still send one. Both
    /// fields, both doors: `AliasControls` is the shared input bundle, so the one
    /// validator has to hold on create and update alike.
    #[tokio::test]
    async fn alias_controls_reject_a_negative_rate_cap() {
        for (field, per_hour, per_day) in [
            ("rate_limit_per_hour", Some(-1i64), None),
            ("rate_limit_per_day", None, Some(-1i64)),
        ] {
            let state = fixture_state().await;
            let user = [5u8; 32];
            state.db.create_user(&user, "free", "test").await.unwrap();
            let alias_id = create_alias(&state, user, "bob").await;

            // The update door.
            let upd = UpdateAccountAliasRequest {
                alias_id: alias_id.clone(),
                pattern: "bob".into(),
                controls: AliasControls {
                    label: String::new(),
                    spam_threshold_override: None,
                    rate_limit_per_hour: per_hour,
                    rate_limit_per_day: per_day,
                },
            };
            let err = update_account_alias_handler()(
                state.clone(),
                user,
                Bytes::from(encode_canonical(&upd).unwrap().to_vec()),
            )
            .await
            .expect_err("a negative cap must be refused on update");
            assert_eq!(err.code, "fauna.protocol.malformed", "field={field}");

            // The refusal left the persisted controls untouched.
            let aliases = list_aliases(&state, user).await;
            assert_eq!(aliases[0].rate_limit_per_hour, None, "field={field}");
            assert_eq!(aliases[0].rate_limit_per_day, None, "field={field}");

            // The CREATE door, driven independently — the two share one
            // validator, and a test that only drove `update` would pass on a
            // create door that had drifted back to the bare label check.
            let create = CreateAccountAliasRequest {
                kind: "exact".into(),
                local_domain: "example.com".into(),
                pattern: "carol".into(),
                controls: AliasControls {
                    label: String::new(),
                    spam_threshold_override: None,
                    rate_limit_per_hour: per_hour,
                    rate_limit_per_day: per_day,
                },
            };
            let err = create_account_alias_handler()(
                state.clone(),
                user,
                Bytes::from(encode_canonical(&create).unwrap().to_vec()),
            )
            .await
            .expect_err("a negative cap must be refused on create too");
            assert_eq!(err.code, "fauna.protocol.malformed", "field={field}");
            // And it persisted no row.
            let aliases = list_aliases(&state, user).await;
            assert!(
                !aliases.iter().any(|a| a.pattern == "carol"),
                "field={field}: the refused create must not persist an alias"
            );
        }
    }

    #[tokio::test]
    async fn alias_controls_accept_a_zero_rate_cap() {
        // `0` is not the nonsense case — the refusal is `< 0`, never `< 1`, the
        // same boundary the admin tier caps keep.
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;

        let upd = UpdateAccountAliasRequest {
            alias_id,
            pattern: "bob".into(),
            controls: AliasControls {
                label: String::new(),
                spam_threshold_override: None,
                rate_limit_per_hour: Some(0),
                rate_limit_per_day: None,
            },
        };
        update_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&upd).unwrap().to_vec()),
        )
        .await
        .expect("a 0 cap passes the range check");
        let aliases = list_aliases(&state, user).await;
        assert_eq!(aliases[0].rate_limit_per_hour, Some(0));
    }

    #[tokio::test]
    async fn revoke_sets_disabled_then_delete_removes() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;

        revoke_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(
                encode_canonical(&RevokeAccountAliasRequest {
                    alias_id: alias_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("revoke ok");
        let aliases = list_aliases(&state, user).await;
        assert_eq!(aliases.len(), 1);
        assert!(aliases[0].disabled, "revoke flips disabled=true, keeps row");

        delete_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(
                encode_canonical(&DeleteAccountAliasRequest {
                    alias_id: alias_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("delete ok");
        assert!(list_aliases(&state, user).await.is_empty());
    }

    #[tokio::test]
    async fn cross_actor_isolation_on_list_update_revoke_delete() {
        let state = fixture_state().await;
        let alice = [5u8; 32];
        let bob = [6u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, alice, "alice").await;

        // Bob can't see Alice's alias.
        assert!(list_aliases(&state, bob).await.is_empty());

        // Bob can't update / revoke / delete Alice's alias → not_found
        // (don't leak existence as permission_denied).
        let upd = UpdateAccountAliasRequest {
            alias_id: alias_id.clone(),
            pattern: "hacked".into(),
            controls: AliasControls::default(),
        };
        let err = update_account_alias_handler()(
            state.clone(),
            bob,
            Bytes::from(encode_canonical(&upd).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
        let err = revoke_account_alias_handler()(
            state.clone(),
            bob,
            Bytes::from(
                encode_canonical(&RevokeAccountAliasRequest {
                    alias_id: alias_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
        let err = delete_account_alias_handler()(
            state.clone(),
            bob,
            Bytes::from(
                encode_canonical(&DeleteAccountAliasRequest {
                    alias_id: alias_id.clone(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");

        // Alice's alias is untouched.
        let aliases = list_aliases(&state, alice).await;
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].pattern, "alice");
    }

    #[tokio::test]
    async fn create_enforces_exact_alias_cap_across_domains() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // mail.account.exact_aliases_max default = 20 (hardcoded this slice;
        // the cap is per-actor across all local domains).
        for i in 0..20 {
            create_alias(&state, user, &format!("bob{i}")).await;
        }
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_alias_req("exact", "example.com", "bob20"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.alias_cap_exceeded");
        assert_eq!(list_aliases(&state, user).await.len(), 20);
    }

    // ── A2.2 — wildcard-prefix create + the resolve_recipient resolver ──

    fn create_wildcard_req(domain: &str, prefix: &str) -> Bytes {
        create_alias_req("wildcard_prefix", domain, prefix)
    }

    async fn resolve(
        state: &Arc<AppState>,
        mta: [u8; 32],
        local_part: &str,
        domain: &str,
    ) -> ResolveRecipientReply {
        resolve_from(state, mta, local_part, domain, "").await
    }

    /// Split a resolved reply's stamps into the alias-match headers (what a
    /// given match kind is *about*) and the delivery-time spam-threshold value
    /// every resolved mailbox now carries as its LAST stamp
    /// (`mail-aliases.md` § Spam-threshold override). Asserting through this
    /// keeps each match-kind test about its own header instead of re-encoding
    /// the fold's presence seven times — and it fails loudly if a resolve path
    /// ever stops stamping, which is the property the fold rests on.
    fn split_threshold_stamp(hs: &[StampedHeader]) -> (&[StampedHeader], u32) {
        let (last, rest) = hs
            .split_last()
            .expect("every resolved mailbox carries at least the threshold stamp");
        assert_eq!(
            last.name,
            fauna_mail::aliases::HEADER_SPAM_THRESHOLD,
            "the delivery-time threshold is stamped last on every resolved mailbox"
        );
        (
            rest,
            last.value.parse().expect("threshold stamps an integer"),
        )
    }

    /// `resolve` with an explicit MAIL FROM `sender_domain` (A2.4 hit-log).
    async fn resolve_from(
        state: &Arc<AppState>,
        mta: [u8; 32],
        local_part: &str,
        domain: &str,
        sender_domain: &str,
    ) -> ResolveRecipientReply {
        let req = ResolveRecipientRequest {
            local_part: local_part.into(),
            domain: domain.into(),
            sender_domain: sender_domain.into(),
            ..Default::default()
        };
        let bytes = resolve_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("resolve ok");
        decode::<ResolveRecipientReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn create_wildcard_then_resolves_with_suffix_header() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_account_alias_handler()(
            state.clone(),
            user,
            create_wildcard_req("example.com", "bob-"),
        )
        .await
        .expect("wildcard create ok");
        let rows = list_aliases(&state, user).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "wildcard_prefix");
        assert_eq!(rows[0].pattern, "bob-");

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve(&state, mta, "bob-amazon", "example.com").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
                let (addr, _threshold) = split_threshold_stamp(&headers_to_stamp);
                assert_eq!(addr.len(), 1);
                assert_eq!(addr[0].name, "X-Fauna-Address-Wildcard-Suffix");
                assert_eq!(addr[0].value, "amazon");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_wildcard_rejects_reserved_glob_short_and_no_dash() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // `dmarc-*` shadows the reserved `dmarc-report`.
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_wildcard_req("example.com", "dmarc-"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.reserved_local_part_in_wildcard");
        // Structural failures map to malformed.
        for bad in ["b-", "bob", "bob-*"] {
            let err = create_account_alias_handler()(
                state.clone(),
                user,
                create_wildcard_req("example.com", bad),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed", "prefix={bad:?}");
        }
        assert!(list_aliases(&state, user).await.is_empty());
    }

    #[tokio::test]
    async fn create_wildcard_one_per_actor() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_account_alias_handler()(
            state.clone(),
            user,
            create_wildcard_req("example.com", "bob-"),
        )
        .await
        .expect("first wildcard ok");
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_wildcard_req("example.com", "bobby-"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.actor_already_has_wildcard");
    }

    #[tokio::test]
    async fn create_wildcard_conflicts_with_other_users_exact_and_wildcard() {
        let state = fixture_state().await;
        let alice = [5u8; 32];
        let bob = [6u8; 32];
        let carol = [7u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();
        state.db.create_user(&carol, "free", "test").await.unwrap();
        // Alice owns the exact `bob-foo`; Bob's `bob-*` would shadow it.
        create_alias(&state, alice, "bob-foo").await;
        let err = create_account_alias_handler()(
            state.clone(),
            bob,
            create_wildcard_req("example.com", "bob-"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");

        // Carol claims `news-*`; another user can't claim the same prefix
        // (UNIQUE(local_domain, pattern, kind)).
        create_account_alias_handler()(
            state.clone(),
            carol,
            create_wildcard_req("example.com", "news-"),
        )
        .await
        .expect("carol wildcard ok");
        let err = create_account_alias_handler()(
            state.clone(),
            bob,
            create_wildcard_req("example.com", "news-"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");
    }

    #[tokio::test]
    async fn resolve_subaddress_stamps_suffix_header() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve(&state, mta, "bob+work", "example.com").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
                let (addr, _threshold) = split_threshold_stamp(&headers_to_stamp);
                assert_eq!(addr.len(), 1);
                assert_eq!(addr[0].name, "X-Fauna-Address-Suffix");
                assert_eq!(addr[0].value, "work");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        // Invalid sub-address rejects 550.
        match resolve(&state, mta, "bob+", "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("Invalid sub-address"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    // ── put_alias_policy — the four nest-side
    //    alias-policy knobs become admin-tunable; the resolver + alias
    //    CRUD read the effective (override-or-default) values. ──────────

    async fn put_alias_policy(state: &Arc<AppState>, admin: [u8; 32], req: PutAliasPolicyRequest) {
        let bytes = put_alias_policy_handler()(
            state.clone(),
            admin,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("put_alias_policy ok");
        assert!(decode::<PutPolicyReply>(&bytes).unwrap().ok);
    }

    #[tokio::test]
    async fn put_alias_policy_admin_writes_round_trip_and_non_admin_denied() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let req = PutAliasPolicyRequest {
            exact_aliases_max: Some(3),
            reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
            subaddressing_enabled: Some(false),
            wildcard_prefix_enabled: Some(true),
            extra: Default::default(),
        };
        put_alias_policy(&state, admin, req.clone()).await;
        let eff = state.db.get_alias_policy().await.unwrap().effective();
        assert_eq!(eff.exact_aliases_max, 3);
        assert!(!eff.subaddressing_enabled);
        assert!(eff.wildcard_prefix_enabled);
        assert_eq!(
            eff.reserved_local_parts,
            vec!["postmaster".to_string(), "sales".to_string()]
        );

        // A bare actor resolves to User class → Admin kind denied.
        let err = put_alias_policy_handler()(
            state.clone(),
            [5u8; 32],
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect_err("non-admin denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn exact_alias_cap_honors_put_alias_policy() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        put_alias_policy(
            &state,
            admin,
            PutAliasPolicyRequest {
                exact_aliases_max: Some(1),
                ..Default::default()
            },
        )
        .await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // First exact alias under the cap of 1 succeeds…
        create_alias(&state, user, "bob").await;
        // …the second is rejected by the now-tunable cap (default would be 20).
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_alias_req("exact", "example.com", "bob.smith"),
        )
        .await
        .expect_err("second create over cap=1 rejected");
        assert_eq!(err.code, "fauna.bridges.alias_cap_exceeded");
    }

    #[tokio::test]
    async fn reserved_local_parts_override_blocks_custom_reserved_name() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        state
            .db
            .create_user(&[9u8; 32], "free", "test")
            .await
            .unwrap();
        state
            .db
            .create_user(&[5u8; 32], "free", "test")
            .await
            .unwrap();
        // "sales" is NOT reserved by default — a user could claim it today.
        create_alias(&state, [9u8; 32], "sales").await;
        // After the admin reserves it, a different user's create is refused.
        put_alias_policy(
            &state,
            admin,
            PutAliasPolicyRequest {
                reserved_local_parts: Some(vec![
                    "postmaster".into(),
                    "abuse".into(),
                    "sales".into(),
                ]),
                ..Default::default()
            },
        )
        .await;
        let err = create_account_alias_handler()(
            state.clone(),
            [5u8; 32],
            create_alias_req("exact", "example.com", "sales"),
        )
        .await
        .expect_err("admin-reserved local-part rejected");
        assert_eq!(err.code, "fauna.bridges.reserved_local_part");
    }

    // ── Admin external forwarders (§ AF / mail-aliases.md § Kind 7) ──

    /// Set up a state with `admin` enrolled, an `mta` bridge approved (for
    /// `resolve_recipient`), and `domain` added as a hosted local domain.
    async fn forwarder_state(admin: [u8; 32], mta: [u8; 32], domain: &str) -> Arc<AppState> {
        let state = fixture_state().await;
        add_admin(&state.db, &admin).await;
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        add_local_domain_handler()(state.clone(), admin, add_req(domain))
            .await
            .expect("add_local_domain ok");
        state
    }

    async fn create_forwarder(
        state: &Arc<AppState>,
        admin: [u8; 32],
        domain: &str,
        pattern: &str,
        target: &str,
    ) -> Result<ByteBuf, RpcError> {
        let req = CreateForwarderRequest {
            local_domain: domain.into(),
            pattern: pattern.into(),
            forward_target: target.into(),
        };
        let bytes = create_forwarder_handler()(
            state.clone(),
            admin,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await?;
        Ok(decode::<CreateForwarderReply>(&bytes).unwrap().alias_id)
    }

    async fn list_forwarders(state: &Arc<AppState>, admin: [u8; 32]) -> Vec<AliasRow> {
        let req = Bytes::from(
            encode_canonical(&ListForwardersRequest {})
                .unwrap()
                .to_vec(),
        );
        let bytes = list_forwarders_handler()(state.clone(), admin, req)
            .await
            .expect("list_forwarders ok");
        decode::<ListForwardersReply>(&bytes).unwrap().forwarders
    }

    #[tokio::test]
    async fn create_forwarder_resolves_to_forward_then_deletes() {
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "fauna.example").await;

        let fwd_id = create_forwarder(&state, admin, "Fauna.Example", "Info", "real@example.net")
            .await
            .expect("create_forwarder ok");

        // resolve_recipient yields Forward with the external target + the
        // managing-admin actor — no local route, no headers.
        match resolve(&state, mta, "info", "fauna.example").await {
            ResolveRecipientReply::Forward {
                forward_target,
                forwarder_actor_id,
            } => {
                assert_eq!(forward_target, "real@example.net");
                assert_eq!(forwarder_actor_id.as_ref(), &admin[..]);
            }
            other => panic!("expected Forward, got {other:?}"),
        }

        // list_forwarders shows it (with forward_target). The forwarder's
        // exclusion from the owner-scoped user alias list is covered at the DB
        // layer (`create_forwarder_round_trips_and_excluded_from_user_list`) —
        // the User-class `list_account_aliases` endpoint isn't callable by this
        // Admin actor anyway.
        let fwds = list_forwarders(&state, admin).await;
        assert_eq!(fwds.len(), 1);
        assert_eq!(fwds[0].kind, "forwarder");
        assert_eq!(fwds[0].forward_target.as_deref(), Some("real@example.net"));

        // delete_forwarder removes it; resolve then 550s.
        let del = Bytes::from(
            encode_canonical(&DeleteForwarderRequest {
                alias_id: fwd_id.clone(),
            })
            .unwrap()
            .to_vec(),
        );
        delete_forwarder_handler()(state.clone(), admin, del)
            .await
            .expect("delete ok");
        match resolve(&state, mta, "info", "fauna.example").await {
            ResolveRecipientReply::Reject { smtp_code, .. } => assert_eq!(smtp_code, 550),
            other => panic!("expected Reject after delete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_forwarder_requires_admin() {
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "fauna.example").await;
        // A bare actor resolves to User class → Admin kind denied.
        let err = create_forwarder(
            &state,
            [5u8; 32],
            "fauna.example",
            "info",
            "real@example.net",
        )
        .await
        .expect_err("non-admin denied");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn create_forwarder_rejects_local_domain_target_and_unhosted_domain() {
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "fauna.example").await;

        // Target on a domain we host → refused (it'd be an alias, not a forward).
        let err = create_forwarder(&state, admin, "fauna.example", "info", "bob@fauna.example")
            .await
            .expect_err("local-domain target rejected");
        assert_eq!(err.code, "fauna.protocol.malformed");

        // Forwarder on a domain we DON'T host → refused.
        let err = create_forwarder(&state, admin, "not.hosted", "info", "real@example.net")
            .await
            .expect_err("unhosted domain rejected");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn create_forwarder_honors_tunable_reserved_local_parts() {
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "fauna.example").await;
        // A default reserved local-part is refused even for an admin forwarder.
        let err = create_forwarder(
            &state,
            admin,
            "fauna.example",
            "postmaster",
            "real@example.net",
        )
        .await
        .expect_err("default reserved refused");
        assert_eq!(err.code, "fauna.bridges.reserved_local_part");
        // After the admin reserves "sales", a "sales" forwarder is refused too
        // (the AF check reads the put_alias_policy tunable, not a const).
        put_alias_policy(
            &state,
            admin,
            PutAliasPolicyRequest {
                reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
                ..Default::default()
            },
        )
        .await;
        let err = create_forwarder(&state, admin, "fauna.example", "sales", "real@example.net")
            .await
            .expect_err("admin-reserved refused");
        assert_eq!(err.code, "fauna.bridges.reserved_local_part");
    }

    #[tokio::test]
    async fn forwarder_and_exact_are_mutually_exclusive_on_a_key() {
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "example.com").await;

        // An exact alias on `info@example.com` blocks a forwarder there.
        state
            .db
            .create_user(&[9u8; 32], "free", "test")
            .await
            .unwrap();
        create_alias(&state, [9u8; 32], "info").await;
        let err = create_forwarder(&state, admin, "example.com", "info", "real@example.net")
            .await
            .expect_err("forwarder over exact rejected");
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");

        // And vice-versa: a forwarder on `sales@` blocks a user exact alias.
        create_forwarder(&state, admin, "example.com", "sales", "real@example.net")
            .await
            .expect("forwarder ok");
        let err = create_account_alias_handler()(
            state.clone(),
            [9u8; 32],
            create_alias_req("exact", "example.com", "sales"),
        )
        .await
        .expect_err("exact over forwarder rejected");
        assert_eq!(err.code, "fauna.bridges.conflicts_with_existing_alias");
    }

    /// A list's posting address sits on the same exact-key tier as an exact
    /// alias and an admin forwarder, but in a different `kind`, so the
    /// `UNIQUE(local_domain, pattern, kind)` constraint cannot keep one
    /// address to one holder (`mail-mass-mailing.md` § Reserved local-part:
    /// `unsubscribe@` — a taken list address is refused). Each create refuses
    /// the other two kinds, in both directions (`exact_key_held_by_other_kind`).
    #[tokio::test]
    async fn list_and_exact_or_forwarder_are_mutually_exclusive_on_a_key() {
        use fauna_protocol::bridge_routing::CreateAccountListRequest;
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "example.com").await;
        let user = [9u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let create_list = |pattern: &str| {
            let req = CreateAccountListRequest {
                local_part: pattern.into(),
                local_domain: "example.com".into(),
                ..Default::default()
            };
            crate::bridge_list_handlers::create_account_list_handler()(
                state.clone(),
                user,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        let conflict = "fauna.bridges.conflicts_with_existing_alias";

        create_alias(&state, user, "info").await;
        let err = create_list("info")
            .await
            .expect_err("list over exact refused");
        assert_eq!(err.code, conflict);

        create_forwarder(&state, admin, "example.com", "sales", "real@example.net")
            .await
            .expect("forwarder ok");
        let err = create_list("sales")
            .await
            .expect_err("list over forwarder refused");
        assert_eq!(err.code, conflict);

        create_list("news").await.expect("list ok");
        let err = create_account_alias_handler()(
            state.clone(),
            user,
            create_alias_req("exact", "example.com", "news"),
        )
        .await
        .expect_err("exact over list refused");
        assert_eq!(err.code, conflict);
        let err = create_forwarder(&state, admin, "example.com", "news", "real@example.net")
            .await
            .expect_err("forwarder over list refused");
        assert_eq!(err.code, conflict);
    }

    /// The exact-key tier's one-holder rule binds EVERY door that
    /// writes an exact-tier row — bulk import and rename-by-update
    /// included — not just the single creates above. An `exact` row on a
    /// list's posting address shadows the resolver's list step (exact is
    /// matched first), so another user's import would capture the list's
    /// inbound mail; a rename onto a forwarder key would likewise silently
    /// retire the forwarder. The exclusion lives in the DB write helpers, so
    /// each door refuses and no `exact` row lands.
    #[tokio::test]
    async fn every_exact_tier_door_refuses_a_key_held_by_another_kind() {
        use fauna_protocol::bridge_routing::CreateAccountListRequest;
        let admin = [7u8; 32];
        let mta = [1u8; 32];
        let state = forwarder_state(admin, mta, "example.com").await;
        let owner = [9u8; 32];
        let other = [8u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state.db.create_user(&other, "free", "test").await.unwrap();
        let req = CreateAccountListRequest {
            local_part: "news".into(),
            local_domain: "example.com".into(),
            ..Default::default()
        };
        crate::bridge_list_handlers::create_account_list_handler()(
            state.clone(),
            owner,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("list ok");
        create_forwarder(&state, admin, "example.com", "sales", "real@example.net")
            .await
            .expect("forwarder ok");
        let conflict = "fauna.bridges.conflicts_with_existing_alias";

        // Bulk import: the list address and the forwarder key are per-line
        // refusals; the free address beside them still lands.
        let req = ImportAccountAliasesRequest {
            lines: vec![
                "news@example.com".into(),
                "sales@example.com".into(),
                "mine@example.com".into(),
            ],
        };
        let bytes = import_account_aliases_handler()(
            state.clone(),
            other,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("import ok");
        let results = decode::<ImportAccountAliasesReply>(&bytes).unwrap().results;
        let statuses: Vec<_> = results
            .iter()
            .map(|r| (r.status, r.reason.as_deref()))
            .collect();
        assert_eq!(
            statuses,
            vec![
                (
                    ImportAliasStatus::SkippedDuplicate,
                    Some("conflicts with existing alias")
                ),
                (
                    ImportAliasStatus::SkippedDuplicate,
                    Some("conflicts with existing alias")
                ),
                (ImportAliasStatus::Created, None),
            ]
        );
        for taken in ["news", "sales"] {
            assert!(
                state
                    .db
                    .lookup_exact_alias_record("example.com", taken)
                    .await
                    .unwrap()
                    .is_none(),
                "no exact row may land on {taken}@"
            );
        }

        // Rename-by-update onto either key is refused the same way.
        let alias_id = create_alias(&state, other, "spare").await;
        for taken in ["news", "sales"] {
            let upd = UpdateAccountAliasRequest {
                alias_id: alias_id.clone(),
                pattern: taken.into(),
                controls: AliasControls::default(),
            };
            let err = update_account_alias_handler()(
                state.clone(),
                other,
                Bytes::from(encode_canonical(&upd).unwrap().to_vec()),
            )
            .await
            .expect_err("rename onto a held key refused");
            assert_eq!(err.code, conflict, "rename onto {taken}@");
        }
        assert!(
            state
                .db
                .lookup_exact_alias_record("example.com", "news")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn subaddressing_disabled_via_policy_falls_through() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // With sub-addressing on (default), `bob+work` resolves to bob.
        match resolve(&state, mta, "bob+work", "example.com").await {
            ResolveRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }

        // Admin disables sub-addressing → `bob+work` no longer strips to
        // bob; no exact/wildcard/catch-all match → 550 user unknown.
        put_alias_policy(
            &state,
            admin,
            PutAliasPolicyRequest {
                subaddressing_enabled: Some(false),
                ..Default::default()
            },
        )
        .await;
        match resolve(&state, mta, "bob+work", "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, .. } => assert_eq!(smtp_code, 550),
            other => panic!("expected Reject after disabling sub-addressing, got {other:?}"),
        }
        // The plain exact `bob@` still resolves (the knob only gates +suffix).
        match resolve(&state, mta, "bob", "example.com").await {
            ResolveRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
            }
            other => panic!("expected Resolved for plain exact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_exact_carries_controls_and_no_header() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // Create with a spam-threshold override so the resolver echoes it.
        let req = CreateAccountAliasRequest {
            kind: "exact".into(),
            local_domain: "example.com".into(),
            pattern: "bob".into(),
            controls: AliasControls {
                label: "x".into(),
                spam_threshold_override: Some(7),
                rate_limit_per_hour: Some(40),
                rate_limit_per_day: None,
            },
        };
        create_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("create ok");
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve(&state, mta, "bob", "example.com").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                control_overrides,
                is_role_address,
            } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
                assert!(!is_role_address, "exact alias is not a role address");
                let (addr, threshold) = split_threshold_stamp(&headers_to_stamp);
                assert!(addr.is_empty(), "exact stamps no ADDRESS header");
                // The alias's own override is the winning tier here.
                assert_eq!(threshold, 7);
                assert_eq!(control_overrides.spam_threshold_override, Some(7));
                assert_eq!(control_overrides.rate_limit_per_hour, Some(40));
                // The resolver carries routing controls, not the UI label.
                assert_eq!(control_overrides.label, "");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// The delivery-time fold, all three tiers through the real resolve path
    /// (`mail-aliases.md:153` — "per-alias override > per-account override >
    /// admin-tier default", resolved at delivery and stamped onto the message).
    /// One test walks the ladder on ONE alias, because the property under test
    /// is the *ordering*: each step only changes which tier is present.
    #[tokio::test]
    async fn resolve_stamps_the_folded_delivery_spam_threshold_in_tier_order() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let req = CreateAccountAliasRequest {
            kind: "exact".into(),
            local_domain: "example.com".into(),
            pattern: "bob".into(),
            controls: AliasControls::default(),
        };
        create_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("create ok");
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        async fn stamped_threshold(state: &Arc<AppState>, mta: [u8; 32]) -> u32 {
            match resolve(state, mta, "bob", "example.com").await {
                ResolveRecipientReply::Resolved {
                    headers_to_stamp, ..
                } => split_threshold_stamp(&headers_to_stamp).1,
                other => panic!("expected Resolved, got {other:?}"),
            }
        }

        // 1. Neither user tier set ⇒ the admin default rides out.
        let admin_default = state
            .db
            .get_spam_policy()
            .await
            .unwrap()
            .effective()
            .max_score_before_spam_folder;
        assert_eq!(stamped_threshold(&state, mta).await, admin_default);

        // 2. The per-account tier — the layer this row built — outranks it.
        state
            .db
            .set_spam_threshold_override(&user, Some(9))
            .await
            .unwrap();
        assert_eq!(stamped_threshold(&state, mta).await, 9);

        // 3. And the per-alias tier outranks the account tier in turn.
        let aliases = state.db.list_aliases_for_actor(&user).await.unwrap();
        let alias_id = aliases[0].alias_id;
        let set_alias_override = async |v: Option<u32>| {
            state
                .db
                .update_alias_controls(
                    &alias_id,
                    &user,
                    "bob",
                    &crate::db::mail_aliases::AliasControlsInput {
                        label: String::new(),
                        spam_threshold_override: v,
                        rate_limit_per_hour: None,
                        rate_limit_per_day: None,
                    },
                )
                .await
                .expect("update ok");
        };
        set_alias_override(Some(2)).await;
        assert_eq!(stamped_threshold(&state, mta).await, 2);

        // 4. A user tier of 0 is a CHOICE (auto-Junk off), not "unset" — it
        //    must win over a non-zero admin default, or the safety valve
        //    `mail-spam.md:29` names cannot be closed by the user.
        set_alias_override(Some(0)).await;
        assert_eq!(stamped_threshold(&state, mta).await, 0);

        // 5. Clearing the alias tier falls back to the account tier, not to
        //    the admin default — the ladder is re-entrant, not one-way.
        set_alias_override(None).await;
        assert_eq!(stamped_threshold(&state, mta).await, 9);
    }

    #[tokio::test]
    async fn resolve_disabled_alias_rejected() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;
        revoke_account_alias_handler()(
            state.clone(),
            user,
            Bytes::from(
                encode_canonical(&RevokeAccountAliasRequest { alias_id })
                    .unwrap()
                    .to_vec(),
            ),
        )
        .await
        .expect("revoke ok");
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve(&state, mta, "bob", "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("disabled"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_catch_all_then_user_unknown() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let catch_all = [9u8; 32];
        // A domain with a per-domain catch-all actor (as `add_local_domain`
        // would set it).
        state
            .db
            .add_mail_domain(
                "ca.example",
                true,
                "enforce",
                "expand_primary",
                Some(&catch_all),
                None,
            )
            .await
            .unwrap();
        match resolve(&state, mta, "no-such-user", "ca.example").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &catch_all[..]);
                // Both the catch-all flag AND the domain-tagging header (valued
                // with the RCPT domain the catch-all fired on) flow end-to-end.
                let (addr, _threshold) = split_threshold_stamp(&headers_to_stamp);
                assert_eq!(addr.len(), 2);
                assert_eq!(addr[0].name, "X-Fauna-Address-Catchall");
                assert_eq!(addr[0].value, "true");
                assert_eq!(addr[1].name, "X-Fauna-Address-Catchall-Domain");
                assert_eq!(addr[1].value, "ca.example");
            }
            other => panic!("expected catch-all Resolved, got {other:?}"),
        }
        // A domain with no catch-all → 550 user unknown.
        state
            .db
            .add_mail_domain("nc.example", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        match resolve(&state, mta, "no-such-user", "nc.example").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("User unknown"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_role_address_routes_to_admin_and_beats_catch_all() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin[..]).await.unwrap();

        // A reserved local-part with no explicit alias and no catch-all → the
        // never-reject route to the admin, flagged so the MTA bypasses quota.
        state
            .db
            .add_mail_domain("nc.example", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        match resolve(&state, mta, "postmaster", "nc.example").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                is_role_address,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &admin[..]);
                assert!(is_role_address, "role address sets the quota-bypass flag");
                let (addr, _threshold) = split_threshold_stamp(&headers_to_stamp);
                assert!(addr.is_empty(), "role route stamps no ADDRESS header");
            }
            other => panic!("expected role-address Resolved, got {other:?}"),
        }

        // Role-address mail must reach the admin even when a (non-admin)
        // catch-all is set: role (step 6) beats catch-all (step 7).
        let catch_all = [9u8; 32];
        state
            .db
            .add_mail_domain(
                "ca.example",
                true,
                "enforce",
                "expand_primary",
                Some(&catch_all),
                None,
            )
            .await
            .unwrap();
        match resolve(&state, mta, "abuse", "ca.example").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                is_role_address,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &admin[..], "role beats catch-all");
                assert!(is_role_address);
            }
            other => panic!("expected role-address Resolved (beats catch-all), got {other:?}"),
        }

        // A non-reserved miss on the catch-all domain still falls to catch-all.
        match resolve(&state, mta, "no-such-user", "ca.example").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                is_role_address,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &catch_all[..]);
                assert!(!is_role_address);
            }
            other => panic!("expected catch-all Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_role_address_override_routes_to_override_actor_else_admin() {
        // Per-domain override: postmaster@ delegated to a moderator actor, while
        // abuse@ (no override) still falls back to the deployment admin.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin[..]).await.unwrap();
        let moderator = [42u8; 32];

        state
            .db
            .add_mail_domain("ov.example", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .set_role_address_override("ov.example", "postmaster", Some(hex::encode(moderator)))
            .await
            .unwrap();

        match resolve(&state, mta, "postmaster", "ov.example").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                is_role_address,
                ..
            } => {
                assert_eq!(
                    actor_id.as_ref(),
                    &moderator[..],
                    "postmaster@ → override actor"
                );
                assert!(
                    is_role_address,
                    "still a role address (quota/greylist bypass)"
                );
            }
            other => panic!("expected override Resolved, got {other:?}"),
        }
        // abuse@ has no override → admin.
        match resolve(&state, mta, "abuse", "ov.example").await {
            ResolveRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id.as_ref(), &admin[..], "abuse@ unset → admin");
            }
            other => panic!("expected admin Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_role_address_override_ignored_for_tlsrpt_and_dmarc() {
        // tlsrpt@/dmarc-report@ always route to the deployment-wide processor
        // (the admin today) regardless of any override — they are not overridable
        // (`mail-multidomain.md` § Per-domain override).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin[..]).await.unwrap();

        state
            .db
            .add_mail_domain("rp.example", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // Even with the four overridable roles all delegated, the report roles
        // stay on the admin.
        for role in ["postmaster", "abuse", "noc", "security"] {
            state
                .db
                .set_role_address_override("rp.example", role, Some(hex::encode([99u8; 32])))
                .await
                .unwrap();
        }
        for local in ["tlsrpt", "dmarc-report"] {
            match resolve(&state, mta, local, "rp.example").await {
                ResolveRecipientReply::Resolved {
                    actor_id,
                    is_role_address,
                    ..
                } => {
                    assert_eq!(
                        actor_id.as_ref(),
                        &admin[..],
                        "{local}@ → deployment processor (admin)"
                    );
                    assert!(is_role_address);
                }
                other => panic!("expected admin Resolved for {local}@, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn resolve_role_address_malformed_override_degrades_to_admin() {
        // A corrupt `role_address_overrides` value must never 550 — degrade to the
        // admin mailbox (never-reject invariant).
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin[..]).await.unwrap();

        state
            .db
            .add_mail_domain(
                "bad.example",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        // Inject a non-hex value directly (bypassing the setter, which always
        // encodes valid hex) to simulate a corrupt column.
        state
            .db
            .update_mail_domain_config(
                "bad.example",
                crate::db::mail_domains::MailDomainUpdate {
                    role_address_overrides_json: Some(Some(
                        r#"{"postmaster":"not-a-valid-hex"}"#.into(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        match resolve(&state, mta, "postmaster", "bad.example").await {
            ResolveRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id.as_ref(), &admin[..], "malformed override → admin");
            }
            other => panic!("expected degraded admin Resolved, got {other:?}"),
        }
    }

    /// A stored override that does not decode reaches the app as **no
    /// override**, typed — the admin-mail reply never fails over one corrupt
    /// column (`mail-multidomain.md` § Wire shape + storage).
    #[tokio::test]
    async fn a_malformed_stored_override_projects_as_none_on_the_wire() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "bad.example",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        let row = state
            .db
            .update_mail_domain_config(
                "bad.example",
                crate::db::mail_domains::MailDomainUpdate {
                    role_address_overrides_json: Some(Some("{not json".into())),
                    dmarc_overrides_json: Some(Some(r#"{"policy_mode":"bogus"}"#.into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let wire = mail_domain_to_row(&row);
        assert!(wire.role_address_overrides.is_empty());
        assert_eq!(wire.dmarc_overrides, Default::default());
        // And it still encodes: the reply this row rides in is answerable.
        let bytes = encode_canonical(&wire).unwrap();
        assert_eq!(decode::<MailDomainRow>(&bytes).unwrap(), wire);
    }

    #[tokio::test]
    async fn set_role_address_handler_designates_then_clears() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("role.example"))
            .await
            .unwrap();

        let actor = [9u8; 32];
        // Designate the abuse@ override.
        let set = Bytes::from(
            encode_canonical(&SetRoleAddressRequest {
                domain: "role.example".into(),
                role: RoleAddressKind::Abuse,
                actor_id: Some(ByteBuf::from(actor.to_vec())),
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_role_address_handler()(state.clone(), admin, set)
            .await
            .expect("set ok");
        let reply: SetRoleAddressReply = decode(&reply).unwrap();
        assert_eq!(
            reply.domain.role_address_overrides.abuse.as_deref(),
            Some(hex::encode(actor).as_str())
        );

        // Clear it — the reply row no longer carries the override.
        let clear = Bytes::from(
            encode_canonical(&SetRoleAddressRequest {
                domain: "role.example".into(),
                role: RoleAddressKind::Abuse,
                actor_id: None,
            })
            .unwrap()
            .to_vec(),
        );
        let reply = set_role_address_handler()(state.clone(), admin, clear)
            .await
            .expect("clear ok");
        let reply: SetRoleAddressReply = decode(&reply).unwrap();
        assert!(
            reply.domain.role_address_overrides.is_empty(),
            "clearing the only override leaves the row with none"
        );
    }

    #[tokio::test]
    async fn set_role_address_handler_rejects_non_32_byte_actor() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let bad = Bytes::from(
            encode_canonical(&SetRoleAddressRequest {
                domain: "primary.example".into(),
                role: RoleAddressKind::Postmaster,
                actor_id: Some(ByteBuf::from(vec![1u8; 16])),
            })
            .unwrap()
            .to_vec(),
        );
        let err = set_role_address_handler()(state, admin, bad)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn resolve_role_address_with_no_admin_tempfails_451() {
        // Never-reject invariant: a reserved name with no admin claimed must
        // surface an RPC error (→ 451 on the wire), never a hard 550.
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        state
            .db
            .add_mail_domain("nc.example", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let req = ResolveRecipientRequest {
            local_part: "postmaster".into(),
            domain: "nc.example".into(),
            sender_domain: String::new(),
            ..Default::default()
        };
        let err = resolve_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect_err("reserved name with no admin must error, not resolve/550");
        // The error maps to a 451 tempfail at the bridge, not a 550.
        let _ = err;
    }

    #[tokio::test]
    async fn resolve_recipient_requires_bridge_class() {
        let state = fixture_state().await;
        let req = || ResolveRecipientRequest {
            local_part: "bob".into(),
            domain: "example.com".into(),
            sender_domain: String::new(),
            ..Default::default()
        };

        // A bare actor resolves to User → denied.
        let user = [5u8; 32];
        let err = resolve_recipient_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req()).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");

        // The MDA legitimately resolves a recipient in its auto-schedule
        // classifier (step C5), so it passes the caller gate —
        // the handler RUNS and returns a reply (unknown `bob` → a reject outcome),
        // NOT a permission error. The MTA path is unchanged.
        let mda = [9u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        resolve_recipient_handler()(
            state.clone(),
            mda,
            Bytes::from(encode_canonical(&req()).unwrap().to_vec()),
        )
        .await
        .expect("BridgeMda must pass the caller gate for resolve_recipient");
    }

    // ── Guardian mail gate: `reject` at RCPT TO ──────────────────────
    //
    // `family-safety.md` § The mail gate. This is the ONLY per-recipient stage
    // in SMTP — a refusal decided at `DATA` emits one reply for every
    // recipient, so a stranger mailing both a ward and a parent would deny the
    // parent their copy. Everything else about the gate is proven end-to-end in
    // `tests/conformance_family_mail_gate.rs` (tier_3).

    /// A ward under `unknown_sender_mail = <policy>`, plus an unsupervised adult
    /// on the same domain. Both have exact aliases so the resolver finds them.
    async fn fixture_ward_and_adult(
        domain: &str,
        policy: &str,
    ) -> (Arc<AppState>, [u8; 32], [u8; 32]) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.add_mail_domain(domain, true, "testing", "self_signed", None, None)
            .await
            .unwrap();
        let guardian = [3u8; 32];
        let ward = [4u8; 32];
        let adult = [6u8; 32];
        db.create_user_with_handle(&guardian, "personal", "parent", None)
            .await
            .unwrap();
        db.create_user_with_handle(&ward, "personal", "kid", Some(&guardian[..]))
            .await
            .unwrap();
        db.create_user_with_handle(&adult, "personal", "adult", None)
            .await
            .unwrap();
        db.update_guardian_policy(
            &ward[..],
            false,
            policy,
            true,
            "allow",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.put_exact_alias(domain, "kid", "exact", &ward)
            .await
            .unwrap();
        db.put_exact_alias(domain, "adult", "exact", &adult)
            .await
            .unwrap();
        let state = Arc::new(AppState::for_test(db));
        (state, ward, adult)
    }

    async fn resolve_as_mta(
        state: &Arc<AppState>,
        mta: [u8; 32],
        local_part: &str,
        domain: &str,
        sender_address: &str,
    ) -> ResolveRecipientReply {
        // Struct-update fixture on a growing wire type: keep
        // `..Default::default()` even while it's a no-op, so concurrent
        // field-adds merge cleanly instead of colliding on the grown axis.
        #[allow(clippy::needless_update)]
        let req = ResolveRecipientRequest {
            local_part: local_part.into(),
            domain: domain.into(),
            sender_domain: String::new(),
            sender_address: sender_address.into(),
            ..Default::default()
        };
        let out = resolve_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("resolve_recipient must answer, never error");
        decode(&out).unwrap()
    }

    #[tokio::test]
    async fn rcpt_rejects_a_cold_sender_for_a_reject_ward_but_a_co_recipient_adult_still_resolves()
    {
        let domain = "ex.test";
        let (state, _ward, _adult) = fixture_ward_and_adult(domain, "reject").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // Same message, same stranger, two RCPT TO commands.
        let for_ward = resolve_as_mta(&state, mta, "kid", domain, "stranger@out.test").await;
        // network-exposure.md § Rulings F5: the ward refusal must be
        // INDISTINGUISHABLE from a nonexistent address — no sharper "approved
        // senders only" text that would reveal to any off-allowlist stranger
        // that `kid@` exists AND is a policy-restricted (typically a minor's)
        // ward. Compare byte-for-byte against a genuinely-unknown local-part.
        let for_unknown =
            resolve_as_mta(&state, mta, "no-such-mailbox", domain, "stranger@out.test").await;
        match (&for_ward, &for_unknown) {
            (
                ResolveRecipientReply::Reject {
                    smtp_code: ward_code,
                    reason: ward_reason,
                },
                ResolveRecipientReply::Reject {
                    smtp_code: unknown_code,
                    reason: unknown_reason,
                },
            ) => {
                assert_eq!(*ward_code, 550);
                assert_eq!(*unknown_code, 550);
                assert_eq!(
                    ward_reason, unknown_reason,
                    "the ward reject must be byte-identical to an unknown-address \
                     reject (ward={ward_reason:?}, unknown={unknown_reason:?})"
                );
                assert_eq!(ward_reason, "User unknown", "unified generic reject");
            }
            other => panic!("both must be refused at RCPT with 550, got {other:?}"),
        }

        // The parent's copy is untouched: this is why the refusal cannot live at
        // ingest, where one DATA reply covers every recipient.
        assert!(
            matches!(
                resolve_as_mta(&state, mta, "adult", domain, "stranger@out.test").await,
                ResolveRecipientReply::Resolved { .. }
            ),
            "an unsupervised co-recipient still receives the message"
        );
    }

    #[tokio::test]
    async fn rcpt_resolves_a_known_sender_and_never_rejects_under_hold_or_allow() {
        let domain = "ex.test";
        let mta = [1u8; 32];

        // A sender the ward has itself mailed is known → always resolves.
        let (state, ward, _) = fixture_ward_and_adult(domain, "reject").await;
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        state
            .db
            .add_mail_allowlist_entry(&ward[..], "pal@out.test", "outbound")
            .await
            .unwrap();
        assert!(matches!(
            resolve_as_mta(&state, mta, "kid", domain, "PAL@Out.Test").await,
            ResolveRecipientReply::Resolved { .. }
        ));

        // `hold` and `allow` never refuse at RCPT — a hold is a placement
        // decision made later, at the sealed-ingest core.
        for policy in ["hold", "allow"] {
            let (state, _, _) = fixture_ward_and_adult(domain, policy).await;
            approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
            assert!(
                matches!(
                    resolve_as_mta(&state, mta, "kid", domain, "stranger@out.test").await,
                    ResolveRecipientReply::Resolved { .. }
                ),
                "{policy} must accept the RCPT"
            );
        }
    }

    #[tokio::test]
    async fn rcpt_never_rejects_the_null_reverse_path_of_a_bounce() {
        // An empty MAIL FROM is the SMTP null reverse-path: a DSN. Refusing it
        // would strand the ward with no way to learn their mail bounced.
        let domain = "ex.test";
        let (state, _, _) = fixture_ward_and_adult(domain, "reject").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        assert!(matches!(
            resolve_as_mta(&state, mta, "kid", domain, "").await,
            ResolveRecipientReply::Resolved { .. }
        ));
    }

    // ── RFC 8058 mailto one-click unsubscribe (mass-mailing #5) ──────

    #[test]
    fn match_unsubscribe_local_part_recognizes_family() {
        // Bare form → Some(None); case-insensitive base.
        assert_eq!(match_unsubscribe_local_part("unsubscribe"), Some(None));
        assert_eq!(match_unsubscribe_local_part("Unsubscribe"), Some(None));
        // +token form → Some(Some(token)); the token case is preserved (the
        // base64url token is case-sensitive even though the base is folded).
        assert_eq!(
            match_unsubscribe_local_part("unsubscribe+AbC-_9"),
            Some(Some("AbC-_9"))
        );
        assert_eq!(
            match_unsubscribe_local_part("UNSUBSCRIBE+AbC-_9"),
            Some(Some("AbC-_9"))
        );
        // Split on the FIRST '+'; any later '+' stays in the token.
        assert_eq!(
            match_unsubscribe_local_part("unsubscribe+a+b"),
            Some(Some("a+b"))
        );
        // Not the family — a normal mailbox / a different word / a +suffix on a
        // non-unsubscribe base.
        for no in ["unsubscribed", "bob", "bob+work", "unsub", "subscribe"] {
            assert_eq!(match_unsubscribe_local_part(no), None, "{no:?}");
        }
    }

    #[tokio::test]
    async fn resolve_recipient_mailto_unsubscribe_discards_and_flips_member() {
        let state = fixture_state().await;
        let mta = [7u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let owner = [5u8; 32];
        let (list_id, _alias_id) = state
            .db
            .create_list(
                &owner,
                "example.com",
                "weekly",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        // The token is normally HMAC-derived; the resolver only needs the
        // cached index, so a fixed mixed-case value exercises both the
        // case-preservation path and the flip-by-token path.
        let token = "MailTok_9-x";
        state
            .db
            .add_member(&list_id, "alice@example.net", token)
            .await
            .unwrap();

        let req = ResolveRecipientRequest {
            local_part: format!("unsubscribe+{token}"),
            domain: "example.com".into(),
            sender_domain: "sender.example".into(),
            ..Default::default()
        };
        let bytes = resolve_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap();
        let reply = decode::<ResolveRecipientReply>(&bytes).unwrap();
        assert!(
            matches!(reply, ResolveRecipientReply::Discard),
            "expected Discard, got {reply:?}"
        );
        // The flip already happened during resolution → a fresh by-token call
        // now reports AlreadyUnsubscribed.
        assert_eq!(
            state.db.unsubscribe_member_by_token(token).await.unwrap(),
            crate::db::mail_lists::UnsubscribeOutcome::AlreadyUnsubscribed
        );
    }

    #[tokio::test]
    async fn resolve_recipient_bare_unsubscribe_rejects_550() {
        let state = fixture_state().await;
        let mta = [7u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = ResolveRecipientRequest {
            local_part: "unsubscribe".into(),
            domain: "example.com".into(),
            sender_domain: String::new(),
            ..Default::default()
        };
        let bytes = resolve_recipient_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap();
        match decode::<ResolveRecipientReply>(&bytes).unwrap() {
            ResolveRecipientReply::Reject { smtp_code, .. } => assert_eq!(smtp_code, 550),
            other => panic!("expected Reject 550, got {other:?}"),
        }
    }

    /// Inbound to a list's posting address refuses with the goal's 550
    /// (`mail-mass-mailing.md` § Pattern) — on a domain with a live catch-all
    /// actor, which before this step silently took list mail.
    #[tokio::test]
    async fn resolve_recipient_list_address_refuses_ahead_of_catch_all() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        add_admin(&state.db, &admin).await;
        let mta = [8u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("primary.example"))
            .await
            .unwrap();
        let _ = add_local_domain_handler()(state.clone(), admin, add_req("cat.example"))
            .await
            .unwrap();
        let catch_all = [9u8; 32];
        let set = Bytes::from(
            encode_canonical(&SetCatchAllActorRequest {
                domain: "cat.example".into(),
                actor_id: Some(ByteBuf::from(catch_all.to_vec())),
            })
            .unwrap()
            .to_vec(),
        );
        set_catch_all_actor_handler()(state.clone(), admin, set)
            .await
            .unwrap();
        state
            .db
            .create_list(
                &[5u8; 32],
                "cat.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let resolve = |local_part: &str| {
            let req = ResolveRecipientRequest {
                local_part: local_part.into(),
                domain: "cat.example".into(),
                sender_domain: "sender.example".into(),
                ..Default::default()
            };
            resolve_recipient_handler()(
                state.clone(),
                mta,
                Bytes::from(encode_canonical(&req).unwrap().to_vec()),
            )
        };
        match decode::<ResolveRecipientReply>(&resolve("News").await.unwrap()).unwrap() {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert_eq!(reason, fauna_mail::aliases::LIST_SUBMISSIONS_REFUSED);
            }
            other => panic!("expected list Reject 550, got {other:?}"),
        }
        // Control: the catch-all is live for a non-list address.
        match decode::<ResolveRecipientReply>(&resolve("typo").await.unwrap()).unwrap() {
            ResolveRecipientReply::Resolved { actor_id, .. } => {
                assert_eq!(actor_id.as_ref(), catch_all.as_slice());
            }
            other => panic!("expected catch-all Resolved, got {other:?}"),
        }
    }

    // ── A2.3 — disposable mint + resolution ──────────────────────────

    async fn generate_disposable(
        state: &Arc<AppState>,
        user: [u8; 32],
        ttl_days: Option<u32>,
        uses: Option<u32>,
        label: &str,
    ) -> GenerateDisposableAliasReply {
        let req = GenerateDisposableAliasRequest {
            ttl_days,
            uses,
            label: label.into(),
        };
        let bytes = generate_disposable_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_or_else(|e| panic!("generate disposable failed: {}", e.code));
        decode::<GenerateDisposableAliasReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn generate_disposable_mints_resolves_then_expires_on_use() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // Canonical address (oldest exact alias) → the mint derives <handle>
        // ("bob") + <domain> ("example.com") from it.
        create_alias(&state, user, "bob").await;

        let reply = generate_disposable(&state, user, None, None, "amazon").await;
        assert_eq!(reply.token.len(), 6);
        assert_eq!(
            reply.full_address,
            format!("bob-temp-{}@example.com", reply.token)
        );

        // The minted row carries the disposable controls + TTL/uses.
        let rows = list_aliases(&state, user).await;
        let disp = rows
            .iter()
            .find(|r| r.kind == "disposable")
            .expect("disposable row");
        assert_eq!(disp.pattern, reply.token);
        assert_eq!(disp.label, "amazon");
        assert_eq!(disp.uses_remaining, Some(1));
        assert!(disp.expires_at.is_some());
        assert_eq!(disp.rate_limit_per_day, Some(100));

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let local_part = format!("bob-temp-{}", reply.token);
        // First inbound routes + stamps the (header-only) disposable token.
        match resolve(&state, mta, &local_part, "example.com").await {
            ResolveRecipientReply::Resolved {
                actor_id,
                headers_to_stamp,
                ..
            } => {
                assert_eq!(actor_id.as_ref(), &user[..]);
                let (addr, _threshold) = split_threshold_stamp(&headers_to_stamp);
                assert_eq!(addr.len(), 1);
                assert_eq!(addr[0].name, "X-Fauna-Address-Disposable");
                assert_eq!(addr[0].value, reply.token);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        // The use is consumed (1 → 0); the second inbound is rejected expired.
        match resolve(&state, mta, &local_part, "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("expired"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_disposable_unlimited_uses_resolves_repeatedly() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        // uses = 0 → unlimited (stored NULL uses_remaining).
        let reply = generate_disposable(&state, user, None, Some(0), "").await;
        let rows = list_aliases(&state, user).await;
        let disp = rows.iter().find(|r| r.kind == "disposable").unwrap();
        assert_eq!(disp.uses_remaining, None);

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let local_part = format!("bob-temp-{}", reply.token);
        for _ in 0..3 {
            assert!(matches!(
                resolve(&state, mta, &local_part, "example.com").await,
                ResolveRecipientReply::Resolved { .. }
            ));
        }
    }

    // ── Per-alias rate cap (`mail-aliases.md` § Per-alias rate-cap) ──

    /// Overwrite one alias's stored controls straight through the DB — the
    /// update RPC needs the pattern, which for a disposable is its minted
    /// token; these tests only care about the caps.
    async fn set_alias_caps(
        state: &Arc<AppState>,
        user: [u8; 32],
        alias_id: &[u8; 16],
        pattern: &str,
        per_hour: Option<i64>,
        per_day: Option<i64>,
    ) {
        state
            .db
            .update_alias_controls(
                alias_id,
                &user,
                pattern,
                &crate::db::mail_aliases::AliasControlsInput {
                    label: String::new(),
                    spam_threshold_override: None,
                    rate_limit_per_hour: per_hour,
                    rate_limit_per_day: per_day,
                },
            )
            .await
            .expect("set caps");
    }

    /// The cap admits exactly `cap` messages per window and tempfails the
    /// next one with the ratified `451` — and, load-bearingly, the tempfailed
    /// resolve logs **no** `alias_hits` row. If it did, every retry of a
    /// tempfailed message would re-arm the window and a capped alias could
    /// never recover: the sender retries, the retry counts, the count stays
    /// at the cap, forever.
    #[tokio::test]
    async fn resolve_recipient_tempfails_over_the_per_hour_rate_cap() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;
        let alias_id: [u8; 16] = alias_id.as_ref().try_into().unwrap();
        set_alias_caps(&state, user, &alias_id, "bob", Some(2), None).await;

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // The two the window admits.
        for i in 0..2 {
            assert!(
                matches!(
                    resolve(&state, mta, "bob", "example.com").await,
                    ResolveRecipientReply::Resolved { .. }
                ),
                "message {i} is under the cap and must deliver"
            );
        }
        // The third finds the window full.
        match resolve(&state, mta, "bob", "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 451, "a rate cap TEMPfails, never 5xx");
                assert_eq!(reason, "Rate limit exceeded");
            }
            other => panic!("expected the rate-cap Reject, got {other:?}"),
        }

        // Exactly two hits: the reject logged none.
        let hits = state
            .db
            .list_alias_hits_for_owner(&user, &alias_id, 100, None)
            .await
            .unwrap()
            .expect("owned");
        assert_eq!(
            hits.len(),
            2,
            "a tempfailed resolve must not log a hit — else retries re-arm the cap"
        );

        // Raising the cap re-opens delivery against the same history, so the
        // gate reads live control state rather than latching.
        set_alias_caps(&state, user, &alias_id, "bob", Some(3), None).await;
        assert!(matches!(
            resolve(&state, mta, "bob", "example.com").await,
            ResolveRecipientReply::Resolved { .. }
        ));
    }

    /// An unlimited alias never pays the count, and never rejects.
    #[tokio::test]
    async fn resolve_recipient_ignores_hit_history_without_a_cap() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        for _ in 0..5 {
            assert!(matches!(
                resolve(&state, mta, "bob", "example.com").await,
                ResolveRecipientReply::Resolved { .. }
            ));
        }
    }

    /// A `0` cap blocks every message — the tempfail "pause this alias",
    /// distinct from `disabled`'s permanent `550`. `0` is reachable because
    /// the create/update doors refuse only a NEGATIVE cap.
    #[tokio::test]
    async fn a_zero_rate_cap_tempfails_the_very_first_message() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;
        let alias_id: [u8; 16] = alias_id.as_ref().try_into().unwrap();
        set_alias_caps(&state, user, &alias_id, "bob", None, Some(0)).await;

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve(&state, mta, "bob", "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 451);
                assert_eq!(reason, "Rate limit exceeded");
            }
            other => panic!("expected the rate-cap Reject, got {other:?}"),
        }
    }

    /// The gate runs AHEAD of the disposable decrement, so a message the cap
    /// turns away costs the alias none of its `uses_remaining`. A sender
    /// retrying a tempfail would otherwise burn a one-shot disposable without
    /// ever delivering a message.
    #[tokio::test]
    async fn an_over_cap_disposable_resolve_burns_no_use() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let reply = generate_disposable(&state, user, None, Some(5), "amazon").await;

        let rows = list_aliases(&state, user).await;
        let disp = rows.iter().find(|r| r.kind == "disposable").unwrap();
        let disp_id: [u8; 16] = disp.alias_id.as_ref().try_into().unwrap();
        assert_eq!(disp.uses_remaining, Some(5));
        // Replace the minted hard 100/day default with a 1/day cap.
        set_alias_caps(&state, user, &disp_id, &reply.token, None, Some(1)).await;

        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let local_part = format!("bob-temp-{}", reply.token);
        assert!(matches!(
            resolve(&state, mta, &local_part, "example.com").await,
            ResolveRecipientReply::Resolved { .. }
        ));
        // Second is over the 1/day cap.
        match resolve(&state, mta, &local_part, "example.com").await {
            ResolveRecipientReply::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 451);
                assert_eq!(reason, "Rate limit exceeded");
            }
            other => panic!("expected the rate-cap Reject, got {other:?}"),
        }
        let rows = list_aliases(&state, user).await;
        let disp = rows.iter().find(|r| r.kind == "disposable").unwrap();
        assert_eq!(
            disp.uses_remaining,
            Some(4),
            "only the DELIVERED message may consume a use"
        );
    }

    #[tokio::test]
    async fn generate_disposable_requires_canonical_address() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // No exact alias → no <handle>/<domain> to mint from.
        let req = GenerateDisposableAliasRequest::default();
        let err = generate_disposable_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.no_canonical_address");
    }

    #[tokio::test]
    async fn generate_disposable_requires_user_class() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = GenerateDisposableAliasRequest::default();
        let err = generate_disposable_alias_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn generate_disposable_enforces_per_day_cap() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let cap = fauna_mail::aliases::DISPOSABLE_GENERATE_PER_DAY_DEFAULT;
        // Exactly `cap` mints succeed.
        for _ in 0..cap {
            generate_disposable(&state, user, None, None, "").await;
        }
        // The next is rate-limited.
        let req = GenerateDisposableAliasRequest::default();
        let err = generate_disposable_alias_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.disposable_generate_rate_limited");
    }

    // ── A2.4 — alias-hit logging + list_account_alias_hits ───────────

    async fn list_alias_hits(
        state: &Arc<AppState>,
        user: [u8; 32],
        alias_id: &ByteBuf,
        limit: u32,
        before_hit_id: Option<ByteBuf>,
    ) -> ListAccountAliasHitsReply {
        let req = ListAccountAliasHitsRequest {
            alias_id: alias_id.clone(),
            limit,
            before_hit_id,
        };
        let bytes = list_account_alias_hits_handler()(
            state.clone(),
            user,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_or_else(|e| panic!("list alias hits failed: {}", e.code));
        decode::<ListAccountAliasHitsReply>(&bytes).unwrap()
    }

    #[tokio::test]
    async fn resolved_exact_logs_a_hit_listable_by_owner() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        // An exact-match resolve with a MAIL FROM domain logs one hit.
        assert!(matches!(
            resolve_from(&state, mta, "bob", "example.com", "amazon.com").await,
            ResolveRecipientReply::Resolved { .. }
        ));

        let reply = list_alias_hits(&state, user, &alias_id, 100, None).await;
        assert_eq!(reply.hits.len(), 1);
        assert_eq!(reply.hits[0].matched_address, "bob@example.com");
        assert_eq!(reply.hits[0].sender_domain, "amazon.com");
    }

    #[tokio::test]
    async fn disposable_logs_hit_only_on_successful_consume() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        create_alias(&state, user, "bob").await;
        let mint = generate_disposable(&state, user, None, None, "amazon").await;
        let disp_alias_id = mint.alias_id.clone();
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let local_part = format!("bob-temp-{}", mint.token);

        // First resolve consumes the single use + logs a hit.
        assert!(matches!(
            resolve_from(&state, mta, &local_part, "example.com", "shop.example").await,
            ResolveRecipientReply::Resolved { .. }
        ));
        // Second resolve is rejected expired → must NOT log a second hit.
        assert!(matches!(
            resolve_from(&state, mta, &local_part, "example.com", "shop.example").await,
            ResolveRecipientReply::Reject { .. }
        ));

        let reply = list_alias_hits(&state, user, &disp_alias_id, 100, None).await;
        assert_eq!(reply.hits.len(), 1, "only the successful route logs a hit");
        assert_eq!(reply.hits[0].sender_domain, "shop.example");
    }

    #[tokio::test]
    async fn list_alias_hits_owner_isolation() {
        let state = fixture_state().await;
        let owner = [5u8; 32];
        let other = [6u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state.db.create_user(&other, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, owner, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        resolve_from(&state, mta, "bob", "example.com", "amazon.com").await;

        // A different user requesting the owner's alias hits → not_found
        // (never leaks the existence of another user's hits).
        let req = ListAccountAliasHitsRequest {
            alias_id: alias_id.clone(),
            limit: 100,
            before_hit_id: None,
        };
        let err = list_account_alias_hits_handler()(
            state.clone(),
            other,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.not_found");
    }

    #[tokio::test]
    async fn list_alias_hits_requires_user_class() {
        let state = fixture_state().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let alias_id = create_alias(&state, user, "bob").await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = ListAccountAliasHitsRequest {
            alias_id,
            limit: 100,
            before_hit_id: None,
        };
        let err = list_account_alias_hits_handler()(
            state.clone(),
            mta,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn catch_all_resolve_logs_no_hit() {
        // A catch-all match has no `account_aliases` row → the handler must not
        // attempt a hit log (which, with FK on, would fail on a dangling
        // alias_id and surface as an Err — the `resolve_from` helper would then
        // panic). This test passing proves catch-all logs no hit. (The DB-side
        // `delete_alias_cascades_its_hits` test asserts the row-count directly,
        // where the `conn` field is in scope.)
        let state = fixture_state().await;
        let catch_all = [9u8; 32];
        state
            .db
            .add_mail_domain(
                "catchall.test",
                true,
                "enforce",
                "expand_primary",
                Some(&catch_all),
                None,
            )
            .await
            .unwrap();
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        match resolve_from(&state, mta, "whoever", "catchall.test", "amazon.com").await {
            ResolveRecipientReply::Resolved {
                headers_to_stamp, ..
            } => {
                assert_eq!(headers_to_stamp[0].name, "X-Fauna-Address-Catchall");
            }
            other => panic!("expected catch-all Resolved, got {other:?}"),
        }
    }

    // ── T2.4 report_tls_attempt → TLSRPT bucket reconstruction ──────

    fn found_policy_wire(mode: &str, mx: &str) -> MtaStsPolicyWire {
        MtaStsPolicyWire {
            id: "id-test".into(),
            mode: mode.into(),
            mx: vec![mx.into()],
            max_age_secs: 604_800,
        }
    }

    #[test]
    fn report_outcome_no_policy_success_buckets_as_no_policy_found() {
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: None,
            mta_sts_outcome: "not_published".into(),
            ..Default::default()
        };
        let outcome = report_to_attempt_outcome(req).unwrap();
        assert_eq!(outcome.policy.policy_type, "no-policy-found");
        assert_eq!(outcome.policy.policy_domain, "dest.test");
        assert!(outcome.policy.policy_string.is_empty());
        assert!(outcome.failure_type.is_none());
    }

    #[test]
    fn report_outcome_dane_takes_precedence_over_sts() {
        // Both an enforce STS policy AND DANE records present — RFC 8460 §4.3
        // gives `tlsa` precedence; the bucket is keyed on the MX host.
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: Some("tlsa-invalid".into()),
            mta_sts_outcome: "found".into(),
            mta_sts_policy: Some(found_policy_wire("enforce", "mx.dest.test")),
            tlsa_records: vec![TlsaRecordWire {
                usage: 3,
                selector: 1,
                matching: 1,
                data: vec![0xde, 0xad, 0xbe, 0xef],
            }],
        };
        let outcome = report_to_attempt_outcome(req).unwrap();
        assert_eq!(outcome.policy.policy_type, "tlsa");
        assert_eq!(outcome.policy.policy_domain, "mx.dest.test");
        assert_eq!(outcome.policy.policy_string, vec!["3 1 1 deadbeef"]);
        assert_eq!(outcome.failure_type.as_deref(), Some("tlsa-invalid"));
    }

    #[test]
    fn report_outcome_sts_found_buckets_with_policy_strings() {
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: Some("sts-webpki-invalid".into()),
            mta_sts_outcome: "found".into(),
            mta_sts_policy: Some(found_policy_wire("enforce", "mx.dest.test")),
            ..Default::default()
        };
        let outcome = report_to_attempt_outcome(req).unwrap();
        assert_eq!(outcome.policy.policy_type, "sts");
        assert_eq!(outcome.policy.policy_domain, "dest.test");
        // policy_strings() canonical projection (single-sourced in fauna-mail).
        assert!(
            outcome
                .policy
                .policy_string
                .contains(&"version: STSv1".to_string()),
            "got {:?}",
            outcome.policy.policy_string
        );
        assert!(
            outcome
                .policy
                .policy_string
                .contains(&"mode: enforce".to_string())
        );
    }

    #[test]
    fn report_outcome_sts_fetch_error_has_empty_policy_string() {
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: Some("sts-policy-fetch-error".into()),
            mta_sts_outcome: "fetch_error".into(),
            ..Default::default()
        };
        let outcome = report_to_attempt_outcome(req).unwrap();
        assert_eq!(outcome.policy.policy_type, "sts");
        assert!(outcome.policy.policy_string.is_empty());
    }

    #[test]
    fn report_outcome_rejects_malformed_attribution() {
        // found without a policy
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            mta_sts_outcome: "found".into(),
            ..Default::default()
        };
        assert!(report_to_attempt_outcome(req).is_err());
        // unknown outcome
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            mta_sts_outcome: "bogus".into(),
            ..Default::default()
        };
        assert!(report_to_attempt_outcome(req).is_err());
        // unknown mode under found
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            mta_sts_outcome: "found".into(),
            mta_sts_policy: Some(found_policy_wire("bogus", "mx.dest.test")),
            ..Default::default()
        };
        assert!(report_to_attempt_outcome(req).is_err());
    }

    #[tokio::test]
    async fn report_tls_attempt_handler_records_into_aggregator() {
        let state = fixture_state().await;
        let mta = [1u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let mk = |result_type: Option<&str>| {
            let req = ReportTlsAttemptRequest {
                recipient_domain: "dest.test".into(),
                mx_host: "mx.dest.test".into(),
                result_type: result_type.map(str::to_string),
                mta_sts_outcome: "not_published".into(),
                ..Default::default()
            };
            Bytes::from(encode_canonical(&req).unwrap().to_vec())
        };

        // One success, one starttls-not-supported failure — same bucket.
        report_tls_attempt_handler()(state.clone(), mta, mk(None))
            .await
            .expect("success report ok");
        report_tls_attempt_handler()(state.clone(), mta, mk(Some("starttls-not-supported")))
            .await
            .expect("failure report ok");

        let snap = state
            .email
            .tlsrpt_aggregator
            .lock()
            .unwrap()
            .dump_snapshot();
        assert_eq!(snap.len(), 1, "one recipient domain");
        assert_eq!(snap[0].domain, "dest.test");
        assert_eq!(snap[0].policies.len(), 1);
        let p = &snap[0].policies[0];
        assert_eq!(p.policy_type, "no-policy-found");
        assert_eq!(p.total_success, 1);
        assert_eq!(p.total_failure, 1);
        assert_eq!(p.failures.len(), 1);
        assert_eq!(p.failures[0].result_type, "starttls-not-supported");
    }

    #[tokio::test]
    async fn report_tls_attempt_handler_rejects_non_mta_class() {
        let state = fixture_state().await;
        let mda = [2u8; 32];
        approve_bridge(&state.db, &mda, BridgeRole::Mda).await;
        let req = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            mta_sts_outcome: "not_published".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = report_tls_attempt_handler()(state, mda, payload)
            .await
            .expect_err("MDA must not be allowed to report TLS attempts");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    // ── resolve_mx — DNSSEC provenance crosses the wire ───────────────
    // smtp-server.md § Architectural rules (outbound DANE): nest owns the
    // DNSSEC-validating MX lookup because the Go stdlib resolver cannot do
    // one, and the reply must carry whether the RRset validated — the
    // bridge gates DANE pinning on exactly that bit. A handler that dropped
    // it (or defaulted it true) would silently restore the finding: the
    // TLSA leg's DNSSEC validation would go on authenticating whatever name
    // a DNS-spoofing attacker put in the MX answer.

    /// Scripted `MxRrsetResolver` — the seam `LiveMxRrsetResolver` fills in
    /// production, so the handler's mapping can be tested without DNS.
    struct ScriptedMx(fauna_mail::outbound::mx::MxAnswer);

    #[async_trait::async_trait]
    impl fauna_mail::outbound::mx::MxRrsetResolver for ScriptedMx {
        async fn lookup(
            &self,
            _domain: &str,
        ) -> anyhow::Result<fauna_mail::outbound::mx::MxAnswer> {
            Ok(self.0.clone())
        }
    }

    async fn resolve_mx_via_handler(answer: fauna_mail::outbound::mx::MxAnswer) -> ResolveMxReply {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let mut st = AppState::for_test(db);
        st.mx_resolver = Arc::new(ScriptedMx(answer));
        let state = Arc::new(st);
        let mta = [0x31u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;

        let req = ResolveMxRequest {
            domain: "dest.test".to_string(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let raw = resolve_mx_handler()(state, mta, payload)
            .await
            .expect("resolve_mx ok");
        fauna_protocol::decode_strict(&raw).expect("decode ResolveMxReply")
    }

    #[tokio::test]
    async fn resolve_mx_carries_a_secure_rrset_through_unchanged() {
        let reply = resolve_mx_via_handler(fauna_mail::outbound::mx::MxAnswer {
            hosts: vec![(10, "mx-a.dest.test".into()), (20, "mx-b.dest.test".into())],
            secure: true,
        })
        .await;
        assert!(
            reply.secure,
            "a DNSSEC-validated MX RRset must reach the bridge as secure — \
             otherwise DANE is silently off for every correctly-signed domain"
        );
        assert_eq!(
            reply
                .hosts
                .iter()
                .map(|h| (h.priority, h.hostname.as_str()))
                .collect::<Vec<_>>(),
            vec![(10, "mx-a.dest.test"), (20, "mx-b.dest.test")],
        );
    }

    #[tokio::test]
    async fn resolve_mx_reports_an_unvalidated_rrset_as_not_secure() {
        let reply = resolve_mx_via_handler(fauna_mail::outbound::mx::MxAnswer {
            hosts: vec![(10, "mx.attacker.test".into())],
            secure: false,
        })
        .await;
        assert!(
            !reply.secure,
            "an MX RRset that did not DNSSEC-validate must reach the bridge as \
             NOT secure — this bit is the whole of the fix"
        );
        // The hosts still travel: a non-secure answer narrows the TLS posture,
        // it never withholds delivery (RFC 7672 §2.2 says don't treat as
        // DANE-capable, not don't deliver).
        assert_eq!(reply.hosts.len(), 1, "delivery must still have its hosts");
    }

    #[tokio::test]
    async fn resolve_mx_rejects_an_empty_domain() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        let mta = [0x32u8; 32];
        approve_bridge(&state.db, &mta, BridgeRole::Mta).await;
        let req = ResolveMxRequest {
            domain: String::new(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        assert!(
            resolve_mx_handler()(state, mta, payload).await.is_err(),
            "an empty domain is malformed, not a lookup"
        );
    }
}

/// Conformance for `fauna.bridges.mail_health` (`mail-deliverability.md`
/// § The mail health readout): each state reachable by fixture, worst wins, the
/// seven rows in their fixed order, the heartbeats advancing on real
/// delivery/ingest, and the 24 h tick leaving a fresh diagnostics row.
#[cfg(test)]
mod mail_health_tests {
    use super::*;
    use crate::bridge_approval_test_support::approve_bridge;
    use crate::db::bridge_service_users::BridgeRole;
    use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
    use fauna_protocol::encode_canonical;

    const ADMIN: [u8; 32] = [7u8; 32];
    const MTA: [u8; 32] = [0x31; 32];
    const MDA: [u8; 32] = [0x32; 32];
    const DAY: i64 = 86_400;

    async fn read(state: &Arc<AppState>) -> MailHealthReply {
        let payload = Bytes::from(encode_canonical(&MailHealthRequest {}).unwrap().to_vec());
        let bytes = mail_health_handler()(state.clone(), ADMIN, payload)
            .await
            .expect("mail_health ok");
        decode(&bytes).unwrap()
    }

    /// Mail on, both mail bridges approved and connected, a clean self-check
    /// and diagnostics run, and the warm-up ramp long finished. Returns the MTA
    /// bridge's connection id so a test can disconnect it.
    async fn healthy() -> (Arc<AppState>, u64) {
        let state = crate::test_support::fixture_state();
        state.db.add_admin_actor(&ADMIN).await.unwrap();
        state.db.set_mail_enabled(true).await.unwrap();
        approve_bridge(&state.db, &MTA, BridgeRole::Mta).await;
        approve_bridge(&state.db, &MDA, BridgeRole::Mda).await;
        // The connection entries outlive the dropped receivers.
        let (mta_conn, _) = state.ws.subscribe(MTA);
        let _ = state.ws.subscribe(MDA);
        let now = state.outbound_now();
        state
            .db
            .record_blocklist_self_check(
                now,
                r#"[{"server":"zen.spamhaus.org","listed":false,"reason":"","error":""}]"#,
            )
            .await
            .unwrap();
        state
            .db
            .record_diagnostic_run(
                now,
                r#"[{"name":"SPF record valid","status":"pass","detail":""}]"#,
                &ADMIN,
            )
            .await
            .unwrap();
        // First outbound 40 days ago → past the 30-day ramp.
        state
            .db
            .try_consume_warmup(now - 40 * DAY, 1)
            .await
            .unwrap();
        (state, mta_conn.conn_id)
    }

    fn outbound<'a>(recipients: &'a [&'a str]) -> NewOutbound<'a> {
        NewOutbound {
            original_msgid: "<m@fauna.test>",
            original_sender: "a@fauna.test",
            recipients,
            raw_message: b"Subject: x\r\n\r\nbody",
            inbound_verdicts: InboundVerdictsSnapshot {
                spf: "none".into(),
                dmarc: "none".into(),
                dmarc_policy: "none".into(),
            },
            is_forwarded: false,
            forward_actor_id: None,
            forward_rule_id: None,
            forward_copy_mode: None,
            submit_actor_id: None,
        }
    }

    #[tokio::test]
    async fn is_admin_only() {
        let (state, _) = healthy().await;
        let payload = Bytes::from(encode_canonical(&MailHealthRequest {}).unwrap().to_vec());
        assert!(
            mail_health_handler()(state.clone(), [0x55; 32], payload.clone())
                .await
                .is_err(),
            "a non-admin is refused"
        );
        assert!(
            mail_health_handler()(state, MTA, payload).await.is_err(),
            "the MTA bridge is refused — Admin-class only"
        );
    }

    #[tokio::test]
    async fn seven_rows_in_the_fixed_order() {
        let (state, _) = healthy().await;
        let reply = read(&state).await;
        let keys: Vec<&str> = reply.checks.iter().map(|c| c.label_key.as_str()).collect();
        assert_eq!(keys, fauna_mail::health::CHECK_LABEL_KEYS);
    }

    /// Walk the states table from best to worst, adding one condition at a
    /// time: each new condition is worse than every one before it, so it wins.
    #[tokio::test]
    async fn every_state_is_reachable_and_the_worst_wins() {
        let (state, mta_conn) = healthy().await;
        assert_eq!(read(&state).await.state, "delivering");

        // warming_up: an admin reset restarts the ramp at day 1.
        state.db.reset_warmup(state.outbound_now()).await.unwrap();
        assert_eq!(read(&state).await.state, "warming_up");

        // records_failing: a later diagnostics run fails a DKIM record.
        let now = state.outbound_now();
        state
            .db
            .record_diagnostic_run(
                now + 1,
                r#"[{"name":"DKIM record present (sel1)","status":"fail","detail":"no DKIM TXT"}]"#,
                &ADMIN,
            )
            .await
            .unwrap();
        assert_eq!(read(&state).await.state, "records_failing");

        // queue_stalled: a row that failed once and is older than the 4 h
        // delay-warning age.
        let ids = state
            .db
            .enqueue_outbound_at(outbound(&["b@example.org"]), now - 5 * 3_600)
            .await
            .unwrap();
        state
            .db
            .mark_outbound_attempt(ids[0], now + 600, Some("451 try later"), None)
            .await
            .unwrap();
        assert_eq!(read(&state).await.state, "queue_stalled");

        // blocklisted: a later self-check lists the IP; the de-listing URL rides.
        state
            .db
            .record_blocklist_self_check(
                now + 2,
                r#"[{"server":"bl.spamcop.net","listed":true,"reason":"","error":""}]"#,
            )
            .await
            .unwrap();
        let reply = read(&state).await;
        assert_eq!(reply.state, "blocklisted");
        assert_eq!(
            reply.delist_url.as_deref(),
            fauna_mail::health::delist_url_for("bl.spamcop.net")
        );

        // bridge_down: the MTA bridge disconnects.
        state.ws.remove(&MTA, mta_conn);
        assert_eq!(read(&state).await.state, "bridge_down");

        // off: mail disabled trumps everything.
        state.db.set_mail_enabled(false).await.unwrap();
        assert_eq!(read(&state).await.state, "off");
    }

    #[tokio::test]
    async fn a_warmup_deferred_row_is_not_a_stall() {
        let (state, _) = healthy().await;
        let now = state.outbound_now();
        // Deferred by the warm-up cap: created a day ago, next attempt
        // tomorrow, never attempted.
        state
            .db
            .enqueue_outbound_split(outbound(&["b@example.org"]), now - DAY, now + DAY)
            .await
            .unwrap();
        let reply = read(&state).await;
        assert_eq!(reply.state, "delivering");
        assert_eq!(reply.checks[2].state, "pass");
    }

    #[tokio::test]
    async fn the_outbound_heartbeat_advances_on_delivery() {
        let (state, _) = healthy().await;
        let before = read(&state).await;
        assert_eq!(before.last_outbound_delivered_at, None);
        assert_eq!(before.last_inbound_accepted_at, None);

        let ids = state
            .db
            .enqueue_outbound(outbound(&["b@example.org"]))
            .await
            .unwrap();
        let req = MarkOutboundDeliveredRequest { id: ids[0] };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        mark_outbound_delivered_handler()(state.clone(), MTA, payload)
            .await
            .expect("delivered ok");
        let after = read(&state).await;
        assert!(after.last_outbound_delivered_at.is_some());
        assert_eq!(after.last_inbound_accepted_at, None);
    }

    #[tokio::test]
    async fn the_daily_tick_persists_a_diagnostics_run() {
        let (state, _) = healthy().await;
        state
            .db
            .add_mail_domain(
                "fauna.test",
                true,
                "testing",
                "expand_primary",
                None,
                Some("default"),
            )
            .await
            .unwrap();
        let before = state
            .db
            .list_deliverability_diagnostic_runs(100)
            .await
            .unwrap()
            .len();
        run_scheduled_blocklist_self_check(&state).await;
        let runs = state
            .db
            .list_deliverability_diagnostic_runs(100)
            .await
            .unwrap();
        assert_eq!(runs.len(), before + 1, "one fresh diagnostics row per tick");
        assert_eq!(runs[0].2, SCHEDULED_DIAGNOSTICS_RAN_BY, "ran by the nest");
    }
}
