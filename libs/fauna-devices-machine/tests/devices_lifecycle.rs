//! Lifecycle tests for `DevicesMachine`. Mirrors
//! `fauna-folders-machine/tests/wizard_lifecycle.rs`: read snapshots, gesture
//! wire-shape, and error handling via the fake nest seam + fake wizard factory.

use std::sync::{Arc, Mutex};

use fauna_account_port::PortFault;
use fauna_account_port::loopback::Loopback;
use fauna_devices_machine::nest_api::{FakeCall, FakeDevicesNestApi, FakeWizardFactory};
use fauna_devices_machine::observer::{CountingObserver, NullObserver};
use fauna_devices_machine::port::{self, PortFleetRemoval};
use fauna_devices_machine::{
    DevicesApiError, DevicesMachine, DevicesNestApi, DevicesObserver, FleetMembersView,
    FleetRemoval, FleetRemovalRefusal, FolderWizardObserver, FollowedFolderSummary,
    FollowedFoldersSource, ForeignSetRow, ForeignSetsSource, MlsQuery, NestDeletion,
    P2pParticipation, UnaccountedMember, WizardFactory,
};
use fauna_folders_machine::FolderWizardStep;
use fauna_protocol::folders::{
    ConflictCandidate, FolderSummary as WireFolderSummary, SyncConflict,
};
use fauna_protocol::sync::{DeviceFolderRole as WireDeviceFolderRole, SyncDevice};

/// A **wire** device row, matching the seam: the machine renders the sealed
/// label at ingest before transcribing to `DeviceSummary` (path-sealing S6-b).
/// `label_sealed: None` is the machine-authored (or keyless-writer) shape,
/// which renders straight through as plaintext.
fn device(id: &str, label: &str) -> SyncDevice {
    SyncDevice {
        device_id: id.repeat(32),
        label: label.into(),
        label_sealed: None,
        capabilities: "read,write".into(),
        registered_at: 1,
        last_seen_at: 2,
        online: true,
        guardian_marked: false,
        principal: None,
        p2p_participation: None,
        p2p_off_requested: false,
        folders: vec![WireDeviceFolderRole {
            name: "docs".into(),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        }],
        extra: Default::default(),
    }
}

/// A **wire** folder row — `DevicesNestApi::list_folders` hands the machine
/// wire rows so it can render the sealed selective-sync pair before transcribing
/// (`DevicesMachine::render_folders`). Struct-update over `Default` so a field
/// added to the wire type does not collide here (the repo's fixture convention).
fn folder(name: &str) -> WireFolderSummary {
    WireFolderSummary {
        id: 1,
        name: name.into(),
        role: Some("owner".into()),
        conflict_policy: Some("auto".into()),
        ..Default::default()
    }
}

/// A B3 member-visible (shared-*with*-me) row: `role == "member"` + a raw hex
/// `mls_group_id`. The join-filter must drop it unless the client has joined `gid`.
fn member_set(name: &str, gid: &str) -> WireFolderSummary {
    WireFolderSummary {
        role: Some("member".into()),
        mls_group_id: Some(gid.into()),
        // `owner_display` is not a wire field — the transcribe computes it from
        // `owner_handle` + `owner_actor_id` (`account_display_label`).
        owner_handle: Some("alice@nest".into()),
        ..folder(name)
    }
}

/// A fake [`MlsQuery`] that reports "joined" only for a fixed allow-set of raw hex
/// group ids (mirrors `MlsEngine::has_group` over the client's joined groups).
struct JoinedGroups(Vec<String>);
impl MlsQuery for JoinedGroups {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        self.0.iter().any(|g| g == mls_group_id_hex)
    }
}

/// The roster the page hands the member door: every row as `(row id, claimed principal)`.
type Roster = Vec<(String, Option<[u8; 32]>)>;

/// One resolution the fake door was asked for: the nest row the user picked,
/// the principal that row claimed, and how many nest calls had been made by
/// then (the ordering witness — resolution must precede the nest deletion).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolution {
    row: String,
    claimed: Option<[u8; 32]>,
    nest_calls_before: usize,
}

/// A fake [`FleetRemoval`] door — records every resolution and every removal
/// it was asked for and answers fixtured results, so a test can assert what
/// `remove_device` hands the door, in what order, and how it reacts.
///
/// Unfixtured, resolution answers the way the real door does for an honest
/// pre-binding fleet: the claimed principal, or nothing when the row has none.
#[derive(Default)]
struct FakeFleetRemoval {
    nest: Mutex<Option<Arc<FakeDevicesNestApi>>>,
    resolutions: Mutex<Vec<Resolution>>,
    resolve_response: Mutex<Option<Result<Vec<[u8; 32]>, FleetRemovalRefusal>>>,
    /// Every stage and settle, in order — the completion rule's witness.
    events: Mutex<Vec<DoorEvent>>,
    stage_response: Mutex<Option<Result<(), String>>>,
    response: Mutex<Option<Result<(), String>>>,
    /// The member door's read: what `fleet_members` answers (`None` = an
    /// empty view under `me = 0x0a…`), and every roster it was asked over.
    members_response: Mutex<Option<Result<FleetMembersView, String>>>,
    member_reads: Mutex<Vec<Roster>>,
    /// The member door's leg: every id `remove_member` was asked for, and
    /// what it answers (`None` = `Ok`).
    member_removals: Mutex<Vec<[u8; 32]>>,
    member_response: Mutex<Option<Result<(), FleetRemovalRefusal>>>,
}

/// One call on the door's completion half. `Staged` records how many nest
/// calls had been made by then: the intent must be durable BEFORE the nest
/// deletion, the transition's single decision point.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DoorEvent {
    Staged {
        row: String,
        targets: Vec<[u8; 32]>,
        nest_calls_before: usize,
    },
    Settled {
        row: String,
        targets: Vec<[u8; 32]>,
        outcome: NestDeletion,
    },
}

impl FakeFleetRemoval {
    fn watching(nest: &Arc<FakeDevicesNestApi>) -> Arc<Self> {
        let door = Self::default();
        *door.nest.lock().unwrap() = Some(Arc::clone(nest));
        Arc::new(door)
    }
    fn set_resolution(&self, r: Result<Vec<[u8; 32]>, FleetRemovalRefusal>) {
        *self.resolve_response.lock().unwrap() = Some(r);
    }
    fn set_response(&self, r: Result<(), String>) {
        *self.response.lock().unwrap() = Some(r);
    }
    fn resolutions(&self) -> Vec<Resolution> {
        self.resolutions.lock().unwrap().clone()
    }
    fn set_stage_response(&self, r: Result<(), String>) {
        *self.stage_response.lock().unwrap() = Some(r);
    }
    fn set_members(&self, r: Result<FleetMembersView, String>) {
        *self.members_response.lock().unwrap() = Some(r);
    }
    fn member_reads(&self) -> Vec<Roster> {
        self.member_reads.lock().unwrap().clone()
    }
    fn set_member_response(&self, r: Result<(), FleetRemovalRefusal>) {
        *self.member_response.lock().unwrap() = Some(r);
    }
    fn member_removals(&self) -> Vec<[u8; 32]> {
        self.member_removals.lock().unwrap().clone()
    }
    fn events(&self) -> Vec<DoorEvent> {
        self.events.lock().unwrap().clone()
    }
    /// The fleet ids the page asked to have `Removed` — those settled `Gone`.
    fn calls(&self) -> Vec<[u8; 32]> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                DoorEvent::Settled {
                    targets,
                    outcome: NestDeletion::Gone,
                    ..
                } => Some(targets),
                _ => None,
            })
            .flatten()
            .collect()
    }
    fn nest_calls(&self) -> usize {
        self.nest
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |n| n.calls().len())
    }
}

#[async_trait::async_trait]
impl FleetRemoval for FakeFleetRemoval {
    async fn resolve_removal(
        &self,
        row_device_id: &str,
        claimed_principal: Option<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
        let nest_calls_before = self
            .nest
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |n| n.calls().len());
        self.resolutions.lock().unwrap().push(Resolution {
            row: row_device_id.to_string(),
            claimed: claimed_principal,
            nest_calls_before,
        });
        self.resolve_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(claimed_principal.into_iter().collect()))
    }

    async fn stage_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
    ) -> Result<(), String> {
        let nest_calls_before = self.nest_calls();
        self.events.lock().unwrap().push(DoorEvent::Staged {
            row: row_device_id.to_string(),
            targets,
            nest_calls_before,
        });
        self.stage_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn settle_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
        outcome: NestDeletion,
    ) -> Result<(), String> {
        self.events.lock().unwrap().push(DoorEvent::Settled {
            row: row_device_id.to_string(),
            targets,
            outcome,
        });
        self.response.lock().unwrap().clone().unwrap_or(Ok(()))
    }

    async fn fleet_members(
        &self,
        roster: Vec<(String, Option<[u8; 32]>)>,
    ) -> Result<FleetMembersView, String> {
        self.member_reads.lock().unwrap().push(roster);
        self.members_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Ok(FleetMembersView {
                    me: [0x0a; 32],
                    unaccounted: Vec::new(),
                })
            })
    }

    async fn remove_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal> {
        self.member_removals.lock().unwrap().push(member);
        self.member_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }
}

/// A member-door view: `me` plus the listed members with their instants.
fn members_view(me: [u8; 32], listed: &[([u8; 32], i64)]) -> FleetMembersView {
    FleetMembersView {
        me,
        unaccounted: listed
            .iter()
            .map(|(device_id, enrolled_at_ms)| UnaccountedMember {
                device_id: *device_id,
                enrolled_at_ms: *enrolled_at_ms,
            })
            .collect(),
    }
}

/// A **wire** candidate — the seam's rows are wire rows now.
fn candidate(hash: &str, created_at: i64) -> ConflictCandidate {
    ConflictCandidate {
        manifest_hash: hash.into(),
        device_id: "aa".repeat(32),
        size_bytes: 10,
        created_at,
        content_key_version: None,
        ..Default::default()
    }
}

/// A **wire** conflict row — what the seam now hands the machine, so the
/// fixtures drive the same sealed-path render production does. `has_other_version`
/// / `file_info` are no longer fixture inputs: they are derived at transcribe,
/// from the *rendered* path.
fn conflict(id: i64) -> SyncConflict {
    SyncConflict {
        id,
        folder: "docs".into(),
        device_id: "aa".repeat(32),
        path: "/a.txt".into(),
        conflict_type: "content".into(),
        details: None,
        created_at: 9,
        candidates: vec![candidate("h1", 9)],
        resolved_at: None,
        resolution: None,
        winning_manifest_hash: None,
        ..Default::default()
    }
}

/// An auto-resolved review-list row: winner `winning`, both parents retained
/// as candidates (`h1` @ t9, `h2` @ t11).
fn resolved_conflict(id: i64, resolution: &str, winning: &str) -> SyncConflict {
    SyncConflict {
        candidates: vec![candidate("h1", 9), candidate("h2", 11)],
        resolved_at: Some(20),
        resolution: Some(resolution.into()),
        winning_manifest_hash: Some(winning.into()),
        ..conflict(id)
    }
}

/// Build a machine over fresh fakes; returns the machine + both fakes so the
/// test can fixture responses and assert recorded calls.
fn setup() -> (
    Arc<DevicesMachine>,
    Arc<FakeDevicesNestApi>,
    Arc<FakeWizardFactory>,
) {
    let nest = Arc::new(FakeDevicesNestApi::new());
    let factory = Arc::new(FakeWizardFactory::new());
    let obs: Arc<dyn DevicesObserver> = Arc::new(NullObserver);
    let api: Arc<dyn DevicesNestApi> = Arc::clone(&nest) as _;
    let wf: Arc<dyn WizardFactory> = Arc::clone(&factory) as _;
    let m = DevicesMachine::new(obs, api, wf);
    (m, nest, factory)
}

