use thiserror::Error;

#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum OnboardingError {
    /// Network / HTTP error — generic transport failure.
    ///
    /// Field is named `detail` (not `message`) so the UniFFI Kotlin
    /// generator doesn't collide with `Throwable.message` on the
    /// generated `OnboardingException.Network` class.
    #[error("network error: {detail}")]
    Network { detail: String },

    /// Domain was unregistered or otherwise rejected by the probe.
    #[error("domain {domain} could not be resolved: {reason}")]
    DomainProbeFailed { domain: String, reason: String },

    /// Tried to register a domain that's already taken.
    #[error("domain {domain} is not available for registration")]
    DomainNotAvailable { domain: String },

    /// Provider rejected the credentials.
    #[error("provider {provider} rejected the credentials")]
    ProviderUnauthorized { provider: String },

    /// Provider returned a malformed response.
    #[error("provider {provider} returned an unexpected response: {detail}")]
    ProviderProtocolError { provider: String, detail: String },

    /// Provisioning step failed mid-flight (server creation, DNS write, …).
    #[error("provisioning failed at {step}: {detail}")]
    ProvisioningFailed { step: String, detail: String },

    /// Nest claim failed (wrong code, expired, etc.).
    #[error("nest claim failed: {reason}")]
    ClaimFailed { reason: String },

    /// Silent challenge or login failed.
    #[error("login failed: {reason}")]
    LoginFailed { reason: String },

    /// Caller invoked a transition that the current state doesn't allow
    /// (e.g. continue_from_dns when no provider is selected).
    #[error("invalid transition from {from}: {reason}")]
    InvalidTransition { from: String, reason: String },

    /// Generic fallback for sources without a specific category.
    ///
    /// Field is `detail` for the same Kotlin-collision reason as `Network`.
    #[error("{detail}")]
    Other { detail: String },
}

impl From<fauna_provisioning::error::ProvisionError> for OnboardingError {
    fn from(e: fauna_provisioning::error::ProvisionError) -> Self {
        // Match on the typed variants so we don't lose info to to_string().
        // Real call sites (verify_dns, verify_vps) wrap with the actual
        // provider id before bubbling — the From impl is the fallback for
        // anywhere a `?` operator hits a ProvisionError unwrapped.
        use fauna_provisioning::error::ProvisionError as P;
        match e {
            P::Http(req_err) => OnboardingError::Network {
                detail: req_err.to_string(),
            },
            P::Provider {
                status: 401 | 403,
                body,
            } => OnboardingError::ProviderUnauthorized {
                provider: format!("(unknown; body: {body})"),
            },
            P::Provider { status, body } => OnboardingError::ProviderProtocolError {
                provider: "(unknown)".into(),
                detail: format!("HTTP {status}: {body}"),
            },
            P::Parse(detail) => OnboardingError::ProviderProtocolError {
                provider: "(unknown)".into(),
                detail,
            },
            P::Other(message) => OnboardingError::Other { detail: message },
            P::Cancelled => OnboardingError::Other {
                detail: "Cancelled".into(),
            },
            // run_step's wrapper around step failures. Carries the failed
            // step's identity and the underlying cause; flatten by
            // forwarding the cause so existing call sites that bubble via
            // `?` keep getting the same shape.
            P::StepFailed {
                step,
                attempts,
                cause,
            } => match (*cause).into() {
                OnboardingError::Network { detail } => OnboardingError::Network {
                    detail: format!("step {step:?} (attempt {attempts}): {detail}"),
                },
                OnboardingError::ProviderProtocolError { provider, detail } => {
                    OnboardingError::ProviderProtocolError {
                        provider,
                        detail: format!("step {step:?} (attempt {attempts}): {detail}"),
                    }
                }
                OnboardingError::Other { detail } => OnboardingError::Other {
                    detail: format!("step {step:?} (attempt {attempts}): {detail}"),
                },
                other => other,
            },
        }
    }
}

impl From<reqwest::Error> for OnboardingError {
    fn from(e: reqwest::Error) -> Self {
        OnboardingError::Network {
            detail: e.to_string(),
        }
    }
}
