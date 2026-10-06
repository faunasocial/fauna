//! The capturing TLS certificate verifier — the shared client-side half of the
//! channel binding (`docs/goal/architecture/security.md` § Transport trust,
//! Axis 1).
//!
//! Lifted here (Spec Y2 slice 4 §5, priority #2/#4) from `fauna-anon-client` so
//! that **both** native WS dialers share one implementation:
//!
//! - the **bearer client channel** (`fauna-client` / `fauna-anon-client`, one
//!   WS per actor), and
//! - the **nest↔nest federation channel** (`bins/fauna-nest`, one `wss://` WS
//!   per peer nest — the dialer captures the peer's served-cert SPKI for the
//!   `fauna.federation.hello` channel binding).
//!
//! `fauna-anon-client` re-exports these symbols, so its existing
//! `tls_verify::{…}` paths are unchanged.
//!
//! It replaces the old `FAUNA_INSECURE_TLS=1 → accept any cert` path with one
//! that is safe by construction. During the rustls handshake it:
//!
//! 1. **Captures** the SHA-256 SPKI fingerprint of the leaf cert the server
//!    actually presented (`fauna_protocol::tls_spki`), and
//! 2. records whether that cert **chains to the public WebPKI roots** (incl.
//!    hostname match).
//!
//! It never *authenticates* on its own — TLS completes before the WS-RPC
//! handshake, so the cert is accepted only **provisionally** (encrypt-only) and
//! the caller retroactively authenticates it via the in-band channel binding
//! (security.md § Connection-teardown rule):
//!
//! - **`webpki_valid == true`** — the cert is authenticated the boring way
//!   (public CA + hostname). The channel binding is belt-and-suspenders; the
//!   caller may connect without it.
//! - **`webpki_valid == false`** — a self-signed / LAN / `.local` cert. The cert
//!   is provisionally accepted (encrypt-only) and the WS-RPC channel binding is
//!   the **sole** authentication: the caller MUST verify it (the anon client's
//!   `trust::graduate_handshake`; the federation dialer's `fauna.federation.hello`)
//!   or tear the connection down.
//!
//! For a **bearer-carrying** connection the binding has already run on the
//! pre-identity handshake connection and yielded a trusted SPKI to pin
//! (security.md § Cross-connection binding). [`spki_pinned_client_config`]
//! builds a verifier that **hard-fails** the handshake unless the served leaf
//! SPKI equals that pin — so an attacker who passes the handshake connection
//! through cleanly but intercepts the authenticated WS cannot complete TLS as a
//! cert whose key it lacks.
//!
//! The residual HTTP content API + `POST /register` ride a **reqwest** client
//! built *before* the WS handshake graduates a pin (and `rustls::ServerName`
//! lacks the port the pin map is keyed on), so they can't bake in a fixed pin.
//! [`dynamic_pinned_client_config`] takes a [`PinResolver`] the verifier calls
//! **per-handshake** — the bearer reqwest leg supplies `move || pinned_spki(&authority)`
//! with [`NoPinPolicy::RequireWebPki`], so it pins the moment the WS handshake
//! graduates one and otherwise refuses a non-WebPKI cert (never accept-any —
//! that was exactly what `FAUNA_INSECURE_TLS` did).

use std::sync::{Arc, Mutex};

use rustls::ClientConfig;
use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use fauna_protocol::tls_spki::spki_sha256_of_cert_der;

/// What the capturing verifier observed about the server's leaf cert. Read by
/// the connect path **after** the TLS handshake completes (a `Mutex` because the
/// verifier callback runs on the connecting task before the connector returns).
#[derive(Debug, Clone, Default)]
pub struct CapturedCert {
    /// SHA-256 SPKI fingerprint of the received leaf cert; `None` if the leaf
    /// failed to parse (a malformed cert never authenticates).
    pub spki: Option<[u8; 32]>,
    /// Whether the chain validated against the public WebPKI roots, hostname
    /// included. When `false`, the channel binding is the sole authentication.
    pub webpki_valid: bool,
}

/// Shared handle the connect path reads after the handshake.
pub type CaptureHandle = Arc<Mutex<CapturedCert>>;

