//! The Outlook source's sign-in: the OAuth 2.0 device authorization grant
//! (RFC 8628) against the Microsoft identity platform.
//!
//! `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the
//! device-code grant owns this surface. Microsoft has refused a username and
//! password from mail apps since 2024-09-16, so the Outlook source signs in
//! here and hands the access token to
//! [`super::ImapSession::authenticate_xoauth2`]. The whole dance runs on the
//! CLIENT: no RPC carries a code, a token or the authority's address, and no
//! nest route exists for it — the Bluesky link, which terminates on the nest,
//! is the pattern this flow must never reuse.
//!
//! # Why a device code and not a redirect
//!
//! It has no redirect URI, so one public-client registration serves every
//! nest and every platform, and it runs wherever the app runs — a terminal
//! over SSH, a sandboxed phone app, a desktop — because the person signs in
//! from any browser on any device. The doc's ruling has the full reasoning.
//!
//! # Shape
//!
//! Everything here is sans-io over two seams: [`OAuthHttp`] (one HTTPS form
//! POST — the flow's only I/O, implemented by each platform shell beside its
//! `ImapTransport`) and [`super::ImapClock`] (the poll cadence and the code's
//! lifetime). [`DeviceFlow::start`] asks for a code, [`DeviceFlow::poll`]
//! makes one token request and folds the answer, [`DeviceFlow::run`] drives
//! the polls to a terminal [`FlowOutcome`], and [`refresh`] trades a refresh
//! token for a fresh access token on a reconnect. The tokens are never
//! decoded or validated — Microsoft's own warning: they may be opaque or
//! encrypted for personal accounts.

use core::fmt;

use fauna_core::secret::SecretString;
use serde::Deserialize;

use super::ImapClock;

/// The Fauna project's own Microsoft app registration and the endpoints it
/// signs in against — bucket 1 of the one configuration surface
/// (`principles.md` § One configuration surface): no human in a deployment
/// chooses any of it, so it is a constant, never nest state or an app
/// setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicrosoftIdentity {
    /// The Microsoft identity platform's host. The v2.0 authority for a
    /// directory is `<login_host>/<tenant>/oauth2/v2.0` ([`Self::authority`]).
    pub login_host: &'static str,
    /// The project registration's directory: `common` — work, school and
    /// personal accounts alike.
    pub tenant: &'static str,
    /// Appended to the authority: where a code is asked for.
    pub device_code_path: &'static str,
    /// Appended to the authority: where the code is redeemed and a refresh
    /// token traded.
    pub token_path: &'static str,
    /// The Outlook IMAP resource's delegated permission plus
    /// `offline_access` (for the refresh token). Never Microsoft Graph — a
    /// Graph token is refused at `AUTHENTICATE XOAUTH2`.
    pub scope: &'static str,
    /// The public client's Application (client) ID. A public client's ID is
    /// public and ships in source. Empty until the owner creates the
    /// registration; while it is, the sign-in is unavailable
    /// ([`Self::is_available`]) and no request is ever made.
    pub client_id: &'static str,
}

/// The Outlook source's identity platform (§ Microsoft sign-in — the
/// device-code grant).
pub const MICROSOFT: MicrosoftIdentity = MicrosoftIdentity {
    login_host: "https://login.microsoftonline.com",
    tenant: fauna_protocol::microsoft_registration::DEFAULT_TENANT,
    device_code_path: "/devicecode",
    token_path: "/token",
    scope: "https://outlook.office.com/IMAP.AccessAsUser.All offline_access",
    client_id: "",
};

impl MicrosoftIdentity {
    /// Whether the registration exists yet. Until it does, the button stays
    /// inert with a sentence saying the sign-in is not available — never a
    /// failing request.
    pub fn is_available(&self) -> bool {
        !self.client_id.is_empty()
    }

    /// The v2.0 authority for `tenant`.
    pub fn authority(&self, tenant: &str) -> String {
        format!("{}/{tenant}/oauth2/v2.0", self.login_host)
    }

    /// The project's own registration's endpoints —
    /// [`MicrosoftRegistration::project`]'s client.
    pub fn client(&self) -> OAuthClient {
        self.client_for(self.client_id, self.tenant)
    }

    /// The ONE door that builds the flow's endpoints — so the e2e authority
    /// seed (`e2e-gated-seeds.md` § The Microsoft authority
    /// seed) lives in one place and no consumer can be the one that forgot
    /// it. The seed replaces the whole authority and ignores `tenant`, so a
    /// fake far end still receives the effective client ID.
    fn client_for(&self, client_id: &str, tenant: &str) -> OAuthClient {
        let base = authority_base(&self.authority(tenant));
        OAuthClient {
            device_code_url: format!("{base}{}", self.device_code_path),
            token_url: format!("{base}{}", self.token_path),
            client_id: client_id.to_string(),
            scope: self.scope.to_string(),
        }
    }
}

