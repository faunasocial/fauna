//! tier_1 coverage for the Backups page machine — the snapshot half.
//!
//! Every ruling in `docs/goal/ui/backups.md` § Snapshot-list shape that the
//! machine (rather than an app) is responsible for gets a pin here, with an
//! emphasis on the ones that reconcile a *measured* six-app divergence: the
//! `last_backed_up` derivation, the single-flight gate, preview-first prune, the
//! called-not-re-derived `is_ok` predicate, and per-row integrity implication.
//!
//! Test-shape note (e2e convention 17's discipline, applied at tier_1): the
//! assertions read the machine's *snapshot* and the fake's *call log*, never the
//! machine's internals. A test that only read back what it had just written
//! would pass against a page that renders a plausible default it never fetched
//! — the exact defect class a prior pass found on linux's Privacy page.

use std::sync::Arc;

use fauna_backups_machine::{
    BackupOp, BackupsMachine, BackupsNestApi, FakeBackupsNestApi, FakeCall, NullObserver,
    PolicyState, RowIntegrity, SnapshotState,
};
use fauna_protocol::filesync::{
    IMMEDIATE_DELETE_ACK_TEXT, PrunableSnapshot, SnapshotCheckError, SnapshotCheckReply,
    SnapshotFileEntry, SnapshotGetReply, SnapshotPruneSetPolicyReply, SnapshotSummaryRow,
};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

// ── Fixtures ────────────────────────────────────────────────────

fn set(name: &str) -> WireFolderSummary {
    WireFolderSummary {
        name: name.to_string(),
        ..Default::default()
    }
}

fn row(id: i64, created_at: i64) -> SnapshotSummaryRow {
    SnapshotSummaryRow {
        id,
        created_at,
        file_count: 3,
        total_bytes: 300,
        ..Default::default()
    }
}

/// Build a machine over a fake with `sets` and, for each set, its rows.
fn machine_with(
    sets: Vec<WireFolderSummary>,
    rows: Vec<(&str, Vec<SnapshotSummaryRow>)>,
) -> (Arc<BackupsMachine>, Arc<FakeBackupsNestApi>) {
    let fake = Arc::new(FakeBackupsNestApi::new());
    fake.set_folders(sets);
    for (name, r) in rows {
        fake.set_snapshots(name, r);
    }
    let api: Arc<dyn BackupsNestApi> = fake.clone();
    let machine = BackupsMachine::new(Arc::new(NullObserver), api);
    (machine, fake)
}

// ── Selector source, filtering, ordering, default selection ─────

#[tokio::test]
async fn refresh_excludes_reserved_sets_orders_by_name_and_defaults_to_the_first() {
    // Deliberately unordered, and carrying a reserved set:
    // the *machine* owns both rulings, so the fake feeds it raw.
    let (machine, _fake) = machine_with(
        vec![set("zulu"), set("__conv/abc"), set("alpha"), set("mike")],
        vec![],
    );

    machine.refresh().await;
    let snap = machine.snapshot();

    // Reserved excluded; every other folder kept (snapshot create refuses
    // only custody copies).
    let names: Vec<&str> = snap.folders.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "mike", "zulu"],
        "name-ordered, reserved dropped"
    );

    // Deterministic default = the first row of the name-ordered list.
    assert_eq!(snap.selected_folder.as_deref(), Some("alpha"));
}

#[tokio::test]
async fn a_selection_that_disappears_falls_back_to_the_default() {
    let (machine, fake) = machine_with(vec![set("alpha"), set("bravo")], vec![]);
    machine.refresh().await;
    machine.select_folder("bravo".to_string()).await;
    assert_eq!(machine.snapshot().selected_folder.as_deref(), Some("bravo"));

    // The set goes away underneath the page.
    fake.set_folders(vec![set("alpha")]);
    machine.refresh().await;

    assert_eq!(
        machine.snapshot().selected_folder.as_deref(),
        Some("alpha"),
        "a vanished selection falls back to the default, never sticks to a name the nest no longer serves"
    );
}

#[tokio::test]
async fn a_still_present_selection_survives_a_refresh() {
    let (machine, _fake) = machine_with(vec![set("alpha"), set("bravo")], vec![]);
    machine.refresh().await;
    machine.select_folder("bravo".to_string()).await;
    machine.refresh().await;
    assert_eq!(machine.snapshot().selected_folder.as_deref(), Some("bravo"));
}

fn two_sets_each_with_one_row() -> (Arc<BackupsMachine>, Arc<FakeBackupsNestApi>) {
    machine_with(
        vec![set("alpha"), set("bravo")],
        vec![("alpha", vec![row(1, 100)]), ("bravo", vec![row(2, 200)])],
    )
}

