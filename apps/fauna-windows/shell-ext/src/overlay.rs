//! Overlay icon status matching logic.
//!
//! Four overlay kinds (Synced, Syncing, CloudOnly, Error), each matching
//! one FileStatus variant. The should_show_overlay function checks the
//! ShellCache and optionally queries the pipe for cache misses.

use std::path::Path;

use fauna_ipc::sync::FileStatus;

use crate::cache::ShellCache;

/// Which overlay variant this handler represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Synced,
    Syncing,
    CloudOnly,
    Error,
}

impl OverlayKind {
    pub fn matches(&self, status: FileStatus) -> bool {
        matches!(
            (self, status),
            (OverlayKind::Synced, FileStatus::Synced)
                | (OverlayKind::Syncing, FileStatus::Syncing)
                | (OverlayKind::CloudOnly, FileStatus::CloudOnly)
                | (OverlayKind::Error, FileStatus::Error)
        )
    }

    pub fn icon_filename(&self) -> &'static str {
        match self {
            OverlayKind::Synced => "synced.ico",
            OverlayKind::Syncing => "syncing.ico",
            OverlayKind::CloudOnly => "cloud.ico",
            OverlayKind::Error => "error.ico",
        }
    }

    pub fn registry_key(&self) -> &'static str {
        match self {
            OverlayKind::Synced => "  FaunaSynced",
            OverlayKind::Syncing => "  FaunaSyncing",
            OverlayKind::CloudOnly => "  FaunaCloudOnly",
            OverlayKind::Error => "  FaunaError",
        }
    }
}

/// Slow-path status-query callback for [`should_show_overlay`]: invoked only on a
/// cache miss to fetch a path's [`FileStatus`] over the sync pipe.
type StatusQueryFn = dyn Fn(&Path) -> Option<FileStatus>;

/// Determine if this overlay handler should claim a given file path.
pub fn should_show_overlay(
    kind: OverlayKind,
    path: &Path,
    cache: &ShellCache,
    query_fn: Option<&StatusQueryFn>,
) -> bool {
    if let Some(status) = cache.get(path) {
        return kind.matches(status);
    }

    if let Some(qfn) = query_fn
        && let Some(status) = qfn(path)
    {
        cache.set(path.to_path_buf(), status);
        return kind.matches(status);
    }

    false
}

// ── Windows COM implementation ──────────────────────────────────────────────

/// The four overlay `OverlayKind`s in registration / priority order.
pub const OVERLAY_KINDS: [OverlayKind; 4] = [
    OverlayKind::Synced,
    OverlayKind::Syncing,
    OverlayKind::CloudOnly,
    OverlayKind::Error,
];

#[cfg(windows)]
pub mod com {
    //! `IShellIconOverlayIdentifier` COM classes — one instance per `OverlayKind`.
    //!
    //! Each handler reads the shared `ShellCache` (kept fresh by the event listener)
    //! and, on a cache miss, performs a single synchronous `GetFileStatus` query over
    //! the sync pipe. `IsMemberOf` returns `S_OK` when the file's status matches this
    //! handler's kind, `S_FALSE` otherwise.

    // `GetOverlayInfo`'s out-params are raw pointers fixed by the windows-rs
    // `IShellIconOverlayIdentifier` COM vtable ABI — they cannot become safe
    // references and the generated trait method cannot be marked `unsafe`, so
    // `not_unsafe_ptr_arg_deref` is inapplicable here (the derefs are already in
    // an `unsafe` block).
    #![allow(clippy::not_unsafe_ptr_arg_deref)]

    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use fauna_ipc::sync::{FileStatus, RequestMethod, ResponsePayload, ResponseResult};
    use fauna_ipc::sync_pipe_client::SyncPipeClient;
    use windows::Win32::Foundation::S_FALSE;
    use windows::Win32::UI::Shell::{
        ISIOI_ICONFILE, ISIOI_ICONINDEX, IShellIconOverlayIdentifier,
        IShellIconOverlayIdentifier_Impl,
    };
    use windows::core::{Error, PCWSTR, PWSTR, Result, implement};

