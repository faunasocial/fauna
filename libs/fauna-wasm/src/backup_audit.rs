//! The web app's local home for the backup audit loop's state — the browser
//! side of `fauna_client_backup::audit`'s [`AuditStateStore`] seam, plus the one
//! observation feed that keeps its freshness comparison honest.
//!
//! Spec: `docs/goal/ui/backups.md` § Audit-alert surface ("audit state … persists
//! client-locally"); the loop itself and its ratified constants are owned by
//! `docs/goal/behavior/backup-restore.md` § Background Tasks. The browser twin of
//! `apps/fauna-linux/src/backup_audit.rs` / `apps/fauna-tui/src/backup_audit.rs`,
//! and like them it implements **no** audit logic: the store, the key, and the
//! observation call are all it owns.
//!
//! ## Why `localStorage`, and why per-actor
//!
//! The state is **this device's own evidence** about a destination, not a user
//! preference, so it never goes near the account plane — two devices auditing the same
//! destination hold independently valid records, and a synced copy would let one
//! device's stale verdict silence another's fresh one. Natively that means a file
//! under the client's data dir; the browser has no filesystem, so it is
//! `localStorage`, exactly as [`AuditStateStore`]'s own doc comment anticipates.
//!
//! The key is **actor-scoped** for the same reason linux scopes its path per
//! account: on a multi-user device (`account-scoping.md`) one owner must never
//! see — or, worse, silently suppress with — another's audit history. Web is
//! where this matters most, because an account switch happens *in-process* with
//! no restart, so a process-global slot would be read by whichever actor is
//! logged in next.
//!
//! Every field is **recreatable** — losing the slot costs exactly one re-audit —
//! which is why the reads below degrade to the default instead of surfacing an
//! error the user could not act on, and why this needs no migration story.
//!
//! ## The observation feed
//!
//! [`observe_thread_activity`] is the load-bearing half. Freshness compares what
//! the *destination* holds against what this client itself knows exists, and the
//! client must not learn the latter from the party being audited: a source nest
//! that answers "nothing new" would otherwise make freshness unfailable forever.
//! So the client writes down what it has actually **displayed** — the newest
//! conversation activity it has ever rendered — and that observation cannot be
//! retracted by the source afterwards. Monotonic, per the shared
//! `observe_local_record`. A shell that renders the audit elements but never
//! feeds this ships a permanently-passing audit that looks perfectly healthy.

#![cfg(target_arch = "wasm32")]

use fauna_client_backup::audit::{self, AuditStateSnapshot, AuditStateStore};

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The per-actor `localStorage` key holding this device's audit state as JSON.
/// `actor_hex` is the lowercase hex actor id — the browser twin of linux's
/// `<account dir>/audit-state.json`.
fn storage_key(actor_hex: &str) -> String {
    format!("fauna_backup_audit_{actor_hex}")
}

/// `localStorage`-backed [`AuditStateStore`] for **web** — the browser twin of
/// native `FileAuditStateStore`. Stateless: every call goes straight to the
/// global `window.localStorage`, like the other web stores.
pub struct LocalStorageAuditStateStore {
    actor_hex: String,
}

impl LocalStorageAuditStateStore {
    /// A store scoped to one actor. Callers pass the actor whose page is being
    /// rendered, never a process-global default — see the module docs.
    pub fn for_actor(actor_hex: impl Into<String>) -> Self {
        Self {
            actor_hex: actor_hex.into(),
        }
    }
}

impl AuditStateStore for LocalStorageAuditStateStore {
    fn load(&self) -> Result<AuditStateSnapshot, String> {
        // No `window`/`localStorage` at all (private mode, a worker) is the same
        // situation as a missing slot: nothing has been recorded yet. It is not
        // an error the caller can act on, and the state is recreatable.
        let Some(raw) = local_storage()
            .and_then(|ls| ls.get_item(&storage_key(&self.actor_hex)).ok().flatten())
        else {
            return Ok(AuditStateSnapshot::default());
        };
        // A corrupt slot IS reported, matching the native store: the caller then
        // degrades to re-auditing, but swallowing a parse error here would hide a
        // real bug in the format.
        serde_json::from_str(&raw).map_err(|e| format!("parse audit state: {e}"))
    }

