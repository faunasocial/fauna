//! The one restore decision (`writer-signed-change-records.md` § Writer-signed
//! change records, ruling (10)(a)/(b)/(f)): what a door that is about to put
//! the current identity's signature over a version's manifest may do with it.
//!
//! Three doors share it — the Media restore (`MediaMachine::restore_version`),
//! the sync agent's verb (`fauna-sync-agent`'s `versions::restore_file_version`)
//! and the conflict review list (`DevicesMachine::use_other_version`, and the
//! choose-winner beside it) — because the three already share the record
//! ([`crate::SyncClient::restore_version`]), and a rule each re-derived is how
//! the third door came to have none.
//!
//! The conflict surface additionally names its version through a row no one
//! signs (`fauna.sync.conflicts.list`), so it first finds the version a
//! candidate names in the judged history ([`JudgedCandidate::find`]) and
//! records the VERSION's signed fields, never the candidate row's.

use fauna_protocol::files::FileVersionInfo;
use fauna_protocol::folders::SyncConflict;
use fauna_protocol::sync_row_verify::RowVerdict;

pub use fauna_core::restore_branch::{RestoreDecision, listed_restore_decision};

/// The decision over one judged version: a row that is not a version (refused
/// or held) is refused; an admitted or a history version (ruling (11)(c),
/// (11)(f) — stamped it re-points verbatim, unstamped it rests under its
/// signer's owner root and needs the re-seal) takes the listed table
/// ([`listed_restore_decision`], whose inputs it shares).
pub fn restore_decision(
    verdict: &RowVerdict,
    content_key_version: Option<u64>,
    current_vouches: bool,
) -> RestoreDecision {
    if !(verdict.admits() || verdict.is_history()) {
        return RestoreDecision::Refuse;
    }
    listed_restore_decision(content_key_version, current_vouches)
}

/// The version a conflict candidate names, found in the judged history of the
/// candidate's set and path (ruling (10)(a)).
#[derive(Debug, Clone, PartialEq)]
pub struct JudgedCandidate {
    /// The picked version, its fields as the writer signed them.
    pub version: FileVersionInfo,
    pub verdict: RowVerdict,
    /// Whether the current identity signed this version or another admitted
    /// row of the listing naming the same manifest ([`restore_decision`]).
    pub current_vouches: bool,
}

impl JudgedCandidate {
    /// Find the version naming `manifest_hex` in `listing` — the judged
    /// history ([`crate::SyncClient::versions_list_judged`], soft-pruned
    /// versions included) looked up under `path_hash`, the hash of the very
    /// path the record will carry. A row of another path (a version's
    /// signature covers its own `path_hash`, so a nest serving a foreign row
    /// under this hash serves one that names another path) and a row that is
    /// not a version are never picked. `None`: the candidate has no version
    /// here, so it is not a candidate (ruling (3), read for this surface).
    ///
    /// Where several versions carry the manifest the pick is one signed as
    /// the current identity, else a stamped one, else the latest.
    pub fn find(
        listing: Vec<(FileVersionInfo, RowVerdict)>,
        path_hash: [u8; 32],
        manifest_hex: &str,
        own: Option<&[u8; 32]>,
    ) -> Option<Self> {
        let manifest = fauna_core::hex32::decode(manifest_hex).ok()?;
        let mut matching: Vec<(FileVersionInfo, RowVerdict)> = listing
            .into_iter()
            .filter(|(v, verdict)| {
                v.manifest_hash.as_ref() == &manifest[..]
                    && v.path_hash.as_ref() == &path_hash[..]
                    && (verdict.admits() || verdict.is_history())
            })
            .collect();
        let current_vouches = matching
            .iter()
            .any(|(_, verdict)| verdict.admits() && verdict.signed_as(own));
        // Rank: signed as current, then stamped, then the latest version.
        matching.sort_by_key(|(v, verdict)| {
            (
                verdict.signed_as(own),
                v.content_key_version.is_some(),
                v.version_num,
            )
        });
        let (version, verdict) = matching.pop()?;
        Some(Self {
            version,
            verdict,
            current_vouches,
        })
    }

    /// [`restore_decision`] over the picked version.
    pub fn decision(&self) -> RestoreDecision {
        restore_decision(
            &self.verdict,
            self.version.content_key_version,
            self.current_vouches,
        )
    }

