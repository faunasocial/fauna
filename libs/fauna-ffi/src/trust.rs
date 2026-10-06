//! TLS-trust startup wiring for the UniFFI apps (windows / macOS / iOS /
//! android). Linux does this natively in Rust; the native shells reach the
//! process-global trust store through this one exported function.
//!
//! See `docs/goal/architecture/security.md` § Transport trust. The disk-backed
//! pin store is the SSH-`known_hosts` analogue that lets learned
//! `(nest host → nest_actor_id)` TOFU pins survive client restarts.

use std::path::Path;
use std::sync::Arc;

/// Install the disk-backed nest-identity pin store at app startup so TOFU pins
/// (self-signed / LAN nests) survive restarts instead of being re-learned every
/// launch. Call **once**, early — before the first authenticated nest connect.
///
/// `data_dir` is the client's own platform config dir (Apple application-support
/// `Fauna/`, Windows `%LocalAppData%\Fauna`, Android `context.filesDir`); the
/// canonical pin filename is appended inside (`DiskPinStore::open_in_dir`), so
/// every app's on-disk layout is identical. The directory is created if
/// absent. Infallible and idempotent-ish (process-global, last write wins) — a
/// best-effort store load never fails a connection, so there is nothing to
/// surface to the caller.
#[uniffi::export]
pub fn install_nest_identity_pin_store(data_dir: String) {
    let dir = Path::new(&data_dir);
    let _ = std::fs::create_dir_all(dir);
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::DiskPinStore::open_in_dir(dir),
    ));
}

/// Install the pin store **read-only** — for a pin-consumer process (the apple
/// File Provider extension; any future background agent) that reads the pins the
/// interactive app minted but must never mint or remove any itself: first-trust
/// is a user decision made where a user is present, so with no pin for a
/// TOFU-rooted host the consumer's connect fails (`PinRequired`) and retries
/// until the app has pinned the nest, instead of silently trusting whatever it
/// reached. The store is uncached, so a pin the app writes after this process
/// launched is visible on the very next connect retry. `data_dir` must be the
/// SAME shared dir the app's writer store uses (on apple: the app-group `trust/`
/// dir). security.md § Transport trust, pin custody.
#[uniffi::export]
pub fn install_nest_identity_pin_store_read_only(data_dir: String) {
    let dir = Path::new(&data_dir);
    let _ = std::fs::create_dir_all(dir);
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::ReadOnlyDiskPinStore::open_in_dir(dir),
    ));
}

/// The **install-scoped trust home** as shared Rust resolves it for this
/// platform (`fauna_client::cert_binding::install_scoped_trust_home` —
/// security.md § Pin custody across processes, rule 1): the ONE dir the
/// interactive app's writer store and every Rust pin consumer on the machine
/// (the sync agent, fauna-tui) agree on. A native shell takes its writer dir
/// from here rather than re-deriving it, so the app and its consumers can
/// never drift apart — on macOS that is `~/Library/Application Support/Fauna/trust`
/// (the user domain, never the TCC-protected app-group container). Meant for
/// unsandboxed desktop shells; a sandboxed shell (iOS) keeps its own container
/// dir, where this resolver would point outside the sandbox.
#[uniffi::export]
pub fn install_scoped_trust_home() -> String {
    fauna_client::cert_binding::install_scoped_trust_home()
        .to_string_lossy()
        .into_owned()
}

/// [`install_nest_identity_pin_store`] plus a **read replica**: every persist
/// to `data_dir` is mirrored into `replica_dir` (write-temp-then-rename), and
/// the replica is refreshed once now. The macOS app uses it so the sandboxed
/// File Provider extension — which cannot reach the user-domain writer dir —
/// reads the app's pins from the app-group container's `trust/`
/// (`install_nest_identity_pin_store_read_only` there). The app stays the
/// only minter; the replica is its file, copied. Best-effort: a replica that
/// cannot be written never fails a mint.
#[uniffi::export]
pub fn install_nest_identity_pin_store_with_mirror(data_dir: String, replica_dir: String) {
    let dir = Path::new(&data_dir);
    let _ = std::fs::create_dir_all(dir);
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::DiskPinStore::open_in_dir_with_mirror(
            dir,
            Path::new(&replica_dir),
        ),
    ));
}

