//! The on-demand **presence plan** and its device-local **preference store** —
//! the one decision every set-level on-demand platform makes before any OS
//! call: which of the account's sets get an on-demand presence on this device
//! (an apple File Provider domain, an android SAF provider root), and which
//! registered identifiers go (`docs/goal/behavior/on-demand-files.md`
//! § Android SAF DocumentsProvider binding, *Set-level, one local presence per
//! set per device … and the plan is shared Rust*; § Apple File Provider
//! binding, *Apple's on-demand is set-level*).
//!
//! Lifted out of FaunaKit's Swift (`FileProviderCoordinator.desiredDomains` /
//! `.plan`, `FileProviderDomainPrefs`) so android consumes what apple does
//! instead of growing a Kotlin twin; each app keeps only its OS calls. The
//! Swift pins (`FileProviderReconcilePlanTests`) are ported one-for-one below.
//!
//! The rules, in one place:
//!
//! - **Desired** = (the account's own sets ∪ the sets shared with it) where
//!   THIS device is a delivery seat ∩ toggle-enabled ∖ the sets this device
//!   presents a stronger way (apple: a bound always-resident location;
//!   android: an ingress). The explicit act outranks the ambient default, so
//!   no set has two engines writing it from one device id. A presence is a
//!   *delivery seat* — it pulls the head and hydrates on open — so an own set
//!   where this device holds no place, or a place with delivery switched off,
//!   gets none (`on-demand-files.md` § Apple File Provider binding, the
//!   mode-free rule). A set shared WITH the account is a delivery seat by the
//!   membership itself: the device roster is the owner's, a member's device
//!   holds no row in it, and accepting the share is the member's yes
//!   (§ Shared sets on a capability host, decision 3). [`held_sets`] is the
//!   one place both rules are applied.
//! - A set the account may only read carries [`PresenceSet::read_only`]: the
//!   platform advertises no write, create, delete or rename on it, and its
//!   host is built with no write half.
//! - Every presence is keyed by the set's **actor-scoped identifier**
//!   (`fauna_core::folder_keys::ActorScopedFolderRef`), so two accounts'
//!   `local:1` are two identifiers and never collide. A set whose ref cannot
//!   be scoped yields no presence.
//! - The toggle is **device-local, default ON**, keyed by that scoped
//!   identifier — a bare ref or a set name is never a key.
//! - A registered identifier no desired set names is **removed**; one that
//!   scopes no set to any account (an unparseable identifier) is
//!   removed **un-gated** — there is no account state behind it to drain.
//! - The **owner record** (apple's domain-owner map) is an optional input,
//!   defense in depth: a registered desired identifier with no owner on record
//!   is backfilled, one recorded to another account is neither adopted nor
//!   re-recorded. The record's persistence stays with the platform that keeps
//!   one; a platform that keeps none passes `None` and the scoped identifier
//!   alone decides.

use std::collections::{BTreeMap, BTreeSet};

use fauna_core::folder_keys::{ActorScopedFolderRef, FolderRef};
use serde::{Deserialize, Serialize};

/// How the account holds a set. Both roles join the plan
/// (`on-demand-files.md` § Shared sets on a capability host, decision 3);
/// [`PresenceRole::joins_plan`] stays the one seam a role is admitted or
/// withheld at, so the plan's signature never changes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PresenceRole {
    /// The account owns the set (bound to a group or not — sharing changes
    /// its keys, not its ownership).
    Own,
    /// Another account's set shared with this one, on this nest or another —
    /// in the plan since the user's answer of 2026-09-30 (default ON, hidden
    /// per set per device by the same toggle).
    SharedWithMe,
}

impl PresenceRole {
    /// Whether a set held in this role is a candidate for an on-demand
    /// presence at all (before this device's place, toggle and stronger
    /// presence).
    #[must_use]
    pub fn joins_plan(self) -> bool {
        match self {
            PresenceRole::Own | PresenceRole::SharedWithMe => true,
        }
    }
}

/// One set the account holds, as the plan needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PresenceSet {
    /// The set's display name (what the platform shows the presence as).
    pub name: String,
    /// The set's own name — what a `fauna.sync.changed` push names it by
    /// (`SyncChangedPayload::names_set`), so a platform finds the presence a
    /// nudge is for without comparing display names: a shared set's `name`
    /// carries its owner, and a member can hold a set named like one of their
    /// own. Equal to `name` for an own set.
    #[cfg_attr(feature = "uniffi", uniffi(default = ""))]
    pub set_name: String,
    /// The set's bare `FolderRef` wire string.
    pub folder_id: String,
    /// Whether THIS device is a delivery seat for the set. An own set: this
    /// device holds a place whose `accepts` flag is set — `false` for one it
    /// holds no place in, where the toggle is the enrol gesture, which writes
    /// the place first. A set shared with the account: always `true` (the
    /// membership is the seat — module doc).
    pub this_device_accepts: bool,
    pub role: PresenceRole,
    /// The account may only read the set (a member without a `writer` grant):
    /// the platform advertises no write capability on its items, and the host
    /// it builds refuses every write typed. Advisory for what is *advertised*
    /// — the host reads the same row fact itself at every build and edge and
    /// is the authority on what is *refused*.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub read_only: bool,
}

