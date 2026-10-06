/// A transport-level failure whose request URL has been stripped of everything
/// that can carry a credential, at the moment the error is built.
///
/// **Why the newtype exists.** `reqwest::Error` carries the request URL and
/// renders it from *both* `Display` and `Debug`. Several providers put
/// user-supplied secrets in that URL — Namecheap's whole API is
/// query-parameter driven, so the user's `ApiKey` (a declared
/// `FieldType::Secret`) rides every request — and `run_step` interpolates the
/// error into a `warn!` on the ordinary transient-retry arm. `fauna-log`
/// installs the user-visible ring *and* a daily-rolling on-disk file
/// regardless of the stderr choice, so an unredacted error writes the
/// credential to disk and onto a page users export into support threads. The
/// rule it breaks is stated, ratified, in
/// `docs/goal/architecture/apps/observability.md` § Target state →
/// *2. Persistence & privacy*: never log secret material, only error metadata.
///
/// **Why a newtype rather than a careful call site.** A log site is easy to add
/// without knowing the hazard — that is exactly how the leak arrived, from a
/// commit that was otherwise a correct fix for a real diagnosability hole. So
/// the unsafe value is made unrepresentable instead: [`ProvisionError::Http`]
/// can hold only this type, whose sole constructor redacts, and the blanket
/// `#[from] reqwest::Error` is gone from the variant. `?` still works at every
/// call site through this crate's `From<reqwest::Error> for ProvisionError`,
/// which routes through the same constructor — the safe path is the only path,
/// and it is also the ergonomic one.
///
/// Scheme, host, port and path are deliberately **kept**: they are the error
/// metadata the rule permits and a support reader needs. The query string goes
/// wholesale rather than by an allowlist of parameter names — a redactor that
/// enumerates the keys it knows about is one provider away from being wrong.
pub struct RedactedHttpError(reqwest::Error);

impl RedactedHttpError {
    fn new(mut e: reqwest::Error) -> Self {
        if let Some(url) = e.url_mut() {
            url.set_query(None);
            // Userinfo is the other standard hiding place for a credential in
            // a URL (`https://user:pass@host/`). Both setters refuse on
            // cannot-be-a-base URLs, where there is no userinfo to strip.
            let _ = url.set_password(None);
            let _ = url.set_username("");
        }
        Self(e)
    }

    /// Transport-failure classification for the retry policy
    /// (`crate::progress::is_transient`) — forwarded rather than exposing the
    /// inner error, so no caller can re-obtain the unredacted rendering.
    pub fn is_timeout(&self) -> bool {
        self.0.is_timeout()
    }

    /// See [`Self::is_timeout`]. Not available on wasm32: upstream gates it to
    /// non-wasm targets, and `is_request` covers roughly the same set there.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_connect(&self) -> bool {
        self.0.is_connect()
    }

    /// See [`Self::is_timeout`].
    pub fn is_request(&self) -> bool {
        self.0.is_request()
    }
}

impl std::fmt::Display for RedactedHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Hand-written so the wrapper's own name appears in `{:?}` output: a reader
/// who notices a URL with no query string should be able to tell that it was
/// removed on purpose rather than never sent.
impl std::fmt::Debug for RedactedHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RedactedHttpError({:?})", self.0)
    }
}

impl std::error::Error for RedactedHttpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// Errors from provider API calls.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("HTTP request failed: {0}")]
    Http(#[source] RedactedHttpError),

    #[error("Provider returned error: {status} {body}")]
    Provider { status: u16, body: String },

    #[error("Unexpected response format: {0}")]
    Parse(String),

    #[error("{0}")]
    Other(String),

    /// Set on the cancel flag mid-run; the run loop returns this from its
    /// next iteration boundary or retry sleep.
    #[error("Cancelled")]
    Cancelled,

    /// Wrapped per-step error returned from `run_step` when the step's
    /// retry budget is exhausted or the cause is terminal. Carries the
    /// step's identity, the attempt count when failure occurred, and the
    /// underlying cause.
    #[error("Step {step:?} failed after {attempts} attempt(s): {cause}")]
    StepFailed {
        step: crate::progress::ProvisionStep,
        attempts: u32,
        #[source]
        cause: Box<ProvisionError>,
    },
}

/// The one conversion `?` uses, at every `reqwest` call site in this crate.
/// It redacts; there is no variant of it that does not.
impl From<reqwest::Error> for ProvisionError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(RedactedHttpError::new(e))
    }
}

impl ProvisionError {
    pub fn provider(status: u16, body: impl Into<String>) -> Self {
        Self::Provider {
            status,
            body: body.into(),
        }
    }

