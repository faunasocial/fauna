//! Self-signed TLS cert synthesis + seal/fan-out, called by the WS-RPC
//! handler `fauna.bridges.provision_self_signed_cert`
//! (`bridge_routing_handlers.rs`). It was factored into this neutral module
//! so it survived the retirement of the transitional admin HTTP twin
//! (`POST /api/admin/local_domains/{domain}/self_signed_cert`, removed once
//! all consumers migrated to the WS-RPC kind — no-HTTP directive).
//!
//! Per `docs/goal/behavior/mail-bridge-lifecycle.md` § TLS provisioning,
//! "Admin-synthesized (self-signed)": the admin asks nest to synthesize a
//! self-signed cert for an active local mail domain; nest does the `rcgen`
//! synthesis + hands the `(cert, key)` pair to `Storage::store_acme_material`
//! (which seals + fans out a wrapped `TlsCertBlob` to every approved bridge
//! that has an x25519 pubkey AND writes the on-disk PEM) — the same code path
//! as the ACME finish. The result names which bridges were sealed-to and which
//! were skipped (no x25519 attested yet); the cert has a 90-day window and is
//! NOT auto-renewed (admin re-calls to refresh).
//!
//! Why a neutral module (not the HTTP route file): the WS-RPC twin is the
//! target-state surface (no-HTTP directive); when the HTTP route is later
//! retired, this synthesis logic survives unchanged behind the WS-RPC handler.

use crate::routes::AppState;
use crate::storage::StorageUnavailable;

/// Default validity window for the synthesized cert. Matches the expiry the
/// `store_acme_material` fan-out already encodes into the sealed
/// `TlsCertBundle` (90 days), so admins re-provision roughly quarterly.
pub(crate) const SELF_SIGNED_VALID_DAYS: i64 = 90;

/// Filename of the persisted self-signed **floor** private key under `acme_dir`.
/// The floor key is *stable* across cert re-synthesis: the served self-signed
/// cert's SPKI changes only when this key is deliberately rotated (the file
/// removed or replaced), never on a routine renewal. Stability is load-bearing
/// for **both** MUA re-prompt frequency (a click-through MUA pins the floor's
/// SPKI) **and** DANE/TLSA (a `_25._tcp` TLSA pins the served key) — see
/// `docs/goal/architecture/nest/tls-certificates.md` § A / § D.
///
/// Stored separately from the served `privkey.pem` so the floor key survives an
/// ACME real-cert swap (which overwrites `privkey.pem` with the CA key) and an
/// admin self-signed re-provision: the floor's identity stays durable and
/// unambiguous regardless of which cert is currently on the wire.
pub const FLOOR_KEY_FILENAME: &str = "floor-privkey.pem";

/// Load the persisted floor keypair from `acme_dir/floor-privkey.pem`, or
/// generate a fresh one and persist it (load-if-exists-else-create-and-persist).
/// The persisted key is what makes the self-signed floor's SPKI stable across
/// re-synthesis (every floor cert minted from it shares one public key), so a
/// routine renewal never churns the SPKI. A deliberate key rotation is "remove
/// `floor-privkey.pem`": the next call regenerates and persists a new key.
///
/// A corrupt/unreadable key file is treated as absent and regenerated — a fresh
/// SPKI (one MUA re-prompt / one TLSA re-publish) beats a dead floor.
pub fn load_or_create_floor_key(acme_dir: &std::path::Path) -> Result<rcgen::KeyPair, String> {
    let key_path = acme_dir.join(FLOOR_KEY_FILENAME);
    if let Ok(pem) = std::fs::read_to_string(&key_path)
        && let Ok(kp) = rcgen::KeyPair::from_pem(&pem)
    {
        return Ok(kp);
    }
    let kp = rcgen::KeyPair::generate().map_err(|e| format!("generate floor key: {e}"))?;
    std::fs::create_dir_all(acme_dir)
        .map_err(|e| format!("create acme dir {}: {e}", acme_dir.display()))?;
    // Write to a temp then rename so a crash mid-write never leaves a truncated
    // key file the next boot would reject (and then silently rotate the SPKI).
    let tmp = acme_dir.join(format!("{FLOOR_KEY_FILENAME}.tmp"));
    std::fs::write(&tmp, kp.serialize_pem()).map_err(|e| format!("write floor key: {e}"))?;
    std::fs::rename(&tmp, &key_path).map_err(|e| format!("persist floor key: {e}"))?;
    Ok(kp)
}

