//! WASM community-labeler execution FFI (labeler-registry design §6).
//!
//! Exposes the shared `fauna-labeler` runtime across the UniFFI boundary so a
//! capability-holder content-processor (a role of the Go `fauna-mail-bridge` MDA)
//! can run a subscribed labeler's `label()` over unsealed content **off the
//! nest** — in encrypted mode the nest holds no content key, which is the whole
//! point of the capability plane (`content-moderation-and-ranking.md` § Tier-3).
//!
//! Two exports, split by concern so the holder does no crypto, no label
//! decoding, and no limit arithmetic itself:
//!
//! - [`mail_to_labeler_input_bare`] — content-kind-specific mapping (mail →
//!   [`LabelerPostInput`]), BARE-encoded for the `label()` ABI.
//! - [`run_wasm_labeler_score`] — the generic, input-agnostic score path:
//!   verify (B1) → clamp limits (F4) → run → decode `Vec<Label>` → one per-mille
//!   score. A future post-content holder reuses it with a post-mapped input.
//!
//! ## The `label()` ABI is BARE (design §6, revision-history 2026-07-07)
//!
//! Input and output cross the WASM boundary as **BARE** (`serde_bare`), NOT the
//! house dag-cbor: dag-cbor forbids floats and [`Label`](fauna_core::scoring::Label)`.confidence` is an
//! `f64`. BARE is a deterministic, schema-driven binary format — every labeler
//! author encodes/decodes the same bytes. The module writes/reads the exact
//! serde shape of the `fauna_core::scoring` types.

use crate::FfiError;
use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_core::scoring::{
    AlgorithmLabeler, LabelerInput, LabelerOutput, LabelerPostInput, ScorerLimits,
    TextModelArtifact, build_text_model_artifact, labels_to_score_entry, sign_labeler_metadata,
};

/// Placeholder author for content whose sender is not a Fauna actor (an external
/// mail sender has no `ActorId`). Mail-kind labelers declare `needs_author=false`
/// and ignore it; the field is required by [`LabelerPostInput`] so v1 supplies a
/// zero id (design §6, D7 mapping).
const PLACEHOLDER_AUTHOR: ActorId = ActorId([0u8; 32]);

/// Map one raw RFC-5322 mail message to the BARE-encoded [`LabelerPostInput`]
/// the `label()` ABI expects (design §6, D7 — the mail content-kind mapping).
///
/// `text` = subject + a blank line + the body, **CRLF-normalized to LF and
/// trailing-whitespace-trimmed** so every labeler author sees uniform text
/// regardless of the sender's line-ending convention (mail transport CRLF is an
/// artifact, not content); `None` when both subject and body are empty.
/// `hashtags` empty, `has_media=false`, `author` the zero placeholder (external
/// senders have no `ActorId`; mail-kind labelers set `needs_author=false`).
/// Attachment/media mapping is deferred.
///
/// Returns bytes on purpose: a `#[uniffi::export]` returning a `fauna_core` type
/// makes uniffi-bindgen-go emit an uncompilable bare `import "fauna_core"` (the
/// justfile `mail-bridge-ffi-check` value-format footgun). The holder passes
/// these bytes straight to [`run_wasm_labeler_score`].
#[uniffi::export]
pub fn mail_to_labeler_input_bare(raw_rfc5322: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let parsed =
        fauna_mail::parser::parse_rfc5322(&raw_rfc5322).map_err(|e| FfiError::General {
            msg: format!("labeler mail parse: {e:?}"),
        })?;

    let subject = parsed.subject.unwrap_or_default();
    let combined = match (subject.is_empty(), parsed.body_text.is_empty()) {
        (true, true) => String::new(),
        (false, true) => subject,
        (true, false) => parsed.body_text,
        (false, false) => format!("{subject}\n\n{}", parsed.body_text),
    };
    // Normalize transport CRLF → LF, trim the trailing newline artifact.
    let normalized = combined.replace("\r\n", "\n");
    let trimmed = normalized.trim_end();
    let text = (!trimmed.is_empty()).then(|| trimmed.to_string());

    let input = LabelerPostInput {
        text,
        hashtags: Vec::new(),
        has_media: false,
        media_type: None,
        duration_ms: None,
        author: PLACEHOLDER_AUTHOR,
    };
    fauna_labeler::encode_labeler_input(&input).map_err(|e| FfiError::General {
        msg: format!("{e:#}"),
    })
}

