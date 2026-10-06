//! A real, UNCLAIMED nest serving HTTPS from a self-signed **floor** cert on a
//! random loopback port — the fixture behind the two onboarding floor-TLS
//! regression binaries.
//!
//! Deliberately **not** in `tests/common/`: every `fauna-nest` integration
//! binary says `mod common;`, so a fixture parked there is compiled into all
//! ~276 of them. Exactly two binaries need this one, and they name it
//! themselves.
//!
//! ⚠ **Its two consumers are separate binaries on purpose — see
//! [`start_unclaimed_tls`]'s note before merging them back together.**

#![allow(dead_code)]

use std::sync::Arc;

use fauna_nest::acme::{ServedCertFacts, ServedCertSpki, spki_sha256_of_cert_der};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::TempDir;

pub const CLAIM_CODE: &str = "ABCDEF";
pub const DOMAIN: &str = "fauna.test";

/// Reports the served leaf's real SPKI for the channel binding — a genuine
/// self-signed nest (the nest signs the SPKI it actually serves).
pub struct FixedSpki(pub [u8; 32]);

impl ServedCertSpki for FixedSpki {
    fn current_spki_sha256(&self) -> Option<[u8; 32]> {
        Some(self.0)
    }
    fn served_cert_facts(&self, _sni: &str) -> Option<ServedCertFacts> {
        None
    }
    fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
        Some(self.0)
    }
}

/// Boot a real, UNCLAIMED nest serving HTTPS with a self-signed `localhost` floor
/// cert on a random loopback port, with the pre-identity onboarding handlers +
/// auth (channel binding) registered. Returns `(port, TempDir)` — keep the dir
/// alive so the `claim-code` file persists.
///
/// ⚠ **ONE TEST PER BINARY. Do not add a second `#[tokio::test]` to either
/// consumer, and do not merge them back into one file.**
///
/// `OnboardingMachine::new_with_provider_base_urls` installs its `"nest"` entry
/// into a **process-global** dial mirror
/// (`fauna_launch_machine::set_nest_dial_override`), and
/// `OnboardingMachine::provider_base_url` falls back to that global for any
/// machine built without its own map — `OnboardingMachine::new` never clears it.
/// So two onboarding machines in one process are not independent, however
/// separate their nests are: whichever installs an override captures the dial of
/// every other machine in the process.
///
/// That is not hypothetical. These two tests lived in one binary until
/// 2026-08-23 and were red on `origin/main`, deterministically, whenever libtest
/// ran them in parallel — the public-domain test installed its override, and the
/// bare-loopback test (which wants **no** override, that being its whole point)
/// then dialled the other test's port and got `Connection refused` once that nest
/// was torn down. It reports as
/// `ProbeError { phase: ChallengeResponse, transient: true, cause: "challenge error" }`.
///
/// **Two things conspire to make that near-undiagnosable, so know them before
/// you go hunting:** the machine's own `nest_url()` records the *resolved* URL
/// and never the override (deliberately — the override must not reach persisted
/// outcome data), so it reads correct while the socket goes elsewhere; and a
/// serial run is green because libtest orders `bare…` before `public…`, i.e. the
/// bare test probes before the global is ever installed. Serial-only is exactly
/// the configuration that hides this, which is why the split — not
/// `--test-threads=1` — is the fix (`e2e-conventions.md` convention 10: delete a
/// machine-global-state dependency, never serialize on it).
pub async fn start_unclaimed_tls() -> (u16, TempDir) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let real_spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let nest_signing_key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("claim-code"), CLAIM_CODE).unwrap();
    let db_path = dir.path().join("nest.db").to_string_lossy().into_owned();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let state = Arc::new(AppState {
        config: crate::common::config_with_db_path(db_path),
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::new(FixedSpki(real_spki))),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);

    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        // Loopback client → exempt from the per-IP cap, so any cap works.
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (port, dir)
}