/// The registration a sign-in uses — the client ID and the directory —
/// resolved between the deployment's own (an admin's, bucket 2, read off
/// `fauna.setup.status`) and the project's constant [`MICROSOFT`] (bucket 1).
/// `mailbox-migration.md` § Microsoft sign-in, *An admin-supplied
/// registration*, owns the precedence; [`Self::effective`] is its one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftRegistration {
    /// The Application (client) ID; empty only for the project's while its
    /// constant is unfilled.
    pub client_id: String,
    /// The directory the authority is built for.
    pub tenant: String,
}

impl MicrosoftRegistration {
    /// The project's own registration, [`MICROSOFT`].
    pub fn project() -> Self {
        Self {
            client_id: MICROSOFT.client_id.to_string(),
            tenant: MICROSOFT.tenant.to_string(),
        }
    }

    /// The admin's registration as `fauna.setup.status` carries it. An
    /// absent tenant (an older writer, a hand-made fixture) is `common`.
    pub fn from_status(
        status: &fauna_protocol::microsoft_registration::MicrosoftRegistrationStatus,
    ) -> Self {
        let tenant = status.tenant.trim();
        Self {
            client_id: status.client_id.trim().to_string(),
            tenant: if tenant.is_empty() {
                fauna_protocol::microsoft_registration::DEFAULT_TENANT.to_string()
            } else {
                tenant.to_string()
            },
        }
    }

    /// The pair every app signs in with: the admin's when one is set (and
    /// has a client ID), else the project's. An admin-set registration makes
    /// the sign-in available even while the project's constant is unfilled —
    /// the deployment's registration is a complete one.
    pub fn effective(admin: Option<&MicrosoftRegistration>) -> Self {
        match admin {
            Some(admin) if !admin.client_id.is_empty() => Self {
                client_id: admin.client_id.clone(),
                tenant: if admin.tenant.is_empty() {
                    fauna_protocol::microsoft_registration::DEFAULT_TENANT.to_string()
                } else {
                    admin.tenant.clone()
                },
            },
            _ => Self::project(),
        }
    }

    /// Whether a sign-in can be attempted: the button is live exactly when
    /// this holds (and the platform has the HTTPS seam).
    pub fn is_available(&self) -> bool {
        !self.client_id.is_empty()
    }

    /// The endpoints and identity this registration signs in with, through
    /// [`MICROSOFT`]'s one door.
    pub fn client(&self) -> OAuthClient {
        MICROSOFT.client_for(&self.client_id, &self.tenant)
    }
}

/// The resolved endpoints plus the client's identity — what every request in
/// this module is made with. Built by [`MicrosoftIdentity::client`]; a test
/// builds one directly against its own far end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClient {
    pub device_code_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scope: String,
}

/// The Microsoft authority seed's env var: an absolute `https://` base URL
/// that stands in for the whole [`MicrosoftIdentity::authority`], so the Outlook
/// sign-in's witnesses run against a fake far end and never reach Microsoft.
///
/// Gated exactly as the source-IMAP trust seed
/// (`super::native::E2E_EXTRA_CA_ENV`) is: the constant itself is compiled
/// out of a release artifact, so a `strings` sweep finds not even the name
/// (convention 15). It redirects the identity far end alone; the client ID
/// and the scopes are not seeded, and the fake's certificate is trusted
/// through the IMAP seed's PEM, so no second trust variable exists.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub const E2E_AUTHORITY_ENV: &str = "FAUNA_E2E_MICROSOFT_AUTHORITY";

/// The authority every request goes to: the seed's, when a test-capable
/// build has it set, else the constant's.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn authority_base(default: &str) -> String {
    seeded_authority(std::env::var(E2E_AUTHORITY_ENV).ok().as_deref(), default)
}

/// The production twin: a shipped build has no authority seed (convention
/// 15's same-signature no-op twin — [`MicrosoftIdentity::client`] is the
/// plumbing the app compiles unconditionally).
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
fn authority_base(default: &str) -> String {
    default.to_string()
}

/// The seed's decision, pure so it is testable without touching the
/// process environment. An unset or blank seed is the constant. A seed that
/// is not `https://` **panics**: a token must never cross a plaintext hop,
/// even in a test, and only the harness sets this variable, so a bad value
/// is a harness bug that should say so rather than read like a sign-in bug —
/// the source-IMAP trust seed's reasoning.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn seeded_authority(seed: Option<&str>, default: &str) -> String {
    match seed.map(str::trim) {
        None | Some("") => default.to_string(),
        Some(seed) if seed.starts_with("https://") => seed.trim_end_matches('/').to_string(),
        Some(seed) => panic!(
            "{E2E_AUTHORITY_ENV} must be an https:// URL, got {seed:?}: a token never \
             crosses a plaintext hop. Only the e2e harness sets this variable, so \
             this is a harness bug."
        ),
    }
}

