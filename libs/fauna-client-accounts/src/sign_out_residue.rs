//! What a sign-out's erase left behind, **recorded so it outlives the process**,
//! and the one re-sweep every seat runs over it — from the
//! `sign-out-residue-retry-button` and, silently, at the next signed-out launch
//! (`account-scoping.md` § Erasure follows scope → *the residue surface*).
//!
//! [`crate::EraseResidueView`] answers *what the user is told* about a residue;
//! this module answers *what the device remembers* about it. A line on screen
//! dies with the window, while the survivors it describes stay on the disk — so
//! a user who signs out, sees the line and closes the app is told nothing next
//! time, with their data still here. The record closes that gap: the sign-out
//! writes it into the app's install-scoped state, a clean re-sweep deletes it,
//! and a launch that finds it re-sweeps first and speaks only if something is
//! still left.
//!
//! ⚠ **A re-sweep is NOT a second sign-out, and must never reach further than
//! the record.** The sign-out's all-accounts sweep runs over bases the device
//! shares with sibling apps (the account-store root), and a launch-time run of
//! that sweep once erased a running sibling's live store. So the re-sweep:
//!
//! 1. **touches only the recorded paths** — and never erases under a recorded
//!    base at all (rule 3's base arm below);
//! 2. **asks the sign-out's own question first** ([`SignOutResidue::retry_blocked`],
//!    the same serving-lock probe as [`crate::sign_out_blocked`]) about every
//!    account those paths belong to, and erases nothing if any is served;
//! 3. **leaves alone any path that changed since it was recorded**
//!    ([`ResiduePath::fingerprint`]): a later sign-in — on this app or a
//!    sibling sharing the store root — re-creates exactly these directories, or
//!    adopts the `account-store/` that survived inside one and writes only
//!    there, and a store someone is using again is no longer residue, even
//!    while no process holds it open. So a scope's fingerprint covers its WHOLE
//!    tree, not the directory's own mtime — a write one level down moves no
//!    mtime on the scope itself. **A recorded base has no fingerprint and is
//!    never re-erased:** it was recorded because the sign-out could not look
//!    into it, so nothing taken then vouches for any scope under it, and no
//!    later reading can be a baseline either (a sibling's sign-in that adopted
//!    a scope and exited before that reading would pass as unchanged). It
//!    stays on the record, untouched, while it still cannot be listed or still
//!    holds an actor scope — the line stays honest — and is done once it lists
//!    and holds none; what erases the scopes under it is the next sign-out's
//!    own sweep, the gesture that may reach every scope;
//! 4. **re-erases credentials only when the record says they survived** — a
//!    clean credential half has nothing of the user's left to remove, and the
//!    namespace may hold state written since.
//!
//! Shared rather than per-seat for the reason the rest of the sign-out copy is:
//! five seats re-deciding which survivor may be erased is how one of them erases
//! a sibling's account.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{CredentialSweep, EraseBlocked, EraseResidueView, ServingBases};

/// The record's file name, directly under the app's install base (the state
/// base its launch-law lock lives in). Not a 64-hex name, so no sign-out sweep
/// — which removes actor scopes and nothing else — ever reaches it.
pub const SIGN_OUT_RESIDUE_FILE: &str = "sign-out-residue.json";

/// One path the erase could not remove, with the fingerprint that lets a later
/// re-sweep tell *still the residue* from *someone's store again*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResiduePath {
    /// An actor scope (`<base>/<64-hex>/`) or, when the base itself could not
    /// be listed, that base — exactly `EraseSweep::survivors`' two shapes.
    pub path: PathBuf,
    /// The path's [`TreeFingerprint`] when it was recorded — taken AFTER the
    /// erase gave up on it, so the erase's own partial deletes are inside it.
    /// Any later write anywhere under an actor scope (a sign-in re-creating its
    /// store, adopting the surviving `account-store/` and rewriting its db, a
    /// SQLite journal opening and closing) moves it. `None` when it could not
    /// be taken — or the record predates it — and then nothing can vouch that
    /// the path is still the residue: the re-sweep leaves it alone. Always
    /// `None` for a recorded base (this module's ⚠ rule 3, the base arm); one
    /// an older record carries is ignored.
    #[serde(default)]
    pub fingerprint: Option<TreeFingerprint>,
}

