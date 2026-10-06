//! WS-RPC handlers for the community-labeler registry (`fauna.labelers.*`).
//!
//! Labeler-registry design (tracked internally),
//! §§ 1–5. Five handlers, all gated to class `User`
//! (`bridge_method_allowlist::is_permitted`):
//!
//! - `publish` — decode + signature-verify the `AlgorithmLabeler`, size/hash
//!   check, per-kind artifact validation (a `wasm` module must compile under
//!   [`LabelerRuntime`]; a `list` artifact must be canonical —
//!   `validate_list_artifact`), store it iff the version strictly increases
//!   (monotonic).
//! - `list` — browse the catalog (metadata only, no bytes).
//! - `inspect` — fetch one labeler's full record + artifact bytes.
//! - `subscribe` (owner == caller) — record the subscription; for a `wasm`
//!   labeler **register its version in `model_versions`** so the existing
//!   re-score obligation scan owes a re-score (the load-bearing link, design
//!   § 5 step 3); for a `list` labeler **materialize its `content_scores` rows
//!   nest-side** instead (design Block A, D12 — no holder, no drain).
//! - `unsubscribe` (owner == caller) — drop the subscription (idempotent); a
//!   List's last unsubscribe withdraws its materialized rows.
//!
//! The nest never executes a labeler here — a `wasm` labeler runs in a
//! capability-holder content-processor (design § 6, Slice 3); a `list` labeler
//! is never executed at all (pure membership lookup, materialized nest-side).

use std::time::Duration;

use serde_bytes::ByteBuf;

use fauna_core::scoring::{
    AlgorithmLabeler, LabelerVerifyError, artifact_kind, labeler_factor, validate_list_artifact,
    validate_text_model_artifact, verify_labeler_metadata,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    labelers::{
        InspectLabelerReply, InspectLabelerRequest, LabelerSummary, ListLabelersReply,
        ListLabelersRequest, PublishLabelerReply, PublishLabelerRequest, SubscribeLabelerReply,
        SubscribeLabelerRequest, UnsubscribeLabelerReply, UnsubscribeLabelerRequest,
    },
};

use crate::db::labelers::{
    LabelerByteQuotaExceeded, LabelerCallerQuotaExceeded, LabelerQuotaExceeded,
    MAX_LABELER_WASM_BYTES, PutLabeler, StaleLabelerVersion, TEXT_MODEL_ARTIFACT_MAX_BYTES,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use fauna_labeler::LabelerRuntime;

/// The content kinds a v1 labeler may declare (Slice-3 D7). `mail` is the
/// holder-served vehicle — only mail is content-at-rest today, so a `mail`-kind
/// labeler's re-score obligation rides the existing MDA drain
/// (`rescore_drain.go`). `post` is the canonical default. The declared kind is
/// **not** part of the signed `AlgorithmLabeler` metadata (design § 3 keeps that
/// frozen); it rides the `PublishLabelerRequest` envelope and is validated here
/// before it is projected into `labelers.content_kind`, so `list`/`inspect`
/// subscribers mint a capability for the right kind.
const ALLOWED_LABELER_CONTENT_KINDS: &[&str] = &["post", "mail"];

/// Resolve a publisher's declared `content_kind`: the value must be in
/// [`ALLOWED_LABELER_CONTENT_KINDS`], else the publish is `malformed` — an empty
/// declaration included (every publisher names its kind; there is no default).
fn resolve_content_kind(declared: &str) -> Result<String, RpcError> {
    if !ALLOWED_LABELER_CONTENT_KINDS.contains(&declared) {
        return Err(malformed(format!(
            "unsupported labeler content_kind {declared:?} (allowed: {ALLOWED_LABELER_CONTENT_KINDS:?})"
        )));
    }
    Ok(declared.to_string())
}

/// Resolve a publisher's declared `artifact_kind` (design Block A, D8): the
/// value must be `wasm`, `list`, or `text-model`, else the publish is
/// `malformed` — an empty declaration included (every publisher names its
/// kind; there is no default). Same envelope-field pattern as
/// [`resolve_content_kind`] — never part of the signed `AlgorithmLabeler`.
fn resolve_artifact_kind(kind: &str) -> Result<String, RpcError> {
    if kind != artifact_kind::WASM
        && kind != artifact_kind::LIST
        && kind != artifact_kind::TEXT_MODEL
    {
        return Err(malformed(format!(
            "unsupported labeler artifact_kind {kind:?} (allowed: wasm | list | text-model)"
        )));
    }
    Ok(kind.to_string())
}

// ── Error helpers (mirroring bridge_blob_handlers) ──────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("labelers", reason)
}

