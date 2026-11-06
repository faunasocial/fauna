//! The one derivation of `FAUNA_BUILD_COMMIT` — the commit a Fauna artifact
//! was compiled from — as the procedural macro `commit!()`, an expression of
//! type `Option<&'static str>`.
//!
//! **Owner:** `docs/goal/ui/status.md` § State & data shape, the build leg.
//! The nest serves the value on `/api/v1/health` (`fauna-nest`'s
//! `build_identity::commit`), and every app's Status page names it as
//! `status-build-sha` (`fauna_client_status::BuildLeg`). One derivation is
//! what keeps "the build you are running" one fact across the fleet. (The web
//! app's JavaScript twin stays: a bundler-time constant cannot expand a Rust
//! macro, so `apps/fauna-web/build-id.js` mirrors the rule below line for
//! line.)
//!
//! **The rule, in order:** the environment when the build passes it (the nest
//! image's `FAUNA_BUILD_COMMIT` build argument, a release workflow's
//! `github.sha`), else the checkout's own `HEAD`, else nothing. `dev` and the
//! empty string read as *nothing* — the value the image's `ARG` defaults to,
//! so a plain `docker build` falls through to git exactly like a plain `cargo
//! build`. Nothing is stamped when neither source answers (a source tarball):
//! the macro yields `None`, and a Status page then renders no build row rather
//! than a placeholder, the nest's `commit()` its `dev`.
//!
//! **Where it is expanded — the rule `status.md` owns:** only in the crate
//! that IS the artifact (a binary's `main`, the thin `fauna-ffi` cdylib),
//! never in a library. The expansion carries the tracking that keeps the value
//! fresh, and cargo's dep-info is per TARGET, so the one target that expands
//! the macro is recompiled when `HEAD` moves while every other target of its
//! package stays `Fresh`. The `build.rs` this crate used to serve marked EVERY
//! target of its package stale whenever it re-ran: the nest library and the
//! FFI core recompiled at every commit, a docs-only one included, and missed
//! every compile cache between two checkouts. Probed on the Windows dev
//! machine 2026-10-26 before the move: in a lib + bin package whose bin alone
//! expanded a macro of this shape, the bin's dep-info named the git file and
//! the variable, the lib's only its own source, and changing either
//! recompiled the bin alone
//! (`docs/goal/architecture/build-target-layout-windows.md` § The rlib +
//! thin-cdylib split → *The stamp*).
//!
//! **Freshness:** the expansion `include_bytes!`s the git files that move when
//! `HEAD` does — `HEAD` itself (a checkout), the loose branch ref it points at
//! (a commit) and the checkout's own `HEAD` reflog, `logs/HEAD`, which every
//! commit, checkout, reset and rebase appends to — and names the variable
//! through `option_env!`, so rustc lists them among the expanding target's
//! inputs. Only files that exist are included (`include_bytes!` of a missing
//! file does not compile). The reflog is what keeps a PACKED ref fresh: a
//! packed ref has no loose file to track, `HEAD` itself does not change on a
//! commit, and the commit's new loose ref is a path nothing tracked — so with
//! `HEAD` and the ref alone the stamp stayed on the old commit until a source
//! change, which on 2026-10-27 a ref packed by git's own housekeeping on the
//! Windows dev machine showed is no corner case. Only a checkout with reflogs
//! turned off falls back to that lag, which the Status witness tolerates
//! ("some commit of this checkout").
//!
//! **Why compile time and not a runtime read:** the commit is a *source* fact
//! — it must not change with how the artifact is run — which is exactly the
//! argument `fauna-nest`'s `build_identity.rs` makes for it (against the build
//! *id*, which is an artifact fact read at runtime).

use proc_macro::TokenStream;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable the build passes, and the one the expansion names.
const ENV: &str = "FAUNA_BUILD_COMMIT";

/// The values the environment uses to mean "no commit was passed" — the image
/// `ARG`'s default and an unset variable's empty string.
const UNSTAMPED: [&str; 2] = ["", "dev"];

/// `fauna_build_commit::commit!()` — the commit this artifact is compiled
/// from, `Option<&'static str>`, `None` when neither the environment nor git
/// answers. Expand it only in the crate that IS the artifact (the crate doc).
#[proc_macro]
pub fn commit(input: TokenStream) -> TokenStream {
    let source = if input.is_empty() {
        let dir = std::env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        expansion(&derive(std::env::var(ENV).ok().as_deref(), &dir))
    } else {
        "::core::compile_error!(\"fauna_build_commit::commit!() takes no arguments\")".to_string()
    };
    source.parse().expect("the expansion is a Rust expression")
}

/// What the expansion stamps and which git files it tracks.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    commit: Option<String>,
    tracked: Vec<String>,
}

