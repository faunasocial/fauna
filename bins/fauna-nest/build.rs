// bins/fauna-nest/build.rs
//
// `FAUNA_BUILD_COMMIT` — the commit `src/build_identity.rs` serves on
// `/api/v1/health` — comes from the ONE shared derivation (the environment when
// the image build passes it, else the checkout's `HEAD`, else nothing), so the
// nest and the apps' Status pages can never disagree about what a build commit
// is (`docs/goal/ui/status.md` § State & data shape, the build leg).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    fauna_build_commit::emit();
}