use crate::rpc_errors::internal;

fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("labelers", reason)
}

// Resolve the caller's class and require the kind be permitted for it — the
// same central-gate check the bridge handlers use (the `every_registered_kind_is_gated`
// tripwire depends on the `is_permitted` arm existing).
use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── Publish validation (pub so the registry test exercises the real path) ──

/// A validated publish, ready to store: the decoded metadata plus the projected
/// catalog columns.
pub struct ValidatedLabeler {
    pub metadata: AlgorithmLabeler,
    /// `labeler_factor(algorithm_id)` — `"labeler:<hex>"` (design § 4).
    pub factor: String,
    /// The `content.read{kind}` this labeler scores.
    pub content_kind: String,
    /// `wasm` | `list` (design Block A, D8).
    pub artifact_kind: String,
    /// A `text-model` artifact's tokenizer/schema `version`, read off the
    /// artifact this function already decodes to validate. `0` for every other
    /// kind — the browse surface reads `0` as "no version claim".
    pub artifact_version: u64,
    /// For a `list` artifact: the decoded, validated `(content_id, score)`
    /// entries (the `labeler_list_entries` projection `put_labeler` stores).
    /// Empty for `wasm`.
    pub list_entries: Vec<(Vec<u8>, i64)>,
}

/// Validate a publish (design § 5 "publish"): decode the canonical-CBOR
/// `AlgorithmLabeler`, verify its Ed25519 signature against `algorithm_id`,
/// check `wasm_size`/`wasm_hash` against the module bytes, enforce the size cap,
/// and validate the module compiles under [`LabelerRuntime`]. Returns the
/// projected catalog columns; the caller persists via `put_labeler` (which
/// enforces the monotonic version + per-publisher quota). Does **not** touch the
/// DB — pure validation, so a test can drive the real path without a router.
///
/// **Signature-key binding (judgment call — design § 6 leaves the exact
/// `ActorId`→signing-key resolution open):** `AlgorithmLabeler.algorithm_id` is
/// an [`fauna_core::identity::ActorId`], i.e. the Ed25519 public key itself
/// (`identity.rs`: "The public key serves as the Actor's identifier"). So the
/// artifact is verified against `algorithm_id` as the verifying key, over the
/// canonical metadata with `signature` zeroed — the `GrantEvent::verify`
/// convention (`grant_event.rs`). `publisher_actor` is therefore the signer
/// (== `algorithm_id`), matching the § 3 schema ("the actor that published (==
/// signer)").
pub fn validate_labeler_publish(
    metadata_blob: &[u8],
    wasm_bytes: &[u8],
    content_kind: &str,
    artifact_kind_declared: &str,
) -> Result<ValidatedLabeler, RpcError> {
    // Resolve + validate the declared kinds before any expensive work, so a bad
    // declaration is rejected as cheaply as an oversize module.
    let content_kind = resolve_content_kind(content_kind)?;
    let resolved_artifact_kind = resolve_artifact_kind(artifact_kind_declared)?;
    // A List is public-content-only in v1 (design Block A, D10): mail is
    // inherently per-recipient, never public, so a `mail` List is meaningless
    // and rejected. (The public-only guarantee itself is enforced at
    // materialization — only ids present in `content_meta` ever get a row.)
    if resolved_artifact_kind == artifact_kind::LIST && content_kind != "post" {
        return Err(malformed(format!(
            "a list labeler is public-post-only (content_kind {content_kind:?} not allowed)"
        )));
    }
    // v1 of the text-model kind is public-post-only for the same reason a List
    // is (`topic-factors.md` § Publishing: "`content_kind: "post"` only at v1 —
    // `mail` rejected at publish, the List's rule").
    if resolved_artifact_kind == artifact_kind::TEXT_MODEL && content_kind != "post" {
        return Err(malformed(format!(
            "a text-model labeler is public-post-only (content_kind {content_kind:?} not allowed)"
        )));
    }
    if wasm_bytes.len() > MAX_LABELER_WASM_BYTES {
        return Err(malformed(format!(
            "labeler wasm too large: {} bytes (max {MAX_LABELER_WASM_BYTES})",
            wasm_bytes.len()
        )));
    }
    // The text-model kind carries its own, much tighter byte cap: a bounded
    // vocabulary, not an arbitrary program.
    if resolved_artifact_kind == artifact_kind::TEXT_MODEL
        && wasm_bytes.len() > TEXT_MODEL_ARTIFACT_MAX_BYTES
    {
        return Err(malformed(format!(
            "text-model artifact too large: {} bytes (max {TEXT_MODEL_ARTIFACT_MAX_BYTES})",
            wasm_bytes.len()
        )));
    }
    let metadata: AlgorithmLabeler = fauna_core::encoding::canonical_decode(metadata_blob)
        .map_err(|e| malformed(format!("labeler metadata decode: {e}")))?;

    // Metadata↔artifact binding (wasm_size + wasm_hash) + Ed25519 signature over
    // `algorithm_id`: the shared pre-trust verify used at BOTH boundaries — this
    // publish gate AND the FFI holder's per-instantiation re-check (security
    // review B1). One copy in `fauna_core::scoring`, so the two can't drift.
    // For a List the binding covers the artifact bytes identically (the
    // `wasm_*` names read "artifact").
    verify_labeler_metadata(&metadata, wasm_bytes).map_err(map_labeler_verify_err)?;
    // `metadata.output_schema.label_abi` is stored unexamined, as the
    // text-model `version` below is: the revision is a contract between the
    // module and whatever runs it (`fauna_labeler::run_published_labeler_bare`
    // refuses one it cannot read), and refusing it here would make an older
    // nest refuse a newer client's artifact.

    // Publish-time artifact validation gate, per kind: a WASM module must
    // compile under the sandbox; a List must decode to canonical form
    // (sorted/deduped 32-byte ids, per-mille scores in [0,1000], entry cap) —
    // design Block A, D9/D10. Reject a bad artifact before it reaches a
    // subscriber either way.
    // Set by the text-model arm below off the artifact it decodes; 0 for every
    // other kind, which a browse surface reads as "no version claim".
    let mut artifact_version: u64 = 0;
    let list_entries = if resolved_artifact_kind == artifact_kind::LIST {
        let artifact = validate_list_artifact(wasm_bytes)
            .map_err(|e| malformed(format!("labeler list artifact invalid: {e}")))?;
        artifact
            .entries
            .into_iter()
            .map(|e| (e.content_id.into_vec(), e.score))
            .collect()
    } else if resolved_artifact_kind == artifact_kind::TEXT_MODEL {
        // Pure data, so no sandbox to instantiate — but the canonical form,
        // the vocabulary cap, the per-class count sanity, the
        // distinct-document **prune floor**, and (at a version this build's
        // tokenizer contract covers) each n-gram's **1-3-token shape** are
        // all enforced here, at the one boundary that can refuse a buggy or
        // hostile publisher's artifact
        // (`fauna_core::scoring::validate_text_model_artifact`). Note it
        // deliberately accepts an unrecognized `version` — shape included:
        // the version is the subscriber's tokenizer contract, not this
        // nest's, so rejecting one would make an older nest refuse a newer
        // client's artifact.
        // The decoded artifact's `version` is kept, not discarded: it is the
        // one number the metadata-only browse needs and cannot get any other
        // way (it lives inside these bytes, which only `inspect` returns), and
        // this gate already paid for the decode.
        let artifact = validate_text_model_artifact(wasm_bytes)
            .map_err(|e| malformed(format!("labeler text-model artifact invalid: {e}")))?;
        artifact_version = artifact.version as u64;
        // No id→score projection: a text-model labeler is evaluated at the
        // subscriber's client, so the nest materializes nothing for it.
        Vec::new()
    } else {
        let rt = LabelerRuntime::new(
            metadata.resource_limits.max_memory_bytes,
            metadata.resource_limits.max_cpu_microseconds,
        )
        .map_err(internal)?;
        rt.load_module(wasm_bytes)
            .map_err(|e| malformed(format!("labeler wasm does not compile: {e}")))?;
        Vec::new()
    };

    let factor = labeler_factor(&metadata.algorithm_id);
    Ok(ValidatedLabeler {
        metadata,
        factor,
        content_kind,
        artifact_kind: resolved_artifact_kind,
        artifact_version,
        list_entries,
    })
}

