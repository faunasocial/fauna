//! Permanent-failure classifier.
//!
//! Implements docs/goal/behavior/smtp-server.md § Permanent-failure bounce
//! generation rule (2): a 5xx with an enhanced status in the 5.x.x range
//! and not in the admin-allowlist of "treat-as-transient" codes
//! promotes to immediate `PermFail`. Other failures fall back to
//! `TempFail`, which the bridge feeds into `RetrySchedule::next` until the
//! retry budget exhausts (the other half of the permfail condition).
//!
//! The classifier is pure-Rust; the bridge crate's drain loop calls it on
//! every send failure to decide whether to schedule another attempt or
//! jump straight to the bounce path.

/// Last-wire-response from a recipient MX, parsed into the fields the
/// classifier needs. Lifetime-borrows the bridge's local strings so the
/// caller doesn't have to allocate when invoking `classify`.
#[derive(Debug, Clone, Copy)]
pub struct WireResponse<'a> {
    pub code: u16,
    pub enhanced_status: Option<&'a str>,
    pub text: &'a str,
}

/// Outcome of classifying a wire response.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Delivered,
    TempFail {
        last_error: String,
        enhanced: Option<String>,
    },
    PermFail {
        last_error: String,
        enhanced: String,
    },
}

pub trait BouncePolicy: Send + Sync + 'static {
    fn classify(&self, resp: &WireResponse<'_>) -> Verdict;
}

/// Default classifier. Admins add enhanced-status codes to
/// `treat_5xx_as_transient` via mail-policy-config to demote specific 5xx
/// responses (typical case: a downstream MX is misconfigured to return
/// 5.7.1 for greylist holds — admin override pending the recipient's
/// fix).
#[derive(Debug, Clone, Default)]
pub struct DefaultBouncePolicy {
    pub treat_5xx_as_transient: Vec<String>,
}

impl BouncePolicy for DefaultBouncePolicy {
    fn classify(&self, resp: &WireResponse<'_>) -> Verdict {
        let last_error = format!("{} {}", resp.code, resp.text);
        if (200..300).contains(&resp.code) {
            return Verdict::Delivered;
        }
        if (400..500).contains(&resp.code) {
            return Verdict::TempFail {
                last_error,
                enhanced: resp.enhanced_status.map(str::to_string),
            };
        }
        // 5xx (and anything outside 2xx/4xx — be defensive).
        let enhanced = resp.enhanced_status.unwrap_or("5.0.0").to_string();
        if self.treat_5xx_as_transient.iter().any(|c| c == &enhanced) {
            return Verdict::TempFail {
                last_error,
                enhanced: Some(enhanced),
            };
        }
        Verdict::PermFail {
            last_error,
            enhanced,
        }
    }
}

impl DefaultBouncePolicy {
    /// Classify from enhanced status alone — used when the wire code
    /// isn't available (connect / TLS / generic mail-send::Error paths).
    /// Falls back to `TempFail` when no enhanced status is present so a
    /// transient network glitch doesn't immediately promote to permfail.
    pub fn classify_enhanced(&self, enhanced: Option<&str>, last_error: &str) -> Verdict {
        match enhanced {
            None => Verdict::TempFail {
                last_error: last_error.to_string(),
                enhanced: None,
            },
            Some(s) if s.starts_with("2.") => Verdict::Delivered,
            Some(s) if s.starts_with("4.") => Verdict::TempFail {
                last_error: last_error.to_string(),
                enhanced: Some(s.to_string()),
            },
            Some(s) if s.starts_with("5.") => {
                if self.treat_5xx_as_transient.iter().any(|c| c == s) {
                    Verdict::TempFail {
                        last_error: last_error.to_string(),
                        enhanced: Some(s.to_string()),
                    }
                } else {
                    Verdict::PermFail {
                        last_error: last_error.to_string(),
                        enhanced: s.to_string(),
                    }
                }
            }
            Some(s) => Verdict::TempFail {
                last_error: last_error.to_string(),
                enhanced: Some(s.to_string()),
            },
        }
    }
}