/// What a recorded path looked like, so a re-sweep can tell *still the
/// residue* from *someone's store again* (this module's ⚠ rule 3).
///
/// Taken for an **actor scope** only, over the whole tree under it: every
/// entry's relative name, kind, length and modification time, walked in name
/// order without following a symlink. A write anywhere below moves it — a
/// rewritten file its length or mtime, an entry created or removed its parent's
/// mtime and the entry list. A recorded **base** gets none: the sign-out could
/// not list it, so there is no tree to take, and the base's own entry — once
/// used here — does not move when a sibling's sign-in writes inside a scope
/// below it (the base arm of this module's ⚠ rule 3). The walk runs at
/// sign-out and at a signed-out launch that finds a record — over a residue,
/// which is small by definition; a scope past [`FINGERPRINT_MAX_ENTRIES`] gets
/// no fingerprint (and so is never re-erased) rather than an unbounded walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeFingerprint {
    /// Entries covered, the recorded path itself included — for the log.
    pub entries: u64,
    /// FNV-1a over every covered entry — a fixed function, so a record written
    /// by one build compares equal under the next (`std`'s `DefaultHasher`
    /// promises no such thing).
    pub digest: u64,
}

/// The most entries a scope's [`TreeFingerprint`] walks before it gives up.
pub const FINGERPRINT_MAX_ENTRIES: u64 = 100_000;

/// The residue a sign-out left, as the device remembers it. Empty is clean, and
/// a clean record is never kept on disk ([`Self::save`]).
///
/// Holds the survivor **paths** and credential **key names** because the
/// re-sweep needs them; they stay on this device, in the log and this file,
/// and never reach the user, who is shown [`Self::view`] and nothing else
/// (`account-scoping.md` § Erasure follows scope → *the count goes to the user;
/// the paths go to the log*).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SignOutResidue {
    /// The filesystem half: what the account-scope sweep could not remove.
    #[serde(default)]
    pub paths: Vec<ResiduePath>,
    /// The credential half's read-back survivors (`CredentialSweep::survivors`).
    #[serde(default)]
    pub credential_keys: Vec<String>,
    /// Whether a wholesale namespace wipe failed (`CredentialSweep::wipe_failed`)
    /// — the one credential residue a read-back cannot see.
    #[serde(default)]
    pub credential_wipe_failed: bool,
}

impl SignOutResidue {
    /// Record an erase's outcome: the filesystem sweep's survivors (fingerprinted
    /// now) and the credential sweep.
    #[must_use]
    pub fn record(survivors: &[PathBuf], credentials: &CredentialSweep) -> Self {
        let mut paths: Vec<ResiduePath> = Vec::new();
        for path in survivors {
            // A survivor is a LOCATION (`EraseSweep::absorb`'s rule): one path
            // recorded twice would be counted twice on the user's line.
            if paths.iter().any(|p| &p.path == path) {
                continue;
            }
            paths.push(ResiduePath {
                path: path.clone(),
                fingerprint: fingerprint(path),
            });
        }
        Self {
            paths,
            credential_keys: credentials.survivors.clone(),
            credential_wipe_failed: credentials.wipe_failed,
        }
    }

    /// The credential half as the sweep type the registry's read-back takes.
    pub fn credentials(&self) -> CredentialSweep {
        CredentialSweep {
            survivors: self.credential_keys.clone(),
            wipe_failed: self.credential_wipe_failed,
        }
    }