// ── Construction + refresh ──────────────────────────────────────────────────

#[tokio::test]
async fn new_machine_has_empty_snapshot() {
    let (m, _, _) = setup();
    let snap = m.snapshot();
    assert!(snap.devices.is_empty());
    assert!(snap.folders.is_empty());
    assert!(snap.conflicts.is_empty());
    assert!(snap.wizard.is_none());
    assert!(snap.error.is_none());
}

#[tokio::test]
async fn refresh_populates_all_three_lists() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);
    nest.set_folders(vec![folder("docs")]);
    nest.set_conflicts(vec![conflict(5)]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.devices.len(), 1);
    assert_eq!(snap.devices[0].label, "laptop");
    assert!(snap.devices[0].folders[0].applies_deletes);
    assert_eq!(snap.folders.len(), 1);
    assert_eq!(snap.folders[0].name, "docs");
    assert_eq!(snap.conflicts.len(), 1);
    assert_eq!(snap.conflicts[0].candidates[0].manifest_hash, "h1");
    assert!(snap.error.is_none());
}

/// Ruling (7)(b)(ii) rule (2): the toggle the snapshot carries is the owner's
/// custody's served state, never the nest's `webdav_enabled` — a set the nest
/// flags served that custody does not serve paints OFF, and the reverse ON.
#[tokio::test]
async fn the_webdav_toggle_reads_custody_never_the_nest_flag() {
    let (m, nest, _) = setup();
    let mut flagged = folder("flagged");
    flagged.webdav_enabled = true;
    nest.set_folders(vec![flagged, folder("served")]);
    nest.set_custody_served(&["served"]);

    m.refresh().await;

    let snap = m.snapshot();
    let served: Vec<(&str, bool)> = snap
        .folders
        .iter()
        .map(|f| (f.name.as_str(), f.webdav_enabled))
        .collect();
    assert_eq!(served, vec![("flagged", false), ("served", true)]);
}

// ── B3 member-row join-filter ───────────────────────────

/// Fail-safe: with NO `MlsQuery` wired, every `role == "member"` row is dropped —
/// a rostered-but-un-joined knock never reaches the snapshot (owner rows still show).
#[tokio::test]
async fn member_rows_dropped_when_no_mls_query_wired() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![
        folder("my-docs"),            // owner row → always shows
        member_set("shared-a", "aa"), // member rows → dropped (no query)
        member_set("shared-b", "bb"),
    ]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.folders.len(),
        1,
        "only the owner row survives (fail-safe)"
    );
    assert_eq!(snap.folders[0].name, "my-docs");
}

/// With an `MlsQuery` wired, a `role == "member"` row shows **only** for a group the
/// client has actually joined; a rostered-but-un-joined knock is still dropped.
#[tokio::test]
async fn member_rows_filtered_by_join_state() {
    let (m, nest, _) = setup();
    m.set_mls_query(Arc::new(JoinedGroups(vec!["aa".into()]))); // joined "aa" only
    nest.set_folders(vec![
        folder("my-docs"),            // owner → shows
        member_set("shared-a", "aa"), // joined → shows
        member_set("shared-b", "bb"), // rostered-but-un-joined knock → dropped
    ]);

    m.refresh().await;

    let snap = m.snapshot();
    let names: Vec<&str> = snap.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["my-docs", "shared-a"],
        "owner + joined-member rows show; the un-joined knock is filtered out"
    );
    assert!(snap.error.is_none());
}

/// A fake [`ForeignSetsSource`] returning a fixed record list (mirrors the
/// production `CustodyForeignSetsSource` over the member's folder-keys custody).
struct FixedForeign(Vec<ForeignSetRow>);
#[async_trait::async_trait]
impl ForeignSetsSource for FixedForeign {
    async fn foreign_sets(&self) -> Vec<ForeignSetRow> {
        self.0.clone()
    }
}

/// Foreign (cross-nest) records union into the list as `role == "member"` rows
/// carrying their `home_nest_url`, under the SAME join-filter (an un-joined —
/// e.g. just-left — record never surfaces), and never render without a wired
/// `MlsQuery` (fail-safe parity with same-nest member rows).
#[tokio::test]
async fn refresh_unions_joined_foreign_sets() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![folder("my-docs")]);
    let foreign_rows = vec![
        ForeignSetRow {
            set_name: Some("their-photos".into()),
            mls_group_id_hex: "cc".into(),
            home_nest_url: "https://home.example".into(),
            access: None,
            metadata_only_residency: None,
        },
        ForeignSetRow {
            set_name: Some("left-set".into()),
            mls_group_id_hex: "dd".into(),
            home_nest_url: "https://other.example".into(),
            access: None,
            metadata_only_residency: None,
        },
    ];
    m.set_foreign_sets_source(Arc::new(FixedForeign(foreign_rows)));

    // No MlsQuery wired yet → fail-safe: no foreign rows.
    m.refresh().await;
    assert_eq!(m.snapshot().folders.len(), 1, "fail-safe without MlsQuery");

    // Joined "cc" only → the joined foreign set unions in; "dd" stays dropped.
    m.set_mls_query(Arc::new(JoinedGroups(vec!["cc".into()])));
    m.refresh().await;
    let snap = m.snapshot();
    let names: Vec<&str> = snap.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["my-docs", "their-photos"]);
    let foreign = &snap.folders[1];
    assert_eq!(foreign.role.as_deref(), Some("member"));
    assert_eq!(foreign.id, -1, "no nest row id for a foreign set");
    assert_eq!(
        foreign.home_nest_url.as_deref(),
        Some("https://home.example")
    );
    assert_eq!(foreign.mls_group_id.as_deref(), Some("cc"));
    assert!(snap.error.is_none());
}

#[tokio::test]
async fn refresh_failure_keeps_prior_data_and_sets_error() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);
    m.refresh().await; // good data
    assert_eq!(m.snapshot().devices.len(), 1);

    nest.fail_lists(DevicesApiError::Transient {
        detail: "offline".into(),
    });
    m.refresh().await;

    let snap = m.snapshot();
    // Prior data retained.
    assert_eq!(snap.devices.len(), 1);
    let err = snap.error.expect("error set");
    assert_eq!(err.key, "devices.error_refresh");
    assert_eq!(err.args.get("message").map(String::as_str), Some("offline"));
}

// ── The refresh barrier (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`) ───────────

#[tokio::test]
async fn refresh_counts_claim_a_generation_and_commit_it_errors_included() {
    let (m, nest, _) = setup();
    assert_eq!(
        m.refresh_counts(),
        (0, 0, 0),
        "a fresh machine has run nothing"
    );

    m.refresh().await;
    assert_eq!(m.refresh_counts(), (1, 1, 1));

    // A refresh that could read nothing still lands its verdict — the barrier
    // proves the page re-read, never that the read succeeded.
    nest.fail_lists(DevicesApiError::Transient {
        detail: "offline".into(),
    });
    let baseline = m.refresh_counts().0;
    m.refresh().await;
    let (started, completed, committed_gen) = m.refresh_counts();
    assert_eq!((started, completed), (2, 2));
    assert!(
        committed_gen > baseline,
        "a refresh that began after the baseline has committed: {committed_gen} > {baseline}"
    );
}

/// The counts are bumped BEFORE the observer fires, so an app that republishes
/// its automation state on `on_changed` publishes the committed generation with
/// the snapshot it belongs to — never a snapshot one generation ahead of its
/// barrier.
#[tokio::test]
async fn refresh_commit_is_counted_before_the_observer_hears_of_it() {
    struct Peek(
        Mutex<Option<Arc<DevicesMachine>>>,
        Mutex<Vec<(u64, u64, u64)>>,
    );
    impl DevicesObserver for Peek {
        fn on_changed(&self) {
            if let Some(m) = self.0.lock().unwrap().as_ref() {
                self.1.lock().unwrap().push(m.refresh_counts());
            }
        }
    }
    let peek = Arc::new(Peek(Mutex::new(None), Mutex::new(Vec::new())));
    let nest: Arc<dyn DevicesNestApi> = Arc::new(FakeDevicesNestApi::new());
    let wf: Arc<dyn WizardFactory> = Arc::new(FakeWizardFactory::new());
    let m = DevicesMachine::new(Arc::clone(&peek) as _, nest, wf);
    *peek.0.lock().unwrap() = Some(Arc::clone(&m));

    m.refresh().await;
    assert_eq!(peek.1.lock().unwrap().last(), Some(&(1, 1, 1)));
}

#[test]
fn devices_refreshes_json_is_the_shared_triple_shape() {
    assert_eq!(
        fauna_devices_machine::devices_refreshes_json(Some((3, 2, 3))),
        serde_json::json!({"started": 3, "completed": 2, "committed_gen": 3})
    );
    // No machine yet (pre-auth) is the legitimate zero, not an absent leg.
    assert_eq!(
        fauna_devices_machine::devices_refreshes_json(None),
        serde_json::json!({"started": 0, "completed": 0, "committed_gen": 0})
    );
}

// ── Wizard open / close / forward ────────────────────────────────────────────

#[tokio::test]
async fn open_wizard_seeds_devices_and_surfaces_snapshot() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop"), device("bb", "phone")]);
    m.refresh().await;

    assert!(m.wizard().is_none());
    m.open_wizard();

    // The page snapshot now carries the wizard, seeded with the 2 devices.
    let snap = m.snapshot();
    let wiz = snap.wizard.expect("wizard open");
    assert_eq!(wiz.device_places.devices.len(), 2);
    assert_eq!(wiz.device_places.devices[0].label, "laptop");

    // The wizard handle is drivable; its tick re-renders the page snapshot.
    let machine = m.wizard().expect("wizard handle");
    machine.set_name("photos".into());
    assert_eq!(m.snapshot().wizard.unwrap().name.name, "photos".to_string());

    m.close_wizard();
    assert!(m.wizard().is_none());
    assert!(m.snapshot().wizard.is_none());
}

#[tokio::test]
async fn wizard_submit_rides_the_factory_fake() {
    let (m, nest, factory) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);
    m.refresh().await;
    m.open_wizard();

    let wiz = m.wizard().unwrap();
    wiz.set_name("docs".into());
    wiz.toggle_device_member(0);
    wiz.next();
    wiz.next();
    wiz.next();
    assert_eq!(wiz.step(), FolderWizardStep::Review);
    let step = wiz.submit().await;
    assert_eq!(step, FolderWizardStep::Done);

    // The wizard's create + member-add went through the factory's fake seam.
    let calls = factory.wizard_fake.calls();
    assert_eq!(calls.len(), 2); // 1 create + 1 member
}

// ── Page write gestures ──────────────────────────────────────────────────────

#[tokio::test]
async fn remove_device_by_index_calls_delete_then_refreshes() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop"), device("bb", "phone")]);
    m.refresh().await;

    m.remove_device(1).await; // phone
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RemoveDevice {
            device_id: "bb".repeat(32),
        }]
    );

    // Out-of-range is ignored (no further call).
    m.remove_device(99).await;
    assert_eq!(nest.calls().len(), 1);
}

/// The fleet-scope removal leg (`devices.md` § Removing a Device): the target
/// is whatever the door RESOLVES from client-held truth for the chosen row —
/// never the principal the nest put on that row. Here the nest pairs the
/// laptop's row with `0x11…` and the fleet's own statements name `0x33…`; the
/// `Removed` row goes to `0x33…`. Resolution runs BEFORE the nest deletion, so
/// a refusal can still leave everything in place.
#[tokio::test]
async fn remove_device_removes_the_resolved_fleet_member_not_the_rows_principal() {
    let (m, nest, _) = setup();
    let mut d = device("aa", "laptop");
    d.principal = Some("11".repeat(32));
    nest.set_devices(vec![d]);
    m.refresh().await;

    let door = FakeFleetRemoval::watching(&nest);
    door.set_resolution(Ok(vec![[0x33; 32]]));
    m.set_fleet_removal(door.clone());

    m.remove_device(0).await;

    assert_eq!(
        door.resolutions(),
        vec![Resolution {
            row: "aa".repeat(32),
            claimed: Some([0x11; 32]),
            nest_calls_before: 0,
        }],
        "the chosen row + the nest's claim, asked before anything is deleted"
    );
    assert_eq!(door.calls(), vec![[0x33; 32]], "the resolved member");
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RemoveDevice {
            device_id: "aa".repeat(32)
        }]
    );
    assert!(m.snapshot().error.is_none());
}

