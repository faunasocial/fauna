//! The native [`AuditStateStore`] over a JSON file — shared by every native
//! shell (linux, tui, and, as they land, windows/macos/ios/android). Before this
//! module the load/save shape was independently reimplemented byte-for-byte in
//! linux's and tui's own `backup_audit.rs` (down to the doc comments); only the
//! *path* (config-dir convention, per-account/per-actor scoping) is genuinely
//! platform-specific, per [`audit::AuditStateStore`]'s own doc comment ("a file
//! under the client's data dir natively, `localStorage` on web").
//! `docs/goal/ui/backups.md` § Audit-alert surface rules that the state stays
//! client-local (never synced) — a data-locality rule, not a per-shell
//! reimplementation mandate.
//!
//! wasm32 has no filesystem, so this module is native-only — mirroring
//! `fauna_client_pair::native_backup_inclusion_source`. Web's twin is
//! `fauna_wasm::backup_audit::LocalStorageAuditStateStore`. The e2e **clock**
//! is not native-only and no longer lives here: it moved to
//! [`crate::audit_clock`] so both stores share one copy, and is re-exported
//! below so every existing `native_store::` import keeps resolving.

use std::path::PathBuf;

use crate::audit::{AuditStateSnapshot, AuditStateStore};

/// An [`AuditStateStore`] over a JSON file at a caller-resolved path.
pub struct FileAuditStateStore {
    path: PathBuf,
}

impl FileAuditStateStore {
    /// A store at an explicit path — each shell resolves its own (account- or
    /// actor-scoped) path and passes it in; nothing here is shell-specific.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }
}

impl AuditStateStore for FileAuditStateStore {
    fn load(&self) -> Result<AuditStateSnapshot, String> {
        // A missing file is the normal first-run state, not an error.
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AuditStateSnapshot::default());
            }
            Err(e) => return Err(format!("read audit state: {e}")),
        };
        serde_json::from_str(&raw).map_err(|e| format!("parse audit state: {e}"))
    }

    /// Persist `snapshot` by **atomic replace**, never in place.
    ///
    /// A plain `fs::write` truncates first, so a concurrent reader can observe a
    /// partial file — and [`Self::load`] deliberately reports a parse failure as
    /// an error rather than degrading, so a torn read surfaces as a format bug.
    /// The account instance lock used to make that unreachable by keeping two
    /// same-account processes apart; it is being retired
    /// (`account-data-plane.md` § Multi-instance concurrency), and this state is
    /// per-actor but not per-process. Write-then-rename makes the partial state
    /// unobservable instead of merely unlikely: the temp name carries pid + a counter so two writers
    /// never share one, `sync_all` gets the bytes down before the name flips,
    /// and rename is atomic within a directory.
    ///
    /// Two same-account writers still race for *last writer wins* on the
    /// snapshot as a whole — each writes a snapshot it read and folded, so the
    /// loser's fold is lost but nothing it could not read is — and unlike a
    /// torn read it is not observable as corruption.
    fn save(&self, snapshot: &AuditStateSnapshot) -> Result<(), String> {
        use std::io::Write as _;

        let dir = self
            .path
            .parent()
            .ok_or_else(|| "audit state path has no parent directory".to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("create audit state dir: {e}"))?;

        let raw =
            serde_json::to_string(snapshot).map_err(|e| format!("encode audit state: {e}"))?;

        static SAVE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = dir.join(format!(
            ".{}.{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("audit-state.json"),
            std::process::id(),
            SAVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));

        let written = std::fs::File::create(&tmp).and_then(|mut f| {
            f.write_all(raw.as_bytes())?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write audit state: {e}"));
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("publish audit state: {e}"));
        }
        Ok(())
    }
}