    /// What the user is told — the shared projection's input.
    #[must_use]
    pub fn view(&self) -> EraseResidueView {
        EraseResidueView::from_survivor_count(self.paths.len())
            .with_credentials(&self.credentials())
    }

    /// Whether anything of the user's is still on the device — the render gate
    /// for the whole `sign-out-residue` view.
    #[must_use]
    pub fn owes_work(&self) -> bool {
        self.view().owes_work()
    }

    /// The record under `state_base`, or `None` when there is none — or when it
    /// cannot be read, which is logged: an unreadable record is a residue the
    /// device has forgotten, and the paths it named are still in the log the
    /// sign-out wrote.
    #[must_use]
    pub fn load(state_base: &Path) -> Option<Self> {
        let file = state_base.join(SIGN_OUT_RESIDUE_FILE);
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!("[sign-out residue] {} unreadable: {e}", file.display());
                return None;
            }
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(record) => Some(record),
            Err(e) => {
                tracing::warn!("[sign-out residue] {} malformed: {e}", file.display());
                None
            }
        }
    }

    /// Persist the record under `state_base` — or, when it is clean, remove the
    /// file: a clean record on disk would be a residue the next launch re-checks
    /// for nothing. Written through a temp file and a rename, so a crash leaves
    /// the old record or the new one, never half of either.
    pub fn save(&self, state_base: &Path) -> std::io::Result<()> {
        let file = state_base.join(SIGN_OUT_RESIDUE_FILE);
        if !self.owes_work() {
            return match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        std::fs::create_dir_all(state_base)?;
        let tmp = state_base.join(format!("{SIGN_OUT_RESIDUE_FILE}.tmp"));
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &file)
    }

    /// The accounts the recorded paths belong to: each actor scope's own name,
    /// and every scope under a recorded base that can be listed now.
    fn accounts(&self) -> Vec<String> {
        let mut actors: Vec<String> = Vec::new();
        let mut push = |actor: String| {
            if !actors.contains(&actor) {
                actors.push(actor);
            }
        };
        for residue in &self.paths {
            match scope_actor(&residue.path) {
                Some(actor) => push(actor),
                None => {
                    let mut under = fauna_account_store::db::account_scopes_under(&residue.path);
                    under.sort();
                    under.into_iter().for_each(&mut push);
                }
            }
        }
        actors
    }

    /// **May the recorded residue be re-swept now?** `Some` names the accounts
    /// another instance — of this app or a sibling app — is serving, and the
    /// caller must erase nothing. The same probe a sign-out asks
    /// ([`crate::sign_out_blocked`]), asked about exactly the accounts this
    /// record would reach.
    #[must_use]
    pub fn retry_blocked(&self, bases: ServingBases<'_>) -> Option<EraseBlocked> {
        let served = crate::actors_served_by_another_instance(bases, &self.accounts());
        (!served.is_empty()).then_some(EraseBlocked { accounts: served })
    }

    /// Re-remove every recorded scope that is still the residue it was, and
    /// return what is left of the filesystem half — fingerprinted afresh.
    ///
    /// A path that is gone is done. A scope whose fingerprint moved is dropped
    /// from the record WITHOUT being touched (this module's ⚠ rule 3); one
    /// still the residue is removed whole. A recorded base is never erased
    /// under (rule 3's base arm): it stays recorded while it still cannot be
    /// listed or still holds an actor scope, and is done once it lists and
    /// holds none.
    #[must_use]
    fn resweep_paths(&self) -> Vec<PathBuf> {
        let mut survivors: Vec<PathBuf> = Vec::new();
        for residue in &self.paths {
            let path = &residue.path;
            if std::fs::symlink_metadata(path).is_err() {
                tracing::info!("[sign-out residue] {} is gone", path.display());
                continue;
            }
            if scope_actor(path).is_none() {
                match fauna_account_store::db::list_account_scopes(path) {
                    Ok(under) if under.is_empty() => {
                        tracing::info!(
                            "[sign-out residue] base {} lists again and holds no account scope",
                            path.display()
                        );
                    }
                    Ok(under) => {
                        tracing::warn!(
                            "[sign-out residue] base {} still holds {} account scope(s) — a \
                             re-sweep never erases under a base the sign-out could not look \
                             into; the next sign-out's own sweep does",
                            path.display(),
                            under.len()
                        );
                        survivors.push(path.clone());
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[sign-out residue] base {} still cannot be listed — {e}",
                            path.display()
                        );
                        survivors.push(path.clone());
                    }
                }
                continue;
            }
            if residue.fingerprint.is_none() || fingerprint(path) != residue.fingerprint {
                tracing::warn!(
                    "[sign-out residue] {} changed since the sign-out recorded it, or its \
                     fingerprint could not be taken — left alone and no longer tracked (a \
                     later sign-in may be using it)",
                    path.display()
                );
                continue;
            }
            match std::fs::remove_dir_all(path) {
                Ok(()) => tracing::info!("[sign-out residue] removed {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(
                        "[sign-out residue] {} SURVIVED the re-sweep — {e}",
                        path.display()
                    );
                    survivors.push(path.clone());
                }
            }
        }
        survivors
    }
}