/// A refused resolution deletes NOTHING and tells the user so — the spared
/// arm's pin: the device the user meant is never shown as removed while it
/// stays a fleet member. One case per refusal the door can answer.
#[tokio::test]
async fn a_refused_resolution_deletes_nothing_and_says_the_device_was_not_removed() {
    for (refusal, key) in [
        (
            FleetRemovalRefusal::OwnDevice,
            "devices.error_remove_own_device",
        ),
        (
            FleetRemovalRefusal::NotAMember,
            "devices.error_remove_unverified_device",
        ),
        (
            // Its own copy: no retry clears it, so it must not say "try again".
            FleetRemovalRefusal::RowMismatch,
            "devices.error_remove_row_mismatch",
        ),
        (
            FleetRemovalRefusal::Unavailable("runtime gone".into()),
            "devices.error_remove_device",
        ),
    ] {
        let (m, nest, _) = setup();
        let mut d = device("aa", "laptop");
        d.principal = Some("11".repeat(32));
        nest.set_devices(vec![d]);
        m.refresh().await;
        let calls_before = nest.calls().len();

        let door = FakeFleetRemoval::watching(&nest);
        door.set_resolution(Err(refusal.clone()));
        m.set_fleet_removal(door.clone());

        m.remove_device(0).await;

        assert_eq!(
            nest.calls().len(),
            calls_before,
            "{refusal:?}: no nest deletion, no re-list"
        );
        assert!(door.calls().is_empty(), "{refusal:?}: no Removed row");
        assert_eq!(m.snapshot().devices.len(), 1, "{refusal:?}: the row stays");
        let err = m.snapshot().error.expect("the refusal surfaces");
        assert_eq!(err.key, key, "{refusal:?}");
    }
}

/// A principal that does not decode is nest-supplied garbage, not "no
/// principal": it is refused outright rather than read as a row that names
/// no fleet member (which would spare whichever device the row really is).
#[tokio::test]
async fn an_undecodable_principal_is_refused_not_skipped() {
    let (m, nest, _) = setup();
    let mut d = device("aa", "laptop");
    d.principal = Some("not-hex".into());
    nest.set_devices(vec![d]);
    m.refresh().await;
    let calls_before = nest.calls().len();

    let door = FakeFleetRemoval::watching(&nest);
    m.set_fleet_removal(door.clone());

    m.remove_device(0).await;

    assert_eq!(nest.calls().len(), calls_before);
    assert!(door.calls().is_empty());
    assert_eq!(
        m.snapshot().error.expect("refused").key,
        "devices.error_remove_unverified_device"
    );
}

/// A row with no principal still goes through resolution — a hostile nest can
/// strip the field, and a member that states this row is the target anyway —
/// but when the door resolves nothing, nothing is removed from the fleet.
#[tokio::test]
async fn a_row_with_no_principal_is_still_resolved_and_may_name_no_fleet_member() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]); // principal: None
    m.refresh().await;

    let door = FakeFleetRemoval::watching(&nest);
    m.set_fleet_removal(door.clone());

    m.remove_device(0).await;

    assert_eq!(
        door.resolutions(),
        vec![Resolution {
            row: "aa".repeat(32),
            claimed: None,
            nest_calls_before: 0,
        }]
    );
    assert!(door.calls().is_empty(), "nothing resolved — no fleet write");
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RemoveDevice {
            device_id: "aa".repeat(32)
        }]
    );
    assert!(m.snapshot().error.is_none());
}

/// How a test wires the fleet door: directly, as the native seats do, or
/// through web's account port — the forwarder, a loopback, the core chunk's
/// `serve` — so the removal-order pins hold for web's crossing too, the same
/// machine over the same trait (`account-client-lifecycle.md` § The
/// client-side lifecycle → *The account port*, decision (i)).
#[derive(Clone, Copy, Debug)]
enum Via {
    Direct,
    Port,
}

const VIAS: [Via; 2] = [Via::Direct, Via::Port];

fn wire(m: &DevicesMachine, door: &Arc<FakeFleetRemoval>, via: Via) {
    match via {
        Via::Direct => m.set_fleet_removal(door.clone()),
        Via::Port => {
            let seam: Arc<dyn FleetRemoval> = door.clone();
            m.set_fleet_removal(Arc::new(PortFleetRemoval::new(Loopback::new(
                move |d, payload| {
                    let seam = Arc::clone(&seam);
                    async move {
                        port::serve(&*seam, d, &payload)
                            .await
                            .unwrap_or_else(|| Err(PortFault::UnknownDoor(d.to_string())))
                    }
                },
            ))));
        }
    }
}

/// **The completion rule at the page** (clause (4)): the intent is staged
/// through the door BEFORE the nest deletion and settled on its outcome — so
/// a crash, a down runtime or a failed write between the legs leaves a staged
/// intent the runtime finishes, never a removed row with a device still in
/// the fleet. One case per outcome of the nest deletion.
#[tokio::test]
async fn remove_device_stages_before_the_nest_deletion_and_settles_on_its_outcome() {
    for via in VIAS {
        for (nest_answer, outcome) in [
            (Ok(()), NestDeletion::Gone),
            // The row is already gone — the deletion's own postcondition.
            (
                Err(DevicesApiError::NotFound {
                    detail: "no device".into(),
                }),
                NestDeletion::Gone,
            ),
            // The nest definitively kept it: nothing may be written.
            (
                Err(DevicesApiError::Conflict {
                    detail: "device is the sole source for folders".into(),
                }),
                NestDeletion::Kept,
            ),
            // Nobody knows: the intent stays staged and the runtime decides.
            (
                Err(DevicesApiError::Transient {
                    detail: "socket closed".into(),
                }),
                NestDeletion::Unknown,
            ),
        ] {
            let (m, nest, _) = setup();
            let mut d = device("aa", "laptop");
            d.principal = Some("11".repeat(32));
            nest.set_devices(vec![d]);
            m.refresh().await;
            let calls_before = nest.calls().len();
            nest.set_remove_device_response(nest_answer.clone());

            let door = FakeFleetRemoval::watching(&nest);
            wire(&m, &door, via);

            m.remove_device(0).await;

            assert_eq!(
                door.events(),
                vec![
                    DoorEvent::Staged {
                        row: "aa".repeat(32),
                        targets: vec![[0x11; 32]],
                        nest_calls_before: calls_before,
                    },
                    DoorEvent::Settled {
                        row: "aa".repeat(32),
                        targets: vec![[0x11; 32]],
                        outcome,
                    },
                ],
                "{via:?} {nest_answer:?}"
            );
            assert_eq!(
                m.snapshot().error.is_some(),
                nest_answer.is_err(),
                "{nest_answer:?}: the nest's own refusal still reaches the page"
            );
        }
    }
}

/// An intent that cannot be staged means a removal the app cannot promise to
/// finish: nothing is deleted, and the page says so.
#[tokio::test]
async fn a_removal_whose_intent_cannot_be_staged_deletes_nothing() {
    for via in VIAS {
        let (m, nest, _) = setup();
        let mut d = device("aa", "laptop");
        d.principal = Some("11".repeat(32));
        nest.set_devices(vec![d]);
        m.refresh().await;
        let calls_before = nest.calls().len();

        let door = FakeFleetRemoval::watching(&nest);
        door.set_stage_response(Err("the slot did not persist".into()));
        wire(&m, &door, via);

        m.remove_device(0).await;

        assert_eq!(
            nest.calls().len(),
            calls_before,
            "{via:?}: no nest deletion"
        );
        assert!(door.calls().is_empty());
        assert_eq!(
            m.snapshot().error.expect("surfaced").key,
            "devices.error_remove_device"
        );
    }
}

/// An unwired door (a machine built without its fleet door) is a plain no-op: the
/// nest deletion alone still succeeds and no error appears.
#[tokio::test]
async fn remove_device_with_no_door_wired_only_does_the_nest_deletion() {
    let (m, nest, _) = setup();
    let mut d = device("aa", "laptop");
    d.principal = Some("11".repeat(32));
    nest.set_devices(vec![d]);
    m.refresh().await;

    m.remove_device(0).await;

    assert_eq!(
        nest.calls(),
        vec![FakeCall::RemoveDevice {
            device_id: "aa".repeat(32)
        }]
    );
    assert!(m.snapshot().error.is_none());
}

/// **The fleet-removal leg is best-effort, but never silent (e2e convention
/// 11): a failure surfaces on `error-message` even though the nest deletion
/// already succeeded and `refresh()` already ran** — the exact leak the row
/// closes would otherwise reproduce with nothing telling the user their
/// removed device is still a wrap target.
#[tokio::test]
async fn remove_device_surfaces_a_fleet_removal_failure_without_undoing_the_nest_deletion() {
    let (m, nest, _) = setup();
    let mut d = device("aa", "laptop");
    d.principal = Some("22".repeat(32));
    nest.set_devices(vec![d]);
    m.refresh().await;

    let door = FakeFleetRemoval::watching(&nest);
    door.set_response(Err("plane unreachable".into()));
    m.set_fleet_removal(door.clone());

    m.remove_device(0).await;

    // The nest deletion went through regardless of the plane leg's fate.
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RemoveDevice {
            device_id: "aa".repeat(32)
        }]
    );
    assert_eq!(door.calls(), vec![[0x22; 32]]);
    // The plane failure still reaches the page error, surviving refresh's own
    // success-clears-error assignment (refresh itself succeeded — the fake's
    // list is untouched by `remove_device`, same as production's own re-list).
    let err = m
        .snapshot()
        .error
        .expect("the fleet-removal failure surfaces");
    assert_eq!(err.key, "devices.error_remove_fleet_device");
    assert_eq!(
        err.args.get("message").map(String::as_str),
        Some("plane unreachable")
    );
}

// ── The member-addressed door (`ui/devices.md` § Members without a matching
// entry) ────────────────────────────────────────────────────────────────────

/// **The refresh reads the member door over the roster it just listed, and
/// the snapshot carries the cards with their fingerprints and this device's
/// own** — every string through `fauna_core::format::fleet_fingerprint`, the
/// one formatter both surfaces share. The roster handed to the door is the
/// page's rows as `(row id, decoded principal)`, an undecodable principal
/// read as none.
#[tokio::test]
async fn refresh_lists_the_members_the_door_answers_with_their_fingerprints() {
    let (m, nest, _) = setup();
    let mut laptop = device("aa", "laptop");
    laptop.principal = Some("11".repeat(32));
    let mut phone = device("bb", "phone");
    phone.principal = Some("not-hex".to_string());
    nest.set_devices(vec![laptop, phone]);
    let door = FakeFleetRemoval::watching(&nest);
    door.set_members(Ok(members_view(
        [0x0a; 32],
        &[([0x22; 32], 1_700_000_000_000), ([0x33; 32], 0)],
    )));
    m.set_fleet_removal(door.clone());

    m.refresh().await;

    assert_eq!(
        door.member_reads(),
        vec![vec![
            ("aa".repeat(32), Some([0x11; 32])),
            ("bb".repeat(32), None),
        ]],
        "the door is asked over the roster the page lists"
    );
    let snap = m.snapshot();
    assert_eq!(snap.members.len(), 2);
    assert_eq!(snap.members[0].device_id, "22".repeat(32));
    assert_eq!(
        snap.members[0].fingerprint,
        fauna_core::format::fleet_fingerprint(&[0x22; 32])
    );
    assert_eq!(snap.members[0].enrolled_at_ms, 1_700_000_000_000);
    assert_eq!(snap.own_fleet_id.as_deref(), Some("0a".repeat(32).as_str()));
    assert_eq!(
        snap.own_fingerprint.as_deref(),
        Some(fauna_core::format::fleet_fingerprint(&[0x0a; 32]).as_str())
    );
    assert!(snap.error.is_none(), "the member read never fails the page");
}

