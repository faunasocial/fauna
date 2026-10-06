//! The one build-script derivation of `FAUNA_BUILD_COMMIT` — the commit a
//! Fauna binary was compiled from.
//!
//! **Owner:** `docs/goal/ui/status.md` § State & data shape, the build leg.
//! The nest serves the value on `/api/v1/health` (`fauna-nest`'s
//! `build_identity::commit`), and every app's Status page names it as
//! `status-build-sha` (`fauna_client_status::BuildLeg`). Both used to derive it
//! on their own — the nest in its `build.rs`, the web app in
//! `apps/fauna-web/build-id.js` — and the terminal app was about to be the
//! third copy; one derivation is what keeps "the build you are running" one
//! fact across the fleet. (The web app's JavaScript twin stays: a bundler-time
//! constant cannot call a Rust build script, so `build-id.js` mirrors the rule
//! below line for line.)
//!
//! **The rule, in order:** the environment when the build passes it (the nest
//! image's `FAUNA_BUILD_COMMIT` build argument, a release workflow's
//! `github.sha`), else the checkout's own `HEAD`, else nothing. `dev` and the
//! empty string read as *nothing* — the value the image's `ARG` defaults to,
//! so a plain `docker build` falls through to git exactly like a plain `cargo
//! build`. Nothing is stamped when neither source answers (a source tarball):
//! the consumer's `option_env!` is `None`, and a Status page then renders no
//! build row rather than a placeholder, the nest's `commit()` its `dev`.
//!
//! **Why a build script and not a runtime read:** the commit is a *source*
//! fact — it must not change with how the artifact is run — which is exactly
//! the argument `build_identity.rs` makes for stamping it at compile time
//! (against the build *id*, which is an artifact fact read at runtime).
//!
//! **Freshness:** the script registers the git files that move when `HEAD`
//! does — `HEAD` itself (a checkout) and the branch ref it points at (a
//! commit) — so a dev build re-stamps after a commit instead of naming the one
//! before. Only files that exist are registered: cargo re-runs a build script
//! on every build when a registered path is missing, and a packed ref has no
//! loose file. A stamp can therefore lag one commit on a checkout whose ref is
//! packed, which the Status witness tolerates ("some commit of this checkout").

use std::path::Path;
use std::process::Command;

/// The environment variable — read by the build, then re-exported to the
/// compiled crate under the same name (`option_env!("FAUNA_BUILD_COMMIT")`).
pub const ENV: &str = "FAUNA_BUILD_COMMIT";

/// The values the environment uses to mean "no commit was passed" — the image
/// `ARG`'s default and an unset variable's empty string.
pub const UNSTAMPED: [&str; 2] = ["", "dev"];

/// Emit the `cargo:` directives that stamp `FAUNA_BUILD_COMMIT` into the
/// calling crate, from a `build.rs`'s `main`. Prints nothing but directives.
pub fn emit() {
    println!("cargo:rerun-if-env-changed={ENV}");
    let stamped = std::env::var(ENV).ok().filter(|v| is_stamp(v));
    let sha = match stamped {
        Some(sha) => Some(sha),
        None => from_git(),
    };
    if let Some(sha) = sha {
        println!("cargo:rustc-env={ENV}={sha}");
    }
}

/// Whether an environment value carries a commit — anything but the two
/// [`UNSTAMPED`] sentinels. Pure, so the rule unit-tests without a build.
pub fn is_stamp(value: &str) -> bool {
    !UNSTAMPED.contains(&value.trim())
}

/// The checkout's `HEAD`, with the re-run registrations that keep it fresh.
/// `None` outside a git checkout, or when `git` is not on the path.
fn from_git() -> Option<String> {
    // Registered before the read so a checkout that answers today and not
    // tomorrow (a deleted `.git`) still re-runs — `git_path` returns `None`
    // rather than a missing path, so nothing bogus is registered.
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(reference) = git_path(&branch)
    {
        println!("cargo:rerun-if-changed={reference}");
    }
    git(&["rev-parse", "HEAD"])
}

/// `git rev-parse --git-path <item>`, only when the resolved file exists.
fn git_path(item: &str) -> Option<String> {
    git(&["rev-parse", "--git-path", item]).filter(|path| Path::new(path).is_file())
}

/// One `git` invocation's trimmed stdout, `None` on any failure.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_sentinels_are_not_stamps() {
        assert!(!is_stamp(""));
        assert!(!is_stamp("dev"));
        assert!(!is_stamp(" dev "));
    }

    #[test]
    fn a_commit_is_a_stamp_whatever_its_length() {
        assert!(is_stamp("4c1b9f5262"));
        assert!(is_stamp("4c1b9f52624c1b9f52624c1b9f52624c1b9f5262"));
    }

    /// This crate's own tests run inside the checkout, so the git arm must
    /// answer with `HEAD` — and never with a `--git-path` that does not exist.
    #[test]
    fn the_checkout_answers_with_a_full_commit() {
        let sha = from_git().expect("the test runs inside the fauna checkout");
        assert_eq!(sha.len(), 40, "{sha} is not a full commit");
        assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
        if let Some(head) = git_path("HEAD") {
            assert!(
                Path::new(&head).is_file(),
                "{head} was registered but is missing"
            );
        }
    }
}
