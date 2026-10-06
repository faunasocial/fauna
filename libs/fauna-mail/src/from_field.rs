//! RFC 5322 §3.6 From-field multiplicity — the count the mail doors refuse on
//! before any reader looks at the From field.
//!
//! A message may carry exactly one `From:` field. Given two, the readers
//! downstream do not choose the same one: `mail-auth`, which DMARC aligns
//! against (`crate::auth::verify_inbound`), takes the first, while
//! `mail-parser`, which every app displays and `crate::envelope::sender_domain`
//! indexes, takes the last. `From: anyone@attacker.example` followed by
//! `From: security@<own domain>` would pass DMARC as the attacker and render as
//! the deployment. `smtp-server.md` § Architectural rules forbids exactly that
//! parser disagreement, so the doors that make a trust decision about a
//! message's sender refuse any message whose count is not one, before either
//! parser runs.
//!
//! The count is lexical, and deliberately never lower than what either parser
//! sees:
//! - a line ends at LF, with or without a preceding CR (both parsers accept
//!   LF-only messages);
//! - a bare CR inside a line starts another candidate field;
//! - the field name matches case-insensitively, with whitespace allowed before
//!   the colon (`From :`);
//! - a line starting with SP or HTAB continues the previous field — except the
//!   first line, which has no field to continue;
//! - the header section ends at the first empty line.
//!
//! Over-counting refuses only malformed mail; under-counting would reopen the
//! split. `tests/from_field_tests.rs` holds the count against both parsers over
//! adversarial shapes.
//!
//! Pure std, no dependencies, so it compiles under every feature set.

/// How many `From:` header fields `raw`'s header section carries.
///
/// The inbound SMTP DATA stage, SMTP submission and `fauna.email.send` accept a
/// message only when this is exactly one (`smtp-server.md` § Architectural
/// rules). Never fails: an empty or header-less input counts zero.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn from_field_count(raw: &[u8]) -> u32 {
    let mut count: u32 = 0;
    for (index, line) in raw.split(|&b| b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        for (segment_index, segment) in line.split(|&b| b == b'\r').enumerate() {
            let continues_previous_field =
                index > 0 && segment_index == 0 && matches!(segment.first(), Some(b' ' | b'\t'));
            if !continues_previous_field && names_from(segment) {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

/// Whether `field` is a `From` field: the bytes before its first colon, with
/// surrounding whitespace trimmed, spell `from` in any case.
fn names_from(field: &[u8]) -> bool {
    field
        .iter()
        .position(|&b| b == b':')
        .is_some_and(|colon| field[..colon].trim_ascii().eq_ignore_ascii_case(b"from"))
}

#[cfg(test)]
mod tests {
    use super::from_field_count;

    #[test]
    fn one_from_field_counts_one() {
        assert_eq!(
            from_field_count(b"From: a@x.test\r\nTo: b@y.test\r\n\r\nbody"),
            1
        );
    }

    #[test]
    fn two_from_fields_count_two() {
        assert_eq!(
            from_field_count(b"From: a@x.test\r\nFrom: b@y.test\r\n\r\nbody"),
            2
        );
        assert_eq!(
            from_field_count(b"From: a@x.test\r\nSubject: s\r\nFrom: b@y.test\r\n\r\nbody"),
            2
        );
    }

    #[test]
    fn the_name_matches_case_insensitively() {
        assert_eq!(
            from_field_count(b"FROM: a@x.test\r\nfrom: b@y.test\r\n\r\n"),
            2
        );
    }

    #[test]
    fn whitespace_before_the_colon_still_names_from() {
        assert_eq!(
            from_field_count(b"From : a@x.test\r\nFrom: b@y.test\r\n\r\n"),
            2
        );
        assert_eq!(
            from_field_count(b"From\t: a@x.test\r\nFrom: b@y.test\r\n\r\n"),
            2
        );
    }

    #[test]
    fn lf_only_lines_are_lines() {
        assert_eq!(
            from_field_count(b"From: a@x.test\nFrom: b@y.test\n\nbody"),
            2
        );
    }

    #[test]
    fn a_bare_cr_starts_another_candidate_field() {
        assert_eq!(
            from_field_count(b"From: a@x.test\rFrom: b@y.test\r\n\r\n"),
            2
        );
        assert_eq!(
            from_field_count(b"From: a@x.test\r\r\nFrom: b@y.test\r\n\r\n"),
            2
        );
    }

    #[test]
    fn the_first_line_has_no_field_to_continue() {
        assert_eq!(
            from_field_count(b" From: a@x.test\r\nFrom: b@y.test\r\n\r\n"),
            2
        );
    }

    #[test]
    fn the_body_is_never_scanned() {
        assert_eq!(
            from_field_count(b"From: a@x.test\r\n\r\nFrom: b@y.test\r\n"),
            1
        );
        assert_eq!(from_field_count(b"From: a@x.test\n\nFrom: b@y.test\n"), 1);
        assert_eq!(
            from_field_count(b"From: a@x.test\r\n\nFrom: b@y.test\r\n"),
            1
        );
        assert_eq!(
            from_field_count(b"From: a@x.test\n\r\nFrom: b@y.test\r\n"),
            1
        );
    }

    #[test]
    fn a_header_section_with_no_separator_runs_to_the_end() {
        assert_eq!(from_field_count(b"From: a@x.test\r\nFrom: b@y.test"), 2);
    }

    #[test]
    fn similar_field_names_do_not_count() {
        assert_eq!(
            from_field_count(
                b"X-From: a@x.test\r\nResent-From: b@x.test\r\nFromage: c\r\nFrom: d@x.test\r\n\r\n"
            ),
            1
        );
    }

    #[test]
    fn no_from_field_counts_zero() {
        assert_eq!(from_field_count(b"Subject: none\r\n\r\nbody"), 0);
        assert_eq!(from_field_count(b""), 0);
        assert_eq!(from_field_count(b"\r\nFrom: a@x.test\r\n"), 0);
    }
}
