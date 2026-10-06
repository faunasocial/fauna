use fauna_nest::video::queue::TranscodeQueue;
use tempfile::TempDir;

#[tokio::test]
async fn enqueue_and_process_transcode_job() {
    let tmp = TempDir::new().unwrap();
    let queue = TranscodeQueue::new(tmp.path().to_path_buf());

    // Create tiny test video
    let input = tmp.path().join("test.mp4");
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64:d=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            input.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(status.success());

    let raw_data = std::fs::read(&input).unwrap();

    let job_id = queue.enqueue(&raw_data, &[360]).await.unwrap();
    assert!(!job_id.is_empty());

    let result = queue.process_next().await.unwrap();
    assert!(result.is_some());
    let segments = result.unwrap();
    assert!(!segments.is_empty());
    assert!(segments[0].byte_size > 0);
}
