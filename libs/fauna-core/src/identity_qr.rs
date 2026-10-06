//! Identity QR code URI codec.
//!
//! Encodes/decodes Ed25519 secret keys as `fauna://identity?secret=<64_hex>` URIs
//! suitable for QR codes.

pub struct IdentityQr;

impl IdentityQr {
    /// Encode a 64-char hex secret key into a fauna:// URI, optionally carrying the
    /// handle so a scanned QR pre-fills the handle step. Per
    /// `docs/goal/behavior/onboarding.md` §1.identity_import — "QR payload is
    /// `(identity, handle)`" — an exported identity QR carries the handle when one is
    /// known; the import side reads it back via [`parse_import_input`] to pre-fill step 2.
    ///
    /// The handle is appended **verbatim** (no percent-encoding) so it round-trips
    /// through [`parse_import_input`], which reads `handle=` verbatim; a handle
    /// (`localpart@domain`) carries none of `&`/`=`/`#`. A `None` or empty handle
    /// writes the bare `?secret=` form.
    pub fn to_uri(secret_hex: &str, handle: Option<&str>) -> String {
        match handle.filter(|h| !h.is_empty()) {
            Some(h) => format!("fauna://identity?secret={secret_hex}&handle={h}"),
            None => format!("fauna://identity?secret={secret_hex}"),
        }
    }

    /// Parse a fauna://identity URI and return the 64-char hex secret.
    pub fn from_uri(uri: &str) -> Result<String, String> {
        let rest = uri
            .strip_prefix("fauna://identity?")
            .ok_or("not a fauna://identity URI")?;
        let secret = rest
            .split('&')
            .find_map(|param| param.strip_prefix("secret="))
            .ok_or("missing secret parameter")?;
        if secret.len() != 64 {
            return Err(format!("secret must be 64 hex chars, got {}", secret.len()));
        }
        hex::decode(secret).map_err(|e| format!("invalid hex: {e}"))?;
        Ok(secret.to_string())
    }
}

/// A parsed identity-import field: the Ed25519 secret plus an optional handle the QR/URI
/// payload carried (used to pre-fill the handle step). The shared shape web / iOS / android
/// all read from one parser instead of each hand-rolling the input grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedIdentity {
    /// 64-char hex Ed25519 secret (as entered; a hex secret is the same key in either case).
    pub secret: String,
    /// Handle from a `&handle=` param, when present and non-empty.
    pub handle: Option<String>,
}

impl ImportedIdentity {
    /// Flat `[secret, handle]` projection for the wasm + UniFFI faces (`handle` = `""` when
    /// absent). An empty list signals a parse failure — see the faces.
    pub fn into_parts(self) -> Vec<String> {
        vec![self.secret, self.handle.unwrap_or_default()]
    }
}

