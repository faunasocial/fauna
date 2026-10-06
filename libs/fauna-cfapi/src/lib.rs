//! Safe Rust wrappers around the Windows Cloud Files API (cfapi / cldapi.dll).
//!
//! Every function here wraps an `unsafe` cfapi call and returns `anyhow::Result`.
//! This module is `#[cfg(windows)]` only — on non-Windows targets the crate
//! compiles to an empty stub so it can stay in the default workspace members.

#![cfg(windows)]

use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HANDLE, NTSTATUS};
use windows::Win32::Storage::CloudFilters::*;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_BASIC_INFO,
};
use windows::core::HSTRING;

/// Stable provider GUID for Fauna sync engine.
/// This must remain constant across versions so Windows recognises existing sync roots.
const PROVIDER_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0xFAD7A001,
    0xFA07,
    0x4E5A,
    [0xB0, 0x0D, 0xFA, 0x07, 0xA0, 0x01, 0x00, 0x01],
);

// ---------------------------------------------------------------------------
// Sync root registration
// ---------------------------------------------------------------------------

/// Register a folder as a Cloud Files sync root.
///
/// Sets hydration policy to FULL (fetch entire file on access) and population policy to
/// FULL (the provider populates directory listings on demand).
///
/// **Registration alone makes the root an on-demand-populatable placeholder directory** —
/// nothing needs to convert it, and trying to (`CfConvertToPlaceholder`) fails, because it
/// already *is* one. Measured (`tests/live_population.rs`): a plain directory goes
/// `DIRECTORY` → `DIRECTORY | RECALL_ON_DATA_ACCESS` on `CfRegisterSyncRoot`, then gains
/// `REPARSE_POINT | OFFLINE` on [`connect`]. `CF_POPULATION_POLICY_FULL` is what asks for
/// that; the opt-*out* is `CF_REGISTER_FLAG_DISABLE_ON_DEMAND_POPULATION_ON_ROOT`, which we
/// deliberately do not pass. (Beware `CF_POPULATION_POLICY_ALWAYS_FULL`: it tells the
/// platform the namespace is always local and FETCH_PLACEHOLDERS is then *never* sent.)
pub fn register_sync_root(path: &str, display_name: &str) -> Result<()> {
    let path_h = HSTRING::from(path);
    let name_h = HSTRING::from(display_name);
    // Must outlive `registration`: `ProviderVersion` borrows this buffer. Inlining it as a
    // temporary (`HSTRING::from("1.0").as_ptr()`) drops the HSTRING at the end of the
    // statement and hands CfRegisterSyncRoot a dangling pointer.
    let version_h = HSTRING::from("1.0");

    let registration = CF_SYNC_REGISTRATION {
        StructSize: std::mem::size_of::<CF_SYNC_REGISTRATION>() as u32,
        ProviderName: windows::core::PCWSTR(name_h.as_ptr()),
        ProviderVersion: windows::core::PCWSTR(version_h.as_ptr()),
        ProviderId: PROVIDER_GUID,
        ..Default::default()
    };

    let policies = CF_SYNC_POLICIES {
        StructSize: std::mem::size_of::<CF_SYNC_POLICIES>() as u32,
        Hydration: CF_HYDRATION_POLICY {
            Primary: CF_HYDRATION_POLICY_FULL,
            Modifier: CF_HYDRATION_POLICY_MODIFIER_NONE,
        },
        Population: CF_POPULATION_POLICY {
            Primary: CF_POPULATION_POLICY_FULL,
            Modifier: CF_POPULATION_POLICY_MODIFIER_NONE,
        },
        InSync: CF_INSYNC_POLICY_TRACK_ALL,
        HardLink: CF_HARDLINK_POLICY(0),
        PlaceholderManagement: CF_PLACEHOLDER_MANAGEMENT_POLICY_DEFAULT,
    };

    unsafe {
        CfRegisterSyncRoot(&path_h, &registration, &policies, CF_REGISTER_FLAG_UPDATE)
            .context("CfRegisterSyncRoot failed")?;
    }

    tracing::info!(path, display_name, "sync root registered");
    Ok(())
}

/// Unregister a Cloud Files sync root.
pub fn unregister_sync_root(path: &str) -> Result<()> {
    let path_h = HSTRING::from(path);
    unsafe {
        CfUnregisterSyncRoot(&path_h).context("CfUnregisterSyncRoot failed")?;
    }
    tracing::info!(path, "sync root unregistered");
    Ok(())
}

/// Whether `path` is under a cloud-files **filter** sync root — the filter-side
/// observable the shell registry ([`shell_registration_for`]) does not show.
/// `CfGetSyncRootInfoByPath` answers `0x80070186 ERROR_NOT_A_CLOUD_FILE` for a
/// folder no sync root covers (measured 2026-09-25); any other failure is an
/// error, never a "no".
pub fn is_filter_registered(path: &str) -> Result<bool> {
    let path_h = HSTRING::from(path);
    let mut info = CF_SYNC_ROOT_BASIC_INFO::default();
    let r = unsafe {
        CfGetSyncRootInfoByPath(
            &path_h,
            CF_SYNC_ROOT_INFO_BASIC,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<CF_SYNC_ROOT_BASIC_INFO>() as u32,
            None,
        )
    };
    match r {
        Ok(()) => Ok(true),
        Err(e) if e.code().0 as u32 == 0x8007_0186 => Ok(false),
        Err(e) => Err(e).context("CfGetSyncRootInfoByPath failed"),
    }
}

// ---------------------------------------------------------------------------
// Shell (WinRT) sync-root registration
// ---------------------------------------------------------------------------

/// Register a sync root with the **shell**, via the WinRT
/// `StorageProviderSyncRootManager` — the registration Explorer's cloud-files UX
/// reads: the *"Free up space"* / *"Always keep on this device"* context-menu
/// verbs, the sync-status column, and the provider grouping are all keyed off
/// the `SyncRootManager` registry state this writes. [`register_sync_root`]
/// (bare `CfRegisterSyncRoot`) registers only with the cloud-files *filter*:
/// placeholders + hydration callbacks work, but the shell knows nothing and
/// renders none of that UX (measured 2026-07-16: `SyncRootManager` stays empty).
///
/// Call this **instead of** [`register_sync_root`], then [`connect`] as usual —
/// the WinRT call performs the filter-level registration itself, and over a
/// folder that is already filter-registered it registers in place (measured
/// 2026-09-25, connected or not; a 2026-07-16 measurement saw
/// `0x8007018B ERROR_CLOUD_FILE_ACCESS_DENIED` there, which no longer
/// reproduces — [`is_filter_registered`] is the way to observe the filter side,
/// never this call's outcome). The filter policies
/// it writes mirror [`register_sync_root`]'s exactly (hydration FULL,
/// population FULL, in-sync TRACK_ALL) so the two paths differ only in shell
/// visibility.
///
/// `account_id` distinguishes roots within the provider (the OS-mandated Id
/// format is `provider!SID!account`); returns the composed Id, which
/// [`unregister_sync_root_with_shell`] takes.
pub fn register_sync_root_with_shell(
    path: &str,
    display_name: &str,
    account_id: &str,
) -> Result<String> {
    use windows::Storage::Provider::{
        StorageProviderHydrationPolicy, StorageProviderHydrationPolicyModifier,
        StorageProviderInSyncPolicy, StorageProviderPopulationPolicy, StorageProviderSyncRootInfo,
        StorageProviderSyncRootManager,
    };
    use windows::Storage::StorageFolder;

    let id = format!("Fauna!{}!{}", current_user_sid_string()?, account_id);

    let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(path))
        .context("GetFolderFromPathAsync")?
        .get()
        .context("resolve StorageFolder")?;

    let info = StorageProviderSyncRootInfo::new().context("StorageProviderSyncRootInfo::new")?;
    info.SetId(&HSTRING::from(id.as_str()))?;
    info.SetPath(&folder)?;
    info.SetDisplayNameResource(&HSTRING::from(display_name))?;
    // An icon is mandatory; until brand art ships, a neutral system folder icon.
    info.SetIconResource(&HSTRING::from("%SystemRoot%\\system32\\imageres.dll,-1043"))?;
    info.SetVersion(&HSTRING::from("1.0"))?;
    info.SetProviderId(PROVIDER_GUID)?;
    info.SetHydrationPolicy(StorageProviderHydrationPolicy::Full)?;
    info.SetHydrationPolicyModifier(StorageProviderHydrationPolicyModifier::None)?;
    info.SetPopulationPolicy(StorageProviderPopulationPolicy::Full)?;
    // Mirror the filter path's CF_INSYNC_POLICY_TRACK_ALL — the WinRT enum is the
    // same flags space as CF_INSYNC_POLICY.
    info.SetInSyncPolicy(StorageProviderInSyncPolicy(CF_INSYNC_POLICY_TRACK_ALL.0))?;
    // Pinning is what the "Always keep on this device" / "Free up space" verbs do.
    info.SetAllowPinning(true)?;
    info.SetShowSiblingsAsGroup(false)?;

    StorageProviderSyncRootManager::Register(&info)
        .context("StorageProviderSyncRootManager::Register")?;
    tracing::info!(path, display_name, id, "sync root registered with shell");
    Ok(id)
}

