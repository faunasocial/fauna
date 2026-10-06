//! The backup-destination **kind discriminator** and its typed projection.
//!
//! Authority: `docs/goal/behavior/backup-destinations.md` § State & data shape → *Kind
//! discriminator* (ratified 2026-08-02) + § *Third destination kind — client
//! device as custodian*.
//!
//! These tests pin the compatibility property the discriminator exists for:
//! a row written by a *newer* client, naming a kind this build does not
//! implement, decodes without being mistaken for a nest. An older client that
//! misread an `"s3"` row as a nest would try to back up to that row's empty
//! sentinel URL.
//!
//! That property is the reason the projection is a typed view rather than a
//! `matches!(dest.kind.as_str(), "nest")` at each call site: the nest-only
//! fields are simply not reachable through the non-nest arms.

use fauna_core::data::{BackupDestination, DestinationKind};
use fauna_core::encoding::{canonical_decode, canonical_encode};

#[test]
fn client_device_row_round_trips_and_projects() {
    let dest = BackupDestination {
        destination_id: "d-ipad".into(),
        folder_name: "__mail".into(),
        added_at: 1_700_000_000,
        kind: "client-device".into(),
        custodian_device_id: Some("dev-abc".into()),
        capacity_cap_bytes: Some(2_000_000_000_000),
        ..Default::default()
    };

    let bytes = canonical_encode(&dest).expect("client-device row encodes");
    let decoded: BackupDestination = canonical_decode(&bytes).expect("and decodes");
    assert_eq!(
        decoded, dest,
        "the row round-trips through the at-rest encoding"
    );

    match decoded.kind_view() {
        DestinationKind::ClientDevice {
            device_id,
            capacity_cap_bytes,
        } => {
            assert_eq!(device_id, "dev-abc");
            assert_eq!(capacity_cap_bytes, Some(2_000_000_000_000));
        }
        other => panic!("expected ClientDevice, got {other:?}"),
    }
}

#[test]
fn an_unimplemented_kind_is_inert_never_nest() {
    // What a future S3 pass writes. This build must not drive it — and above
    // all must not read `destination_nest_url` (an empty sentinel on a non-nest
    // row) and try to back up to it.
    let s3_row = BackupDestination {
        destination_id: "d-s3".into(),
        kind: "s3".into(),
        folder_name: "__mail".into(),
        ..Default::default()
    };

    match s3_row.kind_view() {
        DestinationKind::Inert { kind } => assert_eq!(kind, "s3"),
        other => panic!(
            "an unimplemented kind must be Inert so an older client cannot act on it, got {other:?}"
        ),
    }
}

#[test]
fn a_client_device_row_without_a_device_id_is_inert() {
    // The nest projects a custodian's status row from check-ins keyed by the
    // device id, so a client-device row without one is not drivable. It must
    // land in the same do-not-act arm as an unknown kind rather than yielding a
    // `ClientDevice` arm every caller then has to re-validate.
    let malformed = BackupDestination {
        destination_id: "d-broken".into(),
        kind: "client-device".into(),
        custodian_device_id: None,
        ..Default::default()
    };

    match malformed.kind_view() {
        DestinationKind::Inert { kind } => assert_eq!(kind, "client-device"),
        other => panic!("a device-id-less client-device row must be Inert, got {other:?}"),
    }
}

#[test]
fn default_is_a_nest_row_so_fixtures_keep_their_meaning() {
    // `BackupDestination { .., ..Default::default() }` is the ratified fixture
    // convention (data.rs's own doc comment). Every such fixture predates the
    // discriminator and means "nest", so a *derived* Default — which would give
    // `kind: ""` — would silently strand them all in the Inert arm.
    let d = BackupDestination::default();
    assert_eq!(d.kind, "nest");
    assert!(matches!(d.kind_view(), DestinationKind::Nest { .. }));
}

// ── The custodian-assignment matcher ─────────────────────────────────────────
//
// Slice 3d's shared half. Two hosts must answer "is one of these rows *my*
// custodian assignment?" and they read different carriers: the seed-holding app
// reads the at-rest `BackupDestination` rows, and the bearer-only sync agent
// reads the nest registry's wire rows back over its authed connection
// (`docs/goal/architecture/apps/sync-agent.md` § Control plane split — *policy
// through the nest, never over local IPC*). One rule, fed by both.

