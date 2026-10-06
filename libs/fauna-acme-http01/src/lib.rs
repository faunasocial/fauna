//! Shared ACME **HTTP-01** machinery: the pending-challenge state + port-80
//! router, the account/order/issue flow, certificate inspectors, and the
//! persisted failed-validation retry budget.
//!
//! Lifted from the nest's `acme_http01.rs` (2026-08-21) so the fauna.social
//! front door (`services/fauna-front-door`) and the nest share ONE HTTP-01
//! implementation instead of forking it — `docs/goal/architecture/front-door.md`
//! § TLS policy; ACME mechanics owner:
//! `docs/goal/architecture/nest/tls-certificates.md`. Nest-specific policy —
//! SAN derivation, resolve gates, the cert lifecycle task — deliberately
//! stays in the nest.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::fmt;
use std::path::Path;
use tokio::sync::RwLock;

/// Certificate-chain filename `obtain_certificate` writes into `acme_dir`
/// (fullchain including intermediates).
pub const CERT_FILENAME: &str = "fullchain.pem";
/// Private-key filename `obtain_certificate` writes into `acme_dir`.
pub const KEY_FILENAME: &str = "privkey.pem";

/// Certificate-chain filename [`obtain_ip_certificate`] writes into `acme_dir`
/// — the § B-IP bridge cert's material, deliberately separate from the
/// domain cert's so the two lifecycles never clobber each other.
pub const IP_CERT_FILENAME: &str = "ip-fullchain.pem";
/// Private-key filename [`obtain_ip_certificate`] writes into `acme_dir`.
pub const IP_KEY_FILENAME: &str = "ip-privkey.pem";

/// The ACME **profile** Let's Encrypt requires for every IP-identifier
/// certificate: `shortlived`, 160 h (~6 days) of validity. IP certs are issued
/// under no other profile (`tls-certificates.md` § B-IP *What*), so this is
/// not a preference the caller may vary — it is the only order the CA accepts.
pub const IP_CERT_PROFILE: &str = "shortlived";

/// What an order is *for*: a set of DNS names (§ B, the deployment's own domain
/// cert) or a set of IP addresses attached to this box (§ B-IP, the bridge cert
/// a browser needs to reach a domainless box).
///
/// The two differ in exactly four places — the RFC 8738 identifier kind, the
/// ACME profile, the on-disk filenames, and nothing else — so they share one
/// order core rather than forking it (`tls-certificates.md` § B-IP: "The nest
/// orders it through the same HTTP-01 machinery as § B tier 1").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderSubject {
    /// A DNS SAN set whose first entry is the certificate's primary name.
    Dns(Vec<String>),
    /// Global-unicast addresses attached to this box's own interfaces.
    Ip(Vec<std::net::IpAddr>),
}

impl OrderSubject {
    /// The RFC 8738 identifiers this order asks the CA to validate.
    fn identifiers(&self) -> Vec<instant_acme::Identifier> {
        match self {
            Self::Dns(names) => names
                .iter()
                .map(|d| instant_acme::Identifier::Dns(d.clone()))
                .collect(),
            Self::Ip(addrs) => addrs
                .iter()
                .copied()
                .map(instant_acme::Identifier::Ip)
                .collect(),
        }
    }

    /// The SAN strings handed to the CSR builder. `rcgen` classifies a string
    /// that parses as an IP literal into an `iPAddress` `GeneralName` and
    /// everything else into a `dNSName`, so one list covers both kinds —
    /// unit-pinned in `fauna_acme_core`, because the classification is
    /// invisible at this call site and a silent flip would fail only against a
    /// live CA.
    fn csr_names(&self) -> Vec<String> {
        match self {
            Self::Dns(names) => names.clone(),
            Self::Ip(addrs) => addrs.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// The ACME profile to request, if the CA must be told one.
    fn profile(&self) -> Option<&'static str> {
        match self {
            Self::Dns(_) => None,
            Self::Ip(_) => Some(IP_CERT_PROFILE),
        }
    }

    /// `(cert_chain, private_key)` filenames written into `acme_dir`.
    fn filenames(&self) -> (&'static str, &'static str) {
        match self {
            Self::Dns(_) => (CERT_FILENAME, KEY_FILENAME),
            Self::Ip(_) => (IP_CERT_FILENAME, IP_KEY_FILENAME),
        }
    }

    /// True when there is nothing to order.
    fn is_empty(&self) -> bool {
        match self {
            Self::Dns(names) => names.is_empty(),
            Self::Ip(addrs) => addrs.is_empty(),
        }
    }
}

/// Shared state for pending ACME HTTP-01 challenges.
///
/// Maps challenge tokens to their key authorizations. The HTTP listener
/// serves these at `/.well-known/acme-challenge/{token}`.
#[derive(Debug, Default)]
pub struct ChallengeState {
    challenges: RwLock<HashMap<String, String>>,
}

impl ChallengeState {
    pub fn new() -> Self {
        Self {
            challenges: RwLock::new(HashMap::new()),
        }
    }

    /// Register a challenge token and its key authorization.
    pub async fn set(&self, token: String, key_auth: String) {
        self.challenges.write().await.insert(token, key_auth);
    }

    /// Remove a challenge token after validation completes.
    pub async fn remove(&self, token: &str) {
        self.challenges.write().await.remove(token);
    }

    /// Look up the key authorization for a challenge token.
    pub async fn get(&self, token: &str) -> Option<String> {
        self.challenges.read().await.get(token).cloned()
    }
}

/// Configuration for the HTTP-01 ACME solver.
///
/// The CA is Let's Encrypt production and the account contact is empty — both
/// are constants, not choices (`tls-certificates.md` § ACME settings —
/// constants, not choices). The one CA-agnostic seam is
/// [`directory_url`](Self::directory_url), artifact-set test IPC (the tier_4
/// acceptance points it at an in-network `pebble`; the hosted front door maps
/// its own staging switch onto it). Build with [`Http01Config::new`].
#[derive(Debug, Clone)]
pub struct Http01Config {
    pub domain: String,
    /// Account contact. Empty (a nest's value, always) ⇒ the account is created
    /// with no contact, as RFC 8555 allows. Only the project's own hosted front
    /// door sets one, via [`with_contact_email`](Self::with_contact_email).
    contact_email: String,
    pub acme_dir: PathBuf,
    pub http_port: u16,
    /// Explicit ACME directory URL (`[acme].directory_url`): the client orders
    /// against this CA instead of Let's Encrypt production. `None` ⇒ Let's
    /// Encrypt production. See `tls-certificates.md` § C.
    pub directory_url: Option<String>,
}

impl Http01Config {
    /// A config with no account contact, ordering from Let's Encrypt production
    /// unless `directory_url` names another CA.
    pub fn new(
        domain: String,
        acme_dir: PathBuf,
        http_port: u16,
        directory_url: Option<String>,
    ) -> Self {
        Self {
            domain,
            contact_email: String::new(),
            acme_dir,
            http_port,
            directory_url,
        }
    }