/// The display name of a set shared with the account: the set's own name with
/// its owner's label, because a member can hold a set that wears the name of
/// one of their own (`on-demand-files.md` § Shared sets on a capability host,
/// decision 3). `owner` is the owner's account display label (the handle, else
/// the short id) for a same-nest set; a cross-nest set's custody record names
/// no owner, so it carries its home nest's host instead.
#[must_use]
pub fn shared_display_name(name: &str, owner: &str) -> String {
    if owner.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({owner})")
    }
}

/// Every set the account holds, as the plan takes it — the one mapping from
/// the member-visible folder list and the account's folder-key custody to
/// [`PresenceSet`]s, shared by every set-level platform so none grows its own
/// (`on-demand-files.md` § Shared sets on a capability host, decision 3).
///
/// - `rows` — this nest's `fauna.folders.list` with `include_shared_with_me`.
/// - `custody` — the account's folder-key custody.
/// - `own_place_accepts` — row id → whether this device's place on that OWN
///   row accepts (`SeatRead::delivers_presence`). A row with no entry is not a
///   delivery seat. Never consulted for a member row: the device roster reads
///   are owner-scoped and resolve a set by NAME among the caller's own, so
///   asking for a member row would answer for an own set of the same name.
///
/// The rules:
///
/// - An **owner** row is an own set, a delivery seat where its place accepts.
/// - A **member** row joins only once custody holds the set's content keys —
///   the evidence the share was accepted (the member's custody-ingest wrote
///   them). The nest lists a member row as soon as the owner rosters the
///   account, so without this a stranger's un-accepted share would put a
///   folder into the user's file browser unbidden; and a set whose keys
///   custody does not hold could not list a single path anyway.
/// - A live **cross-nest** record in custody (written at accept) is a set
///   shared with the account from another nest; it has no row here. A record
///   with no set name is not bindable and is left out.
/// - A shared set is read-only unless the account holds a `writer` grant; an
///   absent or unknown grant is a reader.
#[cfg(feature = "rpc-glue")]
#[must_use]
pub fn held_sets(
    rows: &[fauna_protocol::folders::FolderSummary],
    custody: &fauna_core::data::FoldersConfig,
    own_place_accepts: &BTreeMap<i64, bool>,
) -> Vec<PresenceSet> {
    use fauna_client_folders::custody::{content_keys, live_foreign_sets};
    use fauna_client_folders::engine_binding::custody_channel_for;

    let local = rows.iter().filter_map(|row| {
        let folder_id = FolderRef::Local(row.id).to_wire();
        if row.role.as_deref() != Some("member") {
            return Some(PresenceSet {
                name: row.name.clone(),
                set_name: row.name.clone(),
                folder_id,
                this_device_accepts: own_place_accepts.get(&row.id).copied().unwrap_or(false),
                role: PresenceRole::Own,
                read_only: false,
            });
        }
        let channel = custody_channel_for(row, custody).ok().flatten()?;
        content_keys(custody, &channel)?;
        let owner = fauna_core::format::account_display_label(
            row.owner_handle.as_deref(),
            row.owner_actor_id.as_deref().unwrap_or_default(),
        );
        Some(PresenceSet {
            name: shared_display_name(&row.name, &owner),
            set_name: row.name.clone(),
            folder_id,
            this_device_accepts: true,
            role: PresenceRole::SharedWithMe,
            read_only: row.is_reader_member(),
        })
    });
    let foreign = live_foreign_sets(custody).filter_map(|record| {
        let name = record.set_name.as_deref()?;
        // The owner's canonical `handle@domain`, as the home nest stamped it
        // and this member's own nest verified it; the home nest's host until
        // that pair lands (`on-demand-files.md` § Shared sets on a capability
        // host, decision 3; `federation.md` § … *The cross-nest owner label*).
        let owner = fauna_core::format::qualified_handle(
            record.owner_handle.as_deref(),
            record.owner_domain.as_deref(),
        )
        .filter(|_| {
            record
                .owner_domain
                .as_deref()
                .is_some_and(|d| !d.is_empty())
        });
        Some(PresenceSet {
            name: shared_display_name(
                name,
                owner
                    .as_deref()
                    .unwrap_or_else(|| nest_host(&record.home_nest_url)),
            ),
            set_name: name.to_string(),
            folder_id: FolderRef::Foreign(record.channel_id).to_wire(),
            this_device_accepts: true,
            role: PresenceRole::SharedWithMe,
            read_only: record.access.as_deref() != Some("writer"),
        })
    });
    local.chain(foreign).collect()
}