/// Remove a [`register_sync_root_with_shell`] registration (the shell side;
/// the filter side goes down with [`unregister_sync_root`] as usual). Without
/// this a deleted root leaves a ghost `SyncRootManager` entry the shell keeps
/// rendering.
///
/// Order matters (measured 2026-07-16): this fails `0x8007017C
/// ERROR_CLOUD_FILE_INVALID_REQUEST` while the root is still connected, and
/// succeeds once [`disconnect`] + [`unregister_sync_root`] have run — the
/// reverse of registration, where the shell call goes first.
pub fn unregister_sync_root_with_shell(id: &str) -> Result<()> {
    use windows::Storage::Provider::StorageProviderSyncRootManager;
    StorageProviderSyncRootManager::Unregister(&HSTRING::from(id))
        .context("StorageProviderSyncRootManager::Unregister")?;
    tracing::info!(id, "sync root unregistered from shell");
    Ok(())
}

/// One entry of the `SyncRootManager` state that belongs to the Fauna provider
/// (the Id starts with `Fauna!`): what [`list_shell_sync_roots`] returns.
#[derive(Debug, Clone)]
pub struct ShellSyncRoot {
    /// The OS-mandated `Fauna!SID!account` Id ([`unregister_sync_root_with_shell`] takes it).
    pub id: String,
    /// The registered folder as recorded at registration time. The recorded
    /// string persists after the folder is deleted (the registry is static
    /// data), so a caller deciding "ghost or live" must check the path on disk;
    /// `None` means the entry carries no readable path at all.
    pub path: Option<String>,
}

/// The registry home of shell sync-root registrations — **the only readable
/// query surface for an unpackaged Win32 process** (measured 2026-07-16, probe
/// `tests/shell_enumeration_probe.rs`): with a registration demonstrably
/// present here, WinRT `GetCurrentSyncRoots` returns an EMPTY list and
/// `GetSyncRootInformationForFolder` fails `0x80070490 UNABLE_TO_MASK_PATH` —
/// while `Register`/`Unregister` (the write half) work fine. So writes go
/// through WinRT and reads through this key; do not "simplify" back to the
/// WinRT getters without re-running the probe.
const SYNC_ROOT_MANAGER_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager";

/// Enumerate the **Fauna** shell sync-root registrations (Ids with the `Fauna!`
/// prefix) from the `SyncRootManager` registry state — see
/// [`SYNC_ROOT_MANAGER_KEY`] for why this reads the registry and not WinRT.
/// Each entry's folder comes from its `UserSyncRoots\<SID>` value (the SID is
/// the Id's middle segment), which `Register` always writes.
pub fn list_shell_sync_roots() -> Result<Vec<ShellSyncRoot>> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows::core::{PCWSTR, PWSTR};

    fn to_w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let mut hkey = HKEY::default();
        let sub = to_w(SYNC_ROOT_MANAGER_KEY);
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            // No SyncRootManager key = no shell registrations on this machine
            // at all (fresh install) — an empty listing, not an error.
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(
                hkey,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
            .is_err()
            {
                break; // ERROR_NO_MORE_ITEMS (or a real failure — stop either way)
            }
            index += 1;
            let id = String::from_utf16_lossy(&name[..len as usize]);
            if !id.starts_with("Fauna!") {
                continue;
            }
            // Id = Fauna!<SID>!<account>; the folder lives at <id>\UserSyncRoots\<SID>.
            let sid = id.split('!').nth(1).unwrap_or_default();
            let value_key = to_w(&format!(r"{SYNC_ROOT_MANAGER_KEY}\{id}\UserSyncRoots"));
            let sid_w = to_w(sid);
            let mut buf = [0u16; 1024];
            let mut cb = std::mem::size_of_val(&buf) as u32;
            let path = if RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(value_key.as_ptr()),
                PCWSTR(sid_w.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut cb),
            )
            .is_ok()
            {
                // cb counts bytes including the terminating NUL.
                let chars = (cb as usize / 2).saturating_sub(1);
                Some(String::from_utf16_lossy(&buf[..chars]))
            } else {
                None
            };
            out.push(ShellSyncRoot { id, path });
        }
        let _ = RegCloseKey(hkey);
        Ok(out)
    }
}

/// The Fauna shell registration covering `path`, if any — the query the product
/// host's startup reconcile keys off (shell-registered already → just re-connect;
/// absent → shell-register, over any leftover filter registration in place).
/// Path comparison is case-insensitive with trailing separators ignored (Windows
/// path semantics; the OS may return either casing).
pub fn shell_registration_for(path: &str) -> Result<Option<String>> {
    fn norm(p: &str) -> String {
        p.trim_end_matches(['\\', '/']).to_lowercase()
    }
    let wanted = norm(path);
    Ok(list_shell_sync_roots()?
        .into_iter()
        .find(|r| r.path.as_deref().map(norm) == Some(wanted.clone()))
        .map(|r| r.id))
}

/// The current user's SID as a string — the middle segment of the shell
/// registration's OS-mandated `provider!SID!account` Id format. Shared with
/// `fauna-ipc` (`current_user_pipe_name`, `PipeSecurity::for_owner`) via
/// `fauna_ipc::win_token` — this used to hand-roll its own copy of the same
/// win32 dance, with an unsound `Vec<u8>` reinterpret-cast and a leaked token
/// handle `fauna-ipc`'s copy had already fixed.
fn current_user_sid_string() -> Result<String> {
    fauna_ipc::win_token::current_user_sid_string()
}

// ---------------------------------------------------------------------------
// Connect / disconnect
// ---------------------------------------------------------------------------

/// Connect to a sync root to receive fetch callbacks.
///
/// Returns a connection key that must be passed to `disconnect` when done.
/// `fetch_cb` is called when Windows needs a file's data (on-demand hydration);
/// `cancel_cb` when a fetch is cancelled by the user or system;
/// `fetch_placeholders_cb` when Windows browses a not-fully-populated directory
/// and needs its child entries (lazy directory listing — fired under the
/// `CF_POPULATION_POLICY_FULL` policy `register_sync_root` sets, which requests
/// *all* entries of the accessed directory; see
/// `docs/goal/behavior/file-sync.md` § On-Demand Files).
///
/// `dehydrate_completion_cb` fires **after** the OS has freed a placeholder's
/// local bytes — Explorer's native "Free up space" or Storage Sense. We register
/// the **`_COMPLETION`** variant, which is a *pure post-hoc observation*: it needs
/// no acknowledgment and cannot block or veto the dehydration, so it can only make
/// the sync-status row honest (`Synced` → `Placeholder`), never break a user
/// freeing space (`file-sync.md` § Per-file sync-status display → *Windows OS
/// shell-overlay carve-out*). It does **not** fire for the provider's own I/O (the
/// Fauna shell menu's `FreeSpace` verb records the row itself). See that goal-doc
/// section for the measured cfapi behavior and the live-box validation follow-on.
pub fn connect(
    path: &str,
    fetch_cb: CF_CALLBACK,
    cancel_cb: CF_CALLBACK,
    fetch_placeholders_cb: CF_CALLBACK,
    dehydrate_completion_cb: CF_CALLBACK,
) -> Result<CF_CONNECTION_KEY> {
    let path_h = HSTRING::from(path);

    let callbacks = [
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_FETCH_DATA,
            Callback: fetch_cb,
        },
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_CANCEL_FETCH_DATA,
            Callback: cancel_cb,
        },
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS,
            Callback: fetch_placeholders_cb,
        },
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_NOTIFY_DEHYDRATE_COMPLETION,
            Callback: dehydrate_completion_cb,
        },
        // Sentinel: CF_CALLBACK_TYPE_NONE terminates the array.
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_NONE,
            Callback: None,
        },
    ];

    let key = unsafe {
        CfConnectSyncRoot(
            &path_h,
            callbacks.as_ptr(),
            None, // callback context (we use global state)
            CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH | CF_CONNECT_FLAG_REQUIRE_PROCESS_INFO,
        )
        .context("CfConnectSyncRoot failed")?
    };

    tracing::info!(path, key = key.0, "sync root connected");
    Ok(key)
}

