//! Shared grammar for a `fauna://<scheme>` secret-carrying URI.
//!
//! [`crate::recovery::parse_recovery_kit_input`] and
//! [`crate::identity_qr::parse_import_input`] each accept the same union of
//! three input shapes — a bare 64-hex secret, the query form
//! `fauna://<scheme>?secret=<hex>[&k=v...]`, and the colon form
//! `fauna://<scheme>:<hex>` — differing only in the scheme string, the result
//! type, and which extra query params each reads. This is the one parse of
//! that shape; callers pick their own fields out of the returned params.

/// Parse a `fauna://<scheme>` secret URI (or a bare/colon-form hex secret)
/// into its hex secret plus every other `key=value` query param, in the
/// order they appeared. Case-insensitive in `scheme`; `None` on any
/// unrecognized or invalid input, or an invalid (non-64-hex) secret.
pub(crate) fn parse_fauna_secret_uri<'a>(
    input: &'a str,
    scheme: &str,
) -> Option<(String, Vec<(&'a str, &'a str)>)> {
    let trimmed = input.trim();

    if crate::hex32::is_hex64(trimmed) {
        return Some((trimmed.to_string(), Vec::new()));
    }

    let head = trimmed.get(..scheme.len())?;
    if !head.eq_ignore_ascii_case(scheme) {
        return None;
    }
    let tail = &trimmed[scheme.len()..];

    if let Some(query) = tail.strip_prefix('?') {
        let mut secret = None;
        let mut params = Vec::new();
        for param in query.split('&') {
            if let Some(v) = param.strip_prefix("secret=") {
                secret = Some(v);
            } else if let Some((k, v)) = param.split_once('=') {
                params.push((k, v));
            }
        }
        let secret = secret?;
        if !crate::hex32::is_hex64(secret) {
            return None;
        }
        return Some((secret.to_string(), params));
    }

    if let Some(secret) = tail.strip_prefix(':')
        && crate::hex32::is_hex64(secret)
    {
        return Some((secret.to_string(), Vec::new()));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paste/scan with a multibyte character straddling the scheme
    /// boundary must not panic (byte-boundary slicing) — it is simply not
    /// this scheme, exactly like a short input (dxcix; both callers ride
    /// this one parser, so this pin covers them both). Mirrors the same
    /// `get(..n)` pattern and pin shape as `claim_code::parse_claim_input`'s
    /// `multibyte_input_never_panics`.
    #[test]
    fn multibyte_input_never_panics() {
        assert_eq!(
            parse_fauna_secret_uri("fauna://identit\u{e9}", "fauna://identity"),
            None
        );
        assert_eq!(
            parse_fauna_secret_uri("fauna://recover\u{e9}", "fauna://recovery"),
            None
        );
        // A 3-byte character (✓, U+2713) straddling the same boundary.
        assert_eq!(
            parse_fauna_secret_uri("fauna://identit\u{2713}", "fauna://identity"),
            None
        );
    }
}
