//! The **native-player leg** — a random-access byte source over a nest URL,
//! every byte through the pinned client (security.md § Transport trust → *The
//! native-player leg*).
//!
//! A platform media player handed a URL dials the nest through the OS media
//! stack's own HTTP client, which trusts the OS store alone — no SPKI pin — and
//! so refuses every nest on its self-signed floor, the same-box install
//! included. So no native app hands a player a nest URL: the player reads a
//! byte source the app owns (`MediaSource.CreateFromStream`, an
//! `AVAssetResourceLoaderDelegate`, `MediaDataSource`, a seekable
//! `gio::InputStream` — render-model.md § D6c → *Inline playback*, answer
//! (1)), and every such source wraps this reader.
//!
//! **A pure function of the nest URL and the pin store.** The client is
//! [`crate::pinned_http_client`]: its verifier reads the process-global SPKI pin
//! per handshake, which the login graduated. No bearer and no session — the
//! blob route is unauthenticated by design and the proxy's playback ticket rides
//! the path's query.
//!
//! **Bounded by construction.** [`RangedNestReader::open`] and every
//! [`RangedNestReader::read_at`] are single `Range` GETs; the reader never issues
//! a whole-body GET and never holds more than one window, so a 99 MB video is
//! never in memory whole.

use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};

/// Why a ranged read failed. The player surfaces any of them as its own error.
#[derive(Debug, thiserror::Error)]
pub enum RangedReadError {
    /// The request never completed — unreachable, a refused or unpinned TLS
    /// cert, a timeout, a cut body.
    #[error("ranged read transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// The nest answered something other than 200 / 206 / 416.
    #[error("ranged read: nest answered HTTP {0}")]
    Status(u16),
    /// A 206 whose `Content-Range` does not parse as `bytes a-b/(total|*)`.
    #[error("ranged read: malformed Content-Range {0:?}")]
    BadContentRange(String),
}

/// A random-access reader over one nest resource. See the module docs.
pub struct RangedNestReader {
    http: reqwest::Client,
    url: String,
    len: Option<u64>,
    content_type: Option<String>,
    ranged: bool,
}

impl RangedNestReader {
    /// Probe `path` (nest-relative, e.g. `/api/v1/blob/<hash>`, its query
    /// carried verbatim) on `nest_url` with one `Range: bytes=0-0` GET, learning
    /// what a player needs before its first read: the total length, the content
    /// type, and whether the server ranges at all.
    pub async fn open(nest_url: &str, path: &str) -> Result<Self, RangedReadError> {
        let base = nest_url.trim_end_matches('/');
        let url = if path.starts_with('/') {
            format!("{base}{path}")
        } else {
            format!("{base}/{path}")
        };
        let http = crate::pinned_http_client(base);
        // The response body is never read: a 206 carries one byte, and a
        // non-ranging server's whole-body 200 is dropped with the connection.
        let resp = http.get(&url).header(RANGE, "bytes=0-0").send().await?;
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let (len, ranged) = match resp.status() {
            StatusCode::PARTIAL_CONTENT => (content_range(&resp)?.total, true),
            // An empty resource: `bytes=0-0` is unsatisfiable, the total is `*/0`.
            StatusCode::RANGE_NOT_SATISFIABLE => (content_range(&resp)?.total, true),
            StatusCode::OK => (header_u64(&resp, CONTENT_LENGTH.as_str()), false),
            other => return Err(RangedReadError::Status(other.as_u16())),
        };
        Ok(Self {
            http,
            url,
            len,
            content_type,
            ranged,
        })
    }

    /// The resource's total length — `None` when the server named none (the
    /// media proxy relaying an upstream that sent no length); a player reports
    /// that as unknown length.
    pub fn len(&self) -> Option<u64> {
        self.len
    }

    /// True when the resource is known to be empty.
    pub fn is_empty(&self) -> bool {
        self.len == Some(0)
    }

    /// The response's `Content-Type`, if it sent one.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Whether the server answered the probe's `Range` with a 206 (or a 416).
    pub fn supports_ranges(&self) -> bool {
        self.ranged
    }

    /// Up to `max_len` bytes starting at `offset`, as one
    /// `Range: bytes=offset-(offset+max_len-1)` GET: shorter at EOF, empty past
    /// it (the 416) or for `max_len == 0` (no request).
    pub async fn read_at(&self, offset: u64, max_len: u64) -> Result<Vec<u8>, RangedReadError> {
        if max_len == 0 {
            return Ok(Vec::new());
        }
        let last = offset.saturating_add(max_len - 1);
        let mut resp = self
            .http
            .get(&self.url)
            .header(RANGE, format!("bytes={offset}-{last}"))
            .send()
            .await?;
        let cap = usize::try_from(max_len).unwrap_or(usize::MAX);
        // `skip` is how far into the body the window starts: 0 for a 206 (the
        // body IS the range), `offset` for a 200 from a server that ignored the
        // `Range` — correct but slow, the whole prefix streams past and is
        // dropped chunk by chunk, never held.
        let mut skip = match resp.status() {
            StatusCode::PARTIAL_CONTENT => 0,
            StatusCode::RANGE_NOT_SATISFIABLE => return Ok(Vec::new()),
            StatusCode::OK => offset,
            other => return Err(RangedReadError::Status(other.as_u16())),
        };
        let mut out = Vec::with_capacity(cap.min(1 << 20));
        while out.len() < cap {
            let Some(chunk) = resp.chunk().await? else {
                break;
            };
            let mut chunk = &chunk[..];
            if skip > 0 {
                let n = usize::try_from(skip).unwrap_or(usize::MAX).min(chunk.len());
                chunk = &chunk[n..];
                skip -= n as u64;
            }
            let take = chunk.len().min(cap - out.len());
            out.extend_from_slice(&chunk[..take]);
        }
        Ok(out)
    }
}

/// A parsed `Content-Range: bytes a-b/total` (or `bytes */total` on a 416).
struct ContentRange {
    total: Option<u64>,
}

fn content_range(resp: &reqwest::Response) -> Result<ContentRange, RangedReadError> {
    let raw = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    parse_content_range(raw).ok_or_else(|| RangedReadError::BadContentRange(raw.to_string()))
}

fn parse_content_range(raw: &str) -> Option<ContentRange> {
    let rest = raw.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    if span != "*" {
        let (a, b) = span.split_once('-')?;
        a.trim().parse::<u64>().ok()?;
        b.trim().parse::<u64>().ok()?;
    }
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some(ContentRange { total })
}

fn header_u64(resp: &reqwest::Response, name: &str) -> Option<u64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::parse_content_range;

    #[test]
    fn content_range_forms() {
        assert_eq!(
            parse_content_range("bytes 0-0/1000").unwrap().total,
            Some(1000)
        );
        assert_eq!(parse_content_range("bytes 5-9/*").unwrap().total, None);
        assert_eq!(parse_content_range("bytes */0").unwrap().total, Some(0));
        assert!(parse_content_range("bytes 0-0").is_none());
        assert!(parse_content_range("items 0-0/1").is_none());
        assert!(parse_content_range("bytes x-0/1").is_none());
    }
}