/// The publish core (design § 5 "publish"): [`validate_labeler_publish`] then
/// persist via `put_labeler` (which enforces the monotonic version + the two
/// count caps + the byte budget). Returns `(labeler_id, version)`. Depends only
/// on the DB so the registry test drives the real store path directly.
///
/// Two identities flow here: `publisher_actor == algorithm_id` (the signer;
/// design § 3, the self-signed rotatable keypair), and `caller_actor` — the
/// authenticated router `actor_id` (the un-rotatable enrolled identity). The
/// per-caller quota keys on the latter so a `publish` loop with a fresh signing
/// keypair each time cannot evade the storage bound.
pub async fn publish_labeler_core(
    db: &crate::db::CacheDb,
    caller_actor: &[u8; 32],
    metadata_blob: &[u8],
    wasm_bytes: &[u8],
    content_kind: &str,
    artifact_kind_declared: &str,
) -> Result<([u8; 32], u64), RpcError> {
    let validated = validate_labeler_publish(
        metadata_blob,
        wasm_bytes,
        content_kind,
        artifact_kind_declared,
    )?;
    let labeler_id = validated.metadata.algorithm_id.0;
    let version = validated.metadata.version;
    db.put_labeler(PutLabeler {
        labeler_id: &labeler_id,
        version,
        publisher_actor: &labeler_id,
        caller_actor,
        content_kind: &validated.content_kind,
        factor: &validated.factor,
        wasm_hash: validated.metadata.wasm_hash.as_bytes(),
        wasm_size: validated.metadata.wasm_size,
        metadata_blob,
        wasm_bytes,
        artifact_kind: &validated.artifact_kind,
        artifact_version: validated.artifact_version,
        list_entries: &validated.list_entries,
    })
    .await
    // A stale-version / quota / byte-budget rejection is client-actionable, not a
    // server bug to retry — map to `malformed`, not `internal`.
    .map_err(|e| {
        if e.downcast_ref::<StaleLabelerVersion>().is_some()
            || e.downcast_ref::<LabelerQuotaExceeded>().is_some()
            || e.downcast_ref::<LabelerCallerQuotaExceeded>().is_some()
            || e.downcast_ref::<LabelerByteQuotaExceeded>().is_some()
        {
            malformed(e)
        } else {
            internal(e)
        }
    })?;
    Ok((labeler_id, version))
}

