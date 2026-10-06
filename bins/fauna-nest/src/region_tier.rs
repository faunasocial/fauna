//! The region tier — ingesting an authority's published feature policy
//! (`dynamic-features.md` § The region tier, § Fail posture;
//! `region-blocking.md` § The region/authority plumbing, W2 (account-data-plane.md § Workstreams) slice 4).
//!
//! This is the producer `RuleTier::Region` never had. The store has reserved a
//! Region row since slice 2 (`db::feature_gate::stored_tiers`) and nothing on
//! the box has ever written it, so the tier existed in the meet, in the wire
//! types and in the attribution with no way to be populated. Here it is
//! populated — and through the **already tier-generic**
//! `CacheDb::put_feature_policy`, so ingesting a region document needs no store
//! of its own.
//!
//! ## Three rules this module exists to hold
//!
//! **1. The ingress is where log inclusion is held today, so the ingress is a
//! hard-coded constant.** § The transparency log makes publication what gives a
//! policy effect, and no signature can carry that property — so
//! [`REGION_LOG_BASE_URL`] is compiled in (bucket 1 — artifact wiring, chosen by
//! no human: § The region registry is explicit that this has *"no config
//! surface"*), and there is deliberately **no kind that submits an artifact**: a
//! submit surface would be an ingress the admin controls, which is
//! region-blocking.md invariant 5 inverted. The section's ratified strengthening
//! (2026-08-11 — a checkable inclusion proof, head monotonicity, the compiled-in
//! anchor and witnessed checkpoints; built 2026-09-09 as
//! `fauna_core::region_authority::inclusion`) adds cryptographic evidence on top
//! of this line; it does not replace it. The fetch reads the evidence the log
//! serves **beside** the artifact ([`fetch_bundle`]) and
//! [`ingest_feature_policy`] admits the artifact through
//! `admit_artifact` against the anchor this nest persists
//! (`db::region_tier::region_log_anchor`). Today — the log unpublished, the
//! compiled-in anchor `None`, the witness roster empty — that is the **pre-log
//! era**, and its rule is stated in code rather than implied: an artifact with
//! no evidence is admitted only while the anchor is the compiled-in empty one;
//! evidence that is served is checked, and refused under the empty roster.
//!
//! **2. A region with no enrolled authority is not fetched at all.** The
//! compiled-in registry enrols nobody today, so the worker makes no network
//! request on any deployment — dormant by construction rather than by a flag.
//! It also keeps the staleness warning honest: warning that an unreachable
//! authority has gone quiet, when the region has no authority, is the kind of
//! permanent false alarm that teaches an admin to ignore warnings.
//!
//! **3. A failed or refused refresh writes nothing.** That *is* § Fail posture's
//! last-known-good: not a code path that restores an old value, but the absence
//! of any path that removes the binding one. The only ways a region document
//! stops binding are a newer artifact replacing it, the admin withdrawing the
//! declaration, and the authority ceasing to be enrolled (rule below).

use std::time::Duration;

use anyhow::{Context, Result};
use fauna_core::feature_gate::{RuleTier, registry};
use fauna_core::region_authority::{
    InclusionEvidence, PAYLOAD_KIND_CONTENT_POLICY, PAYLOAD_KIND_FEATURE_POLICY, PolicyArtifact,
    RegionCode, RegionRegistry, WitnessRoster, admit_artifact, compiled_in_registry,
    compiled_in_witness_roster, log_evidence_path, log_path, verify_artifact,
};
use fauna_protocol::region::{
    AdminRegionSetReply, AdminRegionSetRequest, AdminRegionStatusReply, AdminRegionStatusRequest,
    RegionDocumentRef,
};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// The transparency log's base URL — the **only** place a policy artifact is
/// ever read from (see rule 1 in the module note).
///
/// A constant rather than a setting because nobody chooses it: it is where the
/// public log lives, the same way a root program's trust list is not a
/// deployment preference. Mirrors are consumed by pointing a *fork of Fauna* at
/// a different constant, which is a build, not a knob.
///
/// ⚠ The log itself is not yet published — hosting it is an administrative act
/// by the Fauna organization, like enrolling an authority. Since the registry
/// enrols nobody, no deployment reaches this URL today (rule 2).
pub const REGION_LOG_BASE_URL: &str = "https://log.fauna.social";

/// The refresh cadence — the shared
/// [`fauna_core::region_authority::REFRESH_INTERVAL_SECS`], which an app's relay
/// refresh runs on too.
pub const REFRESH_INTERVAL: Duration =
    Duration::from_secs(fauna_core::region_authority::REFRESH_INTERVAL_SECS);

/// How long the channel may go unreached before the admin surface warns — the
/// shared [`fauna_core::region_authority::STALE_AFTER_SECS`].
pub const STALE_AFTER: Duration =
    Duration::from_secs(fauna_core::region_authority::STALE_AFTER_SECS);

/// The largest artifact body this nest will read off the wire, before it is even
/// decoded. The envelope's own bound plus headroom for the CBOR framing around
/// the payload.
const MAX_FETCH_BYTES: usize = fauna_core::region_authority::MAX_PAYLOAD_BYTES + 64 * 1024;

