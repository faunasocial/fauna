//! In-memory advisory lease store for the task-delegation heartbeat lease
//! (`fauna.delegation.*`; `docs/goal/behavior/participants.md` § Coordination
//! primitive; design tracked internally).
//!
//! The nest is a **dumb, per-actor, last-writer-wins blackboard** (participants
//! .md:115 — the lease is advisory, never nest-authoritative or transactional):
//! a `heartbeat` records the caller's **self-reported** holder unconditionally
//! (only the map key is the authenticated actor — see
//! [`crate::delegation_handlers`] for why that adds no capability, and what a
//! future `holder_class` consumer must not assume), an `observe` reads
//! the current snapshot, and freshness is a nest-computed `age_ms`
//! (`now - last_write`) so an observer can compare it to
//! `fauna_core::delegation::LEASE_STALE_MS` without trusting any client clock.
//! **No CAS, no epoch, no rejection of a write** — the client's
//! `fauna_core::delegation::decide` owns all convergence logic. The one
//! thing refused is a kind outside `fauna_core::delegation::LIVE_TASK_KINDS`,
//! which is what keeps the blackboard small (see [`LeaseRegistry`]).
//!
//! Intentionally **in-memory only** (participants.md:74 — the lease is live
//! state, never persisted). A nest restart drops every slot ⇒ every client
//! observes "no lease" ⇒ the eligible participants re-acquire. That is lossless
//! recovery: a lease is not user data (no `version-compatibility.md`
//! § No-user-data-loss concern). WS/nest lifetime owns the entry, exactly like
//! [`crate::bridge_push_registry::BridgePushRegistry`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use fauna_core::data::ParticipantRef;
use fauna_core::delegation::{LIVE_TASK_KINDS, ParticipantClass, live_task_kind};
use fauna_protocol::delegation::LeaseState;

/// One lease slot: who last claimed it, their reported class, and the
/// monotonic instant of that write (for `age_ms`).
struct LeaseSlot {
    holder: ParticipantRef,
    holder_class: ParticipantClass,
    last_write: Instant,
}

/// Per-actor advisory lease map: `actor_id → (task_kind → LeaseSlot)`.
///
/// **Bounded by construction** : the inner key is the
/// `'static` spelling from [`live_task_kind`], so an actor holds at most one
/// slot per [`fauna_core::delegation::LIVE_TASK_KINDS`] entry and a heartbeat
/// naming any other kind writes nothing. Keying by actor first makes
/// `observe` read only the caller's own (≤ list-length) slots instead of
/// walking every actor's under the lock.
pub struct LeaseRegistry {
    leases: Mutex<HashMap<[u8; 32], HashMap<&'static str, LeaseSlot>>>,
}

impl Default for LeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaseRegistry {
    pub fn new() -> Self {
        Self {
            leases: Mutex::new(HashMap::new()),
        }
    }

    /// Record a heartbeat (last-writer-wins). Returns the post-write
    /// [`LeaseState`] (`holder = caller`, `age_ms = 0`) and whether the
    /// **holder changed** — `true` on a first claim (no prior record) or a
    /// takeover (different holder), `false` on a plain renew by the same
    /// holder. Only a `true` drives a `fauna.delegation.lease_changed` push, so
    /// steady-state renews (the common case) are silent.
    ///
    /// `None` — and nothing written — when `task_kind` is not a live task kind
    /// (the wire handler refuses it as malformed before reaching here).
    pub fn heartbeat(
        &self,
        actor: [u8; 32],
        task_kind: &str,
        holder: ParticipantRef,
        holder_class: ParticipantClass,
    ) -> Option<(LeaseState, bool)> {
        let kind = live_task_kind(task_kind)?;
        let mut map = self.leases.lock().unwrap();
        let slots = map.entry(actor).or_default();
        let holder_changed = match slots.get(kind) {
            Some(slot) => slot.holder != holder,
            None => true,
        };
        slots.insert(
            kind,
            LeaseSlot {
                holder: holder.clone(),
                holder_class: holder_class.clone(),
                last_write: Instant::now(),
            },
        );
        let state = LeaseState {
            task_kind: kind.to_string(),
            holder,
            holder_class,
            age_ms: 0,
            extra: Default::default(),
        };
        Some((state, holder_changed))
    }