/// Disconnect from a sync root. Infallible — logs errors but does not fail.
pub fn disconnect(key: CF_CONNECTION_KEY) {
    if let Err(e) = unsafe { CfDisconnectSyncRoot(key) } {
        tracing::warn!(key = key.0, error = %e, "CfDisconnectSyncRoot failed");
    } else {
        tracing::info!(key = key.0, "sync root disconnected");
    }
}

// ---------------------------------------------------------------------------
// Data transfer (CfExecute)
// ---------------------------------------------------------------------------

/// Transfer file data to Windows during a FETCH_DATA callback.
///
/// `connection_key` and `transfer_key` come from the callback info.
/// `data` is the file content, `offset` is the byte offset within the file.
pub fn transfer_data(
    connection_key: &CF_CONNECTION_KEY,
    transfer_key: i64,
    request_key: i64,
    data: &[u8],
    offset: i64,
) -> Result<()> {
    let op_info = CF_OPERATION_INFO {
        StructSize: std::mem::size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_DATA,
        ConnectionKey: *connection_key,
        TransferKey: transfer_key,
        // The callback's RequestKey: "an opaque id that uniquely identifies a cloud file
        // operation on a particular cloud file". Threading it through is the honest thing
        // to do. It is NOT required, contrary to an earlier comment here: measured live,
        // CfExecute succeeds with RequestKey = 0 (Nextcloud ships it zeroed in production).
        RequestKey: request_key,
        ..Default::default()
    };

    let mut op_params = CF_OPERATION_PARAMETERS {
        ParamSize: cf_size_of_op_param::<CF_OPERATION_PARAMETERS_0_0>(),
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferData: CF_OPERATION_PARAMETERS_0_0 {
                Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                CompletionStatus: NTSTATUS(0), // STATUS_SUCCESS
                Buffer: data.as_ptr() as *const core::ffi::c_void,
                Offset: offset,
                Length: data.len() as i64,
            },
        },
    };

    unsafe {
        CfExecute(&op_info, &mut op_params).context("CfExecute(TransferData) failed")?;
    }
    Ok(())
}

