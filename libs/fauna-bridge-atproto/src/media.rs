//! Media privacy proxy: URL validation and streaming logic.

/// Allowed media CDN domains for proxying.
const ALLOWED_DOMAINS: &[&str] = &["cdn.bsky.app", "video.bsky.app"];

/// Default maximum media size in bytes (50 MB).
pub const DEFAULT_MAX_MEDIA_SIZE: u64 = 50 * 1024 * 1024;

/// Validate that a URL is safe to proxy (matches allowed Bluesky CDN domains).
pub fn validate_media_url(url_str: &str) -> Result<(), &'static str> {
    let parsed = url::Url::parse(url_str).map_err(|_| "invalid URL")?;

    if parsed.scheme() != "https" {
        return Err("only HTTPS URLs allowed");
    }

    let host = parsed.host_str().ok_or("no host in URL")?;
    if !ALLOWED_DOMAINS.contains(&host) {
        return Err("domain not in allowlist");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_cdn_urls() {
        assert!(
            validate_media_url(
                "https://cdn.bsky.app/img/feed_thumbnail/plain/did:plc:xxx/bafkrei/jpeg"
            )
            .is_ok()
        );
        assert!(
            validate_media_url("https://video.bsky.app/watch/did:plc:xxx/bafkrei/playlist.m3u8")
                .is_ok()
        );
    }

    #[test]
    fn rejects_non_cdn_urls() {
        assert!(validate_media_url("https://evil.com/img.jpg").is_err());
        assert!(validate_media_url("http://cdn.bsky.app/img.jpg").is_err());
        assert!(validate_media_url("https://cdn.bsky.app.evil.com/img.jpg").is_err());
    }
}
