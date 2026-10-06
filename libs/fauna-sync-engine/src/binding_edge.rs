//! **Re-resolve edges** — a resident engine's content keys are final at build,
//! so a host that keeps one resident re-resolves them on its own evidence
//! (`on-demand-files.md` § Shared sets on a capability host, decision 2): the
//! set's row says the binding, the caller's access or the owner-stamped
//! content-key floor moved, or the floor is still ahead of the generation the
//! engine holds.
//!
//! Always compiled (unlike [`crate::engine_lifecycle`], which pulls in the custody
//! graph): the **pre-seal hold** lives in the engine's one seal-root resolver, so
//! every host that runs a [`crate::engine::SyncEngine`] — the control-inverted
//! File Provider host and the desktop sync agent's resident loop alike — holds a
//! write behind the floor through the same code, and the resident loop's per-tick
//! row read ([`crate::engine::SyncEngine::refresh_sync_mode`]) is the refresh edge
//! the agent's re-resolve hangs off.

use std::sync::Arc;

use fauna_core::folder_keys::FolderRef;
use fauna_protocol::folders::FolderSummary;

/// The facts that decide a set's content-key binding: the group binding, the
/// caller's role and access, and the owner-stamped content-key floor — and,
/// for a cross-nest set, the residency its custody record carries.
/// **Not the nest's WebDAV flag**: whether a set is served is the owner's
/// word in custody (`writer-signed-change-records.md` ruling (7)(b)(ii)
/// rule (2)), so a flag flip decides no binding, and a serve-window move
/// reaches a resident engine through the custody re-read that rebuilds its
/// keys (`FolderEngineKeys` carries the stamps). An engine's keys are final at build, so a host that keeps
/// one resident compares this against what it reads now (`on-demand-files.md`
/// § Shared sets on a capability host, decision 2).
///
/// A same-nest set's basis is its row ([`Self::of`]). A cross-nest set has no row
/// on this nest — **its custody record is the row** ([`Self::of_foreign`]): the
/// group, the home nest it relays to, the advisory access, the generation
/// custody holds, which is how a rotation reaches a resident foreign engine
/// (custody moving *is* the row moving), and the residency the home nest
/// stamped — a foreign engine's per-tick sync-mode read never carries one, so
/// a flip reaches it only here, as a rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingBasis {
    mls_group_id: Option<String>,
    role: Option<String>,
    access: Option<String>,
    content_key_floor: Option<u64>,
    /// Cross-nest only: the home nest the set's planes dial.
    home_nest_url: Option<String>,
    /// Cross-nest only: the newest generation the holder's custody carries for
    /// the set. A same-nest row never carries it (custody arriving is invisible
    /// on a row — the floor stands in), so it stays `None` there.
    custody_generation: Option<u64>,
    /// Cross-nest only: the record's residency reading (`Some(true)`
    /// metadata-only, `Some(false)` full, `None` unknown). A same-nest set's
    /// residency reaches its engine on every tick's row read
    /// (`SyncEngine::refresh_sync_mode`), so it stays `None` there.
    residency: Option<bool>,
}

impl BindingBasis {
    /// The basis `summary` carries.
    #[must_use]
    pub fn of(summary: &FolderSummary) -> Self {
        Self {
            mls_group_id: summary.mls_group_id.clone(),
            role: summary.role.clone(),
            access: summary.access.clone(),
            content_key_floor: summary.content_key_floor,
            home_nest_url: None,
            custody_generation: None,
            residency: None,
        }
    }

    /// The basis a cross-nest set's custody `record` carries, with the newest
    /// generation custody holds for it (`None` = keyless). The caller is always
    /// a member of a foreign set; its `access` is the home nest's advisory stamp
    /// (never an authorization input — the home nest's mint is), so a change is
    /// a reason to rebuild and nothing more. The record's `content_key_floor` is
    /// the home nest's stamp off the federated content-key read, so a floor
    /// move is a basis change at the host's custody edge and the pre-seal hold
    /// arms from it (`on-demand-files.md` § Shared sets on a capability host →
    /// *One mechanism*, question 2); a record without one (an unbound set, no floor held)
    /// arms nothing, and the floor is enforced where it
    /// lives — the home nest's `stale_content_key` refusal.
    #[must_use]
    pub fn of_foreign(
        record: &fauna_core::data::ForeignFolder,
        custody_generation: Option<u64>,
    ) -> Self {
        Self {
            mls_group_id: Some(hex::encode(&record.mls_group_id)),
            role: Some("member".to_string()),
            access: record.access.clone(),
            content_key_floor: record.content_key_floor,
            home_nest_url: Some(record.home_nest_url.clone()),
            custody_generation,
            residency: record.metadata_only_residency(),
        }
    }