/// Where an organisation's administrator grants the project's registration
/// the consent a tenant may require (the `AADSTS65001` family) — the address
/// the consent refusal's sentence shows.
pub fn admin_consent_url(client_id: &str) -> String {
    format!("https://login.microsoftonline.com/organizations/adminconsent?client_id={client_id}")
}

/// One answer from the identity platform. The body may hold tokens, so it
/// is held as secret text and zeroized on drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthHttpResponse {
    pub status: u16,
    pub body: SecretString,
}

/// The sign-in's only I/O: one HTTPS POST of an
/// `application/x-www-form-urlencoded` body to an absolute URL, the status
/// and body back. A third seam beside [`super::ImapTransport`] and
/// [`super::ImapClock`] (`mailbox-migration.md` § Where the IMAP client
/// runs): native rides the `reqwest` client the apps already carry
/// (`super::native::NativeOAuthHttp`); web needs the blind byte tunnel,
/// because `/devicecode` and `/token` send no CORS headers, and lands with
/// the relay.
///
/// AFIT for the same per-impl `Send` inference as the other two seams.
#[allow(async_fn_in_trait)] // Static dispatch only — see `ImapTransport`.
pub trait OAuthHttp {
    /// Transport-specific error, bounded `Display` so [`OAuthError::Http`]
    /// can render it.
    type Error: fmt::Display;

    /// POST `form` (already form-encoded) to `url`. Implementations refuse
    /// anything but `https://` and follow no redirect. A non-2xx status is an
    /// answer, not an error: the token endpoint says "keep polling" with a
    /// 400.
    async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, Self::Error>;
}

/// What can stop a request short of a usable answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// The registration is not filled yet ([`MicrosoftIdentity::is_available`]),
    /// so no request was made.
    #[error("Microsoft sign-in is not available yet")]
    Unavailable,
    /// The request did not complete — no network, a TLS failure.
    #[error("the identity platform could not be reached: {0}")]
    Http(String),
    /// The platform answered with something this module cannot read.
    #[error("unexpected answer from the identity platform: {0}")]
    Protocol(String),
    /// The platform refused the request (an OAuth `error` code that is not
    /// one of the device flow's own states — `invalid_client`, a refused
    /// refresh's `invalid_grant`, …).
    #[error("the identity platform refused the request: {0}")]
    Refused(String),
}

/// The code the person types, and where. `device_code` — the secret half
/// the app redeems — is kept private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub verification_uri: String,
    /// When the code dies, on the [`ImapClock`] the flow was started with.
    pub expires_at_ms: u64,
    device_code: SecretString,
}

/// A granted (or refreshed) token pair. Never decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGrant {
    pub access_token: SecretString,
    /// Absent only when the platform sent none — `offline_access` asks for
    /// one, and a refresh that returns none leaves the previous one valid.
    pub refresh_token: Option<SecretString>,
    /// When the access token stops being accepted, on the flow's clock.
    pub expires_at_ms: u64,
}

/// One token poll's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollStep {
    /// `authorization_pending` — the person has not finished yet.
    Pending,
    /// `slow_down` — polling too fast; the interval has been lengthened.
    SlowDown,
    /// The flow ended.
    Done(FlowOutcome),
}

/// How a sign-in attempt ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowOutcome {
    Granted(TokenGrant),
    /// `authorization_declined` — the person said no.
    Declined,
    /// `expired_token`, or the code's lifetime ran out before a grant.
    Expired,
    /// The tenant needs an administrator's consent first (the `AADSTS65001`
    /// family, or `consent_required`).
    AdminConsentRequired,
    /// The caller stopped the attempt.
    Cancelled,
    /// Anything else that ends the attempt — `bad_verification_code`, a
    /// refusal, an unreadable answer. The text goes to the log, never to the
    /// person.
    Failed(String),
}

/// RFC 8628 § 3.5: `slow_down` lengthens the interval by five seconds.
const SLOW_DOWN_STEP_MS: u64 = 5_000;
/// RFC 8628 § 3.2: the interval when the server sends none.
const DEFAULT_INTERVAL_MS: u64 = 5_000;

/// One device-code sign-in attempt: the code, the poll cadence, and the
/// moment of the next allowed poll.
#[derive(Debug, Clone)]
pub struct DeviceFlow {
    client: OAuthClient,
    authorization: DeviceAuthorization,
    interval_ms: u64,
    next_poll_at_ms: u64,
}