/// The staleness verdict and the "last reached" reading a refresh state
/// implies at `now` — pure so it is unit-testable with an injected clock,
/// since `crate::db::now_epoch_secs` has no override seam
/// (`db/mod.rs::now_epoch_secs` hard-wires `SystemTime`). Mirrors the relay's
/// own rule (`crate::region_relay::reply_from`): reads the last time the
/// channel **ANSWERED** (`reached_at`), never the last attempt, so a failing
/// worker cannot keep a silent channel looking fresh; a channel that has never
/// answered is anchored on its first attempt instead. The caller still gates
/// on whether the region is *enrolled* — a channel that does not exist cannot
/// go quiet.
fn refresh_staleness(
    state: &crate::db::region_tier::RefreshState,
    now: i64,
) -> (bool, Option<u64>) {
    let anchor = state.reached_at.unwrap_or(state.first_attempted_at);
    let stale = now.saturating_sub(anchor) > STALE_AFTER.as_secs() as i64;
    (stale, state.reached_at.map(|t| t.max(0) as u64))
}

/// The audit action for a declaration change.
const AUDIT_REGION_SET: &str = "admin:region.set";

/// What ingesting an artifact did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingested {
    /// A newer artifact was accepted and its document is now in force.
    Accepted { sequence: u64 },
    /// The artifact was refused; the previously accepted one still binds.
    Refused(String),
}

/// The registry this nest verifies against.
///
/// A function rather than an inlined call so that the (later) refreshed-registry
/// slice has exactly one place to change, and so tests can see the same seam the
/// production path uses.
fn registry_snapshot() -> RegionRegistry {
    compiled_in_registry()
}

/// The witness roster this nest counts a checkpoint's cosignatures against —
/// the same seam shape as [`registry_snapshot`], for the same two reasons.
fn witness_roster_snapshot() -> WitnessRoster {
    compiled_in_witness_roster()
}

/// Ingest one artifact **for the region this deployment declares**, with the
/// inclusion evidence the log served beside it (`None` when it served none).
///
/// Refuses, before verifying anything, an artifact for any other region: an
/// artifact is legitimately signed and legitimately published and still not this
/// deployment's, and § The region tier binds a nest at *its own* declared situs.
/// Checking that here rather than at the fetch means it holds for every ingress
/// this module will ever grow.
///
/// Two questions, in order: *who signed this* (`verify_artifact`, against the
/// registry) and *was it published* (`admit_artifact`, against the persisted
/// log anchor and the witness roster). Either refusal leaves the last accepted
/// artifact exactly where it was.
pub async fn ingest_feature_policy(
    state: &AppState,
    artifact: PolicyArtifact,
    evidence: Option<InclusionEvidence>,
    registry: &RegionRegistry,
) -> Result<Ingested> {
    let Some(declared) = state.db.get_declared_region().await? else {
        return Ok(Ingested::Refused(
            "this deployment declares no region".into(),
        ));
    };
    if artifact.region != declared {
        return Ok(Ingested::Refused(format!(
            "artifact is for region {}, this deployment declares {declared}",
            artifact.region
        )));
    }
    if artifact.payload_kind != PAYLOAD_KIND_FEATURE_POLICY {
        return Ok(Ingested::Refused(format!(
            "artifact carries payload kind {:?}, not a feature policy",
            artifact.payload_kind
        )));
    }

    // The replay floor is the authority's own high-water mark, so it is read
    // per authority — the registry names who administers this region. An
    // unknown region has no floor to read and no authority to read it for;
    // `verify_artifact` refuses it on `UnknownRegion` a few lines down, which
    // is the one refusal that must not depend on stored state.
    let last = match registry.region(&declared) {
        Some(entry) => {
            state
                .db
                .accepted_region_sequence(
                    &declared,
                    PAYLOAD_KIND_FEATURE_POLICY,
                    &entry.authority_name,
                )
                .await?
        }
        None => None,
    };
    let verified = match verify_artifact(
        artifact,
        registry,
        crate::db::now_epoch_secs().max(0) as u64,
        last,
    ) {
        Ok(verified) => verified,
        Err(e) => return Ok(Ingested::Refused(e.to_string())),
    };
    let document = match verified.feature_policies() {
        Ok(document) => document,
        Err(e) => return Ok(Ingested::Refused(e.to_string())),
    };

    // Was it published? The anchor read is deliberately fallible: a corrupt
    // stored head must not read as "pre-log" and re-open the first-fetch window.
    let anchor = state.db.region_log_anchor().await?;
    let advanced = match admit_artifact(
        verified.artifact(),
        evidence.as_ref(),
        &anchor,
        &witness_roster_snapshot(),
    ) {
        Ok(advanced) => advanced,
        Err(e) => return Ok(Ingested::Refused(e.to_string())),
    };

    // Persist first, fold second. The order is the recoverable one: a crash
    // between them leaves an accepted artifact whose documents have not been
    // written, and the next boot's re-fold (which runs before any fetch) applies
    // them. The opposite order would leave documents in force with no artifact
    // recording *why*, which the transparency read could not explain. The
    // anchor advances after the artifact lands: a crash between the two leaves
    // the anchor *behind* the accepted head, which only means the next head
    // must descend from an older point than necessary — never that a
    // non-descendant gets through.
    state.db.put_region_artifact(&verified).await?;
    if advanced != anchor {
        state.db.put_region_log_anchor(&advanced).await?;
    }
    fold_in(state, &document).await?;
    Ok(Ingested::Accepted {
        sequence: verified.sequence(),
    })
}