fn assert_bravo_is_shown(machine: &BackupsMachine, what: &str) {
    let snap = machine.snapshot();
    assert_eq!(
        snap.selected_folder.as_deref(),
        Some("bravo"),
        "{what}: the pick was dropped, and the picker snapped back"
    );
    assert_eq!(
        snap.snapshots.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![2],
        "{what}: the rows under the pick must be bravo's own"
    );
    assert!(
        snap.in_progress_op.is_none(),
        "{what}: the slot is released"
    );
}

#[tokio::test]
async fn a_folder_picked_while_a_refresh_is_in_flight_is_loaded_not_dropped() {
    // A pick is the user's own gesture, not a background re-read, so a refresh
    // already in flight must not swallow it. In a linux whole-suite sweep
    // (2026-09-22) a pick made while one ran was refused, the picker snapped
    // back to the first folder by name, and the test waited on an empty list.
    let (machine, fake) = two_sets_each_with_one_row();
    machine.refresh().await;
    assert_eq!(
        machine.snapshot().selected_folder.as_deref(),
        Some("alpha"),
        "precondition: the default is the first folder by name"
    );

    // The refresh's folder read yields, so the pick lands mid-refresh.
    fake.pause_next_call();
    let m2 = Arc::clone(&machine);
    tokio::join!(machine.refresh(), async move {
        m2.select_folder("bravo".to_string()).await
    });

    assert_bravo_is_shown(&machine, "a pick during a refresh");
}

#[tokio::test]
async fn a_load_never_paints_the_outgoing_folders_rows_under_a_new_pick() {
    // The later window: the in-flight load has already chosen alpha and is
    // fetching alpha's rows when the pick lands. Its reply describes alpha, so
    // committing it under bravo would paint one set's rows under another's name.
    let (machine, fake) = two_sets_each_with_one_row();
    machine.refresh().await;

    fake.pause_next_list_snapshots();
    let m2 = Arc::clone(&machine);
    tokio::join!(machine.refresh(), async move {
        m2.select_folder("bravo".to_string()).await
    });

    assert_bravo_is_shown(&machine, "a pick while alpha's rows were in flight");
}

#[tokio::test]
async fn a_refresh_asked_for_while_an_op_is_in_flight_re_reads_when_it_ends() {
    // The page asks for a re-read when it comes into view; the app also loads it
    // at sign-in. When the sign-in load is still in flight as the page maps, the
    // second refresh used to be dropped by the single-flight gate — and the load
    // that kept the slot had read the folder list BEFORE a set added meanwhile,
    // so the picker never offered that set (a linux sweep, 2026-10-06: four
    // freshly seeded sets "not selectable on the dropdown" for 25 s).
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);

    // The first load has read the folder list and is fetching alpha's rows when
    // a set is added and the second refresh is asked for.
    fake.pause_next_list_snapshots();
    let m2 = Arc::clone(&machine);
    let fake2 = Arc::clone(&fake);
    tokio::join!(machine.refresh(), async move {
        fake2.set_folders(vec![set("alpha"), set("bravo")]);
        m2.refresh().await
    });

    let snap = machine.snapshot();
    assert_eq!(
        snap.folders
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "bravo"],
        "the refresh asked for mid-load re-read the list when the slot freed"
    );
    assert_eq!(
        snap.selected_folder.as_deref(),
        Some("alpha"),
        "a re-read keeps the standing selection"
    );
    assert!(snap.in_progress_op.is_none(), "the slot is released");
}

#[tokio::test]
async fn a_refresh_queued_behind_an_op_keeps_that_ops_result() {
    // The queued re-read is a READ, not a pick: it must not clear the verdict
    // the op it waited behind just produced (a pick does, because the verdict
    // describes the outgoing set; a re-read stays on the same set).
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    fake.set_check_reply(SnapshotCheckReply {
        status: "ok".to_string(),
        ..Default::default()
    });
    machine.refresh().await;

    fake.pause_next_call();
    let m2 = Arc::clone(&machine);
    tokio::join!(machine.check(), async move { m2.refresh().await });

    let snap = machine.snapshot();
    assert!(
        snap.check_result.is_some(),
        "the check's verdict survives the re-read queued behind it"
    );
    assert!(snap.in_progress_op.is_none(), "the slot is released");
}

#[tokio::test]
async fn no_folders_at_all_selects_nothing_and_lists_nothing() {
    let (machine, _fake) = machine_with(vec![], vec![]);
    machine.refresh().await;
    let snap = machine.snapshot();
    assert_eq!(snap.selected_folder, None);
    assert!(snap.snapshots.is_empty());
    assert_eq!(snap.last_backed_up, None, "no sets renders \"never\"");
    assert!(snap.error.is_none(), "an empty account is not an error");
}

// ── `last-backed-up` derivation ─────────────────────────────────

#[tokio::test]
async fn last_backed_up_is_the_newest_row_of_the_selected_set() {
    let (machine, _fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(3, 300), row(2, 200), row(1, 100)])],
    );
    machine.refresh().await;
    assert_eq!(machine.snapshot().last_backed_up, Some(300));
}

