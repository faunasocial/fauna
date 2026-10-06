//! Fauna Shell Extension — Explorer overlay icons.
//!
//! A COM DLL loaded by explorer.exe that shows sync status overlays
//! on files managed by Fauna.

// The COM entry points that consume the global state live in `#[cfg(windows)]`
// blocks (overlay.rs, context_menu.rs), so off-Windows `global()` and everything
// it reaches has zero callers. We keep compiling the crate on Linux on purpose —
// it is the only type-check most sessions ever run over this code — so suppress
// just the unused complaint, just off-Windows; Windows still gets full dead-code
// detection.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod cache;
pub mod context_menu;
pub mod event_listener;
pub mod overlay;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use cache::ShellCache;
use event_listener::EventListener;

/// Global state, lazily initialized on first COM object creation.
struct GlobalState {
    pub(crate) cache: Arc<ShellCache>,
    _listener: EventListener,
    pub(crate) icon_dir: PathBuf,
}

static GLOBAL: OnceLock<GlobalState> = OnceLock::new();

/// Initialize global state (called once via OnceLock).
fn init_global() -> GlobalState {
    let cache = Arc::new(ShellCache::new());
    let listener = EventListener::spawn(cache.clone());
    let icon_dir = extract_icons();

    GlobalState {
        cache,
        _listener: listener,
        icon_dir,
    }
}

/// Get (or initialize) global state.
pub(crate) fn global() -> &'static GlobalState {
    GLOBAL.get_or_init(init_global)
}

/// Extract embedded .ico files to %LOCALAPPDATA%\Fauna\icons\.
fn extract_icons() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Fauna").join("icons"))
        .unwrap_or_else(|_| std::env::temp_dir().join("fauna-icons"));

    let _ = std::fs::create_dir_all(&base);

    let icons: &[(&str, &[u8])] = &[
        ("synced.ico", include_bytes!("icons/synced.ico")),
        ("syncing.ico", include_bytes!("icons/syncing.ico")),
        ("cloud.ico", include_bytes!("icons/cloud.ico")),
        ("error.ico", include_bytes!("icons/error.ico")),
    ];

    for (name, data) in icons {
        let path = base.join(name);
        let should_write = match std::fs::metadata(&path) {
            Ok(meta) => meta.len() != data.len() as u64,
            Err(_) => true,
        };
        if should_write {
            let _ = std::fs::write(&path, data);
        }
    }

    base
}

// ── Windows-only DLL exports + COM class factory ──

#[cfg(windows)]
pub(crate) mod dll {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows::Win32::Foundation::{
        CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
    use windows::Win32::UI::Shell::{IExplorerCommand, IShellIconOverlayIdentifier};
    use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

    use crate::context_menu::com::{
        FaunaContextMenu, FaunaInfoDevices, FaunaInfoVersions, FaunaShareCommand,
    };
    use crate::overlay::OverlayKind;
    use crate::overlay::com::OverlayHandler;

    /// Outstanding COM object count (drives `DllCanUnloadNow`).
    static OBJECT_COUNT: AtomicU32 = AtomicU32::new(0);