/// Write the document's bounds into the Region tier, and clear the tier for
/// every registry member the document does **not** name.
///
/// Whole-document replace, matching the ratified semantics of the policy-update
/// kinds: a feature an authority stopped naming is *"no opinion at this tier"*,
/// which drops out of the meet — not an authored allow, and not a bound left
/// quietly in force from a document that no longer exists.
///
/// An entry naming a feature this build does not know is skipped rather than
/// dropped: the whole envelope is stored, so a later build re-folds it and picks
/// the bound up (see [`refold_stored_artifact`]).
async fn fold_in(
    state: &AppState,
    document: &fauna_core::region_authority::RegionFeaturePolicies,
) -> Result<()> {
    // The tier is nest-wide, so `put_feature_policy` derives the subject key
    // itself and ignores this actor entirely (its doc comment: *"passing an
    // actor for a nest-wide tier is not an error the caller can make"*).
    let nest_wide = [0u8; 32];
    for entry in registry() {
        match document.get(entry.feature.as_str()) {
            Some(policy) => {
                state
                    .db
                    .put_feature_policy(RuleTier::Region, &nest_wide, entry.feature, policy)
                    .await?
            }
            None => {
                state
                    .db
                    .clear_feature_policy(RuleTier::Region, &nest_wide, entry.feature)
                    .await?;
            }
        }
    }
    Ok(())
}

/// Drop every Region-tier document — the withdrawal path.
async fn clear_tier(state: &AppState) -> Result<()> {
    let nest_wide = [0u8; 32];
    for entry in registry() {
        state
            .db
            .clear_feature_policy(RuleTier::Region, &nest_wide, entry.feature)
            .await?;
    }
    Ok(())
}

/// Re-apply the stored artifact against the **current** registry.
///
/// Two jobs, both of which need the whole envelope to have been kept:
///
/// - **Pick up bounds a newer build now understands.** An authority may name a
///   feature that was unknown when the artifact was accepted; re-folding after
///   an upgrade applies it without waiting for a republication.
/// - **Stop honouring an authority that is no longer enrolled.** If the stored
///   artifact no longer verifies against the current registry — the key was
///   removed, the region de-listed — the tier is **cleared**. That direction is
///   deliberate: a registry revision that de-lists an authority is Fauna saying
///   this party was never (or is no longer) legitimate, and keeping its
///   restrictions in force forever would make de-listing meaningless. Tier 1's
///   constants keep binding underneath regardless, so the floor never moves.
///
/// ⚠ **A payload this build cannot read is neither of those jobs, and clears
/// nothing** (§ Fail posture — the undecodable-document clause). De-listing is a
/// statement by Fauna about the authority; an unreadable payload is a statement
/// about *this build's decoder*. The bounds the last successful fold wrote stay
/// in force — last-known-good, the same clause that makes an unreachable channel
/// a warning rather than an outage.
///
/// ⚠ Verification here passes `last_accepted_sequence: None` on purpose — the
/// stored artifact *is* the accepted one, so checking it against its own
/// sequence would refuse it as a replay.
pub async fn refold_stored_artifact(state: &AppState, registry: &RegionRegistry) -> Result<()> {
    let Some(declared) = state.db.get_declared_region().await? else {
        return Ok(());
    };
    let Some(stored) = state
        .db
        .get_region_artifact(&declared, PAYLOAD_KIND_FEATURE_POLICY)
        .await?
    else {
        return Ok(());
    };

    let outcome = stored.artifact().and_then(|artifact| {
        verify_artifact(
            artifact,
            registry,
            crate::db::now_epoch_secs().max(0) as u64,
            None,
        )
        .map_err(anyhow::Error::from)
    });
    match outcome {
        Ok(verified) => match verified.feature_policies() {
            Ok(document) => fold_in(state, &document).await,
            // § Fail posture: *"a fetched policy stays in force until replaced —
            // never silently relaxes"*, and a re-derivation that fails replaces
            // nothing. The authority is still enrolled and this artifact is
            // still its statement; only *this build's* reading of the payload
            // broke (a decoder tightening across an upgrade, a corrupted row).
            // So the rows the last successful fold wrote keep binding, and the
            // condition is loud rather than lifted.
            //
            // ⚠ The arm below is the opposite case on purpose: an artifact that
            // no longer *verifies* is Fauna de-listing the authority, and that
            // must retire its document. The two failures sit one line apart and
            // mean opposite things — do not collapse them.
            Err(e) => {
                tracing::error!(
                    region = %declared,
                    "stored region artifact still verifies but its payload no longer decodes ({e}); \
                     keeping the last-known-good bounds in force"
                );
                Ok(())
            }
        },
        Err(e) => {
            tracing::warn!(
                region = %declared,
                "stored region artifact no longer verifies ({e}); clearing the region tier"
            );
            state.db.clear_region_artifacts(&declared).await?;
            clear_tier(state).await
        }
    }
}

