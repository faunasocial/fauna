//! Web's **sign-out record** — the one durable decision a browser sign-out is
//! made behind, and what a later page load finishes it from
//! (`apps/account-scoping.md` § The scoping taxonomy → *Erasure follows scope*,
//! the paragraph "Web's account store is in the erase too", decision 2).
//!
//! A web sign-out waits for its account runtime to stop before it wipes the
//! credentials, so a user who confirms Sign Out and closes the tab would
//! otherwise find the account signed in at the next visit. The gesture
//! therefore writes this record before its first await, naming every account
//! it reaches; the wipe and each account's erase are then steps anybody can
//! finish — the gesture's own tail, or the next load of any tab.
//!
//! The record is two facts:
//!
//! - [`SignOutRecord::wipe_owed`] — a sign-out's all-accounts credential wipe
//!   has not run yet. While it is set, every account the registry names is
//!   reached, recorded or not.
//! - [`SignOutRecord::accounts`] — the accounts whose store may still be in
//!   the origin. An account leaves when its store is gone, and the record goes
//!   when it is empty.
//!
//! Once the wipe has run, **an account the registry names again is someone's
//! store again**: the user signed back in before a failed erase was retried,
//! and that store is left untouched and dropped from the record
//! ([`SignOutPlan::keep`]). A remove-account records its one account the same
//! way, with no wipe owed, so a store its erase could not remove is retried at
//! a later load.
//!
//! This is the web shape of the rule the native seats keep in
//! `sign_out_residue` (paths and fingerprints there, store names here). It is
//! pure over [`SecretStore`] so it is tested natively; the erase itself is
//! `fauna-wasm`'s `account_scope`.
//!
//! **Not under the cross-tab mutation lock.** Each operation is one
//! synchronous read-modify-write of one key, and the worst a lost update can
//! do is keep an account in the record whose store is already gone — which the
//! next load settles, because erasing an absent store succeeds.

use serde::{Deserialize, Serialize};

use crate::SecretStore;

/// The record's key. Install-scoped, and deliberately **outside** the `fauna/`
/// prefix: the registry's all-accounts erase removes that namespace, and the
/// record has to survive the wipe it orders.
pub const SIGN_OUT_RECORD_KEY: &str = "fauna_sign_out_record";

/// What a browser remembers of a sign-out (or a remove-account) it has not
/// finished. Empty with no wipe owed is "nothing owed", and is never stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SignOutRecord {
    /// A sign-out was confirmed and its credential wipe has not run.
    #[serde(default)]
    pub wipe_owed: bool,
    /// Lowercase-hex actor ids whose account store may still be in the origin.
    #[serde(default)]
    pub accounts: Vec<String>,
}

/// What a record owes, given the accounts the registry names right now —
/// [`SignOutRecord::plan`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignOutPlan {
    /// Run the all-accounts credential wipe first.
    pub wipe: bool,
    /// Erase these accounts' stores, each leaving the record as its store goes.
    pub erase: Vec<String>,
    /// Drop these from the record without touching their store: the registry
    /// names them again, so they are signed in here once more.
    pub keep: Vec<String>,
}

impl SignOutRecord {
    /// The stored record, or `None` when nothing is owed.
    ///
    /// A value that does not decode reads as a wipe owed with no accounts
    /// named: a record exists only because a sign-out was confirmed, so the
    /// load finishes that sign-out over the accounts the registry names rather
    /// than leave the user signed in.
    pub fn load(store: &dyn SecretStore) -> Option<Self> {
        let raw = store.get(SIGN_OUT_RECORD_KEY)?;
        Some(serde_json::from_str::<Self>(&raw).unwrap_or(Self {
            wipe_owed: true,
            accounts: Vec::new(),
        }))
    }

    /// Whether a confirmed sign-out's credential wipe is still owed — the
    /// identity read's fail-closed gate: while it is, no account reads as
    /// signed in.
    pub fn wipe_owed_in(store: &dyn SecretStore) -> bool {
        Self::load(store).is_some_and(|r| r.wipe_owed)
    }