/// What a residue retry did — see [`retry_sign_out_residue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidueRetry {
    /// Another instance serves an account the residue belongs to; nothing was
    /// touched. The record stands as it was.
    Blocked(EraseBlocked),
    /// The re-sweep ran; this is what is left. Clean means the device is clean.
    Swept(SignOutResidue),
}

/// The one residue retry — the `sign-out-residue-retry-button` and the silent
/// launch re-check both run exactly this.
///
/// `erase_credentials` is the seat's own credential erase — the same one its
/// sign-out ran (`fauna_credential_store::erase_all_credentials` on linux/tui,
/// `FfiAccountRegistry::clear_all` on the FFI seats) — handed the recorded
/// credential sweep so it can fold a read-back of the recorded keys into its
/// answer. It is called **only** when the recorded credential half is not
/// clean (this module's ⚠ rule 4), and only once the serving gate has passed.
pub fn retry_sign_out_residue(
    record: &SignOutResidue,
    bases: ServingBases<'_>,
    erase_credentials: impl FnOnce(CredentialSweep) -> CredentialSweep,
) -> ResidueRetry {
    if let Some(blocked) = record.retry_blocked(bases) {
        tracing::warn!(
            "[sign-out residue] retry REFUSED — another Fauna instance still serves {}",
            blocked.accounts.join(", ")
        );
        return ResidueRetry::Blocked(blocked);
    }
    let survivors = record.resweep_paths();
    let recorded = record.credentials();
    let credentials = if recorded.is_clean() {
        recorded
    } else {
        erase_credentials(recorded)
    };
    ResidueRetry::Swept(SignOutResidue::record(&survivors, &credentials))
}

/// The actor a recorded path is the scope of, when it is one: a directory named
/// by a canonical actor id under its parent. `None` for a base.
fn scope_actor(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let canonical = fauna_account_store::db::actor_state_dir(parent, name).ok()?;
    (canonical == path).then(|| name.to_string())
}

