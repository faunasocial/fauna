//! Pure path/status mapping shared by the cfapi callbacks (Windows-only) and the
//! IPC query handlers (cross-platform): map a full path — a Windows one, or a
//! unix one under a linux on-demand root — to a folder relative path under a
//! sync root and back, resolve which bound folder an
//! absolute path belongs to, and translate the engine's [`SyncState`] to the
//! shell extension's [`FileStatus`]. Kept free of any `#[cfg(windows)]` gate so
//! the IPC overlay-status path (`pipe_server`) can answer `GetFileStatus`
//! queries on any platform's unit tests, not just Windows.

use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{Event, EventKind, FileStatus};
use fauna_sync_engine::db::SyncState;

use crate::config::{LocationMode, SyncConfig};

/// Map a full drive-lettered Windows path to a forward-slash, folder-relative
/// path under `sync_root`.
///
/// The path must be a full Windows path *with drive letter* (e.g.
/// `C:\Users\me\Fauna\sub\f.txt`) — the same shape the IPC overlay-status query
/// passes. cfapi callbacks must first rejoin their `VolumeDosName` +
/// (volume-relative) `NormalizedPath` via [`callback_full_path`]: even under
/// `CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH` the `NormalizedPath` is volume-relative
/// and carries no drive letter. The sync root itself maps to `""` (root
/// directory). The prefix match is
/// case-insensitive (Windows paths are) and rejects a sibling whose name merely
/// starts with the root (`…\FaunaBackup` is not under `…\Fauna`). Returns `None`
/// if the path is not under the sync root.
///
/// **A unix root** (one that starts with `/` — the linux FUSE root's bound
/// directory) is matched as a unix path: case-sensitively, with `/` the only
/// separator, so a name holding a `\` keeps it.
pub fn normalized_path_to_rel(sync_root: &str, normalized: &str) -> Option<String> {
    if is_unix_root(sync_root) {
        let root = sync_root.trim_end_matches('/');
        let tail = normalized.strip_prefix(root)?;
        if !tail.is_empty() && !tail.starts_with('/') {
            return None;
        }
        return Some(tail.trim_start_matches('/').to_string());
    }
    let root = sync_root.trim_end_matches(['\\', '/']);
    // Case-insensitive prefix check without slicing on a non-char boundary.
    let prefix = normalized.get(..root.len())?;
    if !prefix.eq_ignore_ascii_case(root) {
        return None;
    }
    let tail = &normalized[root.len()..];
    // The remainder must be empty (the root itself) or begin with a separator —
    // else `normalized` is a sibling sharing the root's name as a prefix.
    if !tail.is_empty() && !tail.starts_with(['\\', '/']) {
        return None;
    }
    Some(tail.trim_start_matches(['\\', '/']).replace('\\', "/"))
}

/// Is `sync_root` a unix path? Decided by the root's own shape — a windows root
/// is drive-lettered (`C:\…`) or UNC (`\\…`), a unix one starts with `/` — so
/// the mapping is the same function on every platform, and its windows
/// behaviour stays pinned by the linux test run.
fn is_unix_root(sync_root: &str) -> bool {
    sync_root.starts_with('/')
}

/// The prefix of a linux on-demand root's descriptor reach
/// (`crate::fuse_host::Reach`): the engine and the loop work on
/// `/proc/self/fd/<fd>`, the directory UNDER the mount.
const REACH_PREFIX: &str = "/proc/self/fd/";

/// The root as the user and the apps see it: a descriptor reach is mapped back
/// to the bound directory it was opened on (the mount point), by reading the
/// `/proc` link — never by touching the mounted view. Every other root is
/// itself.
fn shown_root(sync_root: &str) -> std::borrow::Cow<'_, str> {
    if let Some(fd) = sync_root.strip_prefix(REACH_PREFIX)
        && !fd.is_empty()
        && fd.bytes().all(|b| b.is_ascii_digit())
        && let Ok(target) = std::fs::read_link(sync_root)
    {
        return std::borrow::Cow::Owned(target.to_string_lossy().into_owned());
    }
    std::borrow::Cow::Borrowed(sync_root)
}