/// The active region document, as the transparency read and the admin screen
/// name it.
pub async fn active_document(state: &AppState) -> Result<Option<RegionDocumentRef>> {
    use crate::db::region_tier::DeclaredRegion;

    let stored = match state.db.declared_region_state().await? {
        DeclaredRegion::Undeclared => return Ok(None),
        DeclaredRegion::Declared(declared) => {
            state
                .db
                .get_region_artifact(&declared, PAYLOAD_KIND_FEATURE_POLICY)
                .await?
        }
        // The declaration is the lookup key, and it is unreadable — but the
        // document's folded bounds keep binding (§ Fail posture: the re-fold
        // stops running rather than clearing them), so answering "no region
        // document" here would hide a live restriction's identity: boundary
        // 4's silent gate. The identity was never lost — the artifact store
        // keeps it, keyed by its own intact region column, and it holds at
        // most one region's rows (the write seam's retirement invariant), so
        // the sole row *is* the binding document (`nest/common.md`
        // § Unreadable stored values, rule 2: the read moves with the fold).
        DeclaredRegion::Unreadable => {
            state
                .db
                .sole_region_artifact(PAYLOAD_KIND_FEATURE_POLICY)
                .await?
        }
    };
    let Some(stored) = stored else {
        return Ok(None);
    };
    // The authority's *name* is the registry's, never the artifact's — the screen
    // that tells a user who restricts them must not let the restrictor choose the
    // label. It is read from the **stored** row rather than looked up again, and
    // that is load-bearing: a read that re-consulted the registry would answer
    // "no region" the moment an authority was de-listed, while its document was
    // still in force — a bound nobody can see, which is boundary 4's silent gate
    // exactly. Retiring a de-listed authority is `refold_stored_artifact`'s job,
    // and it removes the bound and the name in one move.
    Ok(Some(RegionDocumentRef {
        region: stored.region,
        authority_name: stored.authority_name,
        key_id: stored.key_id,
        sequence: stored.sequence,
        issued_at: stored.issued_at,
        extra: Default::default(),
    }))
}

pub const KIND_REGION_GET: &str = "fauna.admin.region.get";
pub const KIND_REGION_SET: &str = "fauna.admin.region.set";

pub fn register_region_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    // Mirror any change in `KindRegistry::register_admin_kinds`.
    b.add(
        KIND_REGION_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: region_get_handler(),
        },
    );
    b.add(
        KIND_REGION_SET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: region_set_handler(),
        },
    );
}

fn region_get_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_REGION_GET,
                internal,
            )
            .await?;
            let _req: AdminRegionStatusRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("region", e))?;

            let declaration = state.db.declared_region_state().await.map_err(internal)?;
            let declared = match &declaration {
                crate::db::region_tier::DeclaredRegion::Declared(region) => Some(region.clone()),
                _ => None,
            };
            let declaration_unreadable = matches!(
                declaration,
                crate::db::region_tier::DeclaredRegion::Unreadable
            );
            let registry = registry_snapshot();
            let enrolled = declared
                .as_ref()
                .is_some_and(|region| registry.region(region).is_some());
            let feature_policy = active_document(&state).await.map_err(internal)?;
            let refresh = state
                .db
                .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .map_err(internal)?;

            let now = crate::db::now_epoch_secs();
            // Stale only where a channel exists to be stale: an unenrolled
            // region has no authority publishing anything, and a nest that has
            // not attempted a refresh yet has not failed one.
            let (stale, last_checked_at) = refresh
                .as_ref()
                .map(|r| refresh_staleness(r, now))
                .unwrap_or((false, None));
            let stale = enrolled && stale;

            crate::rpc_errors::encode_reply(&AdminRegionStatusReply {
                declared,
                declaration_unreadable,
                enrolled,
                feature_policy,
                last_checked_at,
                last_error: refresh.and_then(|r| r.detail),
                stale,
                extra: Default::default(),
            })
        })
    })
}