    use super::{OverlayKind, should_show_overlay};

    /// One COM overlay handler, bound to a single status.
    #[implement(IShellIconOverlayIdentifier)]
    pub struct OverlayHandler {
        kind: OverlayKind,
    }

    impl OverlayHandler {
        pub fn new(kind: OverlayKind) -> Self {
            crate::dll::object_added();
            Self { kind }
        }
    }

    impl Drop for OverlayHandler {
        fn drop(&mut self) {
            crate::dll::object_released();
        }
    }

    /// Synchronous single-file status query over the sync pipe (cache-miss path).
    /// Returns `None` if the service is down or the path is untracked. Shared with
    /// the context-menu handlers (`context_menu::com`) for their slow-path lookups.
    pub(crate) fn query_status(path: &Path) -> Option<FileStatus> {
        let client = SyncPipeClient::connect_pipe().ok()?;
        let resp = client
            .request(RequestMethod::GetFileStatus {
                path: path.to_string_lossy().into_owned(),
            })
            .ok()?;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => Some(info.status),
            _ => None,
        }
    }

    impl IShellIconOverlayIdentifier_Impl for OverlayHandler_Impl {
        fn IsMemberOf(&self, pwszpath: &PCWSTR, _dwattrib: u32) -> Result<()> {
            let path_str = unsafe { pwszpath.to_string() }.unwrap_or_default();
            if path_str.is_empty() {
                return Err(Error::from_hresult(S_FALSE));
            }
            let path = Path::new(&path_str);
            let g = crate::global();
            let query = query_status;
            if should_show_overlay(self.kind, path, &g.cache, Some(&query)) {
                Ok(()) // S_OK — this handler claims the file
            } else {
                Err(Error::from_hresult(S_FALSE)) // not ours
            }
        }

        fn GetOverlayInfo(
            &self,
            pwsziconfile: PWSTR,
            cchmax: i32,
            pindex: *mut i32,
            pdwflags: *mut u32,
        ) -> Result<()> {
            let g = crate::global();
            let icon_path = g.icon_dir.join(self.kind.icon_filename());
            let wide: Vec<u16> = icon_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let cap = cchmax.max(0) as usize;
                if !pwsziconfile.is_null() && cap > 0 {
                    let n = wide.len().min(cap - 1); // reserve room for the NUL
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), pwsziconfile.0, n);
                    *pwsziconfile.0.add(n) = 0;
                }
                if !pindex.is_null() {
                    *pindex = 0;
                }
                if !pdwflags.is_null() {
                    *pdwflags = ISIOI_ICONFILE | ISIOI_ICONINDEX;
                }
            }
            Ok(())
        }

        fn GetPriority(&self) -> Result<i32> {
            Ok(0) // highest
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        #[test]
        fn get_priority_is_zero() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let h: IShellIconOverlayIdentifier = OverlayHandler::new(OverlayKind::Synced).into();
            let prio = unsafe { h.GetPriority() }.unwrap();
            assert_eq!(prio, 0);
        }

        #[test]
        fn get_overlay_info_writes_icon_path() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let h: IShellIconOverlayIdentifier = OverlayHandler::new(OverlayKind::Error).into();
            let mut buf = [0u16; 260];
            let mut index = -1i32;
            let mut flags = 0u32;
            unsafe { h.GetOverlayInfo(&mut buf, &mut index, &mut flags) }.unwrap();
            let nul = buf.iter().position(|&c| c == 0).unwrap();
            let written = String::from_utf16_lossy(&buf[..nul]);
            assert!(written.ends_with("error.ico"), "got: {written}");
            assert_eq!(index, 0);
            assert_eq!(flags, ISIOI_ICONFILE | ISIOI_ICONINDEX);
        }

        #[test]
        fn object_count_tracks_lifetime() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let before = crate::dll::object_count();
            let h: IShellIconOverlayIdentifier = OverlayHandler::new(OverlayKind::Synced).into();
            assert_eq!(crate::dll::object_count(), before + 1);
            drop(h);
            assert_eq!(crate::dll::object_count(), before);
        }

        #[test]
        fn is_member_of_does_not_panic_on_cache_hit() {
            let _serial = crate::dll::OBJECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Seed the shared cache so the cache-hit path runs (no pipe needed).
            crate::global()
                .cache
                .set(PathBuf::from("C:\\seed-overlay.txt"), FileStatus::Synced);
            let h: IShellIconOverlayIdentifier = OverlayHandler::new(OverlayKind::Synced).into();
            let wide: Vec<u16> = "C:\\seed-overlay.txt"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // Through the Result wrapper both S_OK and S_FALSE map to Ok (S_FALSE is a
            // success HRESULT); we assert it runs without panicking and claims the match.
            let r = unsafe { h.IsMemberOf(PCWSTR(wide.as_ptr()), 0) };
            assert!(r.is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn synced_matches_only_synced() {
        assert!(OverlayKind::Synced.matches(FileStatus::Synced));
        assert!(!OverlayKind::Synced.matches(FileStatus::Syncing));
        assert!(!OverlayKind::Synced.matches(FileStatus::CloudOnly));
        assert!(!OverlayKind::Synced.matches(FileStatus::Error));
        assert!(!OverlayKind::Synced.matches(FileStatus::NotTracked));
    }

    #[test]
    fn syncing_matches_only_syncing() {
        assert!(OverlayKind::Syncing.matches(FileStatus::Syncing));
        assert!(!OverlayKind::Syncing.matches(FileStatus::Synced));
    }

    #[test]
    fn cloud_matches_only_cloud() {
        assert!(OverlayKind::CloudOnly.matches(FileStatus::CloudOnly));
        assert!(!OverlayKind::CloudOnly.matches(FileStatus::Synced));
    }

    #[test]
    fn error_matches_only_error() {
        assert!(OverlayKind::Error.matches(FileStatus::Error));
        assert!(!OverlayKind::Error.matches(FileStatus::Synced));
    }

    #[test]
    fn should_show_overlay_cache_hit() {
        let cache = ShellCache::new();
        cache.set(PathBuf::from("C:\\test.txt"), FileStatus::Synced);

        assert!(should_show_overlay(
            OverlayKind::Synced,
            Path::new("C:\\test.txt"),
            &cache,
            None
        ));
        assert!(!should_show_overlay(
            OverlayKind::Syncing,
            Path::new("C:\\test.txt"),
            &cache,
            None
        ));
    }

    #[test]
    fn should_show_overlay_cache_miss_with_query() {
        let cache = ShellCache::new();
        let query = |_path: &Path| -> Option<FileStatus> { Some(FileStatus::CloudOnly) };

        assert!(should_show_overlay(
            OverlayKind::CloudOnly,
            Path::new("C:\\cloud.txt"),
            &cache,
            Some(&query)
        ));
        assert_eq!(
            cache.get(Path::new("C:\\cloud.txt")),
            Some(FileStatus::CloudOnly)
        );
    }

    #[test]
    fn should_show_overlay_cache_miss_no_query() {
        let cache = ShellCache::new();
        assert!(!should_show_overlay(
            OverlayKind::Synced,
            Path::new("C:\\unknown.txt"),
            &cache,
            None
        ));
    }

    #[test]
    fn icon_filenames() {
        assert_eq!(OverlayKind::Synced.icon_filename(), "synced.ico");
        assert_eq!(OverlayKind::Syncing.icon_filename(), "syncing.ico");
        assert_eq!(OverlayKind::CloudOnly.icon_filename(), "cloud.ico");
        assert_eq!(OverlayKind::Error.icon_filename(), "error.ico");
    }

    #[test]
    fn registry_keys_are_space_prefixed() {
        for kind in [
            OverlayKind::Synced,
            OverlayKind::Syncing,
            OverlayKind::CloudOnly,
            OverlayKind::Error,
        ] {
            assert!(kind.registry_key().starts_with("  Fauna"));
        }
    }

    /// Parse one of the embedded single-image 16×16 32-bit .ico assets into
    /// `[[RGBA; 16]; 16]`, row 0 = top (the ICO/BMP payload is bottom-up BGRA).
    fn parse_badge_ico(data: &[u8]) -> Vec<Vec<(u8, u8, u8, u8)>> {
        assert_eq!(&data[..6], &[0, 0, 1, 0, 1, 0], "single-entry .ico header");
        assert_eq!(data[6], 16, "width 16");
        assert_eq!(data[7], 16, "height 16");
        assert_eq!(u16::from_le_bytes([data[12], data[13]]), 32, "32 bpp");
        let off = u32::from_le_bytes([data[18], data[19], data[20], data[21]]) as usize;
        let pixels = &data[off + 40..]; // skip BITMAPINFOHEADER
        let mut rows = Vec::with_capacity(16);
        for y in 0..16usize {
            let mut row = Vec::with_capacity(16);
            for x in 0..16usize {
                let i = ((15 - y) * 16 + x) * 4; // bottom-up
                row.push((pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]));
            }
            rows.push(row);
        }
        rows
    }

    /// **The badge-geometry contract** (`apps/windows.md` § Shell Extension):
    /// Windows composites an overlay icon across the base icon's WHOLE rect, so
    /// the glyph must sit in the lower-left quadrant with everything else fully
    /// transparent. The original placeholder assets were a disc centred on the
    /// full canvas — they rendered as "a gray circle which covers the whole
    /// icon" (user, live pass 2026-07-14) — and all four shared one silhouette
    /// with only the fill colour differing. Interim assets with correct
    /// geometry were USER-approved 2026-07-16; this pins the geometry (and
    /// per-state colour distinctness) so no regeneration can regress it. The
    /// assets come from a dedicated dev-fleet overlay-icon generator.
    #[test]
    fn badge_icons_have_lower_left_quadrant_geometry() {
        let icons: [(&str, &[u8]); 4] = [
            ("synced", include_bytes!("icons/synced.ico")),
            ("syncing", include_bytes!("icons/syncing.ico")),
            ("cloud", include_bytes!("icons/cloud.ico")),
            ("error", include_bytes!("icons/error.ico")),
        ];
        let mut dominant = Vec::new();
        for (name, data) in icons {
            let px = parse_badge_ico(data);
            // Everything outside the lower-left quadrant is fully transparent.
            for (y, row) in px.iter().enumerate() {
                for (x, &(_, _, _, a)) in row.iter().enumerate() {
                    if y < 8 || x >= 8 {
                        assert_eq!(
                            a, 0,
                            "{name}: pixel ({x},{y}) outside the lower-left quadrant \
                             must be transparent — a full-canvas glyph swallows the file icon"
                        );
                    }
                }
            }
            // The quadrant holds a real glyph, not emptiness.
            let opaque: Vec<(u8, u8, u8)> = (8..16usize)
                .flat_map(|y| (0..8usize).map(move |x| (x, y)))
                .map(|(x, y)| px[y][x])
                .filter(|&(_, _, _, a)| a == 255)
                .map(|(r, g, b, _)| (r, g, b))
                .collect();
            assert!(
                opaque.len() >= 20,
                "{name}: expected a visible glyph in the quadrant, got {} opaque px",
                opaque.len()
            );
            // Dominant non-white fill — the per-state colour.
            let mut counts = std::collections::HashMap::new();
            for c in opaque.iter().filter(|&&c| c != (255, 255, 255)) {
                *counts.entry(*c).or_insert(0usize) += 1;
            }
            let (&fill, _) = counts
                .iter()
                .max_by_key(|(_, n)| **n)
                .expect("a fill colour");
            dominant.push(fill);
        }
        // The four states are colour-distinct (the placeholder assets differed
        // ONLY by fill — keep at least that property through any regeneration).
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert_ne!(
                    dominant[i], dominant[j],
                    "badge fills must be distinct per state (icons {i} vs {j})"
                );
            }
        }
    }
}