/// Run a subscribed labeler over one item's BARE-encoded [`LabelerPostInput`] and
/// return its single tier-3 bus score (per-mille, `[0,1000]`).
///
/// This is the whole holder-side execution boundary in one call, so the Go MDA
/// drain does no crypto and cannot skip a guard. The boundary itself — decode,
/// the B1 re-verify, the F4 clamp, the sandboxed run, the BARE decode — is
/// [`fauna_labeler::run_published_labeler_bare`], the one copy every position
/// that runs a `wasm` labeler shares (a community room's home nest calls the
/// same function); this export adds only the mapping to the primary per-mille
/// score via `labels_to_score_entry` (empty output → 0; NaN/out-of-range
/// confidence is canonicalized there).
///
/// `expected_labeler_id` is the 32-byte id the holder parsed from the
/// `labeler:<hex>` factor it is draining — the labeler the owner's grant
/// licenses. The shared boundary refuses a module whose signed
/// `algorithm_id` is any other key, so a nest answering `inspect` for that
/// id with a module of its own choosing runs nothing.
///
/// The caller stamps the `ScoreEntry` factor / tier / version — the score value
/// is factor-independent, so this returns just the `i64` (also avoiding the
/// uniffi-bindgen-go `fauna_core`-type footgun).
///
/// The mail holder holds no attachment bytes (its attachment mapping is
/// deferred, [`mail_to_labeler_input_bare`]), so a module that declared
/// `needs_attachment_bytes` reads the shape it declared with an **empty**
/// facet here — appended at the shared boundary, never a silent v1
/// (`content-moderation-and-ranking.md` § Tier-3 → *The attachment facet*).
#[uniffi::export]
pub fn run_wasm_labeler_score(
    metadata_blob: Vec<u8>,
    wasm_bytes: Vec<u8>,
    expected_labeler_id: Vec<u8>,
    input_bare: Vec<u8>,
) -> Result<i64, FfiError> {
    let expected: [u8; 32] =
        expected_labeler_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::General {
                msg: format!(
                    "expected_labeler_id must be 32 bytes, got {}",
                    expected_labeler_id.len()
                ),
            })?;
    // Empty output = "detected nothing" → no labels → score 0.
    let labels = fauna_labeler::run_published_labeler_bare(
        &metadata_blob,
        &wasm_bytes,
        &ActorId(expected),
        &input_bare,
        &[],
    )
    .map_err(|e| FfiError::General {
        msg: format!("{e:#}"),
    })?;
    // Factor/version are the caller's to stamp; the per-mille score is not.
    Ok(labels_to_score_entry(&labels, String::new(), 0).score)
}