#[tokio::test]
async fn last_backed_up_is_none_on_an_empty_set() {
    // linux's live bug: a stale value carried over onto an emptied set.
    let (machine, fake) = machine_with(
        vec![set("alpha"), set("bravo")],
        vec![("alpha", vec![row(1, 100)]), ("bravo", vec![])],
    );
    machine.refresh().await;
    assert_eq!(machine.snapshot().last_backed_up, Some(100));

    machine.select_folder("bravo".to_string()).await;
    assert_eq!(
        machine.snapshot().last_backed_up,
        None,
        "an empty set renders \"never\", it does not inherit the previous set's timestamp"
    );
    let _ = fake;
}

#[tokio::test]
async fn last_backed_up_does_not_go_stale_after_a_create() {
    // windows' live bug: a hand-refreshed label that lags a mutation.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    assert_eq!(machine.snapshot().last_backed_up, Some(100));

    // The nest now serves a newer row; the machine's own create re-reads.
    fake.set_snapshots("alpha", vec![row(2, 500), row(1, 100)]);
    machine.create_snapshot().await;

    assert_eq!(
        machine.snapshot().last_backed_up,
        Some(500),
        "the derivation re-reads with the list; it is never a separately-refreshed label"
    );
}

#[tokio::test]
async fn last_backed_up_ignores_the_selector_row_cached_column() {
    // web's live bug was sourcing this from a *different* column entirely.
    // The selector row may carry a stale/denormalized cache; the selected set's
    // element is derived from real rows, so the two must be free to disagree.
    let mut cached = set("alpha");
    cached.cached_last_snapshot_at = Some(999_999);
    cached.cached_snapshot_count = 42;

    let (machine, _fake) = machine_with(vec![cached], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    let snap = machine.snapshot();

    assert_eq!(
        snap.folders[0].last_snapshot_at,
        Some(999_999),
        "selector row keeps its cache"
    );
    assert_eq!(snap.folders[0].snapshot_count, 42);
    assert_eq!(
        snap.last_backed_up,
        Some(100),
        "the page element comes from the rows, not the cached column"
    );
}

// ── Row lifecycle state ─────────────────────────────────────────

#[tokio::test]
async fn row_state_transcribes_the_four_lifecycle_fields() {
    let active = row(1, 100);

    let mut pending = row(2, 200);
    pending.deletion_pending = true;
    pending.execute_after = Some(48_000);

    let mut soft = row(3, 300);
    soft.soft_deleted = true;
    soft.purge_after = Some(30_000);

    let (machine, _fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![soft, pending, active])],
    );
    machine.refresh().await;
    let rows = machine.snapshot().snapshots;

    assert_eq!(
        rows[0].state,
        SnapshotState::SoftDeleted {
            purge_after: Some(30_000)
        }
    );
    assert_eq!(
        rows[1].state,
        SnapshotState::DeletionPending {
            execute_after: Some(48_000)
        }
    );
    assert_eq!(rows[2].state, SnapshotState::Active);
    // Wire order preserved — apps do not re-sort, so neither does the machine.
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);
}

#[tokio::test]
async fn a_soft_deleted_row_reads_as_soft_deleted_even_if_deletion_pending_is_also_set() {
    // The soft delete has already happened, so any cancellable window it also
    // carries is spent. Reading it the other way would offer a cancel for a
    // deletion that is done.
    let mut both = row(1, 100);
    both.soft_deleted = true;
    both.purge_after = Some(30_000);
    both.deletion_pending = true;
    both.execute_after = Some(48_000);

    let (machine, _fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![both])]);
    machine.refresh().await;

    assert_eq!(
        machine.snapshot().snapshots[0].state,
        SnapshotState::SoftDeleted {
            purge_after: Some(30_000)
        }
    );
}

#[tokio::test]
async fn absent_lifecycle_flags_read_as_active() {
    // Every lifecycle field absent (a current nest's false flags) must render
    // exactly what it renders today, not an empty/unknown state.
    let (machine, _fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    assert_eq!(machine.snapshot().snapshots[0].state, SnapshotState::Active);
}

// ── Check: the shared predicate + per-row implication ───────────

#[tokio::test]
async fn check_implicates_only_the_rows_structured_errors_name() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(3, 300), row(2, 200), row(1, 100)])],
    );
    fake.set_check_reply(SnapshotCheckReply {
        status: "errors".to_string(),
        snapshots_checked: 3,
        missing_chunks: 1,
        structured_errors: vec![SnapshotCheckError {
            kind: "missing_chunk".to_string(),
            snapshot_id: Some(2),
            ..Default::default()
        }],
        ..Default::default()
    });

    machine.refresh().await;
    // Before a check, every row is Unknown — not a plausible default.
    assert!(
        machine
            .snapshot()
            .snapshots
            .iter()
            .all(|r| r.integrity == RowIntegrity::Unknown)
    );

    machine.check().await;
    let snap = machine.snapshot();

    let by_id = |id: i64| {
        snap.snapshots
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .integrity
    };
    assert_eq!(by_id(2), RowIntegrity::Implicated);
    assert_eq!(
        by_id(1),
        RowIntegrity::CheckedOk,
        "the check covered it and found nothing"
    );
    assert_eq!(by_id(3), RowIntegrity::CheckedOk);

    let result = snap
        .check_result
        .expect("a completed check produces a result");
    assert!(!result.is_ok, "the shared is_ok() predicate, called");
    assert_eq!(result.implicated, vec![2]);
}

