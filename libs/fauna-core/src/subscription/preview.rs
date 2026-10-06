use crate::data::{ContentHash, Facet, MediaItem, PostBody};

/// Maximum character length for text preview.
const MAX_PREVIEW_CHARS: usize = 280;

/// Generate an auto-preview from a full PostBody.
///
/// - Text: truncates to 280 characters + "...", drops facets beyond truncation
/// - Media: keeps thumbnails but zeroes blob_hash (indicating blurred preview)
/// - TextWithMedia: both of the above
/// - Structured: truncates content if present, blurs media
pub fn auto_preview(body: &PostBody) -> PostBody {
    match body {
        PostBody::Text { content, facets } => {
            let (truncated, truncated_facets) = truncate_text(content, facets);
            PostBody::Text {
                content: truncated,
                facets: truncated_facets,
            }
        }
        PostBody::Media { items, alt_text } => PostBody::Media {
            items: blur_media(items),
            alt_text: alt_text.clone(),
        },
        PostBody::TextWithMedia {
            content,
            facets,
            items,
        } => {
            let (truncated, truncated_facets) = truncate_text(content, facets);
            PostBody::TextWithMedia {
                content: truncated,
                facets: truncated_facets,
                items: blur_media(items),
            }
        }
        PostBody::Structured {
            schema,
            fields,
            content,
            facets,
            items,
        } => {
            let (truncated_content, truncated_facets) = match content {
                Some(c) => {
                    let (t, f) = truncate_text(c, facets);
                    (Some(t), f)
                }
                None => (None, facets.clone()),
            };
            PostBody::Structured {
                schema: schema.clone(),
                fields: fields.clone(),
                content: truncated_content,
                facets: truncated_facets,
                items: blur_media(items),
            }
        }
        PostBody::Video {
            manifest,
            thumbnail,
            duration_ms,
            aspect_ratio,
            ..
        } => PostBody::Video {
            manifest: *manifest,
            segments: vec![],
            thumbnail: *thumbnail,
            duration_ms: *duration_ms,
            aspect_ratio: *aspect_ratio,
            anchors: vec![],
        },
    }
}

/// Truncate text to MAX_PREVIEW_CHARS, respecting UTF-8 char boundaries.
/// Returns (truncated_text, filtered_facets).
fn truncate_text(content: &str, facets: &[Facet]) -> (String, Vec<Facet>) {
    if content.chars().count() <= MAX_PREVIEW_CHARS {
        return (content.to_string(), facets.to_vec());
    }

    // Find the byte offset of the 280th character
    let byte_end: usize = content
        .char_indices()
        .nth(MAX_PREVIEW_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(content.len());

    let truncated = format!("{}...", &content[..byte_end]);

    // Keep only facets that fall entirely within the truncated range
    let filtered_facets: Vec<Facet> = facets
        .iter()
        .filter(|f| (f.byte_end as usize) <= byte_end)
        .cloned()
        .collect();

    (truncated, filtered_facets)
}

/// Replace blob_hash with zeroes (signaling "blurred preview"),
/// keeping thumbnails and metadata intact.
fn blur_media(items: &[MediaItem]) -> Vec<MediaItem> {
    items
        .iter()
        .map(|item| MediaItem {
            blob_hash: ContentHash::from_digest_raw([0u8; 32]),
            media_type: item.media_type.clone(),
            size_bytes: item.size_bytes,
            dimensions: item.dimensions.clone(),
            thumbnail: item.thumbnail,
            remote_url: item.remote_url.clone(),
            alt: item.alt.clone(),
        })
        .collect()
}
