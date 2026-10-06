//! One-time admin claim-code file machinery.
//!
//! On first boot the Docker entrypoint (or `main.rs`) writes a claim code
//! (`fauna_core::claim_code` format) to `/data/claim-code` (or
//! `<db_dir>/claim-code`). The first
//! caller with a valid claim code becomes the nest admin via the pre-identity
//! WS-RPC kind `fauna.auth.claim_admin` (`claim_handlers`, ceremony in
//! `claim_core`); the file is deleted on success, so a second attempt fails
//! with `fauna.auth.already_claimed`. This module owns only the claim-code
//! file path/generation helpers the WS handler and startup consume — the
//! `POST /api/v1/claim-admin` HTTP twin was removed in S4d.

use crate::routes::AppState;

/// Resolve the path to the claim-code file.
/// Uses the parent directory of `db_path` from the node config, falling back
/// to `/data/claim-code` when the db lives in the current directory.
pub(crate) fn claim_code_path(state: &AppState) -> std::path::PathBuf {
    let db_path = std::path::Path::new(&state.config.nest.db_path);
    let data_dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("/data"));
    data_dir.join("claim-code")
}

/// The claim-code path derived directly from a configured `db_path` — for
/// startup, before an `AppState` exists. Mirrors `claim_code_path`.
pub fn claim_code_path_for_db(db_path: &str) -> std::path::PathBuf {
    let data_dir = std::path::Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("/data"));
    data_dir.join("claim-code")
}

/// Generate a fresh claim code in display form (grouped, hyphenated).
/// Delegates to `fauna_core::claim_code::generate` — the single source of truth
/// for the claim-code format, shared with the client provisioning flow
/// (`fauna_provisioning::generate_claim_code`) so a nest-minted and a
/// client-minted code are byte-identical. `ensure_claim_code_at` (first boot)
/// and `factory_reset` (re-onboarding) both mint codes this way.
pub fn generate_claim_code() -> String {
    fauna_core::claim_code::generate()
}

/// Standalone version that takes a db_path directly, for use during startup
/// before AppState is constructed.
pub fn ensure_claim_code_at(db_path: &str) {
    let data_dir = std::path::Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("/data"));
    let path = data_dir.join("claim-code");
    if path.exists() {
        tracing::info!("Claim code file exists at {}", path.display());
        return;
    }
    let code = generate_claim_code();
    if let Err(e) = std::fs::write(&path, &code) {
        tracing::error!("Failed to write claim code to {}: {e}", path.display());
        return;
    }
    // Redaction rule (observability.md § Persistence & privacy): a claim code is
    // secret material — it must NOT land in the ring / on-disk log / admin Logs
    // page. The admin reads it from the console banner (stderr, which
    // bypasses the `fauna-log` ring — `print_claim_banner`, which runs later in
    // boot once the nest identity is final); the ring only records that one was
    // minted.
    tracing::info!(
        "Claim code generated; the console banner follows once the nest identity is loaded"
    );
}

/// The claim banner's lines — code plus the `fauna://claim` URI carrying this
/// nest's identity — or `None` when the box is claimed (no claim-code file).
///
/// Pure on the file contents + identity so the gating is unit-testable;
/// [`print_claim_banner`] is the stderr shell. The URI is the admin's
/// out-of-band channel root (the
/// self-hosted public-domain row): a hand-deployed nest is
/// domainless at claim time, so no DNS `self=` record and no injected
/// `deployment_seed` exists — the console print beside the code is the one
/// place its identity can reach the admin out of band. A client that
/// receives the URI pins the identity BEFORE sending the code, so an on-path
/// interceptor can no longer harvest it.
pub fn claim_banner_lines(db_path: &str, nest_actor_id: &[u8; 32]) -> Option<Vec<String>> {
    let path = claim_code_path_for_db(db_path);
    let code = std::fs::read_to_string(&path).ok()?;
    let code = code.trim().to_string();
    if code.is_empty() {
        return None;
    }
    let uri = fauna_core::claim_code::claim_uri(&code, &hex::encode(nest_actor_id));
    Some(vec![
        "╔══════════════════════════════════════════════════════════════╗".into(),
        "║  Enter this code in your Fauna app to become admin.          ║".into(),
        "║  This code is single-use and will be deleted after claim.    ║".into(),
        "╚══════════════════════════════════════════════════════════════╝".into(),
        String::new(),
        format!("  CLAIM CODE:  {code}"),
        String::new(),
        format!("  CLAIM URI:   {uri}"),
        "  (Paste the URI instead of the bare code and your client will also".into(),
        "   verify this nest's identity, protecting the claim from".into(),
        "   interception.)".into(),
    ])
}

