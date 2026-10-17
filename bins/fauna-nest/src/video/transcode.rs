//! ffmpeg-based video transcoding pipeline.
//!
//! Wraps ffmpeg as a subprocess to produce HLS `.ts` segments from an input
//! video file at one or more target resolutions.  Each segment is
//! content-addressed by its BLAKE3 hash and stored as a flat file named by
//! its hex hash under `output_dir`.
//!
//! ## Isolation
//!
//! ffmpeg is a large native parser run over bytes an authenticated user chose,
//! so it runs isolated from the nest's secrets (`deployment-threat-model.md`
//! § Concern 1b, item 2):
//!
//! - **Its own job directory.** Each call makes a fresh `.transcode-<uuid>`
//!   directory (mode 0700) under `output_dir`, links the input in as `input`,
//!   runs ffmpeg there with relative paths only, moves the segments out, and
//!   removes the directory on every exit path.
//! - **A kernel sandbox, where the image provides one.** With
//!   [`SANDBOX_BIN`] present, ffmpeg runs under `fauna-sandbox transcode
//!   --job-dir <dir>`: Landlock grants that one directory read-write and the
//!   system trees read-only, so `nest.db`, `blobs`, `acme` and `keys` are
//!   kernel-denied though ffmpeg shares the nest's uid; seccomp carries the
//!   base deny-list; the wrapper's rlimits bound address space, CPU time, file
//!   size, descriptors and core dumps (`bins/fauna-sandbox/src/rlimits.rs`).
//!   Without it — a developer machine, a test run — the transcode still runs,
//!   with the measures below and a warning.
//! - **No reach beyond the file.** `-nostdin`, `-protocol_whitelist file` (no
//!   network or pipe protocol) and `-format_whitelist` (only the containers a
//!   camera or browser records — never a playlist demuxer that would open
//!   further files the input names).
//! - **No inherited environment.** `env_clear()`, then `PATH` alone.
//! - **A wall-clock bound.** [`FFMPEG_TIMEOUT`] per invocation; on expiry the
//!   process is killed. The wrapper execs into ffmpeg, which forks nothing, so
//!   that is the whole job; a compromised decoder that forked an escapee is
//!   still bounded by the wrapper's `RLIMIT_CPU` and its Landlock domain.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fauna_core::data::{ContentHash, VideoSegment};
use tokio::fs;

/// Where the nest image installs the sandbox wrapper (`Dockerfile`, the
/// `COPY … /usr/local/bin/fauna-sandbox` the mail bridges also run under).
/// Its presence is auto-detected, never configured: the image ships it, a
/// developer machine does not (`principles.md` § One configuration surface,
/// bucket 1).
pub const SANDBOX_BIN: &str = "/usr/local/bin/fauna-sandbox";

/// Wall-clock bound on one ffmpeg invocation (one resolution of one upload).
/// A constant, not a knob: no user or admin chooses how long a decoder may hang.
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The demuxers ffmpeg may open the input with — the containers phones,
/// cameras and browsers record. Everything else (HLS/DASH playlists, concat
/// lists, image sequences, raw streams) is refused before it is parsed.
const INPUT_FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm";

/// The input's name inside the job directory. A fixed string: nothing a user
/// chose ever reaches ffmpeg's command line.
const JOB_INPUT: &str = "input";

/// Transcode `input` into HLS segments at each resolution in `resolutions`.
///
/// For every resolution `res`, inside a fresh per-call job directory (see the
/// module doc's § Isolation):
/// 1. Runs ffmpeg to produce HLS `.ts` segments and a `stream.m3u8` playlist
///    in `{job}/{res}p/`.
/// 2. Reads every `.ts` segment, computes its BLAKE3 hash, and renames the
///    file to `{output_dir}/{hex_hash}` (flat, content-addressed layout).
/// 3. Returns a [`VideoSegment`] for each segment produced.
///
/// # Errors
///
/// Returns an error if:
/// - `input` does not exist.
/// - ffmpeg exits with a non-zero status, or outlives [`FFMPEG_TIMEOUT`].
/// - Any I/O operation on the output files fails.
pub async fn transcode_video(
    input: &Path,
    output_dir: &Path,
    resolutions: &[u16],
) -> Result<Vec<VideoSegment>> {
    let sandbox = Path::new(SANDBOX_BIN);
    let sandbox = if sandbox.exists() {
        Some(sandbox)
    } else {
        tracing::warn!(
            sandbox = SANDBOX_BIN,
            "transcoding without the fauna-sandbox wrapper: ffmpeg runs in its job \
             directory with file-only protocols, but without Landlock or rlimits"
        );
        None
    };
    transcode_in_job_dir(input, output_dir, resolutions, sandbox, FFMPEG_TIMEOUT).await
}

