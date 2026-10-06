//! The shared internal-error seam for WS-RPC handlers.
//!
//! Every handler that runs a DB op and maps its failure to a wire [`RpcError`]
//! routes through here instead of copy-pasting a private `fn internal`. The one
//! reason this seam is shared (rather than ~48 identical local helpers) is
//! **classification**: a *query-time* schema/version mismatch — a rusqlite
//! `no such column …` / `no such table …` raised because the running binary's
//! SQL shape does not match its own database — must surface as the typed
//! [`RpcError::nest_schema_mismatch`] (which the shared client classifier routes
//! to [`fauna_protocol::RpcErrorAction::NeedsUpdate`], an actionable update
//! prompt) rather than the generic `fauna.protocol.internal`, which is classified
//! `Transient` and would be **auto-retried forever** (version-compatibility.md
//! § 2.2 / Dimension 4). The boot-time `schema_meta` gate structurally cannot
//! catch this class — it surfaces mid-session, per RPC, not at boot.
//!
//! Everything that is *not* a schema-shape error keeps its exact prior code and
//! its raw-string `details` (the copyable admin-debug string); only genuine
//! mismatches get the new code, and that path carries **no** SQL in `details`
//! (Dim 4: "never a server SQL phrase"). Panics / cancellations / encode
//! failures (e.g. the `dispatch_core` join-error catch-all) are genuinely
//! transient, not schema mismatches, and deliberately do **not** route here.

use bytes::Bytes;
use fauna_protocol::{RpcError, Value, encode_canonical};

/// True when `msg` is a SQLite schema-shape error — a query referenced a column
/// or table the database does not have. These are SQLite's own stable error
/// phrasings, unchanged for the life of the library, so a substring match is
/// robust: `no such column` / `no such table` (SELECT/read side) and
/// `has no column named …` (the INSERT side — an older DB missing a column a
/// newer binary writes, the doc's own motivating shape).
///
/// A hand-written query bug can also raise these, but classifying *that* as
/// `NeedsUpdate` is still strictly safer than the status quo — a non-retry
/// surface beats retrying a broken query forever — so the check never makes any
/// case worse.
pub(crate) fn is_schema_shape_error(msg: &str) -> bool {
    msg.contains("no such column")
        || msg.contains("no such table")
        || msg.contains("has no column named")
}

/// Build an [`RpcError`] carrying `code` + `i18n` with the raw error text in
/// `details`, unless `err` is a schema-shape error — in which case it becomes
/// the typed [`RpcError::nest_schema_mismatch`] (no SQL in `details`).
fn classify(err: impl std::fmt::Display, code: &str, i18n: &str) -> RpcError {
    let s = err.to_string();
    if is_schema_shape_error(&s) {
        return RpcError::nest_schema_mismatch();
    }
    let mut e = RpcError::new(code, i18n);
    e.details = Some(Box::new(Value::String(s)));
    e
}

/// A cross-nest relay reached a peer nest that predates the required federation
/// kind — its allowlist refused the kind with `fauna.protocol.unauthenticated`
/// before any handler ran (S5, `federation.md` § Cross-nest shared folders +
/// channel append → old-peer error shapes). Mapped to the one client-visible
/// code `fauna.federation.peer_nest_outdated` (NeedsUpdate class, localized
/// "the other side's nest needs an update") by every relay — send_remote,
/// changes.list, content_key.get, leave — so the user never sees a spurious
/// auth failure for what is a peer-version gap.
pub(crate) fn peer_nest_outdated(details: impl Into<String>) -> RpcError {
    let mut e = RpcError::new(
        "fauna.federation.peer_nest_outdated",
        "error.federation.peer_nest_outdated",
    );
    e.details = Some(Box::new(Value::String(details.into())));
    e
}