#[tokio::test]
async fn a_completed_check_with_findings_is_a_result_not_an_error() {
    // Architectural rule 6 — an e2e that reads error_text() as a failure witness
    // depends on this.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    fake.set_check_reply(SnapshotCheckReply {
        status: "errors".to_string(),
        missing_manifests: 2,
        structured_errors: vec![SnapshotCheckError {
            kind: "missing_manifest".to_string(),
            snapshot_id: Some(1),
            ..Default::default()
        }],
        ..Default::default()
    });

    machine.refresh().await;
    machine.check().await;
    let snap = machine.snapshot();

    assert!(snap.check_result.is_some_and(|r| !r.is_ok));
    assert!(
        snap.error.is_none(),
        "findings render in the check-result surface, never in error-message"
    );
}

#[tokio::test]
async fn a_check_that_cannot_run_at_all_is_an_error() {
    // The other side of rule 6: a check that never completed IS a page error.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    fake.fail_check(fauna_backups_machine::BackupsApiError::Unavailable {
        detail: "backup service not configured".to_string(),
    });
    machine.check().await;

    let snap = machine.snapshot();
    assert!(
        snap.error.is_some(),
        "a check that could not run is an error"
    );
    assert!(snap.check_result.is_none(), "and it produces no verdict");
}

#[tokio::test]
async fn a_clean_check_marks_every_row_checked_ok() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(2, 200), row(1, 100)])],
    );
    fake.set_check_reply(SnapshotCheckReply {
        status: "ok".to_string(),
        snapshots_checked: 2,
        ..Default::default()
    });
    machine.refresh().await;
    machine.check().await;

    let snap = machine.snapshot();
    assert!(snap.check_result.is_some_and(|r| r.is_ok));
    assert!(
        snap.snapshots
            .iter()
            .all(|r| r.integrity == RowIntegrity::CheckedOk)
    );
}

#[tokio::test]
async fn the_check_verdict_survives_a_later_refresh() {
    // A create or delete re-reads the list; silently resetting every row to
    // Unknown would make the verdict vanish for no reason the user can see.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    fake.set_check_reply(SnapshotCheckReply {
        status: "errors".to_string(),
        structured_errors: vec![SnapshotCheckError {
            kind: "missing_chunk".to_string(),
            snapshot_id: Some(1),
            ..Default::default()
        }],
        ..Default::default()
    });
    machine.refresh().await;
    machine.check().await;
    assert_eq!(
        machine.snapshot().snapshots[0].integrity,
        RowIntegrity::Implicated
    );

    machine.refresh().await;
    assert_eq!(
        machine.snapshot().snapshots[0].integrity,
        RowIntegrity::Implicated,
        "the session's verdict is re-applied to freshly-read rows"
    );
}

#[tokio::test]
async fn changing_the_selection_drops_the_previous_set_verdict() {
    let (machine, fake) = machine_with(
        vec![set("alpha"), set("bravo")],
        vec![("alpha", vec![row(1, 100)]), ("bravo", vec![row(2, 200)])],
    );
    fake.set_check_reply(SnapshotCheckReply {
        status: "ok".to_string(),
        ..Default::default()
    });
    machine.refresh().await;
    machine.check().await;
    assert!(machine.snapshot().check_result.is_some());

    machine.select_folder("bravo".to_string()).await;
    let snap = machine.snapshot();
    assert!(
        snap.check_result.is_none(),
        "one set's verdict must not render against another set's rows"
    );
    assert!(
        snap.snapshots
            .iter()
            .all(|r| r.integrity == RowIntegrity::Unknown)
    );
}

// ── Prune: preview-first, no client policy ──────────────────────