/// `path`'s [`TreeFingerprint`] — the one function the record and the
/// re-sweep both take it with. `None` when `path` is a base rather than an
/// actor scope (rule 3's base arm: nothing vouches for a base), cannot be read
/// at all, or holds more than [`FINGERPRINT_MAX_ENTRIES`] entries. An
/// unreadable directory or entry below a scope is folded in as a marker, so it
/// compares equal while it stays unreadable and moves the moment it does not.
fn fingerprint(path: &Path) -> Option<TreeFingerprint> {
    scope_actor(path)?;
    let root = std::fs::symlink_metadata(path).ok()?;
    let mut fp = Fnv::new();
    fp.entry(Path::new(""), Some(&root));
    let mut entries: u64 = 1;
    if root.is_dir() {
        // Explicit stack, children pushed in reverse name order: a
        // deterministic depth-first walk with no recursion to overflow.
        let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            let mut children: Vec<std::ffi::OsString> = match std::fs::read_dir(path.join(&rel)) {
                Ok(listing) => {
                    let mut names = Vec::new();
                    for entry in listing {
                        match entry {
                            Ok(entry) => names.push(entry.file_name()),
                            Err(_) => fp.marker(&rel, b"unlistable-entry"),
                        }
                    }
                    names
                }
                Err(_) => {
                    fp.marker(&rel, b"unreadable");
                    continue;
                }
            };
            children.sort();
            for name in children.into_iter().rev() {
                entries += 1;
                if entries > FINGERPRINT_MAX_ENTRIES {
                    return None;
                }
                let child = rel.join(&name);
                let meta = std::fs::symlink_metadata(path.join(&child)).ok();
                fp.entry(&child, meta.as_ref());
                if meta.is_some_and(|m| m.is_dir()) {
                    stack.push(child);
                }
            }
        }
    }
    Some(TreeFingerprint {
        entries,
        digest: fp.0,
    })
}

