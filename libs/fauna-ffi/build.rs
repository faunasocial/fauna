//! Stamps `FAUNA_BUILD_COMMIT` — the commit the native app's shared Rust was
//! compiled from, which `status_text` folds into the Status page's
//! `status-build-sha` (`docs/goal/ui/status.md` § State & data shape, the build
//! leg). The shared derivation, so the nest, the terminal app and every
//! FFI-carried app agree on what "the build you are running" means.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    fauna_build_commit::emit();
}
