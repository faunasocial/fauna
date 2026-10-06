//! Device-local sync state-dir layout.
//!
//! **The in-process sync driver is retired (A3, `sync-agent.md` § Scope per
//! platform):** resident byte-sync engines live in the external per-user
//! `fauna-sync-agent` process, provisioned + driven over the per-user unix
//! socket by [`crate::sync_agent`]. The agent resolves the **same**
//! `~/.config/fauna/sync/<actor-hex>/` per-account dir this module names (its
//! `SyncPaths::base_dir()` unix default, actor-scoped — plan D7
//! move-don't-recreate + account-scoping.md § Serialized switching), so the
//! per-folder state DBs and `device.db` carried over in place.
//!
//! What stays app-side: the XDG state-dir layout (shared with the Backups
//! page's ephemeral coordinator and the agent) and the stable device id read.
//! The agent's own config is the one record of this device's bindings.

use std::path::PathBuf;

use fauna_sync_engine::engine_lifecycle::{
    load_device_id, load_device_id_for_actor, store_device_id_hex,
};

/// One bound location as the Folders page renders it: a local directory, the
/// set's label, and the set's `fauna_core::folder_keys::FolderRef` in wire form —
/// the binding's only key (`on-demand-files.md` § Hosting multiple on-demand
/// folders), so it is required, never optional. The agent's own config is the
/// record; this is only the UI row shape its rows project onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationBinding {
    /// Local filesystem path the agent watches + syncs.
    pub path: PathBuf,
    /// The bound set's name — a label, never a key.
    pub folder: String,
    /// The bound set's `FolderRef` wire string.
    pub folder_id: String,
}

// ---------------------------------------------------------------------------
// XDG state-dir layout (sanctioned device-local config paths).
// ---------------------------------------------------------------------------

/// `~/.config/fauna` (honouring `XDG_CONFIG_HOME`).
fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("fauna");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config").join("fauna")
}

/// `~/.config/fauna/sync` — the unscoped sync base; the external agent's
/// `SyncPaths::flat_base_dir()` resolves to the same path. Holds the install
/// device secret and the per-actor scopes. See [`sync_state_dir`] for the dir
/// callers actually use.
fn flat_sync_dir() -> PathBuf {
    config_dir().join("sync")
}

/// The active account's scoped sync-state dir — `<flat_sync_dir>/<actor-hex>/`
/// — the same scope the external agent's `apply_actor_scope` selects at
/// boot/provision (`file-sync.md` § Multi-account × File Provider,
/// consequence 3). With no account active — which none of this module's real
/// call sites, all post-login, should meet — it resolves under the shared
/// `-unresolved-` scope, exactly as a malformed actor id does; never the
/// unscoped base, whose files belong to no account
/// (`account-scoping-dispositions.md` § Serialized switching).
fn sync_state_dir() -> PathBuf {
    let flat = flat_sync_dir();
    let Some(actor) = crate::account_scope::active_actor_id_hex() else {
        return flat.join(fauna_sync_engine::db::UNRESOLVED_ACTOR_COMPONENT);
    };
    fauna_sync_engine::db::actor_state_dir_or_unresolved(&flat, &actor)
}

/// This device's stable sync device id — the *same* id the pre-cutover
/// in-process engines used and the provisioned agent now presents (so the nest
/// sees one device). Reuses the dedicated `device.db` under
/// [`sync_state_dir`].
///
/// That dir is account-scoped, so a sign-out erases the id with it; the shared
/// get-or-create re-derives it from the install device secret under
/// [`flat_sync_dir`] (which no sweep names), so the sign-in that follows comes
/// back to the same `sync_devices` row instead of registering a new one
/// (`sync-agent-credentials.md` § Credential model, the 2026-09-20 ruling).
pub(crate) fn device_id() -> anyhow::Result<[u8; 32]> {
    let Some(actor) = crate::account_scope::active_actor_id_hex() else {
        return load_device_id(&sync_state_dir());
    };
    let actor_id = fauna_core::hex32::decode(&actor)
        .map_err(|e| anyhow::anyhow!("the active actor id is not 32-byte hex: {e}"))?;
    load_device_id_for_actor(&flat_sync_dir(), &sync_state_dir(), &actor_id)
}

/// Adopt an externally-provided device id as this account's sync identity —
/// the e2e session patch's counterpart to [`device_id`]'s own get-or-create.
/// `devices.md` § This-device marker requires ONE value to do both jobs (the
/// id the app **registers** with is the id the this-device marker
/// **compares** against), so a session patch naming a device id must land in
/// the *same* `device.db` [`device_id`] reads, not just the registry's
/// per-actor `device_id` slot the patch's `add_account` writes.
///
/// Call **after** the active actor is resolvable (`sync_state_dir` reads
/// [`crate::account_scope::active_actor_id_hex`], so this must follow the
/// patch's own `add_account`/`set_active` — on a first login that is what
/// makes an actor active at all, and on an actor switch it is what moves the
/// pointer, or this would adopt into the OUTGOING actor's dir) and **before**
/// `sync_agent::install` starts
/// the agent, which reads the id once at its own init and registers under it
/// — a later write would not move the device the nest already knows. Shares
/// tui's `media::adopt_device_id_hex` implementation via
/// `fauna_sync_engine::engine_lifecycle::store_device_id_hex`.
pub(crate) fn adopt_device_id_hex(hex: &str) -> bool {
    store_device_id_hex(&sync_state_dir(), hex)
        .inspect_err(|e| tracing::warn!("sync: {e}"))
        .is_ok()
}

/// `actor_id_hex`'s scoped backup-state dir —
/// `~/.config/fauna/backup/<actor-hex>/` — distinct from the file-sync engines'
/// per-folder DBs under `sync/`.
///
/// **Keyed by the caller's own (session) actor, never a fresh `active()`
/// read** — [`crate::backup_audit`]'s two call sites (the render-path
/// observation and the Backups page's audit pass) both pass the session's
/// own actor id, resolved once from the client/`AppState`, per
/// `account-scoping.md:818-837` (every session-path identity read goes
/// through the session account, not a per-call-site scope). A bound
/// (secondary) launch's `active()` can name a *different* account than the
/// one this process actually serves, and keying this dir off that stale read
/// would let two accounts share one `audit-state.json` — false freshness
/// alarms both ways, and each account's audit pass wiping the other's
/// last-passed records via `merge_outcomes`.
///
/// **Its only remaining occupant is the audit loop's `audit-state.json`**
/// ([`crate::backup_audit`]). It used to also hold the in-app upload
/// coordinator's `segment-backup.sqlite`, under a canonical-path contract that
/// kept the driver's `last_upload_time` writes visible to the Backups page's
/// status read. Both halves of that contract are gone: leg (d) (2026-07-24)
/// repointed the status read onto the nest's `fauna.backup.status` projection,
/// and the slice-5 flip (2026-07-29) deleted the in-app upload driver outright
/// — the owner's **source nest** is the segment-backup writer now, and it keeps
/// its own per-owner state under the nest data dir, nowhere near here
/// (`message-segment-store.md` § Cross-location backup protocol).
///
/// Audit state is wholly re-derivable (a lost file just means the next pass
/// has no high-water and re-audits).
pub(crate) fn backup_state_dir(actor_id_hex: &str) -> PathBuf {
    fauna_sync_engine::db::actor_state_dir_or_unresolved(&config_dir().join("backup"), actor_id_hex)
}
