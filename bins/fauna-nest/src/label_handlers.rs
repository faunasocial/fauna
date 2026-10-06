//! Content-label WS-RPC handlers (bearer connection) — part of the
//! WS-RPC-everywhere migration (tracked internally). A behavior-preserving
//! transport migration of the (now-deleted) HTTP routes `POST /api/v1/labels`
//! + `GET /api/v1/labels/{content_id}` onto the per-actor WS-RPC connection.
//!
//! Each handler reuses the same `CacheDb` label method the twin called — no
//! shared core (one DB call plus reply shaping), mirroring
//! `register_pending_actions_handlers`. The connection `actor_id` is the
//! authenticated caller (the write twin used `BearerAuth`; the read twin was
//! public and becomes `User`-authed here, the documented feed/posts precedent).
//!
//! - `fauna.labels.attach` — `upsert_content_label` per label (the twin's
//!   non-empty / ≤50 / 0.0–1.0 validation; here the wire carries
//!   `confidence_per_mille` 0–1000 which the handler maps to the stored `f64`).
//! - `fauna.labels.list` — `get_content_labels("post", id)`.
//!
//! Caller class (`bridge_method_allowlist::is_permitted`): `list` is `User`;
//! `attach` is `User` plus the grant-holder classes `BridgeMda |
//! ContentProcessor` — the set `fauna.capabilities.fetch` admits, since only a
//! class that can fetch a grant can hold one. Error codes are scoped to the
//! `fauna.labels.*` namespace; malformed payloads + DB faults map to
//! `fauna.protocol.*` / `fauna.labels.internal`.
//!
//! ## Who may attach — the class check is NOT the gate
//!
//! The caller's class says only *a member is asking*; it does not say the member
//! may speak about **this** post. `attach` writes onto the label plane two
//! verdicts read that bind every reader — the mandatory feed spam guard
//! (`feed_routes::ensure_spam_filter`) and the nest-as-publisher region fold
//! (`web_content::region`) — so a class-only gate let any member remove any
//! member's post from every feed on the nest, and (under an enrolled authority)
//! replace their public page, silently and with no remedy. That contradicts
//! `moderation.md` § Categories & enforcement item 1 (*"No social post is
//! removed nest-wide for everyone as a matter of policy or opinion"*) and
//! `region-blocking.md` § Invariants item 5 (no lever but the authority's).
//!
//! So the door admits exactly the producers `moderation.md` § Per-row badge data
//! path sanctions for a feed post — **the post's own author**, and **a holder of
//! a `content.label-write` grant from that author** (the same scope
//! `fauna.capabilities.submit_scores` gates on, resolved through the same
//! `holder_granted_scopes` — which answers only for an enrolled service user,
//! so the class arm has to admit the holder classes for this half to be
//! reachable at all — it did not, until the arm was widened). Authorization binds to the post's TRUE author read
//! from the store (`routes::resolve_post_author`), never to anything the caller
//! declares — the shape `web_handlers::require_post_authorship` already uses and
//! the lesson `submit_scores`' claimed-owner finding left. A well-formed id
//! naming no post is refused too: there is nothing to own.
//!
//! Every written row then **names its writer** — `scanner_id` (the writing
//! position, as a community room's labeler pass stamps the nest identity) and
//! `classifier_id` (the producer, as that pass stamps the labeler id) both carry
//! the caller's actor id. Since `classifier_id` is part of the upsert key, two
//! writers can no longer collide on one last-writer-wins row, and the two
//! reader-binding verdicts can require attribution — the rule
//! `db::feeds::ATTRIBUTED_LABEL` states and both of them enforce.

use std::time::Duration;

