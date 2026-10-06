//! Context menu visibility logic and info text formatting.
//!
//! Determines whether the Fauna context menu should appear for a given
//! selection, and formats the device/version info strings shown as
//! read-only menu items.

use fauna_core::app_route::AppRoute;
use fauna_core::localized::LocalizedText;
use fauna_ipc::sync::{
    FileDevicesInfo, FileStatus, FileVersionEntry, FileVersionsInfo, Response, ResponsePayload,
    ResponseResult,
};

/// Shorthand for the crate-wide `LocalizedText -> String` resolution: every
/// user-visible string in this file is English-only today (`en.yaml` is the
/// only locale), but routes through the catalog rather than a baked-in
/// literal so a future locale needs no code change here.
fn t(text: LocalizedText) -> String {
    text.resolve(fauna_i18n::strings::lookup)
}

// ── Visibility ──────────────────────────────────────────────────────────────

/// Returns true if the Fauna context menu should be shown.
///
/// Rules:
/// - Exactly one file must be selected (`item_count == 1`).
/// - The file must have a known, tracked status (not `NotTracked` or `None`).
pub fn should_show_menu(status: Option<FileStatus>, item_count: usize) -> bool {
    if item_count != 1 {
        return false;
    }
    matches!(
        status,
        Some(FileStatus::Synced)
            | Some(FileStatus::Syncing)
            | Some(FileStatus::CloudOnly)
            | Some(FileStatus::Error)
    )
}

/// Outcome of the context menu's cache-first status lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLookup {
    /// A definitive answer: the status, or `None` for "untracked".
    Known(Option<FileStatus>),
    /// Not cached, and the shell forbade a slow call this time round.
    Pending,
}

/// What `IExplorerCommand::GetState` must answer for the root submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuState {
    Enabled,
    Hidden,
    /// `E_PENDING` — "I don't know yet; re-ask me on a background thread."
    Pending,
}

/// Decide the root submenu's state from a status lookup and the selection size.
///
/// [`MenuState::Pending`] is the load-bearing case. Explorer calls `GetState` with
/// `fOkToBeSlow = FALSE` on its UI thread, and `E_PENDING` is the documented way to
/// ask it to re-query on a background thread where a pipe round-trip is allowed.
/// Answering `Hidden` there is a *permanent* hide: with the 30 s status cache, every
/// right-click on a file the overlay hasn't touched recently would silently lose the
/// menu. A non-single selection is never pending — it is hidden outright, so we never
/// pay a background re-query for a selection that can never show the menu.
pub fn menu_state(lookup: StatusLookup, item_count: usize) -> MenuState {
    if item_count != 1 {
        return MenuState::Hidden;
    }
    match lookup {
        StatusLookup::Pending => MenuState::Pending,
        StatusLookup::Known(status) => {
            if should_show_menu(status, item_count) {
                MenuState::Enabled
            } else {
                MenuState::Hidden
            }
        }
    }
}

// ── Folder reduced set ───────────────────────────────────────────────────────

/// The three leaves of the Fauna submenu, for per-selection visibility rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmenuLeaf {
    Share,
    Devices,
    Versions,
}

/// **The folder reduced set (USER-decided 2026-07-16).** A tracked *folder*
/// shows the Fauna submenu too (`Directory\shell\Fauna`; the root's tracked-ness
/// already answers for folders via the badge fold), but only the leaves that
/// mean something for one: **Share stays** — a bound folder *is* the folder,
/// the natural share target once sharing is live; **device info** and **version
/// history hide** — both are per-file reads (`GetFileDevices` /
/// `ListFileVersions` resolve a file's row) and would render as a permanent
/// "unavailable" on a folder. Behavior owner: `apps/windows.md` § Shell
/// Extension (matrix row *Folder context menu*).
pub fn leaf_hidden_for_folder(leaf: SubmenuLeaf) -> bool {
    match leaf {
        SubmenuLeaf::Share => false,
        SubmenuLeaf::Devices | SubmenuLeaf::Versions => true,
    }
}

// ── Info text formatting ─────────────────────────────────────────────────────

/// Formats device count into a human-readable string.
///
/// - 0 → "Not synced to any device"
/// - 1 → "On this device only"
/// - N → "Synced to N devices"
pub fn format_device_info(info: &FileDevicesInfo) -> String {
    t(match info.device_count {
        0 => LocalizedText::key("file_context_menu.devices_none"),
        1 => LocalizedText::key("file_context_menu.devices_one"),
        n => LocalizedText::key_arg("file_context_menu.devices_count", "count", n.to_string()),
    })
}

/// Formats version count and optional timestamp into a human-readable string.
///
/// - 0 versions → "No saved versions"
/// - 1 version, no timestamp → "1 saved version"
/// - 1 version, with timestamp → "1 saved version (Mar 22)"
/// - N versions, no timestamp → "N saved versions"
/// - N versions, with timestamp → "N saved versions, latest Mar 22"
pub fn format_version_info(info: &FileVersionsInfo) -> String {
    t(match info.version_count {
        0 => LocalizedText::key("file_context_menu.versions_none"),
        1 => match info.latest_timestamp {
            None => LocalizedText::key("file_context_menu.versions_one"),
            Some(ts) => LocalizedText::key_arg(
                "file_context_menu.versions_one_dated",
                "date",
                format_short_date(ts),
            ),
        },
        n => match info.latest_timestamp {
            None => {
                LocalizedText::key_arg("file_context_menu.versions_count", "count", n.to_string())
            }
            Some(ts) => LocalizedText::key_args(
                "file_context_menu.versions_count_dated",
                [("count", n.to_string()), ("date", format_short_date(ts))],
            ),
        },
    })
}

/// Converts a Unix timestamp (seconds since epoch) to a short date like "Mar 22".
///
/// The calendar arithmetic is `fauna_core::caltime::civil_from_days` — the
/// ratified home for portable Gregorian date math (`ui/events.md` § Where logic
/// lives). It replaced a hand-rolled approximation (365.25 days/year, a fixed
/// 28-day February) that was **wrong across a leap year**: it rendered
/// 2024-01-01 as "Dec 1", a whole month and a year out, and nothing caught it
/// because the only test over this path asserted the surrounding parentheses
/// and never the date (`the_version_stamp_names_the_day_including_across_a_leap_february`
/// is the pin that now does).
///
/// The month name comes from the shared i18n catalog (`fauna_i18n::time::
/// month_name_short`) rather than a local table — this whole menu now adopts
/// the catalog (`ui/events.md` § Where logic lives; ruling recorded in
/// `docs/goal/architecture/apps/windows.md` § Shell Extension), so a stray
/// local copy would drift from the shared one instead of sharing it.
pub fn format_short_date(timestamp_secs: u64) -> String {
    let days_since_epoch = (timestamp_secs / 86400) as i64;
    let (_year, month, day) = fauna_core::caltime::civil_from_days(days_since_epoch);
    format!("{} {}", fauna_i18n::time::month_name_short(month), day)
}

/// Returns the string shown when file info could not be retrieved.
pub fn format_info_unavailable() -> String {
    t(LocalizedText::key("file_context_menu.info_unavailable"))
}

// ── Version-history submenu ──────────────────────────────────────────────────

/// One row of the Explorer **Version history** submenu.
///
/// `version_num` is the nest's `sync_changes` `seq` (`file-sync.md` § File
/// Versions) — carried through opaquely so `Invoke` can hand it straight back to
/// the `RestoreFileVersion` verb. A **disabled** row is never restorable: it is
/// either the current head (restoring it would append a no-op record) or the
/// "nothing here" placeholder, and its `version_num` is meaningless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMenuItem {
    pub version_num: i64,
    pub title: String,
    pub enabled: bool,
}

/// Human-readable byte size for a menu row. Deliberately coarse — this is a
/// context menu, not a properties dialog.
///
/// Delegates to the shared `fauna_core::format::byte_size` (`value-formatting.md`
/// § Byte sizes) rather than reimplementing the scaling: the local version this
/// replaced had no TB tier and always kept a trailing ".0" (`"2.0 KB"`), which
/// the ratified shared rule drops (`"2 KB"`).
pub fn format_size(bytes: i64) -> String {
    t(fauna_core::format::byte_size(bytes.max(0) as u64))
}

/// Build the submenu rows from a file's version history.
///
/// The nest returns oldest→newest; a "previous versions" menu reads newest-first,
/// so the order is reversed here. The newest entry **is** the file's current
/// content: it is shown for context, marked `(current)`, and disabled.
///
/// An empty history yields a single disabled row rather than an empty submenu —
/// an empty `IEnumExplorerCommand` renders as a blank popup, which reads as a bug.
pub fn version_menu_items(versions: &[FileVersionEntry]) -> Vec<VersionMenuItem> {
    if versions.is_empty() {
        return vec![VersionMenuItem {
            version_num: -1,
            title: "No saved versions".to_string(),
            enabled: false,
        }];
    }

    let newest_seq = versions[versions.len() - 1].version_num;
    versions
        .iter()
        .rev()
        .map(|v| {
            let stamp = format_short_date(v.created_at.max(0) as u64);
            let size = format_size(v.size_bytes);
            let is_current = v.version_num == newest_seq;
            VersionMenuItem {
                version_num: v.version_num,
                title: if is_current {
                    format!("{stamp} — {size} (current)")
                } else {
                    format!("{stamp} — {size}")
                },
                enabled: !is_current,
            }
        })
        .collect()
}