// ── the e2e clock ───────────────────────────────────────────────────────────
//
// Moved to the platform-neutral `crate::audit_clock` so web's `localStorage`
// store shares it rather than reimplementing it (the clock has no filesystem in
// it; only the store below does). Re-exported here because every native shell
// already reaches the trio through this module's path, and one import path for
// one concept beats two (priority #3).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use crate::audit_clock::set_clock_offset_secs;
pub use crate::audit_clock::{clock_offset_secs, now_ms, now_secs};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditVerdict, DestinationAuditRecord, DestinationAuditState};

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fauna-client-backup-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("audit-state.json")
    }

    /// The whole point of persisting: what one app launch wrote, the next reads.
    #[test]
    fn a_saved_snapshot_reads_back_with_its_verdict_intact() {
        let store = FileAuditStateStore::at(tmp_path("roundtrip"));
        let snapshot = AuditStateSnapshot {
            records: vec![DestinationAuditRecord {
                state: DestinationAuditState {
                    destination_id: "d1".into(),
                    last_passed_at: Some(1_700_000_000),
                    last_attempt_at: Some(1_700_100_000),
                    verified_ledger_generations: Default::default(),
                    accepted_regressions: Default::default(),
                    seat_settled_under: Some("ab".repeat(32)),
                },
                verdict: Some(AuditVerdict::FreshnessFailure { lag_secs: 300_000 }),
            }],
            observed_high_water: Some(1_700_200_000),
        };
        store.save(&snapshot).expect("save");
        assert_eq!(store.load().expect("load"), snapshot);
    }

    /// First run has no file — that is the normal state, not an error, and it
    /// must not stop the first audit from running.
    #[test]
    fn a_missing_file_loads_as_the_empty_snapshot() {
        let store = FileAuditStateStore::at(tmp_path("missing"));
        assert_eq!(
            store.load().expect("a missing file is not an error"),
            AuditStateSnapshot::default()
        );
    }

    /// A corrupt file IS reported, never passed off as empty: `Err` is what
    /// tells every caller not to save over it (`AuditStateSnapshot`'s doc).
    #[test]
    fn a_corrupt_file_reports_rather_than_pretending_to_be_empty() {
        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let store = FileAuditStateStore::at(path);
        assert!(store.load().is_err());
    }

    /// The refuse-to-rewrite gate, end to end through the real file: a file
    /// this build cannot read — here a newer build's reshaped record list — is
    /// neither reset nor overwritten by an observation or a pass; its bytes are
    /// exactly what the newer build wrote (`transport.md` § Rule 3 in full →
    /// *The store around the enum*).
    #[test]
    fn an_unreadable_file_is_never_reset_or_overwritten() {
        let path = tmp_path("unreadable");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{"records":{"by_destination":{"d1":{"pins":[3]}}},"observed_high_water":1700000000}"#;
        std::fs::write(&path, newer).unwrap();
        let store = FileAuditStateStore::at(path.clone());

        let out = crate::audit::observe_thread_activity(&store, now_ms());
        assert!(!out.persisted);
        assert_eq!(out.degradations.len(), 1, "{out:?}");

        let pass = fauna_client_testkit::block_on(crate::audit::run_audit_pass(
            &NoDestinations,
            &NoInclusion,
            &store,
            &[],
            "https://home.example",
            &[0xB0; 32],
            None,
            now_secs(),
        ));
        assert!(pass.records.is_empty());
        assert_eq!(pass.degradations.len(), 1, "{pass:?}");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    struct NoDestinations;
    #[async_trait::async_trait]
    impl crate::trust::BackupDestinationConnector for NoDestinations {
        async fn connect(&self, _: &str) -> Result<crate::trust::DestinationConnection, String> {
            Err("no destinations in this test".into())
        }
    }

    /// Never asked: a pass over no destinations samples nothing.
    struct NoInclusion;
    impl crate::audit::BackupInclusionSource for NoInclusion {
        fn fetcher(&self, _: &str) -> std::sync::Arc<dyn fauna_core::file_download::BlobFetcher> {
            unreachable!()
        }
        fn keys(&self) -> fauna_core::file_download::FileDownloadKeys {
            unreachable!()
        }
        fn folder_index(&self, _: &str) -> Option<crate::audit::FolderIndex> {
            unreachable!()
        }
    }

    fn snapshot_of(records: usize, high_water: i64) -> AuditStateSnapshot {
        AuditStateSnapshot {
            records: (0..records)
                .map(|i| DestinationAuditRecord {
                    state: DestinationAuditState {
                        destination_id: format!("destination-{i:04}"),
                        last_passed_at: Some(1_700_000_000 + i as i64),
                        last_attempt_at: Some(1_700_100_000 + i as i64),
                        verified_ledger_generations: Default::default(),
                        accepted_regressions: Default::default(),
                        seat_settled_under: None,
                    },
                    verdict: Some(AuditVerdict::FreshnessFailure { lag_secs: 300_000 }),
                })
                .collect(),
            observed_high_water: Some(high_water),
        }
    }

    /// A reader must never see a half-written file.
    ///
    /// The state is per-actor but not per-*process*, and the account instance
    /// lock that has been keeping two same-account processes apart is being
    /// retired (`account-data-plane.md` § Multi-instance concurrency). A
    /// truncate-then-write save is observable mid-flight, and this store
    /// deliberately reports a parse failure as an error
    /// (`a_corrupt_file_reports_rather_than_pretending_to_be_empty`) — so a torn
    /// read does not degrade quietly, it surfaces as "a real bug in the format".
    ///
    /// Causal rendezvous, no sleeps: reader and writer are released together by
    /// a [`Barrier`] and the reader stops when the writer says it is done, so
    /// the assert is on *what was observed*, never on how long anything took.
    #[test]
    fn a_concurrent_reader_never_observes_a_half_written_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier};

        const ROUNDS: usize = 200;

        let path = tmp_path("torn-read");
        let store = Arc::new(FileAuditStateStore::at(path));
        // Big enough that the write is many kilobytes: a truncating writer is
        // then observably mid-flight rather than vanishingly briefly so.
        let big = snapshot_of(2_000, 1_700_200_000);
        store.save(&big).expect("seed the file");

        let gate = Arc::new(Barrier::new(2));
        let writing = Arc::new(AtomicBool::new(true));

        let errors = std::thread::scope(|s| {
            let writer = {
                let store = Arc::clone(&store);
                let gate = Arc::clone(&gate);
                let writing = Arc::clone(&writing);
                let big = big.clone();
                s.spawn(move || {
                    gate.wait();
                    for round in 0..ROUNDS {
                        let mut snapshot = big.clone();
                        snapshot.observed_high_water = Some(1_700_200_000 + round as i64);
                        store.save(&snapshot).expect("save");
                    }
                    writing.store(false, Ordering::Release);
                })
            };
            let reader = {
                let store = Arc::clone(&store);
                let gate = Arc::clone(&gate);
                let writing = Arc::clone(&writing);
                s.spawn(move || {
                    gate.wait();
                    let mut errors = Vec::new();
                    while writing.load(Ordering::Acquire) {
                        if let Err(e) = store.load() {
                            errors.push(e);
                        }
                    }
                    errors
                })
            };
            writer.join().unwrap();
            reader.join().unwrap()
        });

        assert!(
            errors.is_empty(),
            "a concurrent reader saw {} unreadable snapshot(s); first: {}",
            errors.len(),
            errors[0]
        );
    }

    /// The atomic-replace mechanism must not litter: a crashed or racing writer
    /// aside, a completed save leaves exactly the state file behind. Fully
    /// deterministic — this is the pin on the mechanism itself.
    #[test]
    fn a_completed_save_leaves_no_temp_file_behind() {
        let path = tmp_path("no-litter");
        let store = FileAuditStateStore::at(path.clone());
        store.save(&snapshot_of(4, 1_700_200_000)).expect("save");

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "audit-state.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "save left temp files behind: {leftovers:?}"
        );
    }
}
