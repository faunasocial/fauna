//! What build this process is — the fields `/api/v1/health` serves so the
//! release pipeline can tell one artifact from another, and the deploy gate can
//! tell what a box is running without a shell on it.
//!
//! Owner of these fields; both health handlers ([`crate::routes::health`] and
//! [`crate::degraded_serve`]'s outdated twin) read it from here rather than
//! re-deriving it, because a degraded box that could not be identified would be
//! exactly the box you most need to identify.
//!
//! **The two fields are stamped by deliberately different mechanisms, and the
//! asymmetry is the point:**
//!
//! - [`commit`] is a **source** fact, baked at **compile time** (`option_env!`,
//!   `Dockerfile`'s `rust-native` stage sets `FAUNA_BUILD_COMMIT` before
//!   `cargo build`). It cannot be changed by how the container is run.
//! - [`build_id`] is an **artifact** fact, read at **runtime** from the image
//!   config's `FAUNA_BUILD_ID` env (`Dockerfile`'s final stage). Runtime is the
//!   correct seam for three reasons: the value changes on *every* build, so a
//!   compile-time stamp would recompile `fauna-nest` on each re-dispatch of an
//!   unchanged commit; the image config is readable off an already-built
//!   artifact (`docker inspect`), which is what lets `restage-nest.yml` verify
//!   an image it did not build; and it makes the property testable without one
//!   image build per case.
//!
//! Why the pair exists at all: the promotion gate proved a *commit* while the
//! pipeline promoted an *artifact*, and nothing bound the two — so two builds of
//! the same commit were indistinguishable and a container the workflow never
//! built satisfied the gate.
//!
//! Both fields are served **unauthenticated by deliberate design** — see the
//! disclosure rationale on [`crate::routes::health`]. `build_id` leaks strictly
//! less than the `commit` beside it: a CI run number reveals nothing about the
//! source that `commit` does not already reveal.

/// The commit this binary was compiled from, or `"dev"` for a local build.
///
/// Prefix-tolerant on the consuming side: workflow builds stamp the full
/// 40-char sha, `just docker-push-dev` the short 8.
pub fn commit() -> &'static str {
    option_env!("FAUNA_BUILD_COMMIT").unwrap_or("dev")
}

/// The individual build that produced this artifact, or `"dev"` when nothing
/// stamped one (a local `cargo build`).
///
/// Unique per build, never per commit: the pipeline passes
/// `<run_id>-<run_attempt>` and `just docker-push-dev` a `dev-<epoch>` that
/// cannot collide with it. Matched **exactly** by the dev-fleet deploy-verify
/// serving check's `--expect-build-id` — a prefix relation between two
/// build ids means they are different builds.
pub fn build_id() -> String {
    std::env::var("FAUNA_BUILD_ID").unwrap_or_else(|_| "dev".to_string())
}

/// Every binary the nest image ships, by the name it runs under. The running
/// nest is hashed as itself ([`std::env::current_exe`]); the others are looked
/// for beside it, which is where the image installs them (`/usr/local/bin`). A
/// sibling that is not there (a local `cargo build`, an image older than the
/// binary) is left out of the map rather than reported as an error.
pub const SHIPPED_BINARIES: [&str; 3] = ["fauna-nest", "fauna-mail-bridge", "fauna-atproto-bridge"];

/// The sha256 of each shipped binary present beside this process, hex, keyed by
/// [`SHIPPED_BINARIES`] name — or `None` while the hashing is still running.
///
/// This is what the deploy binary gate compares against the binaries in the
/// image it built: the gate has no shell
/// on a box (`docs/goal/architecture/deployment-threat-model.md` § 1a), so the
/// box reports its own binaries over the anonymous health read. The image's
/// own digest is not served because no image can carry it — the digest is a
/// hash over the image, stamp included; the per-binary hashes plus
/// [`build_id`] identify the artifact more sharply anyway, and they are what
/// the two incidents behind the gate were about (`build-system.md` § Verifying
/// a deploy carried the binaries).
///
/// Hashed once, on a background thread the first call starts, so that no
/// health request ever waits on reading a few hundred megabytes and a process
/// that is never asked (most test nests) pays only when it is. The field is
/// absent from health until the thread finishes; the gate retries.
pub fn binary_hashes() -> Option<&'static std::collections::BTreeMap<String, String>> {
    static HASHES: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
        std::sync::OnceLock::new();
    static STARTED: std::sync::Once = std::sync::Once::new();
    if let Some(done) = HASHES.get() {
        return Some(done);
    }
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("binary-hashes".into())
            .spawn(|| {
                let _ = HASHES.set(hash_shipped_binaries());
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the binary-hash thread; health will omit `binaries`");
        }
    });
    HASHES.get()
}

fn hash_shipped_binaries() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(exe) = std::env::current_exe() else {
        return out;
    };
    let dir = exe.parent().map(std::path::Path::to_path_buf);
    for name in SHIPPED_BINARIES {
        let path = if name == "fauna-nest" {
            exe.clone()
        } else {
            match &dir {
                Some(d) => d.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
                None => continue,
            }
        };
        if let Ok(hex) = sha256_file(&path) {
            out.insert(name.to_string(), hex);
        }
    }
    out
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// The identity body both health handlers serve, `status` aside — the field
/// set that makes a degraded box identifiable is one shape, not two hand-kept
/// copies.
///
/// `update_channel` is the channel id in force, `null` on a pinned box and on
/// the degraded needs-update boot, which opened no database
/// (`nest/update-channel.md` § The channel as nest state → *What the nest
/// reports*): the updater reads it here, without a shell on the box.
pub fn identity_json(status: &str, update_channel: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "commit": commit(),
        "build_id": build_id(),
        "update_channel": update_channel,
    });
    if let Some(hashes) = binary_hashes() {
        body["binaries"] = serde_json::json!(hashes);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_hash_matches_the_one_shot_digest_across_chunk_boundaries() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        // Longer than one read buffer, and not a multiple of it.
        let bytes: Vec<u8> = (0..(3 << 20) + 7).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            hex::encode(Sha256::digest(&bytes))
        );
    }

    #[test]
    fn health_carries_the_running_binary_hash_once_hashed() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let hashes = loop {
            if let Some(h) = binary_hashes() {
                break h;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "binary hashes never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let own = sha256_file(&std::env::current_exe().unwrap()).unwrap();
        assert_eq!(hashes.get("fauna-nest"), Some(&own));
        assert_eq!(
            identity_json("ok", None)["binaries"]["fauna-nest"],
            own.as_str()
        );
    }

    #[test]
    fn health_reports_the_update_channel_or_null_when_pinned() {
        assert_eq!(identity_json("ok", Some("test"))["update_channel"], "test");
        let pinned = identity_json("needs_update", None);
        assert!(
            pinned
                .get("update_channel")
                .is_some_and(serde_json::Value::is_null),
            "a pinned or degraded box serves an explicit null: {pinned}"
        );
    }
}