    /// Whether a choose-winner of `conflict` keeping this version may be
    /// signed (ruling (10)(f)): the decision is [`RestoreDecision::Verbatim`],
    /// the conflict's row addresses the path the version was found under, and
    /// the candidate the nest will mint the head from carries the version's
    /// signed device, size and generation. Anything else is refused on the
    /// device — the nest mints from its own candidate row, which would then
    /// not be the version that verified.
    pub fn vouches_for_winner(&self, conflict: &SyncConflict) -> bool {
        if self.decision() != RestoreDecision::Verbatim
            || conflict.path_hash.as_ref() != self.version.path_hash.as_ref()
        {
            return false;
        }
        let manifest = hex::encode(&self.version.manifest_hash);
        let Some(device) = self.version.device_id.as_ref() else {
            return false;
        };
        let device = hex::encode(device);
        conflict.candidates.iter().any(|c| {
            c.manifest_hash.eq_ignore_ascii_case(&manifest)
                && c.device_id.eq_ignore_ascii_case(&device)
                && c.size_bytes == self.version.size_bytes
                && c.content_key_version == self.version.content_key_version
        })
    }
}

/// Why a judged choose-winner ([`crate::SyncClient::conflicts_resolve_judged`])
/// did not resolve.
#[derive(Debug)]
pub enum ChooseWinnerError<E> {
    /// The judged history does not vouch for the candidate as the conflict
    /// row names it ([`JudgedCandidate::vouches_for_winner`]); nothing was
    /// sent.
    NotVouched,
    /// The resolve itself failed.
    Rpc(E),
}