use fauna_core::data::{CustodianAssignment, CustodianRowRef, custodian_assignment_for};

/// A `client-device` row for `device_id`, as the at-rest carrier.
fn custodian_row(destination_id: &str, device_id: &str, cap: Option<u64>) -> BackupDestination {
    BackupDestination {
        destination_id: destination_id.into(),
        kind: "client-device".into(),
        custodian_device_id: Some(device_id.into()),
        capacity_cap_bytes: cap,
        ..Default::default()
    }
}

#[test]
fn a_device_finds_its_own_row_and_its_cap() {
    let rows = [
        BackupDestination {
            destination_id: "peer-nest".into(),
            ..Default::default()
        },
        custodian_row("mine", "dev-a", Some(64 << 30)),
        custodian_row("someone-elses-laptop", "dev-b", Some(1 << 30)),
    ];
    let found = custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a");
    assert_eq!(
        found,
        Some(CustodianAssignment {
            destination_id: "mine".into(),
            capacity_cap_bytes: Some(64 << 30),
            // The assignment names the device it matched, so the check-in that
            // rides it cannot report a different one than the row it drives.
            device_id: "dev-a".into(),
        })
    );
}

#[test]
fn a_device_that_is_not_a_custodian_finds_nothing() {
    let rows = [
        BackupDestination::default(),
        custodian_row("someone-elses", "dev-b", None),
    ];
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a"),
        None
    );
}

#[test]
fn an_uncapped_row_reads_as_uncapped_not_as_zero() {
    // `None` means "fill the disk"; a zero cap would mean "hold nothing", and
    // the pull pass would report cap-reached forever having stored nothing.
    let rows = [custodian_row("mine", "dev-a", None)];
    let found =
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a").expect("found");
    assert_eq!(found.capacity_cap_bytes, None);
}

#[test]
fn a_blank_device_id_matches_nothing_on_either_side() {
    // The failure this refuses is invisible: a device whose capability has not
    // carried a sync id yet would pair with a row that reached the registry
    // without one, and start sealing the owner's whole corpus to disk under a
    // `destination_id` no status row describes.
    let rows = [custodian_row("ghost", "", None)];
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), ""),
        None,
        "blank on both sides"
    );
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a"),
        None,
        "blank row, real device"
    );
    let real = [custodian_row("mine", "dev-a", None)];
    assert_eq!(
        custodian_assignment_for(real.iter().map(|d| d.custodian_row()), "   "),
        None,
        "real row, blank (whitespace) device"
    );
}

#[test]
fn two_rows_naming_this_device_refuse_rather_than_pick_one() {
    // Enrollment writes one row per device, so this is a state no client
    // produces. Picking the first would silently honour one cap and ignore the
    // other — and the cap is the only thing between a custodian and a full disk.
    let rows = [
        custodian_row("a", "dev-a", Some(1 << 30)),
        custodian_row("b", "dev-a", Some(500 << 30)),
    ];
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a"),
        None
    );
}

#[test]
fn an_unimplemented_kind_never_assigns_work() {
    // Forwards-compat: a newer client's `"s3"` row carrying a device id it means
    // something else by must not put this device to work.
    let rows = [BackupDestination {
        destination_id: "future".into(),
        kind: "s3".into(),
        custodian_device_id: Some("dev-a".into()),
        capacity_cap_bytes: Some(1 << 30),
        ..Default::default()
    }];
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a"),
        None
    );
}

#[test]
fn both_carriers_project_into_the_same_rule() {
    // The property that keeps the two hosts from drifting: the same registry
    // contents produce the same assignment whichever carrier they arrive in.
    // (The wire row's own projection is pinned in `fauna-protocol`; this asserts
    // the *rule* is shared, by feeding it hand-built refs.)
    let at_rest = [custodian_row("mine", "dev-a", Some(7))];
    let from_registry = vec![CustodianRowRef {
        destination_id: "mine",
        kind: "client-device",
        custodian_device_id: Some("dev-a"),
        capacity_cap_bytes: Some(7),
    }];
    assert_eq!(
        custodian_assignment_for(at_rest.iter().map(|d| d.custodian_row()), "dev-a"),
        custodian_assignment_for(from_registry, "dev-a"),
    );
}

