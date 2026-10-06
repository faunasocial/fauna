//! ffmpeg-based video transcoding pipeline.
//!
//! Wraps ffmpeg as a subprocess to produce HLS `.ts` segments from an input
//! video file at one or more target resolutions.  Each segment is
//! content-addressed by its BLAKE3 hash and stored as a flat file named by
//! its hex hash under `output_dir`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use fauna_core::data::{ContentHash, VideoSegment};
use tokio::fs;

/// Transcode `input` into HLS segments at each resolution in `resolutions`.
///
/// For every resolution `res`:
/// 1. Creates `{output_dir}/{res}p/` and runs ffmpeg to produce HLS `.ts`
///    segments and a `stream.m3u8` playlist inside that subdirectory.
/// 2. Reads every `.ts` segment, computes its BLAKE3 hash, and renames the
///    file to `{output_dir}/{hex_hash}` (flat, content-addressed layout).
/// 3. Returns a [`VideoSegment`] for each segment produced.
///
/// # Errors
///
/// Returns an error if:
/// - `input` does not exist.
/// - ffmpeg exits with a non-zero status.
/// - Any I/O operation on the output files fails.
pub async fn transcode_video(
    input: &Path,
    output_dir: &Path,
    resolutions: &[u16],
) -> Result<Vec<VideoSegment>> {
    if !input.exists() {
        bail!("input file does not exist: {}", input.display());
    }

    let mut all_segments: Vec<VideoSegment> = Vec::new();

    for &res in resolutions {
        let res_dir = output_dir.join(format!("{}p", res));
        fs::create_dir_all(&res_dir)
            .await
            .with_context(|| format!("create resolution dir {}", res_dir.display()))?;

        let m3u8_path = res_dir.join("stream.m3u8");
        let seg_pattern = res_dir.join("seg%03d.ts");

        // Build the ffmpeg argument list.
        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                input.to_str().context("input path is not valid UTF-8")?,
                "-vf",
                &format!("scale=-2:{res}"),
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "28",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
                "-f",
                "hls",
                "-hls_time",
                "4",
                "-hls_list_size",
                "0",
                "-hls_segment_filename",
                seg_pattern
                    .to_str()
                    .context("seg pattern path is not valid UTF-8")?,
                m3u8_path.to_str().context("m3u8 path is not valid UTF-8")?,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .context("failed to spawn ffmpeg — is it installed and on PATH?")?;

        if !status.success() {
            bail!(
                "ffmpeg failed for resolution {}p (exit status: {})",
                res,
                status
            );
        }

        // Collect and content-address every .ts segment produced.
        let segments = collect_ts_segments(&res_dir, output_dir, res).await?;
        all_segments.extend(segments);
    }

    Ok(all_segments)
}

/// Read every `.ts` file in `res_dir`, hash it, rename to `output_dir/{hex}`,
/// and return a [`VideoSegment`] for each one.
async fn collect_ts_segments(
    res_dir: &Path,
    output_dir: &Path,
    resolution: u16,
) -> Result<Vec<VideoSegment>> {
    let mut read_dir = fs::read_dir(res_dir)
        .await
        .with_context(|| format!("read_dir {}", res_dir.display()))?;

    let mut ts_paths: Vec<std::path::PathBuf> = Vec::new();
    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("ts") {
            ts_paths.push(path);
        }
    }

    // Process in deterministic order so callers get a stable sequence.
    ts_paths.sort();

    let mut segments = Vec::with_capacity(ts_paths.len());

    for ts_path in ts_paths {
        let data = fs::read(&ts_path)
            .await
            .with_context(|| format!("read segment {}", ts_path.display()))?;

        let hash_bytes: [u8; 32] = blake3::hash(&data).into();
        let hash_hex = hex::encode(hash_bytes);

        let dest = output_dir.join(&hash_hex);
        fs::rename(&ts_path, &dest)
            .await
            .with_context(|| format!("rename {} → {}", ts_path.display(), dest.display()))?;

        let byte_size = data.len() as u64;

        // Estimate bitrate from byte size; ffmpeg HLS segments are 4 seconds.
        // bitrate (kbps) = (bytes * 8) / (duration_secs * 1000)
        let bitrate_kbps = ((byte_size * 8) / (4 * 1000)).max(1) as u32;

        segments.push(VideoSegment {
            hash: ContentHash::from_digest_raw(hash_bytes),
            resolution,
            codec: "h264".to_string(),
            bitrate: bitrate_kbps,
            byte_size,
        });
    }

    Ok(segments)
}
