//! Nest-backed per-file version history for the Explorer shell extension.
//!
//! The `ListFileVersions` pipe verb answers from the nest's
//! `fauna.files.versions.list` projection over the append-only `sync_changes`
//! table (`docs/goal/behavior/file-sync.md` § File Versions) — *not* from the
//! local `SyncDb` entry counter the older `GetFileVersions` verb reads.
//!
//! Everything here is generic over [`RpcRequester`] so the request shape (the
//! derived `path_hash`, the `folder` scope) and the reply mapping are unit-tested
//! against an in-memory fake — no nest, no network, no live Explorer.
//!
//! Two rules from the goal doc are load-bearing and enforced by the tests below:
//!
//! * **`path_hash` is shared Rust.** `fauna_core::sync::path_hash` over the
//!   forward-slash, folder-relative path. Never an inline `blake3::hash`
//!   (§ Path hashing).
//! * **`folder` is always sent.** A bare `path_hash` is ambiguous across sets —
//!   the hash is set-relative, so two sets can share one (§ Wire surface).

use fauna_client_sync::SyncClient;
use fauna_client_sync::restore_branch::{RestoreDecision, restore_decision};
use fauna_client_sync::row_judge::{ReaderSeat, retain_judged};
use fauna_ipc::sync::{FileVersionEntry, FileVersionListInfo};
use fauna_protocol::RpcRequester;
use fauna_protocol::files::FilesVersionsListReply;

/// The version list for a path that resolves to no served on-demand folder.
///
/// Not an error: Explorer right-clicks any file, including ones outside every sync
/// root. An empty history renders as a disabled/absent submenu rather than an error
/// dialog.
pub fn empty_version_list(path: &str) -> FileVersionListInfo {
    FileVersionListInfo {
        path: path.to_string(),
        folder: String::new(),
        versions: Vec::new(),
    }
}

/// Map the nest reply onto the IPC shape. Pure — the order the nest returned
/// (oldest→newest) is preserved verbatim, and `version_num` is carried through as
/// the `sync_changes` `seq` it is, never re-numbered into a dense ordinal.
pub fn to_version_list(
    path: &str,
    folder: &str,
    reply: &FilesVersionsListReply,
) -> FileVersionListInfo {
    FileVersionListInfo {
        path: path.to_string(),
        folder: folder.to_string(),
        versions: reply
            .versions
            .iter()
            .map(|v| FileVersionEntry {
                version_num: v.version_num,
                size_bytes: v.size_bytes,
                created_at: v.created_at,
            })
            .collect(),
    }
}

/// List `rel`'s versions in `folder` over the shared typed surface — only the
/// versions the one shared judge admits (writer-signed change records, ruling
/// (3): [`SyncClient::versions_list_judged`] under `seat`, the agent's own
/// actor id and custody-resolved set nonces). A version that does not verify
/// is absent from the menu, so it is never offered for restore.
///
/// `rel` must already be the forward-slash, folder-relative path
/// (`path_map::resolve_to_folder_rel` guarantees this — it normalizes `\` → `/`).
pub async fn list_file_versions<R>(
    sync: &SyncClient<R>,
    path: &str,
    folder: &str,
    rel: &str,
    seat: &ReaderSeat,
) -> Result<FileVersionListInfo, String>
where
    R: RpcRequester,
    R::Error: core::fmt::Display + fauna_protocol::RpcErrorClass,
{
    let hash = fauna_core::sync::path_hash(rel);
    let judged = sync
        .versions_list_judged(hash, folder, false, seat)
        .await
        .map_err(|e| format!("list file versions: {e}"))?;
    let (versions, _) = retain_judged(judged, "fauna.files.versions.list");
    Ok(to_version_list(
        path,
        folder,
        &FilesVersionsListReply {
            versions,
            ..Default::default()
        },
    ))
}

// ── Restore ─────────────────────────────────────────────────────────────────

/// The historical version a restore re-pointed the file at — everything the
/// caller needs to re-point its **own local copy** (§ Restore, *the recording
/// device must re-point its own local copy*).
///
/// The nest half is done by the time this exists; the local apply is the caller's
/// second, explicit step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredVersion {
    /// The historical version's manifest — the hydration anchor the local row
    /// must now carry, so the next `FETCH_DATA` serves the restored bytes.
    pub manifest_hash: [u8; 32],
    pub size_bytes: i64,
    /// The M2 generation the historical chunks were sealed under, carried
    /// verbatim so the reader selects `key_for(version)` (§ Restore).
    pub content_key_version: Option<u64>,
    /// `seq` of the restore's own recording row (the restore is itself a new
    /// version — history is append-only, so restore is reversible).
    pub recorded_seq: i64,
}

/// Why this verb refuses a version that was recorded under a previous identity
/// of the account where it cannot open it: one whose folder no engine is
/// running for ([`InheritedReseal`]).
pub const RESTORE_INHERITED_REFUSED: &str = "it was recorded under a previous identity of \
     this account, so it must be opened and re-sealed to be restored — restore it from the \
     Media page";

