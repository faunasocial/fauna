//! `status_text` — the shared Status snapshot's text projection over UniFFI
//! (`docs/goal/ui/status.md` § State & data shape), the one FFI function that
//! reads the build stamp.
//!
//! It lives in this thin artifact crate, not beside its records in the core's
//! `status.rs`, because the stamp is read only in the crate that IS the
//! artifact (the same §, the build leg): `fauna_build_commit::commit!()` makes
//! the target that expands it track the checkout's `HEAD`, and here that
//! target is the one that links the dll anyway, so a commit recompiles this
//! small crate instead of the whole core
//! (`docs/goal/architecture/build-target-layout-windows.md` § The rlib +
//! thin-cdylib split → *The stamp*). The export joins the core's `fauna_ffi`
//! namespace — UniFFI names a namespace after the crate, and both crates are
//! lib `fauna_ffi` — so every app calls it exactly as before.
//!
//! The build leg is the commit the native app's shared Rust was compiled
//! from, which is what a native app's "build" means — the shared Rust it
//! links. The tests live here with the function (`cargo test -p fauna-ffi`).

use std::sync::Arc;

use fauna_client_status::{BuildLeg, MlsLeg, NodeLeg, StatusSnapshot, SyncLeg, render};
use fauna_ffi_core::{FfiStatusLegs, FfiStatusLookup, FfiStatusText};

/// The commit this build's shared Rust was compiled from, when stamped.
fn build_leg() -> BuildLeg {
    BuildLeg::from_stamp(fauna_build_commit::commit!())
}

fn text_for(
    legs: &FfiStatusLegs,
    build: BuildLeg,
    now_ms: i64,
    lookup: &dyn FfiStatusLookup,
) -> FfiStatusText {
    let snapshot = StatusSnapshot {
        node: legs.node.as_ref().map(|n| NodeLeg {
            domain: n.domain.clone(),
            version: n.version.clone(),
        }),
        sync: legs.sync.map(|s| SyncLeg {
            files_pending: s.files_pending,
            bytes_pending: s.bytes_pending,
            last_sync: s.last_sync,
        }),
        mls: legs.mls.map(|m| MlsLeg {
            key_packages: m.key_packages,
            channels: m.channels,
        }),
        build,
    };
    let text = render(&snapshot, now_ms, |key| lookup.lookup(key.to_owned()));
    FfiStatusText {
        node_domain: text.node_domain,
        node_version: text.node_version,
        sync_pending: text.sync_pending,
        sync_last: text.sync_last,
        mls_key_packages: text.mls_key_packages,
        mls_channels: text.mls_channels,
        build_sha: text.build_sha,
    }
}

/// `fauna_client_status::render` — the seven `status-*` element texts for the
/// legs loaded so far, resolved through the app's own catalog. `now_ms` is the
/// app's clock (the `status-sync-last` relative time).
#[uniffi::export]
pub fn status_text(
    legs: FfiStatusLegs,
    now_ms: i64,
    lookup: Arc<dyn FfiStatusLookup>,
) -> FfiStatusText {
    text_for(&legs, build_leg(), now_ms, lookup.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ffi_core::{FfiMlsLeg, FfiNodeLeg, FfiStatusSyncLeg};

    struct Catalog;
    impl FfiStatusLookup for Catalog {
        fn lookup(&self, key: String) -> Option<String> {
            match key.as_str() {
                "status.sync.pending_summary" => Some("{files} files, {bytes}".into()),
                "common.never" => Some("Never".into()),
                _ => None,
            }
        }
    }

    #[test]
    fn unloaded_legs_render_no_element() {
        let text = text_for(&FfiStatusLegs::default(), BuildLeg::default(), 0, &Catalog);
        assert_eq!(text, FfiStatusText::default());
    }

    #[test]
    fn loaded_legs_project_to_the_bare_values() {
        let legs = FfiStatusLegs {
            node: Some(FfiNodeLeg {
                domain: "nest.example".into(),
                version: "1.2.3".into(),
            }),
            sync: Some(FfiStatusSyncLeg {
                files_pending: 3,
                bytes_pending: 0,
                last_sync: None,
            }),
            mls: Some(FfiMlsLeg {
                key_packages: 5,
                channels: 2,
            }),
        };
        let build = BuildLeg::from_stamp(Some("0123456789abcdef0123"));
        let text = text_for(&legs, build, 1_000, &Catalog);
        assert_eq!(text.node_domain.as_deref(), Some("nest.example"));
        assert_eq!(text.node_version.as_deref(), Some("1.2.3"));
        assert!(text.sync_pending.unwrap().starts_with("3 files"));
        assert_eq!(text.sync_last.as_deref(), Some("Never"));
        assert_eq!(text.mls_key_packages.as_deref(), Some("5"));
        assert_eq!(text.mls_channels.as_deref(), Some("2"));
        assert_eq!(text.build_sha.as_deref(), Some("0123456789ab"));
    }

    /// This crate's tests run inside the checkout, so the artifact's own
    /// stamp is a commit — the build leg is never silently empty here.
    #[test]
    fn the_artifact_carries_its_own_build_commit() {
        assert!(build_leg().commit().is_some_and(|c| c.len() >= 7));
    }
}