    /// The owner-stamped floor this basis was read with.
    #[must_use]
    pub fn content_key_floor(&self) -> Option<u64> {
        self.content_key_floor
    }
}

/// Is the set's content-key `floor` ahead of the generation an engine `held`?
/// No floor established ⇒ never. A floor over an engine holding no generation at
/// all (a bound set built keyless) ⇒ ahead.
#[must_use]
pub fn floor_ahead(held: Option<u64>, floor: Option<u64>) -> bool {
    match floor {
        None => false,
        Some(floor) => held.is_none_or(|held| held < floor),
    }
}

/// What a resident host does with its engine at an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeVerdict {
    /// The engine still answers the row: keep it.
    Keep,
    /// Re-read custody and rebuild over the same state DB.
    Rebuild,
}

/// Decision 2's comparison: given the `basis` an engine was built on and the
/// generation it `held`, what does the basis read `now` call for?
///
/// - **Row gone** (`None` from a successful read — the set was deleted, the
///   caller was removed from it, or a cross-nest set's custody record is gone)
///   → rebuild, which refuses: a host never goes on serving a set it no longer
///   has a row for.
/// - **Binding, serve flag, role, access or floor moved** → rebuild.
/// - **The floor is still ahead of the generation held** → rebuild. Custody
///   arriving is not visible on the row, so a host behind the floor re-reads
///   custody at every edge until the generation is there.
/// - Otherwise keep.
///
/// A list that could not be read is the caller's to handle, not this function's
/// (it is not a row): a refresh keeps its engine, a seal refuses.
#[must_use]
pub fn edge_verdict(
    basis: &BindingBasis,
    held: Option<u64>,
    now: Option<&BindingBasis>,
) -> EdgeVerdict {
    let Some(now) = now else {
        return EdgeVerdict::Rebuild;
    };
    if now != basis || floor_ahead(held, now.content_key_floor) {
        EdgeVerdict::Rebuild
    } else {
        EdgeVerdict::Keep
    }
}

/// The row `folder_ref` names in this nest's own folder list, or `None`.
///
/// `fauna.folders.list` is this nest's own projection (owner-scoped and
/// member-visible rows alike), so every row's ref is [`FolderRef::Local`] over
/// its `id` — the shared `folder_ref_for_row` with no home nest. A
/// [`FolderRef::Foreign`] therefore matches nothing here by construction: a
/// cross-nest set has no row on this nest; its facts are its holder's custody
/// record.
///
/// Matching by identity is the whole point: two sets can share a name, and the
/// by-name lookup this replaced would have bound whichever row the list carried
/// first.
#[must_use]
pub fn summary_for_ref(list: &[FolderSummary], folder_ref: FolderRef) -> Option<&FolderSummary> {
    match folder_ref {
        FolderRef::Local(id) => list.iter().find(|fs| fs.id == id),
        FolderRef::Foreign(_) => None,
    }
}

/// Which row of this nest's folder list a seat's per-tick policy read
/// ([`crate::config::resolve_device_mode_from_nest`]) answers for.
///
/// Folder names are unique only per owner (`on-demand-files.md` § Hosting
/// multiple on-demand folders): a user who owns `docs` and is a member of
/// someone else's `docs` sees both in the member-visible list, OWNER rows
/// first. A name-keyed read therefore made a member seat adopt its user's own
/// row — and, were that row public, arm the plaintext upload arm on the shared
/// set. Every key here resolves to one row or to none, and none is the sealed
/// direction every field of the read already takes for an absent row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatRowKey {
    /// The binding's own ref — the key every by-ref host holds (the resident
    /// agent's [`BindingEdge::folder_ref`]).
    Ref(FolderRef),
    /// No binding installed (an engine built without a [`BindingEdge`]):
    /// matches nothing.
    Unbound,
}

impl SeatRowKey {
    /// The row this key names in `list`, or `None` (absent or unbound).
    #[must_use]
    pub fn find(self, list: &[FolderSummary]) -> Option<&FolderSummary> {
        match self {
            Self::Ref(folder_ref) => summary_for_ref(list, folder_ref),
            Self::Unbound => None,
        }
    }
}