/// **The two surfaces are one comparison**: a member card and the own row
/// rendered for the same id read the same — pinned here, at the one place
/// both strings are produced, so no later change to either path can make
/// "matches none of my devices" fire on a device the user holds.
#[tokio::test]
async fn the_member_card_and_the_own_row_render_one_fingerprint() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);
    let door = FakeFleetRemoval::watching(&nest);
    let id = [0x5c; 32];
    door.set_members(Ok(members_view(id, &[(id, 1)])));
    m.set_fleet_removal(door);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.members[0].fingerprint,
        snap.own_fingerprint.clone().expect("own fingerprint")
    );
}

/// No door (a machine built without it) lists nobody and shows no own
/// fingerprint; a door that cannot answer (the runtime not up yet) keeps
/// what was listed rather than blanking the group.
#[tokio::test]
async fn no_door_lists_no_members_and_a_failed_read_keeps_the_last_list() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);
    m.refresh().await;
    let snap = m.snapshot();
    assert!(snap.members.is_empty());
    assert!(snap.own_fleet_id.is_none() && snap.own_fingerprint.is_none());

    let door = FakeFleetRemoval::watching(&nest);
    door.set_members(Ok(members_view([0x0a; 32], &[([0x22; 32], 5)])));
    m.set_fleet_removal(door.clone());
    m.refresh().await;
    assert_eq!(m.snapshot().members.len(), 1);

    door.set_members(Err("the account runtime is not running".into()));
    m.refresh().await;
    let snap = m.snapshot();
    assert_eq!(snap.members.len(), 1, "kept, not blanked");
    assert!(snap.error.is_none(), "and never a page error");
}

/// **`remove_member_by_id` writes by key and touches no nest row**: the door's
/// member leg is asked for exactly the card's fleet id, the nest sees no
/// deletion, and the page refreshes so the card (now excluded) drops.
#[tokio::test]
async fn remove_member_removes_by_key_and_never_calls_the_nest() {
    for via in VIAS {
        let (m, nest, _) = setup();
        nest.set_devices(vec![device("aa", "laptop")]);
        let door = FakeFleetRemoval::watching(&nest);
        door.set_members(Ok(members_view(
            [0x0a; 32],
            &[([0x22; 32], 1), ([0x33; 32], 2)],
        )));
        wire(&m, &door, via);
        m.refresh().await;
        let nest_calls_before = nest.calls().len();

        let armed_id = m.snapshot().members[1].device_id.clone();

        door.set_members(Ok(members_view([0x0a; 32], &[([0x22; 32], 1)])));
        m.remove_member_by_id(armed_id.clone()).await;

        assert_eq!(door.member_removals(), vec![[0x33; 32]]);
        assert!(
            door.events().is_empty(),
            "nothing staged or settled — there is no nest deletion to bracket"
        );
        assert_eq!(
            nest.calls().len(),
            nest_calls_before,
            "no nest gesture at all — the fake records writes, and a devices.delete would be one"
        );
        let snap = m.snapshot();
        assert_eq!(snap.members.len(), 1, "the refresh re-read the door");
        assert!(snap.error.is_none());

        // An id no longer listed: ignored, nothing asked.
        m.remove_member_by_id(armed_id).await;
        assert_eq!(door.member_removals().len(), 1, "{via:?}");
    }
}

/// **The key-addressed confirm survives a reshaping refresh** (the probe of
/// the verify-back that minted this fix): the thief's card is armed while
/// the list is `[thief]`; a refresh then lists `[sibling, thief]`; the
/// confirm by the armed card's id asks the door for the thief alone — the
/// position it was armed at now names the sibling. An id no longer listed is
/// ignored and never falls through to a position.
#[tokio::test]
async fn remove_member_by_id_survives_a_refresh_that_reshapes_the_list() {
    for via in VIAS {
        let (m, nest, _) = setup();
        nest.set_devices(vec![device("aa", "laptop")]);
        let door = FakeFleetRemoval::watching(&nest);
        let thief = [0x33; 32];
        door.set_members(Ok(members_view([0x0a; 32], &[(thief, 2)])));
        wire(&m, &door, via);
        m.refresh().await;
        let armed_id = m.snapshot().members[0].device_id.clone();

        door.set_members(Ok(members_view([0x0a; 32], &[([0x22; 32], 1), (thief, 2)])));
        m.refresh().await;
        assert_eq!(m.snapshot().members.len(), 2, "{via:?}: the list reshaped");

        door.set_members(Ok(members_view([0x0a; 32], &[([0x22; 32], 1)])));
        m.remove_member_by_id(armed_id).await;
        assert_eq!(door.member_removals(), vec![thief], "{via:?}");

        // An id no longer listed: ignored, nothing asked.
        m.remove_member_by_id(fauna_core::hex32::encode(&thief))
            .await;
        assert_eq!(door.member_removals().len(), 1, "{via:?}");
    }
}

/// **A refused member removal says so and keeps the card** — the same
/// refusal copy as the row gesture, since the door answers the same enum.
#[tokio::test]
async fn a_refused_member_removal_says_so_and_keeps_the_card() {
    for (refusal, key) in [
        (
            FleetRemovalRefusal::OwnDevice,
            "devices.error_remove_own_device",
        ),
        (
            FleetRemovalRefusal::NotAMember,
            "devices.error_remove_unverified_device",
        ),
        (
            FleetRemovalRefusal::Unavailable("runtime gone".into()),
            "devices.error_remove_device",
        ),
    ] {
        let (m, nest, _) = setup();
        nest.set_devices(vec![device("aa", "laptop")]);
        let door = FakeFleetRemoval::watching(&nest);
        door.set_members(Ok(members_view([0x0a; 32], &[([0x22; 32], 1)])));
        door.set_member_response(Err(refusal.clone()));
        m.set_fleet_removal(door.clone());
        m.refresh().await;
        let calls_before = nest.calls().len();

        let armed_id = m.snapshot().members[0].device_id.clone();
        m.remove_member_by_id(armed_id).await;

        assert_eq!(door.member_removals(), vec![[0x22; 32]], "{refusal:?}");
        assert_eq!(
            nest.calls().len(),
            calls_before,
            "{refusal:?}: no refresh, no nest call"
        );
        let snap = m.snapshot();
        assert_eq!(snap.members.len(), 1, "{refusal:?}: the card stays");
        let err = snap.error.expect("the refusal surfaces");
        assert_eq!(err.key, key, "{refusal:?}");
    }
}

#[tokio::test]
async fn delete_folder_calls_kind() {
    let (m, nest, _) = setup();
    m.delete_folder("docs".into()).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::DeleteFolder {
            name: "docs".into(),
        }]
    );
}

#[tokio::test]
async fn resolve_conflict_forwards_chosen_winner() {
    let (m, nest, _) = setup();
    nest.set_conflicts(vec![conflict(7)]);
    m.refresh().await;
    m.resolve_conflict(7, Some("h1".into())).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::ResolveConflict {
            id: 7,
            winning_manifest_hash: Some("h1".into()),
        }]
    );
}

/// A choose-winner is signed by this device, and the seam vouches for it
/// under the conflict's rendered path (ruling (10)(f)) — so a pick the page
/// holds no conflict for, or one that is not among the conflict's candidates,
/// has nothing to be looked up under: refused on the device, nothing sent.
#[tokio::test]
async fn resolve_conflict_refuses_a_winner_the_page_cannot_place() {
    let (m, nest, _) = setup();
    m.resolve_conflict(7, Some("h1".into())).await;
    assert!(nest.calls().is_empty(), "unknown conflict: nothing sent");
    assert!(m.snapshot().error.is_some());

    nest.set_conflicts(vec![conflict(7)]);
    m.refresh().await;
    m.resolve_conflict(7, Some("not-a-candidate".into())).await;
    assert!(nest.calls().is_empty(), "not a candidate: nothing sent");
    assert!(m.snapshot().error.is_some());
}

/// A mark-only resolve signs nothing and needs no conflict on the page.
#[tokio::test]
async fn resolve_conflict_mark_only_is_untouched() {
    let (m, nest, _) = setup();
    m.resolve_conflict(7, None).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::ResolveConflict {
            id: 7,
            winning_manifest_hash: None,
        }]
    );
}

// ── Review list: use-other-version re-point + conflict policy ────────────────

/// The judged version history vouches for `manifest` at the fixtures' path
/// (`docs` / `/a.txt`) as a verbatim-restorable version with these SIGNED
/// fields — what the seam answers once the shared judge admitted it.
fn vouch(nest: &FakeDevicesNestApi, manifest: &str, size_bytes: i64, stamp: Option<u64>) {
    nest.set_judged_version(
        "docs",
        "/a.txt",
        manifest,
        fauna_devices_machine::CandidateVerdict::Verbatim {
            size_bytes,
            content_key_version: stamp,
        },
    );
}

/// **The attack** (`writer-signed-change-records.md` ruling (10)(a)): the
/// conflict row is the nest's word, and a lying nest names a candidate whose
/// manifest has NO admitted version in this file's history — the manifest of
/// a file sealed in another set. The gesture records nothing and says why.
#[tokio::test]
async fn use_other_version_refuses_a_candidate_with_no_judged_version() {
    let (m, nest, _) = setup();
    nest.set_conflicts(vec![resolved_conflict(7, "latest_wins", "h2")]);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert!(
        nest.calls().is_empty(),
        "nothing recorded: {:?}",
        nest.calls()
    );
    let err = m.snapshot().error.expect("the reason is on the page");
    assert_eq!(err.key, "devices.error_other_version_unverified");
}

/// The version is looked up under the hash of the path the record will CARRY
/// (the rendered path), never the conflict row's own `path_hash`: a version
/// that exists only under another path vouches for nothing here.
#[tokio::test]
async fn use_other_version_looks_the_version_up_under_the_rendered_path() {
    let (m, nest, _) = setup();
    let mut c = resolved_conflict(7, "latest_wins", "h2");
    // The row hashes to another path, where the manifest IS a version.
    c.path_hash = fauna_protocol::ByteBuf::from(fauna_core::sync::path_hash("/b.txt").to_vec());
    nest.set_judged_version(
        "docs",
        "/b.txt",
        "h1",
        fauna_devices_machine::CandidateVerdict::Verbatim {
            size_bytes: 10,
            content_key_version: None,
        },
    );
    nest.set_conflicts(vec![c]);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert!(nest.calls().is_empty(), "nothing restored across paths");
    assert_eq!(
        m.snapshot().error.expect("declined").key,
        "devices.error_other_version_unverified"
    );
}

/// An admitted version another identity signed, unstamped, must be opened
/// and re-sealed (ruling (10)(b)); the review list holds no byte seam, so it
/// refuses and names the file's version history.
#[tokio::test]
async fn use_other_version_refuses_a_version_that_needs_the_reseal() {
    let (m, nest, _) = setup();
    nest.set_judged_version(
        "docs",
        "/a.txt",
        "h1",
        fauna_devices_machine::CandidateVerdict::NeedsReseal,
    );
    nest.set_conflicts(vec![resolved_conflict(7, "latest_wins", "h2")]);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert!(nest.calls().is_empty(), "nothing recorded");
    assert_eq!(
        m.snapshot().error.expect("declined").key,
        "devices.error_other_version_needs_history"
    );
}