// ── The sole-client-destination predicate ───────────────────────────────────
//
// `backups.md` § Third destination kind → *Durability + labeling*: the page
// shows a standing `backup-sole-client-destination-warning` while **every**
// configured destination is a client device. tui wrote this predicate privately
// when it led the surface and recorded it as a shared-Rust candidate for the
// second app to render the element (priority #2); linux is that app, so the rule
// lives here now and both consume it.

use fauna_core::data::every_destination_is_a_client_device;

fn nest_row(destination_id: &str) -> BackupDestination {
    BackupDestination {
        destination_id: destination_id.into(),
        destination_nest_url: "https://nest.example".into(),
        ..Default::default()
    }
}

#[test]
fn the_sole_client_warning_needs_every_row_to_be_a_client_device() {
    let mine = custodian_row("mine", "dev-a", Some(1 << 30));
    let also_mine = custodian_row("laptop", "dev-b", None);
    let offsite = nest_row("friend");

    assert!(every_destination_is_a_client_device(std::slice::from_ref(
        &mine
    )));
    assert!(every_destination_is_a_client_device(&[
        mine.clone(),
        also_mine
    ]));
    // One off-site copy is exactly what the warning says the user lacks.
    assert!(!every_destination_is_a_client_device(&[
        mine.clone(),
        offsite.clone()
    ]));
    assert!(!every_destination_is_a_client_device(&[offsite]));
}

#[test]
fn an_empty_list_is_the_empty_state_not_a_sole_client_one() {
    // Zero destinations has its own copy on every page; claiming "all your
    // destinations are your own devices" about no destinations is false and
    // would paint the warning on a fresh account.
    assert!(!every_destination_is_a_client_device(&[]));
}

#[test]
fn an_inert_row_counts_as_not_a_client_device() {
    // The conservative direction, and the load-bearing one: an unrecognised kind
    // may well BE the off-site destination the warning would otherwise tell the
    // user they do not have. Crying wolf at someone who is covered is how a
    // standing warning gets tuned out.
    let unknown_kind = BackupDestination {
        destination_id: "future".into(),
        kind: "s3".into(),
        ..Default::default()
    };
    // A `client-device` row with no device id is Inert too — it is a row nothing
    // can drive, so it is not evidence that a copy lives on this device either.
    let device_id_less = BackupDestination {
        destination_id: "half-written".into(),
        kind: "client-device".into(),
        custodian_device_id: None,
        ..Default::default()
    };
    let mine = custodian_row("mine", "dev-a", None);

    assert!(!every_destination_is_a_client_device(&[unknown_kind]));
    assert!(!every_destination_is_a_client_device(&[
        mine,
        device_id_less
    ]));
}

// ── `distinct_destinations` — folding a destination's coverage rows away ────
//
// Finding: `attach_backup_destination_folder` clones the enrolled
// row per attached folder, so a destination with N covered folders has N+1
// rows sharing one `destination_id`. A reader that counts *destinations*
// rather than *rows* must fold on `destination_id` first
// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).

use fauna_core::data::distinct_destinations;

fn coverage_row(destination_id: &str, folder_set: &str, added_at: u64) -> BackupDestination {
    BackupDestination {
        folder_name: folder_set.into(),
        added_at,
        ..nest_row(destination_id)
    }
}

#[test]
fn distinct_destinations_folds_coverage_rows_into_the_enrollment_row() {
    let enrolled = nest_row("mine");
    let covered_a = coverage_row("mine", "__folder/deadbeef/1", 100);
    let covered_b = coverage_row("mine", "__folder/deadbeef/2", 200);

    let distinct = distinct_destinations(&[enrolled.clone(), covered_a, covered_b]);

    // Exactly one row for the one enrolled destination — its own enrollment
    // row, not one of the two coverage clones.
    assert_eq!(distinct, vec![enrolled]);
}

