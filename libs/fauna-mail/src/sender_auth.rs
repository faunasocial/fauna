//! The **authenticated-sender delivery stamp** — `X-Fauna-Authenticated-Sender`
//! — the one fact a delivery door leaves inside a sealed copy about *who the
//! door authenticated as the sender*, so a consumer that sees only the raw
//! RFC 5322 bytes (a Fauna app unsealing its INBOX) can bind an authorization
//! decision to a sender email proves nothing about on its own.
//!
//! **Who writes it, and what it may say** (`smtp-server.md` § Architectural
//! rules → *The `X-Fauna-*` namespace*; the one consumer rule is
//! `caldav-server.md` § Who may mutate an existing event over the inbound
//! rail → *The mail rail*): each filing door stamps only the address **it**
//! verified — the nest's `fauna.email.send` the handle-gated in-domain `From:`
//! (nothing on the off-domain bypass), the MTA's submission door the envelope
//! sender it validated as owned by the authenticated actor (never the
//! unchecked `From:` header), the MTA's MX door the `From:` addr-spec only
//! under a DMARC pass, and the bridge-presented in-domain partition the
//! validated envelope sender the authenticated bridge presents. IMAP APPEND
//! and the migration import stamp nothing. **Absent means unauthenticated**,
//! never "unknown but fine".
//!
//! **Why it can be trusted:** it lives in the reserved `X-Fauna-*` namespace,
//! which every filing door strips from the bytes it did not compose
//! ([`crate::received_header::strip_fauna_headers`]) *before* prepending its
//! own genuine stamps — the same guarantee the MDA's scorer relies on for
//! `X-Fauna-Spam-Threshold` ([`crate::aliases::HEADER_SPAM_THRESHOLD`]). A
//! sender-supplied copy never survives a door; a copy present on a sealed
//! record was written by the door that sealed it.
//!
//! Pure std, WASM-safe: the reader runs in every Fauna app including the web
//! build; the builder runs at the nest and — over UniFFI — at the Go MTA, so
//! the field name and the value grammar have exactly one home.

/// The stamp's field name. Shares the `X-Fauna-` prefix so the forgery strip
/// every filing door runs covers it without being told (a test below pins
/// that).
pub const HEADER_AUTHENTICATED_SENDER: &str = "X-Fauna-Authenticated-Sender";

/// The most bytes a stamped address may carry — RFC 5321 §4.5.3.1.3's path
/// ceiling with the angle brackets off. Longer input is refused rather than
/// cut: a cut address names nobody the door verified.
pub const MAX_AUTHENTICATED_SENDER_BYTES: usize = 254;

/// The stamp's value for an address a door has just authenticated, or `None`
/// when the address cannot be stamped as one printable header line: empty,
/// over [`MAX_AUTHENTICATED_SENDER_BYTES`], without a single `@` between two
/// non-empty halves, or carrying any byte outside printable ASCII (so no
/// CR / LF / control byte can smuggle a second header line, and no display
/// name or angle bracket rides along — the value is an **addr-spec**).
///
/// Lower-cased: the consumer compares case-insensitively anyway, and one
/// spelling keeps the stamp's bytes independent of how the door happened to
/// see the address.
#[must_use]
pub fn authenticated_sender_value(addr: &str) -> Option<String> {
    let addr = addr.trim();
    if addr.is_empty() || addr.len() > MAX_AUTHENTICATED_SENDER_BYTES {
        return None;
    }
    if !addr.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return None;
    }
    if addr
        .bytes()
        .any(|b| matches!(b, b'<' | b'>' | b'"' | b',' | b';'))
    {
        return None;
    }
    let (local, domain) = addr.split_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    Some(addr.to_ascii_lowercase())
}

/// The `(name, value)` stamp pair for [`crate::aliases::prepend_stamped_headers`]
/// — the nest-side doors' form — or `None` when
/// [`authenticated_sender_value`] refuses the address.
#[must_use]
pub fn authenticated_sender_stamp(addr: &str) -> Option<(String, String)> {
    authenticated_sender_value(addr).map(|v| (HEADER_AUTHENTICATED_SENDER.to_string(), v))
}

/// The full header line (no trailing CRLF) for the Go MTA's `prependHeaders`
/// — the doors' UniFFI form, beside `build_received_header`. Empty when the
/// address cannot be stamped, which the Go side treats as "prepend nothing":
/// a door that could not name its sender leaves the copy unstamped, and an
/// unstamped copy is refused downstream, never admitted.
#[cfg_attr(feature = "uniffi", uniffi::export)]
#[must_use]
pub fn build_authenticated_sender_stamp(addr: String) -> String {
    match authenticated_sender_value(&addr) {
        Some(v) => format!("{HEADER_AUTHENTICATED_SENDER}: {v}"),
        None => String::new(),
    }
}