/// The version list from a `ListFileVersions` response, or `None` when the service
/// is down / returned an error / returned the wrong payload — mirroring
/// [`device_title`]'s tolerance. `None` and an empty list are distinct: the former
/// means "couldn't ask", the latter "no history".
pub fn version_list(resp: Option<&Response>) -> Option<Vec<FileVersionEntry>> {
    match resp.map(|r| &r.result) {
        Some(ResponseResult::Ok(ResponsePayload::FileVersionList(info))) => {
            Some(info.versions.clone())
        }
        _ => None,
    }
}

/// Title of the submenu's parent item.
pub fn version_history_title() -> String {
    t(LocalizedText::key("file_context_menu.version_history"))
}

/// Title of the submenu's "Share" leaf.
pub fn share_title() -> String {
    t(LocalizedText::key("file_context_menu.share"))
}

/// The product name, as it appears in the shell's own UI: the root "Fauna"
/// submenu's title and the caption of the notifications this file raises.
pub fn app_name() -> String {
    t(LocalizedText::key("common.app_name"))
}

/// The submenu's rows for a `ListFileVersions` response.
///
/// Keeps [`version_list`]'s distinction visible to the user: `None` ("couldn't ask" —
/// the service is down) is *not* the same as an empty history ("no saved versions").
/// Either way exactly one disabled row is produced, never an empty popup.
pub fn version_submenu_rows(resp: Option<&Response>) -> Vec<VersionMenuItem> {
    match version_list(resp) {
        Some(versions) => version_menu_items(&versions),
        None => vec![VersionMenuItem {
            version_num: -1,
            title: format_info_unavailable(),
            enabled: false,
        }],
    }
}

/// Outcome of invoking a version row, derived from the `RestoreFileVersion` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The nest recorded the restore and this device re-pointed its own copy.
    Restored(String),
    /// Nothing was restored; the message to surface.
    Failed(String),
}

/// Map a `RestoreFileVersion` response to the user-facing outcome. The service replies
/// `Empty` on success; a service-side error carries its own message, and a dead pipe
/// (`None`) reads as the service being unavailable.
pub fn restore_outcome(resp: Option<&Response>) -> RestoreOutcome {
    match resp.map(|r| &r.result) {
        Some(ResponseResult::Ok(ResponsePayload::Empty)) => {
            RestoreOutcome::Restored(t(LocalizedText::key("file_context_menu.version_restored")))
        }
        Some(ResponseResult::Err(msg)) => RestoreOutcome::Failed(t(LocalizedText::key_arg(
            "file_context_menu.version_restore_failed_detail",
            "message",
            msg.to_string(),
        ))),
        _ => RestoreOutcome::Failed(t(LocalizedText::key(
            "file_context_menu.version_restore_failed",
        ))),
    }
}

/// Whether two Explorer paths name the same file.
///
/// Guards the stale-enumeration hazard: a version child captures its path when the
/// submenu is *enumerated*, but `Invoke` receives the **live** selection. Windows paths
/// are case-insensitive, so a plain `==` would spuriously reject a re-cased path.
pub fn same_file_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ── IPC response → menu text mapping ─────────────────────────────────────────
//
// These map a sync-service `Response` (from `SyncPipeClient::request`) to the
// text/outcome the COM context-menu handlers surface. Keeping them pure (no COM,
// no pipe) makes the menu logic deterministically testable; the Windows-only COM
// glue in `com` below just performs the `request()` and hands the result here.

/// Title text for the "device info" menu item, from a `GetFileDevices` response.
///
/// `None` (service down / request failed) or any non-`FileDevices` payload maps
/// to the generic "File info unavailable" string.
pub fn device_title(resp: Option<&Response>) -> String {
    match resp.map(|r| &r.result) {
        Some(ResponseResult::Ok(ResponsePayload::FileDevices(info))) => format_device_info(info),
        _ => format_info_unavailable(),
    }
}

/// Title text for the "version info" menu item, from a `GetFileVersions` response.
pub fn version_title(resp: Option<&Response>) -> String {
    match resp.map(|r| &r.result) {
        Some(ResponseResult::Ok(ResponsePayload::FileVersions(info))) => format_version_info(info),
        _ => format_info_unavailable(),
    }
}

/// Map a `ShareFile` response to the in-app route the Share leaf opens
/// (`apps/windows.md` § Shell Extension → *The Share hand-off*): a file target
/// → `fauna://share-link`, the set's own root → `fauna://folder-share`. `None`
/// — no target (the agent refused, or is unreachable) — hides the leaf. The
/// agent never returns a link: it is seedless, and the app mints.
pub fn share_route(resp: Option<&Response>) -> Option<AppRoute> {
    match resp.map(|r| &r.result) {
        Some(ResponseResult::Ok(ResponsePayload::ShareTarget(target))) => {
            Some(match &target.path {
                Some(path) => AppRoute::ShareLink {
                    folder_id: target.folder_id,
                    path: path.clone(),
                },
                None => AppRoute::FolderShare {
                    folder_id: target.folder_id,
                },
            })
        }
        _ => None,
    }
}

/// The message for a Share invoke that found no target (a stale menu raced a
/// change) — the leaf is normally hidden before it gets here.
pub fn share_not_available() -> String {
    t(LocalizedText::key("file_context_menu.share_not_available"))
}

/// The message for a route the shell could not open (no `fauna://` handler —
/// the desktop app is not installed).
pub fn share_open_failed() -> String {
    t(LocalizedText::key("file_context_menu.share_open_failed"))
}

// ── Location mode toggle ───────────────────────────────────────────────────────

/// Whether to show the location sync-type toggle.
pub fn should_show_location_mode_toggle(is_synced_location: bool) -> bool {
    is_synced_location
}

/// Label for the location mode toggle menu item.
pub fn location_mode_label(current_mode: &str) -> String {
    t(match current_mode {
        "on-demand" => LocalizedText::key("file_context_menu.keep_on_device"),
        _ => LocalizedText::key("file_context_menu.make_on_demand"),
    })
}

// ── Pin / unpin / free-space visibility ─────────────────────────────────────

/// Whether to show the "Always keep on this device" (pin) menu item.
pub fn should_show_pin(
    status: FileStatus,
    is_pinned: bool,
    is_in_on_demand_location: bool,
) -> bool {
    if !is_in_on_demand_location || is_pinned {
        return false;
    }
    matches!(
        status,
        FileStatus::CloudOnly | FileStatus::Synced | FileStatus::Syncing
    )
}

/// Whether to show the "Unpin" menu item.
pub fn should_show_unpin(is_pinned: bool, is_in_on_demand_location: bool) -> bool {
    is_in_on_demand_location && is_pinned
}

/// Whether to show the "Free up space" menu item.
pub fn should_show_free_space(status: FileStatus, is_in_on_demand_location: bool) -> bool {
    if !is_in_on_demand_location {
        return false;
    }
    matches!(status, FileStatus::Synced | FileStatus::Syncing)
}

// ── Windows COM implementation ──────────────────────────────────────────────

/// `IExplorerCommand` context-menu COM classes — the "Fauna" submenu.
///
/// A root [`FaunaContextMenu`] (registered as a `ContextMenuHandlers` handler for
/// all file types) advertises sub-commands via `EnumSubCommands`, returning a
/// [`SubCommandEnum`] over three leaf commands: [`FaunaShareCommand`] (an action),
/// and the read-only [`FaunaInfoDevices`] / [`FaunaInfoVersions`] info items whose
/// titles are populated from `SyncPipeClient::request()` when the submenu opens.
///
/// All COM-bound glue lives here; the visibility / formatting / response-mapping
/// decisions are the pure functions above (unit-tested), so this layer is thin.
#[cfg(windows)]
pub mod com {
    // COM-glue module. Two clippy lints are inapplicable to everything here and
    // are allowed module-wide rather than silenced per-item:
    //  - `new_without_default`: these handler objects are created by
    //    `IClassFactory::CreateInstance` (and their `new()` bumps the global COM
    //    object count via `dll::object_added`), never via `Default`.
    //  - `not_unsafe_ptr_arg_deref`: the interface-method signatures (e.g.
    //    `IEnumExplorerCommand::Next`) are the fixed windows-rs COM vtable ABI —
    //    the raw pointers cannot become safe references and the methods cannot be
    //    marked `unsafe` (the trait signatures are generated).
    #![allow(clippy::new_without_default, clippy::not_unsafe_ptr_arg_deref)]

    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::Mutex;