    /// Give the ACME account a contact address (the hosted front door's role
    /// address — a nest never calls this).
    #[must_use]
    pub fn with_contact_email(mut self, contact_email: String) -> Self {
        self.contact_email = contact_email;
        self
    }
}

/// The Let's Encrypt staging directory — for a caller that deliberately targets
/// it through [`Http01Config::directory_url`] (the hosted front door's
/// `FAUNA_ACME_STAGING`); a nest never does.
pub const LETS_ENCRYPT_STAGING_URL: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// Resolve the ACME directory URL the HTTP-01 client orders against: the explicit
/// `[acme].directory_url` override (a private/internal ACME CA, or the in-network
/// `pebble` the tier_4 acceptance uses), else Let's Encrypt production. Pure (no
/// I/O) so it is unit-tested without a live CA.
fn acme_directory_url(directory_url: Option<&str>) -> &str {
    use instant_acme::LetsEncrypt;
    directory_url.unwrap_or_else(|| LetsEncrypt::Production.url())
}

/// Build an axum router that serves ACME challenges and redirects everything
/// else to HTTPS.
///
/// Routes:
/// - `GET /.well-known/acme-challenge/{token}` — returns the key authorization
/// - `*` — 308 redirect to `https://{host}{path}`, where `{host}` is the
///   request's own `Host` (port-stripped — HTTPS lives on 443), so a custom
///   web domain / subdomain bounces to ITS https origin and a domainless boot
///   (empty `domain`) still redirects. `domain` is only the fallback for a
///   `Host`-less request; with neither, a hostless `https://` Location would
///   be malformed, so answer 400 (`domains-and-tls-bootstrap.md`
///   § Implementation status — port-80 redirect host).
pub fn http01_router(state: Arc<ChallengeState>, domain: String) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{token}", get(handle_challenge))
        .fallback(move |req: axum::extract::Request| {
            let fallback_host = domain.clone();
            async move {
                use axum::response::IntoResponse as _;
                let path = req.uri().path();
                let query = req
                    .uri()
                    .query()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default();
                // `HeaderValue::to_str` rejects non-visible-ASCII, so the host
                // can't smuggle header-splitting bytes into the Location.
                let host = req
                    .headers()
                    .get(axum::http::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .map(host_without_port)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string)
                    .unwrap_or(fallback_host);
                if host.is_empty() {
                    return (axum::http::StatusCode::BAD_REQUEST, "missing Host").into_response();
                }
                Redirect::permanent(&format!("https://{host}{path}{query}")).into_response()
            }
        })
        .with_state(state)
}

/// Strip a `:port` suffix from a `Host` header value, keeping a bracketed
/// IPv6 literal intact (`[::1]:8080` → `[::1]`) — the result goes straight back
/// into a redirect URL, where a bare `::1` would be unparseable.
fn host_without_port(host: &str) -> &str {
    fauna_core::web::split_host_port(host).0
}

/// Handler for `GET /.well-known/acme-challenge/{token}`.
async fn handle_challenge(
    State(state): State<Arc<ChallengeState>>,
    AxumPath(token): AxumPath<String>,
) -> Response {
    match state.get(&token).await {
        Some(key_auth) => key_auth.into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "challenge not found").into_response(),
    }
}

/// Start the HTTP listener for ACME challenges on the given port.
///
/// Returns a `JoinHandle` for the listener task.
pub async fn start_http01_listener(
    state: Arc<ChallengeState>,
    domain: String,
    port: u16,
) -> Result<tokio::task::JoinHandle<()>> {
    let app = http01_router(state, domain);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .context(format!("bind HTTP-01 listener on port {port}"))?;
    tracing::info!("HTTP-01 challenge listener started on port {port}");
    // spawn-ok(process-lifetime): started once by `main.rs` *before* the
    // serve-and-restart loop, and deliberately outlives every generation — it
    // holds only `ChallengeState` (itself process-lifetime), no `AppState` and
    // no deployment key, and re-binding :8080 per generation would fail. The
    // caller drops the handle knowingly.
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("HTTP-01 listener error: {e}");
        }
    });
    Ok(handle)
}

/// Run the full ACME HTTP-01 certificate acquisition flow for `domains` — a
/// SAN list whose first entry becomes the certificate's primary name. For a
/// mail deployment this is `[apex, <domain>, mail.<domain>, …]` so the one
/// chain covers the nest's own endpoint AND the bridge's mail listeners
/// (submission 465/587, IMAP 993, STARTTLS 25 reached at `mail.<domain>`).
///
/// 1. Load or create an ACME account (credentials saved to `acme_dir/account-key.json`)
/// 2. Create an order over every identifier in `domains`
/// 3. Solve the HTTP-01 challenge for each authorization via [`ChallengeState`]
/// 4. Generate a key pair and a multi-SAN CSR
/// 5. Finalize the order and download the certificate chain
/// 6. Write `fullchain.pem` and `privkey.pem` to `acme_dir`
///
/// NOTE: ACME validates *every* identifier in the order, so a SAN whose
/// A-record/port-80 isn't reachable fails the whole order (including the apex
/// the nest needs for its own WS). Callers must only pass SANs that resolve —
/// see `desired_san_domains` (nest-side), which derives mail hosts from *configured*
/// `mail_domains` for exactly this reason.
///
/// `account_dir` is where the Let's Encrypt **account** key
/// (`account-key.json`) is loaded/created; `config.acme_dir` is where the
/// **certificate** files (`fullchain.pem` / `privkey.pem`) are written. They
/// are usually the same directory (the nest's own apex cert), but the
/// web-content per-domain cert loop (the nest's `web_content::cert`) passes the
/// shared apex `acme_dir` as `account_dir` and a per-domain subdir as the cert
/// output dir, so a nest hosting N custom domains has **one** Let's Encrypt
/// account (shared rate budget) rather than one account per domain.
pub async fn obtain_certificate(
    config: &Http01Config,
    domains: &[String],
    challenge_state: &Arc<ChallengeState>,
    account_dir: &std::path::Path,
) -> Result<()> {
    obtain_certificate_inner(
        config,
        &OrderSubject::Dns(domains.to_vec()),
        challenge_state,
        account_dir,
        None,
    )
    .await
}

