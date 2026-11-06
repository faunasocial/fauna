// The Apple cdylib's install name (LC_ID_DYLIB) is `@rpath/libfauna_ffi.dylib`,
// never a path. Without this rustc leaves the name to `ld`, which bakes in the
// OUTPUT path — under cargo's build-dir layout an absolute
// `<target>/<profile>/build/fauna-ffi/<hash>/out/libfauna_ffi.dylib` — and every
// binary linked against the dylib (the cgo bridges) then loads THAT file,
// ignoring the `-rpath` its link set. A compile cache restoring a sibling
// checkout's link carries the sibling's path, so a bridge in one checkout loaded
// another checkout's dylib and aborted while that checkout rebuilt it
// (build-compile-cache.md § Cargo target dir layout (mac), *A linked artifact
// names no checkout*). With
// `@rpath` the consumer resolves through its own rpath, and the artifact is the
// same bytes wherever it was linked. `rustc-cdylib-link-arg` reaches only the
// cdylib link, never the staticlib the xcframework slices consume.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libfauna_ffi.dylib");
    }
}