    use fauna_ipc::sync::RequestMethod;
    use fauna_ipc::sync_pipe_client::SyncPipeClient;
    use windows::Win32::Foundation::{E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, S_FALSE, S_OK};
    use windows::Win32::System::Com::{CoTaskMemAlloc, CoTaskMemFree, IBindCtx};
    use windows::Win32::UI::Shell::{
        ECF_DEFAULT, ECF_HASSUBCOMMANDS, ECS_DISABLED, ECS_ENABLED, ECS_HIDDEN,
        IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
        IShellItem, IShellItemArray, SIGDN_FILESYSPATH,
    };
    use windows::core::{BOOL, Error, GUID, HRESULT, PWSTR, Ref, Result, implement};

    /// `E_PENDING` — the documented `IExplorerCommand::GetState` answer for "not known
    /// yet; re-ask me on a background thread".
    ///
    /// ⚠ **`windows` 0.61 does not export it** (`Win32::Foundation` carries only
    /// unrelated `*_E_PENDING_*` constants), so it is declared here from the SDK value.
    const E_PENDING: HRESULT = HRESULT(0x8000_000A_u32 as i32);

    use super::{
        MenuState, RestoreOutcome, StatusLookup, app_name, device_title, format_info_unavailable,
        menu_state, restore_outcome, same_file_path, share_not_available, share_open_failed,
        share_route, share_title, version_history_title, version_submenu_rows,
    };
    use crate::dll::clsid;
    use fauna_core::app_route::AppRoute;

    // ── Small COM helpers ─────────────────────────────────────────────────────

    /// Allocate a COM-owned wide string for return to the shell, which frees it
    /// with `CoTaskMemFree`.
    fn alloc_pwstr(s: &str) -> Result<PWSTR> {
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * std::mem::size_of::<u16>();
        let ptr = unsafe { CoTaskMemAlloc(bytes) } as *mut u16;
        if ptr.is_null() {
            return Err(Error::from_hresult(E_OUTOFMEMORY));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        }
        Ok(PWSTR(ptr))
    }