/// The rule: a stamped environment wins (and tracks no git file), else the
/// checkout `dir` sits in, else nothing.
fn derive(env: Option<&str>, dir: &Path) -> Stamp {
    if let Some(value) = env.filter(|v| is_stamp(v)) {
        return Stamp {
            commit: Some(value.trim().to_string()),
            tracked: Vec::new(),
        };
    }
    // Resolved before the read, like the registrations of the build script
    // this replaced: `git_path` answers only with a file that exists, so
    // nothing bogus is included.
    let mut tracked = Vec::new();
    if let Some(head) = git_path(dir, "HEAD") {
        tracked.push(head);
    }
    if let Some(branch) = git(dir, &["symbolic-ref", "-q", "HEAD"])
        && let Some(reference) = git_path(dir, &branch)
    {
        tracked.push(reference);
    }
    if let Some(reflog) = git_path(dir, "logs/HEAD") {
        tracked.push(reflog);
    }
    Stamp {
        commit: git(dir, &["rev-parse", "HEAD"]),
        tracked,
    }
}

/// The macro's output: the tracking items, then the value — a block
/// expression, so it reads as one expression wherever it is expanded.
fn expansion(stamp: &Stamp) -> String {
    let mut out = String::from("{");
    for path in &stamp.tracked {
        out += &format!("const _: &[u8] = ::core::include_bytes!({path:?});");
    }
    out += &format!("const _: ::core::option::Option<&str> = ::core::option_env!({ENV:?});");
    match &stamp.commit {
        Some(commit) => out += &format!("::core::option::Option::Some({commit:?})"),
        None => out += "::core::option::Option::None",
    }
    out + "}"
}

/// Whether an environment value carries a commit — anything but the two
/// [`UNSTAMPED`] sentinels.
fn is_stamp(value: &str) -> bool {
    !UNSTAMPED.contains(&value.trim())
}

/// `git rev-parse --git-path <item>` from `dir`, absolute, with forward
/// slashes (the expansion is Rust source, and rustc reads either separator on
/// Windows) — only when the resolved file exists.
fn git_path(dir: &Path, item: &str) -> Option<String> {
    let path = dir.join(git(dir, &["rev-parse", "--git-path", item])?);
    if !path.is_file() {
        return None;
    }
    let path = path.to_str()?.to_string();
    Some(if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path
    })
}

/// One `git -C <dir>` invocation's trimmed stdout, `None` on any failure.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
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

    fn this_crate() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

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

    /// A stamped environment wins, trimmed, and tracks no git file — the
    /// nest image's build argument decides, whatever the checkout says.
    #[test]
    fn a_stamped_environment_wins_and_tracks_no_git_file() {
        let stamp = derive(Some(" 4c1b9f5262\n"), this_crate());
        assert_eq!(
            stamp,
            Stamp {
                commit: Some("4c1b9f5262".into()),
                tracked: Vec::new()
            }
        );
    }

    /// This crate's own tests run inside the checkout, so a sentinel falls
    /// through to `HEAD` — and every tracked path is an existing, absolute
    /// file written with forward slashes.
    #[test]
    fn a_sentinel_falls_through_to_the_checkout_head() {
        let stamp = derive(Some("dev"), this_crate());
        let sha = stamp
            .commit
            .expect("the test runs inside the fauna checkout");
        assert_eq!(sha.len(), 40, "{sha} is not a full commit");
        assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(
            stamp.tracked.iter().any(|p| p.ends_with("/HEAD")),
            "HEAD is not tracked: {:?}",
            stamp.tracked
        );
        // The reflog is what keeps a packed ref fresh: whenever the checkout
        // has one, it is tracked.
        if let Some(reflog) = git_path(this_crate(), "logs/HEAD") {
            assert!(
                stamp.tracked.contains(&reflog),
                "{reflog} exists but is not tracked: {:?}",
                stamp.tracked
            );
        }
        for path in &stamp.tracked {
            assert!(Path::new(path).is_absolute(), "{path} is relative");
            assert!(Path::new(path).is_file(), "{path} is tracked but missing");
            assert!(!path.contains('\\'), "{path} keeps a backslash");
        }
    }

    #[test]
    fn the_expansion_tracks_each_file_and_the_variable_then_yields_the_value() {
        let stamp = Stamp {
            commit: Some("4c1b9f5262".into()),
            tracked: vec![
                "D:/repo/.git/HEAD".into(),
                "/repo/.git/refs/heads/main".into(),
            ],
        };
        assert_eq!(
            expansion(&stamp),
            "{\
             const _: &[u8] = ::core::include_bytes!(\"D:/repo/.git/HEAD\");\
             const _: &[u8] = ::core::include_bytes!(\"/repo/.git/refs/heads/main\");\
             const _: ::core::option::Option<&str> = ::core::option_env!(\"FAUNA_BUILD_COMMIT\");\
             ::core::option::Option::Some(\"4c1b9f5262\")\
             }"
        );
    }

    /// No source answered: the expansion still names the variable, so setting
    /// it later recompiles the artifact instead of leaving it unstamped.
    #[test]
    fn an_unstamped_expansion_is_none_and_still_names_the_variable() {
        let stamp = Stamp {
            commit: None,
            tracked: Vec::new(),
        };
        assert_eq!(
            expansion(&stamp),
            "{const _: ::core::option::Option<&str> = ::core::option_env!(\"FAUNA_BUILD_COMMIT\");\
             ::core::option::Option::None}"
        );
    }
}
