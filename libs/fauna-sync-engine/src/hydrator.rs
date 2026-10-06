//! The byte-source seam the Windows cfapi FETCH_DATA callback bridges to.
//!
//! On-demand file hydration (Cloud Files API) maps a placeholder path to its
//! full file bytes: the cfapi callback hands a relative path in and gets the
//! reassembled, decrypted file bytes out to feed `CfExecute(TRANSFER_DATA)`.
//! [`SyncEngine`](crate::engine::SyncEngine) is the production impl (it resolves
//! the manifest from the SyncDb, downloads + decrypts + reassembles); tests use
//! a fake. The trait is object-safe (`async_trait`) so the cfapi bridge can hold
//! an `Arc<dyn FileHydrator>` without naming the concrete engine type.
//!
//! ## Why `?Send` / no `Sync` bound
//!
//! `SyncEngine` is `Send` but **not `Sync`** — its `SyncDb` wraps a rusqlite
//! `Connection`, whose statement cache is a `RefCell`. So `&SyncEngine` is not
//! `Send`, the `&self` hydration future is not `Send`, and `SyncEngine: Sync`
//! does not hold. The seam therefore drops the `Sync` bound and uses
//! `#[async_trait(?Send)]`, matching the engine's existing single-task driving
//! model (every host drives one engine from exactly one loop). The cfapi bridge owns the engine and serializes hydration
//! calls onto its driving task rather than sharing `&engine` across threads.

use anyhow::Result;

/// The byte source a Windows cfapi FETCH_DATA callback bridges to: path in → full file bytes out.
/// `SyncEngine` is the production impl; tests use a fake. Object-safe (`async_trait(?Send)`) so the
/// cfapi bridge can hold `Arc<dyn FileHydrator>`. `Send` (not `Sync`) because `SyncEngine` is
/// `Send + !Sync` (rusqlite `Connection`), and the bridge serializes hydration onto one task.
#[async_trait::async_trait(?Send)]
pub trait FileHydrator: Send {
    async fn download_file_bytes(&self, relative_path: &str) -> Result<Vec<u8>>;
}