/// Order the **§ B-IP bridge certificate**: a publicly-trusted cert whose SANs
/// are `addrs` — global-unicast addresses attached to this box's own interfaces
/// — under the `shortlived` profile Let's Encrypt requires for every IP
/// identifier. Written to `acme_dir/ip-fullchain.pem` + `ip-privkey.pem`, kept
/// apart from the domain cert's material so the two lifecycles cannot clobber
/// each other.
///
/// Identical machinery to [`obtain_certificate`] — the same account (one
/// shared rate budget), the same port-80 challenge router, the same finalize
/// tail. Only the identifier kind, the profile and the filenames differ.
///
/// Validation is HTTP-01 against the address itself, so the CA must be able to
/// reach `:80` on it from arbitrary vantage points; a box whose `:80` is
/// firewalled fails here and the caller's failed-validation pacing applies as
/// it does for a domain order (`tls-certificates.md` § B-IP *When it cannot be
/// had*).
pub async fn obtain_ip_certificate(
    config: &Http01Config,
    addrs: &[std::net::IpAddr],
    challenge_state: &Arc<ChallengeState>,
    account_dir: &std::path::Path,
) -> Result<()> {
    obtain_certificate_inner(
        config,
        &OrderSubject::Ip(addrs.to_vec()),
        challenge_state,
        account_dir,
        None,
    )
    .await
}

/// [`obtain_ip_certificate`] with an injected ACME HTTP client — the pebble
/// acceptance path, mirroring [`obtain_certificate_with_http`].
#[cfg(any(test, feature = "test-helpers"))]
pub async fn obtain_ip_certificate_with_http(
    config: &Http01Config,
    addrs: &[std::net::IpAddr],
    challenge_state: &Arc<ChallengeState>,
    account_dir: &std::path::Path,
    http: Box<dyn instant_acme::HttpClient>,
) -> Result<()> {
    obtain_certificate_inner(
        config,
        &OrderSubject::Ip(addrs.to_vec()),
        challenge_state,
        account_dir,
        Some(http),
    )
    .await
}

/// [`obtain_certificate`] with an injected ACME HTTP client — for driving a
/// real order against a CA whose HTTPS listener the native-roots default
/// client won't trust (the pebble acceptance test). `test-helpers` only;
/// production always takes the default client.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn obtain_certificate_with_http(
    config: &Http01Config,
    domains: &[String],
    challenge_state: &Arc<ChallengeState>,
    account_dir: &std::path::Path,
    http: Box<dyn instant_acme::HttpClient>,
) -> Result<()> {
    obtain_certificate_inner(
        config,
        &OrderSubject::Dns(domains.to_vec()),
        challenge_state,
        account_dir,
        Some(http),
    )
    .await
}

