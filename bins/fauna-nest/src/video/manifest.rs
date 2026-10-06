//! HLS manifest generation from video segment metadata.
//!
//! Produces m3u8 playlist strings (master and variant) from [`VideoSegment`]
//! metadata without touching the filesystem or network.

use std::collections::BTreeMap;

use fauna_core::data::VideoSegment;

/// Generate an HLS master playlist that lists one variant stream per
/// distinct resolution found in `segments`.
///
/// Resolutions are emitted highest-first. Each variant URL is
/// `{resolution}.m3u8` (relative), suitable for appending to `base_url`
/// on the client side.
pub fn generate_master_playlist(segments: &[&VideoSegment], _base_url: &str) -> String {
    // Group by resolution, keeping the highest bitrate per resolution.
    let mut by_res: BTreeMap<u16, u32> = BTreeMap::new();
    for seg in segments {
        let entry = by_res.entry(seg.resolution).or_insert(0);
        if seg.bitrate > *entry {
            *entry = seg.bitrate;
        }
    }

    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");

    // Emit highest resolution first.
    let mut resolutions: Vec<(u16, u32)> = by_res.into_iter().collect();
    resolutions.sort_by_key(|r| std::cmp::Reverse(r.0));

    for (resolution, bitrate_kbps) in resolutions {
        let bandwidth = bitrate_kbps as u64 * 1000;
        out.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}\n{resolution}.m3u8\n"
        ));
    }

    out
}

/// Generate an HLS variant (media) playlist for a sequence of segments
/// that share the same resolution.
///
/// Each segment is referenced by the hex encoding of its content hash,
/// prefixed with `base_url`.
pub fn generate_variant_playlist(
    segments: &[VideoSegment],
    base_url: &str,
    segment_duration_secs: f64,
) -> String {
    let target_duration = segment_duration_secs.ceil() as u64;

    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    out.push_str(&format!("#EXT-X-TARGETDURATION:{target_duration}\n"));

    for seg in segments {
        let hash_hex = hex::encode(seg.hash.digest());
        out.push_str(&format!(
            "#EXTINF:{segment_duration_secs:.3},\n{base_url}/{hash_hex}\n"
        ));
    }

    out.push_str("#EXT-X-ENDLIST\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::ContentHash;

    #[test]
    fn master_playlist_ordering() {
        let seg720 = VideoSegment {
            hash: ContentHash::from_digest_raw([0xaa; 32]),
            resolution: 720,
            codec: "h264".into(),
            bitrate: 2500,
            byte_size: 100_000,
        };
        let seg360 = VideoSegment {
            hash: ContentHash::from_digest_raw([0xbb; 32]),
            resolution: 360,
            codec: "h264".into(),
            bitrate: 800,
            byte_size: 40_000,
        };

        let playlist = generate_master_playlist(&[&seg720, &seg360], "https://example.com");
        assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n"));
        // 720p should appear before 360p
        let pos720 = playlist.find("720.m3u8").unwrap();
        let pos360 = playlist.find("360.m3u8").unwrap();
        assert!(pos720 < pos360);
    }
}
