//! React to Explorer's pin-state verbs — the byte work behind *"Free up space"*
//! and *"Always keep on this device"*.
//!
//! Both verbs are **pure `CfSetPinState` writes** (measured 2026-07-16;
//! `docs/goal/behavior/file-sync.md` § Per-file sync-status display, answered
//! question 2): the OS does no byte work and fires no callback — the **provider**
//! is expected to observe the pin-state transition and act. This module owns the
//! observation half: classifying what, if anything, the provider owes a file
//! given its pin state and byte-presence, plus the sweep that finds candidates a
//! watcher couldn't see (flips made while the service was down).
//!
//! The *acting* half lives in `bridge.rs` (`react_to_pin`), because it needs the
//! hydration host and the OS boundary trait. The division of labor for the two
//! directions is asymmetric, and measured (`diag_pin_reaction_mechanics`):
//!
//! - **PINNED → hydrate:** while the provider is connected, **cldflt hydrates a
//!   newly-pinned placeholder itself** through the provider's own FETCH_DATA — the
//!   live direction needs no provider action at all. Only a pin flipped while the
//!   service was down leaves a pinned placeholder behind, which the sweep heals.
//! - **UNPINNED → dehydrate:** the OS never dehydrates on unpin (bytes measured
//!   intact 180 s later; Storage Sense frees them only under disk pressure, much
//!   later). The provider owes the dehydrate, both live and at sweep time.

use std::path::Path;

/// What the provider owes a file whose pin state and byte-presence disagree.
///
/// The two agreeing combinations — an unpinned placeholder, a pinned hydrated
/// file — are steady states, and [`pin_action_for`] maps them (plus unspecified
/// pin states) to `None`. That is what makes reacting **idempotent**: our own
/// dehydrate/hydrate lands the file in a steady state, so the echo events it
/// generates classify to `None` and the reaction converges instead of looping.
// Matched unconditionally by react_to_pin below, but constructed only by
// pin_action_for/pin_action_for_path (both #[cfg(windows)]) or test-only
// FakeInvalidator mocks — dead-by-construction on a Linux-native --lib build.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinAction {
    /// UNPINNED with bytes local — the user asked to *"Free up space"*.
    Dehydrate,
    /// PINNED while cloud-only — the user asked to *"Always keep on this device"*.
    Hydrate,
}

/// Pure classification of one file's (pin state, byte-presence) pair.
#[cfg(windows)]
pub(crate) fn pin_action_for(
    pin: fauna_cfapi::PinState,
    is_cloud_placeholder: bool,
) -> Option<PinAction> {
    use fauna_cfapi::PinState;
    match (pin, is_cloud_placeholder) {
        (PinState::Unpinned, false) => Some(PinAction::Dehydrate),
        (PinState::Pinned, true) => Some(PinAction::Hydrate),
        _ => None,
    }
}

/// [`pin_action_for`] for a path: stat-only (attributes, never data — reading a
/// cloud-only placeholder from the provider's own process stalls for the full
/// recall timeout). Directories carry pin state too (Explorer sets it recursively)
/// but the byte work is per-file, so they classify to `None`; a missing or
/// unstat-able path is `None` (the delete path owns it).
#[cfg(windows)]
pub(crate) fn pin_action_for_path(abs: &Path) -> Option<PinAction> {
    use std::os::windows::fs::MetadataExt;
    let meta = std::fs::metadata(abs).ok()?;
    if !meta.is_file() {
        return None;
    }
    pin_action_for(
        fauna_cfapi::pin_state_from_attrs(meta.file_attributes()),
        fauna_sync_engine::placeholder::is_cloud_placeholder(&meta),
    )
}

/// Walk `root` and return every file whose pin state demands action, as
/// (folder-relative path, action) pairs — the **sweep** half of the reaction
/// loop, healing flips the watcher never saw (made while the service was down)
/// exactly as `converge` backstops the upload watcher.
///
/// Stat-only via `classify` (production: the invalidator's attribute read), so a
/// sweep never opens file data and cannot itself trigger a recall. Dotfiles are
/// skipped to mirror the scan (`watcher::scan_recursive_filtered`): they are
/// never tracked, so no byte work is ever owed on them. Walk errors skip the
/// entry (a vanished file is the delete path's business, not the sweep's).
///
/// Generic over `classify` so loop tests fake the OS attribute read while the
/// walk itself stays real.
pub(crate) fn sweep_candidates(
    root: &Path,
    classify: &dyn Fn(&Path) -> Option<PinAction>,
) -> Vec<(String, PinAction)> {
    let mut out = Vec::new();
    walk(root, root, classify, &mut out);
    out
}

fn walk(
    root: &Path,
    dir: &Path,
    classify: &dyn Fn(&Path) -> Option<PinAction>,
    out: &mut Vec<(String, PinAction)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk(root, &path, classify, out);
        } else if file_type.is_file()
            && let Some(action) = classify(&path)
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push((fauna_sync_engine::watcher::normalize_rel(rel), action));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier's whole truth table: the two disagreements act, the two
    /// steady states and every unspecified pin do nothing. The steady-state rows
    /// are what make the reaction idempotent (see [`PinAction`]).
    #[cfg(windows)]
    #[test]
    fn classifier_acts_on_disagreements_and_rests_on_steady_states() {
        use fauna_cfapi::PinState;
        // Disagreements → action.
        assert_eq!(
            pin_action_for(PinState::Unpinned, false),
            Some(PinAction::Dehydrate)
        );
        assert_eq!(
            pin_action_for(PinState::Pinned, true),
            Some(PinAction::Hydrate)
        );
        // Steady states → rest.
        assert_eq!(pin_action_for(PinState::Unpinned, true), None);
        assert_eq!(pin_action_for(PinState::Pinned, false), None);
        // No expressed preference → rest, whatever the bytes are doing.
        assert_eq!(pin_action_for(PinState::Unspecified, false), None);
        assert_eq!(pin_action_for(PinState::Unspecified, true), None);
    }

    /// The sweep walks recursively, reports rels in the shared normalized form
    /// (forward slashes), skips dotfiles, and carries the classifier's verdicts.
    #[test]
    fn sweep_walks_recursively_normalizes_rels_and_skips_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("free-me.txt"), b"x").unwrap();
        std::fs::write(root.join("sub").join("keep-me.txt"), b"x").unwrap();
        std::fs::write(root.join("steady.txt"), b"x").unwrap();
        std::fs::write(root.join(".faunaignore"), b"x").unwrap();

        let classify = |p: &Path| -> Option<PinAction> {
            match p.file_name().unwrap().to_str().unwrap() {
                "free-me.txt" => Some(PinAction::Dehydrate),
                "keep-me.txt" => Some(PinAction::Hydrate),
                ".faunaignore" => Some(PinAction::Dehydrate), // must never be reached
                _ => None,
            }
        };
        let mut got = sweep_candidates(root, &classify);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("free-me.txt".to_string(), PinAction::Dehydrate),
                ("sub/keep-me.txt".to_string(), PinAction::Hydrate),
            ]
        );
    }
}