async fn transcode_in_job_dir(
    input: &Path,
    output_dir: &Path,
    resolutions: &[u16],
    sandbox: Option<&Path>,
    timeout: Duration,
) -> Result<Vec<VideoSegment>> {
    if !input.exists() {
        bail!("input file does not exist: {}", input.display());
    }
    // Absolute, because the wrapper resolves `--job-dir` from inside the job
    // directory (the child's cwd), where a relative path names something else.
    let output_dir = fs::canonicalize(output_dir)
        .await
        .with_context(|| format!("resolve output dir {}", output_dir.display()))?;

    let job_dir = output_dir.join(format!(".transcode-{}", uuid::Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(&job_dir)
        .await
        .with_context(|| format!("create job dir {}", job_dir.display()))?;

    let result = run_job(input, &output_dir, &job_dir, resolutions, sandbox, timeout).await;
    if let Err(e) = fs::remove_dir_all(&job_dir).await {
        tracing::warn!(job_dir = %job_dir.display(), "remove transcode job dir: {e}");
    }
    result
}

async fn run_job(
    input: &Path,
    output_dir: &Path,
    job_dir: &Path,
    resolutions: &[u16],
    sandbox: Option<&Path>,
    timeout: Duration,
) -> Result<Vec<VideoSegment>> {
    // A hard link where the input shares the job directory's filesystem (the
    // transcode queue's layout), a copy where it does not.
    let job_input = job_dir.join(JOB_INPUT);
    if fs::hard_link(input, &job_input).await.is_err() {
        fs::copy(input, &job_input)
            .await
            .with_context(|| format!("copy {} into the job dir", input.display()))?;
    }

    let path_env = std::env::var_os("PATH");
    let mut all_segments: Vec<VideoSegment> = Vec::new();

    for &res in resolutions {
        let res_dir = job_dir.join(format!("{}p", res));
        fs::create_dir(&res_dir)
            .await
            .with_context(|| format!("create resolution dir {}", res_dir.display()))?;

        let cmd = ffmpeg_command(job_dir, res, sandbox, path_env.as_deref());
        let status = run_bounded(cmd, timeout).await?;
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

/// ffmpeg's argument list for one resolution — every path relative to the
/// job directory, every value a constant but the scale.
fn ffmpeg_args(res: u16) -> Vec<String> {
    let scale = format!("scale=-2:{res}");
    let segments = format!("{res}p/seg%03d.ts");
    let playlist = format!("{res}p/stream.m3u8");
    [
        "-nostdin",
        "-y",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        INPUT_FORMATS,
        "-i",
        JOB_INPUT,
        "-vf",
        &scale,
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
        &segments,
        &playlist,
    ]
    .iter()
    .map(|a| a.to_string())
    .collect()
}

/// The ffmpeg invocation for one resolution: wrapped in the sandbox when one
/// is given, run from the job directory, with `PATH` its only environment.
fn ffmpeg_command(
    job_dir: &Path,
    res: u16,
    sandbox: Option<&Path>,
    path_env: Option<&OsStr>,
) -> std::process::Command {
    let mut cmd = match sandbox {
        Some(wrapper) => {
            let mut cmd = std::process::Command::new(wrapper);
            cmd.arg("transcode")
                .arg("--job-dir")
                .arg(job_dir)
                .arg("--")
                .arg("ffmpeg");
            cmd
        }
        None => std::process::Command::new("ffmpeg"),
    };
    cmd.args(ffmpeg_args(res))
        .current_dir(job_dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    cmd
}

/// Spawn `cmd` and wait at most `timeout` for it; on expiry kill it and fail.
async fn run_bounded(cmd: std::process::Command, timeout: Duration) -> Result<ExitStatus> {
    let mut cmd = tokio::process::Command::from(cmd);
    // A cancelled transcode (the queue's task dropped) takes ffmpeg with it.
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .context("failed to spawn ffmpeg — is it installed and on PATH?")?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.context("wait for ffmpeg"),
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "ffmpeg outlived its {}s wall-clock bound and was killed",
                timeout.as_secs_f64()
            )
        }
    }
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

    let mut ts_paths: Vec<PathBuf> = Vec::new();
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for `fauna-sandbox` that records what it was handed — its
    /// argv, its cwd and its whole environment — so the test asserts the
    /// command the nest really spawns rather than a builder's intent.
    fn recording_wrapper(dir: &Path) -> PathBuf {
        let script = dir.join("fake-sandbox");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             { echo \"cwd=$(pwd)\"; for a in \"$@\"; do echo \"arg=$a\"; done; \
               env | sed 's/^/env=/'; } > \"$(dirname \"$0\")/record\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[tokio::test]
    async fn sandboxed_command_is_wrapped_scoped_and_stripped() {
        let tmp = tempfile::tempdir().unwrap();
        let wrapper = recording_wrapper(tmp.path());
        // Canonical, so `pwd` in the child prints the same spelling.
        let job_dir = tmp.path().canonicalize().unwrap().join("job");
        std::fs::create_dir(&job_dir).unwrap();

        let cmd = ffmpeg_command(
            &job_dir,
            360,
            Some(&wrapper),
            Some(OsStr::new("/usr/local/bin:/usr/bin:/bin")),
        );
        let status = run_bounded(cmd, Duration::from_secs(30)).await.unwrap();
        assert!(status.success());

        let record = std::fs::read_to_string(tmp.path().join("record")).unwrap();
        let args: Vec<&str> = record
            .lines()
            .filter_map(|l| l.strip_prefix("arg="))
            .collect();
        let job = job_dir.to_str().unwrap();
        assert_eq!(&args[..5], ["transcode", "--job-dir", job, "--", "ffmpeg"]);
        let ffmpeg = &args[5..];
        let pair = |flag: &str| {
            let i = ffmpeg.iter().position(|a| *a == flag);
            i.map(|i| ffmpeg[i + 1])
        };
        assert!(ffmpeg.contains(&"-nostdin"), "{ffmpeg:?}");
        assert_eq!(pair("-protocol_whitelist"), Some("file"));
        assert_eq!(pair("-format_whitelist"), Some(INPUT_FORMATS));
        assert_eq!(pair("-i"), Some(JOB_INPUT));
        assert_eq!(pair("-hls_segment_filename"), Some("360p/seg%03d.ts"));
        assert!(
            ffmpeg.iter().all(|a| !a.starts_with('/')),
            "every ffmpeg path must be relative to the job dir: {ffmpeg:?}"
        );

        assert!(record.contains(&format!("cwd={job}\n")), "{record}");

        // env_clear + PATH: nothing of the nest's environment (the test
        // runner's HOME and CARGO_* stand in for its secrets) reaches the
        // child; `sh` itself adds only its bookkeeping variables.
        let env: Vec<&str> = record
            .lines()
            .filter_map(|l| l.strip_prefix("env="))
            .collect();
        assert!(
            env.contains(&"PATH=/usr/local/bin:/usr/bin:/bin"),
            "{env:?}"
        );
        for var in &env {
            let name = var.split('=').next().unwrap();
            assert!(
                matches!(name, "PATH" | "PWD" | "OLDPWD" | "SHLVL" | "_"),
                "inherited environment variable {name} leaked to the transcode"
            );
        }
    }

    #[test]
    fn unsandboxed_command_keeps_every_other_measure() {
        let job_dir = Path::new("/nonexistent/job");
        let cmd = ffmpeg_command(job_dir, 720, None, None);
        assert_eq!(cmd.get_program(), "ffmpeg");
        assert_eq!(cmd.get_current_dir(), Some(job_dir));
        let args: Vec<&OsStr> = cmd.get_args().collect();
        for flag in ["-nostdin", "-protocol_whitelist", "-format_whitelist"] {
            assert!(args.contains(&OsStr::new(flag)), "{flag} missing: {args:?}");
        }
        assert_eq!(cmd.get_envs().count(), 0, "no PATH given, none set");
    }

    #[tokio::test]
    async fn a_hung_child_is_killed_at_the_bound() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        let started = std::time::Instant::now();
        let err = run_bounded(cmd, Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("wall-clock bound"), "{err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the bound must end the wait, not the child's own exit"
        );
    }
}
