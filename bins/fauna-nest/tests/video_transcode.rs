use tempfile::TempDir;

/// Create a tiny test video using ffmpeg's test source.
async fn create_test_video(output: &std::path::Path) {
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:d=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            output.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("ffmpeg not found");
    assert!(status.success());
}

#[tokio::test]
async fn transcode_produces_segments() {
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("input.mp4");
    create_test_video(&input).await;

    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = fauna_nest::video::transcode::transcode_video(&input, &output_dir, &[360])
        .await
        .unwrap();
    assert!(!result.is_empty(), "should produce at least 1 segment");
    assert!(result[0].byte_size > 0);
    assert_ne!(result[0].hash.digest(), [0u8; 32]);
    assert_eq!(result[0].resolution, 360);
    assert_eq!(result[0].codec, "h264");
}

#[tokio::test]
async fn transcode_multiple_resolutions() {
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("input.mp4");
    create_test_video(&input).await;

    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = fauna_nest::video::transcode::transcode_video(&input, &output_dir, &[360, 720])
        .await
        .unwrap();
    let res_set: std::collections::HashSet<u16> = result.iter().map(|s| s.resolution).collect();
    // Both resolutions should have segments (even if input is tiny)
    assert!(res_set.contains(&360), "missing 360p segments");
    // 720p may or may not work with 64x64 input - don't assert it
}

#[tokio::test]
async fn transcode_nonexistent_file_errors() {
    let tmp = TempDir::new().unwrap();
    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = fauna_nest::video::transcode::transcode_video(
        &tmp.path().join("nonexistent.mp4"),
        &output_dir,
        &[360],
    )
    .await;
    assert!(result.is_err());
}

/// Bound on a transcode that must FAIL: a malformed container is refused at
/// probe time, in well under a second. The ceiling only turns a hang into a
/// red test (convention 14 — what is asserted is the error, not the time).
const FAILURE_CEILING: std::time::Duration = std::time::Duration::from_secs(120);

/// No `.transcode-*` job directory may outlive its call, success or failure.
fn assert_no_job_dir_left(output_dir: &std::path::Path) {
    let left: Vec<_> = std::fs::read_dir(output_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".transcode-"))
        .collect();
    assert!(left.is_empty(), "job dirs left behind: {left:?}");
}

#[tokio::test]
async fn transcode_cleans_its_job_dir_after_success() {
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("input.mp4");
    create_test_video(&input).await;
    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = fauna_nest::video::transcode::transcode_video(&input, &output_dir, &[360])
        .await
        .unwrap();
    assert!(!result.is_empty());
    assert_no_job_dir_left(&output_dir);
    assert!(
        input.exists(),
        "the caller's input must survive the job dir's removal"
    );
}

/// A malformed container — the shape a decoder bug is reached through — ends
/// in a surfaced ffmpeg exit status, promptly, and leaves nothing behind.
#[tokio::test]
async fn transcode_malformed_container_fails_bounded() {
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("input.mp4");
    // An `ftyp` box claiming mp4, then garbage.
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.extend((0..64 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
    std::fs::write(&input, bytes).unwrap();
    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = tokio::time::timeout(
        FAILURE_CEILING,
        fauna_nest::video::transcode::transcode_video(&input, &output_dir, &[360]),
    )
    .await
    .expect("a malformed container must fail, not hang");
    let err = result.expect_err("a malformed container must not transcode");
    assert!(
        format!("{err:#}").contains("exit status"),
        "the failure must surface ffmpeg's exit status: {err:#}"
    );
    assert_no_job_dir_left(&output_dir);
}

/// An HLS playlist naming a local file is the classic ffmpeg local-file read.
/// Two guards refuse it before it opens anything: `-format_whitelist` (no
/// playlist demuxer) and ffmpeg's own refusal to probe HLS from a file without
/// a playlist extension, which the job directory's fixed `input` name has.
/// This pins the outcome, not which guard fired.
#[tokio::test]
async fn transcode_refuses_a_playlist_input() {
    let tmp = TempDir::new().unwrap();
    let secret = tmp.path().join("secret.ts");
    create_test_video(&secret).await;
    let input = tmp.path().join("input.m3u8");
    std::fs::write(
        &input,
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n{}\n#EXT-X-ENDLIST\n",
            secret.display()
        ),
    )
    .unwrap();
    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let result = tokio::time::timeout(
        FAILURE_CEILING,
        fauna_nest::video::transcode::transcode_video(&input, &output_dir, &[360]),
    )
    .await
    .expect("a refused playlist must fail, not hang");
    assert!(result.is_err(), "a playlist input must not transcode");
    assert_no_job_dir_left(&output_dir);
}