async fn obtain_certificate_inner(
    config: &Http01Config,
    subject: &OrderSubject,
    challenge_state: &Arc<ChallengeState>,
    account_dir: &std::path::Path,
    http: Option<Box<dyn instant_acme::HttpClient>>,
) -> Result<()> {
    use instant_acme::{
        AccountCredentials, AuthorizationStatus, ChallengeType, NewAccount, NewOrder, OrderStatus,
    };

    anyhow::ensure!(!subject.is_empty(), "ACME order with no identifiers");

    let acme_dir = &config.acme_dir;
    tokio::fs::create_dir_all(acme_dir)
        .await
        .context("create acme directory")?;
    tokio::fs::create_dir_all(account_dir)
        .await
        .context("create acme account directory")?;

    let server_url = acme_directory_url(config.directory_url.as_deref());

    // Step 1: Load or create ACME account (shared account_dir, which may differ
    // from the per-domain cert output dir).
    let account_path = account_dir.join("account-key.json");
    let account = if account_path.exists() {
        tracing::info!(
            "Loading existing ACME account from {}",
            account_path.display()
        );
        let creds_json = tokio::fs::read_to_string(&account_path)
            .await
            .context("read account credentials")?;
        let creds: AccountCredentials =
            serde_json::from_str(&creds_json).context("parse account credentials")?;
        // instant-acme 0.8 moved account construction behind a builder:
        // `Account::builder()` (default hyper-rustls client) or
        // `Account::builder_with_http(http)` (the pebble path), then
        // `from_credentials`/`create` on the builder. The persisted credential
        // blob is unchanged across the bump — pinned by `acme_order.rs`'s
        // `credential_interop` tests, which matter because D6 re-loads an
        // account minted by the other driver.
        account_builder(http)?
            .from_credentials(creds)
            .await
            .map_err(|e| anyhow::anyhow!("restore ACME account: {e}"))?
    } else {
        tracing::info!("Creating new ACME account");
        // RFC 8555 allows an account with NO contact, and a nest's is always
        // empty — omit the contact rather than send a malformed `mailto:` (which
        // some CAs reject).
        let contact_string = format!("mailto:{}", config.contact_email);
        let contacts: Vec<&str> = if config.contact_email.trim().is_empty() {
            Vec::new()
        } else {
            vec![contact_string.as_str()]
        };
        let new_account = NewAccount {
            contact: &contacts,
            terms_of_service_agreed: true,
            only_return_existing: false,
        };
        let (account, creds) = account_builder(http)?
            .create(&new_account, server_url.to_string(), None)
            .await
            .map_err(|e| anyhow::anyhow!("create ACME account: {e}"))?;
        let creds_json = serde_json::to_string_pretty(&creds).context("serialize credentials")?;
        tokio::fs::write(&account_path, creds_json)
            .await
            .context("save account credentials")?;
        tracing::info!(
            "ACME account created and saved to {}",
            account_path.display()
        );
        account
    };

    // Step 2: Create order over the full identifier set — the SAN set (apex +
    // mail hostnames) for a domain order, this box's own public addresses for
    // the § B-IP bridge. An IP order additionally names the `shortlived`
    // profile, which Let's Encrypt requires for every IP identifier.
    let identifiers = subject.identifiers();
    let new_order = NewOrder::new(&identifiers);
    let new_order = match subject.profile() {
        Some(profile) => new_order.profile(profile),
        None => new_order,
    };
    let mut order = account
        .new_order(&new_order)
        .await
        .map_err(|e| anyhow::anyhow!("create ACME order: {e}"))?;

    // Step 3: Solve HTTP-01 challenge.
    //
    // Since 0.8 `authorizations()` is an async iterator of handles that borrow
    // the order, so the tokens are collected here rather than re-walking the
    // authorizations for the post-poll cleanup below (which cannot borrow the
    // order again while the poll holds it mutably).
    let mut solved_tokens: Vec<String> = Vec::new();
    let mut authorizations = order.authorizations();
    while let Some(auth) = authorizations.next().await {
        let mut auth = auth.map_err(|e| anyhow::anyhow!("get authorizations: {e}"))?;
        match auth.status {
            // Pending: solve the HTTP-01 challenge as usual.
            AuthorizationStatus::Pending => {}
            // Valid: the CA reused a still-valid authorization from a recent
            // order (its authz-reuse window). This happens whenever we re-issue
            // shortly after a prior success — canonically a
            // `provision_self_signed_cert` clobber immediately followed by the
            // ACME self-heal (`mail-bridge-lifecycle.md` § Self-healing), or a
            // renew inside the window. Its HTTP-01 challenge is already `valid`,
            // so re-POSTing `set_challenge_ready` is rejected by the CA
            // ("Cannot update challenge with status valid, only status
            // pending") and would fail the WHOLE re-issue — the order is already
            // authorized, so skip straight to finalize below.
            AuthorizationStatus::Valid => {
                tracing::debug!(
                    identifier = ?auth.identifier(),
                    "ACME authorization already valid (CA reused it); skipping HTTP-01 solve"
                );
                continue;
            }
            // Invalid / Revoked / Expired: not solvable on this order; leave it
            // and let the order-status poll below surface the CA's own error.
            other => {
                tracing::warn!(
                    identifier = ?auth.identifier(),
                    status = ?other,
                    "ACME authorization in a non-pending, non-valid state; skipping solve"
                );
                continue;
            }
        }

        let mut challenge = auth
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| anyhow::anyhow!("no HTTP-01 challenge found for authorization"))?;

        let key_auth = challenge.key_authorization();
        tracing::info!("Setting HTTP-01 challenge for token: {}", challenge.token);
        challenge_state
            .set(challenge.token.clone(), key_auth.as_str().to_string())
            .await;
        solved_tokens.push(challenge.token.clone());

        challenge
            .set_ready()
            .await
            .map_err(|e| anyhow::anyhow!("set challenge ready: {e}"))?;
    }

    // Step 4: Poll until order is ready or valid
    let mut retries = 20u8;
    let order_ready = loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let state = order
            .refresh()
            .await
            .map_err(|e| anyhow::anyhow!("refresh order: {e}"))?;
        tracing::debug!("Order status: {:?}", state.status);
        match state.status {
            OrderStatus::Ready => break true,
            OrderStatus::Valid => break false,
            OrderStatus::Pending | OrderStatus::Processing => {
                retries = retries
                    .checked_sub(1)
                    .ok_or_else(|| anyhow::anyhow!("order did not become ready after 20 polls"))?;
            }
            OrderStatus::Invalid => {
                anyhow::bail!("ACME order became invalid: {:?}", state.error);
            }
        }
    };

    // Clean up the challenge tokens this order actually published.
    for token in &solved_tokens {
        challenge_state.remove(token).await;
    }

    let (cert_filename, key_filename) = subject.filenames();

    // Step 5: Generate key and CSR, finalize order, poll for the certificate
    if order_ready {
        let (cert_chain_pem, cert_key_pem) =
            fauna_acme_core::finalize_order_and_fetch_certificate(&mut order, &subject.csr_names())
                .await
                .map_err(anyhow::Error::from)?;

        // Step 6: Write cert and key to disk
        let cert_path = acme_dir.join(cert_filename);
        let key_path = acme_dir.join(key_filename);

        tokio::fs::write(&cert_path, cert_chain_pem.as_bytes())
            .await
            .with_context(|| format!("write {cert_filename}"))?;
        tokio::fs::write(&key_path, cert_key_pem.as_bytes())
            .await
            .with_context(|| format!("write {key_filename}"))?;

        tracing::info!("Certificate obtained and saved to {}", cert_path.display());
    } else {
        tracing::info!("Order already valid, fetching certificate");
        let cert_chain_pem = order
            .certificate()
            .await
            .map_err(|e| anyhow::anyhow!("get certificate: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("no certificate in valid order"))?;

        let cert_path = acme_dir.join(cert_filename);
        tokio::fs::write(&cert_path, cert_chain_pem.as_bytes())
            .await
            .with_context(|| format!("write {cert_filename}"))?;
        tracing::info!("Certificate saved to {}", cert_path.display());
    }

    Ok(())
}

/// The instant-acme 0.8 account builder, with the pebble HTTP client injected
/// when the caller passed one. `Account::builder()` is fallible (it constructs
/// the default hyper-rustls client); `builder_with_http` is not.
fn account_builder(
    http: Option<Box<dyn instant_acme::HttpClient>>,
) -> Result<instant_acme::AccountBuilder> {
    match http {
        Some(http) => Ok(instant_acme::Account::builder_with_http(http)),
        None => instant_acme::Account::builder()
            .map_err(|e| anyhow::anyhow!("build ACME HTTP client: {e}")),
    }
}

/// Parse the seconds remaining until the leaf certificate in `pem_data` expires.
///
/// Returns `None` if the PEM cannot be parsed or contains no certificates.
pub fn cert_seconds_remaining(pem_data: &[u8]) -> Option<i64> {
    use x509_parser::pem::parse_x509_pem;
    use x509_parser::time::ASN1Time;
    // Parse the first PEM block (leaf certificate).
    let (_, pem) = parse_x509_pem(pem_data).ok()?;
    let cert = pem.parse_x509().ok()?;
    let not_after = cert.validity().not_after.timestamp();
    let now = ASN1Time::now().timestamp();
    Some(not_after - now)
}

/// Extract the **IP addresses** from a leaf certificate's Subject Alternative
/// Name extension — the `iPAddress` `GeneralName`s an RFC 8738 order produces.
/// The IP twin of [`cert_dns_sans`]; empty on any parse failure.
pub fn cert_ip_sans(pem_data: &[u8]) -> Vec<std::net::IpAddr> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::pem::parse_x509_pem;

    let Ok((_, pem)) = parse_x509_pem(pem_data) else {
        return Vec::new();
    };
    let Ok(cert) = pem.parse_x509() else {
        return Vec::new();
    };
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            return san
                .general_names
                .iter()
                .filter_map(|gn| match gn {
                    // The raw SAN octets are 4 (v4) or 16 (v6).
                    GeneralName::IPAddress(raw) => match <[u8; 4]>::try_from(*raw) {
                        Ok(v4) => Some(std::net::IpAddr::from(v4)),
                        Err(_) => <[u8; 16]>::try_from(*raw).ok().map(std::net::IpAddr::from),
                    },
                    _ => None,
                })
                .collect();
        }
    }
    Vec::new()
}