/// Map a peer's typed error from a cross-nest relay: the allowlist-first
/// `fauna.protocol.unauthenticated` (an old peer without the kind) becomes
/// [`peer_nest_outdated`]; every other typed refusal (the commit-gate
/// `permission_denied`, `channel.stale`, the structural `federation.forbidden`)
/// rides through untouched so client handling works unchanged on the relayed
/// path.
pub(crate) fn map_peer_relay_error(peer_err: RpcError, what: &str) -> RpcError {
    if peer_err.code == "fauna.protocol.unauthenticated" {
        peer_nest_outdated(format!("the home nest does not support {what} yet"))
    } else {
        peer_err
    }
}

/// The standard internal-error seam: `fauna.protocol.internal` (Transient) for a
/// genuine internal failure, or `fauna.nest.schema_mismatch` (NeedsUpdate) for a
/// query-time schema mismatch. Replaces the copy-pasted per-module `fn internal`.
pub(crate) fn internal(err: impl std::fmt::Display) -> RpcError {
    classify(err, "fauna.protocol.internal", "error.protocol.internal")
}

/// Namespaced variant for the handlers that emit a per-namespace internal code
/// (`fauna.{ns}.internal`) rather than the shared `fauna.protocol.internal`. The
/// schema-mismatch short-circuit is identical; only the fallback code differs, so
/// each caller keeps its exact prior wire code for non-mismatch failures.
pub(crate) fn internal_ns(ns: &str, err: impl std::fmt::Display) -> RpcError {
    classify(
        err,
        &format!("fauna.{ns}.internal"),
        &format!("error.{ns}.internal"),
    )
}

/// Namespaced variant of [`malformed`] for the handlers that emit a
/// per-namespace malformed-payload code (`fauna.{ns}.malformed`) rather than
/// the shared `fauna.protocol.malformed`.
pub(crate) fn malformed_ns(ns: &str, err: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.malformed"),
        format!("error.{ns}.malformed"),
    );
    e.details = Some(Box::new(Value::String(err.to_string())));
    e
}

/// The standard malformed-payload seam: `fauna.protocol.malformed`, carrying
/// the error text in `details`. Unlike [`internal`], a malformed wire payload
/// is never a DB schema mismatch, so there is no [`classify`] short-circuit.
/// Replaces the copy-pasted per-module `fn malformed`.
pub(crate) fn malformed(err: impl std::fmt::Display) -> RpcError {
    malformed_ns("protocol", err)
}

/// Encode `reply` as the canonical wire bytes, mapping a serialize failure to
/// [`internal`]. Replaces the copy-pasted per-module `fn encode_reply`.
pub(crate) fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    encode_canonical(reply).map_err(|e| internal(format!("encode reply: {e}")))
}

