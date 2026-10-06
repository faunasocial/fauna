//! HTTP/1.1 single-range requests (`Range: bytes=…`, RFC 9110 § 14) — the one
//! parser and the one 206/416 shape every nest route that serves stored bytes
//! by range calls.
//!
//! Two routes call it: the blob download's hex branch
//! (`blob_routes::download_blob`), where a `<video>` element seeks — WebKit
//! refuses to play from a server that answers no byte-range request — and the
//! segment download (`segments::segment_route::get_segment`), where a resumed
//! fetch asks for the tail it lacks (`render-model.md` § D6c → *Inline
//! playback*, answer 2: one parser, both routes).
//!
//! **Three outcomes, not two.** A header the nest cannot or will not honour as
//! one range — malformed, a unit other than `bytes`, `end < start`, or a
//! multi-range list (browsers never send one) — is IGNORED and the whole body
//! served with 200, as RFC 9110 § 14.2 directs; only a well-formed single
//! range wholly past the end is *unsatisfiable* and answers 416 with
//! `Content-Range: bytes */<total>`. An `Option` cannot tell those apart, which
//! is why [`ByteRange`] is an enum.

use axum::http::{HeaderMap, StatusCode, header, response::Builder};
use axum::response::{IntoResponse, Response};

/// What a `Range` header asks of a body `total` bytes long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// Serve `start..=end` (inclusive, `end < total`) with 206.
    Satisfiable { start: u64, end: u64 },
    /// A well-formed single range no byte of the body falls in: 416.
    Unsatisfiable,
    /// No header, or one to disregard: serve the whole body with 200.
    Ignore,
}

/// Parse one `Range` header value against a body of `total` bytes.
///
/// `bytes=a-b` (end clamped to the last byte), `bytes=a-`, and the suffix form
/// `bytes=-n` (the last `n` bytes; the whole body when `n >= total`).
pub fn parse_range(header_val: &str, total: u64) -> ByteRange {
    let Some(spec) = header_val.trim().strip_prefix("bytes=") else {
        return ByteRange::Ignore;
    };
    if spec.contains(',') {
        return ByteRange::Ignore;
    }
    let Some((start_s, end_s)) = spec.trim().split_once('-') else {
        return ByteRange::Ignore;
    };
    let (start_s, end_s) = (start_s.trim(), end_s.trim());

    if start_s.is_empty() {
        let Ok(suffix) = end_s.parse::<u64>() else {
            return ByteRange::Ignore;
        };
        if suffix == 0 || total == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Satisfiable {
            start: total.saturating_sub(suffix),
            end: total - 1,
        };
    }

    let Ok(start) = start_s.parse::<u64>() else {
        return ByteRange::Ignore;
    };
    let end = if end_s.is_empty() {
        None
    } else {
        match end_s.parse::<u64>() {
            Ok(e) if e >= start => Some(e),
            _ => return ByteRange::Ignore,
        }
    };
    if start >= total {
        return ByteRange::Unsatisfiable;
    }
    let last = total - 1;
    ByteRange::Satisfiable {
        start,
        end: end.map_or(last, |e| e.min(last)),
    }
}

/// [`parse_range`] over a request's headers: no `Range` header, or one that is
/// not visible ASCII, is [`ByteRange::Ignore`].
pub fn requested(headers: &HeaderMap, total: u64) -> ByteRange {
    match headers.get(header::RANGE).map(|h| h.to_str()) {
        Some(Ok(v)) => parse_range(v, total),
        _ => ByteRange::Ignore,
    }
}

/// Whether the request carries a `Range` header at all — the cheap test a
/// route makes before paying for the body's total length.
pub fn has_range(headers: &HeaderMap) -> bool {
    headers.contains_key(header::RANGE)
}

/// Mark `builder` as the 206 answer for `start..=end` of a `total`-byte body:
/// status, `Content-Range` and `Accept-Ranges`. The caller adds its own
/// content headers and the body (exactly `end - start + 1` bytes).
pub fn partial_content(builder: Builder, start: u64, end: u64, total: u64) -> Builder {
    builder
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        )
        .header(header::ACCEPT_RANGES, "bytes")
}

/// The 416 answer: `Content-Range: bytes */<total>` tells the client the length
/// it overshot, and no body.
pub fn not_satisfiable(total: u64) -> Response {
    (
        StatusCode::RANGE_NOT_SATISFIABLE,
        [(header::CONTENT_RANGE, format!("bytes */{total}"))],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ByteRange::*;

    #[test]
    fn closed_and_open_ended_ranges() {
        assert_eq!(
            parse_range("bytes=10-20", 100),
            Satisfiable { start: 10, end: 20 }
        );
        assert_eq!(
            parse_range("bytes=10-", 100),
            Satisfiable { start: 10, end: 99 }
        );
        assert_eq!(
            parse_range("bytes=0-0", 1),
            Satisfiable { start: 0, end: 0 }
        );
    }

    #[test]
    fn end_past_eof_clamps_to_the_last_byte() {
        assert_eq!(
            parse_range("bytes=10-200", 100),
            Satisfiable { start: 10, end: 99 }
        );
    }

    #[test]
    fn suffix_range_is_the_last_n_bytes() {
        assert_eq!(
            parse_range("bytes=-10", 100),
            Satisfiable { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range("bytes=-500", 100),
            Satisfiable { start: 0, end: 99 },
            "a suffix longer than the body is the whole body, as a 206"
        );
    }

    #[test]
    fn a_range_wholly_past_the_end_is_unsatisfiable() {
        assert_eq!(parse_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=200-300", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), Unsatisfiable);
    }

    #[test]
    fn every_range_of_an_empty_body_is_unsatisfiable() {
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), Unsatisfiable);
    }

    /// RFC 9110 § 14.2: a recipient ignores a Range it does not understand or
    /// finds invalid. A 416 here would turn a harmless odd header into a failed
    /// fetch; serving the whole body is always a correct answer.
    #[test]
    fn malformed_and_multi_range_headers_are_ignored_not_refused() {
        for raw in [
            "bytes=20-10",
            "bytes=0-1,5-6",
            "bytes=abc-",
            "bytes=-",
            "bytes=5",
            "items=0-5",
            "0-5",
            "",
        ] {
            assert_eq!(parse_range(raw, 100), Ignore, "{raw:?} must be ignored");
        }
    }

    #[test]
    fn requested_ignores_a_missing_header() {
        let mut h = HeaderMap::new();
        assert_eq!(requested(&h, 10), Ignore);
        assert!(!has_range(&h));
        h.insert(header::RANGE, "bytes=2-3".parse().unwrap());
        assert!(has_range(&h));
        assert_eq!(requested(&h, 10), Satisfiable { start: 2, end: 3 });
    }

    #[test]
    fn the_416_names_the_total() {
        let resp = not_satisfiable(42);
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(resp.headers()[header::CONTENT_RANGE], "bytes */42");
    }

    #[test]
    fn the_206_carries_content_range_and_accept_ranges() {
        let resp = partial_content(Response::builder(), 2, 5, 10)
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(resp.headers()[header::ACCEPT_RANGES], "bytes");
    }
}
