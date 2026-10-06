// HTTP Signature signing and verification.
//
// Implements draft-cavage-http-signatures as used by the Fediverse for S2S auth.

use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};

/// Parsed fields from a Signature header value.
pub struct ParsedSignature {
    pub key_id: String,
    pub headers: Vec<String>,
    pub signature_bytes: Vec<u8>,
    pub algorithm: Option<String>,
}

/// Build the signing string from an ordered list of (header-name, value) pairs.
/// Each entry is formatted as `lowercase(name): value`, joined by newlines.
pub fn build_signature_string(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{}: {}", name.to_lowercase(), value))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compute a `SHA-256=<base64>` digest string for a request body.
pub fn compute_digest(body: &[u8]) -> String {
    let hash = Sha256::digest(body);
    format!("SHA-256={}", BASE64.encode(hash))
}

/// Construct the full `Signature` header value for an HTTP request.
///
/// Signed headers always include `(request-target)`, `host`, and `date`.
/// If `digest` is `Some`, it is added as an additional signed header.
pub fn build_signature_header(
    key_id: &str,
    privkey_der: &[u8],
    method: &str,
    path: &str,
    host: &str,
    date: &str,
    digest: Option<&str>,
) -> Result<String> {
    let mut header_pairs: Vec<(String, String)> = vec![
        (
            "(request-target)".to_string(),
            format!("{} {}", method.to_lowercase(), path),
        ),
        ("host".to_string(), host.to_string()),
        ("date".to_string(), date.to_string()),
    ];

    if let Some(d) = digest {
        header_pairs.push(("digest".to_string(), d.to_string()));
    }

    let sig_string = build_signature_string(&header_pairs);
    let signature_bytes = crate::identity::rsa_sign(privkey_der, sig_string.as_bytes())?;
    let signature_b64 = BASE64.encode(&signature_bytes);

    let headers_list = header_pairs
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    Ok(format!(
        "keyId=\"{}\",algorithm=\"rsa-sha256\",headers=\"{}\",signature=\"{}\"",
        key_id, headers_list, signature_b64
    ))
}

/// Split a Signature header value on commas that are not inside quoted strings.
pub fn split_signature_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current = String::new();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }

    parts
}

/// Parse a `Signature` header value into its components.
pub fn parse_signature_header(header: &str) -> Result<ParsedSignature> {
    let params = split_signature_params(header);

    let mut key_id: Option<String> = None;
    let mut algorithm: Option<String> = None;
    let mut headers: Option<Vec<String>> = None;
    let mut signature_b64: Option<String> = None;

    for param in &params {
        if let Some((k, v)) = param.split_once('=') {
            // Strip surrounding quotes from the value.
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "keyId" => key_id = Some(v.to_string()),
                "algorithm" => algorithm = Some(v.to_string()),
                "headers" => {
                    headers = Some(v.split_whitespace().map(str::to_string).collect());
                }
                "signature" => signature_b64 = Some(v.to_string()),
                _ => {}
            }
        }
    }

    let key_id = key_id.ok_or_else(|| anyhow!("missing keyId in Signature header"))?;
    let signature_b64 =
        signature_b64.ok_or_else(|| anyhow!("missing signature in Signature header"))?;
    let signature_bytes = BASE64
        .decode(&signature_b64)
        .map_err(|e| anyhow!("invalid base64 in signature: {e}"))?;
    let headers = headers.unwrap_or_else(|| vec!["date".to_string()]);

    Ok(ParsedSignature {
        key_id,
        headers,
        signature_bytes,
        algorithm,
    })
}

