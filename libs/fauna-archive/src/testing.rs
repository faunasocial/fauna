//! The generated fixture corpus as zips, for this crate's tests and
//! downstream machines' — never a real export (`archive-import.md`
//! § Parser contract rule 7: "Fixtures are generated, never real. No real
//! export ever enters the repo.").
//!
//! JSON members are Deflated (the real exports are), everything else Stored.
//! Paths are sorted so the archive is byte-deterministic.
//!
//! Compiled only under `cfg(test)` or the `test-helpers` feature, so a
//! release build of the crate carries none of it.
//!
//! The corpus is embedded with `include_bytes!`, never read from
//! `env!("CARGO_MANIFEST_DIR")` at run time: an `env!` makes the checkout's
//! absolute path part of every compile of this module, so a compile cache
//! keyed on it (`build-target-layout-windows.md`) could never share the unit
//! between two checkouts of the repository. A file added under
//! `tests/fixtures/` goes in [`FIXTURES`]; `fixtures_table_matches_the_tree`
//! fails until it does.

use std::io::{Cursor, Write};
use std::path::Path;

use crate::source::VecSource;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

macro_rules! fixtures {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../tests/fixtures/", $path)))),*]
    };
}

/// Every fixture file, by its path under `tests/fixtures/`.
const FIXTURES: &[(&str, &[u8])] = fixtures![
    "facebook-html/index.html",
    "facebook-html/personal_information/profile_information/profile_information.html",
    "facebook-html/your_facebook_activity/posts/your_posts_1.html",
    "facebook-json/ads_information/advertisers_you_have_interacted_with.json",
    "facebook-json/connections/friends/your_friends.json",
    "facebook-json/personal_information/profile_information/profile_information.json",
    "facebook-json/your_facebook_activity/comments_and_reactions/comments.json",
    "facebook-json/your_facebook_activity/comments_and_reactions/likes_and_reactions_1.json",
    "facebook-json/your_facebook_activity/events/your_event_responses.json",
    "facebook-json/your_facebook_activity/groups/your_group_membership_activity.json",
    "facebook-json/your_facebook_activity/messages/inbox/friendone_abc123/message_1.json",
    "facebook-json/your_facebook_activity/messages/inbox/friendone_abc123/photos/img_1.jpg",
    "facebook-json/your_facebook_activity/posts/album/0.json",
    "facebook-json/your_facebook_activity/posts/media/Mobile_uploads/photo_2.jpg",
    "facebook-json/your_facebook_activity/posts/media/Mobile_uploads/photo_3.jpg",
    "facebook-json/your_facebook_activity/posts/media/Timeline_photos/photo_1.jpg",
    "facebook-json/your_facebook_activity/posts/your_posts__check_ins__photos_and_videos_1.json",
    "facebook-minimal/personal_information/profile_information/profile_information.json",
    "facebook-minimal/your_facebook_activity/posts/your_posts_1.json",
];

/// Zips the named fixture tree under `tests/fixtures/` in memory, from the
/// embedded [`FIXTURES`] — so it is the same bytes whichever crate calls it.
pub fn fixture_zip(name: &str) -> Vec<u8> {
    let prefix = format!("{name}/");
    let mut files: Vec<(&str, &[u8])> = FIXTURES
        .iter()
        .filter_map(|(path, bytes)| path.strip_prefix(prefix.as_str()).map(|rel| (rel, *bytes)))
        .collect();
    assert!(!files.is_empty(), "no fixture tree named {name:?}");
    // Component-wise, as the directory walk this replaced sorted its paths.
    files.sort_by(|a, b| Path::new(a.0).cmp(Path::new(b.0)));
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in files {
        let method = if name.ends_with(".json") {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        w.start_file(
            name,
            SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// [`fixture_zip`] wrapped in the in-memory [`VecSource`] every parser reads
/// through.
pub fn fixture_source(name: &str) -> VecSource {
    VecSource(fixture_zip(name))
}

/// A zip from literal members (Stored) — for shape-specific edge cases.
pub fn zip_of(members: &[(&str, &[u8])]) -> VecSource {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, bytes) in members {
        w.start_file(*name, stored).unwrap();
        w.write_all(bytes).unwrap();
    }
    VecSource(w.finish().unwrap().into_inner())
}

#[cfg(test)]
mod tests {
    use super::FIXTURES;
    use std::path::Path;

    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("fixture dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under root");
                out.push(rel.to_str().expect("utf-8 path").replace('\\', "/"));
            }
        }
    }

    /// The embedded table is the tree: no fixture file left out, none listed
    /// that is gone. Cargo runs a test with the package root as its working
    /// directory, so the relative path needs no `env!`.
    #[test]
    fn fixtures_table_matches_the_tree() {
        let root = Path::new("tests/fixtures");
        let mut on_disk = Vec::new();
        walk(root, root, &mut on_disk);
        on_disk.retain(|p| p != "README.md");
        on_disk.sort();
        let mut listed: Vec<String> = FIXTURES.iter().map(|(p, _)| p.to_string()).collect();
        listed.sort();
        assert_eq!(listed, on_disk);
    }
}
