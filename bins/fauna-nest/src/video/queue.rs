//! Background transcode job queue backed by the local filesystem.
//!
//! Each job lives under `{queue_dir}/{job_id}/`:
//!   - `input.mp4`  — raw video bytes written at enqueue time
//!   - `meta.json`  — JSON object with `{ "resolutions": [...], "status": "pending"|"processing"|"done"|"failed" }`
//!   - `output/`    — populated with content-addressed `.ts` segments after transcoding

use std::path::PathBuf;

use anyhow::{Context, Result};
use fauna_core::data::VideoSegment;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::video::transcode::transcode_video;

/// Filesystem-backed transcode job queue.
pub struct TranscodeQueue {
    queue_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobMeta {
    resolutions: Vec<u16>,
    status: String,
}

impl TranscodeQueue {
    /// Create a new queue rooted at `{base_dir}/transcode_queue/`.
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            queue_dir: base_dir.join("transcode_queue"),
        }
    }

    /// Store raw video bytes on disk and enqueue for transcoding.
    ///
    /// The job ID is the BLAKE3 hex hash of the raw bytes.  If a job with the
    /// same ID already exists the call succeeds without overwriting it.
    ///
    /// Returns the job ID string.
    pub async fn enqueue(&self, raw_video: &[u8], resolutions: &[u16]) -> Result<String> {
        let hash_bytes: [u8; 32] = blake3::hash(raw_video).into();
        let job_id = hex::encode(hash_bytes);

        let job_dir = self.queue_dir.join(&job_id);
        fs::create_dir_all(&job_dir)
            .await
            .with_context(|| format!("create job dir {}", job_dir.display()))?;

        let input_path = job_dir.join("input.mp4");
        // Only write if not already present (idempotent re-enqueue).
        if !input_path.exists() {
            fs::write(&input_path, raw_video)
                .await
                .with_context(|| format!("write input.mp4 for job {job_id}"))?;
        }

        let meta_path = job_dir.join("meta.json");
        if !meta_path.exists() {
            let meta = JobMeta {
                resolutions: resolutions.to_vec(),
                status: "pending".to_string(),
            };
            let meta_json = serde_json::to_string(&meta).context("serialize meta")?;
            fs::write(&meta_path, meta_json)
                .await
                .with_context(|| format!("write meta.json for job {job_id}"))?;
        }

        Ok(job_id)
    }

    /// Process the next pending job.
    ///
    /// Scans the queue directory for the first job whose `meta.json` has
    /// `status == "pending"`, marks it `"processing"`, calls
    /// [`transcode_video`], then marks it `"done"` (or `"failed"` on error).
    ///
    /// Returns `None` if no pending jobs exist.
    pub async fn process_next(&self) -> Result<Option<Vec<VideoSegment>>> {
        // Ensure the queue directory exists before trying to read it.
        if !self.queue_dir.exists() {
            return Ok(None);
        }

        let mut read_dir = fs::read_dir(&self.queue_dir)
            .await
            .with_context(|| format!("read queue dir {}", self.queue_dir.display()))?;

        // Collect all job directories so we can sort for deterministic ordering.
        let mut job_dirs: Vec<PathBuf> = Vec::new();
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                job_dirs.push(path);
            }
        }
        job_dirs.sort();

        for job_dir in job_dirs {
            let meta_path = job_dir.join("meta.json");
            if !meta_path.exists() {
                continue;
            }

            let meta_bytes = fs::read(&meta_path)
                .await
                .with_context(|| format!("read {}", meta_path.display()))?;
            let mut meta: JobMeta = serde_json::from_slice(&meta_bytes)
                .with_context(|| format!("parse meta.json in {}", job_dir.display()))?;

            if meta.status != "pending" {
                continue;
            }

            // Mark as processing.
            meta.status = "processing".to_string();
            fs::write(&meta_path, serde_json::to_string(&meta).unwrap())
                .await
                .with_context(|| format!("update status for {}", job_dir.display()))?;

            let input_path = job_dir.join("input.mp4");
            let output_dir = job_dir.join("output");
            fs::create_dir_all(&output_dir)
                .await
                .with_context(|| format!("create output dir {}", output_dir.display()))?;

            let resolutions = meta.resolutions.clone();
            let transcode_result = transcode_video(&input_path, &output_dir, &resolutions).await;

            match transcode_result {
                Ok(segments) => {
                    meta.status = "done".to_string();
                    fs::write(&meta_path, serde_json::to_string(&meta).unwrap())
                        .await
                        .with_context(|| format!("write done status for {}", job_dir.display()))?;
                    return Ok(Some(segments));
                }
                Err(e) => {
                    meta.status = "failed".to_string();
                    // Best-effort status update — don't mask the original error.
                    let _ = fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).await;
                    return Err(e).with_context(|| {
                        format!("transcode failed for job {}", job_dir.display())
                    });
                }
            }
        }

        Ok(None)
    }
}
