//! Bounded response-body reads — one home for "read a body, but never let the
//! responder choose how much this process allocates".
//!
//! `resp.bytes()`, `.text()` and `.json()` buffer everything the remote chooses
//! to stream. Wherever the responder is not this process's own trust domain —
//! a caller-supplied URL the nest dials (`fauna-nest`'s `ssrf` module), or an
//! owner's nest a custodian pulls segments from (`fauna-sync-engine`'s
//! `nest_client`) — the body is read through here instead.

/// Why a capped body read gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CappedReadError {
    /// The body (declared or streamed) crossed the cap.
    TooLarge,
    /// The transport failed mid-read.
    Network,
}

impl std::fmt::Display for CappedReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "response exceeded the size cap",
            Self::Network => "network error",
        })
    }
}

impl std::error::Error for CappedReadError {}

/// A response body read chunk by chunk under a cap — for a body the caller
/// streams somewhere other than memory (a segment half into a staging file),
/// so what it holds at once is one transport chunk, never the body.
///
/// The cap is enforced here, not by the caller: a truthful `Content-Length`
/// refuses at [`Self::new`], before any body byte moves; the per-chunk check is
/// the real guard against an absent or dishonest one, and refuses the first
/// chunk that would cross the cap — so a refused body costs at most one chunk
/// past the cap, never the whole stream.
pub struct CappedBody {
    resp: reqwest::Response,
    max_bytes: u64,
    read: u64,
}

impl CappedBody {
    /// Start reading `resp`'s body under `max_bytes`.
    pub fn new(resp: reqwest::Response, max_bytes: u64) -> Result<Self, CappedReadError> {
        if let Some(len) = resp.content_length()
            && len > max_bytes
        {
            return Err(CappedReadError::TooLarge);
        }
        Ok(Self {
            resp,
            max_bytes,
            read: 0,
        })
    }

    /// The next chunk, or `None` at the end of the body.
    pub async fn next_chunk(&mut self) -> Result<Option<bytes::Bytes>, CappedReadError> {
        let Some(chunk) = self
            .resp
            .chunk()
            .await
            .map_err(|_| CappedReadError::Network)?
        else {
            return Ok(None);
        };
        let read = self.read.saturating_add(chunk.len() as u64);
        if read > self.max_bytes {
            return Err(CappedReadError::TooLarge);
        }
        self.read = read;
        Ok(Some(chunk))
    }

    /// Bytes handed out so far.
    pub fn read(&self) -> u64 {
        self.read
    }
}

/// Read a response body whole, failing past `max_bytes` — [`CappedBody`]
/// collected, for a body the caller does want in memory (a bounded document,
/// never a segment half).
pub async fn read_capped(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, CappedReadError> {
    let mut body = CappedBody::new(resp, max_bytes as u64)?;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.next_chunk().await? {
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Read at most `max_bytes` of a response body and drop the rest — for an
/// error body kept only as diagnostic text, where a truncated prefix is the
/// point rather than a failure. Best-effort: a network error ends the read with
/// whatever arrived. It stops reading at the cap, so an endless body costs at
/// most one chunk past it, never the whole stream.
pub async fn read_prefix(mut resp: reqwest::Response, max_bytes: usize) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < max_bytes {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(max_bytes - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    buf
}
