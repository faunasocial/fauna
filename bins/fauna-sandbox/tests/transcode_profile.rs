//! The `transcode` profile probed in-domain, through the real wrapper binary —
//! the pattern of the image-level probe in
//! `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py` (a read of
//! a secret attempted INSIDE a fresh `fauna-sandbox` domain, a negative control
//! read unconfined first, an allowed-path control, the enforcement status line
//! asserted), run here against the binary cargo just built so it needs no image.
//!
//! The nest wraps every ffmpeg it spawns in this profile
//! (`bins/fauna-nest/src/video/transcode.rs`); the tests below are the proof
//! that the profile both lets ffmpeg transcode and keeps it out of everything
//! beside its job directory (`deployment-threat-model.md` § Concern 1b, item 2).
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Output};

const SANDBOX: &str = env!("CARGO_BIN_EXE_fauna-sandbox");

/// The distribution ffmpeg the nest image installs (`Dockerfile`, the runtime
/// stage's `apt-get install … ffmpeg`). Named by its absolute path rather than
/// looked up on PATH: a developer's `~/.local/bin` ffmpeg lives outside every
/// tree the profile grants, so it would fail here for the right reason and
/// read as a broken profile.
const FFMPEG: &str = "/usr/bin/ffmpeg";

fn sandboxed(job_dir: &Path, cmd: &[&str]) -> Output {
    Command::new(SANDBOX)
        .arg("transcode")
        .arg("--job-dir")
        .arg(job_dir)
        .arg("--")
        .args(cmd)
        .output()
        .expect("spawn fauna-sandbox")
}

/// The wrapper must report an ENFORCED ruleset — never the warn-and-continue
/// `not enforced` fallback (`landlock::apply`), which would make every denial
/// below vacuous.
fn assert_landlock_enforced(out: &Output, ctx: &str) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("landlock: not enforced"),
        "{ctx}: Landlock is NOT enforced by this kernel — the denial proves nothing; stderr={err}"
    );
    assert!(
        err.contains("landlock: fully enforced") || err.contains("landlock: partially enforced"),
        "{ctx}: no Landlock enforcement status logged; stderr={err}"
    );
}

fn require_ffmpeg() {
    assert!(
        Path::new(FFMPEG).exists(),
        "{FFMPEG} is missing — install the distribution ffmpeg (the package the \
         nest image ships); this test exercises that binary under the profile"
    );
}

#[test]
fn transcode_profile_denies_a_secret_outside_the_job_dir() {
    let job = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let secret = data.path().join("nest.db");
    std::fs::write(&secret, b"SQLite format 3\0").unwrap();
    let granted = job.path().join("input");
    std::fs::write(&granted, b"user bytes").unwrap();

    // Negative control: unconfined, the secret is readable — so a lenient
    // profile turns the denial below red instead of silently green.
    let control = Command::new("cat").arg(&secret).output().unwrap();
    assert!(
        control.status.success(),
        "control: unconfined cat must read the secret"
    );

    // Selectivity control: inside the domain, the job directory still reads.
    let allowed = sandboxed(job.path(), &["cat", granted.to_str().unwrap()]);
    assert!(
        allowed.status.success() && allowed.stdout == b"user bytes",
        "the granted job directory must read inside the domain; stderr={}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_landlock_enforced(&allowed, "allowed-path control");

    let denied = sandboxed(job.path(), &["cat", secret.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&denied.stderr);
    assert!(
        !denied.status.success() && denied.stdout.is_empty(),
        "a file outside the job directory must be Landlock-denied; stdout={:?}",
        denied.stdout
    );
    assert!(
        err.contains("Permission denied"),
        "the denial must be EACCES (the Landlock deny), not another failure; stderr={err}"
    );
    assert_landlock_enforced(&denied, "denied secret");
}

#[test]
fn transcode_profile_lets_ffmpeg_write_its_segments() {
    require_ffmpeg();
    let job = tempfile::tempdir().unwrap();
    let input = job.path().join("input");

    // The fixture is made unconfined; only the transcode runs in the domain.
    let made = Command::new(FFMPEG)
        .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=blue:s=64x64:d=1")
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-f", "mp4"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "fixture: {}",
        String::from_utf8_lossy(&made.stderr)
    );

    let segment = job.path().join("seg000.ts");
    let out = sandboxed(
        job.path(),
        &[
            FFMPEG,
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
            "-i",
            input.to_str().unwrap(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-f",
            "mpegts",
            segment.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "ffmpeg must transcode inside the transcode profile (a missing RO tree or \
         a too-tight rlimit shows here); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::metadata(&segment).unwrap().len() > 0,
        "empty segment"
    );
    assert_landlock_enforced(&out, "ffmpeg transcode");
}

#[test]
fn transcode_profile_refuses_to_run_without_its_job_dir() {
    let out = Command::new(SANDBOX)
        .args(["transcode", "--", "true"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "transcode without --job-dir must refuse"
    );

    let job = tempfile::tempdir().unwrap();
    let out = Command::new(SANDBOX)
        .arg("dns")
        .arg("--job-dir")
        .arg(job.path())
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a profile without a job dir must refuse --job-dir"
    );
}
