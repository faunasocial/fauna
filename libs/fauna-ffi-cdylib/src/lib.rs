//! The linked half of the fauna-ffi rlib + thin-cdylib split
//! (`docs/goal/architecture/build-target-layout-windows.md` § UniFFI cdylib
//! rebuild cost (win) → *The rlib + thin-cdylib split*): every UniFFI export
//! is the core crate's, re-exported so this cdylib/staticlib carries its
//! scaffolding functions and metadata statics unchanged — but `status_text`.

pub use fauna_ffi_core::*;

// The one export that reads the build stamp, which is read only in the crate
// that IS the artifact (the same § → *The stamp*). The thin `value-format`
// is true exactly when the core's is (the feature mirror in Cargo.toml).
#[cfg(feature = "value-format")]
mod status_text;