/// A seam that cannot judge (no identity, no custody) declines the gesture.
#[tokio::test]
async fn use_other_version_declines_when_the_seam_cannot_judge() {
    let (m, nest, _) = setup();
    nest.fail_judge(fauna_devices_machine::DevicesApiError::BadRequest {
        detail: "no identity".into(),
    });
    nest.set_conflicts(vec![resolved_conflict(7, "latest_wins", "h2")]);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert!(nest.calls().is_empty(), "nothing recorded");
    assert!(m.snapshot().error.is_some());
}

#[tokio::test]
async fn use_other_version_restores_the_retained_loser() {
    let (m, nest, _) = setup();
    // latest_wins: h2 (newer) won; h1 is the retained loser.
    nest.set_conflicts(vec![resolved_conflict(7, "latest_wins", "h2")]);
    vouch(&nest, "h1", 10, None);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RestoreFileVersion {
            folder: "docs".into(),
            device_id: "dd".repeat(32),
            path: "/a.txt".into(),
            manifest_hash: "h1".into(),
            size_bytes: 10,
            content_key_version: None,
            // Keyless machine (no LabelCustody wired) — the best-effort
            // degrade records the re-point plaintext-only (S8 D2).
            path_sealed: None,
        }]
    );
    assert!(m.snapshot().error.is_none());
}

/// A keyed machine's re-point seals the path it re-records (S8 D2) — exact
/// bytes by recomputing the derivation, since the re-pointed `sync_changes`
/// row is append-only nest-side and this record is its only chance to seal.
#[tokio::test]
async fn use_other_version_seals_the_re_pointed_path_under_the_owner_root() {
    let (m, nest, _) = setup();
    let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        owner.clone(),
    ));
    nest.set_conflicts(vec![resolved_conflict(7, "latest_wins", "h2")]);
    vouch(&nest, "h1", 10, None);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    let sealed = match nest.calls().first() {
        Some(FakeCall::RestoreFileVersion { path_sealed, .. }) => path_sealed
            .clone()
            .expect("a keyed machine seals the re-point"),
        other => panic!("expected RestoreFileVersion, got {other:?}"),
    };
    let expected = fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::owner_of(&owner),
        &fauna_core::sync::path_hash("/a.txt"),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        "/a.txt".as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    assert_eq!(sealed, expected);
}

#[tokio::test]
async fn use_other_version_on_merged_row_picks_latest_parent() {
    let (m, nest, _) = setup();
    // merged: winner hM is NOT a candidate; both parents retained. The one-tap
    // targets the more recent parent (h2 @ t11); finer control = File Versions.
    nest.set_conflicts(vec![resolved_conflict(7, "merged", "hM")]);
    vouch(&nest, "h2", 10, None);
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    match nest.calls().first() {
        Some(FakeCall::RestoreFileVersion { manifest_hash, .. }) => {
            assert_eq!(manifest_hash, "h2");
        }
        other => panic!("expected RestoreFileVersion, got {other:?}"),
    }
}

/// What is recorded is the VERSION's signed size and stamp, never the
/// candidate row's (ruling (10)(a)): the stamp decides which roots the head is
/// ever offered, so a row that lies about either changes nothing.
#[tokio::test]
async fn use_other_version_records_the_versions_signed_size_and_stamp() {
    let (m, nest, _) = setup();
    let mut c = resolved_conflict(7, "latest_wins", "h2");
    // The nest's row lies about both.
    c.candidates[0].content_key_version = Some(99);
    c.candidates[0].size_bytes = 123_456;
    nest.set_conflicts(vec![c]);
    vouch(&nest, "h1", 10, Some(3));
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    match nest.calls().first() {
        Some(FakeCall::RestoreFileVersion {
            manifest_hash,
            size_bytes,
            content_key_version,
            ..
        }) => {
            assert_eq!(manifest_hash, "h1");
            assert_eq!(*size_bytes, 10);
            assert_eq!(*content_key_version, Some(3));
        }
        other => panic!("expected RestoreFileVersion, got {other:?}"),
    }
}

#[tokio::test]
async fn use_other_version_on_unresolved_row_errors_without_calling() {
    let (m, nest, _) = setup();
    nest.set_conflicts(vec![conflict(7)]); // unresolved: no winner yet
    m.refresh().await;

    m.use_other_version(7, "dd".repeat(32)).await;
    assert!(nest.calls().is_empty());
    let err = m.snapshot().error.expect("error set");
    assert_eq!(err.key, "devices.error_use_other_version");
}

#[tokio::test]
async fn set_conflict_policy_forwards_selective_update() {
    let (m, nest, _) = setup();
    m.set_folder_conflict_policy("docs".into(), "latest_wins_always".into())
        .await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderConflictPolicy {
            name: "docs".into(),
            conflict_policy: "latest_wins_always".into(),
        }]
    );
}

#[tokio::test]
async fn set_folder_paths_forwards_only_paths() {
    let (m, nest, _) = setup();
    m.set_folder_paths(
        "docs".into(),
        Some(vec!["/a".into()]),
        Some(vec!["/b".into()]),
    )
    .await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderPaths {
            name: "docs".into(),
            include_paths: Some(vec!["/a".into()]),
            exclude_paths: Some(vec!["/b".into()]),
            // A keyless machine with no rows loaded mints no seal — the nest
            // then clears the columns rather than keeping a stale one
            // (path-sealing S6-c; the sealed arm is pinned separately below).
            include_sealed: None,
            exclude_sealed: None,
        }]
    );
}

/// The S6-c write half: an owner row + a wired key mints both seals, and they
/// open back to exactly the lists that were saved.
#[tokio::test]
async fn an_owner_save_mints_both_path_seals_under_the_owner_root() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    // The machine salts from the row it already holds, so the row must be
    // loaded before the save — which is the real gesture order (the page lists
    // before it edits).
    nest.set_folders(vec![folder("docs")]);
    m.refresh().await;
    let id = m.snapshot().folders[0].id;

    m.set_folder_paths(
        "docs".into(),
        Some(vec!["/home/me/tax".into()]),
        Some(vec!["/home/me/cache".into()]),
    )
    .await;

    let Some(FakeCall::SetFolderPaths {
        include_sealed,
        exclude_sealed,
        ..
    }) = nest
        .calls()
        .into_iter()
        .find(|c| matches!(c, FakeCall::SetFolderPaths { .. }))
    else {
        panic!("the save must reach the nest seam");
    };

    // Asserted by OPENING them, not merely by `is_some()`: a seal minted under
    // the wrong root or the wrong salt is `Some` too, and would fail silently as
    // an omitted list rather than an error.
    let keys = fauna_core::file_download::FileDownloadKeys::owner(root);
    assert_eq!(
        fauna_core::label_custody::render_include_paths(&keys, include_sealed.as_deref(), None, id),
        Some(vec!["/home/me/tax".to_string()]),
    );
    assert_eq!(
        fauna_core::label_custody::render_exclude_paths(&keys, exclude_sealed.as_deref(), None, id),
        Some(vec!["/home/me/cache".to_string()]),
    );
}

/// A member row is never sealed for — finding: include/exclude is
/// owner-only, so there is no member-facing seal to mint even when this reader
/// holds a key of their own.
#[tokio::test]
async fn a_member_row_mints_no_path_seal_even_with_a_key_wired() {
    let (m, nest, _) = setup();
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        fauna_core::crypto::BackupKey::from_bytes([3u8; 32]),
    ));
    // The member row must genuinely REACH state, or this test passes for the
    // wrong reason: with no `MlsQuery` wired the B3 join-filter drops it, and
    // `seal_paths_for` would then decline on "row not found" while the role
    // guard it exists to pin never runs. (Caught by mutation — removing the role
    // guard left this green.) So join the group and assert the row survived.
    m.set_mls_query(Arc::new(JoinedGroups(vec!["aa".into()])));
    nest.set_folders(vec![member_set("shared-docs", "aa")]);
    m.refresh().await;
    assert_eq!(
        m.snapshot().folders.len(),
        1,
        "the member row must survive the join-filter for this test to mean anything"
    );

    m.set_folder_paths("shared-docs".into(), Some(vec!["/a".into()]), None)
        .await;

    let Some(FakeCall::SetFolderPaths { include_sealed, .. }) = nest
        .calls()
        .into_iter()
        .find(|c| matches!(c, FakeCall::SetFolderPaths { .. }))
    else {
        panic!("the save must reach the nest seam");
    };
    assert_eq!(
        include_sealed, None,
        "a member is not the audience for the owner's filesystem layout"
    );
}

#[tokio::test]
async fn failed_gesture_sets_page_error() {
    let (m, nest, _) = setup();
    nest.set_delete_folder_response(Err(DevicesApiError::Conflict {
        detail: "set has snapshots".into(),
    }));
    m.delete_folder("docs".into()).await;

    let err = m.snapshot().error.expect("error set");
    assert_eq!(err.key, "devices.error_delete_folder");
    assert_eq!(
        err.args.get("message").map(String::as_str),
        Some("set has snapshots")
    );
}

#[tokio::test]
async fn successful_gesture_after_error_clears_error() {
    let (m, nest, _) = setup();
    nest.set_delete_folder_response(Err(DevicesApiError::Conflict {
        detail: "boom".into(),
    }));
    m.delete_folder("docs".into()).await;
    assert!(m.snapshot().error.is_some());

    // A subsequent successful refresh-backed gesture clears it.
    nest.set_delete_folder_response(Ok(()));
    m.delete_folder("docs".into()).await;
    assert!(m.snapshot().error.is_none());
}

// ── Observer ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn observer_notified_on_open_and_refresh() {
    let observer = CountingObserver::new();
    let obs: Arc<dyn DevicesObserver> = Arc::clone(&observer) as _;
    let nest = Arc::new(FakeDevicesNestApi::new());
    let factory = Arc::new(FakeWizardFactory::new());
    let api: Arc<dyn DevicesNestApi> = Arc::clone(&nest) as _;
    let wf: Arc<dyn WizardFactory> = Arc::clone(&factory) as _;
    let m = DevicesMachine::new(obs, api, wf);

    let before = observer.count();
    m.refresh().await;
    m.open_wizard();
    // Driving the embedded wizard also ticks the page observer (bridge).
    m.wizard().unwrap().set_name("x".into());
    assert!(observer.count() >= before + 3);
}

// silence unused-import lint for the FolderWizardObserver re-export check
#[allow(dead_code)]
fn _assert_observer_trait_reexported() -> Option<Box<dyn FolderWizardObserver>> {
    None
}

// ── Sealed-first conflict paths (path-sealing S3) ────────────────────────────
//
// `docs/goal/behavior/file-sync.md` § Sealed names & paths: the conflict list is
// one of the read surfaces that renders a user-chosen path, so it renders the
// SEAL first and falls back to the plaintext path, omitting a row it can open
// neither half of. These drive a real seal through the machine — the plaintext
// is deliberately **blanked** on the wire row, which is the post-flip shape.

/// Seal `path` under `root` the way `SyncEngine::seal_recorded_path` does for an
/// owner-only set: convergent nonce, salted by the path's own hash — via the
/// same `label_custody::seal_path` funnel production code uses, so this
/// fixture can't drift from the real sealing recipe.
fn seal_owner_path(root: &fauna_core::crypto::BackupKey, path: &str) -> Vec<u8> {
    fauna_core::label_custody::seal_path(&fauna_core::path_crypto::LabelRoot::owner_of(root), path)
        .unwrap()
}

/// A conflict row as it arrives once the plaintext column is scrubbed: sealed
/// label + the convergent salt, and NO plaintext path.
fn sealed_conflict(id: i64, root: &fauna_core::crypto::BackupKey, path: &str) -> SyncConflict {
    SyncConflict {
        path: String::new(),
        path_sealed: Some(fauna_protocol::ByteBuf::from(seal_owner_path(root, path))),
        path_hash: fauna_protocol::ByteBuf::from(fauna_core::sync::path_hash(path).to_vec()),
        ..conflict(id)
    }
}