/// The host of a nest base URL (`https://nest.example:8443/` →
/// `nest.example:8443`) — the label a cross-nest set's display name carries.
#[cfg(feature = "rpc-glue")]
fn nest_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

/// A set the plan wants a presence for, its identity resolved exactly once —
/// the one place a bare ref becomes a scoped identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DesiredPresence {
    pub set: PresenceSet,
    /// `<ref-component>@<actor-id-hex>` (`local%3A1@…`) — the presence's
    /// identifier, `ActorScopedFolderRef::to_wire`.
    pub scoped_id: String,
}

/// A desired set whose identifier is registered under ANOTHER account on the
/// owner record. Unreachable for own-nest refs (the identifier carries the
/// account) — defense in depth over a corrupt or hand-edited record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ForeignHold {
    pub presence: DesiredPresence,
    /// The account on record, lowercase hex.
    pub owner_hex: String,
}

/// A registered identifier to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PresenceRemoval {
    pub identifier: String,
    /// The account and set the identifier scopes — whose state a removal gate
    /// (apple iOS's upload-drain gate) consults. `None`: the identifier scopes
    /// no set to any account, so the removal is un-gated.
    pub scope: Option<PresenceScope>,
}

/// The two halves of a parsed scoped identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PresenceScope {
    pub actor_id_hex: String,
    pub folder_id: String,
}

/// What one reconcile does, decided before any OS call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PresencePlan {
    /// Desired sets with no registered presence: add (then record the owner,
    /// where the platform keeps a record).
    pub add: Vec<DesiredPresence>,
    /// Desired sets whose presence is already registered and stays (not
    /// foreign-held) — the ones a platform that hosts its own tree builds a
    /// host for beside `add`.
    pub keep: Vec<DesiredPresence>,
    /// Registered desired identifiers with NO owner on record: record the
    /// owner. Always empty when no owner record is supplied.
    pub backfill: Vec<String>,
    /// Registered desired identifiers recorded to a foreign owner: neither
    /// adopted nor re-recorded — logged and left to the owner's drain.
    pub foreign_held: Vec<ForeignHold>,
    /// Registered identifiers no desired set names — another account's, a
    /// set whose place here stopped accepting (or was removed) or was toggled
    /// off, an unparseable identifier.
    pub remove: Vec<PresenceRemoval>,
}

/// The desired presences: the sets this device is a delivery seat for, own or
/// shared with the account ([`held_sets`] applies each role's seat rule),
/// toggle-enabled under their scoped identifier, minus the sets in `stronger`
/// (bare refs — the binding or ingress surface's key). Order follows `sets`.
#[must_use]
pub fn desired_presences(
    actor_id: [u8; 32],
    sets: &[PresenceSet],
    stronger: &BTreeSet<String>,
    prefs: &OnDemandPrefs,
) -> Vec<DesiredPresence> {
    sets.iter()
        .filter(|set| set.role.joins_plan() && set.this_device_accepts)
        .filter(|set| !stronger.contains(&set.folder_id))
        .filter_map(|set| {
            let scoped = FolderRef::parse(&set.folder_id)?.scoped_to(actor_id);
            prefs.is_enabled(&scoped).then(|| DesiredPresence {
                set: set.clone(),
                scoped_id: scoped.to_wire(),
            })
        })
        .collect()
}

/// The convergence decision over the desired presences and the registered
/// identifiers. `owners` is the platform's owner record (identifier → owner
/// actor hex), `None` where the platform keeps none.
#[must_use]
pub fn plan(
    actor_id: [u8; 32],
    desired: &[DesiredPresence],
    registered: &[String],
    owners: Option<&BTreeMap<String, String>>,
) -> PresencePlan {
    let actor_hex = hex::encode(actor_id);
    let mut plan = PresencePlan::default();
    for presence in desired {
        let id = &presence.scoped_id;
        if !registered.contains(id) {
            plan.add.push(presence.clone());
            continue;
        }
        let Some(owners) = owners else {
            plan.keep.push(presence.clone());
            continue;
        };
        match owners.get(id).map(|o| o.to_ascii_lowercase()) {
            None => {
                plan.backfill.push(id.clone());
                plan.keep.push(presence.clone());
            }
            Some(owner) if owner == actor_hex => plan.keep.push(presence.clone()),
            // A foreign owner is never overwritten: re-recording would hand
            // the lingering presence — and its queued edits — to this account.
            Some(owner) => plan.foreign_held.push(ForeignHold {
                presence: presence.clone(),
                owner_hex: owner,
            }),
        }
    }
    let desired_ids: BTreeSet<&str> = desired.iter().map(|d| d.scoped_id.as_str()).collect();
    plan.remove = registered
        .iter()
        .filter(|id| !desired_ids.contains(id.as_str()))
        .map(|id| PresenceRemoval {
            identifier: id.clone(),
            scope: ActorScopedFolderRef::parse(id).map(|s| PresenceScope {
                actor_id_hex: hex::encode(s.actor_id),
                folder_id: s.folder.to_wire(),
            }),
        })
        .collect();
    plan
}