/// Read the stamp back off a delivered message: the address the filing door
/// authenticated, lower-cased and trimmed, or `None` when the copy carries no
/// stamp (a door that verified nothing, a copy filed before the stamp shipped,
/// an APPEND or import) or one whose value does not parse as an addr-spec.
///
/// Header section only (never the body), first occurrence wins — the genuine
/// stamp is prepended at delivery, ahead of anything further down, and the
/// forgery strip has already removed sender-supplied copies. Fail-safe to
/// `None` if the walker cannot make progress: "no answer" is the refusing
/// direction for every consumer of this stamp.
#[must_use]
pub fn read_authenticated_sender_stamp(raw_message: &[u8]) -> Option<String> {
    use crate::header_walk::{find_body_offset, parse_header};
    let body_offset = find_body_offset(raw_message).unwrap_or(raw_message.len());
    let header_section = &raw_message[..body_offset];

    let mut i = 0;
    while i < header_section.len() {
        let (header_end, name) = parse_header(&header_section[i..]);
        if header_end == 0 {
            break;
        }
        if name
            .trim_ascii()
            .eq_ignore_ascii_case(HEADER_AUTHENTICATED_SENDER.as_bytes())
        {
            let field = &header_section[i..i + header_end];
            let colon = field.iter().position(|b| *b == b':')?;
            let value = std::str::from_utf8(&field[colon + 1..]).ok()?;
            return authenticated_sender_value(value);
        }
        i += header_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_a_lowercased_addr_spec_or_nothing() {
        assert_eq!(
            authenticated_sender_value("  Bob@Fauna.Test "),
            Some("bob@fauna.test".to_string())
        );
        for bad in [
            "",
            "   ",
            "bob",
            "@fauna.test",
            "bob@",
            "bob@a@b",
            "Bob <bob@fauna.test>",
            "\"bob\"@fauna.test",
            "bob@fauna.test\r\nX-Fauna-Spam-Threshold: 0",
            "bob@fauna.test\nEvil: 1",
            "bob@fauna.test, carol@fauna.test",
            "bøb@fauna.test",
        ] {
            assert_eq!(authenticated_sender_value(bad), None, "{bad:?}");
        }
        let long = format!("{}@x.test", "a".repeat(MAX_AUTHENTICATED_SENDER_BYTES));
        assert_eq!(authenticated_sender_value(&long), None);
    }

    #[test]
    fn stamp_forms_agree() {
        assert_eq!(
            authenticated_sender_stamp("Bob@fauna.test"),
            Some((
                HEADER_AUTHENTICATED_SENDER.to_string(),
                "bob@fauna.test".to_string()
            ))
        );
        assert_eq!(
            build_authenticated_sender_stamp("Bob@fauna.test".to_string()),
            "X-Fauna-Authenticated-Sender: bob@fauna.test"
        );
        assert_eq!(build_authenticated_sender_stamp("nope".to_string()), "");
        assert_eq!(authenticated_sender_stamp("nope"), None);
    }

    #[test]
    fn reader_takes_the_first_header_section_occurrence_only() {
        let raw = b"X-Fauna-Authenticated-Sender: Bob@Fauna.Test\r\n\
                    x-fauna-authenticated-sender: mallory@evil.test\r\n\
                    From: mallory@evil.test\r\n\
                    \r\n\
                    X-Fauna-Authenticated-Sender: mallory@evil.test\r\n";
        assert_eq!(
            read_authenticated_sender_stamp(raw),
            Some("bob@fauna.test".to_string())
        );
        // Case-insensitive on the name; a folded value still reads as one line
        // of whitespace, which the addr-spec grammar refuses — fail-safe.
        let folded = b"x-fauna-authenticated-sender: bob\r\n @fauna.test\r\n\r\n";
        assert_eq!(read_authenticated_sender_stamp(folded), None);
        assert_eq!(
            read_authenticated_sender_stamp(b"From: a@b.test\r\n\r\nbody"),
            None
        );
        assert_eq!(read_authenticated_sender_stamp(b""), None);
        // Body-only occurrence is not a stamp.
        assert_eq!(
            read_authenticated_sender_stamp(
                b"From: a@b.test\r\n\r\nX-Fauna-Authenticated-Sender: a@b.test\r\n"
            ),
            None
        );
    }

    #[test]
    fn a_similar_name_is_not_the_stamp() {
        let raw = b"X-Fauna-Authenticated-Sender-Note: bob@fauna.test\r\n\
                    X-Not-Fauna-Authenticated-Sender: bob@fauna.test\r\n\r\n";
        assert_eq!(read_authenticated_sender_stamp(raw), None);
    }

    /// The trust argument: a sender-supplied copy never survives a door,
    /// because the name sits in the namespace every door strips by prefix.
    #[cfg(feature = "received-header")]
    #[test]
    fn the_forgery_strip_removes_a_sender_supplied_stamp() {
        assert!(HEADER_AUTHENTICATED_SENDER.starts_with("X-Fauna-"));
        let forged =
            b"X-Fauna-Authenticated-Sender: bob@fauna.test\r\nFrom: m@evil.test\r\n\r\nbody";
        let stripped = crate::received_header::strip_fauna_headers(forged);
        assert_eq!(read_authenticated_sender_stamp(&stripped), None);
        // And a door's own stamp, prepended after the strip, reads back.
        let genuine = format!(
            "{}\r\n{}",
            build_authenticated_sender_stamp("m@evil.test".to_string()),
            String::from_utf8_lossy(&stripped)
        );
        assert_eq!(
            read_authenticated_sender_stamp(genuine.as_bytes()),
            Some("m@evil.test".to_string())
        );
    }
}