impl DeviceFlow {
    /// Ask for a code (`POST /devicecode` with `client_id` and `scope`).
    /// Refuses with [`OAuthError::Unavailable`] — without a request — while
    /// the client ID is empty.
    pub async fn start<H: OAuthHttp>(
        http: &H,
        client: &OAuthClient,
        now_ms: u64,
    ) -> Result<Self, OAuthError> {
        if client.client_id.is_empty() {
            return Err(OAuthError::Unavailable);
        }
        let form = encode_form(&[("client_id", &client.client_id), ("scope", &client.scope)]);
        let response = post(http, &client.device_code_url, &form).await?;
        if !(200..300).contains(&response.status) {
            let error: ErrorBody = parse(&response.body)?;
            return Err(OAuthError::Refused(error.error));
        }
        let body: DeviceCodeBody = parse(&response.body)?;
        let interval_ms = body
            .interval
            .map_or(DEFAULT_INTERVAL_MS, |s| s.saturating_mul(1_000).max(1_000));
        Ok(Self {
            client: client.clone(),
            authorization: DeviceAuthorization {
                user_code: body.user_code,
                verification_uri: body.verification_uri,
                expires_at_ms: now_ms.saturating_add(body.expires_in.saturating_mul(1_000)),
                device_code: body.device_code,
            },
            interval_ms,
            next_poll_at_ms: now_ms.saturating_add(interval_ms),
        })
    }

    /// The code and address to show.
    pub fn authorization(&self) -> &DeviceAuthorization {
        &self.authorization
    }

    /// The current poll interval — RFC 8628's `interval`, lengthened by each
    /// `slow_down`.
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    /// How long to wait at `now_ms` before the next poll is allowed.
    pub fn next_poll_delay_ms(&self, now_ms: u64) -> u64 {
        self.next_poll_at_ms.saturating_sub(now_ms)
    }

    /// One token request (`grant_type=urn:ietf:params:oauth:grant-type:device_code`),
    /// folded into a [`PollStep`]. A code whose lifetime has run out ends as
    /// [`FlowOutcome::Expired`] without a request.
    pub async fn poll<H: OAuthHttp>(
        &mut self,
        http: &H,
        now_ms: u64,
    ) -> Result<PollStep, OAuthError> {
        if now_ms >= self.authorization.expires_at_ms {
            return Ok(PollStep::Done(FlowOutcome::Expired));
        }
        let form = encode_form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", &self.client.client_id),
            ("device_code", self.authorization.device_code.as_str()),
        ]);
        let response = post(http, &self.client.token_url, &form).await?;
        self.next_poll_at_ms = now_ms.saturating_add(self.interval_ms);
        if (200..300).contains(&response.status) {
            let grant = token_grant(&response.body, now_ms)?;
            return Ok(PollStep::Done(FlowOutcome::Granted(grant)));
        }
        let error: ErrorBody = parse(&response.body)?;
        Ok(match error.error.as_str() {
            "authorization_pending" => PollStep::Pending,
            "slow_down" => {
                self.interval_ms = self.interval_ms.saturating_add(SLOW_DOWN_STEP_MS);
                self.next_poll_at_ms = now_ms.saturating_add(self.interval_ms);
                PollStep::SlowDown
            }
            "authorization_declined" => PollStep::Done(FlowOutcome::Declined),
            "expired_token" => PollStep::Done(FlowOutcome::Expired),
            _ if error.is_consent_refusal() => PollStep::Done(FlowOutcome::AdminConsentRequired),
            other => PollStep::Done(FlowOutcome::Failed(other.to_string())),
        })
    }

    /// Poll until the flow ends: sleep out each interval on `clock`, ask
    /// `cancelled` before every request, and stop at the code's expiry. A
    /// request that fails (no network) ends the attempt as
    /// [`FlowOutcome::Failed`] — the person presses the button again.
    pub async fn run<H, C, F>(&mut self, http: &H, clock: &C, mut cancelled: F) -> FlowOutcome
    where
        H: OAuthHttp,
        C: ImapClock,
        F: FnMut() -> bool,
    {
        loop {
            let delay = self.next_poll_delay_ms(clock.now_ms());
            if delay > 0 {
                clock.sleep_ms(delay).await;
            }
            if cancelled() {
                return FlowOutcome::Cancelled;
            }
            match self.poll(http, clock.now_ms()).await {
                Ok(PollStep::Pending | PollStep::SlowDown) => {}
                Ok(PollStep::Done(outcome)) => return outcome,
                Err(e) => return FlowOutcome::Failed(e.to_string()),
            }
        }
    }
}