/// Map the shared [`LabelerVerifyError`] to the nest's `RpcError` surface: a
/// failed signature is a permission error (a bad artifact from the caller), a
/// re-encode failure is internal, everything else is malformed input. Preserves
/// the malformed-vs-permission-denied distinction the inline verify had, now
/// that the crypto core lives in `fauna_core::scoring::verify_labeler_metadata`
/// (the `GrantEvent::verify` convention; see [`validate_labeler_publish`] for
/// the signing-key binding rationale).
fn map_labeler_verify_err(e: LabelerVerifyError) -> RpcError {
    match e {
        LabelerVerifyError::SignatureInvalid => {
            permission_denied("labeler signature verification failed")
        }
        LabelerVerifyError::Encode(m) => internal(format!("labeler metadata re-encode: {m}")),
        other => malformed(other.to_string()),
    }
}

/// The subscribe core (design § 5 "subscribe"): look up the labeler, record the
/// subscription, and **register its version in `model_versions`** so the
/// re-score obligation scan owes a re-score. Returns the registered factor.
/// Depends only on the DB (not the whole `AppState`) so the registry test drives
/// the real load-bearing link (put_subscription + upsert_model_version) directly.
pub async fn subscribe_labeler_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    labeler_id: &[u8],
    grant_id: Option<&[u8]>,
) -> Result<String, RpcError> {
    let rec = db
        .get_labeler(labeler_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("no such labeler"))?;

    // A List labeler (design Block A, D12a) has no holder and no drain — the
    // nest itself materializes its bus rows, the report:spam shape. It is
    // public-only, so a grant is meaningless: fail loud rather than silently
    // record one (the client minted something nothing will ever fetch).
    if rec.artifact_kind == artifact_kind::LIST {
        if grant_id.is_some() {
            return Err(malformed(
                "a list labeler is public-only; subscribe carries no grant_id",
            ));
        }
        db.put_subscription(owner, labeler_id, None, rec.version)
            .await
            .map_err(internal)?;
        // Deliberately NO `upsert_model_version` and NO `seed_factor_backlog`:
        // keeping List factors out of `model_versions` is what keeps the drain
        // from ever surfacing List work (the report:spam
        // outside-`builtin_factor_versions` precedent). Materialization writes
        // real scores immediately instead.
        db.materialize_list_labeler_scores(labeler_id)
            .await
            .map_err(internal)?;
        return Ok(rec.factor);
    }

    // A `text-model` labeler (frame § Tier-3 artifact kinds) is evaluated at the
    // **subscriber's client, never the nest** — the placement matrix forbids
    // nest-side content evaluation even over public posts, and the client is the
    // one position where post plaintext meets the artifact. So subscribe records
    // the `labeler_subscriptions` row and NOTHING else: no
    // `content_scores` (nothing to materialize — the client fetches the raw
    // artifact via inspect at feed (re)load and scores locally), no
    // `upsert_model_version` and no `seed_factor_backlog` (no holder, no drain,
    // so a registration would owe drain work that can never be done), and no
    // grant (the client scores only content it already reads — a grant would be
    // capability nothing will ever fetch).
    if rec.artifact_kind == artifact_kind::TEXT_MODEL {
        if grant_id.is_some() {
            return Err(malformed(
                "a text-model labeler is client-evaluated; subscribe carries no grant_id",
            ));
        }
        db.put_subscription(owner, labeler_id, None, rec.version)
            .await
            .map_err(internal)?;
        return Ok(rec.factor);
    }

    db.put_subscription(owner, labeler_id, grant_id, rec.version)
        .await
        .map_err(internal)?;
    // The "registers its version so the drain re-scores" step: bump the registry
    // watermark for this labeler's factor. `version` is u64 on the wire; the
    // registry watermark is u32 (matches ScoreEntry.scorer_version) — clamp
    // defensively.
    let registered = u32::try_from(rec.version).unwrap_or(u32::MAX);
    db.upsert_model_version(&rec.factor, registered)
        .await
        .map_err(internal)?;
    // ⚠ The new-factor backlog seed — without this, registration alone owes ZERO
    // drain work: the obligation scan only surfaces rows that already carry the
    // factor, and a fresh `labeler:<id>` has none (design revision 2026-07-07,
    // point 3). Seed a "behind version 0" placeholder per owner item of the
    // labeler's kind so the drain actually has work to do. Idempotent + owner-
    // scoped; a no-op for kinds without a content-at-rest universe (e.g. `post`).
    db.seed_factor_backlog(&rec.factor, &rec.content_kind, owner)
        .await
        .map_err(internal)?;
    Ok(rec.factor)
}