/// The byte seam of a restore: open a version another identity of this account
/// signed and re-seal it under the current owner root
/// (`writer-signed-change-records.md` ruling (8)(d), the restore sentence).
/// Answers the head the restore records in the historical manifest's place, or
/// the reason the version is not restorable — in which case nothing is
/// recorded.
///
/// The agent's is the set's own running engine
/// (`pipe_server::EngineReseal`, over
/// [`SyncEngine::reseal_inherited_version`](fauna_sync_engine::engine::SyncEngine::reseal_inherited_version)),
/// so the roots a signature may reach are decided in the one place every
/// other open of the set's bytes decides them.
pub trait InheritedReseal {
    /// `rel` is the forward-slash, folder-relative path; `signed_as` the
    /// identity the judge verified the version's row as signed as.
    async fn reseal(
        &self,
        rel: &str,
        manifest_hash: [u8; 32],
        signed_as: Option<[u8; 32]>,
    ) -> Result<fauna_sync_engine::engine::ResealedVersion, String>;
}

/// Restore `rel` to the version identified by `version_num` (a `sync_changes`
/// `seq`), returning what the local copy must be re-pointed at.
///
/// Two RPCs, strictly ordered: the JUDGED version listing
/// ([`SyncClient::versions_list_judged`], soft-pruned rows included) for the
/// historical metadata and its verdict, then `fauna.sync.changes.record` (via the shared
/// [`SyncClient::restore_version`], which *is* the ratified record — an ordinary
/// `modify` carrying the historical `manifest_hash`/`size_bytes`/
/// `content_key_version` verbatim, self-healing an unregistered device).
///
/// A failed lookup records **nothing**: recording a `modify` without the
/// historical manifest would re-point the file at whatever we guessed. Nor
/// does a version the shared judge refuses or holds (ruling (3): a row that
/// does not verify is not a record, so it is not a version to restore to).
/// The verdict comes from the listing because only the list reply carries the
/// `signer_certs` a delegated signer's row chains through — a bare
/// `fauna.files.versions.get` would refuse every delegated writer's version.
///
/// **A version recorded under a previous identity whose bytes rest under an
/// owner root is never re-signed unopened** (`writer-signed-change-records.md`
/// § Writer-signed change records, ruling (8)(d)): the restore re-signs under
/// the current identity, and a head so signed is offered the current owner
/// root — so such a version is restored only by opening its bytes under a root
/// its own signature could reach and re-sealing them. That is `reseal`'s act
/// ([`InheritedReseal`]): the record then carries the **new** manifest it
/// answers, and a version it does not open records nothing and surfaces its
/// reason. A stamped version opens under its content-key generation whoever
/// re-signs it, never under an owner root, and re-points verbatim. A history
/// version (ruling (11)(c): a predecessor's row under a retired nonce of the
/// set) takes the same branch — the opening restore of ruling (11)(f): the
/// user's own vouch for one named version, opened under its signer's roots,
/// re-sealed where it rests under an owner root, and recorded under the live
/// nonce as the current identity.
///
/// `rel` must already be the forward-slash, folder-relative path — it is both
/// the `path_hash` preimage and the `path` the record carries, so the nest's own
/// `path_hash(path)` agrees with the one we looked the version up by.
///
/// `path_sealed` is minted by the caller from the agent's key material
/// (`pipe_server::handle_restore_file_version` — the custody-resolved per-set
/// engine keys through `FileDownloadKeys::label_seal_root`, S8 D2) and carried
/// verbatim; `None` records the re-point plaintext-only, best-effort — an S8
/// backfill row, never an error.
#[allow(clippy::too_many_arguments)]
pub async fn restore_file_version<R>(
    sync: &SyncClient<R>,
    folder: &str,
    device_id: &str,
    rel: &str,
    version_num: i64,
    path_sealed: Option<Vec<u8>>,
    seat: &ReaderSeat,
    reseal: &impl InheritedReseal,
) -> Result<RestoredVersion, String>
where
    R: RpcRequester,
    // `RpcErrorClass` is what lets the shared record self-heal an unregistered
    // device: it distinguishes a `device_unregistered` rejection (retry once,
    // after registering) from a transport fault (surface it).
    R::Error: core::fmt::Display + fauna_protocol::RpcErrorClass,
{
    let hash = fauna_core::sync::path_hash(rel);
    let listing = sync
        .versions_list_judged(hash, folder, true, seat)
        .await
        .map_err(|e| format!("get file version {version_num}: {e}"))?;
    // Ruling (10)(b): the current identity vouches for a manifest through any
    // admitted row of the set naming it, not only the picked version's.
    let (info, verdict) = listing
        .iter()
        .find(|(v, _)| v.version_num == version_num)
        .cloned()
        .ok_or_else(|| format!("get file version {version_num}: not found"))?;
    let current_vouches = listing.iter().any(|(v, verdict)| {
        v.manifest_hash == info.manifest_hash && verdict.admits() && v.signed_as_current
    });
    // The one restore decision every door shares
    // (`fauna_client_sync::restore_branch`, ruling (10)(b)).
    let decision = restore_decision(&verdict, info.content_key_version, current_vouches);
    if decision == RestoreDecision::Refuse {
        tracing::warn!(version_num, ?verdict, "restore: the version did not verify");
        return Err(format!(
            "version {version_num} did not verify — it cannot be restored"
        ));
    }
    // The projection only lists rows with `manifest_hash IS NOT NULL` (a delete
    // records a tombstone, not a restorable version), so this is a
    // defence-in-depth check against a malformed reply — never a normal path.
    let historical: [u8; 32] = info
        .manifest_hash
        .as_ref()
        .try_into()
        .map_err(|_| format!("version {version_num} has no restorable content"))?;

    // Not signed as the current identity and resting under an owner root: the
    // head this records is re-signed, so its bytes are opened and re-sealed
    // first and the record carries the NEW manifest. A version the seam does
    // not open records nothing.
    let (manifest_hash, size_bytes) = if decision == RestoreDecision::NeedsReseal {
        let resealed = reseal
            .reseal(rel, historical, info.signed_as)
            .await
            .map_err(|e| {
                tracing::warn!(
                    version_num,
                    error = %e,
                    "restore: a version another identity signed was not re-sealed — \
                     refusing to re-sign it unopened"
                );
                format!("version {version_num}: {e}")
            })?;
        (resealed.manifest_hash.digest(), resealed.size_bytes)
    } else {
        (historical, info.size_bytes)
    };

    let reply = sync
        .restore_version(
            folder,
            device_id,
            rel,
            hex::encode(manifest_hash),
            size_bytes,
            info.content_key_version,
            path_sealed,
        )
        .await
        .map_err(|e| format!("restore version {version_num}: {e}"))?;

    Ok(RestoredVersion {
        manifest_hash,
        size_bytes,
        content_key_version: info.content_key_version,
        recorded_seq: reply.seq,
    })
}