/// Does the leaf in `pem_data` carry an `iPAddress` SAN for **every** address in
/// `addrs`? The IP twin of [`cert_covers_sans`], and how the § B-IP bridge
/// lifecycle notices that the interface set has moved out from under the chain
/// it holds (a floating IP attached post-boot): an address the cert does not
/// cover cannot be served for, so the bridge is re-ordered rather than kept
/// until its renewal lead comes due.
///
/// Fail-closed: an empty `addrs`, an unparseable chain, or one with no SAN
/// extension all report `false` (→ re-order), mirroring [`cert_covers_sans`].
pub fn cert_covers_ip_sans(pem_data: &[u8], addrs: &[std::net::IpAddr]) -> bool {
    if addrs.is_empty() {
        return false;
    }
    let present = cert_ip_sans(pem_data);
    addrs.iter().all(|a| present.contains(a))
}

/// Extract the DNS names from a leaf certificate's Subject Alternative Name
/// extension (lower-cased). Returns an empty vec on any parse failure.
pub fn cert_dns_sans(pem_data: &[u8]) -> Vec<String> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::pem::parse_x509_pem;

    let Ok((_, pem)) = parse_x509_pem(pem_data) else {
        return Vec::new();
    };
    let Ok(cert) = pem.parse_x509() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for gn in &san.general_names {
                if let GeneralName::DNSName(name) = gn {
                    out.push(name.to_ascii_lowercase());
                }
            }
        }
    }
    out
}

/// True iff the leaf certificate in `pem_data` covers every name in `desired`
/// (case-insensitive). Used by the caller's cert-lifecycle task to decide when a
/// newly-configured mail domain means the on-disk cert is stale and must be
/// re-issued with a wider SAN set.
pub fn cert_covers_sans(pem_data: &[u8], desired: &[String]) -> bool {
    let have = cert_dns_sans(pem_data);
    desired
        .iter()
        .all(|d| have.iter().any(|h| h == &d.to_ascii_lowercase()))
}

/// Filename for the persisted ACME issuance retry state (Gap B). Lives in
/// `acme_dir` alongside the cert so it shares the deployment's persistent
/// volume and survives container restarts and upgrades.
pub const RETRY_STATE_FILENAME: &str = "acme-retry-state.json";

/// Let's Encrypt's **failed-validation** budget: 5 per account, per hostname,
/// per rolling hour (<https://letsencrypt.org/docs/rate-limits/>). The lifecycle
/// task keeps a rolling log of recent *failed* issuance attempts and never
/// starts one that would exceed this within the trailing window — but, crucially,
/// retries as fast as the budget allows rather than on an arbitrary escalating
/// wait. We reserve one slot of headroom against the documented 5 so a manual
/// issuance / another tool sharing the account can't tip us over.
pub const ACME_FAILED_VALIDATION_BUDGET_PER_HOUR: usize = 4;
/// Rolling window the budget is measured over.
pub const ACME_RATE_WINDOW_SECS: u64 = 3600;

/// Persisted ACME issuance retry state: a rolling log of recent *failed*
/// validation attempts (unix seconds).
///
/// Persisted (not in-memory) so the budget survives process restarts — otherwise
/// a restart loop during a misconfiguration (port 80 firewalled at first boot)
/// would fire a fresh immediate attempt every boot and blow Let's Encrypt's
/// failed-validation budget. The list is the "log of recent attempts" the
/// scheduler ([`next_attempt_delay`]) reads to decide whether another attempt is
/// within budget and, if so, to retry promptly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryState {
    /// Unix-seconds of recent FAILED issuance attempts, oldest first. Pruned to
    /// the trailing [`ACME_RATE_WINDOW_SECS`]; the count within the window is the
    /// budget consumed.
    #[serde(default)]
    pub recent_failures: Vec<u64>,
}

impl RetryState {
    fn path(acme_dir: &std::path::Path) -> PathBuf {
        acme_dir.join(RETRY_STATE_FILENAME)
    }

    /// Load persisted retry state. Returns the healthy default when the file is
    /// missing or corrupt (never an error — a bad state file must not wedge
    /// issuance).
    pub fn load(acme_dir: &std::path::Path) -> Self {
        let path = Self::path(acme_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(
                    "ACME retry-state at {} is unreadable ({e}); starting healthy",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist retry state. Best-effort: failures are logged, never fatal.
    pub fn save(&self, acme_dir: &std::path::Path) {
        let path = Self::path(acme_dir);
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, bytes) {
                    tracing::warn!(
                        "failed to persist ACME retry-state to {}: {e}",
                        path.display()
                    );
                }
            }
            Err(e) => tracing::warn!("failed to serialize ACME retry-state: {e}"),
        }
    }

    /// Drop failure timestamps older than the rolling window.
    fn prune(&mut self, now: u64) {
        let cutoff = now.saturating_sub(ACME_RATE_WINDOW_SECS);
        self.recent_failures.retain(|&t| t > cutoff);
    }

    /// Number of failed attempts within the trailing window (the budget spent).
    pub fn failures_in_window(&self, now: u64) -> usize {
        let cutoff = now.saturating_sub(ACME_RATE_WINDOW_SECS);
        self.recent_failures.iter().filter(|&&t| t > cutoff).count()
    }

    /// Record a failed issuance attempt at `now` (unix seconds) and prune.
    pub fn record_failure(&mut self, now: u64) {
        self.recent_failures.push(now);
        self.prune(now);
    }

    /// Reset to healthy after a successful issuance.
    pub fn reset(&mut self) {
        self.recent_failures.clear();
    }
}

