//! The device-place editor's rows, named from this page's own device roster.
//!
//! [`fauna_protocol::folders::place_rows`] owns the projection; this is the one
//! place the apps hand it the names. Every app already holds the unsealed
//! roster as [`DeviceSummary`] rows (`DevicesSnapshot::devices`, opened under
//! owner-only custody by `DevicesMachine::render_devices`), and a
//! `folder-place-row` with no roster to join paints every user-named seat
//! blank — `fauna.folders.members.list` carries no plaintext label for them
//! (`path-sealing.md` § device label, gap (a)).

use fauna_protocol::folders::{FolderMember, PlaceRow};

use crate::snapshots::DeviceSummary;

/// Project `members` (`fauna.folders.members.list`, in reply order) into the
/// editor's rows, naming each seat from `devices` — the roster this app painted
/// the Devices page from. A seat `devices` does not hold keeps the nest's label.
pub fn place_rows(members: &[FolderMember], devices: &[DeviceSummary]) -> Vec<PlaceRow> {
    fauna_protocol::folders::place_rows(
        members,
        devices
            .iter()
            .map(|d| (d.device_id.as_str(), d.label.as_str())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, label: &str) -> DeviceSummary {
        DeviceSummary {
            device_id: id.to_string(),
            label: label.to_string(),
            capabilities: "read,write".to_string(),
            registered_at: 0,
            last_seen_at: 0,
            online: false,
            guardian_marked: false,
            folders: Vec::new(),
            principal: None,
            p2p_participation: None,
            p2p_off_requested: false,
            p2p_participation_paint: None,
        }
    }

    fn seat(id: &str) -> FolderMember {
        FolderMember {
            device_id: id.to_string(),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }
    }

    #[test]
    fn seats_take_their_names_from_the_device_roster() {
        let rows = place_rows(
            &[seat("aa"), seat("bb")],
            &[device("bb", "Home NAS"), device("aa", "Work laptop")],
        );
        let names: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(names, ["Work laptop", "Home NAS"]);
    }
}
