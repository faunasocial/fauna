//! The device-place editor's projection — the one implementation the seven apps
//! paint `folder-place-row` from (`ui/folders.md` § Implementation status today).
//!
//! These pin the arm that is a trap rather than a detail: a toggle yields the
//! WHOLE point rather than the box that moved.

use fauna_protocol::folders::{
    FolderMember, PLACE_FLAG_ACCEPTS, PLACE_FLAG_APPLIES_DELETES, PLACE_FLAG_ORIGINATES,
    PlaceFlags, place_rows, toggled,
};

/// No unsealed device roster at hand — every seat keeps the label the nest sent.
const NO_ROSTER: [(&str, &str); 0] = [];

fn member(device: &str, label: &str, flags: PlaceFlags) -> FolderMember {
    FolderMember {
        device_id: device.to_string(),
        label: label.to_string(),
        flags,
        ..Default::default()
    }
}

/// A seat is painted from its flags — any of the eight points.
#[test]
fn a_seat_is_read_from_its_flags() {
    let archive = PlaceFlags::archive_place();
    let rows = place_rows(&[member("aa", "laptop", archive.clone())], NO_ROSTER);

    assert_eq!(rows.len(), 1);
    assert!(rows[0].originates);
    assert!(rows[0].accepts);
    assert!(!rows[0].applies_deletes);
    assert_eq!(rows[0].flags(), archive);
}

/// Every seat keeps its row in roster order — `folder-place-row[j]` is the
/// address the cross-app e2e contract drives.
#[test]
fn rows_keep_roster_order() {
    let rows = place_rows(
        &[
            member("aa", "first", PlaceFlags::default_place()),
            member("bb", "second", PlaceFlags::new(false, false, false)),
            member("cc", "third", PlaceFlags::new(true, false, false)),
        ],
        NO_ROSTER,
    );

    let ids: Vec<_> = rows.iter().map(|r| r.device_id.as_str()).collect();
    assert_eq!(ids, ["aa", "bb", "cc"]);
}

/// The point applies whole: flipping one box must carry the other two along
/// unchanged, or the nest clears them.
#[test]
fn a_toggle_yields_the_whole_point_not_the_box_that_moved() {
    let rows = place_rows(
        &[member("aa", "laptop", PlaceFlags::default_place())],
        NO_ROSTER,
    );

    // default → the archive point.
    let archived = toggled(&rows[0], PLACE_FLAG_APPLIES_DELETES).expect("a known flag toggles");
    assert_eq!(
        archived.flags(),
        PlaceFlags::archive_place(),
        "the two boxes left alone must survive the edit"
    );

    // → originates only.
    let source = toggled(&archived, PLACE_FLAG_ACCEPTS).expect("a known flag toggles");
    assert_eq!(source.flags(), PlaceFlags::new(true, false, false));

    // The identity the write is addressed by rides along untouched.
    assert_eq!(source.device_id, "aa");
    assert_eq!(source.label, "laptop");
}

/// Every one of the three ids the e2e contract clicks must move its own flag,
/// and nothing else.
#[test]
fn each_flag_id_moves_exactly_its_own_box() {
    let rows = place_rows(
        &[member("aa", "laptop", PlaceFlags::default_place())],
        NO_ROSTER,
    );
    for (flag, expected) in [
        (PLACE_FLAG_ORIGINATES, (false, true, true)),
        (PLACE_FLAG_ACCEPTS, (true, false, true)),
        (PLACE_FLAG_APPLIES_DELETES, (true, true, false)),
    ] {
        let next = toggled(&rows[0], flag).expect("a known flag toggles");
        assert_eq!(next.flags().point(), expected, "{flag} moved the wrong box");
    }
}

/// An id this binary does not know is not an excuse to write a half point.
#[test]
fn an_unknown_flag_id_writes_nothing() {
    let rows = place_rows(
        &[member("aa", "laptop", PlaceFlags::default_place())],
        NO_ROSTER,
    );
    assert_eq!(toggled(&rows[0], "teleports"), None);
}

/// Every user-chosen device label rests sealed, so `members.list` hands the
/// editor an EMPTY label for a named seat. The name comes from the device
/// roster the app already unsealed, joined on `device_id` — a seat the roster
/// does not hold (another account's device) keeps whatever the nest sent, which
/// is nameless or a machine-written constant (`path-sealing.md` § device label,
/// gap (a)).
#[test]
fn a_seat_is_named_from_the_unsealed_device_roster() {
    let d = PlaceFlags::default_place;
    let rows = place_rows(
        &[
            member("aa", "", d()),
            member("bb", "", PlaceFlags::archive_place()),
            member("ff", "", d()),
            member("ee", "WebDAV", d()),
        ],
        [
            ("bb", "Home NAS"),
            ("aa", "Work laptop"),
            ("dd", "Not a seat"),
        ],
    );

    let names: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        names,
        ["Work laptop", "Home NAS", "", "WebDAV"],
        "a roster device is named by the roster; a seat outside it keeps the nest's label, \
         and roster order never reorders the seats"
    );
}
