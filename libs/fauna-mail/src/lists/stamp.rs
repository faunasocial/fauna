//! Stamp the authoritative RFC 2369/8058 list headers onto a client-composed
//! message (`docs/goal/behavior/mail-mass-mailing.md` § RFC 2369 list headers).
//!
//! The list owner composes one RFC 5322 message; `send_list_message` fans it
//! out one-per-member, and for each member this strips any `List-*` /
//! `Precedence` the client wrote (§ "the nest's stamp is authoritative") and
//! prepends the nest's set carrying that member's one-click token. The nest
//! enqueues the result and signs THIS byte sequence at the outbound hand-out
//! — the stamped List-* land inside the signature's `h=`
//! set (RFC 8058 §3 requires them signed). Pure byte manipulation, WASM-safe;
//! shares the continuation-aware walk with `outbound::received_strip` via
//! [`crate::header_walk`].

use crate::header_walk::{find_body_offset, parse_header};

/// Header field names stripped from the client message before prepending the
/// nest's authoritative set. Matched case-insensitively on the full field name
/// (so `List-Unsubscribe-Post` and a hypothetical `X-List-Id` are handled
/// correctly — the latter is *not* stripped).
const STRIPPED: &[&[u8]] = &[
    b"List-Id",
    b"List-Help",
    b"List-Archive",
    b"List-Unsubscribe",
    b"List-Unsubscribe-Post",
    b"Precedence",
];

fn is_stripped(name: &[u8]) -> bool {
    STRIPPED.iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// Strip any client-written `List-*` / `Precedence` headers from `raw`, then
/// prepend `new_headers` (the [`super::list_headers`] output) at the top of the
/// header block. The body is copied verbatim and the header/body separator is
/// preserved; a message with no separator is treated as all-headers (the
/// prepend still happens), matching `strip_received_headers`.
pub fn stamp_list_headers_on_message(
    raw: &[u8],
    new_headers: &[(&'static str, String)],
) -> Vec<u8> {
    let body_offset = find_body_offset(raw).unwrap_or(raw.len());
    let header_section = &raw[..body_offset];
    let body_section = &raw[body_offset..];

    let mut out = Vec::with_capacity(raw.len() + 256);
    // Prepend the nest's authoritative set, CRLF-terminated, at the very top so
    // each list header appears exactly once and DKIM signs the nest's copy.
    for (name, value) in new_headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    // Copy the surviving client headers (everything not in STRIPPED), logical-
    // header (continuation) aware so a folded value travels with its field.
    let mut i = 0;
    while i < header_section.len() {
        let (header_end, name) = parse_header(&header_section[i..]);
        let absolute_end = i + header_end;
        if !is_stripped(name) {
            out.extend_from_slice(&header_section[i..absolute_end]);
        }
        if header_end == 0 {
            // Walker stalled — copy the remainder verbatim (fail-safe).
            out.extend_from_slice(&header_section[i..]);
            break;
        }
        i = absolute_end;
    }
    out.extend_from_slice(body_section);
    out
}

#[cfg(test)]
mod tests {
    use super::super::{ListHeaderInputs, list_headers};
    use super::*;

    fn headers() -> Vec<(&'static str, String)> {
        list_headers(&ListHeaderInputs {
            friendly_name: "Weekly",
            list_id_label: "11111111-1111-1111-1111-111111111111",
            list_pattern: "weekly",
            list_domain: "fauna.example",
            primary_domain: "fauna.example",
            token: "TOK123",
            list_help_url: None,
            list_archive_url: None,
        })
    }

    #[test]
    fn prepends_set_and_preserves_body() {
        let raw = b"From: bob@fauna.example\r\nSubject: Issue 5\r\n\r\nHello subscribers\r\n";
        let out = stamp_list_headers_on_message(raw, &headers());
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.starts_with("List-Id: \"Weekly\" <weekly@fauna.example>\r\n"));
        assert!(s.contains("List-Unsubscribe: <mailto:unsubscribe+TOK123@fauna.example>"));
        assert!(s.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n"));
        assert!(s.contains("Precedence: bulk\r\n"));
        // Client headers + body preserved.
        assert!(s.contains("From: bob@fauna.example\r\n"));
        assert!(s.contains("Subject: Issue 5\r\n"));
        assert!(s.ends_with("\r\n\r\nHello subscribers\r\n"));
    }

    #[test]
    fn strips_client_supplied_list_headers() {
        // A client that pre-wrote List-* (and a folded one) must not produce a
        // second copy — the nest's stamp replaces them.
        let raw = b"From: bob@fauna.example\r\n\
                    List-Unsubscribe: <https://evil.example/u?t=forged>\r\n\
                    List-Id: spoof\r\n\
                    Precedence: list\r\n\
                    List-Archive: <https://old\r\n .example/a>\r\n\
                    Subject: hi\r\n\r\nbody";
        let out = stamp_list_headers_on_message(raw, &headers());
        let s = std::str::from_utf8(&out).unwrap();
        assert!(!s.contains("evil.example"), "forged unsubscribe stripped");
        assert!(!s.contains("spoof"), "spoofed List-Id stripped");
        assert!(
            !s.contains("old\r\n .example"),
            "folded List-Archive stripped"
        );
        assert_eq!(s.matches("List-Id:").count(), 1, "exactly one List-Id");
        assert_eq!(
            s.matches("List-Unsubscribe:").count(),
            1,
            "exactly one List-Unsubscribe"
        );
        assert_eq!(
            s.matches("Precedence:").count(),
            1,
            "exactly one Precedence"
        );
        assert!(s.contains("Precedence: bulk"), "nest's Precedence wins");
        // Unrelated client headers survive.
        assert!(s.contains("From: bob@fauna.example\r\n"));
        assert!(s.contains("Subject: hi\r\n"));
    }

    #[test]
    fn does_not_strip_non_list_lookalike_headers() {
        let raw = b"X-List-Id: keep\r\nList-ID-Custom: keep\r\nFrom: a@x\r\n\r\nb";
        let out = stamp_list_headers_on_message(raw, &headers());
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("X-List-Id: keep"));
        assert!(s.contains("List-ID-Custom: keep"));
    }
}