/// Trade a refresh token for a fresh access token
/// (`grant_type=refresh_token`) — run once on a reconnect when the access
/// token has under five minutes left (§ Microsoft sign-in, *Lifetime and
/// refresh*). A refused refresh is [`OAuthError::Refused`]; the caller ends
/// the import and asks the person to sign in again. When the platform sends
/// no new refresh token the returned grant carries none and the caller
/// keeps the one it had.
pub async fn refresh<H: OAuthHttp>(
    http: &H,
    client: &OAuthClient,
    refresh_token: &SecretString,
    now_ms: u64,
) -> Result<TokenGrant, OAuthError> {
    if client.client_id.is_empty() {
        return Err(OAuthError::Unavailable);
    }
    let form = encode_form(&[
        ("grant_type", "refresh_token"),
        ("client_id", &client.client_id),
        ("refresh_token", refresh_token.as_str()),
        ("scope", &client.scope),
    ]);
    let response = post(http, &client.token_url, &form).await?;
    if (200..300).contains(&response.status) {
        return token_grant(&response.body, now_ms);
    }
    let error: ErrorBody = parse(&response.body)?;
    Err(OAuthError::Refused(error.error))
}

async fn post<H: OAuthHttp>(
    http: &H,
    url: &str,
    form: &SecretString,
) -> Result<OAuthHttpResponse, OAuthError> {
    http.post_form(url, form.as_str())
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))
}

fn parse<'a, T: Deserialize<'a>>(body: &'a SecretString) -> Result<T, OAuthError> {
    // The serde error names a position, never the body's text.
    serde_json::from_str(body.as_str()).map_err(|e| OAuthError::Protocol(e.to_string()))
}

fn token_grant(body: &SecretString, now_ms: u64) -> Result<TokenGrant, OAuthError> {
    let body: TokenBody = parse(body)?;
    if body.access_token.is_empty() {
        return Err(OAuthError::Protocol(
            "the token answer carried no access token".into(),
        ));
    }
    Ok(TokenGrant {
        access_token: body.access_token,
        refresh_token: body.refresh_token.filter(|t| !t.is_empty()),
        expires_at_ms: now_ms.saturating_add(body.expires_in.saturating_mul(1_000)),
    })
}

#[derive(Deserialize)]
struct DeviceCodeBody {
    user_code: String,
    device_code: SecretString,
    verification_uri: String,
    #[serde(deserialize_with = "seconds")]
    expires_in: u64,
    #[serde(default, deserialize_with = "optional_seconds")]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: SecretString,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(deserialize_with = "seconds")]
    expires_in: u64,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    error_description: String,
    #[serde(default)]
    error_codes: Vec<u64>,
}

impl ErrorBody {
    /// A tenant that needs an administrator's consent: `consent_required`, or
    /// the `AADSTS65001` family Microsoft sends inside `invalid_grant`.
    fn is_consent_refusal(&self) -> bool {
        self.error == "consent_required"
            || self.error_codes.contains(&65001)
            || self.error_description.contains("AADSTS65001")
    }
}

/// Microsoft sends lifetimes as numbers; an older endpoint sent strings.
/// Both read.
fn seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Seconds {
        Number(u64),
        Text(String),
    }
    match Seconds::deserialize(d)? {
        Seconds::Number(n) => Ok(n),
        Seconds::Text(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

fn optional_seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    seconds(d).map(Some)
}

/// `application/x-www-form-urlencoded`, built straight into secret text: the
/// device code and the refresh token travel in it.
fn encode_form(pairs: &[(&str, &str)]) -> SecretString {
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        push_encoded(&mut out, key);
        out.push('=');
        push_encoded(&mut out, value);
    }
    SecretString::new(out)
}

fn push_encoded(out: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0xF)] as char);
            }
        }
    }
}