    /// Filesystem path of the first item in the selection, if any.
    fn first_path(items: &IShellItemArray) -> Option<String> {
        let item: IShellItem = unsafe { items.GetItemAt(0) }.ok()?;
        let pw: PWSTR = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
        let s = unsafe { pw.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(pw.0 as *const c_void)) };
        s
    }

    /// `(selection count, first item's filesystem path)`.
    fn selection(items: &IShellItemArray) -> (usize, Option<String>) {
        let count = unsafe { items.GetCount() }.unwrap_or(0) as usize;
        let path = if count >= 1 { first_path(items) } else { None };
        (count, path)
    }

    /// First item's path from a (possibly null) `IShellItemArray` argument.
    fn item_path(items: Ref<IShellItemArray>) -> Option<String> {
        items.as_ref().and_then(first_path)
    }

    /// Is `path` a directory? Stat-only (`Path::is_dir` reads attributes, never
    /// data), so it cannot recall a placeholder. Feeds the per-leaf folder
    /// reduced set ([`leaf_hidden_for_folder`]).
    fn path_is_folder(path: &str) -> bool {
        Path::new(path).is_dir()
    }

    /// Cache-first file status, querying the pipe on a miss only when the shell
    /// permits a slow call (`foktobeslow`). Reuses the overlay's query path.
    ///
    /// A miss under `!ok_to_be_slow` is [`StatusLookup::Pending`], **not** "untracked":
    /// the two are indistinguishable in `Option<FileStatus>`, and collapsing them is
    /// what made the menu vanish on any file the overlay had not touched in 30 s.
    fn status_for(path: &str, ok_to_be_slow: bool) -> StatusLookup {
        let p = Path::new(path);
        if let Some(s) = crate::global().cache.get(p) {
            return StatusLookup::Known(Some(s));
        }
        if ok_to_be_slow {
            return StatusLookup::Known(crate::overlay::com::query_status(p));
        }
        StatusLookup::Pending
    }

    /// `IExplorerCommand::GetState` value for the root submenu given a selection.
    ///
    /// `Err(E_PENDING)` asks the shell to re-query on a background thread; see
    /// [`menu_state`].
    fn root_state(items: Ref<IShellItemArray>, ok_to_be_slow: bool) -> Result<u32> {
        let Some(arr) = items.as_ref() else {
            return Ok(ECS_HIDDEN.0 as u32);
        };
        let (count, path) = selection(arr);
        let lookup = match path.as_deref() {
            Some(p) => status_for(p, ok_to_be_slow),
            None => StatusLookup::Known(None),
        };
        match menu_state(lookup, count) {
            MenuState::Enabled => Ok(ECS_ENABLED.0 as u32),
            MenuState::Hidden => Ok(ECS_HIDDEN.0 as u32),
            MenuState::Pending => Err(Error::from_hresult(E_PENDING)),
        }
    }

    /// Query the service for device info and format the menu title (or the
    /// "unavailable" fallback when the service is down / errors).
    fn device_title_for(path: &str) -> String {
        match SyncPipeClient::connect_pipe() {
            Ok(client) => {
                let resp = client
                    .request(RequestMethod::GetFileDevices {
                        path: path.to_string(),
                    })
                    .ok();
                device_title(resp.as_ref())
            }
            Err(_) => format_info_unavailable(),
        }
    }

    /// The submenu's rows for `path`: the file's real, nest-backed version history.
    ///
    /// Runs when Explorer *expands* the Version history item, never on menu render —
    /// so the common right-click costs no pipe call. A dead service yields the single
    /// disabled "unavailable" row rather than an error dialog.
    fn version_rows_for(path: &str) -> Vec<super::VersionMenuItem> {
        match SyncPipeClient::connect_pipe() {
            Ok(client) => {
                let resp = client
                    .request(RequestMethod::ListFileVersions {
                        path: path.to_string(),
                    })
                    .ok();
                version_submenu_rows(resp.as_ref())
            }
            Err(_) => version_submenu_rows(None),
        }
    }

    /// Invoke a restore: record it on the nest, re-point this device's copy, notify.
    ///
    /// The service does the whole job behind `RestoreFileVersion`; the shell only maps
    /// the reply to a message. Bounded by `SyncPipeClient::REQUEST_TIMEOUT`, because
    /// this runs on `explorer.exe`'s thread.
    fn invoke_restore(path: &str, version_num: i64) {
        let outcome = match SyncPipeClient::connect_pipe() {
            Ok(client) => {
                let resp = client
                    .request(RequestMethod::RestoreFileVersion {
                        path: path.to_string(),
                        version_num,
                    })
                    .ok();
                restore_outcome(resp.as_ref())
            }
            Err(_) => restore_outcome(None),
        };
        match outcome {
            RestoreOutcome::Restored(msg) | RestoreOutcome::Failed(msg) => {
                notify(&app_name(), &msg)
            }
        }
    }

    /// Best-effort user notification — a restore's outcome, or a Share whose
    /// hand-off could not happen.
    fn notify(caption: &str, text: &str) {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
        use windows::core::PCWSTR;
        let cap: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
        let txt: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(txt.as_ptr()),
                PCWSTR(cap.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// Ask the agent where the app should open to share `path` (`ShareFile`),
    /// as a route; `None` = no Share here. Bounded by
    /// `SyncPipeClient::REQUEST_TIMEOUT` — this runs inside `explorer.exe`.
    fn share_route_for(path: &str) -> Option<AppRoute> {
        let client = SyncPipeClient::connect_pipe().ok()?;
        let resp = client
            .request(RequestMethod::ShareFile {
                path: path.to_string(),
            })
            .ok();
        share_route(resp.as_ref())
    }

    /// Invoke the Share leaf: hand off to the app (`apps/windows.md` § Shell
    /// Extension → *The Share hand-off*, step 2). The route is opened through
    /// `ShellExecuteW`, which carries Explorer's user-initiated foreground right
    /// to the app the `fauna://` registration starts; the agent never mints.
    fn invoke_share(path: &str) {
        let Some(route) = share_route_for(path) else {
            notify(&app_name(), &share_not_available());
            return;
        };
        if !open_uri(&route.to_uri()) {
            notify(&app_name(), &share_open_failed());
        }
    }

    /// Open `uri` with its registered handler; `false` when the shell could not
    /// (`ShellExecuteW` reports failure as a value ≤ 32).
    fn open_uri(uri: &str) -> bool {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{PCWSTR, w};
        let wide: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        result.0 as isize > 32
    }

    // ── Root submenu ──────────────────────────────────────────────────────────

    /// Root "Fauna" submenu command, registered as a `ContextMenuHandlers` handler.
    #[implement(IExplorerCommand)]
    pub struct FaunaContextMenu;

    impl FaunaContextMenu {
        pub fn new() -> Self {
            crate::dll::object_added();
            Self
        }
    }

    impl Drop for FaunaContextMenu {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IExplorerCommand_Impl for FaunaContextMenu_Impl {
        fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            alloc_pwstr(&app_name())
        }
        fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetCanonicalName(&self) -> Result<GUID> {
            Ok(clsid::CONTEXT_MENU)
        }
        fn GetState(&self, items: Ref<IShellItemArray>, foktobeslow: BOOL) -> Result<u32> {
            root_state(items, foktobeslow.as_bool())
        }
        fn Invoke(&self, _items: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
            Ok(()) // the root has no direct action; the sub-commands do
        }
        fn GetFlags(&self) -> Result<u32> {
            Ok(ECF_HASSUBCOMMANDS.0 as u32)
        }
        fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
            Ok(SubCommandEnum::root().into())
        }
    }

    // ── Sub-command enumerator ────────────────────────────────────────────────

    /// Enumerates an arbitrary list of sub-commands — the root submenu's three leaves,
    /// or the **Version history** item's per-version children.
    ///
    /// It takes its items rather than building them, for two reasons beyond reuse: the
    /// cursor's clamp follows the real length, and `Clone()` can hand out the *same*
    /// child objects. Rebuilding fresh leaves in `Clone` was harmless while every leaf
    /// was stateless, but a version child carries `(path, version_num)` captured at
    /// enumeration time — a rebuilt clone would lose it.
    #[implement(IEnumExplorerCommand)]
    pub struct SubCommandEnum {
        items: Vec<IExplorerCommand>,
        cursor: Mutex<usize>,
    }

    impl SubCommandEnum {
        /// The root "Fauna" submenu: Share + device info + version history.
        pub fn root() -> Self {
            Self::new(vec![
                FaunaShareCommand::new().into(),
                FaunaInfoDevices::new().into(),
                FaunaInfoVersions::new().into(),
            ])
        }

        pub fn new(items: Vec<IExplorerCommand>) -> Self {
            Self::with_cursor(items, 0)
        }

        fn with_cursor(items: Vec<IExplorerCommand>, cursor: usize) -> Self {
            crate::dll::object_added();
            let cursor = cursor.min(items.len());
            Self {
                items,
                cursor: Mutex::new(cursor),
            }
        }
    }

    impl Drop for SubCommandEnum {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IEnumExplorerCommand_Impl for SubCommandEnum_Impl {
        fn Next(
            &self,
            celt: u32,
            puicommand: *mut Option<IExplorerCommand>,
            pceltfetched: *mut u32,
        ) -> HRESULT {
            if puicommand.is_null() {
                return E_POINTER;
            }
            let mut cursor = self.cursor.lock().unwrap();
            let mut fetched = 0u32;
            while fetched < celt && *cursor < self.items.len() {
                let cmd = self.items[*cursor].clone();
                unsafe { puicommand.add(fetched as usize).write(Some(cmd)) };
                *cursor += 1;
                fetched += 1;
            }
            if !pceltfetched.is_null() {
                unsafe { *pceltfetched = fetched };
            }
            if fetched == celt { S_OK } else { S_FALSE }
        }

        fn Skip(&self, celt: u32) -> Result<()> {
            let mut cursor = self.cursor.lock().unwrap();
            *cursor = (*cursor + celt as usize).min(self.items.len());
            Ok(())
        }

        fn Reset(&self) -> Result<()> {
            *self.cursor.lock().unwrap() = 0;
            Ok(())
        }

        fn Clone(&self) -> Result<IEnumExplorerCommand> {
            let pos = *self.cursor.lock().unwrap();
            // Cloning an `IExplorerCommand` is an AddRef, so the clone enumerates the
            // *same* command objects — required for the stateful version children.
            Ok(SubCommandEnum::with_cursor(self.items.clone(), pos).into())
        }
    }

    // ── Leaf: Share action ────────────────────────────────────────────────────

    /// "Share" command — calls `ShareFile` and notifies the user of the result.
    #[implement(IExplorerCommand)]
    pub struct FaunaShareCommand;

    impl FaunaShareCommand {
        pub fn new() -> Self {
            crate::dll::object_added();
            Self
        }
    }

    impl Drop for FaunaShareCommand {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IExplorerCommand_Impl for FaunaShareCommand_Impl {
        fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            alloc_pwstr(&share_title())
        }
        fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetCanonicalName(&self) -> Result<GUID> {
            Ok(clsid::SHARE_COMMAND)
        }
        /// Present only where the agent names a target (`ShareFile` →
        /// `share_route`): a file in a public set, or a bound set's own root.
        /// The pipe round-trip is not allowed on Explorer's UI-thread pass, so
        /// that pass answers `E_PENDING` — the root's contract (`menu_state`) —
        /// and the background re-query decides.
        fn GetState(&self, items: Ref<IShellItemArray>, foktobeslow: BOOL) -> Result<u32> {
            if !foktobeslow.as_bool() {
                return Err(Error::from_hresult(E_PENDING));
            }
            let shown = item_path(items).is_some_and(|path| share_route_for(&path).is_some());
            Ok(if shown { ECS_ENABLED.0 } else { ECS_HIDDEN.0 } as u32)
        }
        fn Invoke(&self, items: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
            if let Some(path) = item_path(items) {
                invoke_share(&path);
            }
            Ok(())
        }
        fn GetFlags(&self) -> Result<u32> {
            Ok(ECF_DEFAULT.0 as u32)
        }
        fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
    }

    // ── Leaf: device info (read-only) ─────────────────────────────────────────

    /// Read-only "Synced to N devices" item; its title is fetched on open.
    #[implement(IExplorerCommand)]
    pub struct FaunaInfoDevices;

    impl FaunaInfoDevices {
        pub fn new() -> Self {
            crate::dll::object_added();
            Self
        }
    }

    impl Drop for FaunaInfoDevices {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IExplorerCommand_Impl for FaunaInfoDevices_Impl {
        fn GetTitle(&self, items: Ref<IShellItemArray>) -> Result<PWSTR> {
            let title = match item_path(items) {
                Some(path) => device_title_for(&path),
                None => format_info_unavailable(),
            };
            alloc_pwstr(&title)
        }
        fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetCanonicalName(&self) -> Result<GUID> {
            Ok(clsid::INFO_DEVICES)
        }
        fn GetState(&self, items: Ref<IShellItemArray>, _foktobeslow: BOOL) -> Result<u32> {
            // Folder reduced set: device info is a per-file read — hidden on a
            // folder rather than rendering a permanent "unavailable".
            let folder = item_path(items).is_some_and(|p| path_is_folder(&p));
            if folder && super::leaf_hidden_for_folder(super::SubmenuLeaf::Devices) {
                return Ok(ECS_HIDDEN.0 as u32);
            }
            // Informational, not a command — disabled (grayed) so it never reads as
            // an actionable item with a no-op Invoke. Same convention the version
            // submenu uses for its own informational rows (the "(current)" head).
            Ok(ECS_DISABLED.0 as u32)
        }
        fn Invoke(&self, _items: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
            Ok(()) // read-only info item
        }
        fn GetFlags(&self) -> Result<u32> {
            Ok(ECF_DEFAULT.0 as u32)
        }
        fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
    }

    // ── Leaf: version history (nested submenu) ────────────────────────────────

    /// "Version history" — a nested submenu of the file's real versions, newest first;
    /// invoking an older one restores it (`file-sync.md` § Restore).
    ///
    /// ⚠ **The COM trap this class exists to work around.**
    /// `IExplorerCommand::EnumSubCommands(&self)` receives **no `IShellItemArray`**, so
    /// it cannot learn which file was right-clicked. `GetTitle` and `GetState` *do* get
    /// the selection, and Explorer always calls at least one of them before expanding a
    /// submenu — so the path is captured there and read back here.
    #[implement(IExplorerCommand)]
    pub struct FaunaInfoVersions {
        last_path: Mutex<Option<String>>,
    }

    impl FaunaInfoVersions {
        pub fn new() -> Self {
            crate::dll::object_added();
            Self {
                last_path: Mutex::new(None),
            }
        }

        /// Remember the selected path for the `EnumSubCommands` that follows.
        fn remember(&self, items: Ref<IShellItemArray>) {
            if let Some(path) = item_path(items) {
                *self.last_path.lock().unwrap() = Some(path);
            }
        }
    }

    impl Drop for FaunaInfoVersions {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IExplorerCommand_Impl for FaunaInfoVersions_Impl {
        fn GetTitle(&self, items: Ref<IShellItemArray>) -> Result<PWSTR> {
            // Static: the history is fetched when the submenu is expanded, not when the
            // menu is drawn, so a right-click never pays a pipe round-trip for it.
            self.remember(items);
            alloc_pwstr(&version_history_title())
        }
        fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetCanonicalName(&self) -> Result<GUID> {
            Ok(clsid::INFO_VERSIONS)
        }
        fn GetState(&self, items: Ref<IShellItemArray>, _foktobeslow: BOOL) -> Result<u32> {
            // Folder reduced set: version history is per-file — hidden on a folder.
            match item_path(items) {
                Some(p)
                    if path_is_folder(&p)
                        && super::leaf_hidden_for_folder(super::SubmenuLeaf::Versions) =>
                {
                    Ok(ECS_HIDDEN.0 as u32)
                }
                Some(p) => {
                    *self.last_path.lock().unwrap() = Some(p);
                    Ok(ECS_ENABLED.0 as u32)
                }
                None => Ok(ECS_ENABLED.0 as u32),
            }
        }
        fn Invoke(&self, _items: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
            Ok(()) // the submenu's children carry the actions
        }
        fn GetFlags(&self) -> Result<u32> {
            Ok(ECF_HASSUBCOMMANDS.0 as u32)
        }
        fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
            let path = self.last_path.lock().unwrap().clone();
            // No captured path (Explorer expanded without ever asking title/state) →
            // the same disabled "unavailable" row a dead service yields.
            let items: Vec<IExplorerCommand> = match path {
                Some(path) => version_rows_for(&path)
                    .into_iter()
                    .map(|row| FaunaVersionCommand::new(path.clone(), row).into())
                    .collect(),
                None => version_submenu_rows(None)
                    .into_iter()
                    .map(|row| FaunaVersionCommand::new(String::new(), row).into())
                    .collect(),
            };
            Ok(SubCommandEnum::new(items).into())
        }
    }

    // ── Child: one version row ────────────────────────────────────────────────

    /// One row of the Version history submenu, carrying the `(path, version_num)` it
    /// was enumerated for.
    ///
    /// **Returned object, never CLSID-activated** — Explorer gets it from
    /// `EnumSubCommands`, so it needs no registry entry and the 8-CLSID contract
    /// (`installers/windows.md` § Shell Extension) is unchanged.
    #[implement(IExplorerCommand)]
    pub struct FaunaVersionCommand {
        /// The path captured when this child was enumerated.
        path: String,
        row: super::VersionMenuItem,
    }

    impl FaunaVersionCommand {
        pub fn new(path: String, row: super::VersionMenuItem) -> Self {
            crate::dll::object_added();
            Self { path, row }
        }
    }

    impl Drop for FaunaVersionCommand {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    impl IExplorerCommand_Impl for FaunaVersionCommand_Impl {
        fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            alloc_pwstr(&self.row.title)
        }
        fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
        fn GetCanonicalName(&self) -> Result<GUID> {
            // A dynamic child has no canonical name (and no CLSID); GUID_NULL is the
            // documented "none" answer.
            Ok(GUID::zeroed())
        }
        fn GetState(&self, _items: Ref<IShellItemArray>, _foktobeslow: BOOL) -> Result<u32> {
            // The current head and the "no versions"/"unavailable" placeholders are
            // shown for context but are not restorable.
            Ok(if self.row.enabled {
                ECS_ENABLED.0 as u32
            } else {
                ECS_DISABLED.0 as u32
            })
        }
        fn Invoke(&self, items: Ref<IShellItemArray>, _pbc: Ref<IBindCtx>) -> Result<()> {
            if !self.row.enabled {
                return Ok(());
            }
            // `Invoke` *does* get the live selection. Assert it still matches the path
            // captured at enumeration time, so a stale enumeration can never restore
            // version N of file A onto file B.
            let Some(live) = item_path(items) else {
                return Ok(());
            };
            if !same_file_path(&live, &self.path) {
                return Ok(());
            }
            invoke_restore(&self.path, self.row.version_num);
            Ok(())
        }
        fn GetFlags(&self) -> Result<u32> {
            Ok(ECF_DEFAULT.0 as u32)
        }
        fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
            Err(Error::from_hresult(E_NOTIMPL))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::context_menu::VersionMenuItem;

        fn row(version_num: i64, enabled: bool) -> VersionMenuItem {
            VersionMenuItem {
                version_num,
                title: format!("Mar 22 — 2.0 KB (v{version_num})"),
                enabled,
            }
        }

        fn lock() -> std::sync::MutexGuard<'static, ()> {
            crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        }

        /// The submenu hook. Without `ECF_HASSUBCOMMANDS` Explorer never calls
        /// `EnumSubCommands`, and the version history silently never opens.
        #[test]
        fn info_versions_advertises_a_submenu() {
            let _serial = lock();
            let cmd: IExplorerCommand = FaunaInfoVersions::new().into();
            let flags = unsafe { cmd.GetFlags() }.unwrap();
            assert_eq!(flags, ECF_HASSUBCOMMANDS.0 as u32);
        }

        /// The parent title is static — no pipe call when the menu is merely drawn.
        #[test]
        fn info_versions_title_is_static() {
            let _serial = lock();
            let cmd: IExplorerCommand = FaunaInfoVersions::new().into();
            let pw = unsafe { cmd.GetTitle(None) }.unwrap();
            let title = unsafe { pw.to_string() }.unwrap();
            unsafe { CoTaskMemFree(Some(pw.0 as *const c_void)) };
            assert_eq!(title, "Version history");
        }

        /// A disabled row (the current head, or a placeholder) must not be invocable.
        #[test]
        fn version_child_state_follows_the_row() {
            let _serial = lock();
            let enabled: IExplorerCommand =
                FaunaVersionCommand::new("C:\\a.txt".into(), row(4, true)).into();
            let disabled: IExplorerCommand =
                FaunaVersionCommand::new("C:\\a.txt".into(), row(9, false)).into();

            assert_eq!(
                unsafe { enabled.GetState(None, false) }.unwrap(),
                ECS_ENABLED.0 as u32
            );
            assert_eq!(
                unsafe { disabled.GetState(None, false) }.unwrap(),
                ECS_DISABLED.0 as u32
            );
        }

        /// The read-only device-info leaf must render disabled (grayed, non-clickable),
        /// the same shape the version submenu already uses for its own informational
        /// rows (the "(current)" head, above) — not `ECS_ENABLED`, which reads as an
        /// actionable item even though `Invoke` is a no-op (user-confusing, 2026-07-17).
        #[test]
        fn info_devices_state_is_disabled_not_enabled() {
            let _serial = lock();
            let cmd: IExplorerCommand = FaunaInfoDevices::new().into();
            assert_eq!(
                unsafe { cmd.GetState(None, false) }.unwrap(),
                ECS_DISABLED.0 as u32
            );
        }

        /// A child is a returned object, not a registered class — it has no CLSID, so
        /// its canonical name is GUID_NULL. (This is what keeps the 8-CLSID contract.)
        #[test]
        fn version_child_has_no_canonical_clsid() {
            let _serial = lock();
            let cmd: IExplorerCommand =
                FaunaVersionCommand::new("C:\\a.txt".into(), row(4, true)).into();
            assert_eq!(unsafe { cmd.GetCanonicalName() }.unwrap(), GUID::zeroed());
        }

        /// A disabled child ignores `Invoke` even if Explorer somehow dispatches it —
        /// restoring the current head would append a no-op record, and the placeholder
        /// rows carry a meaningless `version_num` (-1).
        #[test]
        fn disabled_version_child_invoke_is_a_noop() {
            let _serial = lock();
            let cmd: IExplorerCommand =
                FaunaVersionCommand::new("C:\\a.txt".into(), row(-1, false)).into();
            // No selection is passed, so even an enabled row would bail before the pipe;
            // the point is that this neither panics nor touches the service.
            unsafe { cmd.Invoke(None, None) }.unwrap();
        }

        /// The generalized enumerator serves exactly the items it was given — the old
        /// hardcoded `min(3)` clamp would have mis-clamped any other length.
        #[test]
        fn sub_command_enum_serves_its_own_items() {
            let _serial = lock();
            let items: Vec<IExplorerCommand> = (0..5)
                .map(|i| FaunaVersionCommand::new("C:\\a.txt".into(), row(i, true)).into())
                .collect();
            let e: IEnumExplorerCommand = SubCommandEnum::new(items).into();

            let mut buf: [Option<IExplorerCommand>; 5] = [const { None }; 5];
            let mut fetched = 0u32;
            let hr = unsafe { e.Next(&mut buf, Some(&mut fetched)) };
            assert!(hr.is_ok());
            assert_eq!(fetched, 5, "all five children enumerate");
            assert!(buf.iter().all(|c| c.is_some()));

            // Exhausted: a further Next fetches nothing and reports S_FALSE.
            let mut more: [Option<IExplorerCommand>; 1] = [const { None }];
            let mut n = 0u32;
            let hr = unsafe { e.Next(&mut more, Some(&mut n)) };
            assert_eq!(hr, S_FALSE);
            assert_eq!(n, 0);
        }

        /// `Clone()` must hand out the **same** child objects at the same cursor — a
        /// clone that rebuilt fresh leaves would drop each child's captured
        /// `(path, version_num)`.
        #[test]
        fn sub_command_enum_clone_shares_children_and_position() {
            let _serial = lock();
            let child: IExplorerCommand =
                FaunaVersionCommand::new("C:\\a.txt".into(), row(4, true)).into();
            let e: IEnumExplorerCommand = SubCommandEnum::new(vec![child.clone(), child]).into();

            // Consume one, then clone: the clone resumes at the same position.
            let mut one: [Option<IExplorerCommand>; 1] = [const { None }];
            let mut n = 0u32;
            assert!(unsafe { e.Next(&mut one, Some(&mut n)) }.is_ok());
            assert_eq!(n, 1);

            let cloned = unsafe { e.Clone() }.unwrap();
            let mut rest: [Option<IExplorerCommand>; 2] = [const { None }; 2];
            let mut m = 0u32;
            // S_FALSE (fewer than requested) — not an error, so don't unwrap the HRESULT.
            let _ = unsafe { cloned.Next(&mut rest, Some(&mut m)) };
            assert_eq!(m, 1, "clone resumes at the original cursor, not from 0");

            // Same underlying object, reached through the clone.
            let title = unsafe { rest[0].as_ref().unwrap().GetTitle(None) }.unwrap();
            let s = unsafe { title.to_string() }.unwrap();
            unsafe { CoTaskMemFree(Some(title.0 as *const c_void)) };
            assert!(s.contains("v4"), "got: {s}");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The version submenu's date stamp names the day the version was saved.
    ///
    /// Nothing pinned this before: `version_info_one_with_timestamp` below
    /// carries a comment computing "2026-03-22 → Mar 22" and then asserts only
    /// that the string starts with `"1 saved version ("` and ends with `")"` —
    /// every date renders identically under that assertion, including a wrong
    /// one. So the stamp shipped unobserved.
    ///
    /// The cases are chosen to separate the two things the hand-rolled
    /// arithmetic could get wrong: whether it lands in the right year (the
    /// 1..3 rows walk a non-leap year end to end) and whether it survives a
    /// **leap day** (the 4..6 rows are the same calendar dates in 2024, where a
    /// fixed 28-day February shifts every date from March onward by one).
    #[test]
    fn the_version_stamp_names_the_day_including_across_a_leap_february() {
        for (ts, expected, what) in [
            (1_672_531_200_u64, "Jan 1", "2023-01-01, a year boundary"),
            (1_677_628_800, "Mar 1", "2023-03-01, just past a 28-day Feb"),
            (
                1_703_980_800,
                "Dec 31",
                "2023-12-31, the last day of a year",
            ),
            (1_704_067_200, "Jan 1", "2024-01-01, a leap year begins"),
            (1_709_164_800, "Feb 29", "2024-02-29, the leap day itself"),
            (
                1_709_251_200,
                "Mar 1",
                "2024-03-01, the day after the leap day",
            ),
        ] {
            assert_eq!(format_short_date(ts), expected, "{what} (ts {ts})");
        }
    }

    // ── Version-history submenu ──────────────────────────────────────────────

    fn entry(seq: i64, size: i64, created_at: i64) -> FileVersionEntry {
        FileVersionEntry {
            version_num: seq,
            size_bytes: size,
            created_at,
        }
    }

    #[test]
    fn version_menu_shows_newest_first_and_marks_current() {
        // Nest order is oldest→newest; seqs are sparse `sync_changes` rows.
        let items = version_menu_items(&[
            entry(4, 100, 1_711_100_000),
            entry(9, 2_048, 1_711_200_000),
            entry(21, 5_242_880, 1_711_300_000),
        ]);

        assert_eq!(items.len(), 3);
        // Newest first.
        assert_eq!(items[0].version_num, 21);
        assert_eq!(items[1].version_num, 9);
        assert_eq!(items[2].version_num, 4);

        // The head is the file's current content: shown, marked, not restorable.
        assert!(items[0].title.ends_with("(current)"), "{}", items[0].title);
        assert!(!items[0].enabled);

        // Everything older is restorable.
        assert!(items[1].enabled);
        assert!(items[2].enabled);
        assert!(!items[1].title.contains("current"));
        assert!(items[1].title.contains("2 KB"));
        assert!(items[2].title.contains("100 B"));
    }

    #[test]
    fn version_menu_never_renders_an_empty_popup() {
        let items = version_menu_items(&[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "No saved versions");
        assert!(!items[0].enabled);
    }

    #[test]
    fn version_menu_with_one_version_offers_nothing_to_restore() {
        let items = version_menu_items(&[entry(7, 10, 1_711_100_000)]);
        assert_eq!(items.len(), 1);
        assert!(!items[0].enabled, "the only version IS the current content");
        assert!(items[0].title.ends_with("(current)"));
    }

    // ── Submenu rows (the enumerator's input) ─────────────────────────────────

    /// "Couldn't ask" and "no history" are different facts and must read differently,
    /// but neither may ever produce an empty popup.
    #[test]
    fn version_submenu_rows_separate_unavailable_from_empty_history() {
        use fauna_ipc::sync::FileVersionListInfo;

        let unavailable = version_submenu_rows(None);
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].title, "File info unavailable");
        assert!(!unavailable[0].enabled);

        let empty = resp_ok(ResponsePayload::FileVersionList(FileVersionListInfo {
            path: "C:\\f.txt".into(),
            folder: "docs".into(),
            versions: vec![],
        }));
        let rows = version_submenu_rows(Some(&empty));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "No saved versions");
        assert!(!rows[0].enabled);
    }

    #[test]
    fn version_submenu_rows_map_a_real_history() {
        use fauna_ipc::sync::FileVersionListInfo;
        let r = resp_ok(ResponsePayload::FileVersionList(FileVersionListInfo {
            path: "C:\\f.txt".into(),
            folder: "docs".into(),
            versions: vec![entry(4, 100, 1_711_100_000), entry(9, 2048, 1_711_200_000)],
        }));
        let rows = version_submenu_rows(Some(&r));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].version_num, 9, "newest first");
        assert!(!rows[0].enabled, "head is current");
        assert!(rows[1].enabled, "older is restorable");
        assert_eq!(rows[1].version_num, 4);
    }

    // ── Restore outcome ───────────────────────────────────────────────────────

    #[test]
    fn restore_outcome_ok_is_restored() {
        let r = resp_ok(ResponsePayload::Empty);
        assert_eq!(
            restore_outcome(Some(&r)),
            RestoreOutcome::Restored("Version restored.".to_string())
        );
    }

    /// A service-side failure surfaces its own message — the user needs to know the
    /// file did NOT change.
    #[test]
    fn restore_outcome_err_carries_the_service_message() {
        let r = Response {
            id: 1,
            result: ResponseResult::Err("file is not in a synced folder".into()),
        };
        assert_eq!(
            restore_outcome(Some(&r)),
            RestoreOutcome::Failed(
                "Could not restore version: file is not in a synced folder".to_string()
            )
        );
    }

    #[test]
    fn restore_outcome_dead_pipe_is_failure_not_success() {
        assert!(matches!(restore_outcome(None), RestoreOutcome::Failed(_)));
        // A wrong payload must never read as success.
        let wrong = resp_ok(ResponsePayload::FileDevices(FileDevicesInfo {
            path: "C:\\f.txt".into(),
            device_count: 1,
        }));
        assert!(matches!(
            restore_outcome(Some(&wrong)),
            RestoreOutcome::Failed(_)
        ));
    }

    // ── Stale-enumeration guard ───────────────────────────────────────────────

    /// A version child captures its path at enumeration time; `Invoke` re-checks it
    /// against the live selection so version N of file A can never land on file B.
    #[test]
    fn same_file_path_is_case_insensitive_but_not_permissive() {
        assert!(same_file_path(r"C:\Sync\a.txt", r"C:\Sync\a.txt"));
        // Windows paths are case-insensitive; a re-cased path is the same file.
        assert!(same_file_path(r"C:\Sync\A.TXT", r"c:\sync\a.txt"));
        // Different files never match.
        assert!(!same_file_path(r"C:\Sync\a.txt", r"C:\Sync\b.txt"));
        // A child enumerated with no path (the "unavailable" row) matches nothing real.
        assert!(!same_file_path(r"C:\Sync\a.txt", ""));
    }

    #[test]
    fn format_size_scales() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        // Delegated to the shared `fauna_core::format::byte_size`, whose
        // `fmt_one_decimal` drops a trailing ".0" (was "2.0 KB" locally) and
        // which the local version this replaced had no TB tier for at all.
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(5_242_880), "5 MB");
        assert_eq!(format_size(3_221_225_472), "3 GB");
        assert_eq!(format_size(1024i64.pow(4)), "1 TB");
        assert_eq!(format_size(1536), "1.5 KB");
        // Defensive: a negative size is nonsense, not a panic.
        assert_eq!(format_size(-5), "0 B");
    }

    #[test]
    fn version_list_distinguishes_cannot_ask_from_no_history() {
        use fauna_ipc::sync::FileVersionListInfo;

        // Service down / no response at all.
        assert_eq!(version_list(None), None);

        // Service answered with an error.
        let err = Response {
            id: 1,
            result: ResponseResult::Err("nest unreachable".into()),
        };
        assert_eq!(version_list(Some(&err)), None);

        // Wrong payload → treated as "couldn't ask", never as an empty history.
        let wrong = Response {
            id: 1,
            result: ResponseResult::Ok(ResponsePayload::Empty),
        };
        assert_eq!(version_list(Some(&wrong)), None);

        // A real, empty history is Some(vec![]) — distinct from None.
        let empty = Response {
            id: 1,
            result: ResponseResult::Ok(ResponsePayload::FileVersionList(FileVersionListInfo {
                path: "p".into(),
                folder: String::new(),
                versions: vec![],
            })),
        };
        assert_eq!(version_list(Some(&empty)), Some(vec![]));
    }

    // ── Visibility tests (7) ──────────────────────────────────────────────────

    #[test]
    fn show_for_synced() {
        assert!(should_show_menu(Some(FileStatus::Synced), 1));
    }

    #[test]
    fn show_for_syncing() {
        assert!(should_show_menu(Some(FileStatus::Syncing), 1));
    }

    #[test]
    fn show_for_cloud_only() {
        assert!(should_show_menu(Some(FileStatus::CloudOnly), 1));
    }

    #[test]
    fn show_for_error() {
        assert!(should_show_menu(Some(FileStatus::Error), 1));
    }

    #[test]
    fn hide_for_not_tracked() {
        assert!(!should_show_menu(Some(FileStatus::NotTracked), 1));
    }

    #[test]
    fn hide_for_none_status() {
        assert!(!should_show_menu(None, 1));
    }

    // ── GetState: the E_PENDING contract ────────────────────────────────────
    //
    // Explorer calls GetState with fOkToBeSlow = FALSE on its UI thread. A status
    // cache miss there means "not known YET", not "untracked" — and answering
    // ECS_HIDDEN is a *permanent* hide. With a 30 s cache TTL that silently loses the
    // menu on any file the overlay hasn't touched recently. E_PENDING is the
    // documented "re-ask me on a background thread".

    #[test]
    fn cache_miss_on_the_fast_path_is_pending_not_hidden() {
        assert_eq!(menu_state(StatusLookup::Pending, 1), MenuState::Pending);
    }

    #[test]
    fn known_tracked_status_enables_the_menu() {
        assert_eq!(
            menu_state(StatusLookup::Known(Some(FileStatus::Synced)), 1),
            MenuState::Enabled
        );
    }

    /// A definitive "untracked" is a *hide*, not a pending — we know the answer.
    #[test]
    fn known_untracked_hides_rather_than_pends() {
        assert_eq!(menu_state(StatusLookup::Known(None), 1), MenuState::Hidden);
        assert_eq!(
            menu_state(StatusLookup::Known(Some(FileStatus::NotTracked)), 1),
            MenuState::Hidden
        );
    }

    /// A selection that can never show the menu must not cost a background re-query.
    #[test]
    fn multi_select_hides_even_when_status_is_pending() {
        assert_eq!(menu_state(StatusLookup::Pending, 2), MenuState::Hidden);
        assert_eq!(menu_state(StatusLookup::Pending, 0), MenuState::Hidden);
    }

    #[test]
    fn hide_for_multi_select() {
        assert!(!should_show_menu(Some(FileStatus::Synced), 2));
        assert!(!should_show_menu(Some(FileStatus::Synced), 0));
    }

    // ── Device info tests (3) ─────────────────────────────────────────────────

    #[test]
    fn device_info_zero() {
        let info = FileDevicesInfo {
            path: "C:\\file.txt".to_string(),
            device_count: 0,
        };
        assert_eq!(format_device_info(&info), "Not synced to any device");
    }

    #[test]
    fn device_info_one() {
        let info = FileDevicesInfo {
            path: "C:\\file.txt".to_string(),
            device_count: 1,
        };
        assert_eq!(format_device_info(&info), "On this device only");
    }

    #[test]
    fn device_info_many() {
        let info = FileDevicesInfo {
            path: "C:\\file.txt".to_string(),
            device_count: 3,
        };
        assert_eq!(format_device_info(&info), "Synced to 3 devices");
    }

    // ── Version info tests (5) ────────────────────────────────────────────────

    #[test]
    fn version_info_zero() {
        let info = FileVersionsInfo {
            path: "C:\\file.txt".to_string(),
            version_count: 0,
            latest_timestamp: None,
        };
        assert_eq!(format_version_info(&info), "No saved versions");
    }

    #[test]
    fn version_info_one_no_timestamp() {
        let info = FileVersionsInfo {
            path: "C:\\file.txt".to_string(),
            version_count: 1,
            latest_timestamp: None,
        };
        assert_eq!(format_version_info(&info), "1 saved version");
    }

    #[test]
    fn version_info_one_with_timestamp() {
        // 2026-03-22 00:00:00 UTC  →  Mar 22
        // 2026-03-22: days since epoch = (2026-1970)*365 + leap_days + 80
        // rough: 56*365 + 14 + 80 = 20440 + 14 + 80 = 20534 days
        // timestamp = 20534 * 86400 = 1_774_137_600
        let info = FileVersionsInfo {
            path: "C:\\file.txt".to_string(),
            version_count: 1,
            latest_timestamp: Some(1_774_137_600),
        };
        // Assert the whole string, date included. This previously checked only
        // the opening "1 saved version (" and the closing ")", which every
        // possible date satisfies — so the stamp the comment above computes was
        // never actually compared against anything.
        assert_eq!(format_version_info(&info), "1 saved version (Mar 22)");
    }

    #[test]
    fn version_info_many_no_timestamp() {
        let info = FileVersionsInfo {
            path: "C:\\file.txt".to_string(),
            version_count: 5,
            latest_timestamp: None,
        };
        assert_eq!(format_version_info(&info), "5 saved versions");
    }

    #[test]
    fn version_info_many_with_timestamp() {
        let info = FileVersionsInfo {
            path: "C:\\file.txt".to_string(),
            version_count: 5,
            latest_timestamp: Some(1_774_137_600),
        };
        let result = format_version_info(&info);
        assert!(
            result.starts_with("5 saved versions, latest "),
            "got: {}",
            result
        );
    }

    // ── Unavailable text (1) ──────────────────────────────────────────────────

    #[test]
    fn unavailable_text() {
        assert_eq!(format_info_unavailable(), "File info unavailable");
    }

    // ── Location mode toggle ──

    #[test]
    fn show_location_toggle_for_synced_location() {
        assert!(should_show_location_mode_toggle(true));
    }

    #[test]
    fn hide_location_toggle_for_non_synced() {
        assert!(!should_show_location_mode_toggle(false));
    }

    #[test]
    fn location_mode_label_always() {
        assert_eq!(location_mode_label("always"), "Make available on-demand");
    }

    #[test]
    fn location_mode_label_on_demand() {
        assert_eq!(
            location_mode_label("on-demand"),
            "Always keep on this device"
        );
    }

    // ── Pin visibility ──

    #[test]
    fn show_pin_for_placeholder() {
        assert!(should_show_pin(FileStatus::CloudOnly, false, true));
    }

    #[test]
    fn show_pin_for_synced_unpinned() {
        assert!(should_show_pin(FileStatus::Synced, false, true));
    }

    #[test]
    fn hide_pin_when_already_pinned() {
        assert!(!should_show_pin(FileStatus::Synced, true, true));
    }

    #[test]
    fn hide_pin_outside_on_demand_location() {
        assert!(!should_show_pin(FileStatus::CloudOnly, false, false));
    }

    // ── Unpin visibility ──

    #[test]
    fn show_unpin_when_pinned_in_on_demand() {
        assert!(should_show_unpin(true, true));
    }

    #[test]
    fn hide_unpin_when_not_pinned() {
        assert!(!should_show_unpin(false, true));
    }

    #[test]
    fn hide_unpin_outside_on_demand() {
        assert!(!should_show_unpin(true, false));
    }

    // ── Free space visibility ──

    #[test]
    fn show_free_space_for_synced_in_on_demand() {
        assert!(should_show_free_space(FileStatus::Synced, true));
    }

    #[test]
    fn hide_free_space_for_placeholder() {
        assert!(!should_show_free_space(FileStatus::CloudOnly, true));
    }

    // ── IPC response → title mapping (device) ─────────────────────────────────

    fn resp_ok(payload: ResponsePayload) -> Response {
        Response {
            id: 1,
            result: ResponseResult::Ok(payload),
        }
    }

    #[test]
    fn device_title_formats_ok_response() {
        let r = resp_ok(ResponsePayload::FileDevices(FileDevicesInfo {
            path: "C:\\f.txt".into(),
            device_count: 3,
        }));
        assert_eq!(device_title(Some(&r)), "Synced to 3 devices");
    }

    #[test]
    fn device_title_one_device() {
        let r = resp_ok(ResponsePayload::FileDevices(FileDevicesInfo {
            path: "C:\\f.txt".into(),
            device_count: 1,
        }));
        assert_eq!(device_title(Some(&r)), "On this device only");
    }

    #[test]
    fn device_title_none_is_unavailable() {
        assert_eq!(device_title(None), "File info unavailable");
    }

    #[test]
    fn device_title_err_response_is_unavailable() {
        let r = Response {
            id: 1,
            result: ResponseResult::Err("boom".into()),
        };
        assert_eq!(device_title(Some(&r)), "File info unavailable");
    }

    #[test]
    fn device_title_wrong_payload_is_unavailable() {
        let r = resp_ok(ResponsePayload::Empty);
        assert_eq!(device_title(Some(&r)), "File info unavailable");
    }

    // ── IPC response → title mapping (version) ────────────────────────────────

    #[test]
    fn version_title_formats_ok_response() {
        let r = resp_ok(ResponsePayload::FileVersions(FileVersionsInfo {
            path: "C:\\f.txt".into(),
            version_count: 5,
            latest_timestamp: Some(1_774_137_600),
        }));
        let t = version_title(Some(&r));
        assert!(t.starts_with("5 saved versions, latest "), "got: {t}");
    }

    #[test]
    fn version_title_zero_versions() {
        let r = resp_ok(ResponsePayload::FileVersions(FileVersionsInfo {
            path: "C:\\f.txt".into(),
            version_count: 0,
            latest_timestamp: None,
        }));
        assert_eq!(version_title(Some(&r)), "No saved versions");
    }

    #[test]
    fn version_title_none_is_unavailable() {
        assert_eq!(version_title(None), "File info unavailable");
    }

    // ── ShareFile response → the route the leaf opens (apps/windows.md § Shell
    //    Extension → The Share hand-off, step 2) ─────────────────────────────

    #[test]
    fn share_route_of_a_file_target_opens_the_create_surface() {
        let r = resp_ok(ResponsePayload::ShareTarget(
            fauna_ipc::sync::ShareTargetInfo {
                folder_id: 42,
                path: Some("photos/a b.jpg".into()),
            },
        ));
        let route = share_route(Some(&r)).expect("a file target is a route");
        assert_eq!(
            route,
            AppRoute::ShareLink {
                folder_id: 42,
                path: "photos/a b.jpg".into()
            }
        );
        // The URI the shell opens is the one the app's parser reads back.
        assert_eq!(AppRoute::parse(&route.to_uri()), Some(route));
    }

    #[test]
    fn share_route_of_a_set_root_opens_the_member_share_picker() {
        let r = resp_ok(ResponsePayload::ShareTarget(
            fauna_ipc::sync::ShareTargetInfo {
                folder_id: 7,
                path: None,
            },
        ));
        assert_eq!(
            share_route(Some(&r)).map(|r| r.to_uri()),
            Some("fauna://folder-share?folder=7".to_string())
        );
    }

    /// No target — a refusal, a dead pipe, or any other payload — is no route,
    /// which hides the leaf; there is no link-shaped reply to mishandle.
    #[test]
    fn share_route_is_none_without_a_target() {
        let refused = Response {
            id: 1,
            result: ResponseResult::Err(
                "only a file in a public folder can carry a share link".into(),
            ),
        };
        assert_eq!(share_route(Some(&refused)), None);
        assert_eq!(share_route(None), None);
        assert_eq!(share_route(Some(&resp_ok(ResponsePayload::Empty))), None);
        assert_eq!(share_not_available(), "This item can't be shared from here");
    }

    // ── End-to-end over a fake pipe (request → response → title) ──────────────
    //
    // Proves the `RequestMethod` / `ResponsePayload` variants wire together over
    // a real `SyncPipeClient`, deterministically (no live service). Uses an
    // in-memory blocking duplex + a one-shot "server" thread.
    #[test]
    fn device_title_over_fake_pipe() {
        use fauna_ipc::decode_payload;
        use fauna_ipc::sync::{Request, RequestMethod};
        use fauna_ipc::sync_pipe_client::{SyncPipeClient, read_frame, write_frame};

        let c2s = fake_pipe::Pipe::new(); // client → server
        let s2c = fake_pipe::Pipe::new(); // server → client

        let mut server_in = c2s.clone();
        let mut server_out = s2c.clone();
        let server = std::thread::spawn(move || {
            let payload = read_frame(&mut server_in).expect("server reads request");
            let req: Request = decode_payload(&payload).expect("decode request");
            let device_count = match req.method {
                RequestMethod::GetFileDevices { .. } => 2,
                _ => 0,
            };
            let resp = Response {
                id: req.id,
                result: ResponseResult::Ok(ResponsePayload::FileDevices(FileDevicesInfo {
                    path: "C:\\f.txt".into(),
                    device_count,
                })),
            };
            write_frame(&mut server_out, &resp).expect("server writes response");
        });

        let client = SyncPipeClient::from_streams(Box::new(s2c), Box::new(c2s));
        let resp = client
            .request(RequestMethod::GetFileDevices {
                path: "C:\\f.txt".into(),
            })
            .expect("response arrives");
        assert_eq!(device_title(Some(&resp)), "Synced to 2 devices");
        server.join().unwrap();
    }

    // In-memory blocking byte duplex for the fake-pipe test.
    mod fake_pipe {
        use std::collections::VecDeque;
        use std::io::{self, Read, Write};
        use std::sync::{Arc, Condvar, Mutex};

        #[derive(Default)]
        struct Shared {
            buf: VecDeque<u8>,
            closed: bool,
        }

        /// One unidirectional blocking byte channel; cloning shares the buffer.
        #[derive(Clone)]
        pub struct Pipe(Arc<(Mutex<Shared>, Condvar)>);

        impl Pipe {
            pub fn new() -> Self {
                Self(Arc::new((Mutex::new(Shared::default()), Condvar::new())))
            }
        }

        impl Write for Pipe {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                let (m, cv) = &*self.0;
                let mut s = m.lock().unwrap();
                s.buf.extend(data.iter().copied());
                cv.notify_all();
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl Read for Pipe {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let (m, cv) = &*self.0;
                let mut s = m.lock().unwrap();
                loop {
                    if !s.buf.is_empty() {
                        let n = out.len().min(s.buf.len());
                        for slot in out.iter_mut().take(n) {
                            *slot = s.buf.pop_front().unwrap();
                        }
                        return Ok(n);
                    }
                    if s.closed {
                        return Ok(0);
                    }
                    s = cv.wait(s).unwrap();
                }
            }
        }
    }

    // ── Integration test ────────────────────────────────────────────────────

    #[test]
    fn full_context_menu_pipeline() {
        use crate::cache::ShellCache;
        use std::path::{Path, PathBuf};

        let cache = ShellCache::new();

        // File not in cache → menu hidden
        assert!(!should_show_menu(cache.get(Path::new("C:\\doc.txt")), 1));

        // File tracked → menu visible
        cache.set(PathBuf::from("C:\\doc.txt"), FileStatus::Synced);
        assert!(should_show_menu(cache.get(Path::new("C:\\doc.txt")), 1));

        // Multi-select → menu hidden even for tracked files
        assert!(!should_show_menu(cache.get(Path::new("C:\\doc.txt")), 3));

        // Device info formatting
        let devices = FileDevicesInfo {
            path: "C:\\doc.txt".into(),
            device_count: 2,
        };
        assert_eq!(format_device_info(&devices), "Synced to 2 devices");

        // Version info formatting
        let versions = FileVersionsInfo {
            path: "C:\\doc.txt".into(),
            version_count: 3,
            latest_timestamp: Some(1_774_137_600),
        };
        let text = format_version_info(&versions);
        assert!(text.contains("3 saved versions"));
    }

    /// The folder reduced set (USER-decided 2026-07-16): a tracked folder keeps
    /// the submenu with **Share** (a bound folder IS the folder — the natural
    /// share target), while the per-file reads — device info and version
    /// history — hide rather than render a permanent "unavailable".
    #[test]
    fn folder_reduced_set_hides_per_file_leaves_and_keeps_share() {
        assert!(!leaf_hidden_for_folder(SubmenuLeaf::Share));
        assert!(leaf_hidden_for_folder(SubmenuLeaf::Devices));
        assert!(leaf_hidden_for_folder(SubmenuLeaf::Versions));
    }
}
