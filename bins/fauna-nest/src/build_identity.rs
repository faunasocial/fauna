//! What build this process is — the two fields `/api/v1/health` serves so the
//! release pipeline can tell one artifact from another.
//!
//! Owner of the pair; both health handlers ([`crate::routes::health`] and
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

/// The identity body both health handlers serve, `status` aside — the field
/// set that makes a degraded box identifiable is one shape, not two hand-kept
/// copies.
pub fn identity_json(status: &str) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "commit": commit(),
        "build_id": build_id(),
    })
}