/// The unsubscribe core (design Block A, D12c for the List branch): drop the
/// subscription (idempotent), and — when the labeler is a List whose **last**
/// subscriber just left — withdraw its materialized `content_scores` rows (the
/// report:spam withdraw precedent; the rows are derived from the stored
/// artifact, so a re-subscribe recreates them). A `wasm` labeler's rows are
/// left as-is (the proven revoke→dark path: the drain simply goes dark).
pub async fn unsubscribe_labeler_core(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    labeler_id: &[u8],
) -> Result<(), RpcError> {
    // Look up BEFORE deleting so an unknown labeler stays an idempotent no-op
    // (matching the pre-List behavior: unsubscribe never 404s).
    let rec = db.get_labeler(labeler_id).await.map_err(internal)?;
    let _ = db
        .delete_subscription(owner, labeler_id)
        .await
        .map_err(internal)?;
    if let Some(rec) = rec
        && rec.artifact_kind == artifact_kind::LIST
        && db
            .count_subscriptions_for_labeler(labeler_id)
            .await
            .map_err(internal)?
            == 0
    {
        db.withdraw_labeler_factor_scores(&rec.factor)
            .await
            .map_err(internal)?;
    }
    Ok(())
}

// ── Handlers ─────────────────────────────────────────────────────────

