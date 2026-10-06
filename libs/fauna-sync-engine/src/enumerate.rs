//! Lazy directory enumeration for on-demand file providers.
//!
//! An on-demand placeholder surface (Windows Cloud Files API, macOS File
//! Provider, a Linux FUSE/overlay) populates a directory's contents *when the
//! user first browses it* — `file-sync.md` § On-Demand Files: "Directory
//! listings populate lazily, so browsing a folder never forces a full download."
//! The platform hands the provider the directory being opened and expects the
//! immediate children back.
//!
//! The provider holds a flat list of tracked file paths (forward-slash,
//! folder-relative — the cross-platform `path_hash` key, `watcher.rs`
//! `relative_path_str`). [`immediate_children`] turns that flat list + a parent
//! directory into the directory's *immediate* entries: the files directly under
//! it, plus a synthesized directory entry for each first path segment of a
//! deeper file. This is pure, platform-agnostic logic so every file provider
//! (Windows cfapi today, macOS File Provider next) shares one implementation.
//!
//! [`PlaceholderLister`] is the seam the provider's enumeration callback bridges
//! to: it yields every tracked placeholder row (path + size + mtime) from the
//! state DB. `SyncEngine` is the production impl; tests use a fake.

use anyhow::Result;

/// One tracked placeholder file, reduced to the fields a directory listing
/// needs: its forward-slash folder-relative path, byte size, and mtime
/// (Unix seconds). Sourced from the state DB's `SyncState::Placeholder` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderRow {
    /// Forward-slash, folder-relative path (e.g. `sub/photo.jpg`).
    pub rel: String,
    pub size: u64,
    /// Unix seconds.
    pub mtime: i64,
}

/// One immediate child of a directory being enumerated: either a tracked file
/// or a synthesized subdirectory (intermediate path segment of a deeper file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirChild {
    /// Final path component only (no separators), e.g. `photo.jpg` or `sub`.
    pub name: String,
    /// File byte size; `0` for a directory.
    pub size: u64,
    /// Unix seconds. For a directory: the newest mtime among its descendants.
    pub mtime: i64,
    pub is_dir: bool,
}

/// Compute the immediate children of directory `parent_rel` from the flat list
/// of tracked placeholder rows. Pure and platform-agnostic.
///
/// `parent_rel` is forward-slash, folder-relative; `""` is the sync root.
/// A row directly under `parent_rel` (no further `/`) yields a file child; a
/// row deeper than `parent_rel` yields a directory child named for its first
/// segment under the parent (deduplicated, carrying the newest descendant
/// mtime). Directories sort before files, each group lexicographically, so the
/// output is deterministic regardless of row order.
pub fn immediate_children(rows: &[PlaceholderRow], parent_rel: &str) -> Vec<DirChild> {
    let parent = parent_rel.trim_matches('/');
    // Prefix every child path must start with: "" at the root, "dir/" otherwise.
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/")
    };

    let mut files: Vec<DirChild> = Vec::new();
    // name -> newest descendant mtime; BTreeMap keeps directories sorted + deduped.
    let mut dirs: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();

    for r in rows {
        let Some(rest) = r.rel.strip_prefix(&prefix) else {
            continue; // not under this directory
        };
        if rest.is_empty() {
            continue; // the directory itself, not a child
        }
        match rest.split_once('/') {
            // Directly under `parent`: a file child.
            None => files.push(DirChild {
                name: rest.to_string(),
                size: r.size,
                mtime: r.mtime,
                is_dir: false,
            }),
            // Deeper: the first segment is an immediate subdirectory.
            Some((dir, _)) => {
                let newest = dirs.entry(dir.to_string()).or_insert(r.mtime);
                *newest = (*newest).max(r.mtime);
            }
        }
    }

    files.sort_by(|a, b| a.name.cmp(&b.name));
    // Directories first (lexicographic via BTreeMap), then files.
    let mut out: Vec<DirChild> = dirs
        .into_iter()
        .map(|(name, mtime)| DirChild {
            name,
            size: 0,
            mtime,
            is_dir: true,
        })
        .collect();
    out.extend(files);
    out
}

/// The directory-listing seam an on-demand file provider's enumeration callback
/// bridges to: yields every tracked placeholder row from the state DB.
/// `SyncEngine` is the production impl (reads `SyncState::Placeholder` rows);
/// tests use a fake. `?Send` / no `Sync` bound mirrors [`crate::FileHydrator`] —
/// `SyncEngine` is `Send + !Sync` (rusqlite `Connection`) and the provider
/// serializes listing onto its single driving task.
#[async_trait::async_trait(?Send)]
pub trait PlaceholderLister: Send {
    async fn list_placeholder_rows(&self) -> Result<Vec<PlaceholderRow>>;
}