#[tokio::test]
async fn prune_previews_before_it_executes() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(2, 200), row(1, 100)])],
    );
    fake.set_prune_reply(SnapshotPruneSetPolicyReply {
        dry_run: true,
        pruned: 1,
        remaining: 1,
        snapshots: vec![PrunableSnapshot {
            id: 1,
            created_at: 100,
            tags: Vec::new(),
            extra: Default::default(),
        }],
        policy_state: "applied".to_string(),
        ..Default::default()
    });

    machine.refresh().await;
    fake.clear_calls();
    machine.prune_preview().await;

    let preview = machine.snapshot().prune_preview.expect("preview stands");
    assert_eq!(preview.would_prune, 1);
    assert_eq!(preview.remaining, 1);
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(preview.policy_state, PolicyState::Applied);

    // The preview is a dry run, and it carries no policy — the kind has no
    // policy parameter at all (Architectural rule 5 made structural).
    assert_eq!(
        fake.calls(),
        vec![FakeCall::PruneSetPolicy {
            folder: "alpha".to_string(),
            dry_run: true
        }]
    );
}

#[tokio::test]
async fn prune_execute_without_a_preview_is_a_no_op() {
    // This is what makes preview-first structural rather than a per-app UI
    // convention that six apps could each get wrong.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    fake.clear_calls();

    machine.prune_execute().await;

    assert!(
        fake.calls().is_empty(),
        "no preview standing ⇒ the nest is never asked to prune"
    );
}

#[tokio::test]
async fn prune_execute_after_a_preview_runs_for_real_and_clears_the_preview() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(2, 200), row(1, 100)])],
    );
    fake.set_prune_reply(SnapshotPruneSetPolicyReply {
        pruned: 1,
        remaining: 1,
        policy_state: "applied".to_string(),
        ..Default::default()
    });
    machine.refresh().await;
    machine.prune_preview().await;
    fake.clear_calls();

    machine.prune_execute().await;

    assert!(
        fake.calls().contains(&FakeCall::PruneSetPolicy {
            folder: "alpha".to_string(),
            dry_run: false
        }),
        "the execute leg is the same kind with dry_run = false"
    );
    assert!(
        machine.snapshot().prune_preview.is_none(),
        "an executed preview is spent"
    );
}

#[tokio::test]
async fn cancelling_a_preview_asks_the_nest_nothing() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    fake.set_prune_reply(SnapshotPruneSetPolicyReply {
        policy_state: "applied".to_string(),
        ..Default::default()
    });
    machine.refresh().await;
    machine.prune_preview().await;
    fake.clear_calls();

    machine.cancel_prune_preview();

    assert!(machine.snapshot().prune_preview.is_none());
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn policy_state_transcribes_all_three_arms_and_fails_safe_on_a_new_one() {
    for (wire, expected) in [
        ("applied", PolicyState::Applied),
        ("not_set", PolicyState::NotSet),
        ("unparseable", PolicyState::Unparseable),
        // A value only a newer nest knows must not read as a clean apply.
        ("something_new", PolicyState::Unparseable),
    ] {
        let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
        fake.set_prune_reply(SnapshotPruneSetPolicyReply {
            policy_state: wire.to_string(),
            ..Default::default()
        });
        machine.refresh().await;
        machine.prune_preview().await;

        assert_eq!(
            machine.snapshot().prune_preview.unwrap().policy_state,
            expected,
            "wire policy_state {wire:?}"
        );
    }
}

// ── Create: untagged, with provenance ───────────────────────────

#[tokio::test]
async fn a_manual_create_carries_the_device_id_and_no_tags() {
    // The seam has no `tags` parameter at all — windows' `["manual"]` tag,
    // which silently retention-exempts every manual snapshot, is unrepresentable
    // rather than merely discouraged.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![]);
    machine.set_device_id(Some(vec![0xAB; 32]));
    machine.refresh().await;
    fake.clear_calls();

    machine.create_snapshot().await;

    assert!(fake.calls().contains(&FakeCall::CreateSnapshot {
        folder: "alpha".to_string(),
        device_id: Some(vec![0xAB; 32]),
    }));
}

#[tokio::test]
async fn a_create_with_no_selection_asks_the_nest_nothing() {
    let (machine, fake) = machine_with(vec![], vec![]);
    machine.refresh().await;
    fake.clear_calls();
    machine.create_snapshot().await;
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn a_device_id_is_hex_encoded_for_the_render() {
    let mut r = row(1, 100);
    r.device_id = Some(fauna_protocol::ByteBuf::from(vec![0x01, 0x02, 0xff]));
    let (machine, _fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![r])]);
    machine.refresh().await;
    assert_eq!(
        machine.snapshot().snapshots[0].device_id.as_deref(),
        Some("0102ff")
    );
}

// ── Immediate delete: the friction bar ──────────────────────────

#[tokio::test]
async fn immediate_delete_refuses_unless_both_inputs_match_exactly() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    machine.refresh().await;
    fake.clear_calls();

    // Wrong id.
    machine
        .delete_snapshot_immediate(7, "8".to_string(), IMMEDIATE_DELETE_ACK_TEXT.to_string())
        .await;
    // Wrong acknowledge phrase.
    machine
        .delete_snapshot_immediate(7, "7".to_string(), "delete it".to_string())
        .await;

    assert!(
        fake.calls().is_empty(),
        "rule 4 is a behavioural invariant — a mis-wired client cannot skip it"
    );

    // Both correct.
    machine
        .delete_snapshot_immediate(7, "7".to_string(), IMMEDIATE_DELETE_ACK_TEXT.to_string())
        .await;
    assert!(fake.calls().contains(&FakeCall::DeleteSnapshotImmediate {
        snapshot_id: 7,
        confirm_id: "7".to_string(),
        acknowledge: IMMEDIATE_DELETE_ACK_TEXT.to_string(),
    }));
}