fn region_set_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_REGION_SET,
                internal,
            )
            .await?;
            let req: AdminRegionSetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("region", e))?;

            // Re-declaring or withdrawing always retires what the *old* region's
            // authority had in force. A document is a statement by one region's
            // authority about deployments in that region; carrying it across a
            // change of situs would leave a deployment bound by an authority
            // that no longer claims it.
            //
            // The retirement is keyed on what must SURVIVE (everything not the
            // new region's), never on the outgoing declaration — an outgoing
            // row that no longer parses cannot name its region, and a
            // retirement that needed it to would silently skip, leaving the old
            // document's artifacts and bounds in force behind a declaration
            // that says otherwise (`nest/common.md` § Unreadable stored
            // values). This is also what keeps `region_artifacts` structurally
            // single-region, the invariant `sole_region_artifact`'s identity
            // recovery stands on.
            let previous = state.db.declared_region_state().await.map_err(internal)?;
            let changed = match (&previous, &req.region) {
                (crate::db::region_tier::DeclaredRegion::Declared(p), Some(n)) => p != n,
                (crate::db::region_tier::DeclaredRegion::Undeclared, None) => false,
                // Undeclared→declare, Declared→withdraw, Unreadable→anything:
                // a corrupt row never equals a canonical code, so recovery
                // passes through here too.
                _ => true,
            };
            if changed {
                state
                    .db
                    .clear_region_artifacts_except(req.region.as_ref())
                    .await
                    .map_err(internal)?;
                // The tier rows are the fold of the surviving artifact or of a
                // retired one. If the incoming region's own artifact survived
                // the retirement (re-declaring the region a corrupt row
                // meant), the rows are exactly its fold — keep them, so the
                // recovery costs no coverage gap until the next refresh.
                // Otherwise whatever the tier holds belongs to a retired
                // document and goes with it.
                let keeps_document = match &req.region {
                    Some(region) => state
                        .db
                        .get_region_artifact(region, PAYLOAD_KIND_FEATURE_POLICY)
                        .await
                        .map_err(internal)?
                        .is_some(),
                    None => false,
                };
                if !keeps_document {
                    clear_tier(&state).await.map_err(internal)?;
                }
            }

            // `changed` rides INTO the write: a declaration that moves changes
            // what every public page folds, so the write that moves it is the
            // transaction that owes those renders — not the walk below, which
            // a restart can cut off before its first statement
            // (`web-content-hosting.md` § Routing, render, serving → *A revoke
            // is durable*).
            match &req.region {
                Some(region) => state
                    .db
                    .set_declared_region(region, changed)
                    .await
                    .map_err(internal)?,
                None => {
                    state
                        .db
                        .clear_declared_region(changed)
                        .await
                        .map_err(internal)?;
                }
            }

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    AUDIT_REGION_SET,
                    req.region.as_ref().map(|r| r.as_str()),
                    Some(match &req.region {
                        Some(_) => "declared",
                        None => "withdrawn",
                    }),
                )
                .await;

            if changed {
                // The nest-as-publisher leg's own ask: the relay fetches only
                // what has been asked for, and no app may ever ask for the
                // nest's own situs — so declaring one records the demand for
                // every content policy on its chain, and schedules the first
                // refill rather than waiting a cadence for it.
                if let Some(region) = &req.region {
                    demand_situs_content_policies(&state, region).await;
                }
                // The public pages are static: whatever the old declaration
                // had in force is re-rendered away now, and whatever the new
                // one has cached already is applied now.
                rerender_public_sites(&state).await;
            }

            crate::rpc_errors::encode_reply(&AdminRegionSetReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// One refresh attempt: re-fold what is stored, then — only for an **enrolled**
/// declared region — fetch and ingest.
pub async fn refresh_once(state: &AppState) {
    let registry = registry_snapshot();
    if let Err(e) = refold_stored_artifact(state, &registry).await {
        tracing::warn!("region tier re-fold failed: {e:#}");
    }
    // The app relay rides the same tick and the same cadence (§ The content
    // plane: *"refilled from the log on the same cadence … the feature plane
    // already runs"*), and runs BEFORE the situs checks below: a deployment
    // that declares no region of its own still relays whichever regions its
    // apps declare.
    refresh_relay_once(state, &registry).await;

    let declared = match state.db.get_declared_region().await {
        Ok(Some(region)) => region,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("region tier: cannot read the declared region: {e:#}");
            return;
        }
    };
    // Rule 2: no enrolled authority, no fetch — and therefore no staleness
    // warning about a channel that does not exist.
    if registry.region(&declared).is_none() {
        return;
    }

    match fetch_bundle(&declared, PAYLOAD_KIND_FEATURE_POLICY).await {
        Ok((artifact, evidence)) => {
            match ingest_feature_policy(state, artifact, evidence, &registry).await {
                Ok(Ingested::Accepted { sequence }) => {
                    tracing::info!(region = %declared, sequence, "region tier: accepted a new policy");
                    state
                        .db
                        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, None, true)
                        .await;
                }
                // A refusal is a *reached* channel, so it is not staleness — but it
                // is worth surfacing, because an artifact that keeps failing
                // verification is the shape a compromised mirror has.
                Ok(Ingested::Refused(why)) => {
                    tracing::warn!(region = %declared, "region tier: refused an artifact: {why}");
                    state
                        .db
                        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some(&why), true)
                        .await;
                }
                // An ingest error is not a reach, the same classification the
                // relay worker gives its own `ingest_relay_artifact` `Err` arm
                // (`refresh_relay_one`, below): whatever failed here (a store
                // write, an anchor read) is not the log answering.
                Err(e) => {
                    let why = format!("{e:#}");
                    tracing::error!(region = %declared, "region tier: ingest error: {why}");
                    state
                        .db
                        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some(&why), false)
                        .await;
                }
            }
        }
        Err(e) => {
            let why = format!("{e:#}");
            tracing::warn!(region = %declared, "region tier: fetch failed: {why}");
            state
                .db
                .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some(&why), false)
                .await;
        }
    }
}

/// GET one region's artifact of one payload kind from the transparency log, and
/// the inclusion evidence the log serves **beside** it (§ The transparency log —
/// *a fetch-layer companion beside the envelope, never a field inside it*).
///
/// The evidence path answering 404 is "the log serves no evidence" — `None`,
/// which [`ingest_feature_policy`] admits only in the pre-log era. Any other
/// failure on either GET is a failed fetch: § Fail posture's staleness, not
/// a stripped bundle, because a mirror that can fail a request cannot
/// thereby *downgrade* a consumer to evidence-less acceptance. The two GETs
/// are not atomic — a head published between them fails the proof on the blob
/// hash and is retried at the next cadence.
async fn fetch_bundle(
    region: &RegionCode,
    payload_kind: &str,
) -> Result<(PolicyArtifact, Option<InclusionEvidence>)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build region-log HTTP client")?;

    let artifact_url = format!("{REGION_LOG_BASE_URL}/{}", log_path(region, payload_kind));
    let body = get_bounded(&client, &artifact_url)
        .await?
        .with_context(|| format!("GET {artifact_url}: not found"))?;
    let artifact: PolicyArtifact =
        fauna_protocol::decode_strict(&body).context("decode region artifact")?;

    let evidence_url = format!(
        "{REGION_LOG_BASE_URL}/{}",
        log_evidence_path(region, payload_kind)
    );
    let evidence = match get_bounded(&client, &evidence_url).await? {
        Some(body) => {
            Some(fauna_protocol::decode_strict(&body).context("decode region inclusion evidence")?)
        }
        None => None,
    };
    Ok((artifact, evidence))
}