/// Build a **signed** `AlgorithmLabeler` metadata blob for `fauna.labelers.publish`
/// — the publisher-side twin of the verify at both boundaries (nest publish gate
/// + holder re-verify). This is the shared-Rust seam the (deferred) client
/// publish UI will call; the tier_3 drain test drives it via the Go seal-helper
/// `publish-labeler` mode.
///
/// Given the publisher's 32-byte Ed25519 signing seed and the WASM module bytes,
/// it: sets `algorithm_id` = the seed's verifying key (public-key-is-identity),
/// computes `wasm_hash` (BLAKE3 `ContentHash`) + `wasm_size`, and signs over the
/// canonical dag-cbor with `signature` zeroed via
/// [`fauna_core::scoring::sign_labeler_metadata`] — so the produced blob passes
/// `validate_labeler_publish` → `verify_labeler_metadata` by construction.
/// Returns the canonical-CBOR `metadata_blob` bytes the `PublishLabelerRequest`
/// carries (bytes, not a `fauna_core` type, to avoid the uniffi-bindgen-go
/// value-format import footgun — same as the sibling exports).
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn build_signed_labeler_metadata(
    signing_seed: Vec<u8>,
    wasm_bytes: Vec<u8>,
    version: u64,
    needs_text: bool,
    needs_hashtags: bool,
    needs_media_metadata: bool,
    needs_author: bool,
    needs_attachment_bytes: bool,
    max_memory_bytes: u64,
    max_cpu_microseconds: u64,
    updated_at: u64,
) -> Result<Vec<u8>, FfiError> {
    let seed: [u8; 32] = signing_seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "labeler signing seed must be 32 bytes, got {}",
                signing_seed.len()
            ),
        })?;
    let sk = SigningKey::from_bytes(&seed);
    let meta = AlgorithmLabeler {
        algorithm_id: ActorId(sk.verifying_key().to_bytes()),
        version,
        wasm_hash: fauna_core::encoding::content_hash(&wasm_bytes),
        wasm_size: wasm_bytes.len() as u64,
        input_schema: LabelerInput {
            needs_text,
            needs_hashtags,
            needs_media_metadata,
            needs_author,
            needs_attachment_bytes,
        },
        output_schema: LabelerOutput::default(),
        resource_limits: ScorerLimits {
            max_memory_bytes,
            max_cpu_microseconds,
        },
        updated_at: Timestamp(updated_at),
        signature: Vec::new(), // overwritten by the sign
    };
    let signed = sign_labeler_metadata(&sk, meta).map_err(|e| FfiError::General {
        msg: format!("labeler metadata sign: {e}"),
    })?;
    fauna_core::encoding::canonical_encode(&signed).map_err(|e| FfiError::General {
        msg: format!("labeler metadata encode: {e}"),
    })
}

/// One vocabulary row of a `text-model` artifact minted by
/// [`encode_text_model_artifact_at_version`].
///
/// Deliberately **not** `crate::personalization::FfiPublishNgram`, though the
/// fields are identical: `personalization` is gated default-on precisely so the
/// Go mail-bridge's `--no-default-features` build drops it (there is no
/// Personalization UI on a bridge), while `labeler` is a feature that build
/// *does* enable. Reaching across that split would drag the whole
/// personalization surface into the Go bindings — the one thing its gate exists
/// to prevent. The duplication is the feature boundary made visible, not drift.
#[derive(uniffi::Record)]
pub struct FfiTextModelNgram {
    /// The n-gram itself.
    pub ngram: String,
    /// Distinct *more like this* example documents it occurred in.
    pub more: u32,
    /// Distinct *less like this* example documents it occurred in.
    pub less: u32,
}