#[tokio::test]
async fn the_immediate_delete_enable_predicate_matches_the_actuation() {
    let (machine, _fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    machine.refresh().await;

    let ack = IMMEDIATE_DELETE_ACK_TEXT.to_string();
    assert!(machine.immediate_delete_enabled("7".into(), "7".into(), ack.clone()));
    assert!(!machine.immediate_delete_enabled("8".into(), "7".into(), ack.clone()));
    assert!(!machine.immediate_delete_enabled("7".into(), "7".into(), "nope".into()));
    // Empty target = no snapshot selected.
    assert!(!machine.immediate_delete_enabled("".into(), "".into(), ack));
}

#[test]
fn the_local_friction_bar_predicate_agrees_with_the_shared_one() {
    // The machine keeps a four-line copy of
    // `fauna_client_snapshots::immediate_delete_button_enabled` so its `default`
    // build stays transport-free (the shared crate pulls the WS-RPC stack). This
    // pin is what makes that copy safe: if either side ever changes, this fails.
    let ack = IMMEDIATE_DELETE_ACK_TEXT;
    for (deleting, confirm, target, typed) in [
        (false, "7", "7", ack),
        (true, "7", "7", ack),
        (false, "8", "7", ack),
        (false, "7", "7", "nope"),
        (false, "", "", ack),
        (false, "7", "", ack),
    ] {
        let fake = Arc::new(FakeBackupsNestApi::new());
        let api: Arc<dyn BackupsNestApi> = fake.clone();
        let machine = BackupsMachine::new(Arc::new(NullObserver), api);
        // `deleting` is machine state, so drive the non-deleting arm through the
        // machine and compare the shared predicate on the same inputs.
        if !deleting {
            assert_eq!(
                machine.immediate_delete_enabled(confirm.into(), target.into(), typed.into()),
                fauna_client_snapshots::immediate_delete_button_enabled(
                    false, confirm, target, typed
                ),
                "local vs shared predicate on ({confirm:?}, {target:?}, {typed:?})"
            );
        }
    }
}

// ── Single flight ───────────────────────────────────────────────

#[tokio::test]
async fn a_second_gesture_is_refused_while_one_is_in_flight() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    fake.set_check_reply(SnapshotCheckReply {
        status: "ok".to_string(),
        ..Default::default()
    });
    machine.refresh().await;
    fake.clear_calls();

    // The create's seam call yields once, so the check is genuinely polled
    // while the create is still in flight.
    fake.pause_next_call();
    let m2 = Arc::clone(&machine);
    tokio::join!(machine.create_snapshot(), async move { m2.check().await });

    let calls = fake.calls();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, FakeCall::CreateSnapshot { .. })),
        "the first gesture ran"
    );
    assert!(
        !calls.iter().any(|c| matches!(c, FakeCall::Check { .. })),
        "the second was refused by the single-flight gate, not queued behind it"
    );
    assert!(
        machine.snapshot().in_progress_op.is_none(),
        "the slot is released when the op finishes"
    );
}

#[tokio::test]
async fn the_in_flight_op_is_visible_to_the_render() {
    // Apps disable every mutating control off this field, so it has to be
    // observable *during* the op, not merely set and cleared.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![]);
    machine.refresh().await;

    fake.pause_next_call();
    let m2 = Arc::clone(&machine);
    let (_, observed) = tokio::join!(machine.create_snapshot(), async move {
        m2.snapshot().in_progress_op
    });

    assert_eq!(observed, Some(BackupOp::Create));
}

// ── Errors ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_failed_load_surfaces_on_the_error_element() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![]);
    fake.fail_list_folders(fauna_backups_machine::BackupsApiError::Transient {
        detail: "connection reset".to_string(),
    });
    machine.refresh().await;

    let err = machine
        .snapshot()
        .error
        .expect("the banner carries the failure");
    assert_eq!(err.key, "backups.error_refresh");
    assert!(
        machine.snapshot().in_progress_op.is_none(),
        "the slot is released on failure too"
    );
}

#[tokio::test]
async fn a_new_gesture_clears_the_previous_failure() {
    // The banner describes the last completed attempt, never a stale one.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![]);
    fake.fail_list_folders(fauna_backups_machine::BackupsApiError::Transient {
        detail: "connection reset".to_string(),
    });
    machine.refresh().await;
    assert!(machine.snapshot().error.is_some());

    machine.refresh().await;
    assert!(
        machine.snapshot().error.is_none(),
        "the retry succeeded, so the banner clears"
    );
}