/// Synthesize a self-signed cert (CN = `common_name`, SANs = `san_list`) with a
/// freshly-generated key and return `(cert_pem, key_pem)`. Used by the admin
/// mail-listener path (`provision_self_signed_cert`), where each re-provision is
/// an explicit admin action and a fresh key is acceptable. The always-live
/// **floor** instead synthesizes from a persisted stable key — see
/// [`synthesize_self_signed_pem_with_key`] + [`load_or_create_floor_key`].
/// `rcgen` 0.13 is already a direct dep via the ACME CSR path.
pub fn synthesize_self_signed_pem(
    common_name: &str,
    san_list: Vec<String>,
) -> Result<(String, String), String> {
    let cert_key = rcgen::KeyPair::generate().map_err(|e| format!("generate key: {e}"))?;
    synthesize_self_signed_pem_with_key(common_name, san_list, &cert_key)
}

/// Synthesize a self-signed cert (CN = `common_name`, SANs = `san_list`) signed
/// by the **provided** keypair, returning `(cert_pem, key_pem)`. The floor path
/// passes its persisted stable key ([`load_or_create_floor_key`]) so re-synthesis
/// keeps a constant SPKI (priority #2/#4 — one synthesis impl, two key policies).
pub fn synthesize_self_signed_pem_with_key(
    common_name: &str,
    san_list: Vec<String>,
    cert_key: &rcgen::KeyPair,
) -> Result<(String, String), String> {
    let mut params =
        rcgen::CertificateParams::new(san_list).map_err(|e| format!("cert params: {e}"))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name.to_string());
    let cert = params
        .self_signed(cert_key)
        .map_err(|e| format!("self-sign cert: {e}"))?;
    Ok((cert.pem(), cert_key.serialize_pem()))
}

/// Write a self-signed bootstrap cert (`fullchain.pem` + `privkey.pem`) into
/// `acme_dir` so a nest serves TLS on its main port from boot — **with or without
/// a configured domain**. Required because the in-container mail bridge dials the
/// nest over **https on loopback** (`docker/s6/fauna-mail-bridge-*/run`) and
/// cannot enroll against a plaintext listener, and because the
/// self-signed-TLS-binding design (`mail-bridge-lifecycle.md` § TLS
/// provisioning) has clients pin a self-signed/LAN nest's Ed25519 identity
/// (channel-binding) rather than chain to a public CA — so the cert's name need
/// not match the address the client reached it on.
///
/// - `Some(domain)` — SANs cover the apex domain, `mail.<domain>`,
///   `relay.<domain>`, `pds.<domain>`, and loopback; CN = the domain. An ACME
///   deploy later self-heals this to a real cert.
/// - `None` — a **domainless** nest (the default; identified by its keypair and
///   reached at any IP): SANs cover loopback only, CN = `"fauna-nest"`. A browser
///   reaching it by IP sees the one self-signed prompt (expected for any
///   self-signed cert); native apps pin the identity, ignoring the name.
///   Domains added later from a client extend coverage (ACME / re-synthesis).
///
/// The caller invokes this only when no cert is already present.
pub fn write_self_signed_bootstrap(
    acme_dir: &std::path::Path,
    domain: Option<&str>,
) -> Result<(), String> {
    let mut sans = Vec::new();
    let cn = match domain {
        Some(d) => {
            sans.push(d.to_string());
            sans.push(format!("mail.{d}"));
            // `relay.<domain>` covers the self-hosted iroh P2P relay sidecar's
            // HTTPS listener (`bins/fauna-iroh-relay`, SNI-routed at
            // `relay.<domain>`). Added **unconditionally** here — NOT gated on a
            // connected relay sidecar like the ACME SAN
            // (`acme_http01::desired_san_domains`) — because the floor is
            // self-signed: an extra SAN costs nothing (no ACME order to fail on an
            // unresolvable name) and does NOT churn the SPKI (the persisted floor
            // key is unchanged — see `floor_spki_is_stable_across_resynthesis`).
            // The payoff is correctness at claim: the relay sidecar, standing by
            // since boot, is told the nest has a name and fetches its sealed
            // `relay.<domain>` cert from this floor with no nest reboot; carrying the
            // SAN unconditionally means the cert it gets already matches the
            // hostname clients dial, with no floor re-synthesis lag.
            sans.push(format!("relay.{d}"));
            // `pds.<domain>` covers the out-of-process ATProto PDS bridge's
            // XRPC/OAuth listener (`bins/fauna-bridges` role `atproto.pds`,
            // SNI-routed at `pds.<domain>` → `127.0.0.1:8447`). Added
            // **unconditionally** for the same reasons as `relay.<domain>` above
            // (self-signed floor: no ACME order to fail, stable SPKI, correctness
            // on a runtime Bluesky-enable flip): the bridge is brought up by the
            // supervisor with no nest reboot, then fetches its sealed cert from
            // this floor via `fetch_tls_cert_blob` (seal-on-read) and terminates
            // `pds.<domain>` TLS itself (atproto-pds-full.md § Wire & process
            // topology, F1 packaging resolution). The ACME SAN, by contrast, is
            // resolve-gated (`acme_http01::pds_san_included`).
            sans.push(fauna_bridge_atproto::oauth_metadata::pds_host(d));
            d.to_string()
        }
        None => "fauna-nest".to_string(),
    };
    sans.push("localhost".to_string());
    sans.push("127.0.0.1".to_string());
    // Synthesize from the persisted floor key so re-synthesis (the boot bootstrap
    // re-running, or the floor renew task) keeps a stable SPKI — never churns it
    // on a routine renewal (tls-certificates.md § A).
    let floor_key = load_or_create_floor_key(acme_dir)?;
    let (cert_pem, key_pem) = synthesize_self_signed_pem_with_key(&cn, sans, &floor_key)?;
    std::fs::create_dir_all(acme_dir)
        .map_err(|e| format!("create acme dir {}: {e}", acme_dir.display()))?;
    std::fs::write(acme_dir.join(crate::acme::CERT_FILENAME), cert_pem)
        .map_err(|e| format!("write {}: {e}", crate::acme::CERT_FILENAME))?;
    std::fs::write(acme_dir.join(crate::acme::KEY_FILENAME), key_pem)
        .map_err(|e| format!("write {}: {e}", crate::acme::KEY_FILENAME))?;
    Ok(())
}