/// The per-set, per-account, per-device "show on demand" preference backing
/// `folder-on-demand-toggle`: device-local by design (the same sanctioned
/// class as the folder map), default ON, keyed by the scoped identifier so
/// one account toggling its `local:1` off never hides another's. Persisted
/// as JSON; additive fields only — an older reader ignores fields it does not
/// know, a newer one defaults fields it does not find.
///
/// An entry the current `ActorScopedFolderRef::parse` refuses — one written
/// under the pre-2026-09-29 spelling `local:1@…`, before the ref half was
/// percent-encoded — is an **inert orphan**: it matches no identifier
/// `to_wire` emits, so its set reads as the default ON; it is never re-keyed
/// and never swept (a rewrite keeps it verbatim). The choice is one tap to
/// make again, so no migration arm is kept for it (`on-demand-files.md`
/// § Apple File Provider binding, *the actor-scoped device identity*).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnDemandPrefs {
    /// Scoped identifiers toggled off. Absent = ON.
    #[serde(default)]
    disabled: BTreeSet<String>,
}

impl OnDemandPrefs {
    #[must_use]
    pub fn is_enabled(&self, scoped: &ActorScopedFolderRef) -> bool {
        !self.disabled.contains(&scoped.to_wire())
    }

    pub fn set_enabled(&mut self, scoped: &ActorScopedFolderRef, enabled: bool) {
        let key = scoped.to_wire();
        if enabled {
            self.disabled.remove(&key);
        } else {
            self.disabled.insert(key);
        }
    }