/// Delay until the next issuance attempt is permitted by the rate budget.
///
/// [`Duration::ZERO`](std::time::Duration::ZERO) = attempt now. This *expedites
/// within the limit*: with no recent failures the first attempt fires
/// immediately; while failing it paces attempts evenly across the window
/// (`window / budget`, ~15 min) so retries stay frequent yet spread out; once
/// the budget is spent it waits exactly until the oldest failure ages out of the
/// window (never longer, never exceeding [`ACME_FAILED_VALIDATION_BUDGET_PER_HOUR`]).
/// Called both at task start (so a restart *resumes* the budget rather than
/// resetting it) and after each failed attempt.
pub fn next_attempt_delay(state: &RetryState, now: u64) -> std::time::Duration {
    use std::time::Duration;
    let cutoff = now.saturating_sub(ACME_RATE_WINDOW_SECS);
    let mut recent: Vec<u64> = state
        .recent_failures
        .iter()
        .copied()
        .filter(|&t| t > cutoff)
        .collect();
    recent.sort_unstable();
    let count = recent.len();

    if count == 0 {
        return Duration::ZERO; // expedite: budget fully available
    }
    if count >= ACME_FAILED_VALIDATION_BUDGET_PER_HOUR {
        // Budget spent — wait until the oldest failure leaves the window, which
        // is the soonest another attempt stays within budget.
        let oldest = recent[0];
        let wait_until = oldest.saturating_add(ACME_RATE_WINDOW_SECS);
        return Duration::from_secs(wait_until.saturating_sub(now));
    }
    // Under budget: pace evenly (window / budget) from the last attempt so the
    // remaining budget is spread across the window instead of bursting.
    let min_interval = ACME_RATE_WINDOW_SECS / ACME_FAILED_VALIDATION_BUDGET_PER_HOUR as u64;
    let last = *recent.last().unwrap();
    let next_allowed = last.saturating_add(min_interval);
    Duration::from_secs(next_allowed.saturating_sub(now))
}

/// Current unix time in seconds (saturating to 0 before the epoch).
pub fn now_unix() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// A TLS certificate resolver that supports hot-reloading and a certless
/// "pending" state.
///
/// Uses `ArcSwapOption` to atomically swap the `CertifiedKey` so that in-flight
/// TLS handshakes always see a consistent certificate. A background file
/// watcher can call [`reload`](Self::reload) when the HTTP-01 ACME client renews certs.
///
/// The resolver may also start **pending** ([`pending`](Self::pending)): no cert
/// yet, [`resolve`](Self::resolve) returns `None` (the TLS handshake fails
/// cleanly) until the ACME lifecycle task obtains the first cert and the watcher
/// reloads it. This lets a cold-booted nest bind TLS on 443 immediately and
/// start serving HTTPS the instant the first cert lands — no process restart.
pub struct ReloadableCertResolver {
    inner: ArcSwapOption<CertifiedKey>,
}

impl ReloadableCertResolver {
    /// Load a `CertifiedKey` from PEM files and create a new (already-loaded)
    /// resolver — the boot-with-cert path.
    pub fn from_pem(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let key = load_certified_key(cert_path, key_path)?;
        Ok(Self {
            inner: ArcSwapOption::from_pointee(key),
        })
    }

    /// Create an (already-loaded) resolver from an in-memory
    /// [`CertifiedKey`] — for installing a freshly-minted cert without a
    /// disk round-trip.
    pub fn from_certified_key(key: CertifiedKey) -> Self {
        Self {
            inner: ArcSwapOption::from_pointee(key),
        }
    }

    /// Create a resolver with no cert yet — the cold-boot-no-cert path.
    ///
    /// [`resolve`](Self::resolve) returns `None` (handshakes fail) until
    /// [`reload`](Self::reload) is called with a freshly-issued cert.
    pub fn pending() -> Self {
        Self {
            inner: ArcSwapOption::empty(),
        }
    }

    /// The currently-loaded cert, or `None` if still pending. Primarily for tests.
    pub fn current(&self) -> Option<Arc<CertifiedKey>> {
        self.inner.load_full()
    }

    /// Hot-reload the certificate from disk. The swap is atomic — existing
    /// connections keep using the old cert, new handshakes get the new one.
    /// On a pending resolver this is the moment it starts serving HTTPS.
    pub fn reload(&self, cert_path: &Path, key_path: &Path) -> Result<()> {
        let key = load_certified_key(cert_path, key_path)?;
        self.inner.store(Some(Arc::new(key)));
        tracing::info!("TLS certificate reloaded from {}", cert_path.display());
        Ok(())
    }
}

/// Load a `CertifiedKey` from PEM cert-chain + private-key files.
///
/// Shared by both [`ReloadableCertResolver`] (the nest's own listener cert) and
/// [`MultiDomainCertResolver`] (the default cert + per-custom-domain certs).
pub fn load_certified_key(cert_path: &Path, key_path: &Path) -> Result<CertifiedKey> {
    let cert_pem = std::fs::read(cert_path).context("read cert")?;
    let key_pem = std::fs::read(key_path).context("read key")?;
    load_certified_key_from_pem(&cert_pem, &key_pem)
}

/// Build a [`CertifiedKey`] from in-memory PEM (cert chain + private key) — the
/// in-memory twin of [`load_certified_key`]. Used to install a freshly-minted
/// self-signed floor cert into the resolver without round-tripping through disk.
pub fn load_certified_key_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<CertifiedKey> {
    use rustls::pki_types::pem::{self, PemObject};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs: Vec<_> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("parse certs: {e}"))?;

    let key_der = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| match e {
        pem::Error::NoItemsFound => anyhow::anyhow!("no private key in PEM"),
        e => anyhow::anyhow!("parse key: {e}"),
    })?;

    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
        .map_err(|e| anyhow::anyhow!("unsupported key type: {e}"))?;

    Ok(CertifiedKey::new(certs, signing_key))
}

impl fmt::Debug for ReloadableCertResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReloadableCertResolver").finish()
    }
}

impl ResolvesServerCert for ReloadableCertResolver {
    /// Returns the loaded cert, or `None` while still pending (no cert issued
    /// yet) — rustls then aborts the handshake instead of serving a bad cert.
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.inner.load_full()
    }
}