/// 64-bit FNV-1a, spelled out so the digest is the same function in every build.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// One entry: its relative name, then its kind, length and mtime — or a
    /// marker when its metadata could not be read.
    fn entry(&mut self, rel: &Path, meta: Option<&std::fs::Metadata>) {
        self.bytes(rel.as_os_str().as_encoded_bytes());
        self.bytes(&[0]);
        let Some(meta) = meta else {
            self.bytes(b"unreadable\0");
            return;
        };
        let kind: &[u8] = if meta.is_dir() {
            b"d"
        } else if meta.is_symlink() {
            b"l"
        } else if meta.is_file() {
            b"f"
        } else {
            b"o"
        };
        self.bytes(kind);
        self.bytes(&meta.len().to_le_bytes());
        match meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        {
            Some(since) => self.bytes(&since.as_nanos().to_le_bytes()),
            None => self.bytes(b"no-mtime"),
        }
        self.bytes(&[0]);
    }

    fn marker(&mut self, rel: &Path, what: &[u8]) {
        self.bytes(rel.as_os_str().as_encoded_bytes());
        self.bytes(&[0]);
        self.bytes(what);
        self.bytes(&[0]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_lock::{ServingMode, SessionInstanceHolder};
    use crate::sign_out_residue_retry_blocked_copy;

    fn actor(pair: &str) -> String {
        pair.repeat(32)
    }

    /// An actor scope holding one file — what a sign-out leaves when it fails.
    fn scope(base: &Path, pair: &str) -> PathBuf {
        let dir = base.join(actor(pair));
        std::fs::create_dir_all(&dir).expect("scope dir");
        std::fs::write(dir.join("mls_state.db"), b"the user's data").expect("scope file");
        dir
    }

    fn no_bases() -> ServingBases<'static> {
        ServingBases {
            state_base: None,
            store_root: None,
        }
    }

    fn dirty_credentials() -> CredentialSweep {
        CredentialSweep {
            survivors: vec!["fauna/aa11/secret".to_string()],
            wipe_failed: false,
        }
    }

    fn swept(retry: ResidueRetry) -> SignOutResidue {
        match retry {
            ResidueRetry::Swept(record) => record,
            ResidueRetry::Blocked(b) => panic!("expected a sweep, was refused for {b:?}"),
        }
    }

    #[test]
    fn a_record_survives_a_save_and_load_and_a_clean_one_leaves_no_file() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a1");
        let record = SignOutResidue::record(&[dir], &dirty_credentials());
        record.save(base.path()).expect("save");
        assert_eq!(SignOutResidue::load(base.path()), Some(record));

        SignOutResidue::default()
            .save(base.path())
            .expect("a clean save removes the file");
        assert!(
            !base.path().join(SIGN_OUT_RESIDUE_FILE).exists(),
            "a clean record on disk would be re-checked at every launch for nothing"
        );
        assert_eq!(SignOutResidue::load(base.path()), None);
    }

    #[test]
    fn a_malformed_record_reads_as_none_rather_than_failing_the_launch() {
        let base = tempfile::tempdir().expect("base");
        std::fs::write(base.path().join(SIGN_OUT_RESIDUE_FILE), b"{not json").expect("write");
        assert_eq!(SignOutResidue::load(base.path()), None);
    }

    /// A record without a fingerprint — one written before the fingerprint
    /// covered the tree, carrying the retired `modified` field — still loads
    /// (the launch never fails on it), and nothing vouches for its paths, so
    /// they are left alone; its credential half is still re-checked.
    #[test]
    fn a_record_without_a_fingerprint_loads_and_erases_nothing() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "e5");
        let older = serde_json::json!({
            "paths": [{ "path": dir, "modified": { "secs_since_epoch": 0, "nanos_since_epoch": 0 } }],
            "credential_keys": ["fauna/e5/secret"],
        });
        std::fs::write(
            base.path().join(SIGN_OUT_RESIDUE_FILE),
            serde_json::to_vec(&older).expect("json"),
        )
        .expect("write");
        let record = SignOutResidue::load(base.path()).expect("an older record still loads");
        assert_eq!(record.paths[0].fingerprint, None);

        let mut erased = false;
        let after = swept(retry_sign_out_residue(&record, no_bases(), |_| {
            erased = true;
            CredentialSweep::default()
        }));
        assert!(
            dir.join("mls_state.db").exists(),
            "an unvouched path is never re-erased"
        );
        assert!(erased, "the recorded credential half is still re-checked");
        assert!(!after.owes_work());
    }

    /// One location recorded twice is one item on the user's line.
    #[test]
    fn a_path_recorded_twice_is_counted_once() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a1");
        let record = SignOutResidue::record(&[dir.clone(), dir], &CredentialSweep::default());
        assert_eq!(record.view().survivors, 1);
    }

    /// The retry's whole point: the residue that survived goes once it can.
    #[test]
    fn a_retry_removes_a_recorded_scope_that_will_now_go() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a1");
        let record =
            SignOutResidue::record(std::slice::from_ref(&dir), &CredentialSweep::default());

        let after = swept(retry_sign_out_residue(&record, no_bases(), |_| {
            panic!("a clean credential half must not be re-erased")
        }));
        assert!(!dir.exists(), "the recorded scope must be gone");
        assert!(
            !after.owes_work(),
            "and the record must come back clean: {after:?}"
        );
    }

    /// A scope that still will not go stays recorded — the line stays up.
    #[cfg(unix)]
    #[test]
    fn a_scope_that_still_will_not_go_stays_recorded() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a1");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let record =
            SignOutResidue::record(std::slice::from_ref(&dir), &CredentialSweep::default());

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
        if dir.join("mls_state.db").exists() {
            assert_eq!(after.paths.len(), 1, "the survivor must still be reported");
            assert_eq!(after.paths[0].path, dir);
        } else {
            // Root writes through 0o555 — the fault never happened.
            eprintln!("skipped: this process writes through a read-only directory");
        }
    }

    /// ⚠ The rule that makes a launch-time re-sweep safe on a SHARED store
    /// root: a directory written since the sign-out recorded it is someone's
    /// store again — a later sign-in, possibly a sibling app's, not running now
    /// so no serving lock can see it — and is left alone.
    #[test]
    fn a_scope_changed_since_it_was_recorded_is_left_alone_and_forgotten() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a1");
        let record =
            SignOutResidue::record(std::slice::from_ref(&dir), &CredentialSweep::default());
        // A later sign-in re-creating its store beside what survived.
        std::fs::create_dir_all(dir.join("account-store")).expect("a later sign-in's store");

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        assert!(
            dir.join("mls_state.db").exists(),
            "a store written since the sign-out must never be re-erased"
        );
        assert!(
            !after.owes_work(),
            "and it is no longer this sign-out's residue to report"
        );
    }

    /// ⚠ Rule 3 one level down: the shared-root store lives in
    /// `<root>/<actor>/account-store/`, and a later sign-in that adopts a
    /// surviving one (the lost-slot self-heal) writes only inside that
    /// subdirectory — the recorded scope directory's own mtime never moves.
    /// The fingerprint must see that write all the same.
    #[test]
    fn a_scope_written_only_inside_its_account_store_is_left_alone_and_forgotten() {
        let base = tempfile::tempdir().expect("base");
        let dir = base.path().join(actor("d4"));
        let store = dir.join("account-store");
        std::fs::create_dir_all(&store).expect("store dir");
        let db = store.join("account-store.db");
        std::fs::write(&db, b"the user's rows").expect("db");
        let record =
            SignOutResidue::record(std::slice::from_ref(&dir), &CredentialSweep::default());

        // A real sign-in's footprint, nothing hand-set: the db rewritten and a
        // journal opened and closed beside it.
        std::fs::write(&db, b"the user's rows, and one not yet pushed").expect("rewrite");
        let wal = store.join("account-store.db-wal");
        std::fs::write(&wal, b"wal").expect("wal");
        std::fs::remove_file(&wal).expect("wal gone");

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        assert!(
            db.exists(),
            "a store adopted since the sign-out must never be re-erased"
        );
        assert!(
            !after.owes_work(),
            "and it is no longer this sign-out's residue to report: {after:?}"
        );
    }

    /// ⚠ The dcccvi rule, asked the sign-out's way: a recorded scope whose
    /// account a sibling instance serves refuses the whole retry.
    #[test]
    fn a_retry_refuses_while_a_sibling_serves_a_recorded_account() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "b2");
        let record = SignOutResidue::record(std::slice::from_ref(&dir), &dirty_credentials());

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(
            Some(base.path()),
            &actor("b2"),
            None,
            ServingMode::Concurrent,
        );
        assert!(sibling.holds_lock(), "the sibling is genuinely serving it");

        let bases = ServingBases {
            state_base: Some(base.path()),
            store_root: None,
        };
        let retry = retry_sign_out_residue(&record, bases, |_| {
            panic!("a refused retry must not touch the credentials either")
        });
        assert_eq!(
            retry,
            ResidueRetry::Blocked(EraseBlocked {
                accounts: vec![actor("b2")]
            })
        );
        assert!(dir.join("mls_state.db").exists(), "nothing may be erased");
    }

    /// A base the sign-out could not even list carries no fingerprint and is
    /// never erased under by a re-sweep: the scope under it stays, and so does
    /// the base on the record — the line stays up until the next sign-out's
    /// own sweep takes the scope. Once nothing is under it, it is done.
    #[test]
    fn a_recorded_base_erases_nothing_and_stays_recorded_until_it_holds_no_scope() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "c3");
        let log = base.path().join("fauna.log");
        std::fs::write(&log, b"install-scoped").expect("log");
        let record =
            SignOutResidue::record(&[base.path().to_path_buf()], &CredentialSweep::default());
        assert_eq!(
            record.paths[0].fingerprint, None,
            "nothing vouches for a base"
        );

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        assert!(
            dir.join("mls_state.db").exists(),
            "a re-sweep never erases under a recorded base"
        );
        assert!(log.exists(), "install-scoped state is not residue");
        assert_eq!(
            after.paths.len(),
            1,
            "the base stays on the line: {after:?}"
        );
        assert_eq!(after.paths[0].path, base.path());

        // The next sign-out's own sweep took the scope; the base is done.
        std::fs::remove_dir_all(&dir).expect("the sign-out's sweep");
        let after = swept(retry_sign_out_residue(&after, no_bases(), |c| c));
        assert!(
            !after.owes_work(),
            "a base holding no scope is done: {after:?}"
        );
        assert!(
            log.exists(),
            "and still nothing beside the scopes was touched"
        );
    }

    /// An older record carrying a base's own-entry fingerprint still loads,
    /// and the fingerprint grants nothing: the base arm ignores it.
    #[test]
    fn an_older_records_base_fingerprint_vouches_for_nothing() {
        let base = tempfile::tempdir().expect("base");
        let dir = scope(base.path(), "a7");
        let older = serde_json::json!({
            "paths": [{ "path": base.path(), "fingerprint": { "entries": 1, "digest": 7 } }],
        });
        std::fs::write(
            base.path().join(SIGN_OUT_RESIDUE_FILE),
            serde_json::to_vec(&older).expect("json"),
        )
        .expect("write");
        let record = SignOutResidue::load(base.path()).expect("an older record still loads");

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        assert!(
            dir.join("mls_state.db").exists(),
            "never erased under a base"
        );
        assert_eq!(after.paths.len(), 1, "and still reported: {after:?}");
        assert_eq!(after.paths[0].fingerprint, None, "re-recorded without one");
    }

    /// ⚠ Rule 3 for a recorded BASE: the sign-out could not look into it, so
    /// nothing recorded then can vouch for a scope under it, and a sibling's
    /// sign-in that adopts a surviving `<base>/<actor>/account-store/` adds no
    /// entry in the base itself. A re-sweep must never erase under a recorded
    /// base — this one would have taken an adopted store, un-pushed rows and
    /// all.
    #[test]
    fn a_recorded_base_with_an_adopted_scope_below_keeps_the_store() {
        let base = tempfile::tempdir().expect("base");
        let dir = base.path().join(actor("f6"));
        let store = dir.join("account-store");
        std::fs::create_dir_all(&store).expect("store dir");
        let db = store.join("account-store.db");
        std::fs::write(&db, b"the user's rows").expect("db");
        let record =
            SignOutResidue::record(&[base.path().to_path_buf()], &CredentialSweep::default());

        // A sibling's adopting sign-in: the db rewritten and a journal opened
        // and closed beside it — two levels below the base.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&db, b"the user's rows, and one not yet pushed").expect("rewrite");
        let wal = store.join("account-store.db-wal");
        std::fs::write(&wal, b"wal").expect("wal");
        std::fs::remove_file(&wal).expect("wal gone");

        let after = swept(retry_sign_out_residue(&record, no_bases(), |c| c));
        assert!(
            db.exists(),
            "a recorded base's re-sweep erased an adopted store"
        );
        assert_eq!(
            after
                .paths
                .iter()
                .map(|p| p.path.clone())
                .collect::<Vec<_>>(),
            vec![base.path().to_path_buf()],
            "the base still holds a scope, so it stays on the line: {after:?}"
        );
    }

    /// The credential half is re-erased when — and only when — it survived,
    /// and the seat's answer replaces the recorded one.
    #[test]
    fn surviving_credentials_are_re_erased_and_the_answer_is_kept() {
        let record = SignOutResidue::record(&[], &dirty_credentials());
        let mut handed = None;
        let after = swept(retry_sign_out_residue(&record, no_bases(), |recorded| {
            handed = Some(recorded);
            CredentialSweep::default()
        }));
        assert_eq!(
            handed,
            Some(dirty_credentials()),
            "the seat must be handed the recorded keys to read back"
        );
        assert!(!after.owes_work(), "a clean re-erase is a clean record");
    }

    #[test]
    fn the_retry_refusal_line_resolves_in_the_shipped_catalog() {
        let line = sign_out_residue_retry_blocked_copy().resolve(fauna_i18n::strings::lookup);
        assert!(
            !line.contains("sign_out_residue_retry_blocked_other_window"),
            "the key must resolve, not echo: {line}"
        );
    }
}