    pub fn parse(e: impl std::fmt::Display) -> Self {
        Self::Parse(e.to_string())
    }
}

/// Shared non-2xx handling for provider API calls: passes a successful
/// response through unchanged, otherwise reads the body and returns
/// `ProvisionError::Provider`. Every DNS/registrar/VPS adapter's non-delete
/// call sites funnel through this so the status-check + body-read idiom has
/// one definition (`vps::finish_delete` is the sibling for the 404-tolerant
/// delete case, which this deliberately does not special-case).
pub(crate) async fn ensure_success(
    resp: reqwest::Response,
) -> Result<reqwest::Response, ProvisionError> {
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProvisionError::provider(status, body));
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn ensure_success_passes_through_a_2xx_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();

        let resp = ensure_success(resp).await.unwrap();

        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn ensure_success_reads_the_body_into_a_provider_error_on_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;
        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();

        let err = ensure_success(resp).await.unwrap_err();

        match err {
            ProvisionError::Provider { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body, "rate limited");
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    /// A recognisable stand-in for the user's provider credential. Long and
    /// self-describing so a failure message shows plainly what leaked.
    const FAKE_KEY: &str = "nckey-SUPERSECRET-do-not-log-me";

    /// A port nothing listens on: the connect-refused shape a live retry hits,
    /// and the one transport failure reachable without a network.
    const CLOSED: &str = "http://127.0.0.1:1/xml.response";

    /// **The leak this variant exists to prevent.** Namecheap's API is
    /// query-parameter driven, so the user's `ApiKey` — a declared
    /// `FieldType::Secret` — rides the request URL. `reqwest::Error` carries
    /// that URL, and `run_step` interpolates the error into a `warn!` on the
    /// ORDINARY transient-retry arm; `fauna-log` installs both the
    /// user-visible ring and a daily-rolling on-disk file regardless of the
    /// stderr choice, so an unredacted error writes the credential to disk and
    /// to a page the user exports into support threads.
    ///
    /// `observability.md` § Target state → *2. Persistence & privacy* states
    /// the rule this pins: "NEVER log … secret material (keys, tokens, claim
    /// codes) … Log levels, targets, operation names, and error metadata only."
    ///
    /// **Both renderings are asserted on purpose:** `reqwest::Error`'s `Debug`
    /// carries the URL independently of its `Display`, so a fix that only
    /// touched the `#[error(...)]` format string would leave `Debug` — which
    /// is what `{:?}` and every `unwrap()` panic print — still leaking.
    #[tokio::test]
    async fn a_namecheap_transport_failure_carries_no_api_key() {
        use crate::dns::DnsProvider;

        let nc = crate::dns::namecheap::Namecheap::with_base_url(
            "someuser".into(),
            FAKE_KEY.into(),
            CLOSED.into(),
        );
        let err = nc
            .verify(&reqwest::Client::new())
            .await
            .expect_err("a closed port cannot answer");

        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(
            !display.contains(FAKE_KEY),
            "the API key must not reach Display: {display}"
        );
        assert!(
            !debug.contains(FAKE_KEY),
            "the API key must not reach Debug: {debug}"
        );
        // The redaction is not achieved by throwing the diagnostics away: the
        // host and path a support reader needs are still there.
        assert!(
            display.contains("127.0.0.1") || debug.contains("127.0.0.1"),
            "the failing host must survive redaction: {display} / {debug}"
        );
    }

    /// The same guarantee stated over the conversion itself, so it holds for
    /// every provider and every future call site rather than only the one
    /// adapter the leak was found in. Userinfo is stripped beside the query:
    /// `https://user:pass@host/` is the other standard place a credential
    /// hides in a URL.
    #[tokio::test]
    async fn the_reqwest_conversion_strips_query_and_userinfo() {
        let raw = reqwest::Client::new()
            .get("http://someuser:hunter2@127.0.0.1:1/path")
            .query(&[("ApiKey", FAKE_KEY), ("Command", "domains.getList")])
            .send()
            .await
            .expect_err("a closed port cannot answer");

        let err: ProvisionError = raw.into();

        let rendered = format!("{err} {err:?}");
        assert!(
            !rendered.contains(FAKE_KEY),
            "query-string credential leaked: {rendered}"
        );
        assert!(
            !rendered.contains("hunter2"),
            "userinfo credential leaked: {rendered}"
        );
        assert!(
            !rendered.contains("Command"),
            "the whole query string goes, not just the key it happened to \
             carry — a redactor that allowlists parameter names is one \
             provider away from being wrong: {rendered}"
        );
        assert!(
            rendered.contains("/path"),
            "scheme, host and path survive for diagnosis: {rendered}"
        );
    }
}