/// What an engine last learned about its set's content-key floor — the input of
/// the pre-seal hold ([`crate::engine::SyncEngine::seal_hold`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SealFloor {
    /// No host armed the hold: an engine built without a row to read the floor
    /// from (a one-shot restore). Never holds.
    #[default]
    Unarmed,
    /// The floor the set's row carried at the last successful read (`None` = the
    /// owner never stamped one).
    Floor(Option<u64>),
    /// The last successful list carried no row for the set — deleted, or the
    /// caller removed from it. The floor is unknowable, so every seal is held.
    RowGone,
    /// The pre-seal row read failed, and the floor the last successful read
    /// found is kept (decision 2′): it still binds the seal — (a), a generation
    /// behind it is never sealed under — but a seal it lets through is not
    /// published to a nest until a read succeeds again — (c).
    Unread(Option<u64>),
}

impl SealFloor {
    /// The posture after a pre-seal row read failed (decision 2′): the last
    /// floor read is kept and marked unread. A row the last successful read
    /// found gone stays gone — it holds every write, in reach or not — and an
    /// unarmed engine stays unarmed.
    #[must_use]
    pub fn after_failed_read(self) -> Self {
        match self {
            Self::Floor(floor) | Self::Unread(floor) => Self::Unread(floor),
            Self::RowGone => Self::RowGone,
            Self::Unarmed => Self::Unarmed,
        }
    }
}

/// Why a seal is held rather than sealed under the generation the engine holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealHold {
    /// The owner's floor is ahead of the generation held (`None` = the engine was
    /// built keyless).
    Behind { floor: u64, held: Option<u64> },
    /// The set's row is gone from the list.
    RowGone,
    /// Publication only ([`publish_hold`]): the set's row could not be read
    /// this pass, so what was sealed under the last floor read is not sent to
    /// a nest until a read checks the floor again (decision 2′ (c)).
    Unreadable,
}

impl std::fmt::Display for SealHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Behind { floor, held } => write!(
                f,
                "the set's content keys are at generation {floor} and this host's custody \
                 holds {held} — the write is held, never sealed under an older generation, \
                 until an identity-holding app of this account brings the generation into \
                 custody",
                held = held.map_or_else(|| "none".to_string(), |v| format!("generation {v}")),
            ),
            Self::RowGone => write!(
                f,
                "the set's row is gone from the folder list (deleted, or this account was \
                 removed from it) — the write is held"
            ),
            Self::Unreadable => write!(
                f,
                "the folder list could not be read, so the set's content-key floor cannot be \
                 re-checked — the write is sealed and served to peers under the last floor \
                 read, and published to no nest until the floor can be read again"
            ),
        }
    }
}

/// The **seal** hold [`SealFloor`] calls for over an engine holding `held` —
/// decision 2′ (a): a floor the host has read binds whether or not the nest is
/// in reach, so an unread floor holds exactly as the floor it keeps would.
#[must_use]
pub fn seal_hold(floor: SealFloor, held: Option<u64>) -> Option<SealHold> {
    match floor {
        SealFloor::Unarmed | SealFloor::Floor(None) | SealFloor::Unread(None) => None,
        SealFloor::Floor(Some(floor)) | SealFloor::Unread(Some(floor)) => {
            floor_ahead(held, Some(floor)).then_some(SealHold::Behind { floor, held })
        }
        SealFloor::RowGone => Some(SealHold::RowGone),
    }
}

/// The **publication** hold — decision 2′ (c): every seal hold, plus an unread
/// floor. Every send of sealed content to a nest (a chunk, a manifest, a
/// record, a queued upload's drain) asks this, never [`seal_hold`] alone.
#[must_use]
pub fn publish_hold(floor: SealFloor, held: Option<u64>) -> Option<SealHold> {
    seal_hold(floor, held)
        .or_else(|| matches!(floor, SealFloor::Unread(_)).then_some(SealHold::Unreadable))
}

