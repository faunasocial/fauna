//! Shared **pebble** (Let's Encrypt's ACME test CA, `letsencrypt/pebble`)
//! test-harness helpers for the `#[ignore]`d real-wire acceptance tests that
//! drive an actual ACME account→order→authorizations→finalize flow — the CA
//! half the CA-free unit tests in each ACME-consuming crate cannot exercise.
//!
//! # Why this crate exists
//!
//! `fauna-acme-http01`'s `tests/pebble_http01.rs` and `fauna-client-dns`'s
//! `tests/pebble_dns01.rs` each independently hand-rolled the docker
//! lifecycle (`Pebble::start`/`Drop`), the CA extraction (`docker cp` with
//! retry), the readiness poll, the Docker-availability guard, and the
//! CA-trusting UA-tagged `instant_acme::HttpClient` pebble's strict UA check
//! requires — byte-identical apart from small per-flow parameters (the
//! container-name prefix, the injected User-Agent string). Each flow's own
//! **DNS responder** (what a query answers) stays with its own test file —
//! HTTP-01 always answers `A 127.0.0.1`; DNS-01 serves a mutable TXT/CNAME
//! table with one-shot delegation-chasing — that part is genuinely different
//! per flow, not duplicated.
//!
//! # Dependency posture: native-only, dev-dependency-only
//!
//! Every dependency here (`hyper-util`/`hyper-rustls`/`rustls`/docker via
//! `std::process::Command`) is native-only and pulled in solely as a
//! `[dev-dependencies]` entry of the crates whose `#[ignore]`d acceptance
//! tests need a running pebble container — never a production or wasm build
//! dependency of anything.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::Command;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Request, header};

/// Pebble image, pinned for reproducibility. `2.9.0` is the newest release
/// that still speaks the ACME wire shape `instant-acme 0.7.2` (the workspace
/// pin, shared with the nest's production HTTP-01 path) parses. `2.10.0` adds
/// the `dns-account-01` challenge (an extra entry in every authorization's
/// `challenges` array whose object has no `token` field), and instant-acme
/// 0.7.2's `Challenge` requires `token` (→ "missing field `token`") — so a
/// `2.6.0` .. `2.9.0` pebble is the contemporary match for the
/// production-pinned client. `docker run` auto-pulls it, so a test is
/// self-contained. Override with `FAUNA_PEBBLE_IMAGE` (e.g. to re-test
/// against a newer pebble after an instant-acme bump that understands
/// `dns-account-01`).
const DEFAULT_PEBBLE_IMAGE: &str = "ghcr.io/letsencrypt/pebble:2.9.0";
/// Pebble's fixed ACME directory listener (host network) — see its baked
/// `pebble-config.json`.
pub const PEBBLE_DIR_URL: &str = "https://127.0.0.1:14000/dir";
/// Path inside the image of the CA that signs pebble's HTTPS listener cert.
const PEBBLE_CA_PATH: &str = "/test/certs/pebble.minica.pem";

/// The pebble image to run. See [`DEFAULT_PEBBLE_IMAGE`] for the pin
/// rationale; override with `FAUNA_PEBBLE_IMAGE`.
pub fn pebble_image() -> String {
    std::env::var("FAUNA_PEBBLE_IMAGE").unwrap_or_else(|_| DEFAULT_PEBBLE_IMAGE.to_string())
}

/// A running pebble container, removed on drop.
pub struct Pebble {
    pub name: String,
    pub ca_pem: Vec<u8>,
}

