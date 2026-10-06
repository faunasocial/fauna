//! Shared Namecheap transport, used by both Namecheap capability adapters.
//!
//! Namecheap exposes its whole API through **one** endpoint selected by a
//! `Command` query parameter, so `dns/namecheap.rs` and `registrar/namecheap.rs`
//! are two views of a single transport rather than two independent clients.
//! [`NamecheapApi`] owns that transport: the five global parameters, the
//! HTTP-200-with-`Status="ERROR"` envelope, and the IP-allowlist self-heal.
//!
//! Keeping it in one place is load-bearing, not tidiness. Six per-call-site
//! copies of the global parameters are exactly what let `ClientIp` ship empty
//! on every live request (fixed 2026-07-22); a second capability adapter
//! re-deriving them would reopen that hole.

use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};

/// Direct Namecheap API base. Native builds hit this directly; wasm32 builds
/// route through the CORS proxy (`cors_policy: proxy` in `i18n/providers.yaml`)
/// — see [`crate::proxy`].
///
/// Namecheap's whole API is one endpoint driven by query parameters
/// (`?ApiUser=…&ApiKey=…&Command=…`), which is why the proxy has to forward
/// query strings — see `services/fauna-cors-proxy`.
pub(crate) const DIRECT_API: &str = "https://api.namecheap.com/xml.response";
pub(crate) const PROXY_PREFIX: &str = "namecheap/xml.response";

/// RFC 5737 TEST-NET-1 — the seed value for Namecheap's mandatory `ClientIp`.
///
/// Namecheap rejects a *missing* `ClientIp` outright (error 1010105, verified
/// against the live endpoint 2026-07-22), so every call must carry a
/// syntactically valid address even before we know our public one. This is only
/// ever a seed: Namecheap gates on the request's **source** address and echoes
/// that address back when it rejects (error 1011150), so [`NamecheapApi::call`]
/// learns the real value from the rejection itself.
const PLACEHOLDER_CLIENT_IP: &str = "192.0.2.1";

/// Namecheap's "the calling address is not allowlisted" error.
const ERR_INVALID_REQUEST_IP: &str = "1011150";

/// A Namecheap API-level error.
///
/// ⚠ Namecheap answers API-level failures with **HTTP 200** and an
/// `<ApiResponse Status="ERROR">` body — verified empirically 2026-07-22. An
/// adapter that checks only the HTTP status reads every API failure as success:
/// `verify` returns an empty domain list (indistinguishable from a real empty
/// account), `setHosts` reports a write that never happened, and a refused
/// registration reads as a bought domain.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ApiError {
    pub(crate) number: String,
    pub(crate) message: String,
}

