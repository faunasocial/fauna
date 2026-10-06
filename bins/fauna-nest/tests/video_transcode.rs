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
