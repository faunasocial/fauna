//! Tests for [`crate::enumerate::immediate_children`] — the pure, platform-agnostic
//! lazy-directory-listing logic shared by every on-demand file provider.

use crate::enumerate::{DirChild, PlaceholderRow, immediate_children};

fn row(rel: &str, size: u64, mtime: i64) -> PlaceholderRow {
    PlaceholderRow {
        rel: rel.to_string(),
        size,
        mtime,
    }
}

#[test]
fn root_lists_top_level_files_and_subdirs() {
    let rows = vec![
        row("a.txt", 10, 100),
        row("sub/b.txt", 20, 200),
        row("sub/c.txt", 30, 300),
        row("zed.bin", 40, 400),
    ];
    let children = immediate_children(&rows, "");
    assert_eq!(
        children,
        vec![
            // Directory first, then files; each group lexicographic.
            DirChild {
                name: "sub".into(),
                size: 0,
                mtime: 300, // newest descendant mtime
                is_dir: true,
            },
            DirChild {
                name: "a.txt".into(),
                size: 10,
                mtime: 100,
                is_dir: false,
            },
            DirChild {
                name: "zed.bin".into(),
                size: 40,
                mtime: 400,
                is_dir: false,
            },
        ]
    );
}

#[test]
fn subdir_lists_only_its_immediate_children() {
    let rows = vec![
        row("a.txt", 10, 100),
        row("sub/b.txt", 20, 200),
        row("sub/deep/c.txt", 30, 300),
        row("sub/deep/d.txt", 35, 350),
        row("other/e.txt", 50, 500),
    ];
    let children = immediate_children(&rows, "sub");
    assert_eq!(
        children,
        vec![
            DirChild {
                name: "deep".into(),
                size: 0,
                mtime: 350,
                is_dir: true,
            },
            DirChild {
                name: "b.txt".into(),
                size: 20,
                mtime: 200,
                is_dir: false,
            },
        ]
    );
}

#[test]
fn parent_with_trailing_slash_is_normalized() {
    let rows = vec![row("sub/b.txt", 20, 200)];
    assert_eq!(
        immediate_children(&rows, "sub/"),
        immediate_children(&rows, "sub")
    );
}

#[test]
fn unrelated_prefix_is_not_a_false_match() {
    // "sub2/x" must not be treated as a child of "sub".
    let rows = vec![row("sub2/x.txt", 1, 1), row("sub/y.txt", 2, 2)];
    let children = immediate_children(&rows, "sub");
    assert_eq!(
        children,
        vec![DirChild {
            name: "y.txt".into(),
            size: 2,
            mtime: 2,
            is_dir: false,
        }]
    );
}

#[test]
fn empty_directory_yields_no_children() {
    let rows = vec![row("a.txt", 10, 100)];
    assert!(immediate_children(&rows, "nonexistent").is_empty());
}

#[test]
fn directory_appears_once_for_multiple_descendants() {
    let rows = vec![
        row("d/1.txt", 1, 10),
        row("d/2.txt", 2, 20),
        row("d/e/3.txt", 3, 30),
    ];
    let children = immediate_children(&rows, "");
    assert_eq!(children.len(), 1, "single deduplicated directory entry");
    assert_eq!(children[0].name, "d");
    assert!(children[0].is_dir);
    assert_eq!(children[0].mtime, 30, "newest descendant mtime");
}