#[tokio::test]
async fn a_sealed_conflict_renders_under_the_owner_key_with_the_plaintext_blanked() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    nest.set_conflicts(vec![sealed_conflict(5, &root, "taxes/2026-notice.pdf")]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.conflicts.len(), 1, "the row must survive the render");
    assert_eq!(snap.conflicts[0].path, "taxes/2026-notice.pdf");
    // The load-bearing half: `file_info` is precomputed AT TRANSCRIBE, so it can
    // only be right if the render ran BEFORE the transcribe.
    assert_eq!(
        snap.conflicts[0].file_info, "docs: taxes/2026-notice.pdf",
        "the conflict-file-info line must be built from the RENDERED path"
    );
    assert!(snap.error.is_none(), "a render is never a page error");
}

#[tokio::test]
async fn a_keyless_reader_omits_a_sealed_only_conflict_without_failing_the_page() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    // No custody wired at all — a keyless reader, or a reader outside the set.
    nest.set_conflicts(vec![sealed_conflict(5, &root, "taxes/2026-notice.pdf")]);

    m.refresh().await;

    let snap = m.snapshot();
    assert!(
        snap.conflicts.is_empty(),
        "the ratified degrade is OMIT — never an empty name"
    );
    assert!(
        snap.error.is_none(),
        "one unrenderable row must not take the page down"
    );
}

#[tokio::test]
async fn the_wrong_key_omits_rather_than_rendering_anything() {
    let (m, nest, _) = setup();
    let sealer = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
    ));
    nest.set_conflicts(vec![sealed_conflict(5, &sealer, "taxes/2026-notice.pdf")]);

    m.refresh().await;

    assert!(m.snapshot().conflicts.is_empty());
}

// ── Skipped catch-up changes (`conflicts.md` § Skipped catch-up changes reach
// the review list) ───────────────────────────────────────────────────────────

/// A skipped catch-up change as the device reports it: unresolved, no
/// candidates, `device_id` = the device that skipped.
fn skipped_change(id: i64, root: &fauna_core::crypto::BackupKey, path: &str) -> SyncConflict {
    SyncConflict {
        conflict_type: "catchup_failed".into(),
        candidates: Vec::new(),
        device_id: "bb".repeat(32),
        ..sealed_conflict(id, root, path)
    }
}

/// The one row an unreadable name never drops: there the unreadable name IS
/// the finding, so it renders the placeholder in place of the path.
#[tokio::test]
async fn an_unreadable_skipped_change_renders_name_less_never_omitted() {
    let (m, nest, _) = setup();
    let sealer = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
    ));
    nest.set_devices(vec![device("bb", "phone")]);
    nest.set_conflicts(vec![
        skipped_change(5, &sealer, "taxes/2026-notice.pdf"),
        // An ordinary conflict this reader cannot name still drops.
        sealed_conflict(6, &sealer, "other.pdf"),
    ]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.conflicts.len(), 1, "{:?}", snap.conflicts);
    let row = &snap.conflicts[0];
    assert_eq!(row.id, 5);
    assert_eq!(
        row.path,
        fauna_i18n::strings::devices::conflicts::UNREADABLE_PATH
    );
    assert_eq!(
        row.file_info,
        format!(
            "docs: {} (phone)",
            fauna_i18n::strings::devices::conflicts::UNREADABLE_PATH
        ),
        "the info line names the device that is behind"
    );
}

/// A readable skipped change keeps its path and gains the skipping device's
/// label; a device the list no longer holds is named by its id's prefix.
#[tokio::test]
async fn a_skipped_change_names_the_device_that_is_behind() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    nest.set_devices(vec![device("bb", "phone")]);
    let gone = SyncConflict {
        device_id: "cc".repeat(32),
        ..skipped_change(6, &root, "b.txt")
    };
    nest.set_conflicts(vec![skipped_change(5, &root, "a.txt"), gone]);

    m.refresh().await;

    let snap = m.snapshot();
    let info: Vec<&str> = snap
        .conflicts
        .iter()
        .map(|c| c.file_info.as_str())
        .collect();
    assert_eq!(info, vec!["docs: a.txt (phone)", "docs: b.txt (cccccccc)"]);
}

/// Answers a BOUND set's content keys for one set, addressed by its
/// `name_hash` — and owner-only for every other hash, so a lookup by a
/// scrubbed row's blank plaintext finds no content keys.
struct BoundByHashResolver {
    set: &'static str,
    keys: fauna_core::folder_keys::FolderContentKeys,
}

#[async_trait::async_trait]
impl fauna_core::folder_keys::FolderKeyResolver for BoundByHashResolver {
    async fn resolve(
        &self,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
        Ok(
            if *name_hash == fauna_core::path_crypto::set_name_hash(self.set) {
                fauna_core::folder_keys::ResolvedCustody::ContentKeyed(
                    fauna_core::folder_keys::ResolvedFolderKeys {
                        mls_group_id: Some(vec![9u8; 32]),
                        content_keys: Some(self.keys.clone()),
                        home_nest_url: None,
                        home_nest_actor_id: None,
                    },
                )
            } else {
                fauna_core::folder_keys::ResolvedCustody::owner_only()
            },
        )
    }
}

/// A BOUND set's conflict after the scrub: `folder` is blank, so custody finds
/// the set's content keys only by the row's `folder_hash` — and two scrubbed
/// sets never share one cache slot under the blank name. Before the hash
/// keying, the bound row resolved owner-only custody and omitted.
#[tokio::test]
async fn a_scrubbed_bound_sets_conflict_renders_by_its_folder_hash() {
    let (m, nest, _) = setup();
    let name = "Shared docs";
    let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
    let content_root = fauna_core::path_crypto::LabelRoot::content_key(
        *content.current_key(),
        content.current_version(),
    );
    let owner = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::new(
        Some(Arc::new(BoundByHashResolver {
            set: name,
            keys: content,
        })),
        Some(owner.clone()),
    ));
    let path = "taxes/2026-notice.pdf";
    let bound = SyncConflict {
        folder: String::new(),
        folder_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_set_name(&content_root, name)
                .unwrap()
                .unwrap(),
        )),
        folder_hash: Some(fauna_protocol::ByteBuf::from(
            fauna_core::path_crypto::set_name_hash(name).to_vec(),
        )),
        path: String::new(),
        path_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_path(&content_root, path).unwrap(),
        )),
        path_hash: fauna_protocol::ByteBuf::from(fauna_core::sync::path_hash(path).to_vec()),
        ..conflict(5)
    };
    // An owner-only set, also scrubbed: rendered under the owner root.
    let owned_name = "Taxes";
    let owner_root = fauna_core::path_crypto::LabelRoot::owner_of(&owner);
    let owned = SyncConflict {
        folder: String::new(),
        folder_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_set_name(&owner_root, owned_name)
                .unwrap()
                .unwrap(),
        )),
        folder_hash: Some(fauna_protocol::ByteBuf::from(
            fauna_core::path_crypto::set_name_hash(owned_name).to_vec(),
        )),
        ..sealed_conflict(6, &owner, "a.txt")
    };
    nest.set_conflicts(vec![bound, owned]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.conflicts.len(), 2, "neither scrubbed row omits");
    assert_eq!(snap.conflicts[0].path, path);
    assert_eq!(snap.conflicts[0].file_info, format!("{name}: {path}"));
    assert_eq!(snap.conflicts[1].file_info, format!("{owned_name}: a.txt"));
}

/// A device's places after the scrub: each place's `name` is blank, so the set
/// name renders only from its seal under custody found by the place's
/// `name_hash` — a bound set's content keys, an owned set's owner root — and a
/// place this reader cannot open drops while the device row stays.
#[tokio::test]
async fn a_devices_scrubbed_places_render_by_their_name_hash() {
    let (m, nest, _) = setup();
    let bound_name = "Shared docs";
    let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
    let content_root = fauna_core::path_crypto::LabelRoot::content_key(
        *content.current_key(),
        content.current_version(),
    );
    let owner = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::new(
        Some(Arc::new(BoundByHashResolver {
            set: bound_name,
            keys: content,
        })),
        Some(owner.clone()),
    ));
    let scrubbed = |name: &str, root: &fauna_core::path_crypto::LabelRoot| WireDeviceFolderRole {
        name: String::new(),
        name_hash: Some(fauna_protocol::ByteBuf::from(
            fauna_core::path_crypto::set_name_hash(name).to_vec(),
        )),
        name_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_set_name(root, name)
                .unwrap()
                .unwrap(),
        )),
        flags: fauna_protocol::folders::PlaceFlags::default_place(),
        ..Default::default()
    };
    let owner_root = fauna_core::path_crypto::LabelRoot::owner_of(&owner);
    let stranger_root = fauna_core::path_crypto::LabelRoot::owner_of(
        &fauna_core::crypto::BackupKey::from_bytes([8u8; 32]),
    );
    nest.set_devices(vec![SyncDevice {
        folders: vec![
            scrubbed(bound_name, &content_root),
            scrubbed("Taxes", &owner_root),
            scrubbed("Unopenable", &stranger_root),
        ],
        ..device("aa", "laptop")
    }]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.devices.len(), 1, "the device row stays");
    let names: Vec<&str> = snap.devices[0]
        .folders
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![bound_name, "Taxes"],
        "both openable places render; the unopenable one drops, never blank"
    );
    assert!(snap.error.is_none());
}

/// Since schema 114 a sealed set's list row rests NO plaintext name — `name` is
/// the empty sentinel beside `name_hash` + `name_sealed` (`path-sealing.md`
/// § the set-name plane). The adapter hands the machine those wire rows
/// unrendered, so the machine — the one holder of label custody — renders the
/// set name by the row's hash, together with its sealed retention policy; a row
/// it cannot open drops, never a nameless row. Before the fix the adapter's
/// custody-less `FoldersClient` omitted every such row, and an app-created
/// folder never appeared on the page.
#[tokio::test]
async fn a_scrubbed_folder_row_renders_its_sealed_name_by_hash() {
    let (m, nest, _) = setup();
    let bound_name = "Shared docs";
    let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
    let content_root = fauna_core::path_crypto::LabelRoot::content_key(
        *content.current_key(),
        content.current_version(),
    );
    let owner = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::new(
        Some(Arc::new(BoundByHashResolver {
            set: bound_name,
            keys: content,
        })),
        Some(owner.clone()),
    ));
    let owner_root = fauna_core::path_crypto::LabelRoot::owner_of(&owner);
    let stranger_root = fauna_core::path_crypto::LabelRoot::owner_of(
        &fauna_core::crypto::BackupKey::from_bytes([8u8; 32]),
    );
    let scrubbed =
        |id: i64, name: &str, root: &fauna_core::path_crypto::LabelRoot| WireFolderSummary {
            id,
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            name_sealed: Some(fauna_protocol::ByteBuf::from(
                fauna_core::label_custody::seal_set_name(root, name)
                    .unwrap()
                    .unwrap(),
            )),
            ..folder("")
        };
    let taxes_policy = fauna_core::label_custody::seal_retention_policy(
        &owner_root,
        "Taxes",
        r#"{"keep_last":3}"#,
    )
    .unwrap();
    nest.set_folders(vec![
        scrubbed(1, bound_name, &content_root),
        WireFolderSummary {
            retention_policy_sealed: Some(fauna_protocol::ByteBuf::from(taxes_policy)),
            ..scrubbed(2, "Taxes", &owner_root)
        },
        scrubbed(3, "Unopenable", &stranger_root),
        folder("__mail"),
    ]);

    m.refresh().await;

    let snap = m.snapshot();
    let names: Vec<&str> = snap.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec![bound_name, "Taxes", "__mail"],
        "both openable sets render by hash, the unopenable one drops, a plaintext row stays"
    );
    assert_eq!(
        snap.folders[1].retention_policy.as_deref(),
        Some(r#"{"keep_last":3}"#),
        "the sealed retention policy opens with the name"
    );
    assert!(snap.error.is_none());
}

