use fauna_core::data::{ContentHash, VideoSegment};
use fauna_nest::video::manifest::{generate_master_playlist, generate_variant_playlist};

#[test]
fn generate_master_manifest() {
    let seg_720 = VideoSegment {
        hash: ContentHash::from_digest_raw([0xaa; 32]),
        resolution: 720,
        codec: "h264".into(),
        bitrate: 2500,
        byte_size: 100_000,
    };
    let seg_360 = VideoSegment {
        hash: ContentHash::from_digest_raw([0xbb; 32]),
        resolution: 360,
        codec: "h264".into(),
        bitrate: 800,
        byte_size: 40_000,
    };

    let playlist = generate_master_playlist(&[&seg_720, &seg_360], "https://nest.example.com");

    // Header
    assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n"));

    // Both resolutions present with correct bandwidth (bitrate * 1000)
    assert!(playlist.contains("BANDWIDTH=2500000"));
    assert!(playlist.contains("BANDWIDTH=800000"));

    // Variant m3u8 references
    assert!(playlist.contains("720.m3u8"));
    assert!(playlist.contains("360.m3u8"));

    // 720p (higher resolution) should appear before 360p
    let pos720 = playlist.find("720.m3u8").unwrap();
    let pos360 = playlist.find("360.m3u8").unwrap();
    assert!(
        pos720 < pos360,
        "720p variant should appear before 360p variant"
    );
}

#[test]
fn generate_variant_playlist_test() {
    let hash_a = [0x01; 32];
    let hash_b = [0x02; 32];

    let seg_a = VideoSegment {
        hash: ContentHash::from_digest_raw(hash_a),
        resolution: 720,
        codec: "h264".into(),
        bitrate: 2500,
        byte_size: 100_000,
    };
    let seg_b = VideoSegment {
        hash: ContentHash::from_digest_raw(hash_b),
        resolution: 720,
        codec: "h264".into(),
        bitrate: 2500,
        byte_size: 105_000,
    };

    let base_url = "https://nest.example.com/api/v1/video/segments";
    let duration = 6.0;
    let playlist = generate_variant_playlist(&[seg_a, seg_b], base_url, duration);

    // Header
    assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n"));

    // Target duration is ceil of segment duration
    assert!(playlist.contains("#EXT-X-TARGETDURATION:6\n"));

    // EXTINF entries
    assert!(playlist.contains("#EXTINF:6.000,\n"));

    // Hash-based URLs
    let hex_a = hex::encode(hash_a);
    let hex_b = hex::encode(hash_b);
    assert!(
        playlist.contains(&format!("{base_url}/{hex_a}")),
        "should contain segment A URL"
    );
    assert!(
        playlist.contains(&format!("{base_url}/{hex_b}")),
        "should contain segment B URL"
    );

    // Ends with ENDLIST
    assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
}