/// Parse the identity-import field (a pasted string or scanned QR payload) into an
/// [`ImportedIdentity`]. Accepts the **union** of every app's form (priority #4 — the
/// richest), case-insensitive in the `fauna://identity` scheme/host:
///
/// 1. a bare 64-char hex secret;
/// 2. the query form `fauna://identity?secret=<hex>[&handle=<h>][&…]`;
/// 3. the colon form `fauna://identity:<hex>` (iOS).
///
/// Returns `None` on any unrecognized or invalid input (the client surfaces its own
/// localized error). The `handle=` value is taken verbatim (no percent-decoding — no
/// encoder writes an encoded handle today).
pub fn parse_import_input(input: &str) -> Option<ImportedIdentity> {
    let (secret, params) = crate::secret_uri::parse_fauna_secret_uri(input, "fauna://identity")?;
    let handle = params.iter().find(|(k, _)| *k == "handle").map(|(_, v)| *v);
    Some(ImportedIdentity {
        secret,
        handle: handle.filter(|h| !h.is_empty()).map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let secret = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"; // gitleaks:allow
        let uri = IdentityQr::to_uri(secret, None);
        assert_eq!(uri, format!("fauna://identity?secret={secret}"));
        let decoded = IdentityQr::from_uri(&uri).unwrap();
        assert_eq!(decoded, secret);
    }

    #[test]
    fn to_uri_appends_handle_and_round_trips_through_parser() {
        let secret = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"; // gitleaks:allow
        // Encoder writes the `(identity, handle)` payload (onboarding.md §1.identity_import).
        let uri = IdentityQr::to_uri(secret, Some("alice@fauna.social"));
        assert_eq!(
            uri,
            format!("fauna://identity?secret={secret}&handle=alice@fauna.social")
        );
        // The import-field parser reads both halves back out — the encoder and the
        // parsers' handle branch are now real end-to-end (priority #4).
        assert_eq!(
            parse_import_input(&uri),
            Some(ImportedIdentity {
                secret: secret.into(),
                handle: Some("alice@fauna.social".into()),
            })
        );
    }

    #[test]
    fn to_uri_empty_handle_writes_bare_secret_form() {
        let secret = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"; // gitleaks:allow
        assert_eq!(
            IdentityQr::to_uri(secret, Some("")),
            format!("fauna://identity?secret={secret}")
        );
    }

    #[test]
    fn rejects_wrong_scheme() {
        let result = IdentityQr::from_uri("https://identity?secret=aaaa");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a fauna://identity URI"));
    }

    #[test]
    fn rejects_wrong_host() {
        let result = IdentityQr::from_uri("fauna://other?secret=aaaa");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a fauna://identity URI"));
    }

    #[test]
    fn rejects_invalid_hex_length() {
        let result = IdentityQr::from_uri("fauna://identity?secret=aabb");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("secret must be 64 hex chars"));
    }

    #[test]
    fn rejects_invalid_hex_chars() {
        // 64 chars but contains 'g' which is not valid hex
        let bad = "g1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let result = IdentityQr::from_uri(&format!("fauna://identity?secret={bad}"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid hex"));
    }

    #[test]
    fn parses_with_extra_params() {
        let secret = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"; // gitleaks:allow
        let uri = format!("fauna://identity?secret={secret}&extra=foo");
        let decoded = IdentityQr::from_uri(&uri).unwrap();
        assert_eq!(decoded, secret);
    }

    // ── parse_import_input: the unified import-field parser (web / iOS / android) ──

    const SECRET: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

    #[test]
    fn import_bare_hex() {
        assert_eq!(
            parse_import_input(SECRET),
            Some(ImportedIdentity {
                secret: SECRET.into(),
                handle: None
            })
        );
    }

    #[test]
    fn import_bare_hex_trims_and_preserves_case() {
        let upper = SECRET.to_ascii_uppercase();
        assert_eq!(
            parse_import_input(&format!("  {upper}\n")),
            Some(ImportedIdentity {
                secret: upper,
                handle: None
            })
        );
    }

    #[test]
    fn import_query_form_with_handle() {
        assert_eq!(
            parse_import_input(&format!(
                "fauna://identity?secret={SECRET}&handle=alice@fauna.social"
            )),
            Some(ImportedIdentity {
                secret: SECRET.into(),
                handle: Some("alice@fauna.social".into()),
            })
        );
    }

    #[test]
    fn import_query_form_without_handle() {
        assert_eq!(
            parse_import_input(&format!("fauna://identity?secret={SECRET}")),
            Some(ImportedIdentity {
                secret: SECRET.into(),
                handle: None,
            })
        );
        // Empty handle param → None handle.
        assert_eq!(
            parse_import_input(&format!("fauna://identity?secret={SECRET}&handle=")),
            Some(ImportedIdentity {
                secret: SECRET.into(),
                handle: None,
            })
        );
    }

    #[test]
    fn import_colon_form_ios() {
        // iOS-only form web/android didn't accept before — now unified (priority #4).
        assert_eq!(
            parse_import_input(&format!("fauna://identity:{SECRET}")),
            Some(ImportedIdentity {
                secret: SECRET.into(),
                handle: None,
            })
        );
    }

    #[test]
    fn import_scheme_is_case_insensitive_value_is_not() {
        let upper = SECRET.to_ascii_uppercase();
        assert_eq!(
            parse_import_input(&format!("FAUNA://Identity?secret={upper}&handle=Bob")),
            Some(ImportedIdentity {
                secret: upper,
                handle: Some("Bob".into()),
            })
        );
    }

    #[test]
    fn import_invalid_inputs_are_none() {
        assert_eq!(parse_import_input(""), None);
        assert_eq!(parse_import_input("not a secret"), None);
        assert_eq!(parse_import_input("a1b2"), None); // too short
        assert_eq!(
            parse_import_input(&format!("fauna://identity?secret=zz{}", &SECRET[2..])),
            None
        ); // non-hex
        assert_eq!(parse_import_input("fauna://identity?handle=alice"), None); // no secret
        assert_eq!(parse_import_input("https://identity?secret=x"), None); // wrong scheme
    }

    #[test]
    fn import_into_parts_contract() {
        assert_eq!(
            ImportedIdentity {
                secret: "s".into(),
                handle: Some("h".into())
            }
            .into_parts(),
            vec!["s", "h"]
        );
        assert_eq!(
            ImportedIdentity {
                secret: "s".into(),
                handle: None
            }
            .into_parts(),
            vec!["s", ""]
        );
    }
}