/// One bounded GET: `Ok(None)` on 404, an error on any other non-success.
async fn get_bounded(client: &reqwest::Client, url: &str) -> Result<Option<bytes::Bytes>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let body = response
        .bytes()
        .await
        .with_context(|| format!("read {url}"))?;
    if body.len() > MAX_FETCH_BYTES {
        anyhow::bail!(
            "{url} is {} bytes, over the {MAX_FETCH_BYTES}-byte fetch bound",
            body.len()
        );
    }
    Ok(Some(body))
}

// ── The app relay (`fauna.region.artifact.get`) ─────────────────────────────
//
// `region-blocking.md` § The content plane → *How an app obtains its region's
// policy*. The three rules in the module note hold here unchanged, generalised
// from "the declared situs's feature policy" to "every (region, kind) an app
// has asked for": rule 1 — the ingress is still only the compiled-in log, and
// nothing submits; rule 2 — a pair is fetched only once an app has asked for
// it, and only while its region is enrolled; rule 3 — a failed or refused
// refresh writes nothing, so the cached envelope keeps answering.

/// Ingest one artifact **into the relay cache**, with the evidence the log
/// served beside it.
///
/// The same two questions [`ingest_feature_policy`] asks — *who signed this*
/// against `registry`, with the replay floor read for the region's authority,
/// and *was it published* against the persisted anchor — and the same answer
/// to a refusal: nothing is written, so the previously cached envelope keeps
/// answering. What it deliberately does **not** do is decode the document
/// inside: the nest is a relay, not a trust point, and an app newer than this
/// nest may understand a document version this build does not.
///
/// Takes `registry` rather than reading [`registry_snapshot`] so the caller
/// states which registry it verified against — the worker passes the
/// compiled-in one, the conformance suite a fixture enrolling a synthetic
/// authority — exactly as [`ingest_feature_policy`] does.
pub async fn ingest_relay_artifact(
    state: &AppState,
    artifact: PolicyArtifact,
    evidence: Option<InclusionEvidence>,
    registry: &RegionRegistry,
) -> Result<Ingested> {
    let region = artifact.region.clone();
    let payload_kind = artifact.payload_kind.clone();
    // The floor is the authority's high-water mark, so it is read per
    // authority; an unenrolled region has no floor and is refused by
    // `verify_artifact` on `UnknownRegion` just below.
    let last = match registry.region(&region) {
        Some(entry) => {
            state
                .db
                .accepted_region_sequence(&region, &payload_kind, &entry.authority_name)
                .await?
        }
        None => None,
    };
    let verified = match verify_artifact(
        artifact,
        registry,
        crate::db::now_epoch_secs().max(0) as u64,
        last,
    ) {
        Ok(verified) => verified,
        Err(e) => return Ok(Ingested::Refused(e.to_string())),
    };

    let anchor = state.db.region_log_anchor().await?;
    let advanced = match admit_artifact(
        verified.artifact(),
        evidence.as_ref(),
        &anchor,
        &witness_roster_snapshot(),
    ) {
        Ok(advanced) => advanced,
        Err(e) => return Ok(Ingested::Refused(e.to_string())),
    };

    let evidence_bytes = match &evidence {
        Some(ev) => Some(
            fauna_protocol::encode_canonical(ev)
                .context("encode relay inclusion evidence")?
                .to_vec(),
        ),
        None => None,
    };
    // Store first, anchor second — `ingest_feature_policy`'s order and reason:
    // a crash between them leaves the anchor behind the accepted head, which
    // never lets a non-descendant through.
    // Asked BEFORE the write, because the write is where the answer has to be
    // recorded: a document that binds the public render owes every publishing
    // site a render from the instant it lands, and the walk below is two
    // commits away. The early read is sound — the question is about the situs
    // and the registry, not about this artifact.
    let binds = binds_the_public_render(state, &region, &payload_kind, registry).await;
    state
        .db
        .put_relay_artifact(&verified, evidence_bytes.as_deref(), binds)
        .await?;
    if advanced != anchor {
        state.db.put_region_log_anchor(&advanced).await?;
    }
    if binds {
        rerender_public_sites(state).await;
    }
    Ok(Ingested::Accepted {
        sequence: verified.sequence(),
    })
}

