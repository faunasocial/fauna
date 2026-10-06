//! Base64 line-wrapping — RFC 2045 §6.8 MIME bodies, and any other format
//! that hard-wraps standard-alphabet base64 at a fixed column with a fixed
//! line ending (e.g. RFC 7468 PEM, 64 columns, bare `\n`).

/// Base64-encode `bytes` (standard alphabet) and hard-wrap at `width` chars
/// per line, appending `line_ending` after every line, including the last.
pub fn base64_wrap(bytes: &[u8], width: usize, line_ending: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out =
        String::with_capacity(encoded.len() + encoded.len() / width * line_ending.len() + 2);
    for chunk in encoded.as_bytes().chunks(width) {
        // The base64 alphabet is pure ASCII, so every chunk is valid UTF-8.
        out.push_str(std::str::from_utf8(chunk).expect("base64 output is ASCII"));
        out.push_str(line_ending);
    }
    out
}

/// Base64-encode `bytes` and hard-wrap at 76 chars per RFC 2045 §6.8, with a
/// trailing `\r\n` after every line, including the last.
pub fn base64_wrap_76(bytes: &[u8]) -> String {
    base64_wrap(bytes, 76, "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_76_chars_with_trailing_crlf_on_every_line() {
        let wrapped = base64_wrap_76(&[0u8; 100]); // encodes to > 76 base64 chars
        let mut lines = wrapped.split("\r\n").collect::<Vec<_>>();
        assert_eq!(lines.pop(), Some(""), "must end on a CRLF, not mid-line");
        assert!(lines.len() >= 2, "100 bytes must wrap across >1 line");
        for line in &lines {
            assert!(line.len() <= 76, "line exceeds 76 chars: {line:?}");
        }
    }

    #[test]
    fn short_input_is_one_line_plus_trailing_crlf() {
        assert_eq!(base64_wrap_76(b"hi"), "aGk=\r\n");
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(base64_wrap_76(b""), "");
    }

    /// A different (width, line_ending) than the 76/`\r\n` MIME default —
    /// RFC 7468 PEM's 64-column, bare-`\n` shape.
    #[test]
    fn base64_wrap_honors_a_different_width_and_line_ending() {
        let wrapped = base64_wrap(&[0u8; 100], 64, "\n"); // > 64 base64 chars
        let mut lines = wrapped.split('\n').collect::<Vec<_>>();
        assert_eq!(lines.pop(), Some(""), "must end on a newline, not mid-line");
        assert!(lines.len() >= 2, "100 bytes must wrap across >1 line");
        for line in &lines {
            assert!(line.len() <= 64, "line exceeds 64 chars: {line:?}");
            assert!(!line.contains('\r'), "must not carry a CR: {line:?}");
        }
    }
}