/// Report a failed data transfer to Windows.
///
/// Called when the sync engine cannot provide the requested file data.
pub fn transfer_failed(
    connection_key: &CF_CONNECTION_KEY,
    transfer_key: i64,
    request_key: i64,
    status: NTSTATUS,
) -> Result<()> {
    let op_info = CF_OPERATION_INFO {
        StructSize: std::mem::size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_DATA,
        ConnectionKey: *connection_key,
        TransferKey: transfer_key,
        // The callback's RequestKey: "an opaque id that uniquely identifies a cloud file
        // operation on a particular cloud file". Threading it through is the honest thing
        // to do. It is NOT required, contrary to an earlier comment here: measured live,
        // CfExecute succeeds with RequestKey = 0 (Nextcloud ships it zeroed in production).
        RequestKey: request_key,
        ..Default::default()
    };

    let mut op_params = CF_OPERATION_PARAMETERS {
        ParamSize: cf_size_of_op_param::<CF_OPERATION_PARAMETERS_0_0>(),
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferData: CF_OPERATION_PARAMETERS_0_0 {
                Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                CompletionStatus: status,
                Buffer: std::ptr::null(),
                Offset: 0,
                Length: 0,
            },
        },
    };

    unsafe {
        CfExecute(&op_info, &mut op_params).context("CfExecute(TransferFailed) failed")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Directory population (CfExecute TRANSFER_PLACEHOLDERS)
// ---------------------------------------------------------------------------

/// One entry to materialize into a directory during a FETCH_PLACEHOLDERS
/// response: a tracked file (`is_dir = false`, real `size`) or a synthesized
/// subdirectory (`is_dir = true`, `size` ignored). `mtime` is Unix seconds.
///
/// `rel_path` is the entry's **folder-relative** path (forward slashes, e.g.
/// `docs/report.pdf`) — deliberately *not* just the leaf name, because it serves two
/// purposes at once:
///
/// * its **final component** is the `RelativeFileName` cfapi wants (a placeholder's
///   name relative to the directory being populated), and
/// * the **whole path** is the placeholder's **`FileIdentity`** — the provider's opaque
///   unique id for the entry, which cfapi stores and hands back in
///   `CF_CALLBACK_INFO.FileIdentity` on later callbacks.
///
/// Carrying one field instead of a separate `name` + `identity` is what makes an
/// **empty identity unrepresentable**: a non-empty `rel_path` always yields a non-empty
/// identity, and cfapi *rejects the entire operation* when any identity is empty (see
/// [`transfer_placeholders`]). The rel path is also exactly the key `SyncDb` stores
/// entries under, so the identity agrees with the engine's own naming by construction.
#[derive(Debug, Clone)]
pub struct PlaceholderInfo {
    pub rel_path: String,
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
}

impl PlaceholderInfo {
    /// The final path component — what cfapi wants as `RelativeFileName` (the entry's
    /// name relative to the directory being populated). `docs/report.pdf` → `report.pdf`.
    fn leaf(&self) -> &str {
        self.rel_path.rsplit('/').next().unwrap_or(&self.rel_path)
    }

    /// The placeholder's `FileIdentity` bytes. Never empty for a valid entry — that is
    /// the invariant [`transfer_placeholders`] depends on.
    fn identity(&self) -> &[u8] {
        self.rel_path.as_bytes()
    }
}

/// cfapi requires a **NULL** `PlaceholderArray` when the count is zero. `Vec::as_mut_ptr`
/// on an empty `Vec` yields a dangling-but-non-null pointer, which `CfExecute` rejects
/// *even though* `PlaceholderCount` is 0 — so the empty case must be spelled out. (The
/// reference Rust provider, cloud-filter-rs, carries the same null-on-empty branch with the
/// same comment.) Kept as a named fn so the invariant is unit-pinned: a brand-new folder
/// is empty, making this the very first call a new user hits.
fn placeholder_array_ptr(
    infos: &mut [CF_PLACEHOLDER_CREATE_INFO],
) -> *mut CF_PLACEHOLDER_CREATE_INFO {
    if infos.is_empty() {
        std::ptr::null_mut()
    } else {
        infos.as_mut_ptr()
    }
}

/// `CF_OPERATION_PARAMETERS::ParamSize`, computed the way cfapi actually demands it —
/// the C header's `CF_SIZE_OF_OP_PARAM(field)` macro:
///
/// ```c
/// #define CF_SIZE_OF_OP_PARAM(field)                  \
///     (FIELD_OFFSET(CF_OPERATION_PARAMETERS, field) + \
///      sizeof(((CF_OPERATION_PARAMETERS *)0)->field))
/// ```
///
/// i.e. the offset of *the union member being used* plus **that member's** size — **not**
/// `size_of::<CF_OPERATION_PARAMETERS>()`, which is the size of the *largest* member
/// (48 bytes here, vs the 40 every variant we use actually needs). This is the rule the
/// docs state ("ParamSize must be set to the exact size of OpParams.TransferData plus the
/// offset of OpParams.TransferData") and the one both reference providers follow — Nextcloud
/// via the C macro, cloud-filter-rs via `offset_of!` — so we follow it too.
///
/// ⚠ **It is NOT, however, what `CfExecute` was rejecting**, despite an earlier session's
/// comment here claiming exactly that. Measured live (`tests/live_population.rs`): with a
/// valid `FileIdentity`, the call succeeds with the *whole-union* size too. cfapi tolerates
/// an oversized `ParamSize`. The real defect was the placeholder identity — see
/// [`transfer_placeholders`]. Keep this correct; just don't believe it is load-bearing.
const fn cf_size_of_op_param<T>() -> u32 {
    (std::mem::offset_of!(CF_OPERATION_PARAMETERS, Anonymous) + std::mem::size_of::<T>()) as u32
}

/// Complete a FETCH_PLACEHOLDERS callback: materialize `entries` as the children of the
/// directory being populated.
///
/// # The `FileIdentity` invariant — why on-demand sync never worked on Windows
///
/// **Every placeholder MUST carry a non-empty `FileIdentity`.** If any entry's identity is
/// empty, `CfExecute` rejects the *entire operation* with `ERROR_CLOUD_FILE_INVALID_REQUEST`
/// (0x8007017C) — it does not skip the bad entry, and it does not tell you which field was
/// wrong. Microsoft documents the rule in one clause on `CF_PLACEHOLDER_CREATE_INFO`:
/// *"FileIdentity … This is required for files (not for directories)."*
///
/// This crate shipped `FileIdentity: NULL` on every placeholder, so **no directory could
/// ever be populated** — which is the whole reason Windows on-demand sync has never worked.
/// A live bisect (`tests/live_population.rs`) pinned it to this one field: with a non-empty
/// identity the call succeeds; with a NULL pointer *or* a non-null pointer of length 0 it
/// fails 0x8007017C. Empty length is the trap — it is the pointer's *length* cfapi checks.
///
/// [`PlaceholderInfo`] makes an empty identity unrepresentable by deriving it from the
/// entry's `rel_path`, so this cannot regress by a caller forgetting a field.
///
/// ## Fields that are NOT validated (measured, not assumed — do not "fix" these again)
///
/// A previous session attributed this failure to three other fields and "fixed" all three.
/// The live bisect proved each one innocent: with the identity present, the call succeeds
/// even with `RequestKey = 0`, with `FileAttributes = 0`, with the whole-union `ParamSize`,
/// with `Flags = NONE`, and with `PlaceholderTotalCount = 0`. Those three changes were kept
/// (each independently matches the documented contract — Nextcloud and cloud-filter-rs both
/// compute `ParamSize` via `CF_SIZE_OF_OP_PARAM`), but **none of them was the bug**.
///
/// The operation may be completed from any thread, not just the callback's — *"All
/// operations can be performed in an arbitrary thread context in the sync provider
/// process."* — subject to cfapi's fixed 60 s per-request timeout.
///
/// Directory entries are **not** flagged `DISABLE_ON_DEMAND_POPULATION`, so browsing into
/// one later fires its own FETCH_PLACEHOLDERS (lazy recursion). The operation itself carries
/// that flag, marking *this* directory fully populated so the callback is not re-fired on
/// every access. An empty `entries` slice still completes the operation, marking the
/// directory populated-but-empty. Returns what the platform did with the batch — see
/// [`PlaceholderTransfer`].
pub fn transfer_placeholders(
    connection_key: &CF_CONNECTION_KEY,
    transfer_key: i64,
    request_key: i64,
    entries: &[PlaceholderInfo],
) -> Result<PlaceholderTransfer> {
    // Fail loudly on our own invariant rather than letting cfapi reject the whole batch
    // with an opaque 0x8007017C that names no field.
    if let Some(bad) = entries.iter().find(|e| e.identity().is_empty()) {
        anyhow::bail!(
            "placeholder has an empty FileIdentity (rel_path={:?}); cfapi would reject the \
             entire TRANSFER_PLACEHOLDERS operation with ERROR_CLOUD_FILE_INVALID_REQUEST",
            bad.rel_path
        );
    }

    // Keep the wide-string backing buffers alive across the CfExecute call:
    // each `CF_PLACEHOLDER_CREATE_INFO.RelativeFileName` is a borrowed pointer.
    // cfapi wants the *leaf* here: "It should consist only of the file or directory name."
    let names: Vec<HSTRING> = entries.iter().map(|e| HSTRING::from(e.leaf())).collect();

    let mut infos: Vec<CF_PLACEHOLDER_CREATE_INFO> = entries
        .iter()
        .zip(&names)
        .map(|(e, name_h)| {
            let ft = unix_to_filetime(e.mtime);
            let (attrs, size) = if e.is_dir {
                (FILE_ATTRIBUTE_DIRECTORY.0, 0i64)
            } else {
                (FILE_ATTRIBUTE_NORMAL.0, e.size as i64)
            };
            let identity = e.identity();
            CF_PLACEHOLDER_CREATE_INFO {
                RelativeFileName: windows::core::PCWSTR(name_h.as_ptr()),
                FsMetadata: CF_FS_METADATA {
                    BasicInfo: FILE_BASIC_INFO {
                        CreationTime: ft,
                        LastAccessTime: ft,
                        LastWriteTime: ft,
                        ChangeTime: ft,
                        FileAttributes: attrs,
                    },
                    FileSize: size,
                },
                // THE load-bearing field. See this function's doc comment: a placeholder
                // with an empty identity makes cfapi reject the WHOLE operation.
                FileIdentity: identity.as_ptr() as *const core::ffi::c_void,
                FileIdentityLength: identity.len() as u32,
                Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
                ..Default::default()
            }
        })
        .collect();
    let placeholder_array = placeholder_array_ptr(&mut infos);

    let op_info = CF_OPERATION_INFO {
        StructSize: std::mem::size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS,
        ConnectionKey: *connection_key,
        TransferKey: transfer_key,
        // The callback's RequestKey: "an opaque id that uniquely identifies a cloud file
        // operation on a particular cloud file". Threading it through is the honest thing
        // to do. It is NOT required, contrary to an earlier comment here: measured live,
        // CfExecute succeeds with RequestKey = 0 (Nextcloud ships it zeroed in production).
        RequestKey: request_key,
        ..Default::default()
    };

    let mut op_params = CF_OPERATION_PARAMETERS {
        ParamSize: cf_size_of_op_param::<CF_OPERATION_PARAMETERS_0_4>(),
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferPlaceholders: CF_OPERATION_PARAMETERS_0_4 {
                Flags: CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_DISABLE_ON_DEMAND_POPULATION,
                CompletionStatus: NTSTATUS(0), // STATUS_SUCCESS
                PlaceholderTotalCount: infos.len() as i64,
                PlaceholderArray: placeholder_array,
                PlaceholderCount: infos.len() as u32,
                EntriesProcessed: 0,
            },
        },
    };

    unsafe {
        CfExecute(&op_info, &mut op_params).context("CfExecute(TransferPlaceholders) failed")?;
    }
    // SAFETY: CfExecute wrote EntriesProcessed back into the same union variant.
    let processed = unsafe { op_params.Anonymous.TransferPlaceholders.EntriesProcessed };
    // cfapi writes each entry's own outcome back into its `Result` field: an entry is
    // on the disk only when the operation reached it AND its own result is success.
    let created = entries
        .iter()
        .zip(&infos)
        .take(processed as usize)
        .filter(|(_, info)| info.Result.is_ok())
        .map(|(e, _)| e.rel_path.clone())
        .collect();
    tracing::debug!(
        requested = entries.len(),
        processed,
        "directory placeholders transferred"
    );
    Ok(PlaceholderTransfer { processed, created })
}

/// What one [`transfer_placeholders`] call did — returned only when `CfExecute`
/// itself succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderTransfer {
    /// `EntriesProcessed` as the platform reported it.
    pub processed: u32,
    /// The `rel_path` of every entry the platform processed with a success result —
    /// the placeholders this call provably put on the disk. The engine's evidence
    /// that a placeholder was on this disk rests on exactly this set
    /// (`delete-propagation.md` § *An offline placeholder delete propagates*,
    /// decision (a)): never an entry that failed, never one the batch did not reach.
    pub created: Vec<String>,
}