/// Encode a canonical dag-cbor `TextModelArtifact` at a **caller-chosen**
/// `version` — the one artifact production is structurally unable to mint.
///
/// ⚠ TEST-ONLY IN PRACTICE — MUST NOT GAIN A PRODUCTION CALLER, and the reason
/// is the whole point of the feature it tests. `version` is the subscriber's
/// **tokenizer contract**, so
/// [`fauna_core::scoring::build_text_model_artifact`] stamps
/// [`fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION`] itself and deliberately
/// refuses it as a parameter (the `updated_at` lesson). A publisher that could
/// choose it could claim a contract its counts were never built under. The
/// accepted test-minting-surface shape is `seal_mls_snapshot_blob`'s: a
/// marked export here, with the `dark-rail-audit-check` merge gate as the
/// backstop: it reds the moment any non-test file calls an export carrying
/// this marker.
///
/// It exists because the "needs a newer app" badge
/// (`fauna_core::format::text_model_needs_newer_app`) can only be *proven* by a
/// labeler whose artifact this build does not implement, and **no app UI can
/// publish one** — so the tier_3 journey mints it here and publishes it through
/// the existing raw `fauna.labelers.publish` path (convention 8's fixture-setup
/// carve-out: the mutation under test is the *render*, not the publish).
///
/// The canonical form is still the builder's obligation, not the caller's: this
/// delegates sort/dedup/validate to `build_text_model_artifact` wholesale and
/// then overwrites the single field production may never choose, so a fixture
/// cannot drift from the shape the nest gate enforces. `version` 0 is refused
/// here rather than at the nest — 0 is an *absent* claim, not a future one
/// (`validate_text_model_artifact`'s `ZeroVersion`).
#[uniffi::export]
pub fn encode_text_model_artifact_at_version(
    version: u16,
    name: Option<String>,
    more_docs: u32,
    less_docs: u32,
    ngrams: Vec<FfiTextModelNgram>,
) -> Result<Vec<u8>, FfiError> {
    if version == 0 {
        return Err(FfiError::General {
            msg: "text-model artifact version must be non-zero (0 is an absent claim)".into(),
        });
    }
    // Build through the production path first: it owns the sort, the
    // last-wins dedup, and the validate the nest gate re-runs.
    let canonical = build_text_model_artifact(
        name.as_deref(),
        more_docs,
        less_docs,
        ngrams
            .into_iter()
            .map(|n| (n.ngram, n.more, n.less))
            .collect(),
    )
    .map_err(|e| FfiError::General {
        msg: format!("text-model artifact build: {e}"),
    })?;
    let mut artifact: TextModelArtifact = fauna_core::encoding::canonical_decode(&canonical)
        .map_err(|e| FfiError::General {
            msg: format!("text-model artifact decode: {e}"),
        })?;
    // The one deviation, and the only one this surface exists for.
    artifact.version = version;
    fauna_core::encoding::canonical_encode(&artifact).map_err(|e| FfiError::General {
        msg: format!("text-model artifact encode: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::scoring::{Label, LabelSource};

    /// Escape bytes for a WAT string literal (`\xx` per byte).
    fn wat_escape(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("\\{b:02x}")).collect()
    }

    /// A WAT labeler that ignores its input and returns a fixed, pre-baked
    /// `[4-byte LE len][payload]` output blob (the ABI shape `execute` reads).
    fn fixed_output_wasm(payload: &[u8]) -> Vec<u8> {
        let mut blob = (payload.len() as u32).to_le_bytes().to_vec();
        blob.extend_from_slice(payload);
        let escaped = wat_escape(&blob);
        format!(
            "(module (memory (export \"memory\") 2) \
             (data (i32.const 1024) \"{escaped}\") \
             (func (export \"alloc\") (param i32) (result i32) (i32.const 65536)) \
             (func (export \"label\") (param i32 i32) (result i32) (i32.const 1024)))"
        )
        .into_bytes()
    }

    /// The labeler id `signed_metadata_blob`'s fixture publishes under — the
    /// verifying key of its fixed seed, i.e. the id a drain parses from the
    /// factor `labeler:<hex>` it is scoring for.
    fn labeler_id() -> Vec<u8> {
        SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    /// A validly-signed `AlgorithmLabeler` metadata blob over `wasm_bytes` — via
    /// the public builder, so these tests also pin its output round-trips through
    /// `run_wasm_labeler_score`'s decode + verify.
    fn signed_metadata_blob(wasm_bytes: &[u8]) -> Vec<u8> {
        build_signed_labeler_metadata(
            [9u8; 32].to_vec(),
            wasm_bytes.to_vec(),
            1,                // version
            true,             // needs_text
            false,            // needs_hashtags
            false,            // needs_media_metadata
            false,            // needs_author
            false,            // needs_attachment_bytes
            16 * 1024 * 1024, // max_memory_bytes
            100_000,          // max_cpu_microseconds
            1,                // updated_at
        )
        .unwrap()
    }

    #[test]
    fn mail_to_labeler_input_bare_maps_subject_and_body() {
        let raw = b"From: a@b.com\r\nTo: c@d.com\r\nSubject: Cats\r\n\r\nI love my cat.\r\n";
        let bare = mail_to_labeler_input_bare(raw.to_vec()).unwrap();
        let input: LabelerPostInput = serde_bare::from_slice(&bare).unwrap();
        assert_eq!(input.text.as_deref(), Some("Cats\n\nI love my cat."));
        assert!(input.hashtags.is_empty());
        assert!(!input.has_media);
        assert_eq!(input.author, PLACEHOLDER_AUTHOR);
    }

    #[test]
    fn run_wasm_labeler_score_end_to_end() {
        // A labeler that emits one label at confidence 0.9 → 900 per-mille.
        let labels = vec![Label {
            category: "cat".to_string(),
            confidence: 0.9,
            source: LabelSource::TextAnalysis,
        }];
        let payload = serde_bare::to_vec(&labels).unwrap();
        let wasm = fixed_output_wasm(&payload);
        let metadata_blob = signed_metadata_blob(&wasm);
        let input_bare = mail_to_labeler_input_bare(b"Subject: hi\r\n\r\nbody".to_vec()).unwrap();

        let score = run_wasm_labeler_score(metadata_blob, wasm, labeler_id(), input_bare).unwrap();
        assert_eq!(score, 900);
    }

    #[test]
    fn run_wasm_labeler_score_empty_output_is_zero() {
        // A module returning length 0 (detected nothing) → score 0.
        let wasm = fixed_output_wasm(&[]);
        let metadata_blob = signed_metadata_blob(&wasm);
        let score = run_wasm_labeler_score(metadata_blob, wasm, labeler_id(), vec![]).unwrap();
        assert_eq!(score, 0);
    }

    /// The holder asked for labeler A and the nest served publisher B's
    /// validly signed module: self-consistent, so the B1 checks pass — and
    /// the expected-id check refuses it. Nothing runs.
    #[test]
    fn run_wasm_labeler_score_rejects_a_module_for_another_labeler() {
        let payload = serde_bare::to_vec(&Vec::<Label>::new()).unwrap();
        let wasm = fixed_output_wasm(&payload);
        let metadata_blob = signed_metadata_blob(&wasm);
        let other = SigningKey::from_bytes(&[10u8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let err = run_wasm_labeler_score(metadata_blob, wasm, other, vec![]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not the expected labeler"),
            "expected the expected-id refusal, got: {msg}"
        );
    }

    #[test]
    fn run_wasm_labeler_score_rejects_swapped_module_b1() {
        // The metadata is signed over the ORIGINAL bytes; a swapped module (same
        // length, one byte flipped) must be rejected by the pre-instantiation
        // verify (security review B1) — a compromised store can't smuggle a
        // different module past the subscriber's grant.
        let payload = serde_bare::to_vec(&Vec::<Label>::new()).unwrap();
        let wasm = fixed_output_wasm(&payload);
        let metadata_blob = signed_metadata_blob(&wasm);
        let mut swapped = wasm.clone();
        // Flip a byte in the data-segment region (keeps length, breaks the hash).
        let idx = swapped.len() / 2;
        swapped[idx] ^= 0xFF;
        let err = run_wasm_labeler_score(metadata_blob, swapped, labeler_id(), vec![]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("verify"),
            "expected a verify failure, got: {msg}"
        );
    }

    #[test]
    fn cat_labeler_wat_fixture_scores_cat_and_not() {
        // Pin the committed tier_3 fixture: it is published verbatim as
        // `wasm_bytes` in test_capability_labeler_drain.py, so its hand-written
        // BARE output bytes and cat-scan must speak the real `label()` ABI —
        // caught here (fast) rather than only in the expensive tier_3 drain.
        // `include_str!` also makes the fixture path a compile-time dependency so
        // a move/delete breaks the build, not silently the e2e run.
        let wat = include_str!("../../../tests/e2e-unified/fixtures/labeler/cat_labeler.wat");
        let wasm = wat.as_bytes().to_vec();
        let metadata_blob = signed_metadata_blob(&wasm);

        // The mail path the MDA holder actually runs: raw RFC-5322 → BARE input.
        let cat_input =
            mail_to_labeler_input_bare(b"Subject: hi\r\n\r\nI love my cat.".to_vec()).unwrap();
        let hit =
            run_wasm_labeler_score(metadata_blob.clone(), wasm.clone(), labeler_id(), cat_input)
                .unwrap();
        assert_eq!(hit, 900, "a cat mail must score 900 per-mille");

        let plain_input =
            mail_to_labeler_input_bare(b"Subject: hi\r\n\r\nthe weather is nice".to_vec()).unwrap();
        let miss = run_wasm_labeler_score(metadata_blob, wasm, labeler_id(), plain_input).unwrap();
        assert_eq!(miss, 0, "a non-cat mail must score 0 (empty Vec<Label>)");
    }

    fn ngram(text: &str, more: u32, less: u32) -> FfiTextModelNgram {
        FfiTextModelNgram {
            ngram: text.to_string(),
            more,
            less,
        }
    }

    #[test]
    fn encode_text_model_artifact_at_version_stamps_the_callers_version() {
        // The whole reason this surface exists: an artifact carrying a
        // tokenizer contract THIS build does not implement, which
        // `build_text_model_artifact` structurally cannot produce.
        let future = fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION + 7;
        let bytes = encode_text_model_artifact_at_version(
            future,
            Some("a model from the future".into()),
            3,
            0,
            vec![ngram("kittens", 3, 0)],
        )
        .unwrap();

        // It must still pass the nest's own gate — the version is the ONE rule
        // that gate deliberately does not enforce, so a fixture minted here is
        // publishable exactly as a real artifact is.
        let artifact = fauna_core::scoring::validate_text_model_artifact(&bytes).unwrap();
        assert_eq!(
            artifact.version, future,
            "the caller's version must survive"
        );
        assert!(
            !fauna_core::scoring::text_model_version_supported(artifact.version),
            "the fixture is pointless unless this build reads it as unsupported"
        );
        assert_eq!(artifact.name.as_deref(), Some("a model from the future"));
    }

    #[test]
    fn encode_text_model_artifact_at_version_still_canonicalizes() {
        // Sort/dedup/validate are delegated to the production builder, so a
        // fixture cannot drift from the canonical form the nest demands.
        let bytes = encode_text_model_artifact_at_version(
            9,
            None,
            4,
            1,
            vec![ngram("zebra", 3, 1), ngram("aardvark", 4, 0)],
        )
        .unwrap();
        let artifact = fauna_core::scoring::validate_text_model_artifact(&bytes).unwrap();
        let texts: Vec<&str> = artifact.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert_eq!(
            texts,
            vec!["aardvark", "zebra"],
            "handed over unsorted, it must come back strictly ascending"
        );
    }

    #[test]
    fn encode_text_model_artifact_at_version_refuses_version_zero() {
        // 0 is an ABSENT claim (a non-text-model artifact states no version), never
        // a future one — minting it would fake the wrong thing entirely.
        let err = encode_text_model_artifact_at_version(0, None, 3, 0, vec![ngram("a", 3, 0)]);
        assert!(err.is_err(), "version 0 must be refused at the mint");
    }

    #[test]
    fn encode_text_model_artifact_at_version_rejects_a_below_floor_vocabulary() {
        // The privacy floor is the production builder's, and delegating means a
        // fixture cannot quietly publish below it.
        let err = encode_text_model_artifact_at_version(9, None, 1, 0, vec![ngram("a", 1, 0)]);
        assert!(
            err.is_err(),
            "the distinct-document prune floor must still bite for a fixture"
        );
    }
}