/// Ensure the always-live self-signed floor cert exists under `acme_dir`
/// (tls-certificates.md § A). Idempotent: writes [`write_self_signed_bootstrap`]
/// only when no cert+key pair is already on disk, so it is safe to call from
/// every nest entry point. `domain` is the floor's CN/SAN subject (`None` =
/// domainless loopback floor, CN `"fauna-nest"`).
///
/// Called from BOTH `main.rs`'s `prepare_listener_tls` (which needs the cert on
/// disk *before* it builds its own TLS listener) AND `lib.rs`'s `start_server`
/// (the shared init EVERY caller hits — the Docker/CLI `main.rs` path, the
/// Windows `fauna-nest-service` which calls `start_server` directly with no TLS,
/// and the plain-HTTP e2e harness). Putting the universal write in `start_server`
/// means a nest that never goes through `prepare_listener_tls` still gets a floor
/// for its in-process mail bridges to fetch (`fetch_tls_cert_blob`) and serve over
/// CalDAV/IMAP TLS — the any-locator serving guarantee (caldav-imap-any-locator)
/// holds regardless of the nest's own API-listener TLS mode.
pub fn ensure_floor_present(acme_dir: &std::path::Path, domain: Option<&str>) {
    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    let key_path = acme_dir.join(crate::acme::KEY_FILENAME);
    if cert_path.exists() && key_path.exists() {
        return;
    }
    let label = domain.unwrap_or("no domain (IP/identity-only nest)");
    match write_self_signed_bootstrap(acme_dir, domain) {
        Ok(()) => tracing::info!(
            "Wrote self-signed bootstrap TLS floor for {label} to {} \
             (always-live floor; ACME, if enabled, self-heals it to a trusted cert)",
            acme_dir.display()
        ),
        Err(e) => tracing::warn!(
            "could not write self-signed bootstrap floor for {label}: {e}; \
             starting without TLS (the mail bridge will not be able to enroll over loopback)"
        ),
    }
}

/// Build a listener `rustls::ServerConfig` from the always-live self-signed
/// **floor** under `acme_dir`, synthesizing the floor first when absent.
///
/// This is the floor→`ServerConfig` seam shared by the standalone `fauna-nest`
/// binary (`main.rs::prepare_listener_tls`) and the Windows `fauna-nest-service`
/// (priority #2), so a fresh self-hosted nest serves HTTPS on boot from one code
/// path (`docs/goal/architecture/installers/windows.md` § Network-reachable nest).
/// It:
///
///   1. ensures the floor cert exists ([`ensure_floor_present`] — idempotent; a
///      no-op when a real ACME cert or an earlier floor is already on disk, so a
///      served real cert is loaded unchanged);
///   2. loads whatever cert+key is now on disk into a [`crate::acme::MultiDomainCertResolver`]
///      and builds a `ServerConfig` from it;
///   3. wires the stable floor key into the resolver so a served custom domain
///      whose trusted cert is missing/expired/non-covering still gets a per-SNI
///      floor cert carrying that domain (`tls-certificates.md` § A), rather than
///      the bare apex (name mismatch) or a dead handshake.
///
/// Returns `(config, resolver)`; the resolver is handed back so a long-lived
/// caller can hot-reload it (`cert_watcher_task`) or keep it for the web
/// listener. Returns `None` **only** when the floor write failed AND no cert is
/// on disk (e.g. a disk error) — the caller then chooses its no-cert fallback
/// (plain HTTP, or an ACME-pending resolver). `floor_domain` is the floor's
/// CN/SAN subject (`None` = a domainless loopback/IP nest, CN `"fauna-nest"`).
pub fn listener_tls_from_floor(
    acme_dir: &std::path::Path,
    floor_domain: Option<&str>,
) -> Option<(
    std::sync::Arc<rustls::ServerConfig>,
    std::sync::Arc<crate::acme::MultiDomainCertResolver>,
)> {
    ensure_floor_present(acme_dir, floor_domain);

    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    let key_path = acme_dir.join(crate::acme::KEY_FILENAME);
    if !(cert_path.exists() && key_path.exists()) {
        // The floor write failed and nothing is on disk — let the caller fall
        // back (plain HTTP, or an ACME-pending resolver under a configured domain).
        return None;
    }

    let resolver = std::sync::Arc::new(
        crate::acme::MultiDomainCertResolver::from_pem(&cert_path, &key_path)
            .expect("Failed to load TLS certificate"),
    );
    let config = std::sync::Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver.clone()),
    );
    tracing::info!("TLS certificate loaded from {}", acme_dir.display());

    // Wire the stable self-signed floor key so a custom domain with a
    // missing/expired/non-covering trusted cert gets a per-SNI floor cert that
    // carries that domain (tls-certificates.md § A). Created once and reused.
    match load_or_create_floor_key(acme_dir) {
        Ok(key) => resolver.set_floor_key(key.serialize_pem()),
        Err(e) => tracing::warn!(
            "could not load/create self-signed floor key at {}: {e}; per-SNI \
             floor fallback disabled (a custom domain with no valid cert serves \
             the apex default)",
            acme_dir.display()
        ),
    }

    Some((config, resolver))
}