/// The expand-phase shape: rows still carry their plaintext, so every reader —
/// custody or not — keeps seeing them. This is what makes the write half
/// deployable ahead of the per-app render sweep.
#[tokio::test]
async fn a_plaintext_row_still_renders_for_a_keyless_reader() {
    let (m, nest, _) = setup();
    nest.set_conflicts(vec![conflict(5)]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.conflicts.len(), 1);
    assert_eq!(snap.conflicts[0].path, "/a.txt");
    assert_eq!(snap.conflicts[0].file_info, "docs: /a.txt");
}

/// The salt is the wire's `path_hash`, not a derivation from the plaintext —
/// which is the whole reason a scrubbed row stays openable. Planting a WRONG
/// hash must therefore break the open (proving the render really consumes it).
#[tokio::test]
async fn the_render_salts_from_the_wire_path_hash() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    let mut row = sealed_conflict(5, &root, "taxes/2026-notice.pdf");
    row.path_hash =
        fauna_protocol::ByteBuf::from(fauna_core::sync::path_hash("a-different-path").to_vec());
    nest.set_conflicts(vec![row]);

    m.refresh().await;

    assert!(
        m.snapshot().conflicts.is_empty(),
        "a wrong salt must fail the AEAD tag and omit, never render a wrong name"
    );
}

// ── the device-label render seam (path-sealing S6-b) ────────────────────────

/// A device row as it arrives once the plaintext column is scrubbed: sealed
/// label, no plaintext. The salt is the `device_id` already on the row, which is
/// why this plane needs no hash companion.
fn sealed_device(id: &str, root: &fauna_core::crypto::BackupKey, label: &str) -> SyncDevice {
    let device_id_hex = id.repeat(32);
    let salt = fauna_core::hex32::decode(&device_id_hex).expect("32-byte hex");
    let sealed = fauna_core::label_custody::seal_device_label(
        &fauna_core::path_crypto::LabelRoot::owner_of(root),
        &salt,
        label,
    )
    .expect("seal ok")
    .expect("a user-chosen label seals");
    SyncDevice {
        label: String::new(),
        label_sealed: Some(fauna_protocol::ByteBuf::from(sealed)),
        ..device(id, label)
    }
}

#[tokio::test]
async fn a_sealed_device_label_renders_under_the_owner_key_with_the_plaintext_blanked() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    nest.set_devices(vec![sealed_device("aa", &root, "Alice's Laptop")]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.devices.len(), 1);
    assert_eq!(snap.devices[0].label, "Alice's Laptop");
    assert!(snap.error.is_none(), "a render is never a page error");
}

/// The device degrade is deliberately **weaker** than the conflict/path one: an
/// unopenable label leaves an unnamed row rather than dropping it, because a
/// device is actionable by `device_id` alone and hiding one the user may need to
/// revoke is the worse outcome.
#[tokio::test]
async fn an_unopenable_device_label_keeps_its_row_unnamed_rather_than_dropping_it() {
    let (m, nest, _) = setup();
    let sealer = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
    ));
    nest.set_devices(vec![sealed_device("aa", &sealer, "Alice's Laptop")]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.devices.len(),
        1,
        "the device must stay revocable even unnamed"
    );
    assert_eq!(snap.devices[0].label, "");
    assert_eq!(snap.devices[0].device_id, "aa".repeat(32));
    assert!(snap.error.is_none());
}

/// The expand-phase shape: the plaintext still rests, so a keyless reader keeps
/// seeing the label — what makes the write half deployable ahead of the per-app
/// render sweep.
#[tokio::test]
async fn a_plaintext_device_row_still_renders_for_a_keyless_reader() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "laptop")]);

    m.refresh().await;

    assert_eq!(m.snapshot().devices[0].label, "laptop");
}

// ── the selective-sync path render seam (path-sealing S6-c) ─────────────────

/// A folder row as it arrives once the S9 flip scrubs the plaintext columns:
/// the include/exclude lists rest sealed-only. The salt is the row `id` already
/// on the struct, which is why this plane needs no hash companion.
fn sealed_paths_set(
    name: &str,
    root: &fauna_core::crypto::BackupKey,
    include: &[&str],
    exclude: &[&str],
) -> WireFolderSummary {
    let base = folder(name);
    let include: Vec<String> = include.iter().map(|s| (*s).to_string()).collect();
    let exclude: Vec<String> = exclude.iter().map(|s| (*s).to_string()).collect();
    WireFolderSummary {
        // Scrubbed, exactly as the nest rests them post-flip.
        include_paths: None,
        exclude_paths: None,
        include_paths_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_include_paths(root, base.id, &include)
                .expect("seal ok"),
        )),
        exclude_paths_sealed: Some(fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_exclude_paths(root, base.id, &exclude)
                .expect("seal ok"),
        )),
        ..base
    }
}

/// The bug this seam closes: the machine held the owner key (it seals with it on
/// save) but ingested folder rows through a transcribe that dropped the
/// `*_sealed` columns, so a reader who demonstrably holds the key saw BOTH
/// selective-sync lists as empty. Worse than a blank field — the save gesture is
/// the one durable writer of these seals, so saving from the blank editor
/// overwrote the user's real lists with nothing.
#[tokio::test]
async fn a_keyed_machine_renders_the_sealed_selective_sync_paths() {
    let (m, nest, _) = setup();
    let root = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        root.clone(),
    ));
    nest.set_folders(vec![sealed_paths_set(
        "docs",
        &root,
        &["/docs", "/photos"],
        &["*.tmp", ".git"],
    )]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.folders.len(), 1, "a sealed row is never dropped");
    assert_eq!(
        snap.folders[0].include_paths.as_deref(),
        Some(&["/docs".to_string(), "/photos".to_string()][..]),
    );
    assert_eq!(
        snap.folders[0].exclude_paths.as_deref(),
        Some(&["*.tmp".to_string(), ".git".to_string()][..]),
    );
    assert!(snap.error.is_none(), "a render is never a page error");
}

/// The degrade is the conflict-`details` one, not the row-dropping one: a set is
/// an actionable object in its own right (rename, delete, bind a folder), so an
/// unopenable pair leaves the row with no lists rather than hiding the set.
#[tokio::test]
async fn an_unopenable_selective_sync_pair_keeps_its_row_without_lists() {
    let (m, nest, _) = setup();
    let sealer = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
    m.set_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
        fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
    ));
    nest.set_folders(vec![sealed_paths_set(
        "docs",
        &sealer,
        &["/docs"],
        &[".git"],
    )]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.folders.len(), 1, "the set stays actionable");
    assert_eq!(snap.folders[0].name, "docs");
    assert_eq!(snap.folders[0].include_paths, None);
    assert_eq!(snap.folders[0].exclude_paths, None);
    assert!(snap.error.is_none());
}

/// The expand-phase shape: plaintext still resting, no seal — a keyless reader
/// keeps seeing the lists, which is what let the write half ship ahead of this
/// render.
#[tokio::test]
async fn a_plaintext_selective_sync_pair_still_renders_for_a_keyless_reader() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![WireFolderSummary {
        include_paths: Some(vec!["/docs".into()]),
        exclude_paths: Some(vec![".git".into()]),
        ..folder("docs")
    }]);

    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.folders[0].include_paths.as_deref(),
        Some(&["/docs".to_string()][..])
    );
    assert_eq!(
        snap.folders[0].exclude_paths.as_deref(),
        Some(&[".git".to_string()][..])
    );
}

struct FixedFollowed(Vec<FollowedFolderSummary>);
#[async_trait::async_trait]
impl FollowedFoldersSource for FixedFollowed {
    async fn followed_folders(&self) -> Vec<FollowedFolderSummary> {
        self.0.clone()
    }
}

fn followed(id: i64, name: &str, available: bool) -> FollowedFolderSummary {
    FollowedFolderSummary {
        folder_id: id,
        home_nest_url: "https://home.example".into(),
        owner_actor_id: "cd".repeat(32),
        display_name: name.into(),
        available,
        ..Default::default()
    }
}

/// Followed public folders render as their **own** list, not mixed into
/// `folders` — they are a distinct row kind (their own element IDs, no roster,
/// no binding, no group).
#[tokio::test]
async fn followed_folders_project_into_their_own_list() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![folder("my-docs")]);
    m.set_followed_folders_source(Arc::new(FixedFollowed(vec![
        followed(7, "their-site", true),
        followed(9, "their-notes", true),
    ])));

    m.refresh().await;
    let snap = m.snapshot();

    assert_eq!(
        snap.folders
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["my-docs"],
        "a follow is never mixed into the user's own folders"
    );
    assert_eq!(
        snap.followed
            .iter()
            .map(|f| f.display_name.as_str())
            .collect::<Vec<_>>(),
        vec!["their-site", "their-notes"]
    );
    assert_eq!(snap.followed[0].folder_id, 7, "the pinned address rides");
    assert_eq!(snap.followed[0].home_nest_url, "https://home.example");
}

/// An unwired source is the correct render for an app that has not built the
/// follow surface: no rows, no error, and the user's own folders unaffected.
#[tokio::test]
async fn without_a_followed_source_the_page_shows_no_followed_rows() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![folder("my-docs")]);

    m.refresh().await;
    let snap = m.snapshot();

    assert!(snap.followed.is_empty());
    assert_eq!(snap.folders.len(), 1);
    assert!(
        snap.error.is_none(),
        "an unwired follow source is not an error"
    );
}

/// A revoked follow (the owner flipped the audience back) **stays on the page**
/// in an unavailable state — it is never silently dropped, because the user is
/// the one who decides to remove it, and a re-flip resumes it.
#[tokio::test]
async fn a_revoked_follow_stays_visible_and_unavailable() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![]);
    m.set_followed_folders_source(Arc::new(FixedFollowed(vec![followed(
        7,
        "their-site",
        false,
    )])));

    m.refresh().await;
    let snap = m.snapshot();

    assert_eq!(snap.followed.len(), 1, "a revoked follow is not dropped");
    assert!(!snap.followed[0].available);
    assert_eq!(
        snap.followed[0].display_name, "their-site",
        "and it keeps its name so the row is still recognisable"
    );
    assert!(
        snap.error.is_none(),
        "one unavailable follow is not a page-level failure"
    );
}

/// The list is replaced wholesale each refresh, so an unfollow performed
/// elsewhere disappears on the next read rather than lingering.
#[tokio::test]
async fn refresh_reflects_an_unfollow() {
    let (m, nest, _) = setup();
    nest.set_folders(vec![]);
    m.set_followed_folders_source(Arc::new(FixedFollowed(vec![
        followed(7, "their-site", true),
        followed(9, "their-notes", true),
    ])));
    m.refresh().await;
    assert_eq!(m.snapshot().followed.len(), 2);

    // The source now reports one — the other was unfollowed.
    m.set_followed_folders_source(Arc::new(FixedFollowed(vec![followed(
        9,
        "their-notes",
        true,
    )])));
    m.refresh().await;

    let snap = m.snapshot();
    assert_eq!(snap.followed.len(), 1);
    assert_eq!(snap.followed[0].folder_id, 9);
}

/// A failing FOLDERS read must not take the followed rows down with it: they
/// come from the user's own config, not from that call.
#[tokio::test]
async fn followed_rows_survive_a_failed_folders_read() {
    let (m, nest, _) = setup();
    m.set_followed_folders_source(Arc::new(FixedFollowed(vec![followed(
        7,
        "their-site",
        true,
    )])));
    nest.fail_lists(DevicesApiError::Transient {
        detail: "nest unreachable".into(),
    });

    m.refresh().await;
    let snap = m.snapshot();

    assert!(snap.error.is_some(), "the folders failure still surfaces");
    assert_eq!(
        snap.followed.len(),
        1,
        "but the follow list, which that read never produced, still renders"
    );
}

