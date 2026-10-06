//! Test-only HTTP endpoints seeding the app region relay with a **synthetic**
//! content policy (`region-blocking.md` § The content plane → *What the build
//! owes in tests*: "a synthetic region artifact from a test-only registry
//! enrolling a synthetic authority … never a real region").
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard e2e nest
//! build and never in production. The production relay is filled only from the
//! compiled-in log for regions the compiled-in registry enrols — which is
//! nobody — so without this hook no tier_3 journey could drive the app side of
//! the plane at all.
//!
//! The hook signs one document with a fixed synthetic authority key and writes
//! the envelope straight into `region_relay_cache`, exactly where a refresh
//! would have put it, so the app's read goes through the real
//! `fauna.region.artifact.get` door. It answers with the synthetic registry
//! (hex of its canonical dag-cbor) — the app's `FAUNA_E2E_REGION_REGISTRY`
//! seed, which is the only registry its own verification will accept the
//! envelope under. The nest's own registry is untouched: the relay never
//! consults a registry on the read path.
//!
//! Consumer: `tests/e2e-unified/tests/test_region_content_policy.py`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use fauna_core::region_authority::{
    AuthorityKey, PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, RegionCode, RegionEntry,
    RegionRegistry, sign_artifact, verify_artifact,
};
use fauna_core::region_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION,
    REASON_DEFAULT_KEY, ScorerKind, scorer_factor,
};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// The synthetic authority's name, as the registry enrols it.
const SYNTHETIC_AUTHORITY: &str = "Synthetic Test Authority";
/// The synthetic authority's fixed signing seed — public on purpose; it signs
/// nothing a real registry would ever accept.
const SYNTHETIC_SEED: [u8; 32] = [0x5a; 32];

#[derive(Deserialize)]
struct SeedRule {
    /// `"block"` or `"collapse"`.
    verdict: String,
    reason: String,
    #[serde(default = "default_reason_code")]
    reason_code: String,
    /// A canonical label factor (`nsfw`, `spam`, …) this rule fires on.
    #[serde(default)]
    factor: Option<String>,
    /// Hex 32-byte item ids a bundled `list` scorer names at 1000 per-mille —
    /// the rule then fires on that scorer's `region:<region>/<name>` factor.
    #[serde(default)]
    content_ids: Vec<String>,
}

fn default_reason_code() -> String {
    "TEST-1".into()
}

#[derive(Deserialize)]
struct SeedRequest {
    region: String,
    sequence: u64,
    /// The grammar version; defaults to the one this build implements. A
    /// higher one drives the inert-and-says-so arm.
    #[serde(default)]
    version: Option<u32>,
    rules: Vec<SeedRule>,
}

fn synthetic_registry(region: &RegionCode) -> RegionRegistry {
    let key = ed25519_dalek::SigningKey::from_bytes(&SYNTHETIC_SEED);
    RegionRegistry {
        version: 1,
        regions: vec![RegionEntry {
            region: region.clone(),
            authority_name: SYNTHETIC_AUTHORITY.into(),
            official_domain: "authority.invalid".into(),
            parent: None,
            keys: vec![AuthorityKey {
                key_id: "k1".into(),
                public_key: key.verifying_key().to_bytes().to_vec(),
                enrolled_at: 0,
                retired_at: None,
            }],
        }],
    }
}