/// A byte seam for a restore that must never reach one — a version that
/// re-points verbatim, or is refused before any byte is asked for.
#[cfg(test)]
pub(crate) struct NeverReseals;

#[cfg(test)]
impl InheritedReseal for NeverReseals {
    async fn reseal(
        &self,
        _rel: &str,
        _manifest_hash: [u8; 32],
        _signed_as: Option<[u8; 32]>,
    ) -> Result<fauna_sync_engine::engine::ResealedVersion, String> {
        panic!("this restore must not reach the byte seam")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::files::{FileVersionInfo, FilesVersionsListRequest};
    use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
    use std::sync::{Arc, Mutex};

    /// An in-memory `RpcRequester` that speaks the **real** canonical dag-cbor
    /// codec, so the request/reply shapes a test asserts are the shapes that go on
    /// the wire (a JSON stand-in would silently paper over `ByteBuf`-under-`flatten`
    /// encoding differences).
    ///
    /// Consumed as `Arc<FakeNest>` — `RpcRequester` has a blanket impl for `Arc<T>`
    /// precisely so a call site can retain a handle to assert on afterwards.
    struct FakeNest {
        calls: Mutex<Vec<(String, Vec<u8>)>>,
        reply: Result<FilesVersionsListReply, FakeErr>,
    }

    #[derive(Debug, Clone)]
    struct FakeErr(String);

    impl core::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    /// Every fake error is a *transport fault*, never a server rejection — so the
    /// shared record's `device_unregistered` self-heal (register + retry once)
    /// never fires here. That path has its own tests in `fauna-client-sync`.
    impl fauna_protocol::RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    impl FakeNest {
        fn err(msg: &str) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                reply: Err(FakeErr(msg.to_string())),
            })
        }
    }

    impl RpcRequester for FakeNest {
        type Error = FakeErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_cbor::encode_canonical(&payload).unwrap();
            self.calls.lock().unwrap().push((kind.to_string(), bytes));
            let reply = self.reply.clone()?;
            let encoded = fauna_cbor::encode_canonical(&reply).unwrap();
            Ok(fauna_cbor::decode_strict(&encoded).unwrap())
        }
    }

    fn version(seq: i64, size: i64, created_at: i64) -> FileVersionInfo {
        FileVersionInfo {
            version_num: seq,
            size_bytes: size,
            created_at,
            ..Default::default()
        }
    }

    #[test]
    fn to_version_list_preserves_sparse_seq_and_order() {
        // `version_num` is the sync_changes `seq` — sparse (a superseded middle row
        // leaves a gap) and never renumbered into 1..N.
        let reply = FilesVersionsListReply {
            versions: vec![version(4, 100, 1_000), version(9, 250, 2_000)],
            ..Default::default()
        };
        let info = to_version_list(r"C:\Sync\docs\report.txt", "docs", &reply);

        assert_eq!(info.path, r"C:\Sync\docs\report.txt");
        assert_eq!(info.folder, "docs");
        assert_eq!(
            info.versions,
            vec![
                FileVersionEntry {
                    version_num: 4,
                    size_bytes: 100,
                    created_at: 1_000
                },
                FileVersionEntry {
                    version_num: 9,
                    size_bytes: 250,
                    created_at: 2_000
                },
            ]
        );
    }

    #[test]
    fn empty_version_list_is_not_an_error() {
        let info = empty_version_list(r"C:\Other\note.txt");
        assert!(info.versions.is_empty());
        assert!(info.folder.is_empty());
    }

    #[tokio::test]
    async fn list_file_versions_sends_shared_path_hash_and_folder() {
        // A listable version is a signed one (every writer signs), and its
        // statement needs a manifest; the judge then reads the folder list for
        // the set's binding — so the nest double is the one that answers it.
        let fake = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![signed_by_root(FileVersionInfo {
                manifest_hash: fauna_protocol::ByteBuf::from(vec![4u8; 32]),
                ..version(4, 100, 1_000)
            })],
            ..Default::default()
        }));
        let sync = SyncClient::new(Arc::clone(&fake));

        let info = list_file_versions(
            &sync,
            r"C:\Sync\docs\report.txt",
            "docs",
            "docs/report.txt",
            &seat(),
        )
        .await
        .unwrap();
        assert_eq!(info.versions.len(), 1);

        assert_eq!(
            fake.kinds(),
            vec!["fauna.files.versions.list", "fauna.folders.list"]
        );
        let req: FilesVersionsListRequest = fake.decode(0);

        // The wire key MUST be the shared derivation over the RELATIVE path, not an
        // inline blake3 of the absolute Windows path (file-sync.md § Path hashing).
        assert_eq!(
            req.path_hash.as_ref(),
            &fauna_core::sync::path_hash("docs/report.txt")[..]
        );

        // `folder` is always scoped — a bare path_hash is ambiguous across sets.
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
    }

    #[tokio::test]
    async fn list_file_versions_surfaces_transport_error() {
        let sync = SyncClient::new(FakeNest::err("nest unreachable"));
        let err = list_file_versions(
            &sync,
            r"C:\Sync\docs\report.txt",
            "docs",
            "docs/report.txt",
            &ReaderSeat::default(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("nest unreachable"), "got: {err}");
    }

    // ── Restore ─────────────────────────────────────────────────────────────

    /// Answers `fauna.files.versions.list`, `fauna.sync.changes.record` and the
    /// judge's `fauna.folders.list` (one set, `docs`, the caller's own) by kind,
    /// over the same real dag-cbor codec as [`FakeNest`], and records every call so
    /// a test can assert both the request shapes **and that a call never happened**.
    struct FakeRestoreNest {
        calls: Mutex<Vec<(String, Vec<u8>)>>,
        list_reply: Result<FilesVersionsListReply, FakeErr>,
        record_reply: Result<SyncChangeRecordReply, FakeErr>,
    }

    impl FakeRestoreNest {
        /// A listing of the one version `get_reply` names (or the lookup's failure).
        fn new(get_reply: Result<FileVersionInfo, FakeErr>) -> Arc<Self> {
            Self::listing(get_reply.map(|v| FilesVersionsListReply {
                versions: vec![v],
                ..Default::default()
            }))
        }
        fn listing(list_reply: Result<FilesVersionsListReply, FakeErr>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                list_reply,
                record_reply: Ok(SyncChangeRecordReply {
                    seq: 77,
                    extra: Default::default(),
                }),
            })
        }
        fn kinds(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(k, _)| k.clone())
                .collect()
        }
        fn decode<Req: serde::de::DeserializeOwned>(&self, idx: usize) -> Req {
            let calls = self.calls.lock().unwrap();
            fauna_cbor::decode_strict(&calls[idx].1).unwrap()
        }
    }

    impl RpcRequester for FakeRestoreNest {
        type Error = FakeErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_cbor::encode_canonical(&payload).unwrap();
            self.calls.lock().unwrap().push((kind.to_string(), bytes));
            let encoded = match kind {
                "fauna.files.versions.list" => {
                    fauna_cbor::encode_canonical(&self.list_reply.clone()?).unwrap()
                }
                fauna_protocol::folders::KIND_FOLDERS_LIST => {
                    fauna_cbor::encode_canonical(&fauna_protocol::folders::FoldersListReply {
                        folders: vec![fauna_protocol::folders::FolderSummary {
                            name: "docs".into(),
                            role: Some("owner".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    })
                    .unwrap()
                }
                "fauna.sync.changes.record" => {
                    fauna_cbor::encode_canonical(&self.record_reply.clone()?).unwrap()
                }
                other => panic!("unexpected RPC kind: {other}"),
            };
            Ok(fauna_cbor::decode_strict(&encoded).unwrap())
        }
    }

    /// Version 42 as the nest lists it — signed by its writer, as every
    /// admitted version is.
    fn historical(manifest: [u8; 32], size: i64, ckv: Option<u64>) -> FileVersionInfo {
        signed_by_root(FileVersionInfo {
            version_num: 42,
            manifest_hash: fauna_protocol::ByteBuf::from(manifest.to_vec()),
            size_bytes: size,
            content_key_version: ckv,
            ..Default::default()
        })
    }

    /// The record must carry the historical `manifest_hash` / `size_bytes` /
    /// `content_key_version` **verbatim** (file-sync.md § Restore) — restore is a
    /// re-point, never a re-upload — and must be looked up *before* it is recorded.
    #[tokio::test]
    async fn restore_looks_up_then_records_historical_metadata_verbatim() {
        let fake = FakeRestoreNest::new(Ok(historical([7u8; 32], 4096, Some(3))));
        let sync = SyncClient::new(Arc::clone(&fake));

        let restored = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            Some(vec![9u8; 40]),
            &seat(),
            &NeverReseals,
        )
        .await
        .unwrap();

        // Order is load-bearing: the metadata must exist — and be judged
        // (the folder list is the judge's read of the set's binding) — before
        // anything is recorded.
        assert_eq!(
            fake.kinds(),
            vec![
                "fauna.files.versions.list",
                "fauna.folders.list",
                "fauna.sync.changes.record"
            ]
        );

        let list: FilesVersionsListRequest = fake.decode(0);
        assert_eq!(
            list.path_hash.as_ref(),
            &fauna_core::sync::path_hash("docs/report.txt")[..]
        );
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &list, "docs"
        ));
        assert_eq!(
            list.include_pruned,
            Some(true),
            "a soft-pruned version stays restorable"
        );

        let rec: SyncChangeRecordRequest = fake.decode(2);
        assert_eq!(rec.change_type, "modify");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &rec, "docs"
        ));
        assert_eq!(rec.device_id, "dev-1");
        // The nest re-derives `path_hash(path)`, so `path` must be the same
        // forward-slash relative path we looked the version up by.
        assert_eq!(rec.path, "docs/report.txt");
        assert_eq!(
            rec.manifest_hash.as_deref(),
            Some(hex::encode([7u8; 32])).as_deref()
        );
        assert_eq!(rec.size_bytes, 4096);
        assert_eq!(rec.content_key_version, Some(3));
        // The caller-minted seal rides the wire VERBATIM (S8 D2 — the re-point
        // row is append-only nest-side, so this record is its only seal).
        assert_eq!(
            rec.path_sealed.as_ref().map(|b| &b[..]),
            Some(&[9u8; 40][..])
        );

        // What the caller needs for the local re-point.
        assert_eq!(restored.manifest_hash, [7u8; 32]);
        assert_eq!(restored.size_bytes, 4096);
        assert_eq!(restored.content_key_version, Some(3));
        assert_eq!(restored.recorded_seq, 77);
    }

    /// A failed lookup must record **nothing** — a `modify` without the historical
    /// manifest would re-point the file at a guess.
    #[tokio::test]
    async fn restore_records_nothing_when_the_lookup_fails() {
        let fake = FakeRestoreNest::new(Err(FakeErr("not found".to_string())));
        let sync = SyncClient::new(Arc::clone(&fake));

        let err = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &ReaderSeat::default(),
            &NeverReseals,
        )
        .await
        .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert_eq!(fake.kinds(), vec!["fauna.files.versions.list"]);
    }

    /// A row with no manifest is a delete tombstone, not a restorable version.
    /// It can carry no statement (a version's statement names its manifest),
    /// so the judge refuses it before the manifest check is ever reached —
    /// and nothing is recorded.
    #[tokio::test]
    async fn restore_rejects_a_version_with_no_restorable_content() {
        let info = FileVersionInfo {
            version_num: 42,
            path_hash: fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash("docs/report.txt").to_vec(),
            ),
            author_actor_id: root().actor_id().to_hex(),
            device_id: Some(fauna_protocol::ByteBuf::from(vec![4; 32])),
            change_type: Some("delete".into()),
            ..Default::default()
        };
        let fake = FakeRestoreNest::new(Ok(info));
        let sync = SyncClient::new(Arc::clone(&fake));

        let err = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &seat(),
            &NeverReseals,
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not verify"), "got: {err}");
        assert!(
            !fake
                .kinds()
                .contains(&"fauna.sync.changes.record".to_string()),
            "nothing recorded: {:?}",
            fake.kinds()
        );
    }

    // ── The reader half (writer-signed change records, ruling (3)) ───────────

    const NONCE: [u8; 32] = [0x5a; 32];
    const OTHER_NONCE: [u8; 32] = [0x6b; 32];

    fn root() -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret([0x22; 32])
    }

    /// The agent's seat: its own account, `docs`'s nonce from its custody.
    fn seat() -> ReaderSeat {
        ReaderSeat {
            own: Some(root().actor_id().0),
            nonces: Some(fauna_client_sync::SetNonceSource::by_folder(
                [("docs".to_string(), NONCE)].into_iter().collect(),
            )),
            ..Default::default()
        }
    }

    /// `v` as `docs/report.txt`'s version by the root writer, signed under
    /// [`NONCE`] — every field the statement needs filled in where the fixture
    /// left it empty.
    fn signed_by_root(mut v: FileVersionInfo) -> FileVersionInfo {
        v.path_hash =
            fauna_protocol::ByteBuf::from(fauna_core::sync::path_hash("docs/report.txt").to_vec());
        v.author_actor_id = root().actor_id().to_hex();
        v.device_id = Some(fauna_protocol::ByteBuf::from(vec![4; 32]));
        if v.change_type.is_none() {
            v.change_type = Some("modify".into());
        }
        let mut row = v.as_change_row().expect("statement");
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root())
            .sign_row(&mut row, NONCE)
            .expect("signs");
        v.signature = row.signature;
        v.signer_key = row.signer_key;
        v
    }

    /// Version `num` of `docs/report.txt`, signed by `signer` under `nonce`.
    fn signed_version(
        num: i64,
        signer: &fauna_protocol::sync_writer_sig::ChangeSigner,
        nonce: [u8; 32],
    ) -> FileVersionInfo {
        let mut v = FileVersionInfo {
            version_num: num,
            path_hash: fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash("docs/report.txt").to_vec(),
            ),
            manifest_hash: fauna_protocol::ByteBuf::from(vec![num as u8; 32]),
            size_bytes: 4096,
            created_at: 1_000,
            author_actor_id: root().actor_id().to_hex(),
            device_id: Some(fauna_protocol::ByteBuf::from(vec![4; 32])),
            change_type: Some("modify".into()),
            ..Default::default()
        };
        let mut row = v.as_change_row().expect("statement");
        signer.sign_row(&mut row, nonce).expect("signs");
        v.signature = row.signature;
        v.signer_key = row.signer_key;
        v
    }

    /// The Explorer menu lists only admitted versions: one signed under ANOTHER
    /// set's nonce is absent, and so is the unsigned one (the flipped switch).
    #[tokio::test]
    async fn list_omits_a_version_signed_under_another_sets_nonce() {
        let signer = fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root());
        let fake = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![
                signed_version(4, &signer, NONCE),
                signed_version(6, &signer, OTHER_NONCE),
                version(9, 250, 2_000),
            ],
            ..Default::default()
        }));
        let sync = SyncClient::new(Arc::clone(&fake));
        let info = list_file_versions(
            &sync,
            r"C:\Sync\docs\report.txt",
            "docs",
            "docs/report.txt",
            &seat(),
        )
        .await
        .unwrap();
        let nums: Vec<i64> = info.versions.iter().map(|v| v.version_num).collect();
        assert_eq!(nums, [4]);
    }

    /// A version that does not verify is not a version to restore to: the
    /// restore refuses it and records NOTHING.
    #[tokio::test]
    async fn restore_refuses_a_version_that_does_not_verify() {
        let signer = fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root());
        let fake = FakeRestoreNest::new(Ok(signed_version(42, &signer, OTHER_NONCE)));
        let sync = SyncClient::new(Arc::clone(&fake));
        let err = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &seat(),
            &NeverReseals,
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not verify"), "got: {err}");
        assert!(
            !fake
                .kinds()
                .contains(&"fauna.sync.changes.record".to_string()),
            "nothing recorded: {:?}",
            fake.kinds()
        );
    }

    /// Ruling (8)(d), the restore sentence: a version a PREDECESSOR signed
    /// verifies as the account's own and lists — but a head re-signed under
    /// the current identity is offered the current owner root. So an unstamped
    /// one (its bytes rest under an owner root) is never re-signed unopened:
    /// it goes through the byte seam, and what is recorded is the NEW manifest
    /// the seam re-sealed it into. A seam that refuses (the attack: bytes the
    /// current root sealed, under a predecessor's signature) records NOTHING.
    /// A stamped one opens under its content-key generation whoever re-signs
    /// it, and re-points verbatim, as does the current identity's own.
    #[tokio::test]
    async fn restore_reseals_a_predecessors_owner_keyed_version_and_records_the_new_manifest() {
        let predecessor = fauna_core::identity::ActorKeypair::from_secret([0x11; 32]);
        // `num`, signed by the predecessor, served — as the nest serves it
        // after the succession — with the successor as author.
        let inherited = |num: i64, generation: Option<u64>| {
            let mut v = FileVersionInfo {
                version_num: num,
                path_hash: fauna_protocol::ByteBuf::from(
                    fauna_core::sync::path_hash("docs/report.txt").to_vec(),
                ),
                manifest_hash: fauna_protocol::ByteBuf::from(vec![num as u8; 32]),
                size_bytes: 4096,
                created_at: 1_000,
                author_actor_id: predecessor.actor_id().to_hex(),
                device_id: Some(fauna_protocol::ByteBuf::from(vec![4; 32])),
                change_type: Some("modify".into()),
                content_key_version: generation,
                ..Default::default()
            };
            let mut row = v.as_change_row().expect("statement");
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&predecessor)
                .sign_row(&mut row, NONCE)
                .expect("signs");
            v.signature = row.signature;
            v.signer_key = row.signer_key;
            v.author_actor_id = root().actor_id().to_hex();
            v
        };
        let successor_seat = ReaderSeat {
            predecessors: vec![predecessor.actor_id().0],
            ..seat()
        };

        // It lists: the account's own history.
        let fake = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![inherited(42, None)],
            ..Default::default()
        }));
        let sync = SyncClient::new(Arc::clone(&fake));
        let listed = list_file_versions(
            &sync,
            r"C:\Sync\docs\report.txt",
            "docs",
            "docs/report.txt",
            &successor_seat,
        )
        .await
        .unwrap();
        assert_eq!(listed.versions.len(), 1);

        // Unstamped: opened and re-sealed through the seam — asked with the
        // historical manifest and the identity it was signed as — and the
        // record carries the NEW manifest and its size, never the old one.
        let fake = FakeRestoreNest::new(Ok(inherited(42, None)));
        let sync = SyncClient::new(Arc::clone(&fake));
        let seam = FakeReseal::answering(Ok(fauna_sync_engine::engine::ResealedVersion {
            manifest_hash: fauna_core::data::ContentHash::from_digest_raw([0xee; 32]),
            size_bytes: 5000,
        }));
        let restored = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &successor_seat,
            &seam,
        )
        .await
        .expect("an inherited unstamped version restores through the byte seam");
        assert_eq!(
            *seam.asked.lock().unwrap(),
            vec![(
                "docs/report.txt".to_string(),
                [42u8; 32],
                Some(predecessor.actor_id().0)
            )]
        );
        assert_eq!(
            fake.kinds(),
            vec![
                "fauna.files.versions.list",
                "fauna.folders.list",
                "fauna.sync.changes.record"
            ]
        );
        let rec: SyncChangeRecordRequest = fake.decode(2);
        assert_eq!(
            rec.manifest_hash.as_deref(),
            Some(hex::encode([0xee; 32])).as_deref()
        );
        assert_eq!(rec.size_bytes, 5000);
        assert_eq!(rec.content_key_version, None);
        // The local copy re-points at the new manifest too.
        assert_eq!(restored.manifest_hash, [0xee; 32]);
        assert_eq!(restored.size_bytes, 5000);
        assert_eq!(restored.content_key_version, None);

        // The attack: the seam refuses (the bytes open under no root the
        // predecessor's signature may reach) — the reason surfaces, and
        // nothing is recorded.
        let fake = FakeRestoreNest::new(Ok(inherited(42, None)));
        let sync = SyncClient::new(Arc::clone(&fake));
        let seam = FakeReseal::answering(Err(
            fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE.to_string(),
        ));
        let err = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &successor_seat,
            &seam,
        )
        .await
        .unwrap_err();
        assert!(
            err.contains(fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE),
            "got: {err}"
        );
        assert!(
            !fake
                .kinds()
                .contains(&"fauna.sync.changes.record".to_string()),
            "nothing recorded: {:?}",
            fake.kinds()
        );

        // Stamped: re-points verbatim, the seam never reached.
        let fake = FakeRestoreNest::new(Ok(inherited(43, Some(3))));
        let sync = SyncClient::new(Arc::clone(&fake));
        let restored = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            43,
            None,
            &successor_seat,
            &NeverReseals,
        )
        .await
        .expect("a stamped version never opens under an owner root");
        assert_eq!(restored.content_key_version, Some(3));
        assert_eq!(restored.manifest_hash, [43u8; 32]);

        // The successor's OWN unstamped version re-points verbatim, as it
        // always did — the seam never reached.
        let signer = fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root());
        let fake = FakeRestoreNest::new(Ok(signed_version(44, &signer, NONCE)));
        let sync = SyncClient::new(Arc::clone(&fake));
        let restored = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            44,
            None,
            &successor_seat,
            &NeverReseals,
        )
        .await
        .expect("signed as the current identity");
        assert_eq!(restored.manifest_hash, [44u8; 32]);
        let rec: SyncChangeRecordRequest = fake.decode(2);
        assert_eq!(
            rec.manifest_hash.as_deref(),
            Some(hex::encode([44u8; 32])).as_deref()
        );

        // Ruling (10)(b): the statement binds the manifest to the SET, so an
        // inherited unstamped version restores verbatim once the successor has
        // itself signed a row of the set naming the same manifest.
        let mut own_row = signed_version(45, &signer, NONCE);
        own_row.manifest_hash = fauna_protocol::ByteBuf::from(vec![42; 32]);
        let mut row = own_row.as_change_row().expect("statement");
        row.signature = None;
        row.signer_key = None;
        signer.sign_row(&mut row, NONCE).expect("signs");
        own_row.signature = row.signature;
        own_row.signer_key = row.signer_key;
        let fake = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![inherited(42, None), own_row],
            ..Default::default()
        }));
        let sync = SyncClient::new(Arc::clone(&fake));
        restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &successor_seat,
            &NeverReseals,
        )
        .await
        .expect("the current identity vouches for the manifest in this set");
    }

    /// Ruling (11)(f), the opening restore: a HISTORY version — a
    /// predecessor's row under a retired nonce of the set, what the cut left
    /// behind — is listed under the identity it was signed as, and restores
    /// through the byte seam exactly as an inherited version does: asked with
    /// the historical manifest and its signer, and recorded over the NEW
    /// manifest the seam answers. The seat carries the set's lineage, as the
    /// agent's own does (`ResolvedContentKeys::set_lineages_by_name`). Seen red
    /// with the inherited refusal standing.
    #[tokio::test]
    async fn a_history_version_restores_through_the_opening_restore() {
        let predecessor = fauna_core::identity::ActorKeypair::from_secret([0x13; 32]);
        let mut history = FileVersionInfo {
            version_num: 50,
            path_hash: fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash("docs/report.txt").to_vec(),
            ),
            manifest_hash: fauna_protocol::ByteBuf::from(vec![50; 32]),
            size_bytes: 4096,
            created_at: 1_000,
            author_actor_id: predecessor.actor_id().to_hex(),
            device_id: Some(fauna_protocol::ByteBuf::from(vec![4; 32])),
            change_type: Some("modify".into()),
            ..Default::default()
        };
        let mut row = history.as_change_row().expect("statement");
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(&predecessor)
            .sign_row(&mut row, OTHER_NONCE)
            .expect("signs");
        history.signature = row.signature;
        history.signer_key = row.signer_key;
        history.author_actor_id = root().actor_id().to_hex();
        let lineage_seat = ReaderSeat {
            predecessors: vec![predecessor.actor_id().0],
            nonces: Some(fauna_client_sync::SetNonceSource::ByFolder(Arc::new(
                [(
                    "docs".to_string(),
                    fauna_core::folder_keys::SetNonceLineage {
                        live: Some(NONCE),
                        retired: vec![fauna_core::folder_keys::RetiredSetNonce {
                            nonce: OTHER_NONCE,
                            minted_by: None,
                        }],
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            ))),
            ..seat()
        };

        let fake = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![history.clone()],
            ..Default::default()
        }));
        let sync = SyncClient::new(Arc::clone(&fake));
        let listed = list_file_versions(
            &sync,
            r"C:\Sync\docs\report.txt",
            "docs",
            "docs/report.txt",
            &lineage_seat,
        )
        .await
        .unwrap();
        assert_eq!(
            listed.versions.len(),
            1,
            "a history row is listed as a version"
        );

        let fake = FakeRestoreNest::new(Ok(history));
        let sync = SyncClient::new(Arc::clone(&fake));
        let seam = FakeReseal::answering(Ok(fauna_sync_engine::engine::ResealedVersion {
            manifest_hash: fauna_core::data::ContentHash::from_digest_raw([0xdd; 32]),
            size_bytes: 4100,
        }));
        let restored = restore_file_version(
            &sync,
            "docs",
            "dev-1",
            "docs/report.txt",
            50,
            None,
            &lineage_seat,
            &seam,
        )
        .await
        .expect("a history version restores through the opening restore");
        assert_eq!(
            *seam.asked.lock().unwrap(),
            vec![(
                "docs/report.txt".to_string(),
                [50u8; 32],
                Some(predecessor.actor_id().0)
            )],
            "opened under the roots its signer allows"
        );
        let rec: SyncChangeRecordRequest = fake.decode(2);
        assert_eq!(
            rec.manifest_hash.as_deref(),
            Some(hex::encode([0xdd; 32])).as_deref(),
            "recorded over the re-sealed manifest"
        );
        assert_eq!(restored.manifest_hash, [0xdd; 32]);
    }

    /// One ask of the byte seam: the path, the historical manifest, and the
    /// identity the version was signed as.
    type ResealAsk = (String, [u8; 32], Option<[u8; 32]>);

    /// A byte seam that answers what it is told and remembers what it was
    /// asked.
    struct FakeReseal {
        asked: Mutex<Vec<ResealAsk>>,
        reply: Result<fauna_sync_engine::engine::ResealedVersion, String>,
    }

    impl FakeReseal {
        fn answering(reply: Result<fauna_sync_engine::engine::ResealedVersion, String>) -> Self {
            Self {
                asked: Mutex::new(Vec::new()),
                reply,
            }
        }
    }

    impl InheritedReseal for FakeReseal {
        async fn reseal(
            &self,
            rel: &str,
            manifest_hash: [u8; 32],
            signed_as: Option<[u8; 32]>,
        ) -> Result<fauna_sync_engine::engine::ResealedVersion, String> {
            self.asked
                .lock()
                .unwrap()
                .push((rel.to_string(), manifest_hash, signed_as));
            self.reply.clone()
        }
    }

    /// A DELEGATED writer's version (the agent itself signs as a delegated
    /// machine principal) stays restorable: the verdict comes from the listing,
    /// whose `signer_certs` carry the cert its row chains through — without
    /// them the same version would be refused.
    #[tokio::test]
    async fn a_delegated_signers_version_restores_through_the_listings_certs() {
        let writer = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: root().actor_id(),
            device_key: writer.verifying_key().to_bytes(),
            capabilities: vec![fauna_core::data::Capability::SyncWrite],
            created_at: fauna_core::data::Timestamp(0),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&root(), &auth).unwrap();
        let cert = fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env);
        let signer = fauna_protocol::sync_writer_sig::ChangeSigner::delegated(
            root().actor_id().0,
            writer,
            cert.clone(),
        );
        let version = signed_version(42, &signer, NONCE);

        let with_certs = FakeRestoreNest::listing(Ok(FilesVersionsListReply {
            versions: vec![version.clone()],
            signer_certs: vec![cert],
            ..Default::default()
        }));
        let restored = restore_file_version(
            &SyncClient::new(Arc::clone(&with_certs)),
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &seat(),
            &NeverReseals,
        )
        .await
        .expect("a delegated writer's verified version restores");
        assert_eq!(restored.manifest_hash, [42u8; 32]);

        let without_certs = FakeRestoreNest::new(Ok(version));
        let err = restore_file_version(
            &SyncClient::new(Arc::clone(&without_certs)),
            "docs",
            "dev-1",
            "docs/report.txt",
            42,
            None,
            &seat(),
            &NeverReseals,
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not verify"), "got: {err}");
    }

    /// The absolute Windows path must NOT be what gets hashed — only the
    /// forward-slash folder-relative path, or the hash diverges from every other
    /// app's for the same file.
    #[test]
    fn path_hash_is_over_the_relative_forward_slash_path() {
        assert_ne!(
            fauna_core::sync::path_hash("docs/report.txt"),
            fauna_core::sync::path_hash(r"C:\Sync\docs\report.txt")
        );
        assert_ne!(
            fauna_core::sync::path_hash("docs/report.txt"),
            fauna_core::sync::path_hash(r"docs\report.txt")
        );
    }
}
