//! One RFC 7489 §6.2 `dmarc-uri` — `mailto:<addr-spec>[!<size>]` — read
//! strictly, the one reader both directions share: the nest's admin write of
//! the deployment's own `rua=`/`ruf=` destination (what we publish, which is
//! concatenated into every domain's `_dmarc` TXT body) and the outbound
//! aggregate sender's read of a peer's `rua=` (where the address becomes the
//! report's `To:` header and SMTP recipient). Its address grammar,
//! [`check_report_mailbox`], is also the TLSRPT sender's check on a peer's
//! `rua=mailto:` address. Pure, no dependencies, so every
//! feature set that needs it compiles it.
//!
//! Strict means the address is an RFC 5321 `Mailbox` in its dot-atom form
//! (`docs/goal/behavior/dmarc-reporting.md` § Record shape types the field as
//! "a full `mailto:` URI"): no tag separator (`;`), no URI separator (`,`), no
//! quote, no whitespace or control character, no non-ASCII byte, an
//! `ruri-size` suffix of digits and one optional unit letter, and the RFC 5321
//! length limits. Anything else is refused — a byte that could close a TXT
//! string, start a new DMARC tag or a new mail header never gets past here.

/// RFC 5321 §4.5.3.1.1: the local part is at most 64 octets.
pub const DMARC_URI_LOCAL_PART_MAX_BYTES: usize = 64;

/// RFC 5321 §4.5.3.1.3: a path is at most 256 octets with its angle brackets,
/// so an address is at most 254.
pub const DMARC_URI_ADDRESS_MAX_BYTES: usize = 254;

/// RFC 1035 §2.3.4: a label is at most 63 octets.
const DOMAIN_LABEL_MAX_BYTES: usize = 63;

/// One `dmarc-uri`, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcUri {
    /// The mailbox, exactly as written (no case folding).
    pub address: String,
    /// The `!<size>` suffix as written, lowercased, without the `!` — kept so
    /// a canonical form round-trips the sender's own spelling.
    pub size_suffix: Option<String>,
    /// The `ruri-size` limit in bytes, when a suffix was given.
    pub max_bytes: Option<u64>,
}

impl DmarcUri {
    /// `mailto:<address>[!<size>]` — the canonical spelling, built only from
    /// validated parts (an RFC 6068 `?query` is never carried over).
    pub fn canonical(&self) -> String {
        match &self.size_suffix {
            Some(s) => format!("mailto:{}!{s}", self.address),
            None => format!("mailto:{}", self.address),
        }
    }
}

/// Why a string is not one `dmarc-uri`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmarcUriError {
    /// Not a `mailto:` URI (`https:` above all — DMARC reports are mailto-only).
    NotMailto,
    /// A byte that may never appear in one: a control character, whitespace,
    /// a quote, `;`, `,`, `<`, `>`, `\` or a non-ASCII byte.
    ForbiddenByte,
    /// The address is not `local@domain` in dot-atom form.
    BadAddress,
    /// The `!<size>` suffix is not digits with an optional `k`/`m`/`g`/`t`.
    BadSize,
    /// Over the RFC 5321 length limits.
    TooLong,
}

impl std::fmt::Display for DmarcUriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotMailto => "not a mailto: URI",
            Self::ForbiddenByte => {
                "contains a control character, whitespace, quote, separator or non-ASCII byte"
            }
            Self::BadAddress => "does not name one address (local@domain)",
            Self::BadSize => "has a malformed !size suffix",
            Self::TooLong => "is longer than an address may be",
        })
    }
}

impl std::error::Error for DmarcUriError {}

/// RFC 5322 `atext` minus `!` (the `ruri-size` separator, which RFC 7489 §6.2
/// says a URI must percent-encode) — plus `%`, the percent-encoding itself.
fn is_local_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"#$%&'*+-/=^_`{|}~".contains(&b)
}

fn is_forbidden(b: u8) -> bool {
    b.is_ascii_control()
        || b == b' '
        || !b.is_ascii()
        || matches!(b, b'"' | b';' | b',' | b'<' | b'>' | b'\\')
}

/// Read one `dmarc-uri`. The input is taken as is — no trimming, so a
/// caller that wants leniency about surrounding blanks trims first.
///
/// An RFC 6068 `?query` after the address is accepted and dropped (we write
/// our own headers); its bytes still pass the forbidden-byte check.
pub fn parse_dmarc_uri(uri: &str) -> Result<DmarcUri, DmarcUriError> {
    if uri.bytes().any(is_forbidden) {
        return Err(DmarcUriError::ForbiddenByte);
    }
    let scheme_len = "mailto:".len();
    if uri.len() < scheme_len || !uri[..scheme_len].eq_ignore_ascii_case("mailto:") {
        return Err(DmarcUriError::NotMailto);
    }
    let rest = &uri[scheme_len..];
    let (body, size_suffix, max_bytes) = match rest.split_once('!') {
        Some((b, s)) => {
            let s = s.to_ascii_lowercase();
            let n = parse_ruri_size(&s).ok_or(DmarcUriError::BadSize)?;
            (b, Some(s), Some(n))
        }
        None => (rest, None, None),
    };
    let address = body.split_once('?').map_or(body, |(a, _)| a);
    check_report_mailbox(address)?;
    Ok(DmarcUri {
        address: address.to_string(),
        size_suffix,
        max_bytes,
    })
}