/// Whether `(region, payload_kind)` is a document the nest-as-publisher leg
/// reads: a content policy for a region on the declared situs's chain under
/// `registry`. Only such a document's arrival or retirement changes what the
/// public pages must show.
async fn binds_the_public_render(
    state: &AppState,
    region: &RegionCode,
    payload_kind: &str,
    registry: &RegionRegistry,
) -> bool {
    if payload_kind != PAYLOAD_KIND_CONTENT_POLICY {
        return false;
    }
    match state.db.get_declared_region().await {
        Ok(Some(situs)) => registry
            .chain(&situs)
            .is_ok_and(|chain| chain.contains(region)),
        Ok(None) => false,
        // Unsure is a re-render: one render too many costs a render, one too
        // few leaves a page serving what the policy in force withholds.
        Err(e) => {
            tracing::warn!("region tier: cannot read the declared situs: {e:#}");
            true
        }
    }
}

/// Record the relay demand for every content policy on `situs`'s chain, and
/// schedule a refill for each pair asked for the first time.
async fn demand_situs_content_policies(state: &std::sync::Arc<AppState>, situs: &RegionCode) {
    let registry = demand_door_registry(state);
    let chain = match registry.chain(situs) {
        Ok(chain) => chain,
        Err(e) => {
            tracing::error!(%situs, "region registry chain unusable: {e}");
            return;
        }
    };
    for region in chain {
        // `chain` always includes `situs` itself even when it is not itself
        // enrolled — `RegionRegistry::chain`'s leaf push carries no
        // enrolment check, only the parent-walk does. `demand_relay_artifact`
        // holds the same enrolled-only invariant the relay's own door does
        // : a region nobody enrols never grows `region_relay_cache`,
        // admin-side declaration included.
        if let Err(e) = crate::region_relay::demand_relay_artifact(
            state,
            &registry,
            &region,
            PAYLOAD_KIND_CONTENT_POLICY,
        )
        .await
        {
            tracing::warn!(%region, "cannot record the situs content-policy demand: {e:#}")
        }
    }
}

/// Re-render every publishing site because what the situs chain has in force
/// moved (see [`crate::web_content::service::WebContentService::rerender_after_region_policy_change`]).
async fn rerender_public_sites(state: &AppState) {
    let Some(wcs) = &state.web_content_service else {
        return;
    };
    match wcs.rerender_after_region_policy_change().await {
        Ok(0) => {}
        Ok(failed) => tracing::warn!(
            failed,
            "region policy change: some sites failed to re-render and were cleared"
        ),
        Err(e) => tracing::error!("region policy change: cannot list the publishing sites: {e:#}"),
    }
}

/// Refill ONE (region, kind) the relay caches — the worker's per-pair step, and
/// what a first ask schedules so an app does not wait a whole cadence for it.
///
/// Rule 2 first: an unenrolled region is not fetched and nothing is recorded,
/// so it never reads as stale — a channel that does not exist cannot go quiet.
pub async fn refresh_relay_one(
    state: &AppState,
    region: &RegionCode,
    payload_kind: &str,
    registry: &RegionRegistry,
) {
    if registry.region(region).is_none() {
        return;
    }
    match fetch_bundle(region, payload_kind).await {
        Ok((artifact, evidence)) => {
            // An artifact the log served under this pair's path that names
            // another pair is not this pair's answer, whatever it verifies as.
            if &artifact.region != region || artifact.payload_kind != payload_kind {
                let why = format!(
                    "the log served ({}, {}) under ({region}, {payload_kind})'s path",
                    artifact.region, artifact.payload_kind
                );
                tracing::warn!("region relay: {why}");
                state
                    .db
                    .record_relay_attempt(region, payload_kind, true, &why)
                    .await;
                return;
            }
            match ingest_relay_artifact(state, artifact, evidence, registry).await {
                Ok(Ingested::Accepted { sequence }) => {
                    tracing::info!(%region, payload_kind, sequence, "region relay: cached a new artifact");
                }
                // A refusal is a reached channel — not staleness — and worth
                // surfacing: a mirror serving artifacts that keep failing is the
                // shape a compromised one has.
                Ok(Ingested::Refused(why)) => {
                    tracing::warn!(%region, payload_kind, "region relay: refused an artifact: {why}");
                    state
                        .db
                        .record_relay_attempt(region, payload_kind, true, &why)
                        .await;
                }
                Err(e) => {
                    let why = format!("{e:#}");
                    tracing::error!(%region, payload_kind, "region relay: ingest error: {why}");
                    state
                        .db
                        .record_relay_attempt(region, payload_kind, false, &why)
                        .await;
                }
            }
        }
        Err(e) => {
            let why = format!("{e:#}");
            tracing::warn!(%region, payload_kind, "region relay: fetch failed: {why}");
            state
                .db
                .record_relay_attempt(region, payload_kind, false, &why)
                .await;
        }
    }
}