// ---------------------------------------------------------------------------
// Placeholder management
// ---------------------------------------------------------------------------

/// Eagerly create a placeholder for `entry` inside the `parent` directory (as opposed to
/// [`transfer_placeholders`], which answers an on-demand FETCH_PLACEHOLDERS callback).
///
/// The file appears in Explorer with its full size but occupies no disk space until
/// hydrated. `entry.rel_path` supplies both the name and the **mandatory** `FileIdentity` —
/// see [`transfer_placeholders`] for why an empty identity is fatal here too:
/// `CfCreatePlaceholders` takes the very same `CF_PLACEHOLDER_CREATE_INFO`.
pub fn create_placeholder(parent: &Path, entry: &PlaceholderInfo) -> Result<()> {
    let parent_h = HSTRING::from(parent.as_os_str());
    let name_h = HSTRING::from(entry.leaf());
    let identity = entry.identity();
    if identity.is_empty() {
        anyhow::bail!(
            "placeholder has an empty FileIdentity (rel_path={:?}); cfapi requires one",
            entry.rel_path
        );
    }

    let ft = unix_to_filetime(entry.mtime);
    let (attrs, size) = if entry.is_dir {
        (FILE_ATTRIBUTE_DIRECTORY.0, 0i64)
    } else {
        (FILE_ATTRIBUTE_NORMAL.0, entry.size as i64)
    };

    let mut placeholder = CF_PLACEHOLDER_CREATE_INFO {
        RelativeFileName: windows::core::PCWSTR(name_h.as_ptr()),
        FsMetadata: CF_FS_METADATA {
            BasicInfo: FILE_BASIC_INFO {
                CreationTime: ft,
                LastAccessTime: ft,
                LastWriteTime: ft,
                ChangeTime: ft,
                FileAttributes: attrs,
            },
            FileSize: size,
        },
        FileIdentity: identity.as_ptr() as *const core::ffi::c_void,
        FileIdentityLength: identity.len() as u32,
        Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
        ..Default::default()
    };

    unsafe {
        CfCreatePlaceholders(
            &parent_h,
            std::slice::from_mut(&mut placeholder),
            CF_CREATE_FLAG_NONE,
            None,
        )
        .context("CfCreatePlaceholders failed")?;
    }

    tracing::debug!(parent = %parent.display(), rel_path = entry.rel_path, "placeholder created");
    Ok(())
}

/// Convert an existing file to a Cloud Files placeholder.
///
/// The file must already exist on disk. After conversion it can be dehydrated.
pub fn convert_to_placeholder(path: &Path) -> Result<()> {
    let handle = open_file_handle(path)?;
    let result = unsafe {
        CfConvertToPlaceholder(
            handle,
            None, // no file identity
            0,    // identity length
            CF_CONVERT_FLAG_MARK_IN_SYNC,
            None, // no USN output
            None, // no overlapped
        )
    };
    unsafe { close_handle(handle) };
    result.context("CfConvertToPlaceholder failed")?;
    tracing::debug!(path = %path.display(), "converted to placeholder");
    Ok(())
}

/// Re-anchor an ordinary **directory** under a sync root as a cloud placeholder marked
/// in-sync — `CfConvertToPlaceholder(MARK_IN_SYNC)` with `rel_identity` as its
/// `FileIdentity`; Explorer's Status column flips the folder to ✅. Opens with
/// `FILE_FLAG_BACKUP_SEMANTICS`, which every directory handle requires.
///
/// **Directories only, enforced.** The convert takes no USN condition, so on a FILE it
/// would vouch for whatever bytes were on disk at that instant — a save newer than the
/// proven content included — and licence the next dehydrate to free them. A file takes [`convert_to_placeholder_anchored`] (not in-sync) and then
/// the USN-conditioned [`set_in_sync`]. A directory has no bytes to lose; its caller
/// contract is only that its whole known subtree is clean (the engine's
/// `subtree_fully_synced`).
///
/// Same plain-Win32 handle as [`convert_to_placeholder_anchored`] (the oplock-protected
/// handle is refused `0x80070006` here — measured, `diag_pin_reaction_mechanics`) and
/// the same mandatory non-empty identity (the `FileIdentity` bug class).
pub fn convert_to_placeholder_in_sync(path: &Path, rel_identity: &str) -> Result<()> {
    anyhow::ensure!(
        path.is_dir(),
        "convert_to_placeholder_in_sync is directories-only: {} is not a directory (a file \
         is anchored, then asserted in-sync conditioned on its USN)",
        path.display()
    );
    anyhow::ensure!(
        !rel_identity.is_empty(),
        "placeholder identity must be non-empty (cfapi rejects it and the fetch path can't route)"
    );
    let identity = rel_identity.as_bytes();
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    /// Required to open a DIRECTORY handle at all; harmless on a regular file.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| format!("open {} for conversion", path.display()))?;
    let handle = HANDLE(file.as_raw_handle());
    let result = unsafe {
        CfConvertToPlaceholder(
            handle,
            Some(identity.as_ptr() as *const core::ffi::c_void),
            identity.len() as u32,
            CF_CONVERT_FLAG_MARK_IN_SYNC,
            None, // no USN output
            None, // no overlapped
        )
    };
    drop(file);
    result.context("CfConvertToPlaceholder(MARK_IN_SYNC) failed")?;
    tracing::debug!(path = %path.display(), rel_identity, "re-anchored as in-sync hydrated placeholder");
    Ok(())
}

/// Dehydrate a placeholder file (remove local data, keep cloud appearance).
pub fn dehydrate_placeholder(path: &Path) -> Result<()> {
    let handle = open_file_handle(path)?;
    let result = unsafe {
        CfDehydratePlaceholder(
            handle,
            0,  // starting offset
            -1, // entire file (length = -1 means "to end")
            CF_DEHYDRATE_FLAG_NONE,
            None, // no overlapped
        )
    };
    unsafe { close_handle(handle) };
    result.context("CfDehydratePlaceholder failed")?;
    tracing::debug!(path = %path.display(), "placeholder dehydrated");
    Ok(())
}

/// Set or clear the pinned state on a file.
///
/// Pinned files are kept hydrated (always available offline).
pub fn set_pin_state(path: &Path, pinned: bool) -> Result<()> {
    let handle = open_file_handle(path)?;
    let state = if pinned {
        CF_PIN_STATE_PINNED
    } else {
        CF_PIN_STATE_UNPINNED
    };
    let result = unsafe {
        CfSetPinState(
            handle,
            state,
            CF_SET_PIN_FLAG_NONE,
            None, // no overlapped
        )
    };
    unsafe { close_handle(handle) };
    result.context("CfSetPinState failed")?;
    tracing::debug!(path = %path.display(), pinned, "pin state set");
    Ok(())
}

/// Synchronously hydrate a placeholder — ask cldflt to materialize `path`'s full
/// byte range, blocking until the data is present (or the request fails).
///
/// **Who serves the request is the whole question.** The data comes from the file's
/// *provider* via a `FETCH_DATA` callback — and when the caller **is** the provider,
/// whether that callback fires for the provider's own `CfHydratePlaceholder` (or is
/// swallowed by the own-process suppression that eats the provider's ordinary reads)
/// is an OS-behavior question, measured by `fauna-sync-agent`'s
/// `diag_pin_reaction_mechanics` probe rather than assumed here. If it *does* fire,
/// it arrives on cfapi's threadpool and is served by the hydration loop — so never
/// call this **on** the loop thread itself (the loop can't serve while blocked).
pub fn hydrate_placeholder(path: &Path) -> Result<()> {
    let handle = open_file_handle(path)?;
    let result = unsafe {
        CfHydratePlaceholder(
            handle,
            0,  // starting offset
            -1, // entire file (length = -1 means "to end")
            CF_HYDRATE_FLAG_NONE,
            None, // no overlapped
        )
    };
    unsafe { close_handle(handle) };
    result.context("CfHydratePlaceholder failed")?;
    tracing::debug!(path = %path.display(), "placeholder hydrated");
    Ok(())
}

