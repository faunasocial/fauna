//! The line `folder-lease-status` paints — whether a lease-governed folder is
//! writable right now (`docs/goal/ui/folders.md` § Exclusive editing; the
//! lease itself is `docs/goal/behavior/file-sync.md` § Exclusive editing).
//!
//! One resolver so no app re-derives the three readings, the expiry rule, or
//! the never-the-hex rule. It reads only what the folder projection already
//! carries (`FolderSummary.lease`) and the roster the page already holds — it
//! never asks the nest, because the only kind that could answer ("acquire")
//! takes the lease as a side effect of asking.

use fauna_core::localized::LocalizedText;

use crate::snapshots::{DeviceSummary, FolderSummary};

/// The status line for `folder`, or `None` when the line is absent — the
/// folder's exclusive editing is off, so there is no lease to report.
///
/// * no lease, or one whose `expires_at` is not after `now_secs` → *free*
///   (the nest sweeps expired rows lazily, so a stale row can outlive its
///   expiry in the projection);
/// * the holder is `this_device_row` (`crate::this_device_row`'s answer) →
///   *this device is editing*;
/// * any other holder → *‹label› is editing*, the label looked up in
///   `devices`; a holder the roster cannot name (a member reading an owner's
///   folder holds no roster of the owner's devices; a blank label) →
///   *another device is editing*. Never the hex id.
pub fn folder_lease_status(
    folder: &FolderSummary,
    devices: &[DeviceSummary],
    this_device_row: Option<&str>,
    now_secs: i64,
) -> Option<LocalizedText> {
    if !folder.exclusive_editing {
        return None;
    }
    let Some(lease) = folder.lease.as_ref().filter(|l| l.expires_at > now_secs) else {
        return Some(LocalizedText::key("devices.folder_lease_free"));
    };
    if this_device_row == Some(lease.device_id.as_str()) {
        return Some(LocalizedText::key("devices.folder_lease_held_here"));
    }
    let label = devices
        .iter()
        .find(|d| d.device_id == lease.device_id)
        .map(|d| d.label.trim())
        .filter(|l| !l.is_empty());
    Some(match label {
        Some(label) => LocalizedText::key_arg("devices.folder_lease_held_by", "device", label),
        None => LocalizedText::key("devices.folder_lease_held_elsewhere"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshots::FolderLeaseSummary;

    const NOW: i64 = 1_000;
    const HERE: &str = "aa11";
    const THERE: &str = "bb22";

    fn device(id: &str, label: &str) -> DeviceSummary {
        DeviceSummary {
            device_id: id.into(),
            label: label.into(),
            capabilities: String::new(),
            registered_at: 0,
            last_seen_at: 0,
            online: true,
            guardian_marked: false,
            folders: vec![],
            principal: None,
            p2p_participation: None,
            p2p_off_requested: false,
            p2p_participation_paint: None,
        }
    }

    fn folder(governed: bool, lease: Option<(&str, i64)>) -> FolderSummary {
        FolderSummary {
            name: "db".into(),
            exclusive_editing: governed,
            lease: lease.map(|(id, exp)| FolderLeaseSummary {
                device_id: id.into(),
                expires_at: exp,
            }),
            ..Default::default()
        }
    }

    fn roster() -> Vec<DeviceSummary> {
        vec![device(HERE, "My laptop"), device(THERE, "Studio desktop")]
    }

    fn status(f: &FolderSummary) -> Option<LocalizedText> {
        folder_lease_status(f, &roster(), Some(HERE), NOW)
    }

    /// Off ⇒ no line at all — even with a lease row still in the projection
    /// (disarming does not revoke a held lease, but it is no longer the
    /// user's question).
    #[test]
    fn absent_while_exclusive_editing_is_off() {
        assert_eq!(status(&folder(false, None)), None);
        assert_eq!(status(&folder(false, Some((THERE, NOW + 60)))), None);
    }

    #[test]
    fn free_when_unleased_or_the_lease_has_lapsed() {
        let free = Some(LocalizedText::key("devices.folder_lease_free"));
        assert_eq!(status(&folder(true, None)), free);
        assert_eq!(status(&folder(true, Some((THERE, NOW)))), free);
        assert_eq!(status(&folder(true, Some((THERE, NOW - 1)))), free);
    }

    #[test]
    fn this_device_holding_it_says_so() {
        assert_eq!(
            status(&folder(true, Some((HERE, NOW + 60)))),
            Some(LocalizedText::key("devices.folder_lease_held_here"))
        );
    }

    /// The holder is named by the label the user picked — the hex id must
    /// never reach the text.
    #[test]
    fn another_device_is_named_by_its_label_never_its_id() {
        let text = status(&folder(true, Some((THERE, NOW + 60)))).unwrap();
        assert_eq!(
            text,
            LocalizedText::key_arg("devices.folder_lease_held_by", "device", "Studio desktop")
        );
        let resolved = text.resolve(|_| Some("{device} is editing".to_string()));
        assert!(!resolved.contains(THERE), "{resolved}");
    }

    /// A reader with no roster entry for the holder (a member on an owner's
    /// folder; a blank label) still gets a sentence — never the hex.
    #[test]
    fn an_unnamed_holder_reads_as_another_device() {
        let elsewhere = Some(LocalizedText::key("devices.folder_lease_held_elsewhere"));
        let f = folder(true, Some(("cc33", NOW + 60)));
        assert_eq!(status(&f), elsewhere);
        let blank = vec![device(THERE, "  ")];
        let f = folder(true, Some((THERE, NOW + 60)));
        assert_eq!(folder_lease_status(&f, &blank, Some(HERE), NOW), elsewhere);
    }

    /// No known own row (a fresh install) cannot claim the lease as ours.
    #[test]
    fn an_unknown_own_row_never_reads_as_held_here() {
        let f = folder(true, Some((HERE, NOW + 60)));
        assert_eq!(
            folder_lease_status(&f, &roster(), None, NOW),
            Some(LocalizedText::key_arg(
                "devices.folder_lease_held_by",
                "device",
                "My laptop"
            ))
        );
    }
}