/// Test-only helpers shared with consumers' test suites (enable the
/// `test-helpers` feature from a `[dev-dependencies]` re-declaration).
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support {
    /// Build a self-signed PEM certificate carrying `sans` as DNS SANs.
    pub fn make_multi_san_cert_pem(sans: &[&str]) -> Vec<u8> {
        make_multi_san_cert_and_key_pem(sans).0
    }

    /// [`make_multi_san_cert_pem`] plus the matching private key, for the
    /// callers that must load the pair back through `load_certified_key` — the
    /// § B-IP bridge lifecycle writes `ip-fullchain.pem` **and**
    /// `ip-privkey.pem` and installs both into the resolver, so a cert alone
    /// cannot stand in for it.
    ///
    /// An entry of `sans` that parses as an IP literal becomes an `iPAddress`
    /// SAN rather than a `dNSName` — rcgen's own classification, which is what
    /// makes an IP-identifier order's CSR correct and is pinned in
    /// `fauna_acme_core`.
    pub fn make_multi_san_cert_and_key_pem(sans: &[&str]) -> (Vec<u8>, Vec<u8>) {
        let key = rcgen::KeyPair::generate().expect("keypair");
        let params =
            rcgen::CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .expect("params");
        let cert = params.self_signed(&key).expect("self-signed");
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::make_multi_san_cert_pem;
    use super::*;

    #[tokio::test]
    async fn challenge_state_set_and_get() {
        let state = ChallengeState::new();
        state.set("token-1".into(), "key-auth-1".into()).await;
        assert_eq!(state.get("token-1").await, Some("key-auth-1".to_string()));
    }

    #[tokio::test]
    async fn challenge_state_remove() {
        let state = ChallengeState::new();
        state.set("token-1".into(), "key-auth-1".into()).await;
        state.remove("token-1").await;
        assert_eq!(state.get("token-1").await, None);
    }

    #[tokio::test]
    async fn challenge_state_multiple_tokens() {
        let state = ChallengeState::new();
        state.set("a".into(), "auth-a".into()).await;
        state.set("b".into(), "auth-b".into()).await;
        state.set("c".into(), "auth-c".into()).await;
        assert_eq!(state.get("a").await, Some("auth-a".to_string()));
        assert_eq!(state.get("b").await, Some("auth-b".to_string()));
        assert_eq!(state.get("c").await, Some("auth-c".to_string()));
        assert_eq!(state.get("d").await, None);
    }

    #[tokio::test]
    async fn challenge_state_overwrite() {
        let state = ChallengeState::new();
        state.set("token".into(), "old".into()).await;
        state.set("token".into(), "new".into()).await;
        assert_eq!(state.get("token").await, Some("new".to_string()));
    }

    // -------------------------------------------------------------------------
    // RetryState / backoff persistence tests (Gap B: backoff survives restart)
    // -------------------------------------------------------------------------

    #[test]
    fn retry_state_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = RetryState::default();
        state.record_failure(1_000);
        state.record_failure(2_000);
        assert_eq!(state.recent_failures, vec![1_000, 2_000]);
        state.save(dir.path());

        let loaded = RetryState::load(dir.path());
        assert_eq!(loaded.recent_failures, vec![1_000, 2_000]);
    }

    #[test]
    fn retry_state_load_defaults_when_missing_or_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Missing file → default (healthy), never an error.
        let fresh = RetryState::load(dir.path());
        assert!(fresh.recent_failures.is_empty());

        // Corrupt file → default, not a panic.
        std::fs::write(dir.path().join(RETRY_STATE_FILENAME), b"not json").unwrap();
        let corrupt = RetryState::load(dir.path());
        assert!(corrupt.recent_failures.is_empty());
    }

    #[test]
    fn retry_state_record_prunes_outside_window_and_reset_clears() {
        let mut state = RetryState::default();
        let now = 1_000_000;
        // An old failure (just outside the window) is pruned on the next record.
        state.record_failure(now - ACME_RATE_WINDOW_SECS - 1);
        state.record_failure(now);
        assert_eq!(state.recent_failures, vec![now], "stale failure pruned");
        assert_eq!(state.failures_in_window(now), 1);
        state.reset();
        assert!(state.recent_failures.is_empty());
    }

    #[test]
    fn next_attempt_delay_expedites_within_budget_and_holds_at_the_limit() {
        use std::time::Duration;
        let now = 1_000_000u64;

        // No recent failures → attempt immediately (expedite).
        assert_eq!(
            next_attempt_delay(&RetryState::default(), now),
            Duration::ZERO
        );

        // Under budget → paced evenly (window / budget) from the last attempt.
        let one = RetryState {
            recent_failures: vec![now],
        };
        let min_interval = ACME_RATE_WINDOW_SECS / ACME_FAILED_VALIDATION_BUDGET_PER_HOUR as u64;
        assert_eq!(
            next_attempt_delay(&one, now),
            Duration::from_secs(min_interval),
            "under budget, the next attempt is paced one even slice away"
        );
        // …and that slice elapsing makes it immediate again.
        assert_eq!(next_attempt_delay(&one, now + min_interval), Duration::ZERO);

        // Budget spent (BUDGET failures within the window) → wait exactly until
        // the OLDEST one ages out, never longer.
        let spent = RetryState {
            recent_failures: (0..ACME_FAILED_VALIDATION_BUDGET_PER_HOUR as u64)
                .map(|i| now + i) // all within the window, oldest = now
                .collect(),
        };
        assert_eq!(
            next_attempt_delay(&spent, now + 10),
            Duration::from_secs(ACME_RATE_WINDOW_SECS - 10),
            "at the budget, hold until the oldest failure leaves the rolling hour"
        );

        // Once the oldest ages out, the window has room again → immediate.
        assert_eq!(
            next_attempt_delay(&spent, now + ACME_RATE_WINDOW_SECS + 1),
            Duration::ZERO
        );
    }

    // -------------------------------------------------------------------------
    // cert_lifecycle_loop integration tests (Gap B glue: resume/persist).
    //
    // These drive the real loop with a fake issuer (no ACME network) and a
    // paused tokio clock, so they assert the actual resume-on-boot decision
    // rather than just the pure scheduler math. `RetryState`'s failure
    // timestamps are real wall-clock seconds; the loop's `tokio::time::sleep` is
    // the only pending timer, so the paused runtime auto-advances through it and
    // the fake issuer observes how long it waited via `tokio::time::Instant`.
    // -------------------------------------------------------------------------

    /// The `[acme].directory_url` override is the CA (so a deployment can target a
    /// private/internal ACME CA — or the tier_4 `pebble`); absent one, the CA is
    /// Let's Encrypt production — never staging, which only a caller naming
    /// [`LETS_ENCRYPT_STAGING_URL`] reaches.
    #[test]
    fn directory_url_override_else_lets_encrypt_production() {
        use instant_acme::LetsEncrypt;
        assert_eq!(acme_directory_url(None), LetsEncrypt::Production.url());
        let pebble = "https://pebble:14000/dir";
        assert_eq!(acme_directory_url(Some(pebble)), pebble);
        assert_eq!(
            LETS_ENCRYPT_STAGING_URL,
            LetsEncrypt::Staging.url(),
            "the exported staging constant must be the CA's own staging directory"
        );
    }

    #[tokio::test]
    async fn challenge_state_remove_nonexistent() {
        let state = ChallengeState::new();
        // Should not panic.
        state.remove("nonexistent").await;
        assert_eq!(state.get("nonexistent").await, None);
    }

    #[test]
    fn challenge_state_default_is_empty() {
        let state = ChallengeState::default();
        // The default state should have an empty map.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            assert_eq!(state.get("anything").await, None);
        });
    }

    #[tokio::test]
    async fn http01_router_serves_challenge() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower_service::Service;

        let state = Arc::new(ChallengeState::new());
        state.set("test-token".into(), "test-key-auth".into()).await;

        let mut app = http01_router(state, "example.com".into());

        let req = Request::builder()
            .uri("/.well-known/acme-challenge/test-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"test-key-auth");
    }

    #[tokio::test]
    async fn http01_router_returns_404_for_unknown_token() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower_service::Service;

        let state = Arc::new(ChallengeState::new());
        let mut app = http01_router(state, "example.com".into());

        let req = Request::builder()
            .uri("/.well-known/acme-challenge/missing")
            .body(Body::empty())
            .unwrap();

        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http01_router_redirects_other_paths() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower_service::Service;

        let state = Arc::new(ChallengeState::new());
        let mut app = http01_router(state, "example.com".into());

        let req = Request::builder()
            .uri("/some/page?q=1")
            .body(Body::empty())
            .unwrap();

        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(location, "https://example.com/some/page?q=1");
    }

    #[tokio::test]
    async fn http01_router_redirect_echoes_request_host() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower_service::Service;

        let state = Arc::new(ChallengeState::new());
        let mut app = http01_router(state, "example.com".into());

        // The redirect targets the host the client actually asked for (with
        // any port stripped — HTTPS lives on 443), not the configured apex: a
        // plain-HTTP hit on a custom web domain or subdomain must bounce to
        // ITS https origin, and a domainless box (empty apex) still redirects.
        let req = Request::builder()
            .uri("/some/page?q=1")
            .header("host", "custom.test:8080")
            .body(Body::empty())
            .unwrap();

        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(location, "https://custom.test/some/page?q=1");
    }

    #[tokio::test]
    async fn http01_router_no_host_and_no_domain_is_bad_request() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower_service::Service;

        let state = Arc::new(ChallengeState::new());
        // Domainless boot: empty fallback domain. A Host-less request then has
        // no redirect target at all — a hostless `https://` Location would be
        // malformed, so answer 400 instead.
        let mut app = http01_router(state, String::new());

        let req = Request::builder()
            .uri("/some/page")
            .body(Body::empty())
            .unwrap();

        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn cert_dns_sans_lists_all_dns_names() {
        let pem = make_multi_san_cert_pem(&["a.example.com", "mail.a.example.com"]);
        let mut got = cert_dns_sans(&pem);
        got.sort();
        assert_eq!(
            got,
            vec![
                "a.example.com".to_string(),
                "mail.a.example.com".to_string()
            ]
        );
    }

    #[test]
    fn cert_covers_sans_true_when_all_present() {
        let pem = make_multi_san_cert_pem(&["example.com", "mail.example.com"]);
        assert!(cert_covers_sans(
            &pem,
            &["example.com".to_string(), "mail.example.com".to_string()]
        ));
    }

    #[test]
    fn cert_covers_sans_false_when_mail_host_missing() {
        // An apex-only cert (today's behavior) does NOT cover mail.<domain> —
        // exactly the staleness cert_lifecycle_task must detect to re-issue.
        let pem = make_multi_san_cert_pem(&["example.com"]);
        assert!(!cert_covers_sans(
            &pem,
            &["example.com".to_string(), "mail.example.com".to_string()]
        ));
        assert!(cert_covers_sans(&pem, &["example.com".to_string()]));
    }

    #[test]
    fn cert_covers_sans_is_case_insensitive() {
        let pem = make_multi_san_cert_pem(&["Mail.Example.com"]);
        assert!(cert_covers_sans(&pem, &["mail.example.com".to_string()]));
    }

    // -------------------------------------------------------------------------
    // load_certified_key_from_pem — the PEM shapes the loader accepts
    // -------------------------------------------------------------------------

    use super::test_support::make_multi_san_cert_and_key_pem;

    #[test]
    fn pem_loader_reads_a_full_chain_and_a_pkcs8_key() {
        let (leaf, key) = make_multi_san_cert_and_key_pem(&["example.com"]);
        let intermediate = make_multi_san_cert_pem(&["intermediate.example"]);
        let chain = [leaf, intermediate].concat();
        let ck = load_certified_key_from_pem(&chain, &key).expect("load");
        assert_eq!(
            ck.cert.len(),
            2,
            "every CERTIFICATE block is kept, in order"
        );
    }

    #[test]
    fn pem_loader_skips_text_and_foreign_blocks() {
        // A combined file: explanatory text, a CERTIFICATE block, then the key
        // — the key reader takes the first private-key block and ignores the
        // rest, and the cert reader ignores the key block.
        let (cert, key) = make_multi_san_cert_and_key_pem(&["example.com"]);
        let combined = [b"issued by a test\n".as_slice(), &cert, &key].concat();
        let ck = load_certified_key_from_pem(&combined, &combined).expect("load");
        assert_eq!(ck.cert.len(), 1);
    }

    #[test]
    fn pem_loader_rejects_a_key_file_without_a_key() {
        let (cert, _) = make_multi_san_cert_and_key_pem(&["example.com"]);
        let err = load_certified_key_from_pem(&cert, &cert).expect_err("no key");
        assert!(err.to_string().contains("no private key in PEM"), "{err:#}");
    }

    #[test]
    fn pem_loader_rejects_a_malformed_certificate_block() {
        let (_, key) = make_multi_san_cert_and_key_pem(&["example.com"]);
        let bad = b"-----BEGIN CERTIFICATE-----\n!!not base64!!\n-----END CERTIFICATE-----\n";
        assert!(load_certified_key_from_pem(bad, &key).is_err());
    }

    #[test]
    fn pem_loader_accepts_an_empty_chain() {
        // The loader does not police chain length; the resolver serves what it
        // is given (the iroh relay's twin adds its own non-empty check).
        let (_, key) = make_multi_san_cert_and_key_pem(&["example.com"]);
        let ck = load_certified_key_from_pem(b"", &key).expect("load");
        assert!(ck.cert.is_empty());
    }
}