/// Check that `address` is one plain RFC 5321 `Mailbox` in dot-atom form —
/// the address grammar every report sender applies to a peer-published
/// destination before it becomes a `To:` header and an SMTP recipient: the
/// DMARC aggregate sender (through [`parse_dmarc_uri`]) and the TLSRPT
/// sender (`crate::outbound::tlsrpt::populate_transports`). No control
/// character, whitespace, quote, separator or non-ASCII byte, no `!`, and the
/// RFC 5321 length limits — so a CR/LF can never start a new header.
pub fn check_report_mailbox(address: &str) -> Result<(), DmarcUriError> {
    if address.bytes().any(is_forbidden) {
        return Err(DmarcUriError::ForbiddenByte);
    }
    if address.len() > DMARC_URI_ADDRESS_MAX_BYTES {
        return Err(DmarcUriError::TooLong);
    }
    let (local, domain) = address.rsplit_once('@').ok_or(DmarcUriError::BadAddress)?;
    if local.len() > DMARC_URI_LOCAL_PART_MAX_BYTES {
        return Err(DmarcUriError::TooLong);
    }
    let dot_atom = |s: &str, ok: fn(u8) -> bool| {
        !s.is_empty()
            && s.split('.')
                .all(|atom| !atom.is_empty() && atom.bytes().all(ok))
    };
    if !dot_atom(local, is_local_char) {
        return Err(DmarcUriError::BadAddress);
    }
    let label_ok = domain
        .split('.')
        .all(|l| l.len() <= DOMAIN_LABEL_MAX_BYTES && !l.starts_with('-') && !l.ends_with('-'));
    if !dot_atom(domain, |b| b.is_ascii_alphanumeric() || b == b'-') || !label_ok {
        return Err(DmarcUriError::BadAddress);
    }
    Ok(())
}

/// `ruri-size` (RFC 7489 §6.2): `1*DIGIT [ "k" / "m" / "g" / "t" ]`, lowercase.
fn parse_ruri_size(s: &str) -> Option<u64> {
    let (digits, mult) = match s.as_bytes().last()? {
        b'k' => (&s[..s.len() - 1], 1u64 << 10),
        b'm' => (&s[..s.len() - 1], 1u64 << 20),
        b'g' => (&s[..s.len() - 1], 1u64 << 30),
        b't' => (&s[..s.len() - 1], 1u64 << 40),
        _ => (s, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_and_a_sized_mailto() {
        let u = parse_dmarc_uri("mailto:agg@third.example").unwrap();
        assert_eq!(u.address, "agg@third.example");
        assert_eq!(u.max_bytes, None);
        assert_eq!(u.canonical(), "mailto:agg@third.example");

        let u = parse_dmarc_uri("MAILTO:agg+x@third.example!10m").unwrap();
        assert_eq!(u.max_bytes, Some(10 << 20));
        assert_eq!(u.canonical(), "mailto:agg+x@third.example!10m");
    }

    #[test]
    fn drops_a_query() {
        let u = parse_dmarc_uri("mailto:a@b.example?subject=x").unwrap();
        assert_eq!(u.canonical(), "mailto:a@b.example");
    }

    #[test]
    fn refuses_each_forbidden_class() {
        for bad in [
            "mailto:a@b.example; ruf=mailto:x@elsewhere.example",
            "mailto:a@b.example,mailto:c@d.example",
            "mailto:a\"@b.example",
            "mailto:a@b.example\r\nBcc: x@y.example",
            "mailto:a@b.example\n",
            "mailto:a @b.example",
            "mailto:a\t@b.example",
            "mailto:<a@b.example>",
            "mailto:ä@b.example",
        ] {
            assert_eq!(
                parse_dmarc_uri(bad),
                Err(DmarcUriError::ForbiddenByte),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refuses_non_mailto_bad_address_bad_size_and_over_length() {
        assert_eq!(
            parse_dmarc_uri("https://r.example/x"),
            Err(DmarcUriError::NotMailto)
        );
        for bad in [
            "mailto:",
            "mailto:nobody",
            "mailto:@b.example",
            "mailto:a@",
            "mailto:a..b@c.example",
            "mailto:a@b..example",
            "mailto:a@-b.example",
            "mailto:a@b_c.example",
        ] {
            assert_eq!(
                parse_dmarc_uri(bad),
                Err(DmarcUriError::BadAddress),
                "{bad:?}"
            );
        }
        for bad in [
            "mailto:a@b.example!",
            "mailto:a@b.example!m",
            "mailto:a@b.example!1x",
        ] {
            assert_eq!(parse_dmarc_uri(bad), Err(DmarcUriError::BadSize), "{bad:?}");
        }
        let long_local = format!("mailto:{}@b.example", "a".repeat(65));
        assert_eq!(parse_dmarc_uri(&long_local), Err(DmarcUriError::TooLong));
        let long_addr = format!("mailto:a@{}.example", "b".repeat(250));
        assert_eq!(parse_dmarc_uri(&long_addr), Err(DmarcUriError::TooLong));
    }
}