/// The namespaced invalid-params seam: `fauna.{ns}.invalid_params`, carrying
/// the reason text in `details`. Replaces the copy-pasted per-module
/// `fn invalid_params`.
pub(crate) fn invalid_params_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.invalid_params"),
        format!("error.{ns}.invalid_params"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced invalid-request seam: `fauna.{ns}.invalid_request`, carrying
/// the reason text in `details`. Replaces the copy-pasted per-module
/// `fn invalid_request`. A distinct code family from [`invalid_params_ns`] —
/// each module already committed to one or the other on the wire, and this
/// never merges the two codes, only their construction boilerplate.
pub(crate) fn invalid_request_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.invalid_request"),
        format!("error.{ns}.invalid_request"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced not-found seam: `fauna.{ns}.not_found`, carrying the reason
/// text in `details`. Replaces the copy-pasted per-module `fn not_found`.
pub(crate) fn not_found_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.not_found"),
        format!("error.{ns}.not_found"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The CENTRAL permission-denied — `fauna.bridges.permission_denied` — reserved
/// for refusals that are not statements about a kind's family: an unknown or
/// revoked actor (denied on every kind — the every-kind shape the Go bridges'
/// revocation probe keys on, `wsrpc/reconnect.go`) and an unlisted kind.
/// Listed-kind class refusals use [`permission_denied_ns`] with the kind's
/// family instead (`api-layers.md` § Caller-class authorization → *Refusal
/// codes at the gate*).
pub(crate) fn central_permission_denied() -> RpcError {
    RpcError::new(
        "fauna.bridges.permission_denied",
        "error.bridges.permission_denied",
    )
}

/// The namespaced permission-denied seam: `fauna.{ns}.permission_denied`,
/// carrying the reason text in `details`. Replaces the copy-pasted per-module
/// `fn permission_denied`.
pub(crate) fn permission_denied_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.permission_denied"),
        format!("error.{ns}.permission_denied"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// Map a shared-core [`crate::api_error::ApiError`] onto an [`RpcError`],
/// preserving HTTP status semantics: 400 -> invalid_params, 404 -> not_found,
/// else -> internal. Replaces the copy-pasted per-handler `rpc_error_from_api`
/// / `map_api_error` in `feed_handlers`, `posts_handlers` and
/// `federation_handlers`. The 403 arm is caller-supplied rather than fixed to
/// [`permission_denied_ns`]: `federation_handlers` deliberately answers 403
/// with [`forbidden_ns`] instead, a distinct code family (see
/// [`permission_denied_ns`]'s own doc) — this never merges the two, only the
/// status-dispatch boilerplate around them. `posts_handlers` layers its own
/// 501 arm on top before falling back here.
pub(crate) fn rpc_error_from_api_ns(
    ns: &str,
    api: crate::api_error::ApiError,
    forbidden: impl FnOnce(&str, String) -> RpcError,
) -> RpcError {
    use axum::http::StatusCode;
    match api.status {
        StatusCode::BAD_REQUEST => invalid_params_ns(ns, api.message),
        StatusCode::FORBIDDEN => forbidden(ns, api.message),
        StatusCode::NOT_FOUND => not_found_ns(ns, api.message),
        _ => internal(api.message),
    }
}

/// The namespaced rate-limited seam: `fauna.{ns}.rate_limited`, no `details`
/// payload (the refusal itself is the whole message). Replaces the
/// copy-pasted per-module `fn rate_limited`.
pub(crate) fn rate_limited_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.rate_limited"),
        format!("error.{ns}.rate_limited"),
    )
}

/// The standard rate-limited seam: `fauna.protocol.rate_limited`. Mirrors
/// [`malformed`]'s relationship to [`malformed_ns`].
pub(crate) fn rate_limited() -> RpcError {
    rate_limited_ns("protocol")
}

/// The namespaced service-unavailable seam: `fauna.{ns}.unavailable`, carrying
/// the reason text in `details`. Replaces the copy-pasted per-module
/// `fn unavailable`. Some callers hard-code a fixed reason string (their
/// signature stayed zero-arg); this still routes through here with that
/// literal as `reason`.
pub(crate) fn unavailable_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.unavailable"),
        format!("error.{ns}.unavailable"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced conflict seam: `fauna.{ns}.conflict`, carrying the reason
/// text in `details`. Replaces the copy-pasted per-module `fn conflict` for
/// the callers that pass a reason. See [`bare_conflict_ns`] for the two
/// callers whose conflict carries no reason text at all.
pub(crate) fn conflict_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.conflict"),
        format!("error.{ns}.conflict"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced conflict seam with no `details` payload — for the two
/// call sites (`config`, `mls`) whose CAS/precondition refusal carries no
/// extra reason text. Mirrors [`rate_limited_ns`]'s relationship to
/// [`permission_denied_ns`].
pub(crate) fn bare_conflict_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.conflict"),
        format!("error.{ns}.conflict"),
    )
}

/// The namespaced forbidden seam: `fauna.{ns}.forbidden`, carrying the reason
/// text in `details`. Replaces the copy-pasted per-module `fn forbidden`.
pub(crate) fn forbidden_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.forbidden"),
        format!("error.{ns}.forbidden"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The standard unauthenticated seam: `fauna.protocol.unauthenticated`, no
/// `details` payload. Both known callers (`sidecar_channel`,
/// `federation_channel`) are the identical zero-arg protocol-namespaced
/// shape, so unlike [`rate_limited`] there is no namespaced twin yet.
pub(crate) fn unauthenticated() -> RpcError {
    RpcError::new(
        "fauna.protocol.unauthenticated",
        "error.protocol.unauthenticated",
    )
}

/// The namespaced tier-not-found seam: `fauna.{ns}.tier_not_found`, carrying
/// the reason text in `details`. Replaces the copy-pasted per-module
/// `fn tier_not_found`.
pub(crate) fn tier_not_found_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.tier_not_found"),
        format!("error.{ns}.tier_not_found"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced open-code seam: `fauna.{ns}.{code}`, carrying the detail text
/// in `details`. Unlike the other `_ns` helpers, `code` is caller-supplied
/// rather than fixed — this is what each of `share_handlers.rs`,
/// `sync_handlers.rs`, `media_handlers.rs` and `folder_handlers.rs` used to
/// re-derive as its own private `fn coded`. Replaces those copy-pasted bodies.
pub(crate) fn coded_ns(ns: &str, code: &str, detail: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(format!("fauna.{ns}.{code}"), format!("error.{ns}.{code}"));
    e.details = Some(Box::new(Value::String(detail.to_string())));
    e
}

/// The namespaced pure-backup-destination seam: `fauna.{ns}.pure_backup_destination`,
/// carrying the detail text in `details`. Replaces the copy-pasted per-module
/// `fn pure_backup_destination` in `bridge_routing_handlers.rs`,
/// `conversations_handlers.rs`, `filesync_handlers.rs` and
/// `segments/compact_handler.rs` — each namespace's refusal text differs (what
/// specifically needs the local plaintext-framed segments a pure-backup
/// destination doesn't have), so `detail` stays caller-supplied.
pub(crate) fn pure_backup_destination_ns(ns: &str, detail: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.pure_backup_destination"),
        format!("error.{ns}.pure_backup_destination"),
    );
    e.details = Some(Box::new(Value::String(detail.to_string())));
    e
}

/// The namespaced guardian-approval-required seam: `fauna.{ns}.guardian_approval_required`,
/// carrying the detail text in `details`. Replaces the copy-pasted per-module
/// inline `let mut e = RpcError::new(...); e.details = ...` construction.
pub(crate) fn guardian_approval_required_ns(ns: &str, detail: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.guardian_approval_required"),
        format!("error.{ns}.guardian_approval_required"),
    );
    e.details = Some(Box::new(Value::String(detail.to_string())));
    e
}

/// The namespaced forbidden seam with no `details` payload — for the
/// mode-commit `NotAdmin` arm, whose forbidden refusal carries no reason
/// text. Mirrors [`bare_conflict_ns`]'s relationship to [`conflict_ns`].
/// Deliberately distinct from [`forbidden_ns`]: fabricating a `reason` here
/// would add new wire-visible `details` text on a code the module's own tests
/// pin as wire-stable.
pub(crate) fn bare_forbidden_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.forbidden"),
        format!("error.{ns}.forbidden"),
    )
}