/// Verify an HTTP Signature against a known public key.
///
/// `header_getter` is called with a header name (lower-case) and must return
/// the header value, or `None` if the header is absent.  The special pseudo-
/// header `(request-target)` is reconstructed from `method` and `path`.
///
/// The signed-header set is `parsed.headers` — the SIGNER's own `headers=`
/// list, not a policy this function chooses. Without a floor, a signer could
/// cover only e.g. `date` and produce a signature that verifies against any
/// method/path/body at all, since nothing outside `parsed.headers` is ever
/// checked. `(request-target)` is required unconditionally: every signer in
/// this codebase (`build_signature_header`) always includes it, so this
/// costs no legitimate caller anything, and it is the only thing binding the
/// signature to a specific method + path.
pub fn verify_signature(
    parsed: &ParsedSignature,
    pubkey_pem: &str,
    method: &str,
    path: &str,
    header_getter: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    if !parsed.headers.iter().any(|h| h == "(request-target)") {
        return Err(anyhow!(
            "signature does not cover (request-target) — refusing to verify a \
             signature that isn't bound to the request method and path"
        ));
    }

    let mut header_pairs: Vec<(String, String)> = Vec::new();

    for name in &parsed.headers {
        let value = if name == "(request-target)" {
            format!("{} {}", method.to_lowercase(), path)
        } else {
            header_getter(name).ok_or_else(|| anyhow!("required header '{}' not present", name))?
        };
        header_pairs.push((name.clone(), value));
    }

    let sig_string = build_signature_string(&header_pairs);
    let valid =
        crate::identity::rsa_verify(pubkey_pem, sig_string.as_bytes(), &parsed.signature_bytes)?;

    if valid {
        Ok(())
    } else {
        Err(anyhow!("HTTP Signature verification failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::generate_rsa_keypair;

    #[test]
    fn build_signature_string_basic() {
        let headers = vec![
            ("(request-target)".to_string(), "post /inbox".to_string()),
            ("host".to_string(), "example.com".to_string()),
            (
                "date".to_string(),
                "Sun, 20 Mar 2026 00:00:00 GMT".to_string(),
            ),
        ];
        let sig_string = build_signature_string(&headers);
        assert!(sig_string.contains("(request-target): post /inbox"));
        assert!(sig_string.contains("host: example.com"));
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (privkey_der, pubkey_pem) = generate_rsa_keypair().unwrap();
        let headers = vec![
            ("(request-target)".to_string(), "post /inbox".to_string()),
            ("host".to_string(), "example.com".to_string()),
            (
                "date".to_string(),
                "Sun, 20 Mar 2026 00:00:00 GMT".to_string(),
            ),
        ];
        let sig_string = build_signature_string(&headers);
        let signature = crate::identity::rsa_sign(&privkey_der, sig_string.as_bytes()).unwrap();
        let valid =
            crate::identity::rsa_verify(&pubkey_pem, sig_string.as_bytes(), &signature).unwrap();
        assert!(valid);
    }

    #[test]
    fn build_signature_header_format() {
        let (privkey_der, _) = generate_rsa_keypair().unwrap();
        let header = build_signature_header(
            "https://example.com/users/alice#main-key",
            &privkey_der,
            "post",
            "/inbox",
            "example.com",
            "2026-03-20T00:00:00Z",
            Some("SHA-256=abc123"),
        )
        .unwrap();
        assert!(header.contains("keyId=\"https://example.com/users/alice#main-key\""));
        assert!(header.contains("algorithm=\"rsa-sha256\""));
        assert!(header.contains("headers=\""));
        assert!(header.contains("signature=\""));
    }

    #[test]
    fn parse_signature_header_roundtrip() {
        let (privkey_der, _) = generate_rsa_keypair().unwrap();
        let header = build_signature_header(
            "https://example.com/users/alice#main-key",
            &privkey_der,
            "post",
            "/inbox",
            "example.com",
            "2026-03-20T00:00:00Z",
            None,
        )
        .unwrap();
        let parsed = parse_signature_header(&header).unwrap();
        assert_eq!(parsed.key_id, "https://example.com/users/alice#main-key");
        assert!(!parsed.signature_bytes.is_empty());
    }

    #[test]
    fn compute_digest_sha256() {
        let digest = compute_digest(b"hello");
        assert!(digest.starts_with("SHA-256="));
    }

    /// `verify_signature` iterates only `parsed.headers` — the signer's own
    /// `headers=` list — to rebuild the signing string. Nothing forces that
    /// list to include `(request-target)`, so a signature covering only
    /// `date` verifies against *any* method/path: the caller passes
    /// `method`/`path` in, but they're only consulted if `(request-target)`
    /// is actually one of the signed headers. That turns a signature meant
    /// for one request into a valid signature for every request, as long as
    /// the (unsigned) `date` header is replayed unchanged. A real signer
    /// never has a reason to omit `(request-target)` — `build_signature_header`
    /// always includes it — so refusing an under-covered signature costs no
    /// legitimate caller anything.
    #[test]
    fn verify_rejects_a_signature_not_covering_request_target() {
        let (privkey_der, pubkey_pem) = generate_rsa_keypair().unwrap();
        let date = "Sun, 20 Mar 2026 00:00:00 GMT".to_string();
        let headers = vec![("date".to_string(), date.clone())];
        let sig_string = build_signature_string(&headers);
        let signature_bytes =
            crate::identity::rsa_sign(&privkey_der, sig_string.as_bytes()).unwrap();
        let signature_b64 = BASE64.encode(&signature_bytes);
        let header = format!(
            "keyId=\"k\",algorithm=\"rsa-sha256\",headers=\"date\",signature=\"{signature_b64}\""
        );
        let parsed = parse_signature_header(&header).unwrap();

        // Same signature, replayed against a request-target it never covered.
        let result = verify_signature(
            &parsed,
            &pubkey_pem,
            "post",
            "/ap/inbox",
            |name| match name {
                "date" => Some(date.clone()),
                _ => None,
            },
        );
        assert!(
            result.is_err(),
            "a signature that never covered (request-target) must not verify against any method/path"
        );
    }

    #[test]
    fn full_sign_verify_roundtrip() {
        let (privkey_der, pubkey_pem) = generate_rsa_keypair().unwrap();
        let header = build_signature_header(
            "https://example.com/users/alice#main-key",
            &privkey_der,
            "post",
            "/inbox",
            "example.com",
            "2026-03-20T00:00:00Z",
            None,
        )
        .unwrap();
        let parsed = parse_signature_header(&header).unwrap();
        let result = verify_signature(&parsed, &pubkey_pem, "post", "/inbox", |name| match name {
            "host" => Some("example.com".to_string()),
            "date" => Some("2026-03-20T00:00:00Z".to_string()),
            _ => None,
        });
        assert!(result.is_ok(), "verification failed: {:?}", result);
    }
}