    /// The sign-out's decision: `reach` joins whatever an earlier unfinished
    /// record names, and the wipe is owed. Written before the gesture's first
    /// await.
    pub fn record_sign_out(
        store: &dyn SecretStore,
        reach: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut record = Self::load(store).unwrap_or_default();
        record.wipe_owed = true;
        record.name(reach);
        record.save(store);
        record
    }

    /// A remove-account's decision: `actor_id_hex` joins the record, and
    /// whether a wipe is owed is left as it was.
    pub fn record_removal(store: &dyn SecretStore, actor_id_hex: &str) -> Self {
        let mut record = Self::load(store).unwrap_or_default();
        record.name([actor_id_hex.to_owned()]);
        record.save(store);
        record
    }

    /// What this record owes when the registry names `registry_accounts`.
    pub fn plan(&self, registry_accounts: &[String]) -> SignOutPlan {
        if self.wipe_owed {
            let mut all = self.clone();
            all.name(registry_accounts.iter().cloned());
            return SignOutPlan {
                wipe: true,
                erase: all.accounts,
                keep: Vec::new(),
            };
        }
        let named = |a: &String| registry_accounts.iter().any(|r| r.eq_ignore_ascii_case(a));
        let (keep, erase) = self.accounts.iter().cloned().partition(named);
        SignOutPlan {
            wipe: false,
            erase,
            keep,
        }
    }

    /// The wipe is about to run over `reach`: name every reached account
    /// first, so a tab closed right after the wipe still knows whose stores
    /// are left — the registry that named them is gone by then.
    pub fn before_wipe(store: &dyn SecretStore, reach: impl IntoIterator<Item = String>) {
        let mut record = Self::load(store).unwrap_or_default();
        record.wipe_owed = true;
        record.name(reach);
        record.save(store);
    }

    /// The credential wipe ran.
    pub fn wipe_done(store: &dyn SecretStore) {
        if let Some(mut record) = Self::load(store) {
            record.wipe_owed = false;
            record.save(store);
        }
    }

    /// `actor_id_hex` owes nothing more: its store is gone, or it is signed in
    /// here again. The record goes with its last account.
    pub fn settle(store: &dyn SecretStore, actor_id_hex: &str) {
        if let Some(mut record) = Self::load(store) {
            record
                .accounts
                .retain(|a| !a.eq_ignore_ascii_case(actor_id_hex));
            record.save(store);
        }
    }

    /// Add `actors` in their one lowercase spelling, each once.
    fn name(&mut self, actors: impl IntoIterator<Item = String>) {
        for actor in actors {
            let actor = actor.to_ascii_lowercase();
            if !actor.is_empty() && !self.accounts.contains(&actor) {
                self.accounts.push(actor);
            }
        }
    }