/// How long before a floor cert's `notAfter` the renew task re-synthesizes it.
/// Matches the HTTP-01 renewal lead (`acme_http01.rs`, 30 days) so every cert
/// renewal shares one cadence; re-synthesis is cheap and SPKI-stable because the
/// floor reuses its persisted key (`tls-certificates.md` § A).
pub(crate) const FLOOR_RENEW_LEAD_SECS: i64 = 30 * 24 * 3600;

/// How often [`floor_renew_task`] wakes to re-check the on-disk floor's expiry.
const FLOOR_RENEW_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 3600);

/// The leaf cert's `notAfter` as Unix seconds (parsed via x509-parser).
fn leaf_not_after_unix(cert_pem: &[u8]) -> Result<i64, String> {
    let block = x509_parser::pem::Pem::iter_from_buffer(cert_pem)
        .next()
        .ok_or_else(|| "no PEM block in cert".to_string())?
        .map_err(|e| format!("parse pem: {e}"))?;
    let cert = block.parse_x509().map_err(|e| format!("parse x509: {e}"))?;
    Ok(cert.validity().not_after.timestamp())
}

/// Decide whether the on-disk floor at `cert_path` is due for renewal: true iff
/// it is a **self-signed** cert within [`FLOOR_RENEW_LEAD_SECS`] of expiry. False
/// for a CA-issued cert (ACME owns it — never ours to touch) or a floor with
/// ample validity left. A missing/unparseable cert is an `Err` the caller treats
/// as "skip this cycle".
fn floor_due_for_renewal(cert_path: &std::path::Path) -> Result<bool, String> {
    let pem = std::fs::read(cert_path).map_err(|e| format!("read cert: {e}"))?;
    // A real CA cert is being served (e.g. ACME self-healed the floor) — never
    // ours to renew, and clobbering it would downgrade a live trusted cert.
    if !crate::acme::pem_is_self_signed(&pem) {
        return Ok(false);
    }
    let not_after = leaf_not_after_unix(&pem)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("clock before epoch: {e}"))?
        .as_secs() as i64;
    Ok(now + FLOOR_RENEW_LEAD_SECS >= not_after)
}

/// Check the on-disk floor once and re-synthesize it (from the persisted stable
/// key) iff it is a self-signed cert nearing expiry; returns whether it renewed.
/// Factored out of [`floor_renew_task`] so the decision + action are unit-testable
/// without the async sleep loop.
pub(crate) fn maybe_renew_floor(
    acme_dir: &std::path::Path,
    domain: Option<&str>,
) -> Result<bool, String> {
    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    if !floor_due_for_renewal(&cert_path)? {
        return Ok(false);
    }
    // Re-synthesize from the persisted stable key (write_self_signed_bootstrap
    // loads it via load_or_create_floor_key) → fresh validity, unchanged SPKI.
    write_self_signed_bootstrap(acme_dir, domain)?;
    Ok(true)
}