/// Print the claim banner on **every boot while the box is unclaimed** (the
/// claim-code file exists). Runs after the deployment-keypair reconcile — the
/// earliest point the identity in the URI is final; `ensure_claim_code_at`
/// (pre-DB) only mints the file. Printing every unclaimed boot also closes the
/// old gap where a restart never re-showed the code.
pub fn print_claim_banner(db_path: &str, nest_actor_id: &[u8; 32]) {
    let Some(lines) = claim_banner_lines(db_path, nest_actor_id) else {
        return;
    };
    eprintln!();
    for line in lines {
        eprintln!("{line}");
    }
    eprintln!();
}

/// Boot reconcile: an already-claimed nest should not leave a stale claim code
/// lying around (best-effort hygiene).
///
/// `ensure_claim_code_at` runs at startup *before* the db is open, so it gates
/// only on the claim-code file's existence — it regenerates the code whenever
/// the file is absent, including on **every restart of an already-claimed box**
/// (the successful claim deleted the file, so the next boot mints a fresh one).
/// `setup.status.claimed` and the `already_claimed` gate are now **DB-positive**
/// (keyed on an admin row, not claim-code absence), so a resurrected code no
/// longer wedges the claimed-state; this reconcile is just hygiene that removes
/// the unusable secret file. Once the db is open and we can see an admin already
/// exists, delete any resurrected code.
///
/// Pure on `admin_exists` (the caller passes `db.admin_count() > 0` — whether an
/// *admin* actor exists, NOT merely any user), so the file-lifecycle decision is
/// unit-testable without a db. Keyed on an admin, not any user, so a
/// half-completed claim (a `users` row written before `add_admin_actor`
/// succeeded — e.g. a crash mid-ceremony) keeps its still-live code rather than
/// having it deleted, leaving the recovery retry possible. A factory reset
/// deletes the admin *before* minting a new code, so `admin_exists` is then
/// false and the freshly-minted code is correctly preserved.
pub fn reconcile_claim_code(admin_exists: bool, claim_code_path: &std::path::Path) {
    if !admin_exists || !claim_code_path.exists() {
        return;
    }
    match std::fs::remove_file(claim_code_path) {
        Ok(()) => tracing::info!(
            "Removed resurrected claim code at {} (an admin already exists; the box is claimed)",
            claim_code_path.display()
        ),
        Err(e) => tracing::warn!(
            "Failed to remove resurrected claim code at {}: {e}",
            claim_code_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generates_claim_code_when_missing() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nest.db").to_str().unwrap().to_string();
        ensure_claim_code_at(&db_path);
        let code_path = dir.path().join("claim-code");
        assert!(code_path.exists(), "claim-code file must be created");
        let code = std::fs::read_to_string(&code_path).unwrap();
        // Ambiguity-free base32, displayed grouped — exact format and length
        // owned by `fauna_core::claim_code` (see its unit tests, which pin both
        // the length and the entropy floor). Here we assert only that the file
        // carries a grouped, normalizable code, deriving the expected length
        // from the generator rather than restating it: this assertion was
        // pinned at 26 and went red when the code shortened to 8 chars, which nothing caught because no CI runs these tests.
        assert!(code.contains('-'), "claim code must be grouped: {code}");
        assert_eq!(
            fauna_core::claim_code::normalize(&code).len(),
            fauna_core::claim_code::normalize(&fauna_core::claim_code::generate()).len(),
            "claim code must normalize to the generator's length: {code}"
        );
    }

    #[test]
    fn preserves_existing_claim_code() {
        let dir = TempDir::new().unwrap();
        let code_path = dir.path().join("claim-code");
        std::fs::write(&code_path, "AABBCC").unwrap();
        let db_path = dir.path().join("nest.db").to_str().unwrap().to_string();
        ensure_claim_code_at(&db_path);
        let code = std::fs::read_to_string(&code_path).unwrap();
        assert_eq!(code, "AABBCC", "existing code must not be overwritten");
    }

    #[test]
    fn reconcile_deletes_resurrected_code_when_admin_exists() {
        // A claimed box (admin_exists) whose claim code was regenerated on a
        // restart must have it removed, so `setup.status.claimed` reads true.
        let dir = TempDir::new().unwrap();
        let code_path = dir.path().join("claim-code");
        std::fs::write(&code_path, "RESURRECTED").unwrap();
        reconcile_claim_code(true, &code_path);
        assert!(
            !code_path.exists(),
            "an already-claimed box must not retain a claim code"
        );
    }

    #[test]
    fn reconcile_preserves_code_on_unclaimed_box() {
        // No admin yet (fresh or just-factory-reset): the claim code is live and
        // must survive boot.
        let dir = TempDir::new().unwrap();
        let code_path = dir.path().join("claim-code");
        std::fs::write(&code_path, "LIVE-CODE").unwrap();
        reconcile_claim_code(false, &code_path);
        assert_eq!(
            std::fs::read_to_string(&code_path).unwrap(),
            "LIVE-CODE",
            "an unclaimed box must keep its claim code"
        );
    }

    #[test]
    fn reconcile_is_noop_when_no_code_present() {
        // Already-claimed and already-reconciled (no file): must not panic.
        let dir = TempDir::new().unwrap();
        let code_path = dir.path().join("claim-code");
        reconcile_claim_code(true, &code_path);
        assert!(!code_path.exists());
    }

    /// The banner exists exactly while the box is unclaimed, and its URI line
    /// carries this nest's identity — the admin's out-of-band Axis-2
    /// root.
    #[test]
    fn claim_banner_prints_only_while_unclaimed_and_carries_the_identity_uri() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nest.db");
        let db_path = db_path.to_str().unwrap();
        let id = [0xCD_u8; 32];

        // No claim-code file (claimed box) → no banner.
        assert!(claim_banner_lines(db_path, &id).is_none());

        // Unclaimed box → the banner carries both the code and the URI.
        std::fs::write(dir.path().join("claim-code"), "K7Q2-M9XJ\n").unwrap();
        let lines = claim_banner_lines(db_path, &id).expect("unclaimed ⇒ banner");
        let joined = lines.join("\n");
        assert!(joined.contains("CLAIM CODE:  K7Q2-M9XJ"), "{joined}");
        let expected_uri = fauna_core::claim_code::claim_uri("K7Q2-M9XJ", &hex::encode(id));
        assert!(joined.contains(&expected_uri), "{joined}");
        // The URI round-trips through the shared parser to the same identity.
        let parsed = fauna_core::claim_code::parse_claim_input(&expected_uri).unwrap();
        assert_eq!(parsed.code, "K7Q2-M9XJ");
        assert_eq!(
            parsed.nest_actor_id.as_deref(),
            Some(hex::encode(id).as_str())
        );

        // An empty file (half-written) prints nothing rather than a code-less banner.
        std::fs::write(dir.path().join("claim-code"), "\n").unwrap();
        assert!(claim_banner_lines(db_path, &id).is_none());
    }
}
