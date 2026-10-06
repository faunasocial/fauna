//! In-memory fake for the Media page seam, for `MediaMachine` lifecycle tests.
//!
//! [`FakeMediaNestApi`] fixtures the cross-set aggregate the read returns (or a
//! fixtured error) and records each call, so tests can drive the machine without
//! a transport and assert refresh behavior. Mirrors
//! `fauna_devices_machine::nest_api::fake`.

#![cfg(any(test, debug_assertions, feature = "test-helpers"))]

use std::sync::Mutex;

use fauna_client_media::{MediaFolder, MediaSnapshot};
use fauna_client_share::share::{ShareCreateReply, ShareCreateRequest, ShareRecord};

use super::{ListedVersion, MediaApiError, MediaNestApi};
use crate::snapshots::FileVersionSummary;

/// The identity [`FakeMediaNestApi::set_predecessor_signed`] reports a marked
/// row signed as — pair it with a retired key
/// ([`crate::MediaMachine::set_predecessor_chain`]) for the row to open.
pub const FAKE_PREDECESSOR_ID: [u8; 32] = [0xA5; 32];

/// The identity the fake reports every unmarked row signed as — the seat's
/// current one.
pub const FAKE_CURRENT_ID: [u8; 32] = [0xC0; 32];

/// One recorded call (read or write gesture), with its arguments where a test
/// asserts on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaNestCall {
    MediaSnapshot,
    /// `list_folders()` — the control-plane option list read.
    ListFolders,
    /// `record_member(folder, device_id, path, manifest_hash, size_bytes,
    /// content_key_version, thumbnail_hash, path_sealed)`.
    RecordMember {
        folder: String,
        device_id: String,
        path: String,
        manifest_hash: String,
        size_bytes: i64,
        /// The M2 generation a content-keyed set's upload was sealed under;
        /// `None` for an owner-only set.
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        /// The sealed label the upload gesture computed — recorded so a test
        /// can assert the Media write plane seals its path
        /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
        path_sealed: Option<Vec<u8>>,
    },
    /// `delete_member(folder, device_id, path, path_sealed)`.
    DeleteMember {
        folder: String,
        device_id: String,
        path: String,
        /// The machine-minted tombstone seal (S8 D2) — recorded so a test can
        /// assert the delete gesture seals (and that a keyless machine passes
        /// `None`, the best-effort degrade).
        path_sealed: Option<Vec<u8>>,
    },
    /// `file_versions(folder, path)`.
    FileVersions {
        folder: String,
        path: String,
        include_pruned: bool,
    },
    /// `undelete_version(path, version_num)` — the recovery browse's restore
    /// verb (`file-versions.md` § Retention (3)).
    UndeleteVersion {
        path: String,
        version_num: i64,
    },
    /// `restore_member(folder, device_id, path, manifest_hash, size_bytes,
    /// content_key_version, path_sealed)`.
    RestoreMember {
        folder: String,
        device_id: String,
        path: String,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        /// Same contract as [`Self::DeleteMember::path_sealed`].
        path_sealed: Option<Vec<u8>>,
    },
    /// `share_register(request)` — the minted token, as sent.
    ShareRegister {
        token: String,
    },
    /// `share_list()`.
    ShareList,
    /// `share_revoke(token_id)`.
    ShareRevoke {
        token_id: String,
    },
}

#[derive(Debug, Default)]
pub struct FakeMediaNestApi {
    /// The aggregate the next `media_snapshot()` returns when no error is set.
    snapshot: Mutex<MediaSnapshot>,
    /// The control-plane sets the next `list_folders()` returns. Empty by
    /// default — a test that says nothing about folders gets the pre-fix
    /// item-derived option behavior.
    folders: Mutex<Vec<MediaFolder>>,
    /// `Some` makes `media_snapshot()` fail with this error.
    error: Mutex<Option<MediaApiError>>,
    /// `Some` makes the write gestures (`record_member` / `delete_member` /
    /// `restore_member`) fail with this error — independent of the read error.
    write_error: Mutex<Option<MediaApiError>>,
    /// The versions the next `file_versions()` returns (the read error fixture
    /// applies to it too).
    versions: Mutex<Vec<FileVersionSummary>>,
    /// Item paths and version manifest hashes the fake's "judge" reports as
    /// NOT signed under the seat's current identity, each with the identity it
    /// reports them signed as (a predecessor's rows, or another writer's).
    /// Everything else is reported signed-as-current: the fake stands for a
    /// seam whose judge admitted every fixture row as the account's own.
    predecessor_signed: Mutex<std::collections::HashMap<String, [u8; 32]>>,
    /// What `set_reader_predecessors` was last handed.
    reader_predecessors: Mutex<Vec<[u8; 32]>>,
    /// The share registry the `share_*` calls read and write — a nest in
    /// miniature: a register appends the record the nest would derive.
    shares: Mutex<Vec<ShareRecord>>,
    /// Each registration's sealed key envelope, in register order — what a
    /// private link's holder opens with the key from the URL fragment (the
    /// nest stores it opaque, so the registry row above does not carry it).
    share_envelopes: Mutex<Vec<Option<Vec<u8>>>>,
    /// `Some` makes the `share_*` calls fail with this error.
    share_error: Mutex<Option<MediaApiError>>,
    calls: Mutex<Vec<MediaNestCall>>,
}