/// Reconstruct the full Windows path a cfapi callback refers to from its
/// `VolumeDosName` (e.g. `C:`) and `NormalizedPath`. Under
/// `CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH` the `NormalizedPath` is the full path
/// *relative to the volume* — it does **not** include the drive letter (that
/// lives in `VolumeDosName`, a separate `CF_CALLBACK_INFO` field). The callbacks
/// must rejoin the two before [`normalized_path_to_rel`], whose prefix match is
/// against the drive-lettered registered sync root. Exactly one separator joins
/// them (`"C:"` + `"\Users\…"` → `"C:\Users\…"`).
// Called only from cfapi_host.rs, itself #[cfg(windows)] (lib.rs) — dead by
// clippy's count on a Linux-native --lib build.
#[allow(dead_code)]
pub fn callback_full_path(volume_dos_name: &str, normalized: &str) -> String {
    let volume = volume_dos_name.trim_end_matches(['\\', '/']);
    if normalized.starts_with(['\\', '/']) {
        format!("{volume}{normalized}")
    } else {
        format!("{volume}\\{normalized}")
    }
}

/// Build the absolute Windows path the shell extension's overlay cache is keyed
/// by (the path `IShellIconOverlayIdentifier::IsMemberOf` receives) from a sync
/// root and a forward-slash, folder-relative path. The inverse of
/// [`normalized_path_to_rel`]: backslash-joined, `rel == ""` yields the root
/// itself. The cache does no path normalization, so this must match the OS path
/// byte-for-byte (modulo the case-insensitive lookup the OS itself does).
///
/// A unix root ([`is_unix_root`]) joins with `/` — the path a linux app keys
/// its badges by — and a linux root's descriptor reach is reported as the
/// mount point it covers ([`shown_root`]): an event naming `/proc/self/fd/…`
/// would match no path any app shows.
pub fn overlay_abs_path(sync_root: &str, rel: &str) -> String {
    if is_unix_root(sync_root) {
        let shown = shown_root(sync_root);
        let root = shown.trim_end_matches('/');
        return if rel.is_empty() {
            root.to_string()
        } else {
            format!("{root}/{rel}")
        };
    }
    let root = sync_root.trim_end_matches(['\\', '/']);
    if rel.is_empty() {
        return root.to_string();
    }
    format!("{root}\\{}", rel.replace('/', "\\"))
}

/// What [`resolve_to_folder_rel`] resolves an absolute path to: the bound
/// folder serving it, by name **and** identity, plus the folder-relative
/// path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFolder {
    /// The set's name (`LocationConfig::folder`) — what the nest's
    /// name-addressed version verbs (`fauna.files.versions.*`) take.
    pub folder: String,
    /// The set's identity ([`LocationConfig::binding`]). **Every state-DB
    /// read/write must key on this** (via `SyncPaths::sync_db_path_for_ref`) —
    /// the engine keeps its DB in the `fsid-` namespace, so a name-keyed path
    /// silently reads a DB that is not there: status,
    /// dehydrate and restore all failed toward "nothing here".
    pub folder_ref: FolderRef,
    /// Forward-slash folder-relative path.
    pub rel: String,
}

/// Resolve an absolute path to the bound folder serving it plus its folder
/// relative path. Only **on-demand** folders with a **binding** are
/// served by the multi-root hydration host (and so have a per-folder state DB
/// to answer overlay-status queries); always-resident and unbound on-demand
/// folders return `None`. First matching folder wins.
///
/// The single resolver for the IPC read/write sites: it carries the binding's
/// `folder_ref` so no consumer re-reads config to recover the identity it
/// needs for the state-DB path (one-resolver rule, as `engine_driver` resolves
/// once per engine).
pub fn resolve_to_folder_rel(config: &SyncConfig, abs_path: &str) -> Option<ResolvedFolder> {
    config.locations.iter().find_map(|f| {
        if f.mode != LocationMode::OnDemand {
            return None;
        }
        let binding = f.binding()?;
        let rel = normalized_path_to_rel(&f.path, abs_path)?;
        Some(ResolvedFolder {
            folder: binding.folder.to_string(),
            folder_ref: binding.folder_ref,
            rel,
        })
    })
}