/// The namespaced signature-failed seam: `fauna.{ns}.signature_failed`,
/// carrying the reason text in `details`. Replaces the copy-pasted
/// `nat_mode_handlers.rs` construction.
pub(crate) fn signature_failed_ns(ns: &str, reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{ns}.signature_failed"),
        format!("error.{ns}.signature_failed"),
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// The namespaced signature-failed seam with no `details` payload — for
/// `recovery_handlers.rs`'s zero-arg local helper. Mirrors [`bare_conflict_ns`]'s
/// relationship to [`conflict_ns`].
pub(crate) fn bare_signature_failed_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.signature_failed"),
        format!("error.{ns}.signature_failed"),
    )
}

/// The namespaced not-claimed seam: `fauna.{ns}.not_claimed`, no `details`
/// payload. Replaces the copy-pasted `nat_mode_handlers.rs` construction.
pub(crate) fn not_claimed_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.not_claimed"),
        format!("error.{ns}.not_claimed"),
    )
}

/// The namespaced no-relays-configured seam: `fauna.{ns}.no_relays_configured`,
/// no `details` payload — one seam for every handler that refuses a publish
/// to zero relays (`nostr/content_handlers.rs`, and the bridged family's
/// `send` on a Nostr-leg room).
///
/// `allow(dead_code)`: both call sites live behind the `nostr` feature, which
/// is not in `fauna-nest`'s default set, so a default-featured build never
/// calls this — while this seam itself stays unconditional, since
/// `rpc_errors.rs` has no business knowing which downstream feature happens
/// to be its only consumer today (mirrors `tip_handlers.rs::TipRow`'s
/// rationale).
#[allow(dead_code)]
pub(crate) fn no_relays_configured_ns(ns: &str) -> RpcError {
    RpcError::new(
        format!("fauna.{ns}.no_relays_configured"),
        format!("error.{ns}.no_relays_configured"),
    )
}

