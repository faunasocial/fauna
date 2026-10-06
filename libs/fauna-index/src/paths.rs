//! Virtual-path conventions for the `__index/` reserved folder.
//!
//! These strings are the single source of truth for what gets written into
//! the `path` column of `sync_changes` rows on the nest. Writers (Plan 5+)
//! call `manifest_path()` / `segment_path()`; readers (Plan 6+) parse paths
//! that came back from the nest with `parse_segment_path()` to recover the
//! kind + segment id.
//!
//! The spec D4 layout is fixed: `__index/manifest.idx` and
//! `__index/<kind>/seg-<8-digit>.idx`. No other names are valid; the parser
//! is strict so a malformed path can't be silently accepted.

use crate::types::{ContentKind, KindClass};

/// Reserved folder name on the nest, in the `__`-prefixed internal namespace
/// (`__mls`, `__drafts`).
pub const INDEX_FOLDER: &str = "__index";

/// Filename within `INDEX_FOLDER` for the per-actor **master-class** manifest
/// (kinds conversation / post / file / contact / draft / media, sealed under the
/// index master key).
pub const MANIFEST_FILE_NAME: &str = "manifest.idx";

/// Filename within `INDEX_FOLDER` for the per-actor **mail/calendar** manifest
/// (kinds mail + calendar, sealed under the MSEK-derived index-segment key so
/// the MDA bridge can open it without the cross-kind master key — the S0-ratified
/// per-kind manifest split, `content-index.md` § Encryption posture).
pub const MAILCAL_MANIFEST_FILE_NAME: &str = "manifest-mailcal.idx";

/// Virtual path for the master-class manifest. Always one per actor.
pub fn manifest_path() -> String {
    format!("{INDEX_FOLDER}/{MANIFEST_FILE_NAME}")
}

/// Virtual path for the mail/calendar manifest. Always one per actor.
pub fn mailcal_manifest_path() -> String {
    format!("{INDEX_FOLDER}/{MAILCAL_MANIFEST_FILE_NAME}")
}

/// Virtual path for a kind+seq segment. Segments are immutable after write —
/// each new segment increments `KindManifest::next_seg_id` and gets a fresh
/// path here.
pub fn segment_path(kind: ContentKind, seq: u32) -> String {
    format!("{INDEX_FOLDER}/{}/seg-{seq:08}.idx", kind.as_str())
}

/// Which key class a valid `__index` virtual path belongs to — `None` for any
/// string this module could not have produced.
///
/// **Why a path-derived answer at all:** the at-rest bytes carry no class
/// discriminator (both classes share the seal framing and its magic), so a
/// holder that must decide "may this writer touch this blob?" *before* it has a
/// key — the nest, gating the MDA's bridge-plane rail reach — has nothing but
/// the path to go on. This is that decision, once, here, rather than re-derived
/// by each consumer from [`parse_segment_path`] plus the two manifest names.
///
/// Total over the accepted set: every path [`segment_path`] /
/// [`manifest_path`] / [`mailcal_manifest_path`] can emit maps to exactly one
/// class, and adding a [`ContentKind`] cannot silently escape it — the mapping
/// goes through [`ContentKind::class`], whose match is exhaustive.
pub fn path_key_class(path: &str) -> Option<KindClass> {
    if path == manifest_path() {
        return Some(KindClass::Master);
    }
    if path == mailcal_manifest_path() {
        return Some(KindClass::MailCal);
    }
    parse_segment_path(path).map(|(kind, _)| kind.class())
}

/// Inverse of [`segment_path`]. Returns `None` for any path that doesn't
/// match the exact `__index/<kind>/seg-<8-digit>.idx` shape.
pub fn parse_segment_path(path: &str) -> Option<(ContentKind, u32)> {
    let rest = path.strip_prefix(INDEX_FOLDER)?.strip_prefix('/')?;
    let (kind_str, file) = rest.split_once('/')?;
    let kind = match kind_str {
        "mail" => ContentKind::Mail,
        "calendar" => ContentKind::Calendar,
        "conversation" => ContentKind::Conversation,
        "post" => ContentKind::Post,
        "file" => ContentKind::File,
        "contact" => ContentKind::Contact,
        "draft" => ContentKind::Draft,
        "media" => ContentKind::Media,
        _ => return None,
    };
    let seq_str = file.strip_prefix("seg-")?.strip_suffix(".idx")?;
    if seq_str.len() != 8 {
        return None;
    }
    let seq: u32 = seq_str.parse().ok()?;
    Some((kind, seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification a holder-without-a-key depends on: every path this
    /// module can emit lands in exactly one class, and the mail/calendar side is
    /// exactly {mail, calendar} + the mailcal manifest — which is what bounds a
    /// MUA credential's index reach (`key-material-hierarchy.md` rule #7).
    #[test]
    fn every_emittable_path_classifies_and_the_two_classes_are_disjoint() {
        assert_eq!(path_key_class(&manifest_path()), Some(KindClass::Master));
        assert_eq!(
            path_key_class(&mailcal_manifest_path()),
            Some(KindClass::MailCal)
        );
        for kind in [
            ContentKind::Mail,
            ContentKind::Calendar,
            ContentKind::Conversation,
            ContentKind::Post,
            ContentKind::File,
            ContentKind::Contact,
            ContentKind::Draft,
            ContentKind::Media,
        ] {
            let p = segment_path(kind, 7);
            assert_eq!(
                path_key_class(&p),
                Some(kind.class()),
                "{p} must classify as its kind's class"
            );
        }
    }

    /// `None` is the honest answer for anything outside the rail — a caller
    /// gating on the class must not be handed a default it could mistake for a
    /// decision.
    #[test]
    fn a_path_the_rail_never_emits_has_no_class() {
        for bad in [
            "",
            "notes/secret.txt",
            "__index/mail/seg-1.idx",
            "__index/nonsense/seg-00000001.idx",
            "__index/manifest.txt",
            "__config/user.faunaconfig",
            "__index/manifest-mailcal.idx.bak",
        ] {
            assert_eq!(path_key_class(bad), None, "{bad:?} must not classify");
        }
    }
}