/// **Provider-initiated** data push: materialize `data` into the placeholder at
/// `path` without any `FETCH_DATA` round-trip.
///
/// This is the same `CfExecute(TRANSFER_DATA)` the fetch path answers callbacks
/// with — the only difference is where the transfer key comes from: a callback
/// hands one in, while a provider acting on its own initiative asks for one via
/// `CfGetTransferKey`. No callback is involved anywhere, so the own-process
/// suppression question that hangs over [`hydrate_placeholder`] does not arise,
/// and there is no loop round-trip to deadlock on.
///
/// `connection_key` is the provider's own key from [`connect`] — pushing data is
/// a provider privilege, tied to the registration.
///
/// The push starts at offset 0 and covers all of `data`; cfapi wants 4096-aligned
/// offsets except at EOF, so a full-file push is always aligned. Whether a full
/// push also flips the file's OS attributes to "hydrated" (clearing
/// `OFFLINE`/`RECALL_ON_DATA_ACCESS`) is measured by `diag_pin_reaction_mechanics`.
pub fn provider_push_data(
    connection_key: &CF_CONNECTION_KEY,
    path: &Path,
    data: &[u8],
) -> Result<()> {
    let handle = open_file_handle(path)?;
    let transfer_key = unsafe { CfGetTransferKey(handle) };
    let result = transfer_key.and_then(|transfer_key| {
        let res = transfer_data(connection_key, transfer_key, 0, data, 0);
        unsafe { CfReleaseTransferKey(handle, &transfer_key) };
        res.map_err(|e| windows::core::Error::new(windows::core::HRESULT(0), format!("{e:#}")))
    });
    unsafe { close_handle(handle) };
    result.context("provider-initiated TRANSFER_DATA failed")?;
    tracing::debug!(path = %path.display(), len = data.len(), "provider pushed placeholder data");
    Ok(())
}

/// Mark a placeholder **in sync** — the provider's assertion that the file's local
/// content matches what the provider has stored.
///
/// Under this crate's registration policy (`CF_INSYNC_POLICY_TRACK_ALL`) the OS flips
/// a placeholder *not*-in-sync on local changes, and `CfDehydratePlaceholder` refuses
/// a not-in-sync file — the refusal that protects a user's unsynced edit from
/// destruction. Only the provider can flip it back, and only the provider knows when
/// that is true (its own engine says the content is uploaded). Nothing in the service
/// called this before the pin-reaction loop landed, which meant the refusal, once
/// tripped, held **forever** — even after the edit had long been uploaded.
///
/// **Conditioned on `usn`, and there is no unconditioned form.** The
/// caller reads `usn` with [`file_usn`] BEFORE it proves the content is the provider's
/// stored copy (the engine's `is_dehydration_safe` hash); this call then opens the file
/// with a share mode that shuts out every other writer, deleter and renamer, re-reads the
/// USN through that handle, and asserts in-sync on the same handle only if the USN has not
/// moved. A write that landed any time after `usn` — during the proof included — refuses
/// the assertion, and none can land between the re-read and the assertion, because no
/// writer can hold the file while the handle is open. So whatever bytes the proof hashed
/// are the bytes this vouches for, or nothing is vouched for. An unconditioned assertion
/// vouched for whatever was on disk at the moment of the call — a newer save included —
/// and the next dehydrate freed it. Writers are shut out for two syscalls, never for the
/// proof; a file another process holds open for writing refuses the assertion (it is not
/// settled content), and the next recorded upload retries.
///
/// Why not cfapi's own USN condition (`CfSetInSyncState`'s `InSyncUsn`): measured on
/// Windows 11 ARM64 (2026-09-28), a conditioned call is refused `0x80070179` ("not in sync
/// with the cloud") for EVERY value, including the exact USN read through the handle it is
/// called on — it cannot express "not written since". The platform condition is therefore
/// never sent; the write-excluding handle does its job.
///
/// A `usn` of 0 is refused: 0 is what a volume without a USN journal reports for every
/// file, where no later write could be told apart. Such a volume keeps its files honestly
/// pending instead.
pub fn set_in_sync(path: &Path, usn: i64) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    /// `FILE_SHARE_READ` alone: no other handle may write, delete or rename while ours is open.
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    /// Required to open a directory handle at all; harmless on a regular file.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    anyhow::ensure!(
        usn != 0,
        "refusing an unconditioned in-sync assertion on {} (USN 0: the volume keeps no USN \
         journal, so a newer write could not be told apart)",
        path.display()
    );
    // Write access because `CfSetInSyncState` requires WRITE_DATA on its handle; a plain
    // Win32 handle, because the oplock-protected one cannot carry this share mode.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| {
            format!(
                "open {} excluding writers (held open for writing elsewhere?)",
                path.display()
            )
        })?;
    let handle = HANDLE(file.as_raw_handle());
    let now =
        usn_of_handle(handle).with_context(|| format!("re-read the USN of {}", path.display()))?;
    anyhow::ensure!(
        now == usn,
        "{} was written since USN {usn} (now {now}); not asserting in-sync over content \
         nobody proved",
        path.display()
    );
    let result = unsafe {
        CfSetInSyncState(
            handle,
            CF_IN_SYNC_STATE_IN_SYNC,
            CF_SET_IN_SYNC_FLAG_NONE,
            None, // the platform's USN condition is unusable (see above); the handle guards
        )
    };
    drop(file);
    result.context("CfSetInSyncState failed")?;
    tracing::debug!(path = %path.display(), usn, "marked in sync");
    Ok(())
}

/// The file's (or directory's) current update sequence number — what [`set_in_sync`]
/// is conditioned on. Read through `FSCTL_READ_FILE_USN_DATA` on an attributes-only
/// handle, so it never opens the data stream and cannot trigger a recall. 0 on a volume
/// without a USN journal (which [`set_in_sync`] refuses).
pub fn file_usn(path: &Path) -> Result<i64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    /// `FILE_READ_ATTRIBUTES` — enough for the FSCTL, and never a data open.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    /// Required to open a directory handle at all; harmless on a regular file.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| format!("open {} to read its USN", path.display()))?;
    usn_of_handle(HANDLE(file.as_raw_handle()))
        .with_context(|| format!("read the USN of {}", path.display()))
}

/// The OS's `CF_PLACEHOLDER_STATE` bits for `path` (a file or a directory), read through
/// `CfGetPlaceholderStateFromFileInfo` over an attributes-only handle — so it never opens
/// the data stream, and (the provider's own I/O firing no callbacks) never populates.
/// Directory-population reads go through [`is_listed_directory`].
pub fn placeholder_state(path: &Path) -> Result<u32> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    /// `FILE_READ_ATTRIBUTES` — enough for the attribute-tag query, never a data open.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    /// Required to open a directory handle at all; harmless on a regular file.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// Open the reparse point itself — the tag is what the state is derived from.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("open {} to read its placeholder state", path.display()))?;
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileAttributeTagInfo,
            &mut info as *mut FILE_ATTRIBUTE_TAG_INFO as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .with_context(|| format!("read the attribute tag of {}", path.display()))?;
    let state = unsafe {
        CfGetPlaceholderStateFromFileInfo(
            &info as *const FILE_ATTRIBUTE_TAG_INFO as *const core::ffi::c_void,
            FileAttributeTagInfo,
        )
    };
    Ok(state.0)
}

/// Has the OS finished listing the directory at `path` — so it will **never** fire
/// FETCH_PLACEHOLDERS for it again, and a child the provider does not push itself never
/// appears? `CF_PLACEHOLDER_STATE_PARTIAL` clear on a readable state. Measured on a live
/// root (2026-09-29, `cfapi_live_integration::a_remote_create_into_a_browsed_directory_materializes`):
///
/// | directory | state |
/// |---|---|
/// | the root, never browsed | `0x33` (PLACEHOLDER\|SYNC_ROOT\|PARTIAL\|PARTIALLY_ON_DISK) |
/// | the root, browsed | `0x3` |
/// | a subdirectory, browsed | `0x9` (PLACEHOLDER\|IN_SYNC) |
/// | a subdirectory, never browsed | `0x39` (…\|PARTIAL\|PARTIALLY_ON_DISK) |
///
/// An ordinary (non-placeholder) directory carries no `PARTIAL` either, and rightly
/// reads listed: the OS never populates it. `CF_PLACEHOLDER_STATE_INVALID` (unreadable)
/// reads NOT listed — pushing a child into a directory the OS will still populate would
/// race its own listing, so the unsure answer is the lazy one.
pub fn is_listed_directory(path: &Path) -> Result<bool> {
    let state = placeholder_state(path)?;
    Ok(state != CF_PLACEHOLDER_STATE_INVALID.0 && state & CF_PLACEHOLDER_STATE_PARTIAL.0 == 0)
}