/// Extract Namecheap's API-level error from a response body, if it is one.
///
/// Conservative by construction: only an explicit `Status="ERROR"` counts, so
/// an unrecognised body still reads as success exactly as it did before. A
/// `Status="ERROR"` whose `<Error>` element we cannot parse still yields an
/// error — never a silent success.
pub(crate) fn parse_api_error(xml: &str) -> Option<ApiError> {
    if !xml.contains(r#"Status="ERROR""#) {
        return None;
    }
    let detail = (|| {
        let after = xml.split_once(r#"<Error Number=""#)?.1;
        let (number, rest) = after.split_once('"')?;
        let message = rest.split_once('>')?.1.split_once("</Error>")?.0;
        Some((number.to_string(), message.trim().to_string()))
    })();
    Some(match detail {
        Some((number, message)) => ApiError { number, message },
        None => ApiError {
            number: String::new(),
            message: "Namecheap reported an error with no detail".to_string(),
        },
    })
}

/// The source address Namecheap says it observed, from
/// `Invalid request IP: 203.0.113.7`.
///
/// This is the project's only reflector for this call path. Determining a
/// public address normally needs an external STUN/echo server, and picking one
/// is a self-hosted-invariant decision the user deferred (2026-07-07 —
/// `fauna-client-dns::host_address`); Namecheap volunteering the address in its
/// rejection sidesteps that decision entirely, on web and native alike.
pub(crate) fn echoed_source_ip(err: &ApiError) -> Option<String> {
    if err.number != ERR_INVALID_REQUEST_IP {
        return None;
    }
    // Split on the FIRST colon — an IPv6 address contains colons of its own.
    let candidate = err.message.split_once(':')?.1.trim();
    candidate
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

/// Turn a Namecheap API error into a user-actionable [`ProvisionError`],
/// naming the exact address to allowlist when that is the cause.
pub(crate) fn to_provision_error(err: &ApiError) -> ProvisionError {
    match echoed_source_ip(err) {
        Some(ip) => ProvisionError::Other(format!(
            "Namecheap rejected the request because the calling IP address {ip} is not \
             allowlisted for API access. Add {ip} at namecheap.com → Profile → Tools → \
             API Access, then try again. (Namecheap error {}: {})",
            err.number, err.message
        )),
        None => ProvisionError::Other(format!(
            "Namecheap API error {}: {}",
            err.number, err.message
        )),
    }
}

// ---------------------------------------------------------------------------
// XML attribute scraping
// ---------------------------------------------------------------------------
//
// Deliberately hand-rolled rather than a parser dependency: Namecheap's answers
// are shallow and attribute-heavy, `dns/namecheap.rs` set the hand-parsing
// precedent, and the obvious crate (`quick-xml`) carries two live CVSS-7.5
// advisories with no compatible upgrade.

/// The opening-tag fragment of every `<Tag …>` element in `xml`, in document
/// order — the bytes between `<Tag` and the closing `>`.
///
/// Quote-aware (a `>` inside an attribute value doesn't end the tag) and
/// exact-name-matching: `<Domain` must be followed by whitespace, `/` or `>`,
/// so it never matches `<DomainCheckResult`. That distinction is live — both
/// tags appear in Namecheap responses this crate parses.
///
/// Scanning the whole document rather than line-by-line also removes a latent
/// fragility: the previous line-based scan found at most one element per line,
/// so a non-pretty-printed response would have silently dropped records.
pub(crate) fn elements<'x>(xml: &'x str, tag: &str) -> Vec<&'x str> {
    let needle = format!("<{tag}");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&needle) {
        let after = &rest[start + needle.len()..];
        // Exact tag name: the next byte must end the name.
        let ends_name = after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '/' || c == '>');
        if !ends_name {
            rest = after;
            continue;
        }
        let mut in_quotes = false;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '"' => in_quotes = !in_quotes,
                '>' if !in_quotes => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        match end {
            Some(i) => {
                out.push(&after[..i]);
                rest = &after[i..];
            }
            // Unterminated tag — a truncated body. Stop rather than loop.
            None => break,
        }
    }
    out
}

