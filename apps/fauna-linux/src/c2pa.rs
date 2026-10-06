//! Thin wrapper around the `c2pa` crate for reading provenance manifests.
//!
//! Deliberately uncalled (`docs/goal/ui/media.md`'s linux bullet, registry
//! `c2pa`): a *different* mechanism — local-file upload-time detection — from
//! the shared `fauna_media::process::detect_c2pa` pipeline linux's upload path
//! actually uses. Kept for a future local re-verification pass, not dead code
//! to remove.

use std::path::Path;

/// Summary of a C2PA manifest embedded in a media file.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ProvenanceInfo {
    pub signer: String,
    pub signing_date: String,
    pub claim_generator: String,
    pub is_valid: bool,
}

/// Try to read C2PA provenance from a local file.
///
/// Returns `None` if the file has no C2PA manifest or if reading fails.
#[allow(dead_code)]
pub fn read_provenance(path: &Path) -> Option<ProvenanceInfo> {
    let reader = c2pa::Reader::from_context(c2pa::Context::default())
        .with_file(path)
        .ok()?;
    let manifest = reader.active_manifest()?;

    let signer = manifest
        .signature_info()
        .and_then(|s| s.issuer.as_deref().map(str::to_owned))
        .unwrap_or_else(|| manifest.claim_generator().unwrap_or("unknown").to_owned());

    let signing_date = manifest
        .signature_info()
        .and_then(|s| s.time.as_deref().map(str::to_owned))
        .unwrap_or_default();

    let claim_generator = manifest.claim_generator().unwrap_or("unknown").to_owned();

    let is_valid = reader.validation_state() != c2pa::ValidationState::Invalid;

    Some(ProvenanceInfo {
        signer,
        signing_date,
        claim_generator,
        is_valid,
    })
}