    /// Decode the persisted form.
    ///
    /// # Errors
    /// The bytes are not the store's JSON.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    #[must_use]
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a set of strings always serializes")
    }

    /// Load the store at `path`; a missing file is the all-ON default.
    ///
    /// # Errors
    /// An unreadable or undecodable file.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Self::from_json(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist to `path`, crash-atomically (temp + fsync + rename).
    ///
    /// # Errors
    /// The write or rename failed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        fauna_core::secret_file::write_secret_file_0600(path, &self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [0xaa; 32];
    const B: [u8; 32] = [0xbb; 32];

    fn set(name: &str, folder_id: &str) -> PresenceSet {
        PresenceSet {
            name: name.into(),
            set_name: name.into(),
            folder_id: folder_id.into(),
            this_device_accepts: true,
            role: PresenceRole::Own,
            read_only: false,
        }
    }

    fn docs() -> PresenceSet {
        set("docs", "local:1")
    }

    fn pics() -> PresenceSet {
        set("pics", "local:2")
    }

    fn scoped(set: PresenceSet, actor: [u8; 32]) -> DesiredPresence {
        let scoped_id = FolderRef::parse(&set.folder_id)
            .unwrap()
            .scoped_to(actor)
            .to_wire();
        DesiredPresence { set, scoped_id }
    }

    fn removals(ids: &[&str]) -> Vec<PresenceRemoval> {
        ids.iter()
            .map(|id| PresenceRemoval {
                identifier: (*id).into(),
                scope: ActorScopedFolderRef::parse(id).map(|s| PresenceScope {
                    actor_id_hex: hex::encode(s.actor_id),
                    folder_id: s.folder.to_wire(),
                }),
            })
            .collect()
    }

    fn owners(pairs: &[(&str, [u8; 32])]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, a)| ((*id).into(), hex::encode(a)))
            .collect()
    }

    /// Account A left its `local:1` registered; account B wants its own
    /// `local:1`. Two identifiers: B's is added, A's is removed through the
    /// gated path (its scope names A), and nothing is held.
    #[test]
    fn two_accounts_same_ref_never_collide() {
        let docs_a = scoped(docs(), A);
        let docs_b = scoped(docs(), B);
        assert_ne!(docs_a.scoped_id, docs_b.scoped_id);
        let record = owners(&[(&docs_a.scoped_id, A)]);

        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            std::slice::from_ref(&docs_a.scoped_id),
            Some(&record),
        );
        assert_eq!(plan.add, vec![docs_b]);
        assert!(plan.backfill.is_empty());
        assert!(plan.foreign_held.is_empty());
        assert_eq!(plan.remove, removals(&[&docs_a.scoped_id]));
        assert_eq!(
            plan.remove[0].scope.as_ref().unwrap().actor_id_hex,
            hex::encode(A),
            "the removal gate consults A's state, never B's"
        );
    }

    /// A desired identifier whose RECORD names another account is neither
    /// adopted nor re-recorded.
    #[test]
    fn a_foreign_recorded_presence_is_neither_adopted_nor_re_recorded() {
        let docs_b = scoped(docs(), B);
        let record = owners(&[(&docs_b.scoped_id, A)]);
        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            std::slice::from_ref(&docs_b.scoped_id),
            Some(&record),
        );
        assert!(plan.add.is_empty(), "registered — no add");
        assert!(
            plan.keep.is_empty(),
            "a foreign-held presence is not ours to keep"
        );
        assert!(
            plan.backfill.is_empty(),
            "never re-recorded over a foreign owner"
        );
        assert_eq!(
            plan.foreign_held,
            vec![ForeignHold {
                presence: docs_b,
                owner_hex: hex::encode(A)
            }]
        );
        assert!(plan.remove.is_empty());
    }

    /// A registered desired identifier with no owner on record is backfilled.
    #[test]
    fn an_unowned_registered_presence_is_backfilled() {
        let docs_b = scoped(docs(), B);
        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            std::slice::from_ref(&docs_b.scoped_id),
            Some(&BTreeMap::new()),
        );
        assert!(plan.add.is_empty());
        assert_eq!(plan.backfill, vec![docs_b.scoped_id.clone()]);
        assert_eq!(plan.keep, vec![docs_b]);
        assert!(plan.foreign_held.is_empty());
    }

    /// One this account already owns needs nothing.
    #[test]
    fn an_own_registered_presence_is_left_alone() {
        let docs_b = scoped(docs(), B);
        let record = owners(&[(&docs_b.scoped_id, B)]);
        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            std::slice::from_ref(&docs_b.scoped_id),
            Some(&record),
        );
        assert_eq!(
            plan,
            PresencePlan {
                keep: vec![docs_b],
                ..PresencePlan::default()
            }
        );
    }

    /// A platform that keeps no owner record: a registered desired presence
    /// needs nothing — the scoped identifier alone decides.
    #[test]
    fn without_an_owner_record_a_registered_presence_is_kept() {
        let docs_b = scoped(docs(), B);
        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            std::slice::from_ref(&docs_b.scoped_id),
            None,
        );
        assert_eq!(
            plan,
            PresencePlan {
                keep: vec![docs_b],
                ..PresencePlan::default()
            }
        );
    }

    #[test]
    fn adds_the_missing_and_removes_the_undesired() {
        let docs_b = scoped(docs(), B);
        let pics_b = scoped(pics(), B);
        let stale_b = scoped(set("old", "local:9"), B);
        let record = owners(&[(&stale_b.scoped_id, A)]);
        let plan = plan(
            B,
            &[docs_b.clone(), pics_b.clone()],
            &[pics_b.scoped_id.clone(), stale_b.scoped_id.clone()],
            Some(&record),
        );
        assert_eq!(plan.add, vec![docs_b]);
        assert_eq!(plan.backfill, vec![pics_b.scoped_id.clone()]);
        assert_eq!(plan.keep, vec![pics_b]);
        assert!(plan.foreign_held.is_empty());
        assert_eq!(plan.remove, removals(&[&stale_b.scoped_id]));
    }

    /// An unparseable identifier (a bare ref, a set name) is never desired:
    /// removed without adoption and un-gated, even when it spells this
    /// account's own set; the scoped identity is added beside it.
    #[test]
    fn a_pre_scoping_identifier_is_removed_not_adopted() {
        let docs_b = scoped(docs(), B);
        let record = owners(&[("local:1", B), ("docs", B)]);
        let plan = plan(
            B,
            std::slice::from_ref(&docs_b),
            &["local:1".into(), "docs".into()],
            Some(&record),
        );
        assert_eq!(plan.add, vec![docs_b]);
        assert!(plan.backfill.is_empty());
        assert_eq!(
            plan.remove,
            vec![
                PresenceRemoval {
                    identifier: "local:1".into(),
                    scope: None
                },
                PresenceRemoval {
                    identifier: "docs".into(),
                    scope: None
                },
            ],
            "an identifier that scopes no set is removed un-gated"
        );
    }

    #[test]
    fn nothing_desired_removes_everything() {
        let docs_a = scoped(docs(), A);
        let record = owners(&[(&docs_a.scoped_id, A)]);
        let plan = plan(
            B,
            &[],
            std::slice::from_ref(&docs_a.scoped_id),
            Some(&record),
        );
        assert!(plan.backfill.is_empty());
        assert_eq!(plan.remove, removals(&[&docs_a.scoped_id]));
    }

    /// Each bare ref scopes exactly once: a stronger presence excludes by its
    /// bare ref, the toggle is read under the SCOPED identifier, an unscopable
    /// ref yields nothing — and default ON.
    #[test]
    fn desired_presences_scope_once_and_read_the_toggle_under_the_scoped_id() {
        let docs_b = scoped(docs(), B);
        let sets = [docs(), pics(), set("bad", "docs")];
        let stronger: BTreeSet<String> = ["local:2".into()].into();

        let desired = desired_presences(B, &sets, &stronger, &OnDemandPrefs::default());
        assert_eq!(desired, vec![docs_b.clone()], "default ON");

        let mut prefs = OnDemandPrefs::default();
        prefs.set_enabled(&FolderRef::Local(1).scoped_to(A), false);
        assert_eq!(
            desired_presences(B, &[docs()], &BTreeSet::new(), &prefs),
            vec![docs_b.clone()],
            "another account's toggle never hides this account's local:1"
        );

        prefs.set_enabled(&FolderRef::Local(1).scoped_to(B), false);
        assert!(
            desired_presences(B, &[docs()], &BTreeSet::new(), &prefs).is_empty(),
            "toggled off under its own identity"
        );
        prefs.set_enabled(&FolderRef::Local(1).scoped_to(B), true);
        assert_eq!(
            desired_presences(B, &[docs()], &BTreeSet::new(), &prefs),
            vec![docs_b]
        );
    }

    /// A stronger local presence outranks the toggle even when it is ON.
    #[test]
    fn a_stronger_presence_outranks_the_toggle() {
        let stronger: BTreeSet<String> = ["local:1".into()].into();
        assert!(desired_presences(B, &[docs()], &stronger, &OnDemandPrefs::default()).is_empty());
    }

    /// Only sets this device is a delivery seat for join: an own set this
    /// device holds no place in (or a place with delivery off) gets no
    /// presence — the wizard's unchecked device box means what it says. A set
    /// shared with the account joins beside the own ones (decision 3).
    #[test]
    fn only_sets_this_device_is_a_delivery_seat_for_join_the_plan() {
        let mut placeless = set("p", "local:3");
        placeless.this_device_accepts = false;
        let mut shared = set("s", "local:5");
        shared.role = PresenceRole::SharedWithMe;
        let desired = desired_presences(
            B,
            &[placeless, shared.clone(), docs()],
            &BTreeSet::new(),
            &OnDemandPrefs::default(),
        );
        assert_eq!(desired, vec![scoped(shared, B), scoped(docs(), B)]);
    }

    /// A shared-with-me set obeys the same toggle, under the same scoped
    /// identifier, and the same stronger-presence subtraction as an own one.
    #[test]
    fn a_shared_set_obeys_the_toggle_and_the_one_local_presence_rule() {
        let mut shared = set("s (alice)", "local:5");
        shared.role = PresenceRole::SharedWithMe;
        shared.read_only = true;
        let none = BTreeSet::new();

        let on = desired_presences(B, std::slice::from_ref(&shared), &none, &Default::default());
        assert_eq!(on, vec![scoped(shared.clone(), B)], "default ON");
        assert!(on[0].set.read_only, "the reader flag rides the plan");

        let mut prefs = OnDemandPrefs::default();
        prefs.set_enabled(&FolderRef::Local(5).scoped_to(B), false);
        assert!(
            desired_presences(B, std::slice::from_ref(&shared), &none, &prefs).is_empty(),
            "a member hides it with the toggle"
        );

        let bound: BTreeSet<String> = ["local:5".into()].into();
        assert!(
            desired_presences(B, &[shared], &bound, &Default::default()).is_empty(),
            "a writer's bound location outranks the presence"
        );
    }

    mod held {
        use super::super::*;
        use fauna_core::data::{FoldersConfig, ForeignFolder};
        use fauna_protocol::folders::FolderSummary;

        const GID: &[u8] = b"group-one-16byte";
        const GID2: &[u8] = b"group-two-16byte";

        fn owner_row(id: i64, name: &str) -> FolderSummary {
            FolderSummary {
                id,
                name: name.into(),
                role: Some("owner".into()),
                ..Default::default()
            }
        }

        fn member_row(id: i64, name: &str, gid: &[u8], access: Option<&str>) -> FolderSummary {
            FolderSummary {
                id,
                name: name.into(),
                role: Some("member".into()),
                access: access.map(str::to_string),
                mls_group_id: Some(hex::encode(gid)),
                owner_handle: Some("alice".into()),
                owner_actor_id: Some("ab".repeat(32)),
                ..Default::default()
            }
        }

        /// Custody holding one generation for each group's channel — what the
        /// member's custody-ingest leaves after an accepted share.
        fn custody_with(gids: &[&[u8]]) -> FoldersConfig {
            let mut cfg = FoldersConfig::default();
            for gid in gids {
                let channel = fauna_core::folder_keys::channel_id_for_group(gid);
                fauna_client_folders::custody::merge_received_keys(
                    &mut cfg,
                    channel,
                    fauna_core::folder_keys::FolderContentKeys::genesis([7u8; 32], 1),
                );
            }
            cfg
        }

        fn accepts(pairs: &[(i64, bool)]) -> BTreeMap<i64, bool> {
            pairs.iter().copied().collect()
        }

        /// Owner rows: own sets, a delivery seat exactly where the roster read
        /// said this device's place accepts — an unread row is not one.
        #[test]
        fn an_owner_row_is_an_own_set_seated_by_its_place() {
            let rows = [
                owner_row(1, "docs"),
                owner_row(2, "pics"),
                owner_row(3, "x"),
            ];
            let sets = held_sets(
                &rows,
                &FoldersConfig::default(),
                &accepts(&[(1, true), (2, false)]),
            );
            let seats: Vec<(&str, &str, bool, PresenceRole, bool)> = sets
                .iter()
                .map(|s| {
                    (
                        s.name.as_str(),
                        s.folder_id.as_str(),
                        s.this_device_accepts,
                        s.role,
                        s.read_only,
                    )
                })
                .collect();
            assert_eq!(
                seats,
                vec![
                    ("docs", "local:1", true, PresenceRole::Own, false),
                    ("pics", "local:2", false, PresenceRole::Own, false),
                    ("x", "local:3", false, PresenceRole::Own, false),
                ]
            );
        }

        /// A member row joins once custody holds its keys: seated by the
        /// membership (no place is read for it — the map's entry for its id is
        /// ignored), named with its owner, read-only unless a writer.
        #[test]
        fn an_accepted_share_is_a_seat_named_with_its_owner() {
            let rows = [
                member_row(5, "docs", GID, None),
                member_row(6, "plans", GID2, Some("writer")),
            ];
            let sets = held_sets(&rows, &custody_with(&[GID, GID2]), &accepts(&[(5, false)]));
            assert_eq!(
                sets,
                vec![
                    PresenceSet {
                        name: "docs (alice)".into(),
                        set_name: "docs".into(),
                        folder_id: "local:5".into(),
                        this_device_accepts: true,
                        role: PresenceRole::SharedWithMe,
                        read_only: true,
                    },
                    PresenceSet {
                        name: "plans (alice)".into(),
                        set_name: "plans".into(),
                        folder_id: "local:6".into(),
                        this_device_accepts: true,
                        role: PresenceRole::SharedWithMe,
                        read_only: false,
                    },
                ]
            );
        }

        /// A rostered-but-unaccepted share — custody holds no keys for it — has
        /// no presence: a stranger cannot put a folder into the file browser.
        #[test]
        fn an_unaccepted_share_has_no_presence() {
            let rows = [member_row(5, "docs", GID, None), owner_row(1, "docs")];
            let sets = held_sets(&rows, &custody_with(&[GID2]), &accepts(&[(1, true)]));
            assert_eq!(sets.len(), 1);
            assert_eq!(sets[0].role, PresenceRole::Own);

            let mut corrupt = member_row(5, "docs", GID, None);
            corrupt.mls_group_id = Some("not hex".into());
            assert!(held_sets(&[corrupt], &custody_with(&[GID]), &BTreeMap::new()).is_empty());
        }

        /// An own set and a set shared with the account can wear one name: two
        /// sets, two refs, and only the shared one's name carries the owner.
        #[test]
        fn a_shared_set_named_like_an_own_one_stays_apart() {
            let rows = [owner_row(1, "docs"), member_row(5, "docs", GID, None)];
            let sets = held_sets(&rows, &custody_with(&[GID]), &accepts(&[(1, true)]));
            let names: Vec<(&str, &str)> = sets
                .iter()
                .map(|s| (s.name.as_str(), s.folder_id.as_str()))
                .collect();
            assert_eq!(
                names,
                vec![("docs", "local:1"), ("docs (alice)", "local:5")]
            );
            // An owner with no handle falls back to the short id, never blank.
            let mut anon = member_row(5, "docs", GID, None);
            anon.owner_handle = None;
            let sets = held_sets(&[anon], &custody_with(&[GID]), &BTreeMap::new());
            assert!(sets[0].name.starts_with("docs (") && sets[0].name.len() > "docs ()".len());
        }

        /// A cross-nest set is its custody record: a live, named record is a
        /// seat keyed by the channel, labelled with its home nest; a left or
        /// name-less record is none.
        #[test]
        fn a_cross_nest_record_is_a_seat_keyed_by_its_channel() {
            let record = |channel: u8, name: Option<&str>, access: Option<&str>| ForeignFolder {
                channel_id: [channel; 32],
                mls_group_id: GID.to_vec(),
                home_nest_url: "https://nest.example:8443/".into(),
                set_name: name.map(str::to_string),
                access: access.map(str::to_string),
                accepted_at: 10,
                ..Default::default()
            };
            let mut left = record(3, Some("gone"), None);
            left.left_at = Some(20);
            let cfg = FoldersConfig {
                foreign_sets: vec![
                    record(1, Some("trip"), None),
                    record(2, Some("notes"), Some("writer")),
                    left,
                    record(4, None, None),
                ],
                ..Default::default()
            };
            let sets = held_sets(&[], &cfg, &BTreeMap::new());
            assert_eq!(
                sets,
                vec![
                    PresenceSet {
                        name: "trip (nest.example:8443)".into(),
                        set_name: "trip".into(),
                        folder_id: FolderRef::Foreign([1; 32]).to_wire(),
                        this_device_accepts: true,
                        role: PresenceRole::SharedWithMe,
                        read_only: true,
                    },
                    PresenceSet {
                        name: "notes (nest.example:8443)".into(),
                        set_name: "notes".into(),
                        folder_id: FolderRef::Foreign([2; 32]).to_wire(),
                        this_device_accepts: true,
                        role: PresenceRole::SharedWithMe,
                        read_only: false,
                    },
                ]
            );
            // Every ref the lister emits scopes to an identifier.
            assert_eq!(
                desired_presences([9; 32], &sets, &BTreeSet::new(), &Default::default()).len(),
                2
            );
        }

        /// A cross-nest record carrying its verified owner label is named by
        /// the owner's canonical `handle@domain`; one without it — or with a
        /// handle and no domain, which would read as a local user — keeps the
        /// home nest's host (`on-demand-files.md` decision 3).
        #[test]
        fn a_cross_nest_set_carries_its_owners_handle_at_domain() {
            let record = |channel: u8, handle: Option<&str>, domain: Option<&str>| ForeignFolder {
                channel_id: [channel; 32],
                mls_group_id: GID.to_vec(),
                home_nest_url: "https://nest.example:8443/".into(),
                set_name: Some("docs".into()),
                owner_handle: handle.map(str::to_string),
                owner_domain: domain.map(str::to_string),
                accepted_at: 10,
                ..Default::default()
            };
            let cfg = FoldersConfig {
                foreign_sets: vec![
                    record(1, Some("alice"), Some("example.com")),
                    record(2, None, None),
                    record(3, Some("alice"), None),
                ],
                ..Default::default()
            };
            let names: Vec<String> = held_sets(&[], &cfg, &BTreeMap::new())
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(
                names,
                vec![
                    "docs (alice@example.com)".to_string(),
                    "docs (nest.example:8443)".to_string(),
                    "docs (nest.example:8443)".to_string(),
                ]
            );
        }
    }

    #[test]
    fn prefs_round_trip_and_tolerate_unknown_fields() {
        let mut prefs = OnDemandPrefs::default();
        prefs.set_enabled(&FolderRef::Local(1).scoped_to(B), false);
        assert_eq!(OnDemandPrefs::from_json(&prefs.to_json()).unwrap(), prefs);

        let newer = format!(
            r#"{{"disabled":["local%3A1@{}"],"some_future_field":7}}"#,
            hex::encode(B)
        );
        assert_eq!(OnDemandPrefs::from_json(newer.as_bytes()).unwrap(), prefs);
        assert_eq!(
            OnDemandPrefs::from_json(b"{}").unwrap(),
            OnDemandPrefs::default()
        );
    }

    /// An entry in the pre-2026-09-29 spelling (the bare `:` in the ref half)
    /// is an inert orphan: its set reads ON, a toggle writes the live
    /// spelling beside it, and a rewrite keeps it verbatim — never re-keyed,
    /// never swept.
    #[test]
    fn prefs_keep_an_old_spelling_entry_inert() {
        let old = format!("local:1@{}", hex::encode(B));
        let json = format!(r#"{{"disabled":["{old}"]}}"#);
        let mut prefs = OnDemandPrefs::from_json(json.as_bytes()).unwrap();
        let scoped = FolderRef::Local(1).scoped_to(B);
        assert!(prefs.is_enabled(&scoped), "an orphan hides nothing");

        prefs.set_enabled(&scoped, false);
        assert!(!prefs.is_enabled(&scoped));
        let rewritten = String::from_utf8(prefs.to_json()).unwrap();
        assert!(rewritten.contains(&old), "kept verbatim: {rewritten}");
        assert!(rewritten.contains(&scoped.to_wire()));

        prefs.set_enabled(&scoped, true);
        assert_eq!(
            OnDemandPrefs::from_json(json.as_bytes()).unwrap(),
            prefs,
            "re-enabling removes only the live key"
        );
    }

    #[test]
    fn prefs_persist_through_the_file_store() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-on-demand-prefs-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("on-demand-prefs.json");
        assert_eq!(
            OnDemandPrefs::load(&path).unwrap(),
            OnDemandPrefs::default()
        );

        let mut prefs = OnDemandPrefs::default();
        prefs.set_enabled(&FolderRef::Local(1).scoped_to(B), false);
        prefs.save(&path).unwrap();
        assert_eq!(OnDemandPrefs::load(&path).unwrap(), prefs);

        std::fs::write(&path, b"not json").unwrap();
        assert!(OnDemandPrefs::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