/// The overlay status event a successful dehydrate (`FreeSpace`) must broadcast
/// for `abs_path` — the symmetric reverse of the hydrate path's
/// `FileStatusChanged { Synced }` (this crate's `run_hydration_loop`
/// success arm). Returns `Some` iff `abs_path` resolves to a served on-demand
/// folder via [`resolve_to_folder_rel`] (only those carry a per-root state DB
/// and a meaningful overlay — the same structural gate under which the hydrate
/// loop fires); `None` leaves an untracked path's overlay alone. The event key is
/// the absolute path Explorer's overlay cache uses verbatim (no normalization),
/// and `CloudOnly` equals [`file_status_from_state`]`(SyncState::Placeholder)`, so
/// the live flip and the 30 s cache re-query agree (no badge flicker).
// Both real callers (pipe_server.rs's handle_free_space + apply_restore_locally)
// are #[cfg(windows)]; the module doc's "kept free of any cfg(windows) gate" is
// about ITS OWN definition, so cfg(test) code can exercise it cross-platform —
// but that leaves it unreferenced by any non-test code on a Linux --lib build.
#[allow(dead_code)]
pub fn dehydrate_status_event(config: &SyncConfig, abs_path: &str) -> Option<Event> {
    resolve_to_folder_rel(config, abs_path).map(|_| Event {
        event: EventKind::FileStatusChanged {
            path: abs_path.to_string(),
            // == file_status_from_state(SyncState::Placeholder); hardcoded to mirror the
            // hydrate emit's hardcoded FileStatus::Synced (bridge.rs run_hydration_loop).
            status: FileStatus::CloudOnly,
        },
    })
}

/// Translate the engine's [`SyncState`] to the shell extension's overlay
/// [`FileStatus`]. The single source of truth for this mapping (both the
/// synchronous `GetFileStatus` query and the pushed `FileStatusChanged` event
/// must agree, or overlays flicker on cache re-query).
pub fn file_status_from_state(state: SyncState) -> FileStatus {
    match state {
        SyncState::Synced => FileStatus::Synced,
        SyncState::Uploading
        | SyncState::Downloading
        | SyncState::LocallyModified
        | SyncState::RemotelyModified => FileStatus::Syncing,
        SyncState::Placeholder => FileStatus::CloudOnly,
        SyncState::Conflicted => FileStatus::Error,
        // No file on this disk either way: gone everywhere, or gone here with the
        // nest's record still owed (`SyncState::to_display` renders neither).
        SyncState::Deleted | SyncState::LocallyDeleted => FileStatus::NotTracked,
    }
}

/// A folder's overlay [`FileStatus`] — the **severity-aggregate of its tracked
/// descendants** (`apps/windows.md` § Shell Extension, USER-ratified
/// 2026-07-14). Folders carry no `SyncDb` row, so their badge is folded from the
/// states of the rows beneath them: the worst status any tracked descendant
/// carries wins — any `Error` → `Error`; else any `Syncing` → `Syncing`; else any
/// `Synced` → `Synced` (a folder mixing hydrated and cloud-only children reads
/// `Synced` — its content is all present-or-available); else all cloud-only →
/// `CloudOnly`.
///
/// Returns `None` when the folder has **no tracked descendant** — every input maps
/// to `NotTracked` (all descendants `Deleted`), or the iterator is empty. The
/// caller renders `None` as `NotTracked`: an unbadged folder, exactly like an
/// untracked file.
///
/// Takes engine [`SyncState`]s, each mapped through [`file_status_from_state`], so
/// a folder's aggregate and its children's per-file badges can never disagree on
/// what a given state means.
pub fn folder_status_from_states(
    states: impl IntoIterator<Item = SyncState>,
) -> Option<FileStatus> {
    let mut best: Option<FileStatus> = None;
    let mut best_rank = 0u8;
    for state in states {
        let status = file_status_from_state(state);
        let rank = overlay_severity(status);
        if rank > best_rank {
            best_rank = rank;
            best = Some(status);
        }
    }
    best
}