impl Pebble {
    /// Start pebble on the host network, validating DNS through `dns_addr`,
    /// and extract its CA. `PEBBLE_VA_NOSLEEP` skips the artificial
    /// validation delay; `PEBBLE_WFE_NONCEREJECT=0` makes nonces
    /// deterministic. `flow` names the validation method (`"http01"` /
    /// `"dns01"`) and `label` distinguishes a file's own (serialized) tests —
    /// both feed the container name alongside this process's pid, so two
    /// flows (or two processes) never collide on a name.
    pub fn start(flow: &str, label: &str, dns_addr: &str) -> Pebble {
        let name = format!("fauna-pebble-{flow}-{label}-{}", std::process::id());
        // Clear any stale same-named container from an interrupted prior run.
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();

        let image = pebble_image();
        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--name",
                &name,
                "--network",
                "host",
                "-e",
                "PEBBLE_VA_NOSLEEP=1",
                "-e",
                "PEBBLE_VA_ALWAYS_VALID=0",
                "-e",
                "PEBBLE_WFE_NONCEREJECT=0",
                &image,
                "-dnsserver",
                dns_addr,
            ])
            .output()
            .expect("docker run pebble");
        assert!(
            run.status.success(),
            "docker run pebble failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );

        let ca_pem = copy_pebble_ca(&name);
        wait_tcp_reachable("127.0.0.1:14000");
        Pebble { name, ca_pem }
    }
}

impl Drop for Pebble {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

/// `docker cp` pebble's CA out to a temp file and read it (retried while the
/// container filesystem comes up).
pub fn copy_pebble_ca(container: &str) -> Vec<u8> {
    let dest = std::env::temp_dir().join(format!("{container}-minica.pem"));
    let dest_str = dest.to_string_lossy().to_string();
    for attempt in 0..40 {
        let cp = Command::new("docker")
            .args(["cp", &format!("{container}:{PEBBLE_CA_PATH}"), &dest_str])
            .output()
            .expect("docker cp pebble CA");
        if cp.status.success()
            && let Ok(bytes) = std::fs::read(&dest)
        {
            let _ = std::fs::remove_file(&dest);
            return bytes;
        }
        if attempt == 39 {
            panic!(
                "could not copy pebble CA from {container}:{PEBBLE_CA_PATH}: {}",
                String::from_utf8_lossy(&cp.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    unreachable!()
}

/// Block until `addr` accepts a TCP connection (pebble's ACME listener is
/// up).
pub fn wait_tcp_reachable(addr: &str) {
    let addr: SocketAddr = addr.parse().expect("parse pebble addr");
    for _ in 0..160 {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("pebble ACME listener never became reachable on {addr}");
}

/// Fail fast (with a clear reason) if Docker is unavailable. `what` names the
/// calling test for the panic message (e.g. `"the pebble DNS-01 acceptance
/// test"`).
pub fn ensure_docker(what: &str) {
    let available = Command::new("docker")
        .arg("version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    assert!(
        available,
        "{what} needs a working Docker daemon (to run {})",
        pebble_image()
    );
}

/// Pebble 400s an ACME request carrying no `User-Agent` — production never
/// trusts pebble's throwaway CA and never omits a UA against a real CA, so
/// only the test injects one, ahead of the real hyper client, through the
/// pluggable `HttpClient` extension point.
struct UserAgentClient(Box<dyn instant_acme::HttpClient>, &'static str);

impl instant_acme::HttpClient for UserAgentClient {
    fn request(
        &self,
        mut req: Request<instant_acme::BodyWrapper<Bytes>>,
    ) -> Pin<
        Box<dyn Future<Output = Result<instant_acme::BytesResponse, instant_acme::Error>> + Send>,
    > {
        req.headers_mut()
            .insert(header::USER_AGENT, HeaderValue::from_static(self.1));
        self.0.request(req)
    }
}

/// Build the `Box<dyn instant_acme::HttpClient>` an order uses to reach
/// pebble: a hyper client whose rustls roots include pebble's throwaway CA
/// (`ca_pem`) so the otherwise-untrusted self-signed ACME endpoint verifies,
/// wrapped to add the `user_agent` pebble requires. The rustls `ClientConfig`
/// is built with an explicit `ring` provider to avoid relying on a
/// process-default crypto provider.
pub fn pebble_http_client(
    ca_pem: &[u8],
    user_agent: &'static str,
) -> Box<dyn instant_acme::HttpClient> {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let mut roots = rustls::RootCertStore::empty();
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
        roots
            .add(cert.expect("parse pebble CA pem"))
            .expect("add pebble CA to roots");
    }

    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("rustls default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    let client: Client<_, instant_acme::BodyWrapper<Bytes>> =
        Client::builder(TokioExecutor::new()).build(https);
    Box::new(UserAgentClient(Box::new(client), user_agent))
}