fn publish_labeler_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labelers.publish").await?;
            let req: PublishLabelerRequest = decode(&payload).map_err(malformed)?;
            // `actor_id` is the authenticated router identity — the un-rotatable
            // caller the per-caller quota keys on. Unlike the
            // class gate above, it is now threaded into the store path, not discarded.
            let (labeler_id, version) = publish_labeler_core(
                &state.db,
                &actor_id,
                req.metadata_blob.as_ref(),
                req.wasm_bytes.as_ref(),
                &req.content_kind,
                &req.artifact_kind,
            )
            .await?;
            encode_reply(&PublishLabelerReply {
                labeler_id: ByteBuf::from(labeler_id.to_vec()),
                version,
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn list_labelers_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labelers.list").await?;
            let _req: ListLabelersRequest = decode(&payload).map_err(malformed)?;
            let labelers = list_labelers_core(&state.db, &actor_id).await?;
            encode_reply(&ListLabelersReply {
                labelers,
                extra: Default::default(),
            })
        })
    })
}

/// The `list` core (design § 5 "list"): every catalog entry, metadata-only, each
/// stamped with whether **this caller** currently subscribes to it — the
/// personalization home's subscribed-labelers facet reads this flag rather than
/// a bulk "list my subscriptions" RPC (there is only one; a per-row flag is
/// simpler than a second round-trip + client-side join). Depends only on the DB
/// so the registry test drives it directly, like `subscribe_labeler_core`.
pub async fn list_labelers_core(
    db: &crate::db::CacheDb,
    caller: &[u8; 32],
) -> Result<Vec<LabelerSummary>, RpcError> {
    let rows = db.list_labelers().await.map_err(internal)?;
    let subscribed = db
        .list_subscribed_labeler_ids(caller)
        .await
        .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|r| LabelerSummary {
            subscribed: subscribed.contains(&r.labeler_id),
            labeler_id: ByteBuf::from(r.labeler_id),
            version: r.version,
            publisher_actor: ByteBuf::from(r.publisher_actor),
            content_kind: r.content_kind,
            factor: r.factor,
            wasm_hash: ByteBuf::from(r.wasm_hash),
            wasm_size: r.wasm_size,
            artifact_kind: r.artifact_kind,
            artifact_version: r.artifact_version,
            extra: Default::default(),
        })
        .collect())
}

fn inspect_labeler_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labelers.inspect").await?;
            let req: InspectLabelerRequest = decode(&payload).map_err(malformed)?;
            let rec = state
                .db
                .get_labeler(req.labeler_id.as_ref())
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("no such labeler"))?;
            encode_reply(&InspectLabelerReply {
                metadata_blob: ByteBuf::from(rec.metadata_blob),
                wasm_bytes: ByteBuf::from(rec.wasm_bytes),
                artifact_kind: rec.artifact_kind,
                extra: Default::default(),
            })
        })
    })
}

fn subscribe_labeler_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labelers.subscribe").await?;
            let req: SubscribeLabelerRequest = decode(&payload).map_err(malformed)?;
            let grant_id = req.grant_id.as_ref().map(|g| g.as_ref());
            let factor =
                subscribe_labeler_core(&state.db, &actor_id, req.labeler_id.as_ref(), grant_id)
                    .await?;
            encode_reply(&SubscribeLabelerReply {
                factor,
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn unsubscribe_labeler_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labelers.unsubscribe").await?;
            let req: UnsubscribeLabelerRequest = decode(&payload).map_err(malformed)?;
            // Owner-scoped (owner == caller) + idempotent (absent row → ok:true).
            // The core also withdraws a List labeler's materialized bus rows
            // when the last subscriber leaves (design Block A, D12c).
            unsubscribe_labeler_core(&state.db, &actor_id, req.labeler_id.as_ref()).await?;
            encode_reply(&UnsubscribeLabelerReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Register all five `fauna.labelers.*` handlers (mirrors
/// `register_capability_handlers`). Wired into `lib.rs::build_rpc_router`. The
/// per-kind deadlines match `KindRegistry::register_labeler_kinds` (the
/// client-facing twin): `publish`/`inspect` carry WASM bytes → 30 s;
/// `list`/`subscribe`/`unsubscribe` are light → 5 s.
pub fn register_labeler_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.labelers.publish",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: publish_labeler_handler(),
        },
    );
    b.add(
        "fauna.labelers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_labelers_handler(),
        },
    );
    b.add(
        "fauna.labelers.inspect",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: inspect_labeler_handler(),
        },
    );
    b.add(
        "fauna.labelers.subscribe",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: subscribe_labeler_handler(),
        },
    );
    b.add(
        "fauna.labelers.unsubscribe",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: unsubscribe_labeler_handler(),
        },
    );
}