    fn save(&self, snapshot: &AuditStateSnapshot) -> Result<(), String> {
        let raw =
            serde_json::to_string(snapshot).map_err(|e| format!("encode audit state: {e}"))?;
        let ls = local_storage().ok_or_else(|| "no localStorage for audit state".to_string())?;
        ls.set_item(&storage_key(&self.actor_hex), &raw)
            .map_err(|e| format!("write audit state: {e:?}"))
    }
}

/// Record that this client has displayed conversation activity stamped
/// `last_activity_ms` (epoch **milliseconds**, as `ThreadSummary` carries it) for
/// `actor_hex`.
///
/// Called from the conversation list's render — the one place the client shows
/// the user what it knows about the nest-originated message kinds, and therefore
/// the honest moment to say "I have seen a record this new". The load → observe
/// → save fold, and the millisecond→second boundary it crosses, are owned once
/// by `fauna_client_backup::audit::observe_thread_activity`; this shell supplies
/// only the browser's store.
///
/// Returns whether anything was persisted, so the SPA can keep its own in-memory
/// high-water and skip the call entirely on the common no-op — the per-frame
/// cost `backups.md` § Audit-alert surface warns a render-path feed pays
/// otherwise. Never throws: a missed observation costs at most a weaker freshness
/// comparison until the next render, and there is nothing a user could do.
pub fn observe_thread_activity(actor_hex: &str, last_activity_ms: i64) -> bool {
    let store = LocalStorageAuditStateStore::for_actor(actor_hex);
    audit::observe_thread_activity(&store, last_activity_ms).persisted
}

// ── the wasm-bindgen face ────────────────────────────────────────────────────

use wasm_bindgen::prelude::*;

/// Feed the audit's observation high-water from the conversation list's render:
/// "this client has displayed activity stamped `last_activity_ms`".
///
/// Free function rather than a `WsRpcClient` method — it touches only
/// `localStorage`, never the nest. See [`observe_thread_activity`] for why the
/// return value is worth acting on (skip the call on the common no-op).
#[wasm_bindgen(js_name = backupAuditObserve)]
pub fn backup_audit_observe(actor_hex: String, last_activity_ms: f64) -> bool {
    // `f64` not `i64`: a wasm-bindgen `i64` parameter crosses as a JS `bigint`,
    // which every existing caller would have to opt into.
    observe_thread_activity(&actor_hex, last_activity_ms as i64)
}

/// Shift the audit's clock by `offset_secs` — the `backup_audit_run_now` agent
/// command's own half (the pass itself is the production `backupAuditRunPass`).
///
/// **Compiled out of every release artifact** (testing.md convention 15): keyed on
/// `fauna-wasm`'s `test-helpers` feature alone, never on the profile, so the
/// generated JS/`.d.ts` face is a pure function of the feature set and `just wasm`
/// ships no clock lever at all. Only *time* is fakeable here — the connection to
/// the destination, its custody reply, and this client's own observation
/// high-water all stay real, so nothing about the audit's *finding* is injectable.
///
/// ⚠ Process-wide, and nothing auto-resets it: a test that leaves an offset behind
/// silently subtracts from the next audit test's elapsed time. Zero it at start
/// and end.
///
/// The `ForTest` suffix is load-bearing, not decoration:
/// `scripts/check-wasm-seam-exclusion.py` derives the seam surface from the
/// two flavors' `.d.ts` diff and asserts every test-only export *looks* like a
/// seam, so a gated export named otherwise fails the check as a probably-misplaced
/// `#[cfg]`. Name new seams to satisfy that regex; never loosen it.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = backupAuditSetClockOffsetForTest)]
pub fn backup_audit_set_clock_offset_for_test(offset_secs: f64) {
    fauna_client_backup::audit_clock::set_clock_offset_secs(offset_secs as i64);
}