#[tokio::test]
async fn clear_error_clears_the_banner() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![]);
    fake.fail_list_folders(fauna_backups_machine::BackupsApiError::Transient {
        detail: "boom".to_string(),
    });
    machine.refresh().await;
    assert!(machine.snapshot().error.is_some());
    machine.clear_error();
    assert!(machine.snapshot().error.is_none());
}

// ── The snapshot detail read (`snapshot-detail-files`) ──────────
//
// The detail is the one page read that needs label custody, which is why it
// belongs to the seam and not to each app (`ui/backups.md` § User actions puts
// it on "the machine's custody-wired `SnapshotsClient::get`"). These pins cover
// what an app would otherwise have to re-derive: the id keying, and the two
// ways an open detail can be left describing a snapshot that is no longer there.

fn get_reply(id: i64, files: Vec<(&str, i64, &str)>) -> SnapshotGetReply {
    SnapshotGetReply {
        id,
        files: files
            .into_iter()
            .map(|(path, size_bytes, file_type)| SnapshotFileEntry {
                path: path.to_string(),
                manifest_hash: fauna_protocol::ByteBuf::from(vec![0xab; 32]),
                size_bytes,
                file_type: file_type.to_string(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn opening_a_snapshot_reads_its_file_list_through_the_custody_wired_seam() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    fake.clear_calls();

    machine.open_snapshot(7).await;

    assert_eq!(
        fake.calls(),
        vec![FakeCall::GetSnapshot { snapshot_id: 7 }],
        "the detail is ONE seam call — the app never reaches the nest itself"
    );
    let detail = machine.snapshot().detail.expect("detail is open");
    assert_eq!(detail.snapshot_id, 7);
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].path, "docs/a.txt");
    assert_eq!(detail.files[0].size_bytes, 12);
    assert_eq!(
        detail.files[0].manifest_hash,
        "ab".repeat(32),
        "the download walk's key crosses as hex, like SnapshotRow::device_id"
    );
}

#[tokio::test]
async fn the_detail_is_keyed_to_the_row_clicked_not_to_the_reply() {
    // A reply whose `id` disagrees with the request must NOT re-point the open
    // pane: the user clicked row 7, so the detail is row 7's or it is nothing.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    fake.set_get_reply(7, get_reply(999, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;

    machine.open_snapshot(7).await;

    assert_eq!(machine.snapshot().detail.unwrap().snapshot_id, 7);
}

#[tokio::test]
async fn selecting_another_set_closes_the_open_detail() {
    // Carrying it across would paint one set's file list under another set's rows.
    let (machine, fake) = machine_with(
        vec![set("alpha"), set("beta")],
        vec![("alpha", vec![row(7, 100)]), ("beta", vec![row(9, 200)])],
    );
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    machine.open_snapshot(7).await;
    assert!(machine.snapshot().detail.is_some());

    machine.select_folder("beta".to_string()).await;

    assert!(
        machine.snapshot().detail.is_none(),
        "the detail belonged to alpha's snapshot"
    );
}

#[tokio::test]
async fn deleting_the_open_snapshot_closes_its_detail() {
    // The sharp edge: an immediate delete of the OPEN row leaves a file list
    // whose per-file download buttons point at manifests the nest has dropped.
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(7, 100), row(8, 200)])],
    );
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    machine.open_snapshot(7).await;
    assert!(machine.snapshot().detail.is_some());

    // The nest now lists only row 8 — row 7 is gone.
    fake.set_snapshots("alpha", vec![row(8, 200)]);
    machine
        .delete_snapshot_immediate(7, "7".to_string(), IMMEDIATE_DELETE_ACK_TEXT.to_string())
        .await;

    assert!(
        machine.snapshot().detail.is_none(),
        "an open detail whose snapshot is no longer listed is closed, not rendered"
    );
}

#[tokio::test]
async fn a_refresh_that_still_lists_the_open_snapshot_keeps_its_detail() {
    // The negative of the rule above — a create must not close an open file list.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    machine.open_snapshot(7).await;

    fake.set_snapshots("alpha", vec![row(8, 300), row(7, 100)]);
    machine.create_snapshot().await;

    assert_eq!(
        machine.snapshot().detail.expect("still open").snapshot_id,
        7
    );
}

#[tokio::test]
async fn a_detail_read_that_fails_is_an_error_and_opens_nothing() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    machine.refresh().await;
    fake.fail_get(fauna_backups_machine::BackupsApiError::NotFound {
        detail: "no such snapshot".to_string(),
    });

    machine.open_snapshot(7).await;

    let snap = machine.snapshot();
    assert!(snap.detail.is_none(), "a failed read opens no pane");
    assert!(
        snap.error.is_some(),
        "and it IS an error, unlike a check verdict"
    );
}