    pub(crate) fn object_added() {
        OBJECT_COUNT.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn object_released() {
        OBJECT_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
    #[cfg(test)]
    pub(crate) fn object_count() -> u32 {
        OBJECT_COUNT.load(Ordering::SeqCst)
    }

    /// Serializes the tests that construct counted COM objects (and so mutate the
    /// process-global `OBJECT_COUNT`). The harness runs tests concurrently in one
    /// process, so without this a sibling test's object creation lands inside
    /// `object_count_tracks_lifetime`'s before/after window and breaks its absolute
    /// assertions. Every shell-ext test that constructs an `OverlayHandler` or a
    /// context-menu COM object MUST hold this for that object's lifetime.
    /// (`can_unload_returns_valid_code` reads the count tolerantly and needs no lock.)
    #[cfg(test)]
    pub(crate) static OBJECT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Cross-process critical section for tests that register/unregister the shell-ext
    /// COM classes against the REAL, user-global `HKEY_CURRENT_USER` hive under the
    /// production CLSID paths (`registry::register`/`unregister`). HKCU is per-*user*,
    /// not per-checkout or per-process, so two `cargo test -p fauna-shell-ext` runs from
    /// different checkouts of the repo — or even two threads inside one test binary,
    /// since the default test harness runs tests concurrently — clobber each other's
    /// registry writes mid-test without this. `OBJECT_TEST_LOCK` above
    /// only serializes same-process COM object-count assertions; this is the
    /// registry-state equivalent, widened to a named kernel mutex so it holds across
    /// processes too. Chose a named mutex over a per-process subkey prefix so the
    /// production `registry::register`/`unregister` path construction stays completely
    /// untouched — only the tests gain a critical section. Every shell-ext test that
    /// calls `registry::register`/`registry::unregister` on `HKEY_CURRENT_USER` MUST
    /// hold this for that critical section. Same pattern as `fauna-sync-agent`'s cfapi
    /// shell-root test guard — both now share `fauna_ipc::test_support`.
    #[cfg(test)]
    pub(crate) fn hkcu_registry_test_guard() -> fauna_ipc::test_support::NamedMutexTestGuard {
        fauna_ipc::test_support::NamedMutexTestGuard::acquire("FaunaShellExtHkcuRegistryTest")
    }

    /// CLSIDs (8 total): 4 overlay handlers + 4 context-menu handlers, per
    /// `docs/goal/architecture/installers/windows.md` § Shell Extension.
    pub mod clsid {
        use windows::core::GUID;
        pub const SYNCED: GUID = GUID::from_u128(0x4a7b8c01_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const SYNCING: GUID = GUID::from_u128(0x4a7b8c02_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const CLOUD_ONLY: GUID = GUID::from_u128(0x4a7b8c03_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const ERROR: GUID = GUID::from_u128(0x4a7b8c04_f1e2_4d3a_b5c6_d7e8f9a0b1c2);

        // Context menu: the root submenu handler plus its three leaf commands.
        pub const CONTEXT_MENU: GUID = GUID::from_u128(0x4a7b8c10_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const SHARE_COMMAND: GUID = GUID::from_u128(0x4a7b8c11_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const INFO_DEVICES: GUID = GUID::from_u128(0x4a7b8c12_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
        pub const INFO_VERSIONS: GUID = GUID::from_u128(0x4a7b8c13_f1e2_4d3a_b5c6_d7e8f9a0b1c2);
    }

    /// The four context-menu CLSIDs with their registration friendly names. Only
    /// the root (`CONTEXT_MENU`) is `CoCreateInstance`d by Explorer — as the
    /// `ExplorerCommandHandler` of the `*\shell\Fauna` verb key, **not** as a legacy
    /// `ContextMenuHandlers` handler (see `registry::CONTEXT_VERB_PATH`); the three
    /// leaves are constructed in-process by the root's `EnumSubCommands`. All four are
    /// registered as COM classes so the DLL exposes the full 8-CLSID contract and any
    /// could be created by CLSID.
    const CONTEXT_CLSIDS: [(GUID, &str); 4] = [
        (clsid::CONTEXT_MENU, "Fauna Context Menu"),
        (clsid::SHARE_COMMAND, "Fauna Share Command"),
        (clsid::INFO_DEVICES, "Fauna Device Info"),
        (clsid::INFO_VERSIONS, "Fauna Version Info"),
    ];

    /// Map a CLSID to its `OverlayKind`, if it is an overlay handler.
    fn overlay_kind_for(clsid: &GUID) -> Option<OverlayKind> {
        match *clsid {
            clsid::SYNCED => Some(OverlayKind::Synced),
            clsid::SYNCING => Some(OverlayKind::Syncing),
            clsid::CLOUD_ONLY => Some(OverlayKind::CloudOnly),
            clsid::ERROR => Some(OverlayKind::Error),
            _ => None,
        }
    }

    /// True if this DLL serves the CLSID.
    fn is_supported(clsid: &GUID) -> bool {
        overlay_kind_for(clsid).is_some() || CONTEXT_CLSIDS.iter().any(|(g, _)| *g == *clsid)
    }

    /// Construct the COM object for a CLSID and `QueryInterface` it into `ppv`.
    fn create_instance(clsid: &GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
        if let Some(kind) = overlay_kind_for(clsid) {
            let obj: IShellIconOverlayIdentifier = OverlayHandler::new(kind).into();
            return unsafe { obj.query(riid, ppv) };
        }
        let cmd: Option<IExplorerCommand> = match *clsid {
            clsid::CONTEXT_MENU => Some(FaunaContextMenu::new().into()),
            clsid::SHARE_COMMAND => Some(FaunaShareCommand::new().into()),
            clsid::INFO_DEVICES => Some(FaunaInfoDevices::new().into()),
            clsid::INFO_VERSIONS => Some(FaunaInfoVersions::new().into()),
            _ => None,
        };
        if let Some(obj) = cmd {
            return unsafe { obj.query(riid, ppv) };
        }
        CLASS_E_CLASSNOTAVAILABLE
    }

    /// Generic COM class factory, parameterised by the CLSID it constructs.
    #[implement(IClassFactory)]
    struct ClassFactory {
        clsid: GUID,
    }

    impl IClassFactory_Impl for ClassFactory_Impl {
        fn CreateInstance(
            &self,
            punkouter: Ref<IUnknown>,
            riid: *const GUID,
            ppvobject: *mut *mut c_void,
        ) -> Result<()> {
            if !punkouter.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            create_instance(&self.clsid, riid, ppvobject).ok()
        }

        fn LockServer(&self, flock: BOOL) -> Result<()> {
            if flock.as_bool() {
                object_added();
            } else {
                object_released();
            }
            Ok(())
        }
    }

    /// COM in-proc server entry point: hand out a class factory for a CLSID.
    #[unsafe(no_mangle)]
    extern "system" fn DllGetClassObject(
        rclsid: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if rclsid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        let clsid = unsafe { *rclsid };
        if !is_supported(&clsid) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory { clsid }.into();
        unsafe { factory.query(riid, ppv) }
    }

    /// COM unload gate: only allow unload when no objects are outstanding.
    #[unsafe(no_mangle)]
    extern "system" fn DllCanUnloadNow() -> HRESULT {
        if OBJECT_COUNT.load(Ordering::SeqCst) == 0 {
            S_OK
        } else {
            S_FALSE
        }
    }

    /// CLSID for an overlay kind (inverse of `overlay_kind_for`).
    fn clsid_for(kind: OverlayKind) -> GUID {
        match kind {
            OverlayKind::Synced => clsid::SYNCED,
            OverlayKind::Syncing => clsid::SYNCING,
            OverlayKind::CloudOnly => clsid::CLOUD_ONLY,
            OverlayKind::Error => clsid::ERROR,
        }
    }

    /// Human-readable CLSID registration name for an overlay kind.
    fn friendly_name(kind: OverlayKind) -> &'static str {
        match kind {
            OverlayKind::Synced => "Fauna Synced Overlay",
            OverlayKind::Syncing => "Fauna Syncing Overlay",
            OverlayKind::CloudOnly => "Fauna Cloud-Only Overlay",
            OverlayKind::Error => "Fauna Error Overlay",
        }
    }

    /// Self-registration of the overlay COM classes + Explorer overlay-identifier
    /// keys. Parameterised by the registry root so the production path
    /// (`HKEY_LOCAL_MACHINE`) and the non-elevated test path (`HKEY_CURRENT_USER`)
    /// share one implementation.
    pub(crate) mod registry {
        use super::{clsid_for, friendly_name};
        use crate::overlay::OVERLAY_KINDS;
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::System::LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            GetModuleFileNameW, GetModuleHandleExW,
        };
        use windows::Win32::System::Registry::{
            HKEY, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
            RegDeleteTreeW, RegSetValueExW,
        };
        use windows::core::{GUID, PCWSTR, Result};

        const OVERLAY_IDS_PATH: &str =
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\ShellIconOverlayIdentifiers";

        /// Address anchor inside this module, for `GET_..._FROM_ADDRESS`.
        static ANCHOR: u8 = 0;

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// Format a GUID as a braced registry string `{XXXXXXXX-....}`.
        pub(crate) fn guid_braced(g: &GUID) -> String {
            format!(
                "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
                g.data1,
                g.data2,
                g.data3,
                g.data4[0],
                g.data4[1],
                g.data4[2],
                g.data4[3],
                g.data4[4],
                g.data4[5],
                g.data4[6],
                g.data4[7],
            )
        }

        /// Absolute path of this DLL on disk.
        pub(crate) fn module_path() -> Option<String> {
            let mut hmod = HMODULE::default();
            let anchor = (&ANCHOR as *const u8).cast::<u16>();
            unsafe {
                GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                        | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                    PCWSTR(anchor),
                    &mut hmod,
                )
                .ok()?;
                let mut buf = [0u16; 1024];
                let n = GetModuleFileNameW(Some(hmod), &mut buf);
                if n == 0 {
                    return None;
                }
                Some(String::from_utf16_lossy(&buf[..n as usize]))
            }
        }

        /// Create `root\subkey` and set a REG_SZ value (`name` empty → default value).
        fn set_sz(root: HKEY, subkey: &str, name: PCWSTR, value: &str) -> Result<()> {
            let sub = wide(subkey);
            let mut hkey = HKEY::default();
            unsafe {
                RegCreateKeyExW(
                    root,
                    PCWSTR(sub.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    None,
                    &mut hkey,
                    None,
                )
                .ok()?;
                let data = wide(value);
                let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
                let res = RegSetValueExW(hkey, name, None, REG_SZ, Some(bytes));
                let _ = RegCloseKey(hkey);
                res.ok()?;
            }
            Ok(())
        }

        /// `*\shell\Fauna` — the verb key whose `ExplorerCommandHandler` value names an
        /// [`IExplorerCommand`] handler for all file types.
        ///
        /// **This is the only registry surface Explorer honours for `IExplorerCommand`.**
        /// The `*\shellex\ContextMenuHandlers\FaunaContextMenu` key demands the *other*
        /// contract — `IShellExtInit` + `IContextMenu` — which `FaunaContextMenu` does
        /// not implement. Registered there, Explorer `CoCreateInstance`s the class,
        /// `QueryInterface`s for `IContextMenu`, gets `E_NOINTERFACE`, and **silently
        /// drops the handler**: no error, no log, no menu. That is exactly what shipped,
        /// and why the Fauna submenu had never once appeared in Explorer — in *either*
        /// the Windows 11 menu or the legacy "Show more options" menu (measured live,
        /// 2026-07-14). Every in-process test passed throughout, because none of them
        /// asserted that the registered key's contract matches the implemented
        /// interface; `registration_contract_matches_implemented_interface` now does.
        ///
        /// Note this surfaces the verb in the **legacy** ("Show more options") menu only.
        /// The Windows 11 *default* menu takes context-menu handlers exclusively from an
        /// MSIX/sparse-package `desktop4:FileExplorerContextMenus` registration, never
        /// from `HKCR` — that is a separate installer track.
        const CONTEXT_VERB_PATH: &str = "Software\\Classes\\*\\shell\\Fauna";

        /// `Directory\shell\Fauna` — the same verb (same CLSID, same
        /// `ExplorerCommandHandler` contract as [`CONTEXT_VERB_PATH`]) for
        /// **folders** (USER-decided 2026-07-16: folders get the submenu with the
        /// reduced leaf set — `context_menu::leaf_hidden_for_folder`). The root's
        /// tracked-ness gate already answers for folders via the badge fold, so
        /// no handler change is needed for visibility. Same caveat as the `*`
        /// key: this drives the **legacy** menu; the Windows 11 default menu
        /// needs the sparse package's `desktop4:FileExplorerContextMenus` to add
        /// a `Directory` type (installer-owned).
        const DIR_CONTEXT_VERB_PATH: &str = "Software\\Classes\\Directory\\shell\\Fauna";

        /// The value under [`CONTEXT_VERB_PATH`] naming the handler's CLSID.
        const EXPLORER_COMMAND_HANDLER: &str = "ExplorerCommandHandler";

        /// Register one in-proc COM class: `CLSID\{..}` default name plus its
        /// `InprocServer32` (`(Default)` = DLL path, `ThreadingModel` = Apartment).
        fn register_clsid(
            root: HKEY,
            dll_path: &str,
            clsid_braced: &str,
            friendly: &str,
        ) -> Result<()> {
            let clsid_key = format!("Software\\Classes\\CLSID\\{clsid_braced}");
            set_sz(root, &clsid_key, PCWSTR::null(), friendly)?;
            let inproc = format!("{clsid_key}\\InprocServer32");
            set_sz(root, &inproc, PCWSTR::null(), dll_path)?;
            let model = wide("ThreadingModel");
            set_sz(root, &inproc, PCWSTR(model.as_ptr()), "Apartment")?;
            Ok(())
        }

        /// Register all overlay + context-menu COM classes and their shell keys
        /// under `root`.
        pub(crate) fn register(root: HKEY, dll_path: &str) -> Result<()> {
            // Overlay handlers + their ShellIconOverlayIdentifiers keys.
            for kind in OVERLAY_KINDS {
                let clsid = guid_braced(&clsid_for(kind));
                register_clsid(root, dll_path, &clsid, friendly_name(kind))?;
                let ov_key = format!("{OVERLAY_IDS_PATH}\\{}", kind.registry_key());
                set_sz(root, &ov_key, PCWSTR::null(), &clsid)?;
            }

            // Context-menu COM classes (root submenu + three leaf commands).
            for (guid, friendly) in super::CONTEXT_CLSIDS {
                register_clsid(root, dll_path, &guid_braced(&guid), friendly)?;
            }

            // The root submenu is the IExplorerCommand handler for all file types
            // AND for folders (the folder reduced set lives in the leaves, not
            // here). Default value = fallback display text; ExplorerCommandHandler
            // = the CLSID.
            let root_clsid = guid_braced(&super::clsid::CONTEXT_MENU);
            let handler_value = wide(EXPLORER_COMMAND_HANDLER);
            for verb_path in [CONTEXT_VERB_PATH, DIR_CONTEXT_VERB_PATH] {
                set_sz(root, verb_path, PCWSTR::null(), "Fauna")?;
                set_sz(root, verb_path, PCWSTR(handler_value.as_ptr()), &root_clsid)?;
            }

            Ok(())
        }

        /// Remove everything `register` created under `root` (best-effort).
        pub(crate) fn unregister(root: HKEY) -> Result<()> {
            for kind in OVERLAY_KINDS {
                let clsid = guid_braced(&clsid_for(kind));
                let clsid_key = wide(&format!("Software\\Classes\\CLSID\\{clsid}"));
                let ov_key = wide(&format!("{OVERLAY_IDS_PATH}\\{}", kind.registry_key()));
                unsafe {
                    let _ = RegDeleteTreeW(root, PCWSTR(clsid_key.as_ptr()));
                    let _ = RegDeleteTreeW(root, PCWSTR(ov_key.as_ptr()));
                }
            }
            for (guid, _friendly) in super::CONTEXT_CLSIDS {
                let clsid_key = wide(&format!("Software\\Classes\\CLSID\\{}", guid_braced(&guid)));
                unsafe {
                    let _ = RegDeleteTreeW(root, PCWSTR(clsid_key.as_ptr()));
                }
            }
            // The verb keys (files + folders).
            for path in [CONTEXT_VERB_PATH, DIR_CONTEXT_VERB_PATH] {
                let key = wide(path);
                unsafe {
                    let _ = RegDeleteTreeW(root, PCWSTR(key.as_ptr()));
                }
            }
            Ok(())
        }

        /// Test helper: does `root\subkey` exist?
        #[cfg(test)]
        pub(crate) fn key_exists(root: HKEY, subkey: &str) -> bool {
            use windows::Win32::System::Registry::{KEY_READ, RegOpenKeyExW};
            let sub = wide(subkey);
            let mut hkey = HKEY::default();
            unsafe {
                if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_READ, &mut hkey).is_ok() {
                    let _ = RegCloseKey(hkey);
                    true
                } else {
                    false
                }
            }
        }

        /// Test helper: read a REG_SZ value (`name` empty → the key's default value).
        ///
        /// `key_exists` is not enough for the context-menu registration: the load-bearing
        /// part is the **`ExplorerCommandHandler` value**, not the key.
        #[cfg(test)]
        pub(crate) fn read_sz(root: HKEY, subkey: &str, name: &str) -> Option<String> {
            use std::ffi::c_void;
            use windows::Win32::System::Registry::{RRF_RT_REG_SZ, RegGetValueW};
            let sub = wide(subkey);
            let val = wide(name);
            let mut buf = [0u16; 512];
            let mut cb = (buf.len() * 2) as u32;
            unsafe {
                RegGetValueW(
                    root,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(val.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr().cast::<c_void>()),
                    Some(&mut cb),
                )
                .ok()
                .ok()?;
            }
            let n = (cb as usize / 2).saturating_sub(1);
            Some(String::from_utf16_lossy(&buf[..n]))
        }
    }

    /// COM self-registration entry point (HKLM; run elevated via `regsvr32`).
    #[unsafe(no_mangle)]
    extern "system" fn DllRegisterServer() -> HRESULT {
        use windows::Win32::Foundation::E_FAIL;
        use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        let Some(path) = registry::module_path() else {
            return E_FAIL;
        };
        match registry::register(HKEY_LOCAL_MACHINE, &path) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }

    /// COM self-unregistration entry point.
    #[unsafe(no_mangle)]
    extern "system" fn DllUnregisterServer() -> HRESULT {
        use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        match registry::unregister(HKEY_LOCAL_MACHINE) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn get_class_object_unknown_clsid_is_unavailable() {
            let unknown = GUID::from_u128(0xdead_dead_dead_dead_dead_dead_dead_dead);
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let hr = DllGetClassObject(&unknown, &IClassFactory::IID, &mut ptr);
            assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
            assert!(ptr.is_null());
        }

        #[test]
        fn get_class_object_overlay_clsid_returns_factory() {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let hr = DllGetClassObject(&clsid::SYNCED, &IClassFactory::IID, &mut ptr);
            assert!(hr.is_ok(), "hr = {hr:?}");
            assert!(!ptr.is_null());
            // Take ownership of the returned ref and release it.
            let _factory: IClassFactory = unsafe { IClassFactory::from_raw(ptr) };
        }

        #[test]
        fn get_class_object_context_menu_clsid_returns_factory() {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let hr = DllGetClassObject(&clsid::CONTEXT_MENU, &IClassFactory::IID, &mut ptr);
            assert!(hr.is_ok(), "hr = {hr:?}");
            assert!(!ptr.is_null());
            let _factory: IClassFactory = unsafe { IClassFactory::from_raw(ptr) };
        }

        #[test]
        fn class_factory_creates_explorer_command_for_each_context_clsid() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Each of the four context-menu CLSIDs must dispatch through the class
            // factory to an `IExplorerCommand` instance.
            for (guid, name) in CONTEXT_CLSIDS {
                let mut fptr: *mut c_void = std::ptr::null_mut();
                let hr = DllGetClassObject(&guid, &IClassFactory::IID, &mut fptr);
                assert!(hr.is_ok(), "factory for {name}: {hr:?}");
                let factory: IClassFactory = unsafe { IClassFactory::from_raw(fptr) };
                let cmd: IExplorerCommand = unsafe { factory.CreateInstance(None) }
                    .unwrap_or_else(|e| panic!("CreateInstance for {name}: {e:?}"));
                drop(cmd);
            }
        }

        #[test]
        fn class_factory_creates_overlay_handler_for_each_overlay_clsid() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Mirror of the context-menu test: each of the four overlay CLSIDs must
            // dispatch through the class factory to an `IShellIconOverlayIdentifier`.
            use crate::overlay::OVERLAY_KINDS;
            for kind in OVERLAY_KINDS {
                let mut fptr: *mut c_void = std::ptr::null_mut();
                let hr = DllGetClassObject(&clsid_for(kind), &IClassFactory::IID, &mut fptr);
                assert!(hr.is_ok(), "factory for {kind:?}: {hr:?}");
                let factory: IClassFactory = unsafe { IClassFactory::from_raw(fptr) };
                let overlay: IShellIconOverlayIdentifier = unsafe { factory.CreateInstance(None) }
                    .unwrap_or_else(|e| panic!("CreateInstance for {kind:?}: {e:?}"));
                drop(overlay);
            }
        }

        #[test]
        fn can_unload_returns_valid_code() {
            // Don't assert a specific value — sibling tests may hold objects
            // concurrently; just confirm it's one of the two valid codes.
            let hr = DllCanUnloadNow();
            assert!(hr == S_OK || hr == S_FALSE);
        }

        #[test]
        fn register_unregister_round_trip_hkcu() {
            use windows::Win32::System::Registry::HKEY_CURRENT_USER;
            // Serialize against any other process (another checkout's `cargo test`)
            // or thread touching the real, user-global HKCU state under these same
            // production CLSID paths.
            let _hkcu = crate::dll::hkcu_registry_test_guard();
            // HKCU needs no elevation; register, verify, then clean up.
            let dll_path = "C:\\test\\fauna_shell.dll";
            registry::register(HKEY_CURRENT_USER, dll_path).expect("register");

            let synced = registry::guid_braced(&clsid::SYNCED);
            let clsid_key = format!("Software\\Classes\\CLSID\\{synced}\\InprocServer32");
            let ov_key = format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\ShellIconOverlayIdentifiers\\{}",
                crate::overlay::OverlayKind::Synced.registry_key()
            );
            assert!(registry::key_exists(HKEY_CURRENT_USER, &clsid_key));
            assert!(registry::key_exists(HKEY_CURRENT_USER, &ov_key));

            // Context-menu CLSID + BOTH IExplorerCommand verb keys (all files +
            // folders — the folder verb is the same CLSID; USER-decided 2026-07-16).
            let ctx = registry::guid_braced(&clsid::CONTEXT_MENU);
            let ctx_inproc = format!("Software\\Classes\\CLSID\\{ctx}\\InprocServer32");
            let verb_key = "Software\\Classes\\*\\shell\\Fauna";
            let dir_verb_key = "Software\\Classes\\Directory\\shell\\Fauna";
            assert!(registry::key_exists(HKEY_CURRENT_USER, &ctx_inproc));
            assert!(registry::key_exists(HKEY_CURRENT_USER, verb_key));
            assert!(registry::key_exists(HKEY_CURRENT_USER, dir_verb_key));

            registry::unregister(HKEY_CURRENT_USER).expect("unregister");
            assert!(!registry::key_exists(HKEY_CURRENT_USER, &clsid_key));
            assert!(!registry::key_exists(HKEY_CURRENT_USER, &ov_key));
            assert!(!registry::key_exists(HKEY_CURRENT_USER, &ctx_inproc));
            assert!(!registry::key_exists(HKEY_CURRENT_USER, verb_key));
            assert!(!registry::key_exists(HKEY_CURRENT_USER, dir_verb_key));
        }

        /// **The test whose absence let a dead context menu ship.**
        ///
        /// A shell registration key is a *contract*: it tells Explorer which COM
        /// interface to `QueryInterface` for. Register a class under a key whose
        /// interface it does not implement and Explorer silently drops it — no error, no
        /// log, an invisible menu. Every other test here passed while the menu had never
        /// once appeared, because they all activated the class and called
        /// `IExplorerCommand` on it *directly*, never asking whether the **registry key
        /// we write** demands that same interface.
        ///
        /// So this asserts both halves together:
        /// 1. the class satisfies `IExplorerCommand` (what the verb key's
        ///    `ExplorerCommandHandler` value promises), and
        /// 2. it does **not** satisfy `IContextMenu` — proving the legacy
        ///    `shellex\ContextMenuHandlers` key would be a broken contract, so we must
        ///    never register there again.
        #[test]
        fn registration_contract_matches_implemented_interface() {
            use windows::Win32::System::Registry::HKEY_CURRENT_USER;
            use windows::Win32::UI::Shell::IContextMenu;
            use windows::core::{IUnknown, Interface};

            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // This test also registers/unregisters against the real, user-global
            // HKCU state below (same production CLSID paths as
            // `register_unregister_round_trip_hkcu`) — serialize that too.
            let _hkcu = crate::dll::hkcu_registry_test_guard();

            let mut fptr: *mut c_void = std::ptr::null_mut();
            let hr = DllGetClassObject(&clsid::CONTEXT_MENU, &IClassFactory::IID, &mut fptr);
            assert!(hr.is_ok(), "factory for FaunaContextMenu: {hr:?}");
            let factory: IClassFactory = unsafe { IClassFactory::from_raw(fptr) };
            let unk: IUnknown =
                unsafe { factory.CreateInstance(None) }.expect("activate FaunaContextMenu");

            // (1) The interface the verb key's ExplorerCommandHandler promises.
            assert!(
                unk.cast::<IExplorerCommand>().is_ok(),
                "FaunaContextMenu must implement IExplorerCommand — the verb key's \
                 ExplorerCommandHandler value promises Explorer exactly that"
            );

            // (2) The interface the legacy ContextMenuHandlers key would demand. It does
            // NOT implement it — which is precisely why registering there produced a
            // permanently invisible menu. If this ever starts passing, someone has added
            // IContextMenu and the registration choice must be revisited deliberately.
            assert!(
                unk.cast::<IContextMenu>().is_err(),
                "FaunaContextMenu does not implement IContextMenu, so it must never be \
                 registered under *\\shellex\\ContextMenuHandlers — Explorer would \
                 QueryInterface, get E_NOINTERFACE, and silently drop the handler"
            );

            // (3) And the registration we actually write must be the matching one —
            // on the all-files verb AND the folder verb (same CLSID, same contract).
            registry::register(HKEY_CURRENT_USER, "C:\\test\\fauna_shell.dll").expect("register");
            let expected = registry::guid_braced(&clsid::CONTEXT_MENU);
            for verb in [
                "Software\\Classes\\*\\shell\\Fauna",
                "Software\\Classes\\Directory\\shell\\Fauna",
            ] {
                let handler = registry::read_sz(HKEY_CURRENT_USER, verb, "ExplorerCommandHandler");
                assert_eq!(
                    handler.as_deref(),
                    Some(expected.as_str()),
                    "{verb} must carry ExplorerCommandHandler = the CLSID"
                );
            }
            assert!(
                !registry::key_exists(
                    HKEY_CURRENT_USER,
                    "Software\\Classes\\*\\shellex\\ContextMenuHandlers\\FaunaContextMenu"
                ),
                "the wrong-contract shellex key must never be written"
            );
            registry::unregister(HKEY_CURRENT_USER).expect("unregister");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_state_initializes() {
        // On non-Windows, EventListener won't connect but should not panic.
        let g = global();
        assert!(g.cache.get(std::path::Path::new("nonexistent")).is_none());
        // Icon dir should exist after initialization
        assert!(g.icon_dir.exists());
    }
}