/// Force-re-synthesize the self-signed floor to cover a domain learned *after*
/// boot — the claim path on a domainless-booted box, where the boot floor is the
/// domainless loopback cert (CN `fauna-nest`) but the admin just claimed a real
/// domain. Unlike [`ensure_floor_present`] (idempotent — no-op when any cert
/// exists) and [`maybe_renew_floor`] (only near expiry), this rewrites the floor
/// NOW so it carries `<domain>` + `mail.<domain>` + `relay.<domain>` +
/// `pds.<domain>` SANs, from
/// the persisted stable key (SPKI unchanged → no MUA re-prompt / TLSA churn). The
/// `cert_watcher_task` then hot-reloads it — no restart, and the in-container
/// MTA/MDA fetch a floor that already matches `mail.<domain>` for CalDAV/IMAP TLS.
///
/// **Never downgrades a trusted cert:** if a real CA cert is already on disk (ACME
/// self-healed the floor), this is a no-op — clobbering it would take live TLS from
/// trusted back to self-signed. So it only ever widens a *floor*; the ACME issuance
/// woken alongside it supersedes the floor with a trusted cert when it lands. Same
/// `pem_is_self_signed` guard [`floor_due_for_renewal`] uses.
pub fn resynthesize_floor_for_domain(
    acme_dir: &std::path::Path,
    domain: &str,
) -> Result<(), String> {
    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    if let Ok(pem) = std::fs::read(&cert_path)
        && !crate::acme::pem_is_self_signed(&pem)
    {
        // A trusted CA cert is being served — ACME owns it; leave it untouched.
        return Ok(());
    }
    write_self_signed_bootstrap(acme_dir, Some(domain))
}

/// Background task: keep the self-signed **floor** fresh unattended. The boot
/// bootstrap writes the floor once; without renewal it would silently expire on a
/// long-lived self-hosted / LAN nest (ACME off), eventually failing even native
/// apps' rustls handshakes (an expired cert is rejected). Periodically
/// re-checks the on-disk floor and, when it nears `notAfter`, re-synthesizes it
/// from the persisted stable key — the existing `cert_watcher_task` then
/// hot-reloads the refreshed PEM. The SPKI is unchanged (stable key), so no MUA
/// re-prompt and no TLSA churn. Backs off without clobbering if ACME later swaps
/// a real CA cert in (`cert_lifecycle_task`).
pub async fn floor_renew_task(acme_dir: std::path::PathBuf, domain: Option<String>) {
    let label = domain.as_deref().unwrap_or("no domain (IP/identity only)");
    loop {
        match maybe_renew_floor(&acme_dir, domain.as_deref()) {
            Ok(true) => tracing::info!(
                "Renewed self-signed TLS floor for {label} (stable key — SPKI unchanged)"
            ),
            Ok(false) => {}
            Err(e) => tracing::debug!("floor renewal check for {label} skipped: {e}"),
        }
        tokio::time::sleep(FLOOR_RENEW_CHECK_INTERVAL).await;
    }
}

/// `{role, bridge_id}` identity of a bridge a synthesized cert was (or was
/// not) sealed to. `Serialize` so the HTTP route can render it as JSON; the
/// WS-RPC handler maps it to the `fauna-protocol` wire type.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SealedBridge {
    pub role: String,
    pub bridge_id: String,
}

/// Outcome of a successful synthesize-and-seal.
pub(crate) struct SelfSignedCertOutcome {
    /// Bridges the sealed `TlsCertBlob` was fanned out to (had an x25519 key).
    pub bridges_sealed_to: Vec<SealedBridge>,
    /// Approved bridges skipped because they had no x25519 pubkey on file yet
    /// (re-provision after the bridge attests via `register_service_user`).
    pub bridges_skipped_no_x25519: Vec<SealedBridge>,
    /// Unix-seconds expiry of the synthesized cert.
    pub expires_at_unix: i64,
}

/// Failure modes, mapped by each caller to its own error envelope (HTTP
/// status / `RpcError`).
pub(crate) enum SelfSignedCertError {
    /// No active `mail_domains` row by that name.
    DomainNotActive,
    /// `rcgen` keygen / cert-synthesis failure (carries the detail message).
    Synthesis(String),
    /// A DB lookup failed (carries the `{e:#}`-formatted message).
    Db(String),
    /// The storage-trait seal/fan-out (`store_acme_material`) failed.
    Storage(StorageUnavailable),
}