impl<E: core::fmt::Display> core::fmt::Display for ChooseWinnerError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotVouched => f.write_str(
                "that version is not the one in this file's verified history — nothing was resolved",
            ),
            Self::Rpc(e) => e.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::encoding::AuthoringOrigin;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::folders::ConflictCandidate;
    use fauna_protocol::sync_row_verify::Held;
    use fauna_protocol::sync_writer_sig::ChangeVerifyError;

    const OWN: [u8; 32] = [1; 32];
    const OTHER: [u8; 32] = [2; 32];

    fn verified(signed_as: [u8; 32]) -> RowVerdict {
        RowVerdict::Verified {
            writer: signed_as,
            signed_as,
            origin: AuthoringOrigin::Direct,
        }
    }

    fn history() -> RowVerdict {
        RowVerdict::History {
            writer: OTHER,
            signed_as: OTHER,
            origin: AuthoringOrigin::Direct,
            nonce: [9; 32],
        }
    }

    // ── The decision table: one test per arm (ruling (10)(b), (11)(f)). ──

    #[test]
    fn signed_as_current_is_verbatim() {
        assert_eq!(
            restore_decision(&verified(OWN), None, true),
            RestoreDecision::Verbatim
        );
    }

    #[test]
    fn another_signers_stamped_version_is_verbatim() {
        assert_eq!(
            restore_decision(&verified(OTHER), Some(3), false),
            RestoreDecision::Verbatim
        );
    }

    #[test]
    fn another_signers_unstamped_version_needs_the_reseal() {
        assert_eq!(
            restore_decision(&verified(OTHER), None, false),
            RestoreDecision::NeedsReseal
        );
    }

    #[test]
    fn an_exempt_unstamped_version_needs_the_reseal() {
        assert_eq!(
            restore_decision(&RowVerdict::Exempt, None, false),
            RestoreDecision::NeedsReseal
        );
    }

    #[test]
    fn a_stamped_history_version_is_verbatim() {
        assert_eq!(
            restore_decision(&history(), Some(2), false),
            RestoreDecision::Verbatim
        );
    }

    #[test]
    fn an_unstamped_history_version_needs_the_reseal() {
        assert_eq!(
            restore_decision(&history(), None, false),
            RestoreDecision::NeedsReseal
        );
    }

    #[test]
    fn a_refused_or_held_row_is_no_version() {
        let refused = RowVerdict::Refused(ChangeVerifyError::NotAWriter);
        assert_eq!(
            restore_decision(&refused, Some(1), true),
            RestoreDecision::Refuse
        );
        let held = RowVerdict::Held(Held::RosterUnread);
        assert_eq!(restore_decision(&held, None, true), RestoreDecision::Refuse);
    }

    // ── The lookup (ruling (10)(a)). ──

    fn path() -> [u8; 32] {
        fauna_core::sync::path_hash("docs/a.txt")
    }

    fn version(num: i64, manifest: u8, stamp: Option<u64>) -> FileVersionInfo {
        FileVersionInfo {
            path_hash: ByteBuf::from(path().to_vec()),
            version_num: num,
            manifest_hash: ByteBuf::from(vec![manifest; 32]),
            size_bytes: 10 * num,
            content_key_version: stamp,
            device_id: Some(ByteBuf::from(vec![5; 32])),
            ..Default::default()
        }
    }

    fn hex_of(manifest: u8) -> String {
        hex::encode([manifest; 32])
    }

    #[test]
    fn a_candidate_with_no_version_is_no_candidate() {
        let listing = vec![(version(1, 0xaa, None), verified(OWN))];
        assert!(JudgedCandidate::find(listing, path(), &hex_of(0xbb), Some(&OWN)).is_none());
    }

    #[test]
    fn a_refused_version_is_never_picked() {
        let listing = vec![(
            version(1, 0xaa, None),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter),
        )];
        assert!(JudgedCandidate::find(listing, path(), &hex_of(0xaa), Some(&OWN)).is_none());
    }

    #[test]
    fn a_version_of_another_path_is_never_picked() {
        let mut foreign = version(1, 0xaa, None);
        foreign.path_hash = ByteBuf::from(fauna_core::sync::path_hash("other").to_vec());
        let listing = vec![(foreign, verified(OWN))];
        assert!(JudgedCandidate::find(listing, path(), &hex_of(0xaa), Some(&OWN)).is_none());
    }

    #[test]
    fn the_pick_prefers_current_then_stamped_then_latest() {
        let listing = vec![
            (version(1, 0xaa, None), verified(OWN)),
            (version(2, 0xaa, Some(4)), verified(OTHER)),
            (version(3, 0xaa, None), verified(OTHER)),
        ];
        let current = JudgedCandidate::find(listing.clone(), path(), &hex_of(0xaa), Some(&OWN))
            .expect("found");
        assert_eq!(current.version.version_num, 1);
        let stamped =
            JudgedCandidate::find(listing.clone(), path(), &hex_of(0xaa), None).expect("found");
        assert_eq!(stamped.version.version_num, 2);
        let latest = JudgedCandidate::find(listing[2..].to_vec(), path(), &hex_of(0xaa), None)
            .expect("found");
        assert_eq!(latest.version.version_num, 3);
    }

    #[test]
    fn a_current_row_of_the_same_manifest_vouches_for_another() {
        // The predecessor's unstamped version is the pick only when no current
        // row exists; here one does, and it vouches for the manifest.
        let listing = vec![
            (version(1, 0xaa, None), verified(OWN)),
            (version(2, 0xaa, None), verified(OTHER)),
        ];
        let found =
            JudgedCandidate::find(listing, path(), &hex_of(0xaa), Some(&OWN)).expect("found");
        assert!(found.current_vouches);
        assert_eq!(found.decision(), RestoreDecision::Verbatim);
    }

    // ── The choose-winner (ruling (10)(f)). ──

    fn conflict_for(v: &FileVersionInfo) -> SyncConflict {
        SyncConflict {
            id: 7,
            folder: "docs".into(),
            path: "docs/a.txt".into(),
            path_hash: v.path_hash.clone(),
            candidates: vec![ConflictCandidate {
                manifest_hash: hex::encode(&v.manifest_hash),
                device_id: hex::encode([5u8; 32]),
                size_bytes: v.size_bytes,
                content_key_version: v.content_key_version,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn judged(v: FileVersionInfo, verdict: RowVerdict) -> JudgedCandidate {
        JudgedCandidate::find(
            vec![(v.clone(), verdict)],
            path(),
            &hex::encode(&v.manifest_hash),
            Some(&OWN),
        )
        .expect("found")
    }

    #[test]
    fn the_winner_is_signed_when_the_row_is_the_version() {
        let v = version(1, 0xaa, None);
        assert!(judged(v.clone(), verified(OWN)).vouches_for_winner(&conflict_for(&v)));
    }

    #[test]
    fn the_winner_is_refused_when_the_row_differs_from_the_version() {
        let v = version(1, 0xaa, None);
        let found = judged(v.clone(), verified(OWN));
        for lie in [
            |c: &mut ConflictCandidate| c.size_bytes += 1,
            |c: &mut ConflictCandidate| c.content_key_version = Some(9),
            |c: &mut ConflictCandidate| c.device_id = hex::encode([6u8; 32]),
        ] {
            let mut conflict = conflict_for(&v);
            lie(&mut conflict.candidates[0]);
            assert!(!found.vouches_for_winner(&conflict));
        }
        let mut moved = conflict_for(&v);
        moved.path_hash = ByteBuf::from(fauna_core::sync::path_hash("elsewhere").to_vec());
        assert!(!found.vouches_for_winner(&moved));
    }

    #[test]
    fn the_winner_is_refused_when_the_version_needs_the_reseal() {
        let v = version(1, 0xaa, None);
        assert!(!judged(v.clone(), verified(OTHER)).vouches_for_winner(&conflict_for(&v)));
    }
}