/// Forget the TOFU nest-identity pin for `nest_url` — the explicit,
/// user-approved recovery behind the `launch_identity_changed` warning's
/// "trust this nest" action (the `ssh-keygen -R host` analogue; web's
/// `forgetNestIdentityPin` twin). The next connect re-TOFUs against whatever
/// identity the nest then proves. Never call this outside that user action —
/// a silent forget defeats the pin (security.md § Transport trust). Clients
/// on the shared `LaunchMachine` should prefer its `trust_nest_identity()`
/// (forget + re-challenge in one step); this free fn is for shells that
/// drive the silent challenge directly.
#[uniffi::export]
pub fn forget_nest_identity_pin(nest_url: String) {
    let host = fauna_client::trust::authority_of(&nest_url);
    fauna_client::trust::forget_identity_pin(&host);
}

// ── Residual-HTTP TLS-pin query accessors (gated `nest-trust`) ──
//
// These let the windows C# `DirectNestClient`'s `ServerCertificateCustomValidationCallback`
// trust a self-signed nest the SAME way the rest of the Rust stack does — by comparing the
// served cert's SPKI fingerprint against the SPKI the WS handshake graduated into the
// process-global pin cache. The byte-bulk content API stays HTTP/1.1 in C# (transport.md
// § HTTP residue — not migrated to WS-RPC, just secured); only the *trust decision* moves
// to shared Rust. Computing the cert SPKI here (not via .NET's re-encoding
// `ExportSubjectPublicKeyInfo`) guarantees byte-identical agreement with the pin, since the
// pin is produced by this very function during the handshake (`tls_verify::CapturedCert`).
// See `docs/goal/architecture/security.md` § Transport trust.

/// SHA-256 of the cert's raw DER `SubjectPublicKeyInfo` — the exact value the WS
/// handshake pins (`fauna_protocol::tls_spki::spki_sha256_of_cert_der`). `None` if
/// `cert_der` doesn't parse as an X.509 certificate. The C# cert-validation callback
/// passes the received leaf cert's `RawData` to compare against `pinned_spki_for_host`.
#[cfg(feature = "nest-trust")]
#[uniffi::export]
pub fn spki_sha256_of_cert_der(cert_der: Vec<u8>) -> Option<Vec<u8>> {
    fauna_protocol::tls_spki::spki_sha256_of_cert_der(&cert_der).map(|h| h.to_vec())
}

/// The SPKI SHA-256 pinned for `host` (a `host[:port]` authority, see
/// [`authority_of`]) by a prior graduated WS handshake, or `None` if none yet —
/// `fauna_client::trust::pinned_spki`. The C# callback accepts a self-signed cert
/// for a non-loopback host iff its computed SPKI equals this pin.
#[cfg(feature = "nest-trust")]
#[uniffi::export]
pub fn pinned_spki_for_host(host: String) -> Option<Vec<u8>> {
    fauna_client::trust::pinned_spki(&host).map(|s| s.to_vec())
}

/// Extract the `host[:port]` authority a nest URL is pinned under
/// (`fauna_client::trust::authority_of`) — the canonical pin key. The C# callback
/// MUST use this rather than .NET `Uri.Authority`, which strips the default `:443`
/// and would mismatch the `host:port` key Rust pins (e.g. `https://127.0.0.1:443`
/// → `127.0.0.1:443`, not `127.0.0.1`).
#[cfg(feature = "nest-trust")]
#[uniffi::export]
pub fn authority_of(nest_url: String) -> String {
    fauna_client::trust::authority_of(&nest_url)
}

/// True if a `host[:port]` authority (see [`authority_of`]) is a loopback host —
/// IPv4 `127.0.0.0/8`, IPv6 `::1`, or `localhost` (`fauna_client::trust::is_loopback_authority`).
/// The C# callback accepts a same-box self-signed nest's cert when this is true
/// (the unauthenticated health check can precede the WS handshake that graduates a
/// pin), so the loopback classification — including the IPv6-bracket parse — is one
/// shared Rust impl rather than a hand-rolled per-app parser.
#[cfg(feature = "nest-trust")]
#[uniffi::export]
pub fn is_loopback_authority(authority: String) -> bool {
    fauna_client::trust::is_loopback_authority(&authority)
}