/// A resident host's **refresh edge** hook: the engine's per-tick row read
/// compares the set's row (by `folder_ref`) against the `basis` its keys were
/// resolved from, and calls `on_rebuild` whenever [`edge_verdict`] says the keys
/// no longer answer the row — the host then re-reads custody and rebuilds the
/// engine if the keys it resolves differ. Best-effort by construction: the
/// callback only asks, and the host's other edges are the backstop.
#[derive(Clone)]
pub struct BindingEdge {
    pub folder_ref: FolderRef,
    pub basis: BindingBasis,
    pub on_rebuild: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for BindingEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingEdge")
            .field("folder_ref", &self.folder_ref)
            .field("basis", &self.basis)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_ahead_of_the_held_generation_holds_the_seal() {
        assert_eq!(
            seal_hold(SealFloor::Floor(Some(2)), Some(1)),
            Some(SealHold::Behind {
                floor: 2,
                held: Some(1)
            })
        );
        assert_eq!(
            seal_hold(SealFloor::Floor(Some(2)), None),
            Some(SealHold::Behind {
                floor: 2,
                held: None
            }),
            "a keyless engine behind an established floor holds"
        );
    }

    #[test]
    fn a_met_floor_an_unstamped_one_and_an_unarmed_engine_seal() {
        assert_eq!(seal_hold(SealFloor::Floor(Some(2)), Some(2)), None);
        assert_eq!(seal_hold(SealFloor::Floor(None), Some(1)), None);
        assert_eq!(seal_hold(SealFloor::Unarmed, None), None);
    }

    #[test]
    fn a_gone_row_holds_the_seal_and_the_publication_read_or_not() {
        assert_eq!(
            seal_hold(SealFloor::RowGone, Some(9)),
            Some(SealHold::RowGone)
        );
        assert_eq!(
            publish_hold(SealFloor::RowGone, Some(9)),
            Some(SealHold::RowGone)
        );
        let unread = SealFloor::RowGone.after_failed_read();
        assert_eq!(
            unread,
            SealFloor::RowGone,
            "a failed read never revives a gone row"
        );
        assert_eq!(seal_hold(unread, Some(9)), Some(SealHold::RowGone));
    }

    /// Decision 2′ (b)+(c): a failed read after a met floor seals under the
    /// last floor read, and holds what it sealed back from every nest.
    #[test]
    fn a_failed_read_after_a_met_floor_seals_and_holds_publication() {
        let unread = SealFloor::Floor(Some(9)).after_failed_read();
        assert_eq!(unread, SealFloor::Unread(Some(9)));
        assert_eq!(seal_hold(unread, Some(9)), None, "(b) the seal stands");
        assert_eq!(
            publish_hold(unread, Some(9)),
            Some(SealHold::Unreadable),
            "(c) nothing reaches a nest without a floor check"
        );
        assert_eq!(
            unread.after_failed_read(),
            unread,
            "a second failed read keeps the same floor"
        );
        assert_eq!(
            publish_hold(SealFloor::Floor(Some(9)), Some(9)),
            None,
            "a read floor that is met publishes"
        );
    }

    /// Decision 2′ (a): the floor a host has read binds offline as online.
    #[test]
    fn a_failed_read_after_a_floor_ahead_still_holds_the_seal() {
        let unread = SealFloor::Floor(Some(3)).after_failed_read();
        let behind = Some(SealHold::Behind {
            floor: 3,
            held: Some(2),
        });
        assert_eq!(seal_hold(unread, Some(2)), behind);
        assert_eq!(publish_hold(unread, Some(2)), behind);
    }

    #[test]
    fn only_a_local_ref_matches_a_row_and_only_by_id() {
        let list = vec![
            FolderSummary {
                id: 1,
                name: "docs".into(),
                ..Default::default()
            },
            FolderSummary {
                id: 2,
                name: "docs".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            summary_for_ref(&list, FolderRef::Local(2)).map(|s| s.id),
            Some(2),
            "same-named rows are told apart by id"
        );
        assert!(summary_for_ref(&list, FolderRef::Foreign([2; 32])).is_none());
    }

    #[test]
    fn a_seat_key_names_one_row_or_none() {
        let list = vec![
            FolderSummary {
                id: 1,
                name: "docs".into(),
                ..Default::default()
            },
            FolderSummary {
                id: 2,
                name: "docs".into(),
                ..Default::default()
            },
            FolderSummary {
                id: 3,
                name: "photos".into(),
                ..Default::default()
            },
        ];
        let id = |key: SeatRowKey| key.find(&list).map(|s| s.id);
        assert_eq!(id(SeatRowKey::Ref(FolderRef::Local(2))), Some(2));
        assert_eq!(id(SeatRowKey::Unbound), None);
    }
}