    fn save(&self, store: &dyn SecretStore) {
        if !self.wipe_owed && self.accounts.is_empty() {
            store.delete(SIGN_OUT_RECORD_KEY);
            return;
        }
        match serde_json::to_string(self) {
            Ok(raw) => store.set(SIGN_OUT_RECORD_KEY, &raw),
            Err(e) => tracing::warn!("sign-out record: not written: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemorySecretStore;

    fn a() -> String {
        "aa".repeat(32)
    }
    fn b() -> String {
        "bb".repeat(32)
    }

    #[test]
    fn nothing_recorded_owes_nothing() {
        let store = InMemorySecretStore::new();
        assert_eq!(SignOutRecord::load(&store), None);
        assert!(!SignOutRecord::wipe_owed_in(&store));
    }

    #[test]
    fn a_sign_out_records_its_reach_once_each_and_owes_the_wipe() {
        let store = InMemorySecretStore::new();
        let record = SignOutRecord::record_sign_out(&store, [a(), a().to_uppercase(), b()]);
        assert!(record.wipe_owed);
        assert_eq!(record.accounts, vec![a(), b()]);
        assert_eq!(SignOutRecord::load(&store), Some(record));
        assert!(SignOutRecord::wipe_owed_in(&store));
    }

    #[test]
    fn the_record_sits_outside_the_namespace_the_wipe_removes() {
        assert!(!SIGN_OUT_RECORD_KEY.starts_with("fauna/"));
    }

    #[test]
    fn a_wipe_owed_reaches_every_account_the_registry_names_too() {
        let store = InMemorySecretStore::new();
        let record = SignOutRecord::record_sign_out(&store, [a()]);
        let plan = record.plan(&[b()]);
        assert!(plan.wipe);
        assert_eq!(plan.erase, vec![a(), b()]);
        assert!(plan.keep.is_empty());
    }

    #[test]
    fn after_the_wipe_an_account_the_registry_names_again_is_kept() {
        let store = InMemorySecretStore::new();
        SignOutRecord::record_sign_out(&store, [a(), b()]);
        SignOutRecord::wipe_done(&store);
        let record = SignOutRecord::load(&store).expect("stores still owed");
        assert!(!record.wipe_owed);
        // The user signed back in as `a` before `a`'s failed erase was retried.
        let plan = record.plan(&[a().to_uppercase()]);
        assert!(!plan.wipe);
        assert_eq!(plan.erase, vec![b()]);
        assert_eq!(plan.keep, vec![a()]);
    }

    #[test]
    fn the_record_goes_with_its_last_account() {
        let store = InMemorySecretStore::new();
        SignOutRecord::record_sign_out(&store, [a(), b()]);
        SignOutRecord::wipe_done(&store);
        SignOutRecord::settle(&store, &a());
        assert_eq!(
            SignOutRecord::load(&store).map(|r| r.accounts),
            Some(vec![b()])
        );
        SignOutRecord::settle(&store, &b().to_uppercase());
        assert_eq!(store.get(SIGN_OUT_RECORD_KEY), None);
    }

    #[test]
    fn settling_every_account_before_the_wipe_keeps_the_wipe_owed() {
        let store = InMemorySecretStore::new();
        SignOutRecord::record_sign_out(&store, [a()]);
        SignOutRecord::settle(&store, &a());
        assert!(SignOutRecord::wipe_owed_in(&store));
    }

    #[test]
    fn before_the_wipe_the_record_names_everyone_the_registry_did() {
        let store = InMemorySecretStore::new();
        SignOutRecord::record_sign_out(&store, [a()]);
        SignOutRecord::before_wipe(&store, [a(), b()]);
        SignOutRecord::wipe_done(&store);
        // The registry is empty now; the record alone knows whose stores remain.
        let plan = SignOutRecord::load(&store).expect("stores owed").plan(&[]);
        assert_eq!(plan.erase, vec![a(), b()]);
    }

    #[test]
    fn a_removal_joins_the_record_without_ordering_a_wipe() {
        let store = InMemorySecretStore::new();
        let record = SignOutRecord::record_removal(&store, &a());
        assert!(!record.wipe_owed);
        assert_eq!(record.accounts, vec![a()]);
        // The registry no longer names the removed account: its store goes.
        assert_eq!(record.plan(&[b()]).erase, vec![a()]);
        // A removal that never happened (the tab closed first): untouched.
        assert_eq!(record.plan(&[a()]).keep, vec![a()]);
    }

    #[test]
    fn a_removal_beside_an_unfinished_sign_out_keeps_the_wipe_owed() {
        let store = InMemorySecretStore::new();
        SignOutRecord::record_sign_out(&store, [a()]);
        let record = SignOutRecord::record_removal(&store, &b());
        assert!(record.wipe_owed);
        assert_eq!(record.accounts, vec![a(), b()]);
    }

    #[test]
    fn an_unreadable_record_finishes_the_sign_out_over_the_registry() {
        let store = InMemorySecretStore::new();
        store.seed(SIGN_OUT_RECORD_KEY, "not json");
        let record = SignOutRecord::load(&store).expect("a record is there");
        assert!(record.wipe_owed);
        assert_eq!(record.plan(&[a()]).erase, vec![a()]);
    }

    #[test]
    fn a_record_from_a_newer_build_reads_its_known_fields() {
        let store = InMemorySecretStore::new();
        store.seed(
            SIGN_OUT_RECORD_KEY,
            &format!(r#"{{"wipe_owed":false,"accounts":["{}"],"later":1}}"#, a()),
        );
        let record = SignOutRecord::load(&store).expect("decodes");
        assert!(!record.wipe_owed);
        assert_eq!(record.accounts, vec![a()]);
    }
}