/// Resolves the SPKI pin to enforce, called **once per TLS handshake** inside the
/// verifier. `Some(pin)` ⇒ require the served leaf SPKI to equal it; `None` ⇒ no
/// pin is known, fall to [`NoPinPolicy`]. A *dynamic* resolver (e.g.
/// `move || pinned_spki(&authority)`) lets a long-lived reqwest client pick up the
/// pin the WS handshake graduates later, and re-read it across a cert rotation —
/// neither of which a fixed `Option<[u8; 32]>` baked in at build time can do.
pub type PinResolver = Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>;

/// What to do when [`PinResolver`] yields `None` (no pin known for this host).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoPinPolicy {
    /// Accept any cert **provisionally** (encrypt-only). For the pre-identity
    /// capturing connection that retroactively authenticates via the in-band
    /// channel binding (`trust::graduate_handshake`).
    AcceptProvisional,
    /// Require a WebPKI-valid cert; reject a self-signed / LAN cert outright. For
    /// a **bearer-carrying** connection that has no graduated pin yet — it must
    /// never blindly accept a self-signed cert (that is the MITM-open hole
    /// `FAUNA_INSECURE_TLS` opened). A public-CA nest still takes the boring path;
    /// a self-signed nest is refused until the WS handshake graduates its pin.
    RequireWebPki,
}

/// Build the rustls WebPKI verifier from the bundled Mozilla roots, once.
///
/// The root store itself moved to `fauna-tls-bootstrap` on 2026-08-30 — this
/// crate had owned it, but `fauna-mail`'s native IMAP connector was building
/// the identical store rather than depending on a full WS-transport stack to
/// borrow four lines, and the nest's mail-deliverability probe was reaching
/// into this crate for exactly that. The bootstrap crate is where the shared
/// rustls preamble lives, so the store lives there and both copies are gone.
fn webpki_verifier() -> Arc<WebPkiServerVerifier> {
    WebPkiServerVerifier::builder(Arc::new(fauna_tls_bootstrap::webpki_root_store()))
        .build()
        .expect("static webpki roots build a verifier")
}

/// The capturing (and optionally SPKI-pinning) `ServerCertVerifier`.
struct CapturingVerifier {
    /// Delegated public-WebPKI verifier — used to *classify* the cert
    /// (`webpki_valid`) and to verify the TLS handshake signature (which proves
    /// the server holds the leaf's private key regardless of chain trust).
    inner: Arc<WebPkiServerVerifier>,
    capture: CaptureHandle,
    /// Resolves the pin to enforce, called per-handshake. `Some` ⇒ the served leaf
    /// SPKI MUST equal it or the handshake hard-fails (cross-connection binding on
    /// a bearer connection). `None` ⇒ fall to [`CapturingVerifier::no_pin`].
    resolve_pin: PinResolver,
    /// What to do when `resolve_pin` yields `None`.
    no_pin: NoPinPolicy,
}