/// The value of `name="…"` within an opening-tag `fragment` from [`elements`].
pub(crate) fn attr(fragment: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut rest = fragment;
    while let Some(start) = rest.find(&needle) {
        // Guard against a suffix match: `Price="1"` must not satisfy a lookup
        // for `YourPrice`. The byte before the name must not be name-ish.
        let preceding = rest[..start].chars().next_back();
        if preceding.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            rest = &rest[start + needle.len()..];
            continue;
        }
        let after = &rest[start + needle.len()..];
        return after.split_once('"').map(|(v, _)| v.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// The transport
// ---------------------------------------------------------------------------

/// One authenticated Namecheap endpoint, shared by the DNS and registrar
/// adapters.
pub struct NamecheapApi {
    api_user: String,
    api_key: String,
    /// Namecheap's required `ClientIp` global parameter — the address the
    /// caller *claims*, seeded to [`PLACEHOLDER_CLIENT_IP`].
    ///
    /// Namecheap enforces its allowlist against the request's **source**
    /// address, not this value, and the source differs by build env: on web it
    /// is the CORS proxy's, on native the user's own. Neither is knowable
    /// up-front (determining a public address needs an external reflector — a
    /// deferred user decision, see [`echoed_source_ip`]), so this field is not
    /// where correctness lives: [`NamecheapApi::call`] learns the real address
    /// from Namecheap's own rejection and retries with it.
    client_ip: String,
    api_base: String,
}

impl NamecheapApi {
    pub fn new(api_user: String, api_key: String) -> Self {
        Self {
            api_user,
            api_key,
            client_ip: PLACEHOLDER_CLIENT_IP.to_string(),
            api_base: default_api_base(DIRECT_API, PROXY_PREFIX),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(api_user: String, api_key: String, env: BuildEnv) -> Self {
        Self {
            api_base: default_api_base_for_env(DIRECT_API, PROXY_PREFIX, env),
            ..Self::new(api_user, api_key)
        }
    }

    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(api_user: String, api_key: String, api_base: String) -> Self {
        Self {
            api_user,
            api_key,
            client_ip: PLACEHOLDER_CLIENT_IP.to_string(),
            api_base,
        }
    }

    /// The API base this transport will call.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Override the seeded `ClientIp`.
    ///
    /// Only an optimisation: a caller that already knows the public address
    /// skips the self-heal round-trip. Correctness never depends on it — an
    /// empty value is ignored, because Namecheap rejects a missing `ClientIp`
    /// outright and that would break every call.
    pub fn set_client_ip(&mut self, client_ip: String) {
        if !client_ip.is_empty() {
            self.client_ip = client_ip;
        }
    }

    /// The `ClientIp` this transport currently claims.
    pub fn client_ip(&self) -> &str {
        &self.client_ip
    }

    /// The five global parameters Namecheap requires on every command. This is
    /// the **only** place `ClientIp` is applied — the previous per-call-site
    /// copies are what let it ship empty on every live request.
    fn global_params(&self, client_ip: &str, command: &str) -> Vec<(String, String)> {
        vec![
            ("ApiUser".to_string(), self.api_user.clone()),
            ("ApiKey".to_string(), self.api_key.clone()),
            ("UserName".to_string(), self.api_user.clone()),
            ("ClientIp".to_string(), client_ip.to_string()),
            ("Command".to_string(), command.to_string()),
        ]
    }

    /// One HTTP round-trip, returning the raw body. HTTP-level failures become
    /// errors here; API-level ones are [`NamecheapApi::call`]'s job.
    async fn send_once(
        &self,
        client: &reqwest::Client,
        client_ip: &str,
        command: &str,
        extra: &[(String, String)],
    ) -> Result<String, ProvisionError> {
        let mut params = self.global_params(client_ip, command);
        params.extend_from_slice(extra);

        let resp = client.get(&self.api_base).query(&params).send().await?;
        let resp = ensure_success(resp).await?;
        Ok(resp.text().await?)
    }

    /// Issue a Namecheap command, converting its HTTP-200-with-`Status="ERROR"`
    /// answers into real errors.
    ///
    /// Includes one self-heal: when Namecheap rejects the calling address it
    /// echoes the address it actually saw, so the call is retried once carrying
    /// that value. This rescues the case where the user *has* allowlisted their
    /// address but the request carried the placeholder — without it the wizard
    /// would tell them to allowlist an address they had already allowlisted,
    /// with no way out. The retry costs a round-trip only on a rejection.
    pub(crate) async fn call(
        &self,
        client: &reqwest::Client,
        command: &str,
        extra: &[(String, String)],
    ) -> Result<String, ProvisionError> {
        let xml = self
            .send_once(client, &self.client_ip, command, extra)
            .await?;
        let Some(err) = parse_api_error(&xml) else {
            return Ok(xml);
        };

        match echoed_source_ip(&err) {
            Some(observed) if observed != self.client_ip => {
                let retried = self.send_once(client, &observed, command, extra).await?;
                match parse_api_error(&retried) {
                    None => Ok(retried),
                    Some(err) => Err(to_provision_error(&err)),
                }
            }
            _ => Err(to_provision_error(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A successful response must not be mistaken for an error.
    #[test]
    fn ok_response_is_not_an_error() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse/></ApiResponse>"#;
        assert_eq!(parse_api_error(xml), None);
    }

    /// The shape verified against the live endpoint 2026-07-22: an API-level
    /// failure carried by HTTP 200.
    #[test]
    fn parses_the_live_missing_client_ip_error() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="ERROR" xmlns="http://api.namecheap.com/xml.response">
  <Errors>
    <Error Number="1010105">Parameter ClientIP is missing</Error>
  </Errors>
</ApiResponse>"#;
        let err = parse_api_error(xml).expect("must detect the error");
        assert_eq!(err.number, "1010105");
        assert_eq!(err.message, "Parameter ClientIP is missing");
        // Not an allowlist failure, so no address to echo.
        assert_eq!(echoed_source_ip(&err), None);
    }

    /// A `Status="ERROR"` we cannot parse in detail must still be an error —
    /// never a silent success.
    #[test]
    fn unparsable_error_body_still_fails() {
        let xml = r#"<ApiResponse Status="ERROR"><Errors/></ApiResponse>"#;
        let err = parse_api_error(xml).expect("Status=ERROR must always be an error");
        assert!(err.number.is_empty());
        assert!(matches!(to_provision_error(&err), ProvisionError::Other(_)));
    }

    /// The allowlist rejection echoes the source address — the seam that lets
    /// the wizard name the exact IP without any external reflector.
    #[test]
    fn echoes_the_observed_source_address() {
        let err = ApiError {
            number: "1011150".to_string(),
            message: "Invalid request IP: 203.0.113.7".to_string(),
        };
        assert_eq!(echoed_source_ip(&err), Some("203.0.113.7".to_string()));

        let msg = to_provision_error(&err).to_string();
        assert!(msg.contains("203.0.113.7"), "must name the address: {msg}");
        assert!(
            msg.contains("API Access"),
            "must name where to add it: {msg}"
        );
    }

    /// IPv6 addresses contain colons of their own, so the message must be split
    /// on the first colon, not the last.
    #[test]
    fn echoed_address_handles_ipv6() {
        let err = ApiError {
            number: "1011150".to_string(),
            message: "Invalid request IP: 2001:db8::1".to_string(),
        };
        assert_eq!(echoed_source_ip(&err), Some("2001:db8::1".to_string()));
    }

    /// Garbage where an address should be must not become a retry value.
    #[test]
    fn non_address_is_not_echoed() {
        let err = ApiError {
            number: "1011150".to_string(),
            message: "Invalid request IP: not-an-ip".to_string(),
        };
        assert_eq!(echoed_source_ip(&err), None);
    }

    /// Namecheap rejects a missing `ClientIp` outright (1010105), so the
    /// transport must never construct itself with an empty one.
    #[test]
    fn constructors_seed_a_syntactically_valid_client_ip() {
        for api in [
            NamecheapApi::new("u".into(), "k".into()),
            NamecheapApi::with_base_url("u".into(), "k".into(), "http://localhost".into()),
        ] {
            assert!(
                api.client_ip.parse::<std::net::IpAddr>().is_ok(),
                "ClientIp must always be a valid address, got {:?}",
                api.client_ip
            );
        }
    }

    /// An empty override must not blank the seed — that is the exact shape the
    /// two direct-verify shims used to pass, and it made every call fail.
    #[test]
    fn empty_client_ip_override_is_ignored() {
        let mut api = NamecheapApi::new("u".into(), "k".into());
        api.set_client_ip(String::new());
        assert_eq!(api.client_ip(), PLACEHOLDER_CLIENT_IP);
        api.set_client_ip("203.0.113.7".into());
        assert_eq!(api.client_ip(), "203.0.113.7");
    }

    /// `<Domain` is a prefix of `<DomainCheckResult`, and both appear in
    /// Namecheap responses this crate parses — the scan must not confuse them.
    #[test]
    fn element_scan_matches_exact_tag_names() {
        let xml = r#"<DomainCheckResult Domain="a.com" Available="true"/>
                     <Domain Name="b.com"/>"#;
        let domains = elements(xml, "Domain");
        assert_eq!(domains.len(), 1, "must not match DomainCheckResult");
        assert_eq!(attr(domains[0], "Name").as_deref(), Some("b.com"));

        let checks = elements(xml, "DomainCheckResult");
        assert_eq!(checks.len(), 1);
        assert_eq!(attr(checks[0], "Available").as_deref(), Some("true"));
    }

    /// The whole document is scanned, so a response that isn't pretty-printed
    /// yields every element rather than one per line.
    #[test]
    fn element_scan_is_not_line_based() {
        let xml = r#"<host Name="@" Type="A"/><host Name="www" Type="CNAME"/>"#;
        let hosts = elements(xml, "host");
        assert_eq!(hosts.len(), 2);
        assert_eq!(attr(hosts[1], "Name").as_deref(), Some("www"));
    }

    /// A `>` inside an attribute value must not terminate the tag.
    #[test]
    fn element_scan_is_quote_aware() {
        let xml = r#"<Error Number="1" Description="a > b" Extra="tail"/>"#;
        let els = elements(xml, "Error");
        assert_eq!(els.len(), 1);
        assert_eq!(attr(els[0], "Extra").as_deref(), Some("tail"));
    }

    /// Attribute lookup must not match a longer name's suffix — Namecheap's
    /// pricing element carries `Price`, `RegularPrice` and `YourPrice` side by
    /// side, so a suffix match silently reads the wrong number.
    #[test]
    fn attr_lookup_does_not_match_a_suffix() {
        let frag = r#"Price="10.98" RegularPrice="13.98" YourPrice="9.98""#;
        assert_eq!(attr(frag, "Price").as_deref(), Some("10.98"));
        assert_eq!(attr(frag, "YourPrice").as_deref(), Some("9.98"));
        assert_eq!(attr(frag, "RegularPrice").as_deref(), Some("13.98"));
        assert_eq!(attr(frag, "Nope"), None);
    }
}