/// Synthesize a self-signed cert for `domain` (CN = `domain`, SAN =
/// `domain` + `additional_dns_sans`) and seal+fan it out to every approved
/// bridge with an x25519 pubkey, writing the on-disk PEM. The domain must be
/// an active local mail domain. Idempotent in effect — re-calling
/// re-synthesizes and overwrites; this is how an admin refreshes the cert
/// before the 90-day window lapses, or lifts the seal onto a newly-attested
/// bridge.
pub(crate) async fn synthesize_and_seal_self_signed_cert(
    state: &AppState,
    domain: &str,
    additional_dns_sans: Vec<String>,
) -> Result<SelfSignedCertOutcome, SelfSignedCertError> {
    // (1) Confirm the domain exists in active `mail_domains`. We don't enforce
    //     a specific `mta_sts_cert_mode` value — admins legitimately re-issue
    //     a one-off self-signed cert even when the persistent mode is `ca`
    //     (e.g. during an ACME outage).
    match state.db.lookup_active_mail_domain(domain).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(SelfSignedCertError::DomainNotActive),
        Err(e) => return Err(SelfSignedCertError::Db(format!("{e:#}"))),
    }

    // (2) Generate the self-signed cert. CN = the domain; SANs = the domain
    //     plus any extras. `rcgen` 0.13 is already a direct dep for the ACME
    //     CSR path. Validity is pinned to the 90-day fetch cadence the bridge
    //     sees in the sealed bundle so the reported expiry aligns.
    let mut san_list = vec![domain.to_string()];
    san_list.extend(additional_dns_sans);

    let now_unix = fauna_core::data::Timestamp::now_secs_or_zero();
    let expires_at_unix = now_unix + (SELF_SIGNED_VALID_DAYS * 24 * 3600);

    let (cert_pem, key_pem) =
        synthesize_self_signed_pem(domain, san_list).map_err(SelfSignedCertError::Synthesis)?;

    // (3) Snapshot the approved-bridge set BEFORE fan-out so we can report
    //     which bridges received the seal and which were skipped (no x25519).
    //     The fan-out itself walks the same list inside `store_acme_material`;
    //     the split here is purely for the reported result, letting an admin
    //     notice when a newly-approved bridge hasn't yet self-attested its
    //     x25519 pubkey and the cert needs re-provisioning post-attestation.
    let approved = state
        .db
        .list_approved_bridge_service_users()
        .await
        .map_err(|e| SelfSignedCertError::Db(format!("{e:#}")))?;
    let (sealed_to, skipped): (Vec<_>, Vec<_>) = approved
        .iter()
        .map(|b| SealedBridge {
            role: b.role.as_str().to_string(),
            bridge_id: b.bridge_id.clone(),
        })
        .zip(approved.iter().map(|b| b.x25519_pubkey.is_some()))
        .partition(|(_, has_x)| *has_x);
    let bridges_sealed_to: Vec<SealedBridge> = sealed_to.into_iter().map(|(b, _)| b).collect();
    let bridges_skipped_no_x25519: Vec<SealedBridge> =
        skipped.into_iter().map(|(b, _)| b).collect();

    // (4) Hand the cert+key to the mode-aware storage trait, which writes
    //     on-disk PEM AND fans out wrapped `TlsCertBlob`s to each approved
    //     bridge with an x25519 pubkey. Plaintext + Encrypted have identical
    //     fan-out semantics (storage-trait parity port).
    let material = crate::storage::AcmeMaterial {
        domain,
        cert_chain_pem: cert_pem.as_bytes(),
        priv_key_pem: key_pem.as_bytes(),
    };
    state
        .storage()
        .store_acme_material(&material)
        .await
        .map_err(SelfSignedCertError::Storage)?;

    Ok(SelfSignedCertOutcome {
        bridges_sealed_to,
        bridges_skipped_no_x25519,
        expires_at_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPKI-SHA256 of the leaf cert in the `fullchain.pem` written to `acme_dir`.
    fn leaf_spki(acme_dir: &std::path::Path) -> [u8; 32] {
        let pem = std::fs::read(acme_dir.join(crate::acme::CERT_FILENAME)).expect("read cert");
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        let der = CertificateDer::pem_slice_iter(&pem)
            .next()
            .expect("a leaf cert")
            .expect("parse leaf cert");
        crate::acme::spki_sha256_of_cert_der(der.as_ref()).expect("spki of leaf")
    }

    /// The floor's SPKI must be **stable** across re-synthesis (a routine
    /// renewal re-mints the cert from a persisted key, not a fresh key) — so an
    /// MUA never re-prompts and a published TLSA stays valid
    /// (`tls-certificates.md` § A: "the SPKI changes only on a genuine,
    /// deliberate key rotation, never on a routine renewal").
    #[test]
    fn floor_spki_is_stable_across_resynthesis() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("first synth");
        let spki1 = leaf_spki(dir.path());
        // A second synthesis (stands in for a renewal) must reuse the persisted
        // floor key and therefore keep the same SPKI.
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("second synth");
        let spki2 = leaf_spki(dir.path());
        assert_eq!(
            spki1, spki2,
            "routine floor re-synthesis must keep a stable SPKI"
        );
    }

    /// A *deliberate* key rotation (removing the persisted floor key) is the one
    /// case that changes the SPKI — the escape hatch the stability guarantee
    /// leaves open (`tls-certificates.md` § A: SPKI changes "only on a genuine,
    /// deliberate key rotation").
    #[test]
    fn floor_spki_changes_on_deliberate_key_rotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("first synth");
        let spki1 = leaf_spki(dir.path());
        // Deliberate rotation = drop the persisted floor key; the next synthesis
        // regenerates and persists a fresh one.
        std::fs::remove_file(dir.path().join(FLOOR_KEY_FILENAME)).expect("rm floor key");
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("second synth");
        let spki2 = leaf_spki(dir.path());
        assert_ne!(spki1, spki2, "removing the floor key must rotate the SPKI");
    }

    /// `load_or_create_floor_key` persists on first call and returns the *same*
    /// public key on subsequent calls (the property the cert SPKI inherits).
    #[test]
    fn load_or_create_floor_key_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let k1 = load_or_create_floor_key(dir.path()).expect("create");
        assert!(
            dir.path().join(FLOOR_KEY_FILENAME).exists(),
            "floor key is persisted on first call"
        );
        let k2 = load_or_create_floor_key(dir.path()).expect("load");
        assert_eq!(
            k1.serialize_pem(),
            k2.serialize_pem(),
            "the persisted floor key reloads to the same keypair"
        );
    }

    /// A **domainless** nest (the default — identified by its keypair, reached at
    /// any IP) still gets a self-signed floor on boot, so HTTPS serves from boot
    /// before any domain is added from a client. `write_self_signed_bootstrap(None)`
    /// must write a valid, self-signed cert + key.
    #[test]
    fn domainless_bootstrap_writes_a_self_signed_cert() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir.path(), None).expect("domainless synth");
        let cert = dir.path().join(crate::acme::CERT_FILENAME);
        let key = dir.path().join(crate::acme::KEY_FILENAME);
        assert!(
            cert.exists() && key.exists(),
            "domainless floor writes cert + key"
        );
        // Parses (leaf_spki panics otherwise) and is self-signed.
        let _spki = leaf_spki(dir.path());
        let pem = std::fs::read(&cert).unwrap();
        assert!(
            crate::acme::pem_is_self_signed(&pem),
            "the domainless floor is self-signed"
        );
    }

    /// A domained floor carries the `relay.<domain>` SAN unconditionally (the
    /// self-hosted iroh relay sidecar's HTTPS host), so a relay brought up at
    /// runtime fetches a cert that already matches the name clients dial — no
    /// floor re-synthesis needed. A domainless floor adds no subdomains.
    #[test]
    fn floor_carries_relay_san_for_a_domain() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("synth");
        let pem = std::fs::read(dir.path().join(crate::acme::CERT_FILENAME)).expect("read cert");
        let sans = crate::acme_http01::cert_dns_sans(&pem);
        assert!(
            sans.iter().any(|s| s == "relay.example.com"),
            "domained floor must carry relay.<domain>; got {sans:?}"
        );
        assert!(
            sans.iter().any(|s| s == "mail.example.com"),
            "domained floor still carries mail.<domain>; got {sans:?}"
        );
        assert!(
            sans.iter().any(|s| s == "pds.example.com"),
            "domained floor must carry pds.<domain> (ATProto PDS bridge host); got {sans:?}"
        );

        // A domainless floor adds no subdomains (loopback only).
        let dir2 = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir2.path(), None).expect("domainless synth");
        let pem2 = std::fs::read(dir2.path().join(crate::acme::CERT_FILENAME)).expect("read cert");
        let sans2 = crate::acme_http01::cert_dns_sans(&pem2);
        assert!(
            !sans2.iter().any(|s| s.starts_with("relay.")),
            "a domainless floor must not carry any relay.<domain>; got {sans2:?}"
        );
        assert!(
            !sans2.iter().any(|s| s.starts_with("pds.")),
            "a domainless floor must not carry any pds.<domain>; got {sans2:?}"
        );
    }

    /// The lifted floor→`tls_config` seam: a caller (the standalone binary's
    /// `prepare_listener_tls` AND the Windows `fauna-nest-service`) hands it an
    /// `acme_dir` that does not yet exist and gets back a built `ServerConfig`,
    /// because the fn synthesizes the always-live loopback floor first. This is
    /// the path that lets a fresh Windows nest serve HTTPS on boot
    /// (installers/windows.md § Network-reachable nest).
    #[test]
    fn listener_tls_from_floor_synthesizes_floor_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acme_dir = dir.path().join("acme"); // does NOT exist yet
        assert!(!acme_dir.exists(), "precondition: acme dir absent");

        let result = listener_tls_from_floor(&acme_dir, None);

        assert!(
            result.is_some(),
            "a missing acme dir must synthesize the loopback floor and build a config"
        );
        // The floor cert, served key, and stable floor key are now on disk.
        assert!(
            acme_dir.join(crate::acme::CERT_FILENAME).exists(),
            "floor cert written"
        );
        assert!(
            acme_dir.join(crate::acme::KEY_FILENAME).exists(),
            "served key written"
        );
        assert!(
            acme_dir.join(FLOOR_KEY_FILENAME).exists(),
            "stable floor key persisted"
        );
        // The written floor is a self-signed cert that parses.
        let _spki = leaf_spki(&acme_dir);
    }

    /// When a floor (or any real cert) is already on disk, the seam reuses it —
    /// `ensure_floor_present` is a no-op, so the served cert's SPKI is unchanged
    /// (it does not re-synthesize and churn the key).
    #[test]
    fn listener_tls_from_floor_reuses_existing_cert() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_self_signed_bootstrap(dir.path(), Some("example.com")).expect("seed floor");
        let spki_before = leaf_spki(dir.path());

        let result = listener_tls_from_floor(dir.path(), Some("example.com"));
        assert!(result.is_some(), "an existing cert builds a config");

        let spki_after = leaf_spki(dir.path());
        assert_eq!(
            spki_before, spki_after,
            "an existing floor is reused, not re-synthesized — SPKI unchanged"
        );
    }

    // ---- sub-slice 2: floor auto-renew ----

    fn cert_path(acme_dir: &std::path::Path) -> std::path::PathBuf {
        acme_dir.join(crate::acme::CERT_FILENAME)
    }

    /// Write a self-signed floor cert valid for `days`, signed by `key`, into the
    /// served `fullchain.pem`/`privkey.pem` under `acme_dir` — like
    /// `write_self_signed_bootstrap` but with caller-controlled validity so the
    /// renewal threshold is exercisable (mirrors `web_content::cert`'s `gen_cert`).
    fn write_floor_with_validity(
        acme_dir: &std::path::Path,
        domain: &str,
        key: &rcgen::KeyPair,
        days: i64,
    ) {
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).expect("params");
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(key).expect("self-sign");
        std::fs::create_dir_all(acme_dir).unwrap();
        std::fs::write(acme_dir.join(crate::acme::CERT_FILENAME), cert.pem()).unwrap();
        std::fs::write(
            acme_dir.join(crate::acme::KEY_FILENAME),
            key.serialize_pem(),
        )
        .unwrap();
    }

    #[test]
    fn floor_due_for_renewal_tracks_self_signed_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        // Ample validity left (> 30-day lead) → not due.
        write_floor_with_validity(dir.path(), "example.com", &key, 90);
        assert!(!floor_due_for_renewal(&cert_path(dir.path())).unwrap());
        // Inside the 30-day lead → due.
        write_floor_with_validity(dir.path(), "example.com", &key, 10);
        assert!(floor_due_for_renewal(&cert_path(dir.path())).unwrap());
    }

    #[test]
    fn floor_due_for_renewal_skips_ca_issued_cert() {
        // A CA-issued (non-self-signed) cert nearing expiry must NOT be renewed by
        // the floor task — ACME owns it; clobbering it with a self-signed floor
        // would downgrade a live trusted cert.
        let dir = tempfile::tempdir().unwrap();
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec![]).unwrap();
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Test Root CA".to_string());
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let mut leaf_params =
            rcgen::CertificateParams::new(vec!["example.com".to_string()]).unwrap();
        leaf_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "example.com".to_string());
        leaf_params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(5);
        let leaf = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();

        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(cert_path(dir.path()), leaf.pem()).unwrap();
        assert!(
            !floor_due_for_renewal(&cert_path(dir.path())).unwrap(),
            "a CA-issued cert is not the floor's to renew"
        );
    }

    #[test]
    fn maybe_renew_floor_resynthesizes_near_expiry_with_stable_spki() {
        let dir = tempfile::tempdir().unwrap();
        // Seed the persisted floor key, then write a near-expiry floor signed by it.
        let floor_key = load_or_create_floor_key(dir.path()).unwrap();
        write_floor_with_validity(dir.path(), "example.com", &floor_key, 10);
        let spki_before = leaf_spki(dir.path());
        let na_before =
            leaf_not_after_unix(&std::fs::read(cert_path(dir.path())).unwrap()).unwrap();

        let renewed = maybe_renew_floor(dir.path(), Some("example.com")).unwrap();
        assert!(renewed, "near-expiry floor must be renewed");

        let spki_after = leaf_spki(dir.path());
        let na_after = leaf_not_after_unix(&std::fs::read(cert_path(dir.path())).unwrap()).unwrap();
        assert_eq!(
            spki_before, spki_after,
            "renewal reuses the stable floor key — SPKI unchanged"
        );
        assert!(na_after > na_before, "renewal advances notAfter");
    }

    #[test]
    fn maybe_renew_floor_is_noop_for_fresh_floor() {
        let dir = tempfile::tempdir().unwrap();
        write_self_signed_bootstrap(dir.path(), Some("example.com")).unwrap(); // fresh ~90-day floor
        assert!(
            !maybe_renew_floor(dir.path(), Some("example.com")).unwrap(),
            "a fresh floor is not due for renewal"
        );
    }
}