/// The overlay-badge severity rank of a [`FileStatus`] — higher is worse, and the
/// worst status a folder's descendants carry wins. `NotTracked` is rank 0: it
/// never contributes to a folder badge (a `Deleted` descendant, which
/// [`file_status_from_state`] maps to `NotTracked`, is not a tracked file). The
/// order is the USER-ratified one (`apps/windows.md` § Shell Extension,
/// 2026-07-14): `Error` > `Syncing` > `Synced` > `CloudOnly`.
fn overlay_severity(status: FileStatus) -> u8 {
    match status {
        FileStatus::Error => 4,
        FileStatus::Syncing => 3,
        FileStatus::Synced => 2,
        FileStatus::CloudOnly => 1,
        // Never produced by this agent; an unknown status shows no badge.
        FileStatus::NotTracked | FileStatus::Unknown => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocationConfig;

    // ── normalized_path_to_rel (lifted from cfapi_host) ──

    #[test]
    fn root_itself_maps_to_empty() {
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna", r"C:\Users\me\Fauna").as_deref(),
            Some("")
        );
        // Trailing separator on either side is tolerated.
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna\", r"C:\Users\me\Fauna").as_deref(),
            Some("")
        );
    }

    #[test]
    fn nested_path_becomes_forward_slash_rel() {
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna", r"C:\Users\me\Fauna\sub\photo.jpg")
                .as_deref(),
            Some("sub/photo.jpg")
        );
    }

    #[test]
    fn prefix_match_is_case_insensitive() {
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna", r"c:\users\me\fauna\a.txt").as_deref(),
            Some("a.txt")
        );
    }

    #[test]
    fn sibling_sharing_root_name_is_rejected() {
        // `…\FaunaBackup\x` must NOT be treated as under `…\Fauna`.
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna", r"C:\Users\me\FaunaBackup\x.txt"),
            None
        );
    }

    #[test]
    fn path_outside_sync_root_is_none() {
        assert_eq!(
            normalized_path_to_rel(r"C:\Users\me\Fauna", r"C:\Windows\system32"),
            None
        );
    }

    // ── callback_full_path (cfapi VolumeDosName + volume-relative NormalizedPath) ──

    #[test]
    fn callback_full_path_rejoins_drive_letter_onto_volume_relative_normalized() {
        // cfapi's NormalizedPath (REQUIRE_FULL_FILE_PATH) is the full path
        // *relative to the volume* — no drive letter; the drive is in
        // VolumeDosName. The sync root itself and a nested file both rejoin.
        assert_eq!(
            callback_full_path("C:", r"\Users\me\Fauna"),
            r"C:\Users\me\Fauna"
        );
        assert_eq!(
            callback_full_path("C:", r"\Users\me\Fauna\sub\f.txt"),
            r"C:\Users\me\Fauna\sub\f.txt"
        );
        // The rejoined path maps under the drive-lettered registered sync root —
        // this is the whole point (the regression: the bare volume-relative path
        // below never matched it, so FETCH_PLACEHOLDERS returned empty).
        let root = r"C:\Users\me\Fauna";
        assert_eq!(
            normalized_path_to_rel(
                root,
                &callback_full_path("C:", r"\Users\me\Fauna\sub\f.txt")
            )
            .as_deref(),
            Some("sub/f.txt"),
        );
        // Documents the bug being fixed: the bare NormalizedPath (no drive) does
        // NOT map under the drive-lettered sync root.
        assert_eq!(
            normalized_path_to_rel(root, r"\Users\me\Fauna\sub\f.txt"),
            None
        );
    }

    // ── overlay_abs_path (inverse of normalized_path_to_rel) ──

    #[test]
    fn overlay_abs_path_backslash_joins_rel() {
        assert_eq!(
            overlay_abs_path(r"C:\Root", "sub/a.txt"),
            r"C:\Root\sub\a.txt"
        );
    }

    #[test]
    fn overlay_abs_path_empty_rel_is_root() {
        assert_eq!(overlay_abs_path(r"C:\Root", ""), r"C:\Root");
    }

    #[test]
    fn overlay_abs_path_tolerates_trailing_root_separator() {
        assert_eq!(overlay_abs_path(r"C:\Root\", "a.txt"), r"C:\Root\a.txt");
    }

    #[test]
    fn overlay_abs_path_round_trips_with_normalized_path_to_rel() {
        let root = r"C:\Users\me\Fauna";
        let rel = "sub/photo.jpg";
        let abs = overlay_abs_path(root, rel);
        assert_eq!(normalized_path_to_rel(root, &abs).as_deref(), Some(rel));
    }

    // ── unix roots (the linux FUSE root) ──

    #[test]
    fn a_unix_root_joins_with_a_slash_and_round_trips() {
        let root = "/home/me/Fauna";
        assert_eq!(
            overlay_abs_path(root, "sub/a.txt"),
            "/home/me/Fauna/sub/a.txt"
        );
        assert_eq!(
            overlay_abs_path("/home/me/Fauna/", "a.txt"),
            "/home/me/Fauna/a.txt"
        );
        assert_eq!(overlay_abs_path(root, ""), "/home/me/Fauna");
        assert_eq!(
            normalized_path_to_rel(root, &overlay_abs_path(root, "sub/a.txt")).as_deref(),
            Some("sub/a.txt")
        );
        assert_eq!(normalized_path_to_rel(root, root).as_deref(), Some(""));
    }

    #[test]
    fn a_unix_root_matches_case_sensitively_and_keeps_backslashes() {
        let root = "/home/me/Fauna";
        assert_eq!(normalized_path_to_rel(root, "/home/me/fauna/a.txt"), None);
        assert_eq!(
            normalized_path_to_rel(root, "/home/me/FaunaBackup/a.txt"),
            None
        );
        assert_eq!(
            normalized_path_to_rel(root, r"/home/me/Fauna/a\b.txt").as_deref(),
            Some(r"a\b.txt")
        );
    }

    /// A linux root's events name the bound directory, never the descriptor reach
    /// the loop works through — read off the `/proc` link, not the mounted view.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_descriptor_reach_is_reported_as_the_directory_it_was_opened_on() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let bound = dir.path().canonicalize().unwrap();
        let held = std::fs::File::open(&bound).unwrap();
        let reach = format!("{REACH_PREFIX}{}", held.as_raw_fd());
        assert_eq!(
            overlay_abs_path(&reach, "sub/a.txt"),
            format!("{}/sub/a.txt", bound.display())
        );
        assert_eq!(overlay_abs_path(&reach, ""), bound.display().to_string());
    }

    // ── resolve_to_folder_rel ──

    /// A location bound to `(name, Local(id))`, or unbound when `None`.
    fn folder(path: &str, mode: LocationMode, binding: Option<(&str, i64)>) -> LocationConfig {
        LocationConfig {
            path: path.to_string(),
            mode,
            folder: binding.map(|(name, _)| name.to_string()),
            folder_id: binding.map(|(_, id)| FolderRef::Local(id).to_wire()),
            ..Default::default()
        }
    }

    fn config_with(folders: Vec<LocationConfig>) -> SyncConfig {
        SyncConfig {
            locations: folders,
            ..SyncConfig::default()
        }
    }

    #[test]
    fn resolves_path_under_on_demand_bound_folder() {
        let config = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        assert_eq!(
            resolve_to_folder_rel(&config, r"C:\od-docs\sub\a.txt"),
            Some(ResolvedFolder {
                folder: "docs".to_string(),
                folder_ref: FolderRef::Local(1),
                rel: "sub/a.txt".to_string(),
            })
        );
    }

    #[test]
    fn resolves_folder_root_to_empty_rel() {
        let config = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        assert_eq!(
            resolve_to_folder_rel(&config, r"C:\od-docs"),
            Some(ResolvedFolder {
                folder: "docs".to_string(),
                folder_ref: FolderRef::Local(1),
                rel: String::new(),
            })
        );
    }

    #[test]
    fn a_label_without_a_ref_is_not_resolved() {
        // The identity is what the state-DB path is keyed by,
        // and the name-keyed binding a pre-identity client wrote is retired: a
        // location carrying only a label is unbound, never resolved by name.
        let mut f = folder(r"C:\od-docs", LocationMode::OnDemand, Some(("docs", 1)));
        f.folder_id = None;
        let config = config_with(vec![f]);
        assert_eq!(resolve_to_folder_rel(&config, r"C:\od-docs\a.txt"), None);
    }

    #[test]
    fn always_resident_folder_is_not_resolved() {
        // Always folders are not served by the hydration host, so they carry no
        // per-folder db to query — even if (wrongly) bound to a folder.
        let config = config_with(vec![folder(
            r"C:\always",
            LocationMode::Always,
            Some(("ignored", 2)),
        )]);
        assert_eq!(resolve_to_folder_rel(&config, r"C:\always\a.txt"), None);
    }

    #[test]
    fn unbound_on_demand_folder_is_not_resolved() {
        let config = config_with(vec![folder(r"C:\od", LocationMode::OnDemand, None)]);
        assert_eq!(resolve_to_folder_rel(&config, r"C:\od\a.txt"), None);
    }

    #[test]
    fn path_under_no_folder_is_not_resolved() {
        let config = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        assert_eq!(resolve_to_folder_rel(&config, r"C:\elsewhere\a.txt"), None);
    }

    #[test]
    fn first_matching_bound_folder_wins() {
        let config = config_with(vec![
            folder(r"C:\od-photos", LocationMode::OnDemand, Some(("photos", 3))),
            folder(r"C:\od-docs", LocationMode::OnDemand, Some(("docs", 1))),
        ]);
        assert_eq!(
            resolve_to_folder_rel(&config, r"C:\od-docs\a.txt"),
            Some(ResolvedFolder {
                folder: "docs".to_string(),
                folder_ref: FolderRef::Local(1),
                rel: "a.txt".to_string(),
            })
        );
    }

    // ── file_status_from_state ──

    #[test]
    fn maps_each_sync_state_to_overlay_status() {
        assert_eq!(
            file_status_from_state(SyncState::Synced),
            FileStatus::Synced
        );
        for s in [
            SyncState::Uploading,
            SyncState::Downloading,
            SyncState::LocallyModified,
            SyncState::RemotelyModified,
        ] {
            let label = format!("{s:?}");
            assert_eq!(file_status_from_state(s), FileStatus::Syncing, "{label}");
        }
        assert_eq!(
            file_status_from_state(SyncState::Placeholder),
            FileStatus::CloudOnly
        );
        assert_eq!(
            file_status_from_state(SyncState::Conflicted),
            FileStatus::Error
        );
        assert_eq!(
            file_status_from_state(SyncState::Deleted),
            FileStatus::NotTracked
        );
    }

    // ── dehydrate_status_event (FreeSpace → CloudOnly, reverse of the hydrate
    //    FileStatusChanged{Synced}) ──

    #[test]
    fn dehydrate_status_event_for_served_on_demand_path_is_cloud_only() {
        let config = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        let event = dehydrate_status_event(&config, r"C:\od-docs\sub\a.txt")
            .expect("a served on-demand file must produce a status event");
        // Event/EventKind don't derive PartialEq — destructure (as bridge.rs does).
        let EventKind::FileStatusChanged { path, status } = event.event else {
            panic!("expected FileStatusChanged");
        };
        // The key is the absolute path verbatim — the overlay cache does no normalization.
        assert_eq!(path, r"C:\od-docs\sub\a.txt");
        assert_eq!(status, FileStatus::CloudOnly);
    }

    #[test]
    fn dehydrate_status_event_is_none_for_untracked_paths() {
        // always-mode: not served by the host, so no per-root DB / overlay.
        let always = config_with(vec![folder(
            r"C:\always",
            LocationMode::Always,
            Some(("x", 4)),
        )]);
        assert!(dehydrate_status_event(&always, r"C:\always\a.txt").is_none());
        // unbound on-demand: no folder → not served.
        let unbound = config_with(vec![folder(r"C:\od", LocationMode::OnDemand, None)]);
        assert!(dehydrate_status_event(&unbound, r"C:\od\a.txt").is_none());
        // outside any sync folder.
        let docs = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        assert!(dehydrate_status_event(&docs, r"C:\elsewhere\a.txt").is_none());
    }

    #[test]
    fn dehydrate_status_event_status_matches_get_file_status_mapping() {
        // The pushed CloudOnly must equal what the synchronous GetFileStatus query
        // returns once the entry is set to Placeholder, or the live flip and the cache
        // re-query disagree (flicker). Pin that the emit equals the shared mapping.
        let config = config_with(vec![folder(
            r"C:\od-docs",
            LocationMode::OnDemand,
            Some(("docs", 1)),
        )]);
        let event = dehydrate_status_event(&config, r"C:\od-docs\a.txt").unwrap();
        let EventKind::FileStatusChanged { status, .. } = event.event else {
            panic!("expected FileStatusChanged");
        };
        assert_eq!(status, file_status_from_state(SyncState::Placeholder));
    }

    // ── folder_status_from_states (folder badge = severity-aggregate of tracked
    //    descendants; apps/windows.md § Shell Extension, USER-ratified 2026-07-14) ──

    #[test]
    fn folder_with_no_descendants_is_unbadged() {
        // Empty iterator → None → the caller renders NotTracked (unbadged).
        assert_eq!(folder_status_from_states([]), None);
    }

    #[test]
    fn folder_of_only_deleted_descendants_is_unbadged() {
        // A Deleted row maps to NotTracked (the file is gone) — not a tracked
        // descendant — so a folder holding only tombstones is unbadged, not badged.
        assert_eq!(
            folder_status_from_states([SyncState::Deleted, SyncState::Deleted]),
            None
        );
    }

    #[test]
    fn folder_of_only_cloud_only_children_is_cloud_only() {
        assert_eq!(
            folder_status_from_states([SyncState::Placeholder]),
            Some(FileStatus::CloudOnly)
        );
        assert_eq!(
            folder_status_from_states([SyncState::Placeholder, SyncState::Placeholder]),
            Some(FileStatus::CloudOnly)
        );
    }

    #[test]
    fn synced_outranks_cloud_only_in_a_mixed_folder() {
        // A folder mixing a hydrated (Synced) and a cloud-only (Placeholder) child
        // reads Synced — its content is all present-or-available. Order-independent.
        assert_eq!(
            folder_status_from_states([SyncState::Placeholder, SyncState::Synced]),
            Some(FileStatus::Synced)
        );
        assert_eq!(
            folder_status_from_states([SyncState::Synced, SyncState::Placeholder]),
            Some(FileStatus::Synced)
        );
    }

    #[test]
    fn syncing_outranks_synced() {
        // Any in-flight child pulls the folder to Syncing, over any number of
        // settled Synced/CloudOnly children. All four "in-flight" engine states map
        // to Syncing (file_status_from_state), so each must outrank Synced here.
        for in_flight in [
            SyncState::Uploading,
            SyncState::Downloading,
            SyncState::LocallyModified,
            SyncState::RemotelyModified,
        ] {
            let label = format!("{in_flight:?}");
            assert_eq!(
                folder_status_from_states([SyncState::Synced, in_flight, SyncState::Placeholder]),
                Some(FileStatus::Syncing),
                "{label} descendant must pull the folder to Syncing"
            );
        }
    }

    #[test]
    fn error_outranks_everything() {
        // One Conflicted (→ Error) child wins over Synced, Syncing and CloudOnly —
        // the badge always reports the worst thing inside.
        assert_eq!(
            folder_status_from_states([
                SyncState::Placeholder,
                SyncState::Downloading,
                SyncState::Synced,
                SyncState::Conflicted,
            ]),
            Some(FileStatus::Error)
        );
    }

    #[test]
    fn deleted_descendants_are_skipped_but_do_not_suppress_a_badge() {
        // A tombstone beside a real cloud-only file must not drag the badge to
        // NotTracked: the Deleted row is skipped, the Placeholder decides the badge.
        assert_eq!(
            folder_status_from_states([SyncState::Deleted, SyncState::Placeholder]),
            Some(FileStatus::CloudOnly)
        );
    }
}