use fauna_protocol::labels::{
    LabelInput, LabelOutput, LabelsAttachReply, LabelsAttachRequest, LabelsListReply,
    LabelsListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.labels.*` code.
const NS: &str = "labels";

/// Max labels per attach (mirrors the former `POST /api/v1/labels` twin).
const MAX_LABELS: usize = 50;

// ── Helpers (mirroring `pending_action_handlers`, scoped to `labels`) ────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

fn permission_denied(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::permission_denied_ns(NS, reason)
}

fn not_found(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::not_found_ns(NS, reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// The `details` text of the ownership-or-grant refusal — the one refusal
/// `authorize_attach` ever emits. Exported because it is the ONLY wire-visible
/// difference between this refusal and the class gate's: both answer
/// `fauna.labels.permission_denied` (`api-layers.md` § Caller-class
/// authorization → *Refusal codes at the gate* keeps within-class authorship
/// refusals on the family code), and neither class gate's `details` is this
/// text: on a live connection the central capability gate (`routes.rs`, step
/// 1d) refuses a class before the handler runs, reading `"caller class not
/// permitted for this kind"`, and the handler's own defense-in-depth gate — the
/// one a handler-level `dispatch` test reaches — reads `"<kind> not permitted
/// for caller class <Class>"`. A test that wants to know
/// WHICH gate refused — "your class may never call this door" versus "you lack
/// the author's grant" — asserts on this constant, not on the code
/// alone.
pub const GRANT_REFUSAL_DETAILS: &str =
    "labelling another actor's post needs a content.label-write grant from its author";

/// Validate one label input and return its stored `f64` confidence.
/// Mirrors the twin's checks: non-empty category, confidence in domain.
fn validate_label(label: &LabelInput) -> Result<f64, RpcError> {
    if label.category.is_empty() {
        return Err(invalid_request("category must not be empty"));
    }
    if !(0..=1000).contains(&label.confidence_per_mille) {
        return Err(invalid_request(format!(
            "confidence_per_mille must be 0-1000, got {}",
            label.confidence_per_mille
        )));
    }
    Ok(label.confidence_per_mille as f64 / 1000.0)
}

fn now_millis() -> i64 {
    fauna_core::data::Timestamp::now_millis() as i64
}

/// The wire `content_id` → the post id it names. `content_labels.content_id` is
/// TEXT lowercase hex of the 32-byte content id — the same `lower(hex(c.id))`
/// bridge the feed filter predicates and `project_content_labels` join on — so a
/// string that is not that is a request about no post at all. The twin took any
/// string because it authorized nothing; a door that must check ownership has to
/// resolve the id first.
fn post_id_of(content_id: &str) -> Result<[u8; 32], RpcError> {
    let bytes = hex::decode(content_id)
        .map_err(|_| invalid_request("content_id must be the post id's lowercase hex"))?;
    bytes
        .try_into()
        .map_err(|_| invalid_request("content_id must be 32 bytes of lowercase hex"))
}

/// Refuse unless `actor_id` is a sanctioned producer for `post_id`'s labels:
/// its **author**, or a holder of a `content.label-write` grant **from that
/// author** (`moderation.md` § Per-row badge data path → *Producers unchanged*;
/// the grant primitive is `encryption-at-rest.md` § Capability tiering).
///
/// The author is read from the store, never from the request — the same
/// `resolve_post_author` oracle the post-delete rule and
/// `web_handlers::require_post_authorship` bind to, so the three cannot drift
/// and a caller cannot name an owner it likes (the
/// claimed-owner-not-content-bound shape `submit_scores` was hardened against).
async fn authorize_attach(
    state: &std::sync::Arc<crate::routes::AppState>,
    actor_id: &[u8; 32],
    post_id: &[u8; 32],
) -> Result<(), RpcError> {
    let author = crate::routes::resolve_post_author(state, post_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("no post with that content_id"))?;
    if author == *actor_id {
        return Ok(());
    }
    // A third party's label needs the author's own grant. `kind: None` is an
    // any-kind grant (the shape the client mint issues beside
    // `content.read{kind}`); a kinded one must name this plane's kind. The
    // resolver answers only for an enrolled service user, which is why the
    // class arm admits the holder classes: a plain `User` who is not the
    // author reaches here and is refused below, holding nothing.
    //
    // Fail closed on ANY error from the scope resolve, deliberately: its
    // ordinary answer for a plain member is `permission_denied` ("not an
    // enrolled service user"), which is not a fault but simply "holds no
    // grants", and the one refusal this function should ever emit is the
    // grant-shaped one below. A real DB fault therefore also reads as "no
    // grant" — the safe direction for an authorization gate, and the caller
    // still learns it was refused.
    let granted = crate::bridge_blob_handlers::holder_granted_scopes(state, actor_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .any(|(owner, scope)| {
            owner == author
                && scope.class == "content.label-write"
                && (scope.kind.is_none() || scope.kind.as_deref() == Some("post"))
        });
    if granted {
        return Ok(());
    }
    Err(permission_denied(GRANT_REFUSAL_DETAILS))
}

// ── fauna.labels.attach (≡ POST /api/v1/labels) ─────────────────────────────

fn attach_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labels.attach").await?;
            let req: LabelsAttachRequest = decode(&payload).map_err(malformed)?;

            if req.labels.is_empty() {
                return Err(invalid_request("labels array must not be empty"));
            }
            if req.labels.len() > MAX_LABELS {
                return Err(invalid_request(format!(
                    "too many labels (max {MAX_LABELS})"
                )));
            }
            // Validate all before writing any (the twin rejected the whole batch
            // on the first bad label).
            let confidences: Vec<f64> = req
                .labels
                .iter()
                .map(validate_label)
                .collect::<Result<_, _>>()?;

            // Authorize only after the payload validates, so a malformed batch
            // still answers `invalid_request` rather than leaking the ownership
            // verdict for an id the caller never sent a usable label for.
            let post_id = post_id_of(&req.content_id)?;
            authorize_attach(&state, &actor_id, &post_id).await?;

            let now = now_millis();
            // The row names its writer, both columns, the way a community room's
            // labeler pass names its own (`db::room_labels::record_bus`):
            // `scanner_id` = the position that wrote it, `classifier_id` = the
            // producer of the verdict. For a hand-attached API label those are
            // the same principal — the caller. `classifier_id` being part of the
            // upsert key is what stops two writers sharing one last-writer-wins
            // row, and a non-zero `scanner_id` is what the reader-binding
            // verdicts require (`db::feeds::ATTRIBUTED_LABEL`).
            let classifier_id = actor_id;
            let scanner_id = actor_id;
            let mut stored = 0i64;
            for (label, confidence) in req.labels.iter().zip(confidences) {
                state
                    .db
                    .upsert_content_label(
                        "post",
                        &req.content_id,
                        &label.category,
                        confidence,
                        0, // mechanism_type
                        &classifier_id,
                        1, // classifier_version
                        0, // attestation_type
                        None,
                        None,
                        now,
                        &scanner_id,
                        &[],
                    )
                    .await
                    .map_err(internal)?;
                stored += 1;
            }

            // A label on a web-published post can change what that post's pages
            // show — the nest-as-publisher fold folds it through the region
            // content policy in force, and a blocked or collapsed post renders
            // its placeholder on every page that carries it
            // (`region-blocking.md` § The nest-as-publisher leg). The write's
            // own transaction recorded the owed render
            // (`upsert_content_label`); this pays it, keyed on the marker like
            // every other owed door, so a client's retry of a torn attach —
            // which changes nothing the second time — still renders. Logged:
            // the labels are committed and authoritative either way.
            if let Some(wcs) = &state.web_content_service {
                for publisher in state
                    .db
                    .web_publishers_of_post(&post_id)
                    .await
                    .unwrap_or_default()
                {
                    if let Err(e) = wcs.render_owed(&publisher, "a label attach").await {
                        tracing::warn!("web re-render after a label attach: {e:#}");
                    }
                }
            }

            encode_reply(&LabelsAttachReply {
                stored,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.labels.list (≡ GET /api/v1/labels/{content_id}) ───────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.labels.list").await?;
            let req: LabelsListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .get_content_labels("post", &req.content_id)
                .await
                .map_err(internal)?;
            let labels = rows
                .into_iter()
                .map(|r| LabelOutput {
                    category: r.category,
                    confidence_per_mille: (r.confidence * 1000.0).round() as i64,
                    mechanism_type: r.mechanism_type as i64,
                    created_at: r.created_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&LabelsListReply {
                content_id: req.content_id,
                labels,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the content-label surface on the **bearer** router. Both kinds are
/// `forbid_replay = false` @5 s — a pure read plus an idempotent UPSERT. See
/// `KindRegistry::register_labels_kinds`.
pub fn register_labels_handlers(b: &mut RpcRouterBuilder) {
    let meta = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.labels.attach", meta(attach_handler()));
    b.add("fauna.labels.list", meta(list_handler()));
}