fn document(region: &RegionCode, req: &SeedRequest) -> Result<ContentPolicyDocument, String> {
    let mut rules = Vec::new();
    let mut scorers = Vec::new();
    for (index, seed) in req.rules.iter().enumerate() {
        let verdict = match seed.verdict.as_str() {
            "block" => ContentVerdict::Block,
            "collapse" => ContentVerdict::Collapse,
            other => return Err(format!("rule {index}: unknown verdict {other:?}")),
        };
        let factor = if seed.content_ids.is_empty() {
            seed.factor
                .clone()
                .ok_or_else(|| format!("rule {index}: neither factor nor content_ids"))?
        } else {
            let mut entries = Vec::new();
            for id in &seed.content_ids {
                let id = fauna_core::hex32::decode(id)
                    .map_err(|_| format!("rule {index}: content id {id:?} is not 32-byte hex"))?;
                entries.push((id, 1000));
            }
            entries.sort_by_key(|a| a.0);
            let name = format!("r{index}");
            let bytes = fauna_core::scoring::build_list_artifact(None, entries)
                .map_err(|e| format!("rule {index}: list: {e}"))?;
            scorers.push(BundledScorer {
                name: name.clone(),
                kind: ScorerKind::List,
                bytes,
                extra: Default::default(),
            });
            scorer_factor(region, &name)
        };
        rules.push(ContentRule {
            factor,
            min_permille: 500,
            verdict,
            reason_code: seed.reason_code.clone(),
            reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), seed.reason.clone())]),
            extra: Default::default(),
        });
    }
    Ok(ContentPolicyDocument {
        version: req.version.unwrap_or(GRAMMAR_VERSION),
        rules,
        scorers,
        extra: Default::default(),
    })
}

/// `POST /api/v1/test/region/content-policy` — sign a synthetic content policy
/// for `region` and seed the relay cache with it. Returns
/// `{"registry": <hex>, "authority_name": …}`.
async fn seed_content_policy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SeedRequest>,
) -> impl IntoResponse {
    let Ok(region) = RegionCode::parse(req.region.clone()) else {
        return ApiError::bad_request("malformed region code").into_response();
    };
    let doc = match document(&region, &req) {
        Ok(d) => d,
        Err(e) => return ApiError::bad_request(e).into_response(),
    };
    let payload = match fauna_protocol::encode_canonical(&doc) {
        Ok(b) => b.to_vec(),
        Err(e) => return ApiError::internal(format!("{e:?}")).into_response(),
    };
    let now = crate::db::now_epoch_secs().max(0) as u64;
    let key = ed25519_dalek::SigningKey::from_bytes(&SYNTHETIC_SEED);
    let artifact = match sign_artifact(
        PolicyArtifact {
            region: region.clone(),
            key_id: "k1".into(),
            sequence: req.sequence,
            issued_at: now,
            payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
            payload,
            sig: Vec::new(),
        },
        &key,
    ) {
        Ok(a) => a,
        Err(e) => return ApiError::internal(e.to_string()).into_response(),
    };
    let registry = synthetic_registry(&region);
    let verified = match verify_artifact(artifact, &registry, now, None) {
        Ok(v) => v,
        Err(e) => return ApiError::internal(e.to_string()).into_response(),
    };
    if let Err(e) = state.db.put_relay_artifact(&verified, None, false).await {
        return ApiError::internal(format!("{e:#}")).into_response();
    }
    let registry_bytes = match fauna_protocol::encode_canonical(&registry) {
        Ok(b) => b,
        Err(e) => return ApiError::internal(format!("{e:?}")).into_response(),
    };
    Json(json!({
        "registry": hex::encode(&registry_bytes),
        "authority_name": SYNTHETIC_AUTHORITY,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RetireRequest {
    region: String,
}

/// `POST /api/v1/test/region/content-policy/retire` — empty the relay's cell
/// for `region`, so the app's next ask answers "no document": what proves a
/// relaunched app blocks from its own device store rather than a fresh fetch.
async fn retire_content_policy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RetireRequest>,
) -> impl IntoResponse {
    let Ok(region) = RegionCode::parse(req.region) else {
        return ApiError::bad_request("malformed region code").into_response();
    };
    match state
        .db
        .retire_relay_artifact(&region, PAYLOAD_KIND_CONTENT_POLICY, false)
        .await
    {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => ApiError::internal(format!("{e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/region/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/region/content-policy",
            post(seed_content_policy),
        )
        .route(
            "/api/v1/test/region/content-policy/retire",
            post(retire_content_policy),
        )
}