#[test]
fn distinct_destinations_keeps_one_row_per_distinct_destination_in_order() {
    let a = nest_row("a");
    let b = nest_row("b");
    let a_covered = coverage_row("a", "__folder/deadbeef/1", 100);

    let distinct = distinct_destinations(&[a.clone(), a_covered, b.clone()]);

    assert_eq!(distinct, vec![a, b]);
}

#[test]
fn distinct_destinations_of_an_empty_list_is_empty() {
    assert_eq!(distinct_destinations(&[]), Vec::new());
}

#[test]
fn row_predicate_agrees_with_kind_view() {
    // `every_destination_is_a_client_device` reads the at-rest row through
    // `kind_view()`; `every_row_is_a_client_device` reads a *carrier's* row
    // through `row_is_a_client_device`, because the at-rest struct deliberately
    // does not cross FFI (the native projection and the registry wire row carry
    // only `kind` + `custodian_device_id`). Two expressions of one rule, so this
    // pins them to agree — remove either arm of `row_is_a_client_device` and
    // exactly this test fails.
    let rows = [
        nest_row("offsite"),
        custodian_row("mine", "dev-a", Some(1 << 30)),
        custodian_row("uncapped", "dev-b", None),
        BackupDestination {
            destination_id: "future".into(),
            kind: "s3".into(),
            ..Default::default()
        },
        BackupDestination {
            destination_id: "half-written".into(),
            kind: "client-device".into(),
            custodian_device_id: None,
            ..Default::default()
        },
    ];
    for row in &rows {
        assert_eq!(
            fauna_core::data::row_is_a_client_device(row.custodian_row()),
            matches!(row.kind_view(), DestinationKind::ClientDevice { .. }),
            "the carrier-level rule and kind_view() disagree about {:?}",
            row.destination_id
        );
    }
}

#[test]
fn the_carrier_level_predicate_is_the_same_answer_as_the_at_rest_one() {
    // What the FFI and wasm faces call. Same three properties as the at-rest
    // predicate above, reached through the borrowed carrier shape.
    use fauna_core::data::every_row_is_a_client_device;

    let mine = custodian_row("mine", "dev-a", None);
    let offsite = nest_row("friend");

    assert!(every_row_is_a_client_device([mine.custodian_row()]));
    assert!(!every_row_is_a_client_device([
        mine.custodian_row(),
        offsite.custodian_row()
    ]));
    // The empty arm has to survive the iterator rewrite: an `all()` over an
    // empty iterator is `true`, which would paint the warning on a fresh
    // account with no backup at all.
    assert!(!every_row_is_a_client_device([]));
}

#[test]
fn the_predicate_survives_the_json_round_trip_the_wasm_face_performs() {
    // `everyDestinationIsAClientDevice` takes the array `backupDestinationList` returned,
    // which means the rows make a Rust → JS → Rust round trip through
    // `serde_wasm_bindgen`'s json-compatible shape before the rule ever sees
    // them. A field that does not survive that trip would flip the answer
    // silently, and the harmful direction is silent: the user simply never sees
    // a warning about durability they do not have.
    let rows = vec![
        custodian_row("mine", "dev-a", Some(50 << 30)),
        custodian_row("uncapped", "dev-b", None),
    ];
    let json = serde_json::to_string(&rows).expect("rows serialize");
    let back: Vec<BackupDestination> = serde_json::from_str(&json).expect("rows deserialize");
    assert_eq!(back, rows);
    assert!(every_destination_is_a_client_device(&back));

    let mixed = vec![custodian_row("mine", "dev-a", None), nest_row("friend")];
    let json = serde_json::to_string(&mixed).expect("rows serialize");
    let back: Vec<BackupDestination> = serde_json::from_str(&json).expect("rows deserialize");
    assert!(!every_destination_is_a_client_device(&back));
}

// ── The kind-select option catalog ──────────────────────────────────────────

#[test]
fn the_kind_select_offers_the_two_implemented_kinds_nest_first() {
    let options = fauna_core::format::backup_destination_kind_options();
    let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
    // Nest first: it is the kind that actually satisfies "off-site", so it is
    // the default a user lands on. S3 is ratified but deferred to its own design
    // pass, so it is ABSENT rather than present-and-disabled — an option that
    // cannot be chosen teaches the user nothing.
    assert_eq!(values, vec!["nest", "client-device"]);
}