impl FakeMediaNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fixture the cross-set aggregate the read returns.
    pub fn set_snapshot(&self, snapshot: MediaSnapshot) {
        *self.snapshot.lock().unwrap() = snapshot;
    }

    /// Fixture the control-plane folder list the read returns — the sets that
    /// exist whether or not they hold media.
    pub fn set_folders(&self, folders: Vec<MediaFolder>) {
        *self.folders.lock().unwrap() = folders;
    }

    /// Make `media_snapshot()` fail with `err` (refresh-error path).
    pub fn fail(&self, err: MediaApiError) {
        *self.error.lock().unwrap() = Some(err);
    }

    /// Clear a previously-set failure (so the next read succeeds).
    pub fn clear_failure(&self) {
        *self.error.lock().unwrap() = None;
    }

    /// Make the `share_*` calls fail with `err` (`None` clears it).
    pub fn fail_shares(&self, err: Option<MediaApiError>) {
        *self.share_error.lock().unwrap() = err;
    }

    /// The fake registry's rows, newest first (as the nest lists them).
    pub fn shares(&self) -> Vec<ShareRecord> {
        self.shares.lock().unwrap().clone()
    }

    /// Each registration's sealed key envelope (`None` for a public link), in
    /// register order.
    pub fn share_envelopes(&self) -> Vec<Option<Vec<u8>>> {
        self.share_envelopes.lock().unwrap().clone()
    }

    /// Make the write gestures (`record_member` / `delete_member`) fail with
    /// `err` (the record/delete-error path).
    pub fn fail_writes(&self, err: MediaApiError) {
        *self.write_error.lock().unwrap() = Some(err);
    }

    /// Fixture the version history the next `file_versions()` returns.
    pub fn set_versions(&self, versions: Vec<FileVersionSummary>) {
        *self.versions.lock().unwrap() = versions;
    }

    /// Mark fixture rows as signed under a RETIRED identity
    /// ([`FAKE_PREDECESSOR_ID`]): each entry is a Media item's `path`, or the
    /// hex `manifest_hash` of an item or version.
    pub fn set_predecessor_signed(&self, rows: Vec<String>) {
        self.set_signed_as(rows, FAKE_PREDECESSOR_ID);
    }

    /// Mark fixture rows as signed as `signer` — a retired identity of the
    /// account, or another writer. Replaces every earlier mark.
    pub fn set_signed_as(&self, rows: Vec<String>, signer: [u8; 32]) {
        *self.predecessor_signed.lock().unwrap() =
            rows.into_iter().map(|row| (row, signer)).collect();
    }

    /// [`Self::set_signed_as`] without replacing the earlier marks — for a
    /// fixture whose rows several identities signed.
    pub fn add_signed_as(&self, rows: Vec<String>, signer: [u8; 32]) {
        self.predecessor_signed
            .lock()
            .unwrap()
            .extend(rows.into_iter().map(|row| (row, signer)));
    }

    /// The predecessor ids the machine last handed the seam's reader.
    pub fn reader_predecessors(&self) -> Vec<[u8; 32]> {
        self.reader_predecessors.lock().unwrap().clone()
    }

    /// All recorded read calls, in order.
    pub fn calls(&self) -> Vec<MediaNestCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MediaNestApi for FakeMediaNestApi {
    async fn media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::MediaSnapshot);
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => {
                let mut snapshot = self.snapshot.lock().unwrap().clone();
                let predecessor_signed = self.predecessor_signed.lock().unwrap();
                for item in &mut snapshot.items {
                    let signer = item
                        .manifest_hash
                        .as_ref()
                        .and_then(|m| predecessor_signed.get(&hex::encode(&m[..])))
                        .or_else(|| predecessor_signed.get(&item.path))
                        .copied();
                    item.signed_as_current = signer.is_none();
                    item.signed_as = Some(signer.unwrap_or(FAKE_CURRENT_ID));
                }
                Ok(snapshot)
            }
        }
    }

    fn set_reader_predecessors(&self, predecessors: Vec<[u8; 32]>) {
        *self.reader_predecessors.lock().unwrap() = predecessors;
    }

    async fn list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError> {
        self.calls.lock().unwrap().push(MediaNestCall::ListFolders);
        // Shares the read-error fixture with `media_snapshot` — both are the
        // page's load path, and a test fixturing "the read fails" means both.
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(self.folders.lock().unwrap().clone()),
        }
    }

    async fn record_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::RecordMember {
                folder: folder.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                manifest_hash,
                size_bytes,
                content_key_version,
                thumbnail_hash,
                path_sealed,
            });
        match self.write_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn delete_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::DeleteMember {
                folder: folder.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                path_sealed,
            });
        match self.write_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn file_versions(
        &self,
        folder: &str,
        path: &str,
        include_pruned: bool,
    ) -> Result<Vec<ListedVersion>, MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::FileVersions {
                folder: folder.to_string(),
                path: path.to_string(),
                include_pruned,
            });
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            // The nest's own projection rule: pruned rows ride only an
            // include_pruned listing.
            None => {
                let predecessor_signed = self.predecessor_signed.lock().unwrap();
                Ok(self
                    .versions
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|v| include_pruned || !v.pruned)
                    .cloned()
                    .map(|summary| {
                        let signer = predecessor_signed.get(&summary.manifest_hash).copied();
                        ListedVersion {
                            signed_as_current: signer.is_none(),
                            signed_as: Some(signer.unwrap_or(FAKE_CURRENT_ID)),
                            summary,
                        }
                    })
                    .collect())
            }
        }
    }

    async fn share_register(
        &self,
        request: ShareCreateRequest,
    ) -> Result<ShareCreateReply, MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::ShareRegister {
                token: request.token.clone(),
            });
        if let Some(e) = self.share_error.lock().unwrap().clone() {
            return Err(e);
        }
        let token = fauna_core::share::ShareToken::from_base64url(&request.token).map_err(|e| {
            MediaApiError::BadRequest {
                detail: e.to_string(),
            }
        })?;
        let id = fauna_core::share::token_id_from_base64url(&request.token).map_err(|e| {
            MediaApiError::BadRequest {
                detail: e.to_string(),
            }
        })?;
        let record = ShareRecord {
            token_id: hex::encode(id),
            manifest_hash: hex::encode(token.manifest_hash),
            expires_at: i64::try_from(token.expires).unwrap_or(i64::MAX),
            public: token.public,
            key_in_fragment: token.key_in_fragment,
            filename_sealed: request.filename_sealed,
            ..Default::default()
        };
        self.share_envelopes
            .lock()
            .unwrap()
            .push(request.key_envelope.map(|b| b.into_vec()));
        self.shares.lock().unwrap().insert(0, record.clone());
        Ok(ShareCreateReply {
            share: record,
            extra: Default::default(),
        })
    }

    async fn share_list(&self) -> Result<Vec<ShareRecord>, MediaApiError> {
        self.calls.lock().unwrap().push(MediaNestCall::ShareList);
        if let Some(e) = self.share_error.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.shares())
    }

    async fn share_revoke(&self, token_id: &str) -> Result<(), MediaApiError> {
        self.calls.lock().unwrap().push(MediaNestCall::ShareRevoke {
            token_id: token_id.to_string(),
        });
        if let Some(e) = self.share_error.lock().unwrap().clone() {
            return Err(e);
        }
        let mut shares = self.shares.lock().unwrap();
        match shares.iter_mut().find(|r| r.token_id == token_id) {
            Some(r) => {
                r.revoked = true;
                Ok(())
            }
            None => Err(MediaApiError::NotFound {
                detail: token_id.to_string(),
            }),
        }
    }

    async fn undelete_version(&self, path: &str, version_num: i64) -> Result<(), MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::UndeleteVersion {
                path: path.to_string(),
                version_num,
            });
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => {
                // Mirror the nest: the row returns to the live population.
                for v in self.versions.lock().unwrap().iter_mut() {
                    if v.version_num == version_num {
                        v.pruned = false;
                        v.purge_after = None;
                    }
                }
                Ok(())
            }
        }
    }

    async fn restore_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(MediaNestCall::RestoreMember {
                folder: folder.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            });
        match self.write_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