/// A scripted identity far end for this module's tests: hands out canned
/// answers in order and records every request.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::RefCell;

    /// One recorded request.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Request {
        pub url: String,
        pub form: String,
    }

    /// Answers in order; an exhausted script is a transport error.
    #[derive(Default)]
    pub struct ScriptedOAuthHttp {
        answers: RefCell<std::collections::VecDeque<(u16, String)>>,
        pub requests: RefCell<Vec<Request>>,
    }

    impl ScriptedOAuthHttp {
        pub fn new<I: IntoIterator<Item = (u16, String)>>(answers: I) -> Self {
            Self {
                answers: RefCell::new(answers.into_iter().collect()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl OAuthHttp for ScriptedOAuthHttp {
        type Error = &'static str;

        async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, Self::Error> {
            self.requests.borrow_mut().push(Request {
                url: url.to_string(),
                form: form.to_string(),
            });
            let (status, body) = self
                .answers
                .borrow_mut()
                .pop_front()
                .ok_or("scripted identity platform ran out of answers")?;
            Ok(OAuthHttpResponse {
                status,
                body: body.into(),
            })
        }
    }

    /// A `/devicecode` answer.
    pub fn device_code(user_code: &str, expires_in: u64, interval: u64) -> (u16, String) {
        (
            200,
            format!(
                r#"{{"user_code":"{user_code}","device_code":"dc-secret","verification_uri":"https://microsoft.com/devicelogin","expires_in":{expires_in},"interval":{interval},"message":"To sign in, use a web browser"}}"#
            ),
        )
    }

    /// A token answer.
    pub fn token(access: &str, refresh: &str, expires_in: u64) -> (u16, String) {
        (
            200,
            format!(
                r#"{{"token_type":"Bearer","scope":"https://outlook.office.com/IMAP.AccessAsUser.All","expires_in":{expires_in},"access_token":"{access}","refresh_token":"{refresh}"}}"#
            ),
        )
    }

    /// An OAuth error answer.
    pub fn error(code: &str) -> (u16, String) {
        (
            400,
            format!(r#"{{"error":"{code}","error_description":"x"}}"#),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::imap_client::transport::testing::{FakeClock, block_on};

    fn client() -> OAuthClient {
        OAuthClient {
            device_code_url: "https://idp.test/devicecode".into(),
            token_url: "https://idp.test/token".into(),
            client_id: "client-123".into(),
            scope: MICROSOFT.scope.into(),
        }
    }

    #[test]
    fn the_constants_match_the_ruling() {
        assert_eq!(
            MICROSOFT.authority(MICROSOFT.tenant),
            "https://login.microsoftonline.com/common/oauth2/v2.0"
        );
        assert_eq!(
            MICROSOFT.scope,
            "https://outlook.office.com/IMAP.AccessAsUser.All offline_access"
        );
        let c = OAuthClient {
            client_id: String::new(),
            ..MICROSOFT.client()
        };
        assert!(c.device_code_url.ends_with("/oauth2/v2.0/devicecode"));
        assert!(c.token_url.ends_with("/oauth2/v2.0/token"));
        // Unavailable until the owner fills the registration: no request.
        assert_eq!(MICROSOFT.is_available(), !MICROSOFT.client_id.is_empty());
    }

    #[test]
    fn the_authority_seed_redirects_only_to_https() {
        let d = &MICROSOFT.authority(MICROSOFT.tenant);
        assert_eq!(seeded_authority(None, d), *d);
        assert_eq!(seeded_authority(Some("  "), d), *d);
        assert_eq!(
            seeded_authority(Some("https://127.0.0.1:4443/fake/"), d),
            "https://127.0.0.1:4443/fake"
        );
        let plaintext = std::panic::catch_unwind(|| seeded_authority(Some("http://x"), d));
        assert!(plaintext.is_err(), "a plaintext authority is refused");
    }

    fn admin(client_id: &str, tenant: &str) -> MicrosoftRegistration {
        MicrosoftRegistration {
            client_id: client_id.into(),
            tenant: tenant.into(),
        }
    }

    #[test]
    fn the_admins_registration_wins_over_the_constant() {
        let id = "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c";
        let got = MicrosoftRegistration::effective(Some(&admin(id, "contoso.example")));
        assert_eq!(got, admin(id, "contoso.example"));
        assert!(
            got.is_available(),
            "live even while the constant is unfilled"
        );
        let c = got.client();
        assert_eq!(c.client_id, id);
        // The authority is built for the admin's tenant (no seed in this
        // process — tests never set the variable).
        if std::env::var(E2E_AUTHORITY_ENV).is_err() {
            assert_eq!(
                c.device_code_url,
                "https://login.microsoftonline.com/contoso.example/oauth2/v2.0/devicecode"
            );
            assert_eq!(
                c.token_url,
                "https://login.microsoftonline.com/contoso.example/oauth2/v2.0/token"
            );
        }
        assert_eq!(c.scope, MICROSOFT.scope);
    }

    #[test]
    fn an_admin_id_alone_signs_in_through_common() {
        let id = "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c";
        assert_eq!(
            MicrosoftRegistration::effective(Some(&admin(id, ""))),
            admin(id, "common")
        );
        let status = fauna_protocol::microsoft_registration::MicrosoftRegistrationStatus {
            client_id: id.into(),
            tenant: String::new(),
            extra: Default::default(),
        };
        assert_eq!(
            MicrosoftRegistration::from_status(&status),
            admin(id, "common")
        );
    }

    #[test]
    fn no_admin_registration_is_the_project_constant() {
        let project = MicrosoftRegistration::project();
        assert_eq!(project.client_id, MICROSOFT.client_id);
        assert_eq!(project.tenant, "common");
        assert_eq!(MicrosoftRegistration::effective(None), project);
        // A cleared (empty) admin pair is the project's too.
        assert_eq!(
            MicrosoftRegistration::effective(Some(&admin("", ""))),
            project
        );
        assert_eq!(project.is_available(), MICROSOFT.is_available());
        assert_eq!(project.client(), MICROSOFT.client());
    }

    #[test]
    fn start_refuses_without_a_request_while_the_client_id_is_empty() {
        let http = ScriptedOAuthHttp::new([]);
        let c = OAuthClient {
            client_id: String::new(),
            ..client()
        };
        assert_eq!(
            block_on(DeviceFlow::start(&http, &c, 0)).unwrap_err(),
            OAuthError::Unavailable
        );
        assert!(http.requests.borrow().is_empty());
    }

    #[test]
    fn start_posts_client_id_and_scope_and_tracks_the_lifetime() {
        let http = ScriptedOAuthHttp::new([device_code("ABCD-EFGH", 900, 5)]);
        let flow = block_on(DeviceFlow::start(&http, &client(), 1_000)).unwrap();
        let req = &http.requests.borrow()[0];
        assert_eq!(req.url, "https://idp.test/devicecode");
        assert_eq!(
            req.form,
            "client_id=client-123&scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All+offline_access"
        );
        assert_eq!(flow.authorization().user_code, "ABCD-EFGH");
        assert_eq!(
            flow.authorization().verification_uri,
            "https://microsoft.com/devicelogin"
        );
        assert_eq!(flow.authorization().expires_at_ms, 901_000);
        assert_eq!(flow.interval_ms(), 5_000);
        assert_eq!(flow.next_poll_delay_ms(1_000), 5_000);
        assert_eq!(flow.next_poll_delay_ms(4_000), 2_000);
    }

    #[test]
    fn a_refused_code_request_names_the_oauth_error() {
        let http = ScriptedOAuthHttp::new([error("invalid_client")]);
        assert_eq!(
            block_on(DeviceFlow::start(&http, &client(), 0)).unwrap_err(),
            OAuthError::Refused("invalid_client".into())
        );
    }

    #[test]
    fn pending_keeps_the_interval_and_slow_down_lengthens_it_by_five_seconds() {
        let http = ScriptedOAuthHttp::new([
            device_code("C", 900, 5),
            error("authorization_pending"),
            error("slow_down"),
            error("slow_down"),
        ]);
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert_eq!(
            block_on(flow.poll(&http, 5_000)).unwrap(),
            PollStep::Pending
        );
        assert_eq!(flow.interval_ms(), 5_000);
        assert_eq!(flow.next_poll_delay_ms(5_000), 5_000);
        assert_eq!(
            block_on(flow.poll(&http, 10_000)).unwrap(),
            PollStep::SlowDown
        );
        assert_eq!(flow.interval_ms(), 10_000);
        assert_eq!(flow.next_poll_delay_ms(10_000), 10_000);
        assert_eq!(
            block_on(flow.poll(&http, 20_000)).unwrap(),
            PollStep::SlowDown
        );
        assert_eq!(flow.interval_ms(), 15_000);
        let req = &http.requests.borrow()[1];
        assert_eq!(req.url, "https://idp.test/token");
        assert_eq!(
            req.form,
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id=client-123&device_code=dc-secret"
        );
    }

    #[test]
    fn the_device_flow_errors_map_to_typed_outcomes() {
        for (code, want) in [
            ("authorization_declined", FlowOutcome::Declined),
            ("expired_token", FlowOutcome::Expired),
            ("consent_required", FlowOutcome::AdminConsentRequired),
            (
                "bad_verification_code",
                FlowOutcome::Failed("bad_verification_code".into()),
            ),
        ] {
            let http = ScriptedOAuthHttp::new([device_code("C", 900, 5), error(code)]);
            let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
            assert_eq!(
                block_on(flow.poll(&http, 5_000)).unwrap(),
                PollStep::Done(want),
                "{code}"
            );
        }
    }

    #[test]
    fn the_aadsts65001_family_inside_invalid_grant_is_an_admin_consent_refusal() {
        let consent = (
            400,
            r#"{"error":"invalid_grant","error_description":"AADSTS65001: The user or administrator has not consented","error_codes":[65001]}"#.to_string(),
        );
        let http = ScriptedOAuthHttp::new([device_code("C", 900, 5), consent]);
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert_eq!(
            block_on(flow.poll(&http, 5_000)).unwrap(),
            PollStep::Done(FlowOutcome::AdminConsentRequired)
        );
    }

    #[test]
    fn a_grant_carries_both_tokens_and_the_access_tokens_expiry() {
        let http = ScriptedOAuthHttp::new([device_code("C", 900, 5), token("at-1", "rt-1", 3_600)]);
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        let PollStep::Done(FlowOutcome::Granted(grant)) =
            block_on(flow.poll(&http, 7_000)).unwrap()
        else {
            panic!("granted")
        };
        assert_eq!(grant.access_token.as_str(), "at-1");
        assert_eq!(grant.refresh_token.unwrap().as_str(), "rt-1");
        assert_eq!(grant.expires_at_ms, 7_000 + 3_600_000);
    }

    #[test]
    fn an_expired_code_ends_without_a_request() {
        let http = ScriptedOAuthHttp::new([device_code("C", 10, 5)]);
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert_eq!(
            block_on(flow.poll(&http, 10_000)).unwrap(),
            PollStep::Done(FlowOutcome::Expired)
        );
        assert_eq!(http.requests.borrow().len(), 1);
    }

    #[test]
    fn run_sleeps_each_interval_and_returns_the_grant() {
        let http = ScriptedOAuthHttp::new([
            device_code("C", 900, 5),
            error("authorization_pending"),
            error("slow_down"),
            token("at", "rt", 3_600),
        ]);
        let clock = FakeClock::new();
        let mut flow = block_on(DeviceFlow::start(&http, &client(), clock.now_ms())).unwrap();
        let outcome = block_on(flow.run(&http, &clock, || false));
        assert!(matches!(outcome, FlowOutcome::Granted(_)));
        // 5 s, 5 s, then 10 s after the slow_down.
        assert_eq!(*clock.slept.borrow(), vec![5_000, 5_000, 10_000]);
    }

    #[test]
    fn run_stops_on_cancel_before_the_next_request() {
        let http =
            ScriptedOAuthHttp::new([device_code("C", 900, 5), error("authorization_pending")]);
        let clock = FakeClock::new();
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        let mut asked = 0;
        let outcome = block_on(flow.run(&http, &clock, || {
            asked += 1;
            asked > 1
        }));
        assert_eq!(outcome, FlowOutcome::Cancelled);
        assert_eq!(http.requests.borrow().len(), 2, "one poll, then cancelled");
    }

    #[test]
    fn run_ends_expired_when_the_lifetime_runs_out_while_pending() {
        let http = ScriptedOAuthHttp::new([
            device_code("C", 12, 5),
            error("authorization_pending"),
            error("authorization_pending"),
        ]);
        let clock = FakeClock::new();
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert_eq!(
            block_on(flow.run(&http, &clock, || false)),
            FlowOutcome::Expired
        );
    }

    #[test]
    fn run_ends_failed_when_the_platform_cannot_be_reached() {
        let http = ScriptedOAuthHttp::new([device_code("C", 900, 5)]);
        let clock = FakeClock::new();
        let mut flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert!(matches!(
            block_on(flow.run(&http, &clock, || false)),
            FlowOutcome::Failed(_)
        ));
    }

    #[test]
    fn refresh_trades_the_refresh_token_once() {
        let http = ScriptedOAuthHttp::new([token("at-2", "rt-2", 3_600)]);
        let grant = block_on(refresh(&http, &client(), &"rt-1".into(), 50)).unwrap();
        assert_eq!(grant.access_token.as_str(), "at-2");
        assert_eq!(grant.refresh_token.unwrap().as_str(), "rt-2");
        assert_eq!(grant.expires_at_ms, 50 + 3_600_000);
        let reqs = http.requests.borrow();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].form,
            "grant_type=refresh_token&client_id=client-123&refresh_token=rt-1&scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All+offline_access"
        );
    }

    #[test]
    fn a_refused_refresh_is_refused() {
        let http = ScriptedOAuthHttp::new([error("invalid_grant")]);
        assert_eq!(
            block_on(refresh(&http, &client(), &"rt".into(), 0)).unwrap_err(),
            OAuthError::Refused("invalid_grant".into())
        );
    }

    #[test]
    fn lifetimes_sent_as_strings_still_read() {
        let http = ScriptedOAuthHttp::new([(
            200,
            r#"{"user_code":"C","device_code":"d","verification_uri":"https://microsoft.com/devicelogin","expires_in":"900","interval":"5"}"#.to_string(),
        )]);
        let flow = block_on(DeviceFlow::start(&http, &client(), 0)).unwrap();
        assert_eq!(flow.authorization().expires_at_ms, 900_000);
    }

    #[test]
    fn form_encoding_escapes_reserved_bytes() {
        assert_eq!(
            encode_form(&[("a b", "x&y=z/é")]).as_str(),
            "a+b=x%26y%3Dz%2F%C3%A9"
        );
    }
}