#[test]
fn an_option_label_is_the_same_text_as_the_badge_it_produces() {
    // The property the catalog exists for: the option a user picks and the badge
    // they get back on the resulting row cannot drift apart, on any of 7 apps.
    for option in fauna_core::format::backup_destination_kind_options() {
        assert_eq!(
            option.label,
            fauna_core::format::backup_destination_kind_label(&option.value),
            "option {:?} must paint the badge text its own row will carry",
            option.value
        );
    }
}

// ── The orphaned-store predicate ────────────────────────────────────────────
//
// `docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
// device's copy*: `backup-orphaned-store-row` renders "whenever this device
// holds a sealed store with **no** matching destination row", and carries the
// reclaim gesture. Removing a client-device destination deliberately KEEPS the
// local store (3c-ii — it is the owner's only offline copy), which is what
// makes an orphaned store a normal state rather than a corruption, and what
// makes this predicate the guard on a destructive button.

use fauna_core::data::{a_destination_row_claims_this_device, custodian_store_is_orphaned};

#[test]
fn a_store_with_no_row_naming_this_device_is_orphaned() {
    // The ordinary path: the owner removed their client-device destination
    // without ticking the reclaim opt-in, so the row is gone and the bytes are
    // still on this disk with nothing driving them.
    let rows = [
        BackupDestination::default(),
        custodian_row("someone-elses-laptop", "dev-b", None),
    ];
    assert!(custodian_store_is_orphaned(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
        true,
    ));
}

#[test]
fn a_store_this_device_is_still_enrolled_for_is_not_orphaned() {
    let rows = [custodian_row("mine", "dev-a", Some(64 << 30))];
    assert!(!custodian_store_is_orphaned(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
        true,
    ));
}

#[test]
fn two_rows_naming_this_device_still_claim_it() {
    // ⚠ The rule this file exists to pin, and the one a re-derivation gets
    // wrong: `custodian_assignment_for` answers `None` here — it refuses to
    // guess which cap to honour — and reading that refusal as "no row claims
    // this device" would offer to delete the store while both rows sit on the
    // user's own Backups page, one config repair away from being driven again.
    let rows = [
        custodian_row("mine", "dev-a", Some(1 << 30)),
        custodian_row("mine-again", "dev-a", Some(2 << 30)),
    ];
    assert_eq!(
        custodian_assignment_for(rows.iter().map(|d| d.custodian_row()), "dev-a"),
        None,
        "precondition: the host refuses an ambiguous assignment",
    );
    assert!(a_destination_row_claims_this_device(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
    ));
    assert!(!custodian_store_is_orphaned(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
        true,
    ));
}

#[test]
fn a_device_with_no_id_of_its_own_is_never_orphaned() {
    // "Cannot tell whether a row names me" must not paint a delete button over
    // the owner's only offline copy.
    let rows = [custodian_row("mine", "dev-a", None)];
    assert!(!custodian_store_is_orphaned(
        rows.iter().map(|d| d.custodian_row()),
        "   ",
        true,
    ));
}

#[test]
fn an_empty_store_offers_no_reclaim() {
    // Reclaim frees disk space; with nothing held there is nothing to free, and
    // a row that rendered anyway would invite a no-op on every device that never
    // enrolled as a custodian.
    let rows: [BackupDestination; 0] = [];
    assert!(!custodian_store_is_orphaned(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
        false,
    ));
}

#[test]
fn a_nest_row_carrying_this_devices_id_does_not_claim_the_store() {
    // The kind is half the rule. A nest destination that somehow carries a
    // device id is not custody of this device's store, and treating it as one
    // would strand the bytes unreclaimable.
    let rows = [BackupDestination {
        destination_id: "peer-nest".into(),
        destination_nest_url: "https://nest.example".into(),
        custodian_device_id: Some("dev-a".into()),
        ..Default::default()
    }];
    assert!(!a_destination_row_claims_this_device(
        rows.iter().map(|d| d.custodian_row()),
        "dev-a",
    ));
}
