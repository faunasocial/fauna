//! UniFFI surface of the shared on-demand **presence plan** and its
//! device-local **preference store**
//! (`fauna_folders_machine::on_demand_presence`; `on-demand-files.md`
//! § Android SAF DocumentsProvider binding, *… the plan is shared Rust*). Both
//! set-level on-demand platforms — apple's File Provider domains, android's
//! SAF provider roots — call these and keep only their OS calls.
//!
//! The store's path is internal wiring the app sets (its own data dir), never
//! a user choice; the toggle's value is the user's, through
//! `folder-on-demand-toggle`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fauna_folders_machine::on_demand_presence::{self as odp, OnDemandPrefs};
pub use fauna_folders_machine::on_demand_presence::{
    DesiredPresence, ForeignHold, PresencePlan, PresenceRemoval, PresenceRole, PresenceScope,
    PresenceSet,
};

use crate::{FfiError, general_err};

/// The device-local "show on demand" preference store (default ON), one JSON
/// file at a path the app supplies. Keyed by the set's actor-scoped identifier
/// (`actor_scoped_folder_ref`), so one account's toggle never hides another's
/// same-numbered set. Read-modify-write is serialized in-process.
#[derive(uniffi::Object)]
pub struct FfiOnDemandPrefsStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FfiOnDemandPrefsStore {
    /// The current prefs. An undecodable file reads as the all-ON default
    /// (logged) — the next toggle write replaces it — so a damaged file can
    /// only re-show sets, never wedge the reconcile.
    fn snapshot(&self) -> OnDemandPrefs {
        OnDemandPrefs::load(&self.path).unwrap_or_else(|e| {
            tracing::warn!(
                target: "fauna.on_demand",
                path = %self.path.display(),
                "on-demand prefs unreadable — treating every set as ON: {e:#}"
            );
            OnDemandPrefs::default()
        })
    }
}

#[uniffi::export]
impl FfiOnDemandPrefsStore {
    #[uniffi::constructor]
    pub fn new(path: String) -> Arc<Self> {
        Arc::new(Self {
            path: PathBuf::from(path),
            lock: Mutex::new(()),
        })
    }

    /// Whether the set's on-demand presence is on. An identifier that scopes
    /// no set reads as the default (ON) — no presence is ever keyed by one.
    pub fn is_enabled(&self, scoped_id: String) -> bool {
        let Some(scoped) = fauna_core::folder_keys::ActorScopedFolderRef::parse(&scoped_id) else {
            return true;
        };
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.snapshot().is_enabled(&scoped)
    }

    /// Turn the set's on-demand presence on or off and persist. Refuses an
    /// identifier that scopes no set (a bare ref, a set name): the store is
    /// never keyed by one.
    pub fn set_enabled(&self, scoped_id: String, enabled: bool) -> Result<(), FfiError> {
        let scoped = fauna_core::folder_keys::ActorScopedFolderRef::parse(&scoped_id)
            .ok_or_else(|| general_err(format!("not an actor-scoped identifier: {scoped_id}")))?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut prefs = self.snapshot();
        prefs.set_enabled(&scoped, enabled);
        prefs.save(&self.path).map_err(general_err)
    }
}

/// One reconcile's decision: the account's sets (every set it holds, own and
/// shared with it — a capability host's platform gets the list from
/// `capability_host_presence_sets`, never by mapping rows itself), the bare
/// refs this device presents a stronger
/// way (apple: bound locations; android: ingresses), the toggle store, the
/// registered identifiers, and the owner record where the platform keeps one
/// (identifier → owner actor hex; `None` where it keeps none). Errors only on
/// a malformed `actor_id_hex`.
#[uniffi::export]
pub fn on_demand_presence_plan(
    actor_id_hex: String,
    sets: Vec<PresenceSet>,
    stronger: Vec<String>,
    prefs: Arc<FfiOnDemandPrefsStore>,
    registered: Vec<String>,
    owners: Option<HashMap<String, String>>,
) -> Result<PresencePlan, FfiError> {
    let actor_id = hex::decode(&actor_id_hex)
        .ok()
        .and_then(|raw| <[u8; 32]>::try_from(raw.as_slice()).ok())
        .ok_or_else(|| general_err(format!("malformed actor id: {actor_id_hex}")))?;
    let stronger: BTreeSet<String> = stronger.into_iter().collect();
    let snapshot = {
        let _guard = prefs.lock.lock().unwrap_or_else(|p| p.into_inner());
        prefs.snapshot()
    };
    let desired = odp::desired_presences(actor_id, &sets, &stronger, &snapshot);
    let owners: Option<BTreeMap<String, String>> = owners.map(|m| m.into_iter().collect());
    Ok(odp::plan(actor_id, &desired, &registered, owners.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("fauna-ffi-odp-{}-{tag}", std::process::id()))
            .join("on-demand-prefs.json")
    }

    #[test]
    fn the_store_and_plan_round_trip_through_the_ffi_surface() {
        let path = temp_path("rt");
        let store = FfiOnDemandPrefsStore::new(path.to_string_lossy().into());
        let actor = "bb".repeat(32);
        let id = format!("local%3A1@{actor}");
        let sets = vec![PresenceSet {
            name: "docs".into(),
            set_name: "docs".into(),
            folder_id: "local:1".into(),
            this_device_accepts: true,
            role: PresenceRole::Own,
            read_only: false,
        }];

        assert!(store.is_enabled(id.clone()), "default ON");
        let plan = on_demand_presence_plan(
            actor.clone(),
            sets.clone(),
            vec![],
            store.clone(),
            vec!["docs".into()],
            None,
        )
        .unwrap();
        assert_eq!(plan.add.len(), 1);
        assert_eq!(plan.add[0].scoped_id, id);
        assert_eq!(plan.remove[0].scope, None);

        store.set_enabled(id.clone(), false).unwrap();
        assert!(!store.is_enabled(id.clone()));
        let plan = on_demand_presence_plan(
            actor.clone(),
            sets,
            vec![],
            store.clone(),
            vec![id.clone()],
            None,
        )
        .unwrap();
        assert!(plan.add.is_empty() && plan.keep.is_empty());
        assert_eq!(plan.remove[0].identifier, id);

        assert!(
            store.set_enabled("local:1".into(), false).is_err(),
            "never keyed by a bare ref"
        );
        assert!(on_demand_presence_plan("zz".into(), vec![], vec![], store, vec![], None).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