// `PinResolver` is a boxed `Fn` (no `Debug`), so derive a manual one for the
// `#[derive(Debug)]` that rustls' `ServerCertVerifier` bound transitively wants.
impl std::fmt::Debug for CapturingVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturingVerifier")
            .field("no_pin", &self.no_pin)
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let spki = spki_sha256_of_cert_der(end_entity.as_ref());
        let webpki_valid = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .is_ok();
        *self.capture.lock().unwrap() = CapturedCert { spki, webpki_valid };

        if let Some(pin) = (self.resolve_pin)() {
            // Cross-connection binding: this connection carries the bearer, so
            // it must present the exact SPKI the handshake binding authenticated.
            return match spki {
                Some(s) if s == pin => Ok(ServerCertVerified::assertion()),
                _ => Err(rustls::Error::General(
                    "served cert SPKI does not match the pinned nest identity".into(),
                )),
            };
        }

        // No pin known for this host. The policy decides:
        match self.no_pin {
            // Pre-identity capturing connection: accept provisionally (encrypt-
            // only). When `webpki_valid` the boring path already authenticated;
            // otherwise the caller MUST verify the in-band channel binding before
            // trusting the connection.
            NoPinPolicy::AcceptProvisional => Ok(ServerCertVerified::assertion()),
            // Bearer-carrying connection with no graduated pin: a public-CA nest
            // is fine (boring path), but a self-signed cert is refused — never
            // accept-any (the retired `FAUNA_INSECURE_TLS` behaviour).
            NoPinPolicy::RequireWebPki => {
                if webpki_valid {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General(
                        "no pinned nest identity and the served cert is not WebPKI-valid".into(),
                    ))
                }
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // Verifying the handshake signature against the presented leaf's key
        // proves the server *holds that key* — independent of chain trust, and
        // exactly what makes SPKI pinning sound. Delegate to the WebPKI verifier.
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn config_with(resolve_pin: PinResolver, no_pin: NoPinPolicy) -> (ClientConfig, CaptureHandle) {
    // `ClientConfig::builder()` needs a process-default `CryptoProvider`. The WS
    // dial path installs ring before connecting, but the reqwest config is built
    // at client-construction time (before any handshake), so install it here too.
    // Idempotent.
    crate::adapter::ensure_tls_provider();
    let capture: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
    let verifier = Arc::new(CapturingVerifier {
        inner: webpki_verifier(),
        capture: capture.clone(),
        resolve_pin,
        no_pin,
    });
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    (config, capture)
}

/// A `ClientConfig` that captures the served leaf SPKI + WebPKI validity and
/// provisionally accepts any cert (encrypt-only). The returned [`CaptureHandle`]
/// holds the observation once the handshake completes. For the **pre-identity**
/// client connection that carries `fauna.auth.handshake`, and the **federation
/// dialer** that binds the captured SPKI into `fauna.federation.hello`.
pub fn capturing_client_config() -> (Arc<ClientConfig>, CaptureHandle) {
    let (config, capture) = config_with(Arc::new(|| None), NoPinPolicy::AcceptProvisional);
    (Arc::new(config), capture)
}

/// A `ClientConfig` that additionally **requires** the served leaf SPKI to equal
/// `pin`, hard-failing the TLS handshake otherwise. For a **bearer-carrying**
/// connection, after the handshake binding has authenticated which SPKI to pin
/// (security.md § Cross-connection binding).
pub fn spki_pinned_client_config(pin: [u8; 32]) -> (Arc<ClientConfig>, CaptureHandle) {
    let (config, capture) = config_with(Arc::new(move || Some(pin)), NoPinPolicy::RequireWebPki);
    (Arc::new(config), capture)
}

/// A `ClientConfig` whose pin is resolved **per-handshake** by `resolve_pin`, with
/// `no_pin` deciding what happens when none is known. Returns the owned config
/// (not `Arc`) so a reqwest client can take it via
/// [`reqwest::ClientBuilder::use_preconfigured_tls`]; the capture handle is not
/// returned because the verifier self-enforces (nothing reads it back). For the
/// residual HTTP content API + `POST /register` bearer reqwest leg, which supplies
/// `move || pinned_spki(&authority)` + [`NoPinPolicy::RequireWebPki`]
/// (`fauna_anon_client::trust::store_pinned_reqwest_tls`).
pub fn dynamic_pinned_client_config(resolve_pin: PinResolver, no_pin: NoPinPolicy) -> ClientConfig {
    config_with(resolve_pin, no_pin).0
}

#[cfg(test)]
mod tests {
    use super::*;

    // A self-signed leaf cert (DER) generated with rcgen, plus its SPKI, so the
    // verifier can be exercised without a live TLS server.
    fn self_signed_der() -> Vec<u8> {
        let cert = rcgen::generate_simple_self_signed(vec!["pi.local".to_string()]).unwrap();
        cert.cert.der().to_vec()
    }

    /// Build a verifier with a static pin (`None` ⇒ no pin) + a no-pin policy,
    /// mirroring how the public config builders wire `CapturingVerifier`.
    fn verifier(
        handle: CaptureHandle,
        pin: Option<[u8; 32]>,
        no_pin: NoPinPolicy,
    ) -> CapturingVerifier {
        CapturingVerifier {
            inner: webpki_verifier(),
            capture: handle,
            resolve_pin: Arc::new(move || pin),
            no_pin,
        }
    }

    fn verify(v: &CapturingVerifier, der: &[u8]) -> Result<ServerCertVerified, rustls::Error> {
        v.verify_server_cert(
            &CertificateDer::from(der.to_vec()),
            &[],
            &ServerName::try_from("pi.local").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn captures_spki_and_marks_self_signed_invalid() {
        let der = self_signed_der();
        let expected = spki_sha256_of_cert_der(&der).expect("parses");
        let handle: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
        // `AcceptProvisional` (capturing config): a self-signed cert fails WebPKI
        // but is provisionally accepted.
        let v = verifier(handle.clone(), None, NoPinPolicy::AcceptProvisional);
        assert!(
            verify(&v, &der).is_ok(),
            "self-signed cert is provisionally accepted"
        );
        let cap = handle.lock().unwrap().clone();
        assert_eq!(cap.spki, Some(expected));
        assert!(!cap.webpki_valid, "self-signed cert is not WebPKI-valid");
    }

    #[test]
    fn pinned_config_rejects_mismatched_spki() {
        let der = self_signed_der();
        let handle: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
        let v = verifier(
            handle.clone(),
            Some([0xaau8; 32]),
            NoPinPolicy::RequireWebPki,
        );
        assert!(
            verify(&v, &der).is_err(),
            "a cert whose SPKI != pin must hard-fail"
        );
        // The capture is still recorded (the caller can log the mismatch).
        assert_eq!(handle.lock().unwrap().spki, spki_sha256_of_cert_der(&der));
    }

    #[test]
    fn pinned_config_accepts_matching_spki() {
        let der = self_signed_der();
        let pin = spki_sha256_of_cert_der(&der).unwrap();
        let handle: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
        let v = verifier(handle, Some(pin), NoPinPolicy::RequireWebPki);
        assert!(verify(&v, &der).is_ok(), "matching pinned SPKI is accepted");
    }

    #[test]
    fn require_webpki_rejects_unpinned_self_signed() {
        // The reqwest bearer leg with no graduated pin: a self-signed cert is
        // refused outright (this is what retires `FAUNA_INSECURE_TLS` — the old
        // accept-any path). A public-CA cert would pass the boring path, but a
        // unit test can't mint one; the security property is the *rejection*.
        let der = self_signed_der();
        let handle: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
        let v = verifier(handle, None, NoPinPolicy::RequireWebPki);
        assert!(
            verify(&v, &der).is_err(),
            "no pin + non-WebPKI cert must be refused, never accept-any"
        );
    }

    #[test]
    fn dynamic_resolver_is_read_per_handshake() {
        // The store-backed reqwest leg reads the pin live each handshake, so a pin
        // that appears *after* the config is built is still enforced. Model that
        // with a resolver backed by mutable shared state.
        let der = self_signed_der();
        let good = spki_sha256_of_cert_der(&der).unwrap();
        let pin_slot: Arc<Mutex<Option<[u8; 32]>>> = Arc::new(Mutex::new(None));
        let slot = pin_slot.clone();
        let handle: CaptureHandle = Arc::new(Mutex::new(CapturedCert::default()));
        let v = CapturingVerifier {
            inner: webpki_verifier(),
            capture: handle,
            resolve_pin: Arc::new(move || *slot.lock().unwrap()),
            no_pin: NoPinPolicy::RequireWebPki,
        };
        // No pin yet → self-signed refused.
        assert!(verify(&v, &der).is_err(), "no pin yet → refuse self-signed");
        // Pin appears (the WS handshake graduated it) → same self-signed cert now
        // matches and is accepted, with no rebuild of the config.
        *pin_slot.lock().unwrap() = Some(good);
        assert!(
            verify(&v, &der).is_ok(),
            "pin now present → accept matching SPKI"
        );
        // A different served SPKI than the live pin → rejected.
        *pin_slot.lock().unwrap() = Some([0x01u8; 32]);
        assert!(
            verify(&v, &der).is_err(),
            "served SPKI != live pin → reject"
        );
    }

    #[test]
    fn dynamic_pinned_client_config_builds() {
        // Smoke: the owned-config builder used by the reqwest leg constructs.
        let _cfg = dynamic_pinned_client_config(Arc::new(|| None), NoPinPolicy::RequireWebPki);
    }
}
