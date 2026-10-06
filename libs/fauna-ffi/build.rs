//! Stamps `FAUNA_BUILD_COMMIT` for `fauna_ffi_build_commit()` — the commit the
//! native app's shared Rust was compiled from, which the Status page names as
//! `status-build-sha` (`docs/goal/ui/status.md` § State & data shape, the build
//! leg). The shared derivation, so the nest, the terminal app and every
//! FFI-carried app agree on what "the build you are running" means.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    fauna_build_commit::emit();
}