    /// Snapshot the leases the `actor` currently holds a record for. `task_kinds`
    /// empty ⇒ every one of the actor's leases; otherwise only the named kinds
    /// that have a record (kinds with no lease — unknown kinds included — are
    /// simply absent; the observer treats absent = free). Each carries a fresh
    /// nest-computed `age_ms`.
    ///
    /// The caller's list is reduced to the live kinds it names *before* the
    /// lock is taken, so however long it is, the locked section reads at most
    /// one actor's ≤ list-length slots.
    pub fn observe(&self, actor: [u8; 32], task_kinds: &[String]) -> Vec<LeaseState> {
        let wanted: Option<Vec<&'static str>> = (!task_kinds.is_empty()).then(|| {
            LIVE_TASK_KINDS
                .iter()
                .map(|s| s.kind)
                .filter(|k| task_kinds.iter().any(|t| t == k))
                .collect()
        });
        let now = Instant::now();
        let map = self.leases.lock().unwrap();
        let Some(slots) = map.get(&actor) else {
            return Vec::new();
        };
        slots
            .iter()
            .filter(|(k, _)| wanted.as_ref().is_none_or(|w| w.contains(k)))
            .map(|(k, slot)| LeaseState {
                task_kind: k.to_string(),
                holder: slot.holder.clone(),
                holder_class: slot.holder_class.clone(),
                age_ms: now.saturating_duration_since(slot.last_write).as_millis() as u64,
                extra: Default::default(),
            })
            .collect()
    }

    /// Drop the `(actor, task_kind)` slot **iff** `expected_holder` is the
    /// recorded holder; returns whether a slot was removed (⇒ the caller
    /// should push `lease_changed`). Nest-internal only (the wire has no
    /// release — a client's lease simply goes stale): the in-process lease
    /// runner uses it so a grant revoke frees the kind *immediately* instead
    /// of after `LEASE_STALE_MS`, keeping "revoking the grant unassigns it"
    /// prompt (participants.md § Dispatch by kind). The holder match keeps a
    /// release from clobbering a slot another participant took over in the
    /// meantime (LWW discipline preserved).
    pub fn release(
        &self,
        actor: [u8; 32],
        task_kind: &str,
        expected_holder: &ParticipantRef,
    ) -> bool {
        let mut map = self.leases.lock().unwrap();
        let Some(slots) = map.get_mut(&actor) else {
            return false;
        };
        if !slots
            .get(task_kind)
            .is_some_and(|s| &s.holder == expected_holder)
        {
            return false;
        }
        slots.remove(task_kind);
        if slots.is_empty() {
            map.remove(&actor);
        }
        true
    }

    /// Number of `(actor, task_kind)` leases currently held. Tests/observability.
    pub fn lease_count(&self) -> usize {
        self.leases.lock().unwrap().values().map(HashMap::len).sum()
    }

    /// Back-date the `(actor, task_kind)` slot's recorded heartbeat by `age_ms`,
    /// so an `observe` reports it that much older — the seam that makes "this
    /// lease has gone stale" a state a test can *stand in* rather than an
    /// interval it waits out.
    ///
    /// `task_kind` `None` ⇒ every kind this actor has a slot for. Returns the
    /// post-write `(task_kind, age_ms)` pairs, so the caller asserts the
    /// resulting age instead of assuming it (the observability property
    /// [`crate::rpc_hold_test_hook`] documents: a seam whose effect cannot be
    /// read back is still a race, just a quieter one).
    ///
    /// A lease is live advisory state a heartbeat overwrites, so aging one
    /// destroys nothing and no user data is involved. Restricted to
    /// `test-hooks` all the same: the staleness rule is what hands a task kind
    /// from one participant to the next (`fauna_core::delegation::decide`), and
    /// a production caller able to move it could strand a kind on a participant
    /// that is not working. Convention 15 — the feature is the boundary, never
    /// a runtime gate.
    #[cfg(feature = "test-hooks")]
    pub fn backdate(
        &self,
        actor: [u8; 32],
        task_kind: Option<&str>,
        age_ms: u64,
    ) -> Vec<(String, u64)> {
        let back = std::time::Duration::from_millis(age_ms);
        let now = Instant::now();
        let mut map = self.leases.lock().unwrap();
        let mut aged = Vec::new();
        let Some(slots) = map.get_mut(&actor) else {
            return aged;
        };
        for (k, slot) in slots.iter_mut() {
            if task_kind.is_some_and(|want| want != *k) {
                continue;
            }
            // `checked_sub` rather than `-`: `Instant` subtraction panics past
            // the platform's epoch, and a test is free to ask for an age far
            // larger than this process's uptime (the point is usually "older
            // than LEASE_STALE_MS", not a specific instant). Saturating at the
            // earliest representable instant is the honest answer — the slot is
            // then as stale as this clock can express, which is what was asked.
            slot.last_write = slot.last_write.checked_sub(back).unwrap_or(slot.last_write);
            aged.push((
                k.to_string(),
                now.saturating_duration_since(slot.last_write).as_millis() as u64,
            ));
        }
        aged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    const A: [u8; 32] = [1u8; 32];
    const B: [u8; 32] = [2u8; 32];

    #[test]
    fn first_heartbeat_creates_lease_and_reports_change() {
        let r = LeaseRegistry::new();
        let (state, changed) = r
            .heartbeat(
                A,
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            )
            .expect("a live kind is recorded");
        assert!(changed, "first claim is a holder change");
        assert_eq!(state.holder, dev("dev-a"));
        assert_eq!(state.age_ms, 0);
        let snap = r.observe(A, &[]);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].holder, dev("dev-a"));
        assert_eq!(snap[0].task_kind, "backup-upload");
    }

    #[test]
    fn renew_by_same_holder_is_not_a_change() {
        let r = LeaseRegistry::new();
        r.heartbeat(
            A,
            "backup-upload",
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
        );
        let (_s, changed) = r
            .heartbeat(
                A,
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            )
            .expect("a live kind is recorded");
        assert!(!changed, "a renew by the same holder is silent");
    }

    #[test]
    fn takeover_by_different_holder_is_a_change() {
        let r = LeaseRegistry::new();
        r.heartbeat(
            A,
            "backup-upload",
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
        );
        let (state, changed) = r
            .heartbeat(
                A,
                "backup-upload",
                dev("dev-b"),
                ParticipantClass::PluggedInDesktop,
            )
            .expect("a live kind is recorded");
        assert!(changed, "a different holder is a takeover");
        assert_eq!(state.holder, dev("dev-b"));
        // The store now reflects dev-b (last-writer-wins).
        let snap = r.observe(A, &["backup-upload".into()]);
        assert_eq!(snap[0].holder, dev("dev-b"));
    }

    #[test]
    fn observe_filters_by_kind_and_isolates_by_actor() {
        let r = LeaseRegistry::new();
        r.heartbeat(
            A,
            "backup-upload",
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
        );
        r.heartbeat(A, "index", dev("dev-a"), ParticipantClass::PluggedInDesktop);
        r.heartbeat(
            B,
            "backup-upload",
            dev("dev-z"),
            ParticipantClass::PluggedInDesktop,
        );

        // A sees both its kinds; filter narrows to one.
        assert_eq!(r.observe(A, &[]).len(), 2);
        let only = r.observe(A, &["index".into()]);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].task_kind, "index");
        // A never sees B's lease.
        let a_backup = r.observe(A, &["backup-upload".into()]);
        assert_eq!(a_backup[0].holder, dev("dev-a"));
        // A kind with no record is absent, not an error.
        assert!(r.observe(A, &["content-rescore".into()]).is_empty());
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn backdate_ages_only_the_named_actors_slots() {
        let r = LeaseRegistry::new();
        r.heartbeat(A, "index", dev("dev-a"), ParticipantClass::PluggedInDesktop);
        r.heartbeat(
            A,
            "backup-upload",
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
        );
        r.heartbeat(B, "index", dev("dev-z"), ParticipantClass::PluggedInDesktop);

        let aged = r.backdate(A, Some("index"), 120_000);
        assert_eq!(aged.len(), 1, "only the named kind is aged: {aged:?}");
        assert_eq!(aged[0].0, "index");
        assert!(aged[0].1 >= 120_000, "reported age is the post-write one");

        // The one kind moved; its sibling and the other actor did not.
        let a_index = r.observe(A, &["index".into()]);
        assert!(a_index[0].age_ms >= 120_000);
        let a_backup = r.observe(A, &["backup-upload".into()]);
        assert!(a_backup[0].age_ms < 120_000, "a sibling kind is untouched");
        let b_index = r.observe(B, &["index".into()]);
        assert!(b_index[0].age_ms < 120_000, "another actor is untouched");

        // The holder and class survive: aging says "no heartbeat since", not
        // "somebody else has it".
        assert_eq!(a_index[0].holder, dev("dev-a"));
        assert_eq!(a_index[0].holder_class, ParticipantClass::PluggedInDesktop);

        // A real heartbeat resets it, exactly as one would have anyway.
        r.heartbeat(A, "index", dev("dev-a"), ParticipantClass::PluggedInDesktop);
        assert!(r.observe(A, &["index".into()])[0].age_ms < 120_000);
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn backdate_without_a_kind_ages_every_slot_of_that_actor() {
        let r = LeaseRegistry::new();
        for kind in ["index", "backup-upload"] {
            r.heartbeat(A, kind, dev("dev-a"), ParticipantClass::PluggedInDesktop);
        }
        let mut aged = r.backdate(A, None, 95_000);
        aged.sort();
        assert_eq!(aged.len(), 2, "every kind this actor holds: {aged:?}");
        assert!(aged.iter().all(|(_, age)| *age >= 95_000));

        // An actor with nothing recorded is not an error — an empty answer is
        // the honest reading of "nothing was holding anything", and the caller
        // asserts on it rather than being told a lie.
        assert!(r.backdate(B, None, 95_000).is_empty());
    }

    #[test]
    fn unknown_kind_writes_nothing_and_slots_stay_bounded_per_actor() {
        let r = LeaseRegistry::new();
        for i in 0..1000 {
            let kind = format!("junk-{i}");
            assert!(
                r.heartbeat(A, &kind, dev("dev-a"), ParticipantClass::PluggedInDesktop)
                    .is_none(),
                "an unlisted kind is refused"
            );
        }
        assert_eq!(r.lease_count(), 0);
        for _ in 0..3 {
            for spec in LIVE_TASK_KINDS {
                r.heartbeat(
                    A,
                    spec.kind,
                    dev("dev-a"),
                    ParticipantClass::PluggedInDesktop,
                )
                .expect("a live kind is recorded");
            }
        }
        assert_eq!(r.lease_count(), LIVE_TASK_KINDS.len());
        // Unknown kinds in an observe filter are absent, not an error.
        assert!(r.observe(A, &["junk-1".into()]).is_empty());
    }

    #[test]
    fn release_of_last_slot_drops_the_actor_entry() {
        let r = LeaseRegistry::new();
        r.heartbeat(A, "index", dev("dev-a"), ParticipantClass::PluggedInDesktop);
        assert!(!r.release(A, "index", &dev("dev-b")), "holder must match");
        assert!(r.release(A, "index", &dev("dev-a")));
        assert!(r.leases.lock().unwrap().is_empty());
        assert!(
            !r.release(A, "index", &dev("dev-a")),
            "nothing left to release"
        );
    }

    #[test]
    fn age_ms_grows_with_elapsed_time() {
        let r = LeaseRegistry::new();
        r.heartbeat(
            A,
            "backup-upload",
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
        );
        std::thread::sleep(std::time::Duration::from_millis(15));
        let snap = r.observe(A, &[]);
        assert!(
            snap[0].age_ms >= 15,
            "age tracks elapsed time, got {}",
            snap[0].age_ms
        );
    }
}
