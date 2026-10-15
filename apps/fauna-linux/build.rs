//! Stamps `FAUNA_BUILD_COMMIT` for the Status page's `status-build-sha`
//! (`ui/status.md` § State & data shape, the build leg) — the shared
//! derivation, so the nest, the terminal app and the desktop app agree on what
//! "the build you are running" means.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    fauna_build_commit::emit();
}