/// `FSCTL_READ_FILE_USN_DATA` on an open handle: the USN of the file's newest change
/// journal record.
fn usn_of_handle(handle: HANDLE) -> Result<i64> {
    use windows::Win32::System::IO::DeviceIoControl;
    /// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 58, METHOD_NEITHER, FILE_ANY_ACCESS)`.
    const FSCTL_READ_FILE_USN_DATA: u32 = 0x0009_00EB;
    /// `READ_FILE_USN_DATA` — V3 is what ReFS (128-bit file ids) answers with; NTFS
    /// answers V2. A writable, aligned stack value: a promoted `const` lands in read-only
    /// memory, which the METHOD_NEITHER FSCTL rejects `0x800706F8` (measured).
    #[repr(C, align(8))]
    struct ReadFileUsnData {
        min_major_version: u16,
        max_major_version: u16,
    }
    let mut versions = ReadFileUsnData {
        min_major_version: 2,
        max_major_version: 3,
    };
    // A USN_RECORD_V2/V3 header plus the file name; 1 KiB covers any NTFS name.
    let mut record = [0u64; 128];
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_READ_FILE_USN_DATA,
            Some(&mut versions as *mut ReadFileUsnData as *const core::ffi::c_void),
            4, // sizeof(READ_FILE_USN_DATA): two WORDs
            Some(record.as_mut_ptr() as *mut core::ffi::c_void),
            std::mem::size_of_val(&record) as u32,
            Some(&mut returned),
            None,
        )
    }
    .context("FSCTL_READ_FILE_USN_DATA")?;
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(record.as_ptr() as *const u8, returned as usize) };
    // USN_RECORD_COMMON_HEADER: RecordLength u32, MajorVersion u16, MinorVersion u16; then
    // two file references (u64 each in V2, 128-bit in V3) and the USN.
    let usn_at = match bytes.get(4..6).map(|v| u16::from_le_bytes([v[0], v[1]])) {
        Some(2) => 24,
        Some(3) => 40,
        other => anyhow::bail!("unexpected USN record version {other:?}"),
    };
    let usn = bytes
        .get(usn_at..usn_at + 8)
        .context("USN record too short")?;
    Ok(i64::from_le_bytes(usn.try_into().expect("8 bytes")))
}

/// Re-anchor an **ordinary** file under a sync root as a HYDRATED cloud placeholder that is
/// **not** in sync — `CfConvertToPlaceholder` with no flags and `rel_identity` as its
/// `FileIdentity`. The bytes stay local and nothing is vouched for: the result is exactly
/// what an in-place edit leaves (a dirty placeholder the platform refuses to dehydrate),
/// which is why this may run before the content is proven. The in-sync assertion is then a
/// separate, USN-conditioned [`set_in_sync`] — `CfConvertToPlaceholder` takes no USN
/// condition, so a convert that also marked in-sync would vouch for whatever bytes were on
/// disk at that instant.
///
/// Same plain-Win32 handle and mandatory non-empty identity as
/// [`convert_to_placeholder_in_sync`] (the oplock-protected handle is refused `0x80070006`
/// here; an identity-less placeholder can't route a later FETCH_DATA).
pub fn convert_to_placeholder_anchored(path: &Path, rel_identity: &str) -> Result<()> {
    anyhow::ensure!(
        !rel_identity.is_empty(),
        "placeholder identity must be non-empty (cfapi rejects it and the fetch path can't route)"
    );
    let identity = rel_identity.as_bytes();
    use std::os::windows::io::AsRawHandle;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open {} for conversion", path.display()))?;
    let handle = HANDLE(file.as_raw_handle());
    let result = unsafe {
        CfConvertToPlaceholder(
            handle,
            Some(identity.as_ptr() as *const core::ffi::c_void),
            identity.len() as u32,
            CF_CONVERT_FLAG_NONE,
            None, // no USN output
            None, // no overlapped
        )
    };
    drop(file);
    result.context("CfConvertToPlaceholder(anchor, not in sync) failed")?;
    tracing::debug!(path = %path.display(), rel_identity, "re-anchored as a not-in-sync placeholder");
    Ok(())
}

// ---------------------------------------------------------------------------
// Pin state (the user's expressed intent, read from OS attributes)
// ---------------------------------------------------------------------------

/// `FILE_ATTRIBUTE_PINNED` — the user asked for this file to be kept hydrated
/// ("Always keep on this device").
pub const FILE_ATTRIBUTE_PINNED: u32 = 0x0008_0000;

/// `FILE_ATTRIBUTE_UNPINNED` — the user asked for this file's local bytes to be
/// freed ("Free up space").
pub const FILE_ATTRIBUTE_UNPINNED: u32 = 0x0010_0000;

/// A placeholder's pin state as the user (or shell) last expressed it.
///
/// Explorer's cloud verbs are **pure pin-state writes** (measured 2026-07-16,
/// `file-sync.md` § Per-file sync-status display): *"Free up space"* =
/// `CfSetPinState(UNPINNED)`, *"Always keep on this device"* = `CfSetPinState(PINNED)`.
/// The OS does no byte work — the provider observes the transition and acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinState {
    /// The user wants the file kept hydrated on this device.
    Pinned,
    /// The user wants the file's local bytes freed.
    Unpinned,
    /// No expressed preference (or both bits set — contradictory, treated as none).
    Unspecified,
}

/// Classify raw OS file attributes into a [`PinState`]. Pure, so it is unit-testable
/// and callable from a scan that already holds the attributes.
///
/// Both bits set is contradictory (the OS shouldn't produce it) → `Unspecified`,
/// because acting on a contradiction in either direction would be a guess.
pub fn pin_state_from_attrs(attrs: u32) -> PinState {
    match (
        attrs & FILE_ATTRIBUTE_PINNED != 0,
        attrs & FILE_ATTRIBUTE_UNPINNED != 0,
    ) {
        (true, false) => PinState::Pinned,
        (false, true) => PinState::Unpinned,
        _ => PinState::Unspecified,
    }
}

