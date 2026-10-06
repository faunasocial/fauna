//! The **revealed** remote-image fetch — the one request a fauna app makes to a
//! host the user's nest does not control.
//!
//! A `RenderBlock::RemoteImage` is a third-party `![alt](https://…)` in someone
//! else's body. Loading it tells that third party the reader opened the post, so
//! it is blocked by default and fetched only after the reader's own
//! `load-remote-content-button` (`render-model.md` § D3 — the manager owns the
//! reveal set and projects `revealed` onto the block). **Nothing here consults
//! that flag:** the gate belongs at the call site that builds the url list, so
//! this function can only ever be handed urls the reader already consented to.
//!
//! Deliberately *not* the path for the other two body-image sinks, which address
//! the user's own nest by content hash over the authenticated bulk plane — the
//! `post-image` blob and the link-preview og:image (a blob the nest itself
//! fetched and stored, § D4). Those are `GET /api/v1/blob/<hash>`; this is an
//! arbitrary host.
//!
//! **Shared because the request is identical on every Rust-native app** — only
//! what each does with the bytes differs (a `gtk::Picture` on linux, half-block
//! art on tui). Keeping one implementation is what stops the bounds below from
//! existing on one app and not the other, which is exactly the drift this crate
//! exists to prevent (priority #2).

/// How long one remote image may take. Generous — a slow blog is not an error —
/// but bounded, because nothing else ever ends this request.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The most body we will read. Far above any real inline image, far below what
/// would hurt: an app decodes these bytes into a full pixel buffer, so an
/// unbounded read is an unbounded allocation driven by a stranger's server.
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Fetch a revealed remote image's bytes, or `None`.
///
/// A **bare default client** — no auth, no cookies, no fauna headers, no
/// connection reuse with anything else: the image host is untrusted and must
/// learn nothing about the account beyond the request it was consented to
/// receive.
///
/// Every refusal — unreachable host, TLS failure, non-2xx, oversized body —
/// collapses to `None`, because every one of them means the same thing to a
/// caller: paint the placeholder. One broken image in someone else's post is
/// never a page banner (the per-item degrade the by-hash sinks already follow).
pub async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().ok()?;
    fetch_with(&client, url).await
}

/// [`fetch_bytes`] against a caller-supplied client, so a caller fetching several
/// images at once pays for one connection pool instead of one per image.
///
/// The client must carry its own timeout — a caller that builds a bare
/// `Client::new()` gets no request bound, which is the failure this module exists
/// to make hard rather than easy.
pub async fn fetch_with(client: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    use futures_util::StreamExt as _;

    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    // Read incrementally so `MAX_BYTES` bounds what is *held*, not merely what is
    // accepted: a `Content-Length` is the server's claim about itself, and a
    // chunked response makes no claim at all.
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len() + chunk.len() > MAX_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

/// A client carrying this module's bounds — what [`fetch_with`] callers should
/// build, so the timeout cannot be forgotten.
pub fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder().timeout(TIMEOUT).build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unreachable_host_degrades_to_none_rather_than_an_error() {
        // `.invalid` is reserved and never resolvable (RFC 6761 § 6.4), so this
        // asserts the degrade without touching the network.
        assert!(
            fetch_bytes("https://invalid.invalid/nope.png")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_non_http_scheme_is_refused_rather_than_panicking() {
        // A body's url is remote-authored: the producer only promotes http(s),
        // but this must not depend on that to stay safe.
        assert!(fetch_bytes("file:///etc/passwd").await.is_none());
        assert!(fetch_bytes("not a url at all").await.is_none());
    }
}