#[tokio::test]
async fn closing_the_detail_asks_the_nest_nothing() {
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    machine.open_snapshot(7).await;
    fake.clear_calls();

    machine.close_snapshot_detail();

    assert!(machine.snapshot().detail.is_none());
    assert!(fake.calls().is_empty(), "close is local state only");
}

#[tokio::test]
async fn opening_a_detail_takes_the_single_flight_slot() {
    // Otherwise a detail read could land against a pre-create population.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(7, 100)])]);
    fake.set_get_reply(7, get_reply(7, vec![("docs/a.txt", 12, "regular")]));
    machine.refresh().await;
    fake.clear_calls();
    fake.pause_next_call();

    let (_, _) = tokio::join!(machine.open_snapshot(7), machine.create_snapshot());

    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::CreateSnapshot { .. }))
            .count(),
        0,
        "the create was refused while the detail read held the slot"
    );
    assert!(machine.snapshot().in_progress_op.is_none(), "slot released");
}

// ── Undelete (§ Snapshot-list shape, *Soft-deleted rows* ruling) ─

/// A wire row inside its 30-day recovery window.
fn soft_deleted(id: i64, created_at: i64) -> SnapshotSummaryRow {
    SnapshotSummaryRow {
        soft_deleted: true,
        purge_after: Some(created_at + 1_000),
        ..row(id, created_at)
    }
}

#[tokio::test]
async fn undelete_recovers_a_soft_deleted_row() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![row(2, 200), soft_deleted(1, 100)])],
    );
    machine.refresh().await;
    assert_eq!(
        machine.snapshot().snapshots[1].state,
        SnapshotState::SoftDeleted {
            purge_after: Some(1_100)
        },
        "precondition: the row arrived soft-deleted"
    );
    fake.clear_calls();

    // The nest recovers the row; the machine's own undelete re-reads.
    fake.set_snapshots("alpha", vec![row(2, 200), row(1, 100)]);
    machine.undelete_snapshot(1).await;

    let calls = fake.calls();
    assert_eq!(
        calls.first(),
        Some(&FakeCall::UndeleteSnapshot { snapshot_id: 1 }),
        "the gesture reached the nest with the clicked row's id"
    );
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, FakeCall::ListSnapshots { .. })),
        "a successful undelete re-reads the list rather than patching local state"
    );
    assert_eq!(
        machine.snapshot().snapshots[1].state,
        SnapshotState::Active,
        "the recovered row renders Active from the re-read"
    );
    assert!(machine.snapshot().error.is_none());
}

#[tokio::test]
async fn undelete_is_refused_unless_the_row_is_soft_deleted() {
    // The affordance renders only on SoftDeleted rows; the machine enforces the
    // rule structurally rather than trusting each app's render — the same shape
    // as prune_execute's no-preview no-op.
    let (machine, fake) = machine_with(vec![set("alpha")], vec![("alpha", vec![row(1, 100)])]);
    machine.refresh().await;
    fake.clear_calls();

    machine.undelete_snapshot(1).await;
    machine.undelete_snapshot(99).await;

    assert!(
        fake.calls().is_empty(),
        "neither an Active row's undelete nor an unknown id ever reaches the nest"
    );
}

#[tokio::test]
async fn undelete_takes_the_single_flight_slot() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![soft_deleted(1, 100)])],
    );
    machine.refresh().await;
    fake.clear_calls();
    fake.pause_next_call();

    let m2 = Arc::clone(&machine);
    tokio::join!(machine.undelete_snapshot(1), async move {
        m2.create_snapshot().await
    });

    let calls = fake.calls();
    assert_eq!(
        calls.first(),
        Some(&FakeCall::UndeleteSnapshot { snapshot_id: 1 }),
        "the undelete ran"
    );
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, FakeCall::CreateSnapshot { .. })),
        "the create was refused while the undelete held the slot"
    );
    assert!(machine.snapshot().in_progress_op.is_none(), "slot released");
}

#[tokio::test]
async fn a_failed_undelete_surfaces_on_the_error_element() {
    let (machine, fake) = machine_with(
        vec![set("alpha")],
        vec![("alpha", vec![soft_deleted(1, 100)])],
    );
    machine.refresh().await;
    fake.fail_undelete(fauna_backups_machine::BackupsApiError::Conflict {
        detail: "already purged".to_string(),
    });

    machine.undelete_snapshot(1).await;

    let err = machine
        .snapshot()
        .error
        .expect("the banner carries the failure");
    assert_eq!(err.key, "backups.error_undelete_snapshot");
    assert_eq!(
        machine.snapshot().snapshots[0].state,
        SnapshotState::SoftDeleted {
            purge_after: Some(1_100)
        },
        "a failed undelete leaves the row as the nest last served it"
    );
    assert!(machine.snapshot().in_progress_op.is_none(), "slot released");
}