// ── Phase-4 write gestures ─────
//
// The three writes the six lagging apps drive through this machine:
// `folder-audience-select`, `folder-website-toggle`, and the post-create device
// place editor's checkboxes. tui reaches all three crate-direct over
// `FoldersClient` (`ui/folders.md` § Audience and website serving → Where the
// logic lives); a machine-mediated app needs them here, with the same
// refresh-on-success + error-on-failure contract every sibling gesture has.

#[tokio::test]
async fn set_folder_audience_forwards_the_audience_alone() {
    let (m, nest, _) = setup();
    m.set_folder_audience("docs".into(), "public".into()).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderAudience {
            name: "docs".into(),
            audience: "public".into(),
            attested: false,
        }],
        "a plain `folders.update` carrying only the audience — no content key \
         and no custody sentinel staged (the ->shared direction that needs one \
         is never offered as a destination); unwired, the seam is handed no \
         attestor either"
    );
}

/// The `→public` flip is the owner confirm's landing point, so it is where the
/// owner's attestation is minted — and the seam can only sign under a key the
/// machine hands it. The build glue wires that key post-construction
/// (`set_audience_attestor`, the `set_label_custody` delivery); this pins that
/// the gesture forwards it, so an unwired app is a wiring gap the FFI / wasm
/// pins catch, never a machine that silently drops the key on the way.
///
/// Mutation: drop the `attestor` argument in `set_folder_audience` → the seam
/// records `attested: false` → this reds.
#[tokio::test]
async fn set_folder_audience_hands_the_seam_the_wired_attestor() {
    let (m, nest, _) = setup();
    assert!(!m.has_audience_attestor(), "unwired at construction");
    m.set_audience_attestor(Arc::new(fauna_core::identity::ActorKeypair::from_secret(
        [7; 32],
    )));
    assert!(m.has_audience_attestor());
    m.set_folder_audience("docs".into(), "public".into()).await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderAudience {
            name: "docs".into(),
            audience: "public".into(),
            attested: true,
        }],
        "the wired identity key rides to the seam, so `set_audience` signs"
    );
}

#[tokio::test]
async fn set_folder_website_enabled_forwards_the_flag_alone() {
    let (m, nest, _) = setup();
    m.set_folder_website_enabled("docs".into(), true).await;
    m.set_folder_website_enabled("docs".into(), false).await;
    assert_eq!(
        nest.calls(),
        vec![
            FakeCall::SetFolderWebsiteEnabled {
                name: "docs".into(),
                enabled: true,
            },
            FakeCall::SetFolderWebsiteEnabled {
                name: "docs".into(),
                enabled: false,
            },
        ],
        "both directions ride the same write — the toggle is orthogonal to the \
         audience and is real (merely inert) on a folder nobody can read"
    );
}

/// **The point applies whole.** A place edit sends the seat's full flag triple,
/// never just the box that moved — the nest writes the point, not a delta, and a
/// caller that sent one flag would silently clear the other two.
#[tokio::test]
async fn set_folder_place_forwards_the_whole_flag_triple() {
    let (m, nest, _) = setup();
    m.set_folder_place("docs".into(), "ab".repeat(32), true, false, false)
        .await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderPlace {
            name: "docs".into(),
            device_id: "ab".repeat(32),
            originates: true,
            accepts: false,
            applies_deletes: false,
        }],
        "all three flags ride every call, including the two left alone"
    );
}

/// A point the four legacy roles cannot name is sendable since phase 2 slice f —
/// the machine must not refuse or round it (`ui/folders.md` § Implementation
/// status today: *a trickle-down leg paints three checkboxes and no refusal*).
#[tokio::test]
async fn set_folder_place_sends_a_point_no_legacy_role_names() {
    let (m, nest, _) = setup();
    // originates + accepts + never-applies-deletes: the archive seat that the
    // four-role vocabulary has no word for.
    m.set_folder_place("docs".into(), "cd".repeat(32), true, true, false)
        .await;
    assert_eq!(
        nest.calls(),
        vec![FakeCall::SetFolderPlace {
            name: "docs".into(),
            device_id: "cd".repeat(32),
            originates: true,
            accepts: true,
            applies_deletes: false,
        }],
        "the unnamed point goes out unrounded"
    );
}

/// Every one of the three fails the way its siblings do: the page's own
/// `error-message`, never a silent no-op. Failure is the arm an app cannot
/// render if the machine swallows it.
#[tokio::test]
async fn a_failed_phase_4_write_surfaces_on_the_page_error() {
    for (label, expected_key) in [
        ("audience", "devices.error_set_audience"),
        // Reuses tui's existing key for the same failure, not a twin of it.
        ("website", "devices.error_serve_website"),
        ("place", "devices.error_set_place"),
    ] {
        let (m, nest, _) = setup();
        let failure = Err(DevicesApiError::Transient {
            detail: "nest unreachable".into(),
        });

        match label {
            "audience" => {
                nest.set_audience_response(failure);
                m.set_folder_audience("docs".into(), "public".into()).await
            }
            "website" => {
                nest.set_website_response(failure);
                m.set_folder_website_enabled("docs".into(), true).await
            }
            _ => {
                nest.set_place_response(failure);
                m.set_folder_place("docs".into(), "ab".repeat(32), true, true, true)
                    .await
            }
        }

        let err = m
            .snapshot()
            .error
            .unwrap_or_else(|| panic!("the {label} write must surface its failure"));
        assert_eq!(err.key, expected_key);
    }
}

// ── Per-device p2p participation (`behavior/p2p.md` § Per-device participation) ──

/// A participation door whose answers are scripted: `own_row` returns what
/// the test set, `set_local` records the flip and answers `set_answer`. The
/// runtime-absent seat is every call erring — what `RuntimeP2pParticipation`
/// answers with no handle.
struct FakeParticipation {
    own_row: Result<Option<String>, String>,
    set_answer: Result<(), String>,
    flips: Mutex<Vec<bool>>,
}

const RUNTIME_ABSENT: &str = "the account runtime is not running";

impl FakeParticipation {
    fn scripted(
        own_row: Result<Option<String>, String>,
        set_answer: Result<(), String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            own_row,
            set_answer,
            flips: Mutex::new(Vec::new()),
        })
    }

    fn runtime_absent() -> Arc<Self> {
        Self::scripted(Err(RUNTIME_ABSENT.into()), Err(RUNTIME_ABSENT.into()))
    }

    fn flips(&self) -> Vec<bool> {
        self.flips.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl P2pParticipation for FakeParticipation {
    async fn local(&self) -> Result<bool, String> {
        Err(RUNTIME_ABSENT.into())
    }

    async fn set_local(&self, on: bool) -> Result<(), String> {
        self.flips.lock().unwrap().push(on);
        self.set_answer.clone()
    }

    async fn own_row(&self) -> Result<Option<String>, String> {
        self.own_row.clone()
    }
}

/// A seat with no account runtime (iOS today) can name its own row neither
/// through the door nor through the fleet id (the member door is
/// runtime-backed too) — so the app's own device id, the one its
/// `device-this-mark-badge` already paints from, is the last word on which row
/// is this device's. The own-row gesture then takes the OWN arm and says why it
/// cannot flip, on `error-message`; it never sends the nest an off-request
/// against the device's own row.
#[tokio::test]
async fn a_runtimeless_seat_names_its_own_row_and_the_own_switch_says_why_it_cannot_flip() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "phone"), device("bb", "laptop")]);
    let door = FakeParticipation::runtime_absent();
    m.set_p2p_participation_door(door.clone());
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    m.set_p2p_participation(0, false).await;

    assert_eq!(door.flips(), vec![false], "the own arm, through the door");
    assert_eq!(nest.calls(), vec![], "no off-request against our own row");
    assert_eq!(
        m.snapshot().error.expect("the refusal is surfaced").key,
        "devices.error_set_p2p_participation"
    );
}

/// The app's hint names only its own row: a sibling's row on the same
/// runtimeless seat still takes the request-off arm.
#[tokio::test]
async fn a_this_device_hint_leaves_a_siblings_row_on_the_request_off_arm() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "phone"), device("bb", "laptop")]);
    let door = FakeParticipation::runtime_absent();
    m.set_p2p_participation_door(door.clone());
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    m.set_p2p_participation(1, false).await;

    assert_eq!(door.flips(), Vec::<bool>::new());
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RequestP2pOff {
            device_id: "bb".repeat(32)
        }]
    );
}

/// The door's own enrolled row outranks the app's hint — a seat whose runtime
/// answers is never steered by a stale app-held id.
#[tokio::test]
async fn the_doors_enrolled_row_outranks_the_apps_hint() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "phone"), device("bb", "laptop")]);
    let door = FakeParticipation::scripted(Ok(Some("bb".repeat(32))), Ok(()));
    m.set_p2p_participation_door(door.clone());
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    m.set_p2p_participation(0, false).await;

    assert_eq!(door.flips(), Vec::<bool>::new(), "row 0 is a sibling");
    assert_eq!(
        nest.calls(),
        vec![FakeCall::RequestP2pOff {
            device_id: "aa".repeat(32)
        }]
    );
}

/// Each published row's `own` flag, in roster order.
fn painted_own(m: &DevicesMachine) -> Vec<bool> {
    m.snapshot()
        .devices
        .iter()
        .map(|d| {
            d.p2p_participation_paint
                .as_ref()
                .expect("the machine paints every row it publishes")
                .own
        })
        .collect()
}

/// The snapshot's per-row paint resolves own-ness at refresh by the SAME
/// rule the gesture takes its arm by: the door's enrolled row outranks the
/// app's hint, so the paint and the click cannot disagree.
#[tokio::test]
async fn the_painted_own_row_is_the_doors_enrolled_row_over_the_apps_hint() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "phone"), device("bb", "laptop")]);
    m.set_p2p_participation_door(FakeParticipation::scripted(
        Ok(Some("bb".repeat(32))),
        Ok(()),
    ));
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    assert_eq!(painted_own(&m), vec![false, true]);
    let own = m.snapshot().devices[1]
        .p2p_participation_paint
        .clone()
        .unwrap();
    assert_eq!(own.label.key, "devices.p2p_participation_own");
    assert!(own.actionable, "the own switch goes both ways");
}

/// A runtimeless seat's door names no row and no fleet id is known, so the
/// app's this-device hint names the painted own row — the one whose gesture
/// takes the own arm (`a_runtimeless_seat_names_its_own_row_…` above).
#[tokio::test]
async fn a_runtimeless_seat_paints_the_hinted_row_as_its_own() {
    let (m, nest, _) = setup();
    nest.set_devices(vec![device("aa", "phone"), device("bb", "laptop")]);
    m.set_p2p_participation_door(FakeParticipation::runtime_absent());
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    assert_eq!(painted_own(&m), vec![true, false]);
}

/// No door (web): no row is this device's own, whatever the app's hint says
/// — every row paints the sibling arm, exactly as every click takes the
/// request-off arm (`p2p.md` § Per-device participation: web has no own row).
#[tokio::test]
async fn with_no_door_no_row_paints_as_its_own() {
    let (m, nest, _) = setup();
    let mut reported_off = device("bb", "laptop");
    reported_off.p2p_participation = Some(false);
    nest.set_devices(vec![device("aa", "phone"), reported_off]);
    m.set_this_device_row(Some("aa".repeat(32)));
    m.refresh().await;

    assert_eq!(painted_own(&m), vec![false, false]);
    let devices = m.snapshot().devices;
    let hinted = devices[0].p2p_participation_paint.clone().unwrap();
    assert_eq!(hinted.label.key, "devices.p2p_participation_unreported");
    assert!(hinted.checked && hinted.actionable, "may be on: ask it off");
    let off = devices[1].p2p_participation_paint.clone().unwrap();
    assert!(
        !off.checked && !off.actionable,
        "enabling is that device's consent"
    );
}