/// One relay pass: retire what no longer verifies, then refill every
/// (region, kind) an app has asked for.
///
/// **De-listing retires the cached envelope** — the third ruling this module
/// holds, applied to the relay. Each cached envelope is re-verified against the
/// current registry (`last_accepted_sequence: None`: the cached artifact *is* the
/// accepted one); one that no longer verifies is Fauna de-listing its authority,
/// and relaying it on would hand every app a document from a party no longer
/// recognised. The demand row stays, so a re-enrolled region is fetched again.
/// The first tick after boot runs this, which is what a registry change across
/// an upgrade needs.
pub async fn refresh_relay_once(state: &AppState, registry: &RegionRegistry) {
    let demand = match state.db.relay_demand().await {
        Ok(demand) => demand,
        Err(e) => {
            tracing::warn!("region relay: cannot read the demand list: {e:#}");
            return;
        }
    };
    for (region, payload_kind) in demand {
        match state.db.relay_artifact(&region, &payload_kind).await {
            Ok(Some(cached)) => {
                if let Some(envelope) = cached.envelope {
                    let still_verifies = fauna_protocol::decode_strict::<PolicyArtifact>(&envelope)
                        .map_err(anyhow::Error::from)
                        .and_then(|artifact| {
                            verify_artifact(
                                artifact,
                                registry,
                                crate::db::now_epoch_secs().max(0) as u64,
                                None,
                            )
                            .map_err(anyhow::Error::from)
                        });
                    if let Err(e) = still_verifies {
                        tracing::warn!(
                            %region, payload_kind,
                            "relayed region artifact no longer verifies ({e}); retiring it"
                        );
                        // Before the write, for `ingest_relay_artifact`'s reason.
                        let binds =
                            binds_the_public_render(state, &region, &payload_kind, registry).await;
                        match state
                            .db
                            .retire_relay_artifact(&region, &payload_kind, binds)
                            .await
                        {
                            Ok(()) => {
                                if binds {
                                    rerender_public_sites(state).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("region relay: could not retire an artifact: {e:#}");
                            }
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("region relay: cannot read the cache: {e:#}"),
        }
        refresh_relay_one(state, &region, &payload_kind, registry).await;
    }
}

/// The registry the relay's first-ask refill verifies against — the seam the
/// relay handler shares with the worker, so the two cannot drift.
pub(crate) fn relay_registry() -> RegionRegistry {
    registry_snapshot()
}

/// The registry the app relay's two demand doors — `region_relay::
/// artifact_get_handler` and [`demand_situs_content_policies`] — verify
/// enrollment against: `state.region_registry_override` when a test installed
/// one (`AppState::install_region_registry_for_test`), else the real
/// [`relay_registry`]. Deliberately **not** consulted by the refill a first
/// ask schedules (`region_relay::schedule_refill`, `refresh_relay_one`) —
/// those keep reading `relay_registry()` directly, so an enrolled override
/// never causes a real fetch to [`REGION_LOG_BASE_URL`] under test.
pub(crate) fn demand_door_registry(state: &AppState) -> RegionRegistry {
    state
        .region_registry_override
        .clone()
        .unwrap_or_else(relay_registry)
}

/// Spawn the periodic refresh (§ Publication and signing — *"update cadence is
/// pull-based"*).
///
/// The first tick is **not** skipped: it doubles as the at-boot re-fold, which
/// is what applies a stored artifact's bounds after an upgrade and what retires
/// a de-listed authority's document. On a deployment that declares no region —
/// every deployment today — the tick reads one row and returns.
pub fn spawn_region_refresh(state: std::sync::Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        REFRESH_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move { refresh_once(&state).await }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::region_tier::RefreshState;

    fn state(checked_at: i64, first_attempted_at: i64, reached_at: Option<i64>) -> RefreshState {
        RefreshState {
            checked_at,
            ok: reached_at == Some(checked_at),
            detail: None,
            first_attempted_at,
            reached_at,
        }
    }

    /// **The regression this row exists to fix**: a channel that never
    /// answers, but is retried every cadence (so `checked_at` keeps moving),
    /// must go stale — the attempts alone must never keep it looking fresh.
    #[test]
    fn an_unreachable_channel_goes_stale_while_the_worker_keeps_trying() {
        let stale_after = STALE_AFTER.as_secs() as i64;
        let now = 100 * stale_after;

        let (stale, last_checked_at) =
            refresh_staleness(&state(now - 60, now - stale_after - 1, None), now);
        assert!(
            stale,
            "a failing worker must not keep an unreached channel looking fresh"
        );
        assert_eq!(last_checked_at, None);
    }

    #[test]
    fn a_channel_reached_recently_is_not_stale_however_recent_the_last_attempt() {
        let stale_after = STALE_AFTER.as_secs() as i64;
        let now = 100 * stale_after;

        let (stale, last_checked_at) =
            refresh_staleness(&state(now, now - 10 * stale_after, Some(now - 60)), now);
        assert!(!stale);
        assert_eq!(last_checked_at, Some((now - 60) as u64));
    }

    /// Reached once, long ago; every attempt since has failed (`checked_at`
    /// recent, `reached_at` unmoved): the channel has gone quiet and must warn.
    #[test]
    fn a_channel_reached_long_ago_and_not_since_goes_stale() {
        let stale_after = STALE_AFTER.as_secs() as i64;
        let now = 100 * stale_after;

        let (stale, last_checked_at) = refresh_staleness(
            &state(
                now - 60,
                now - 10 * stale_after,
                Some(now - stale_after - 1),
            ),
            now,
        );
        assert!(stale);
        assert_eq!(last_checked_at, Some((now - stale_after - 1) as u64));
    }

    #[test]
    fn a_never_reached_channel_tried_only_recently_is_not_stale() {
        let stale_after = STALE_AFTER.as_secs() as i64;
        let now = 100 * stale_after;

        let (stale, last_checked_at) = refresh_staleness(&state(now, now, None), now);
        assert!(!stale, "a pair asked for a moment ago has not gone quiet");
        assert_eq!(last_checked_at, None);
    }
}