/// [`pin_state_from_attrs`] for a path: stat-only (reads attributes, never data),
/// so it cannot itself trigger a recall.
pub fn pin_state(path: &Path) -> Result<PinState> {
    use std::os::windows::fs::MetadataExt;
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat {} for pin state", path.display()))?;
    Ok(pin_state_from_attrs(meta.file_attributes()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open a file handle suitable for Cloud Files API operations.
///
/// Uses `CfOpenFileWithOplock` which acquires the proper oplock for cfapi use.
pub fn open_file_handle(path: &Path) -> Result<HANDLE> {
    let path_h = HSTRING::from(path.as_os_str());
    let handle = unsafe {
        CfOpenFileWithOplock(&path_h, CF_OPEN_FILE_FLAG_NONE)
            .context("CfOpenFileWithOplock failed")?
    };
    Ok(handle)
}

/// Close a file handle. Uses `CfCloseHandle` which properly releases the oplock.
///
/// # Safety
/// `handle` must be a valid handle previously obtained from `open_file_handle`.
unsafe fn close_handle(handle: HANDLE) {
    unsafe { CfCloseHandle(handle) };
}

/// Convert a Unix timestamp (**seconds** since 1970-01-01) to a Windows FILETIME
/// value (100-nanosecond intervals since 1601-01-01).
///
/// **Saturating, never panicking.** This runs under an engine future on the
/// engine-host thread. On 2026-07-13 a millisecond timestamp reaching this function
/// panicked (`attempt to multiply with overflow`) and took down *every* sync root on
/// the host at once — silently, since every engine shared one current-thread runtime,
/// so the unwind escaped `block_on`, and a detached service's stderr has nobody
/// reading it.
///
/// ⚠ **That blast radius is now contained, and this function is still saturating on
/// purpose — do not "simplify" it back.** `fauna_sync_engine`'s engine host wraps each
/// engine future in `catch_unwind` (`engine_host.rs`, the `start_one!` macro), so a
/// panic here would now retire *this* root and leave its siblings serving. Two reasons
/// the saturating arithmetic still earns its place: losing one root for one bad row is
/// still disproportionate, and containment does nothing about the **release** build,
/// which is the worse half — the workspace sets no `[profile.release]` and
/// `overflow-checks` defaults off, so shipped artifacts do not panic at all. They wrap,
/// stamping every cloud file with a garbage FILETIME, quietly. Saturating is the only
/// one of the three behaviours that is correct in both profiles.
///
/// The unit bug itself is fixed at its source (`fauna_sync_engine`'s
/// `created_at_ms_to_unix_secs`). This is defence in depth: a bad timestamp should
/// cost at most one wrong file date, never a whole sync root.
pub fn unix_to_filetime(unix_secs: i64) -> i64 {
    // Difference between Windows epoch (1601) and Unix epoch (1970) in seconds:
    // 11644473600 seconds = 369 years worth of seconds
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    // FILETIME uses 100-nanosecond intervals
    const TICKS_PER_SEC: i64 = 10_000_000;

    unix_secs
        .saturating_add(EPOCH_DIFF_SECS)
        .saturating_mul(TICKS_PER_SEC)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin classifier maps the two attribute bits and refuses to guess on a
    /// contradiction. `0x20` = a plain `ARCHIVE` file; `0x0040_1620` = the measured
    /// cloud-only placeholder attributes (`placeholder.rs` module table).
    #[test]
    fn pin_state_classification_maps_bits_and_refuses_contradictions() {
        assert_eq!(pin_state_from_attrs(0x20), PinState::Unspecified);
        assert_eq!(
            pin_state_from_attrs(0x20 | FILE_ATTRIBUTE_PINNED),
            PinState::Pinned
        );
        assert_eq!(
            pin_state_from_attrs(0x0040_1620 | FILE_ATTRIBUTE_UNPINNED),
            PinState::Unpinned
        );
        assert_eq!(
            pin_state_from_attrs(FILE_ATTRIBUTE_PINNED | FILE_ATTRIBUTE_UNPINNED),
            PinState::Unspecified
        );
    }

    /// A brand-new folder is empty, so a zero-entry transfer is the very first call a new
    /// user hits. cfapi wants a **NULL** `PlaceholderArray` then; `Vec::as_mut_ptr` hands
    /// back a dangling-but-non-null pointer instead. The premise — that the naive
    /// `infos.as_mut_ptr()` is non-null even when empty — needs no assertion here: clippy's
    /// `useless_ptr_null_checks` rejects null-checking an `as_mut_ptr()` result *precisely
    /// because* it is never null. That lint is the guarantee.
    #[test]
    fn an_empty_placeholder_set_passes_a_null_array_not_a_dangling_pointer() {
        let mut empty: Vec<CF_PLACEHOLDER_CREATE_INFO> = Vec::new();

        assert!(
            placeholder_array_ptr(&mut empty).is_null(),
            "cfapi requires a NULL PlaceholderArray when PlaceholderCount is 0"
        );
    }

    #[test]
    fn a_non_empty_placeholder_set_passes_the_real_buffer() {
        let mut infos = vec![CF_PLACEHOLDER_CREATE_INFO::default(); 2];
        let expected = infos.as_mut_ptr();

        let got = placeholder_array_ptr(&mut infos);

        assert!(!got.is_null(), "a populated set must not be nulled out");
        assert_eq!(got, expected, "must point at the caller's buffer");
    }

    /// A real timestamp converts exactly: 2023-11-14T22:13:20Z.
    #[test]
    fn a_normal_unix_timestamp_converts_to_the_expected_filetime() {
        // (1_700_000_000 + 11_644_473_600) * 10_000_000
        assert_eq!(unix_to_filetime(1_700_000_000), 133_444_736_000_000_000);
        // The Windows epoch itself is 0 ticks.
        assert_eq!(unix_to_filetime(-11_644_473_600), 0);
    }

    /// Regression, found live on Windows 2026-07-13: a **millisecond** timestamp
    /// reached this function (the nest stamps `sync_changes.created_at` in millis and
    /// the engine was writing it verbatim into `remote_mtime`, which is read as
    /// seconds). The old `(unix_secs + EPOCH_DIFF) * TICKS_PER_SEC` overflowed `i64`
    /// and **panicked on the engine-host thread**, unwinding the shared current-thread
    /// runtime and silently tearing down *every* cfapi sync root on the box.
    ///
    /// **The bug that kept Windows on-demand sync from ever working.** cfapi requires a
    /// non-empty `FileIdentity` on every placeholder ("This is required for files"), and
    /// rejects the *entire* `TRANSFER_PLACEHOLDERS` operation with
    /// `ERROR_CLOUD_FILE_INVALID_REQUEST` (0x8007017C) when one is missing — naming no field.
    /// This crate shipped `FileIdentity: NULL`, so no directory could ever be populated.
    ///
    /// The identity is derived from `rel_path`, so the only way to produce an empty one is an
    /// empty `rel_path` — which `transfer_placeholders` rejects up front rather than letting
    /// cfapi fail opaquely. The live proof is `tests/live_population.rs`.
    #[test]
    fn a_placeholders_identity_is_its_rel_path_and_is_never_empty() {
        let file = PlaceholderInfo {
            rel_path: "docs/report.pdf".to_string(),
            size: 12,
            mtime: 1_700_000_000,
            is_dir: false,
        };

        // cfapi wants ONLY the leaf as RelativeFileName ("It should consist only of the file
        // or directory name") — but the WHOLE rel path as the identity.
        assert_eq!(file.leaf(), "report.pdf");
        assert_eq!(file.identity(), b"docs/report.pdf");
        assert!(!file.identity().is_empty());

        // A root-level entry has no separator: leaf and identity coincide, still non-empty.
        let root_level = PlaceholderInfo {
            rel_path: "hello.txt".to_string(),
            ..file
        };
        assert_eq!(root_level.leaf(), "hello.txt");
        assert_eq!(root_level.identity(), b"hello.txt");
    }

    /// `ParamSize` must be `CF_SIZE_OF_OP_PARAM(<the union member in use>)` — the member's
    /// offset plus **its own** size — NOT `size_of::<CF_OPERATION_PARAMETERS>()`, which is the
    /// size of the *largest* member. That is the documented rule and what both reference
    /// providers (Nextcloud, cloud-filter-rs) compute.
    ///
    /// ⚠ It is *not* what `CfExecute` rejects — measured live, the whole-union size is
    /// accepted too. An earlier comment here claimed this field was why nothing ever worked;
    /// it was not (the identity above was). Keep the value correct; don't credit it.
    #[test]
    fn param_size_is_the_member_size_not_the_whole_union() {
        let member = cf_size_of_op_param::<CF_OPERATION_PARAMETERS_0_4>();
        let whole = std::mem::size_of::<CF_OPERATION_PARAMETERS>() as u32;

        assert_eq!(
            member,
            (std::mem::offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
                + std::mem::size_of::<CF_OPERATION_PARAMETERS_0_4>()) as u32,
            "must equal the C macro CF_SIZE_OF_OP_PARAM(TransferPlaceholders)"
        );
        assert_ne!(
            member, whole,
            "if these ever coincide this test has stopped distinguishing anything"
        );
        // TransferData's variant is sized the same; pin it too, since hydration rides it.
        assert_eq!(cf_size_of_op_param::<CF_OPERATION_PARAMETERS_0_0>(), member);
    }

    #[test]
    fn an_out_of_range_timestamp_saturates_instead_of_panicking() {
        let millis = 1_700_000_000_000; // what the nest actually sends, mistaken for secs
        assert_eq!(
            unix_to_filetime(millis),
            i64::MAX,
            "must clamp, not overflow — a panic here kills every engine on the host"
        );
        assert_eq!(unix_to_filetime(i64::MAX), i64::MAX);
        assert_eq!(unix_to_filetime(i64::MIN), i64::MIN);
    }
}