/// The mail/CalDAV/CardDAV placement-journal seam: `fauna.bridges.placement_journal_diverged`,
/// carrying the reason text in `details`. For the one recurring shape across
/// `bridge_imap_handlers.rs`'s STORE/EXPUNGE/COPY/MOVE/APPEND/CREATE/DELETE/
/// RENAME/SUBSCRIBE/UNSUBSCRIBE handlers: the SQLite mutation has already
/// committed, and this specific call appends the post-commit record to the
/// placement journal (the manifest's compacted current state). A failure
/// here is **not** "the mutation didn't happen" — it's a narrow crash window
/// Plan 2 T9's divergence detection repairs on the next SELECT/QRESYNC — so
/// it gets its own code rather than sharing the generic `internal()`, which
/// would be indistinguishable from a failure where nothing committed at all.
/// Classified `Transient` in [`fauna_protocol::RpcError::action`] (same
/// retry-safety as the generic internal error it replaces at these sites).
pub(crate) fn placement_journal_diverged(reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.placement_journal_diverged",
        "error.bridges.placement_journal_diverged",
    );
    e.details = Some(Box::new(Value::String(reason.to_string())));
    e
}

/// Convert a wire byte-slice field to a fixed 32-byte array, or a plain error
/// message naming which field failed. Replaces the copy-pasted per-handler
/// `<bytes>.try_into().map_err(|_| <ctor>(format!("{field} must be 32
/// bytes")))` idiom. Deliberately returns the bare conversion, never a
/// pre-baked [`RpcError`]: call sites wrap this with whichever error
/// constructor their wire contract already commits to (`malformed`,
/// `invalid_params`, `invalid_request`, and their namespaced variants all
/// appear at different call sites), so this only unifies the conversion, not
/// which error code it becomes.
pub(crate) fn require_bytes32(field: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| format!("{field} must be 32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::RpcErrorAction;

    #[test]
    fn schema_shape_errors_are_detected() {
        assert!(is_schema_shape_error("no such column: user_pref_v2"));
        assert!(is_schema_shape_error("no such table: outbound_mail_queue"));
        // rusqlite's SqliteFailure Display embeds the phrase verbatim.
        assert!(is_schema_shape_error(
            "SqliteFailure(Error { code: Unknown, extended_code: 1 }, Some(\"no such column: x\"))"
        ));
        // INSERT side: an older DB missing a column a newer binary writes.
        assert!(is_schema_shape_error(
            "table outbound_mail_queue has no column named is_forwarded"
        ));
        assert!(!is_schema_shape_error("database is locked"));
        assert!(!is_schema_shape_error("UNIQUE constraint failed"));
        assert!(!is_schema_shape_error("connection reset"));
    }

    #[test]
    fn internal_routes_schema_mismatch_to_needs_update_with_no_sql() {
        // The Dim 4 fix: a query-time schema mismatch must NOT reach the client as
        // the auto-retried Transient `fauna.protocol.internal`.
        let e = internal("no such column: user_pref_v2");
        assert_eq!(e.code, RpcError::CODE_NEST_SCHEMA_MISMATCH);
        assert_eq!(e.action(), RpcErrorAction::NeedsUpdate);
        assert_ne!(e.action(), RpcErrorAction::Transient);
        // The column name never rides to the client (no server SQL phrase).
        assert_eq!(e.details, None);
    }

    #[test]
    fn internal_preserves_generic_failures() {
        // A non-schema internal failure keeps its exact prior code + raw details.
        let e = internal("database is locked");
        assert_eq!(e.code, "fauna.protocol.internal");
        assert_eq!(e.action(), RpcErrorAction::Transient);
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("database is locked".into()))
        );
    }

    #[test]
    fn internal_ns_preserves_namespaced_code_but_still_catches_mismatch() {
        // Non-mismatch → the caller's namespaced code, unchanged wire behaviour.
        let e = internal_ns("account", "row not found");
        assert_eq!(e.code, "fauna.account.internal");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("row not found".into()))
        );
        // A schema mismatch through a namespaced helper still routes to NeedsUpdate.
        let e = internal_ns("account", "no such table: account_aliases");
        assert_eq!(e.code, RpcError::CODE_NEST_SCHEMA_MISMATCH);
        assert_eq!(e.action(), RpcErrorAction::NeedsUpdate);
        assert_eq!(e.details, None);
    }

    #[test]
    fn malformed_carries_the_generic_protocol_code_and_details() {
        let e = malformed("bad payload");
        assert_eq!(e.code, "fauna.protocol.malformed");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("bad payload".into()))
        );
    }

    #[test]
    fn malformed_ns_uses_the_namespaced_code() {
        let e = malformed_ns("payments", "bad payload");
        assert_eq!(e.code, "fauna.payments.malformed");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("bad payload".into()))
        );
    }

    #[test]
    fn encode_reply_round_trips_a_serializable_value() {
        let bytes = encode_reply(&42i64).expect("encode succeeds");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn invalid_params_ns_uses_the_namespaced_code() {
        let e = invalid_params_ns("family", "bad payload");
        assert_eq!(e.code, "fauna.family.invalid_params");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("bad payload".into()))
        );
    }

    #[test]
    fn invalid_request_ns_uses_the_namespaced_code() {
        let e = invalid_request_ns("web", "bad payload");
        assert_eq!(e.code, "fauna.web.invalid_request");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("bad payload".into()))
        );
    }

    #[test]
    fn not_found_ns_uses_the_namespaced_code() {
        let e = not_found_ns("feed", "post not found");
        assert_eq!(e.code, "fauna.feed.not_found");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("post not found".into()))
        );
    }

    #[test]
    fn permission_denied_ns_uses_the_namespaced_code() {
        let e = permission_denied_ns("posts", "not the author");
        assert_eq!(e.code, "fauna.posts.permission_denied");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("not the author".into()))
        );
    }

    #[test]
    fn rate_limited_ns_uses_the_namespaced_code_with_no_details() {
        let e = rate_limited_ns("moderation");
        assert_eq!(e.code, "fauna.moderation.rate_limited");
        assert_eq!(e.details, None);
    }

    #[test]
    fn rate_limited_uses_the_generic_protocol_code() {
        let e = rate_limited();
        assert_eq!(e.code, "fauna.protocol.rate_limited");
        assert_eq!(e.details, None);
    }

    #[test]
    fn unavailable_ns_uses_the_namespaced_code() {
        let e = unavailable_ns("push", "push service not configured");
        assert_eq!(e.code, "fauna.push.unavailable");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("push service not configured".into()))
        );
    }

    #[test]
    fn conflict_ns_uses_the_namespaced_code() {
        let e = conflict_ns("storage", "backup already running");
        assert_eq!(e.code, "fauna.storage.conflict");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("backup already running".into()))
        );
    }

    #[test]
    fn bare_conflict_ns_carries_no_details() {
        let e = bare_conflict_ns("mls");
        assert_eq!(e.code, "fauna.mls.conflict");
        assert_eq!(e.details, None);
    }

    #[test]
    fn forbidden_ns_uses_the_namespaced_code() {
        let e = forbidden_ns("inbox", "sender not approved");
        assert_eq!(e.code, "fauna.inbox.forbidden");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("sender not approved".into()))
        );
    }

    #[test]
    fn unauthenticated_uses_the_generic_protocol_code_with_no_details() {
        let e = unauthenticated();
        assert_eq!(e.code, "fauna.protocol.unauthenticated");
        assert_eq!(e.details, None);
    }

    #[test]
    fn tier_not_found_ns_uses_the_namespaced_code() {
        let e = tier_not_found_ns("payments", "tier expired");
        assert_eq!(e.code, "fauna.payments.tier_not_found");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("tier expired".into()))
        );
    }

    #[test]
    fn coded_ns_uses_the_caller_supplied_code() {
        let e = coded_ns("share", "not_found", "no such token");
        assert_eq!(e.code, "fauna.share.not_found");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("no such token".into()))
        );
    }

    #[test]
    fn guardian_approval_required_ns_uses_the_namespaced_code() {
        let e = guardian_approval_required_ns("account", "ask your guardian");
        assert_eq!(e.code, "fauna.account.guardian_approval_required");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("ask your guardian".into()))
        );
    }

    #[test]
    fn bare_forbidden_ns_carries_no_details() {
        let e = bare_forbidden_ns("setup");
        assert_eq!(e.code, "fauna.setup.forbidden");
        assert_eq!(e.details, None);
    }

    #[test]
    fn signature_failed_ns_uses_the_namespaced_code() {
        let e = signature_failed_ns("setup", "bad signature");
        assert_eq!(e.code, "fauna.setup.signature_failed");
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("bad signature".into()))
        );
    }

    #[test]
    fn bare_signature_failed_ns_carries_no_details() {
        let e = bare_signature_failed_ns("recovery");
        assert_eq!(e.code, "fauna.recovery.signature_failed");
        assert_eq!(e.details, None);
    }

    #[test]
    fn not_claimed_ns_carries_no_details() {
        let e = not_claimed_ns("setup");
        assert_eq!(e.code, "fauna.setup.not_claimed");
        assert_eq!(e.details, None);
    }

    #[test]
    fn no_relays_configured_ns_carries_no_details() {
        let e = no_relays_configured_ns("nostr");
        assert_eq!(e.code, "fauna.nostr.no_relays_configured");
        assert_eq!(e.details, None);
    }

    #[test]
    fn placement_journal_diverged_carries_its_own_code_and_is_transient() {
        // Distinct from the generic `internal()` code: this is specifically
        // the post-commit placement-journal-append failure — the underlying
        // SQL mutation already committed, so it's safe to auto-retry (Plan 2
        // T9 repairs the divergence on next SELECT/QRESYNC), same as the
        // generic internal error it used to share a code with.
        let e = placement_journal_diverged("disk full");
        assert_eq!(e.code, "fauna.bridges.placement_journal_diverged");
        assert_eq!(e.action(), RpcErrorAction::Transient);
        assert_eq!(
            e.details.as_deref(),
            Some(&Value::String("disk full".into()))
        );
    }

    #[test]
    fn require_bytes32_converts_an_exact_length_slice() {
        let bytes = [7u8; 32];
        assert_eq!(require_bytes32("actor_id", &bytes), Ok(bytes));
    }

    #[test]
    fn require_bytes32_names_the_field_on_wrong_length() {
        let bytes = [7u8; 31];
        assert_eq!(
            require_bytes32("actor_id", &bytes),
            Err("actor_id must be 32 bytes".to_string())
        );
    }
}
