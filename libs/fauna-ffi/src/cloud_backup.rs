//! The shell's side of a store's **cloud-backup exclusion** — the FFI
//! projection of the shared three-arm [`CloudBackupExclusion`], shared by the
//! two stores that must state one: the client-device custodian store
//! (`custodian_host`) and, since 2026-08-26, the W3 (account-data-plane.md § Workstreams) account store
//! (`account_runtime` — `apps/common.md` § Credential storage → *The shared
//! Rust credential slots on the phones*).
//!
//! # Why there is no `NotApplicable` arm on the FFI enum
//!
//! The shared enum has three arms; this boundary exposes **two**.
//! `NotApplicable` is the desktop claim ("no OS-managed cloud backup reaches
//! this path"), and it is exactly the claim a mobile shell must never make:
//! iOS and android app storage *is* reached by iCloud / Google device backup,
//! and replicating a sealed corpus — or an account store whose writer key is
//! restore-excluded — into the same vendor cloud that holds the keychain is
//! the failure the enum exists to prevent, invisible on the device. Omitting
//! the arm makes that claim unrepresentable from a shell rather than merely
//! discouraged. A **desktop** shell hosting through this crate (macOS's
//! account runtime) never states an arm at all: it passes no sandboxed
//! container, and shared Rust states the desktop posture itself
//! ([`CloudBackupExclusion::platform_desktop`]).

use std::path::Path;
use std::sync::Arc;

use fauna_sync_engine::custodian_store::CloudBackupExclusion;

use crate::FfiError;

/// The shell's side of an imperative cloud-backup exclusion — apple's
/// `URL.setResourceValue(true, forKey: .isExcludedFromBackup)`, which needs the
/// directory to exist first and has no Rust binding.
///
/// Called with the store root after it is created and before anything is
/// written into it. Returning an error aborts the whole build / assembly: a
/// store whose exclusion failed must not be written into, because from that
/// point on it looks identical to a correctly excluded one.
#[uniffi::export(with_foreign)]
pub trait FfiCloudBackupExcluder: Send + Sync {
    fn exclude(&self, root: String) -> Result<(), FfiError>;
}

/// How this shell keeps a store out of the platform's cloud backup — the FFI
/// projection of [`CloudBackupExclusion`], minus its desktop arm (module docs).
#[derive(uniffi::Enum)]
pub enum FfiCloudBackupExclusion {
    /// **android** — excluded declaratively by the app's own manifest, which no
    /// runtime call can substitute for. `declaration` names the file + rule so
    /// a reviewer can check it exists rather than take the claim; android's is
    /// `AndroidManifest.xml android:allowBackup="false"`.
    DeclaredInManifest { declaration: String },
    /// **apple** — excluded imperatively by the shell once the root exists.
    ExcludedByShell {
        excluder: Arc<dyn FfiCloudBackupExcluder>,
    },
}

impl From<FfiCloudBackupExclusion> for CloudBackupExclusion {
    fn from(exclusion: FfiCloudBackupExclusion) -> Self {
        match exclusion {
            FfiCloudBackupExclusion::DeclaredInManifest { declaration } => {
                CloudBackupExclusion::DeclaredInManifest { declaration }
            }
            FfiCloudBackupExclusion::ExcludedByShell { excluder } => {
                CloudBackupExclusion::ExcludedByShell(Arc::new(move |p: &Path| {
                    excluder
                        .exclude(p.display().to_string())
                        .map_err(|e| anyhow::anyhow!("shell cloud-backup exclusion failed: {e}"))
                }))
            }
        }
    }
}

/// A **sandboxed shell's** account-store container, paired with how it stays
/// out of the platform's cloud backup — the two facts travel together because
/// they are one fact: a shell that supplies its own container (the per-app
/// container IS the per-user root there) is the only party that can say how
/// that container is kept out of the backup its keychain rows are kept out of.
/// Desktops pass `None` for the whole thing and shared Rust resolves both the
/// per-OS root and the desktop posture (`account_runtime.rs`).
#[derive(uniffi::Record)]
pub struct FfiStoreContainer {
    /// The container the account store roots under. Not per-actor —
    /// `StoreRoot::store_dir(actor_hex)` scopes underneath this itself.
    pub dir: String,
    /// How `dir` stays out of the platform's cloud backup.
    pub exclusion: FfiCloudBackupExclusion,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingExcluder(std::sync::Mutex<Vec<String>>);

    impl FfiCloudBackupExcluder for CountingExcluder {
        fn exclude(&self, root: String) -> Result<(), FfiError> {
            self.0.lock().unwrap().push(root);
            Ok(())
        }
    }

    struct RefusingExcluder;

    impl FfiCloudBackupExcluder for RefusingExcluder {
        fn exclude(&self, _root: String) -> Result<(), FfiError> {
            Err(FfiError::General {
                msg: "setResourceValue refused".into(),
            })
        }
    }

    /// The shell arm crosses the boundary intact: the foreign excluder is
    /// called with the exact path, and its refusal surfaces as the error the
    /// shared stores abort on.
    #[test]
    fn the_shell_arm_reaches_the_foreign_excluder_and_propagates_its_refusal() {
        let excluder = Arc::new(CountingExcluder(std::sync::Mutex::new(Vec::new())));
        let exclusion: CloudBackupExclusion = FfiCloudBackupExclusion::ExcludedByShell {
            excluder: Arc::clone(&excluder) as Arc<dyn FfiCloudBackupExcluder>,
        }
        .into();
        exclusion.apply(Path::new("/containers/app/store")).unwrap();
        assert_eq!(&*excluder.0.lock().unwrap(), &["/containers/app/store"]);

        let refusing: CloudBackupExclusion = FfiCloudBackupExclusion::ExcludedByShell {
            excluder: Arc::new(RefusingExcluder),
        }
        .into();
        let err = refusing
            .apply(Path::new("/containers/app/store"))
            .expect_err("a refused exclusion must not read as applied");
        assert!(
            err.to_string().contains("setResourceValue refused"),
            "the shell's own cause survives the crossing: {err:#}"
        );
    }

    /// The manifest arm carries its declaration verbatim and applies nothing
    /// — the manifest did the work.
    #[test]
    fn the_manifest_arm_is_declarative() {
        let exclusion: CloudBackupExclusion = FfiCloudBackupExclusion::DeclaredInManifest {
            declaration: "AndroidManifest.xml android:allowBackup=\"false\"".into(),
        }
        .into();
        assert!(matches!(
            &exclusion,
            CloudBackupExclusion::DeclaredInManifest { declaration }
                if declaration.contains("allowBackup")
        ));
        exclusion.apply(Path::new("/never/touched")).unwrap();
    }
}
