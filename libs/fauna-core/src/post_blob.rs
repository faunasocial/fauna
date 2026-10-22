//! The browser's post-blob plane — a post's own bytes (the sealed body behind
//! `gated_blob_hash`, its image, video and C2PA probe) read by content address
//! from the nest they rest on, the web twin of the native
//! `fauna_client::post_blob::PostBlobHosts` (`transport-trust.md` § Transport
//! trust → *Mechanism for the federation-granted row* → *The post arm* → *The
//! browser's leg*).
//!
//! That nest is the session's own for a post it holds, and the post's **home**
//! (`fauna_feed::PostSummary::blob_home`) for one it only relays or was
//! delivered: the home's open-CORS `GET /api/v1/blob/{hash}`, read direct, never
//! relayed through the reader's nest. The properties the native plane holds hold
//! here, from the same two shared pieces:
//!
//! - **no bearer** — the by-hash route is open by possession (a content hash is
//!   the capability), so no read here carries one, and a foreign home never sees
//!   the session's;
//! - **capped** — a reply past [`INLINE_BLOB_BODY_LIMIT`] is refused, its
//!   declared length before any byte and its streamed body under the cap
//!   ([`crate::file_download::read_body_capped`]);
//! - **content-addressed** — bytes are kept only when they are the content the
//!   hash names ([`checked_post_blob`], over
//!   [`crate::encoding::content_address_matches`]).
//!
//! What it does NOT hold is the pin: a browser fetch is WebPKI only and hands
//! WASM no certificate, so there is no SPKI to graduate against and the home's
//! stamped `nest_actor_id` is carried but cannot be used. A home serving the
//! self-signed floor is unreachable from the SPA and its post stays teased —
//! the declared browser residual, not a gap this plane closes.
//!
//! [`INLINE_BLOB_BODY_LIMIT`]: crate::attachment_limits::INLINE_BLOB_BODY_LIMIT

use anyhow::Result;

/// `GET`/`HEAD` URL of the blob `hash` on the nest at `base_url` (a trailing
/// slash on the base is dropped).
pub fn post_blob_url(base_url: &str, hash: &str) -> String {
    format!(
        "{}/api/v1/blob/{hash}",
        base_url.trim().trim_end_matches('/')
    )
}

/// `bytes`, kept only when they are the content `hash` names — the one
/// acceptance step of every post-blob read. A refusal is never transient: the
/// same home serves the same wrong bytes on every retry.
pub fn checked_post_blob(hash: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
    anyhow::ensure!(
        crate::encoding::content_address_matches(hash, &bytes),
        "blob {hash}: the served bytes are not the named content"
    );
    Ok(bytes)
}

/// The blob `hash` from the nest at `base_url` — anonymous, capped and verified
/// against its content address (the module docs).
#[cfg(all(target_arch = "wasm32", feature = "blob-fetch-wasm"))]
pub async fn get_post_blob(base_url: &str, hash: &str) -> Result<Vec<u8>> {
    use crate::file_download::{check_declared_body_len, read_body_capped, read_error_body};

    let cap = crate::attachment_limits::INLINE_BLOB_BODY_LIMIT as u64;
    let url = post_blob_url(base_url, hash);
    let resp = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        let message = read_error_body(resp).await;
        anyhow::bail!("GET {url} failed ({status}): {message}");
    }
    check_declared_body_len(resp.headers().get("content-length").as_deref(), cap)
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
    let bytes = read_body_capped(resp, cap)
        .await
        .map_err(|e| anyhow::anyhow!("GET {url} read body: {e}"))?;
    checked_post_blob(hash, bytes)
}

/// Whether the blob `hash` on the nest at `base_url` asserts C2PA provenance —
/// the `x-c2pa` HEAD, a pre-filter only (the verdict is a manifest found in the
/// bytes [`get_post_blob`] returns). Readable cross-origin because the open
/// layer exposes the header.
#[cfg(all(target_arch = "wasm32", feature = "blob-fetch-wasm"))]
pub async fn head_has_c2pa(base_url: &str, hash: &str) -> Result<bool> {
    let url = post_blob_url(base_url, hash);
    let resp = gloo_net::http::RequestBuilder::new(&url)
        .method(gloo_net::http::Method::HEAD)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HEAD {url}: {e}"))?;
    let status = resp.status();
    anyhow::ensure!((200..300).contains(&status), "HEAD {url} failed ({status})");
    Ok(resp.headers().get("x-c2pa").as_deref() == Some("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_hex(bytes: &[u8]) -> String {
        hex::encode(crate::encoding::content_hash(bytes).digest())
    }

    #[test]
    fn the_named_content_is_kept() {
        let hash = hash_hex(b"the real image");
        assert_eq!(
            checked_post_blob(&hash, b"the real image".to_vec()).unwrap(),
            b"the real image"
        );
    }

    /// Substitute bytes under a hash are an error, never bytes to paint — the
    /// browser twin of `fauna_ffi::post_blob`'s case.
    #[test]
    fn substitute_bytes_are_an_error() {
        let hash = hash_hex(b"the real image");
        assert!(checked_post_blob(&hash, b"a substitute".to_vec()).is_err());
    }

    #[test]
    fn the_url_drops_a_trailing_slash() {
        assert_eq!(
            post_blob_url(" https://home.example/ ", "ab"),
            "https://home.example/api/v1/blob/ab"
        );
    }
}
