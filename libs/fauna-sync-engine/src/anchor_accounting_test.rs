//! The anchor's accounting law, pinned at the apply loop
//! ([`SyncEngine::apply_remote_changes`]).
//!
//! The anchor asserts *"every row at or below me is applied or deliberately
//! skipped"* (`docs/goal/behavior/file-sync.md` § the anchor). Every cell here
//! is a way that assertion was false, in one direction or the other:
//!
//! - **Past what it never read.** A failed download logged and `continue`d, and
//!   the anchor then advanced over the row — dropping a peer's change on this
//!   device for good, and (worse) licensing every later local edit to claim it
//!   had incorporated the row it never read.
//! - **Never past what can never open.** A sealed path that did not open held
//!   the anchor below it *forever*, for any reason whatsoever — so a malformed
//!   envelope was indistinguishable from "key material is still syncing", and
//!   since the nest cannot check a seal, ONE member's bad row froze every other
//!   member's catch-up on a shared set.
//! - **An opened path was not bound to its row.** A writer holding the set's
//!   label key could seal path P under Q's hash; the AEAD binds the salt, not
//!   the plaintext, so every device wrote P while the nest's per-path heads,
//!   conflicts and history recorded Q.
//!
//! Pure db + filesystem work against hand-built fixtures — the rows here either
//! carry no openable path (so no consumer downloads them) or are deletes.

use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::label_custody;
use fauna_core::path_crypto::{LabelField, LabelRoot};
use fauna_protocol::sync::SyncChange;

use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::{our_device_hex, seed_tracked_file, test_engine_with_keys};

/// The owner key the engine under test holds — and therefore the one root its
/// `label_open_roots` offers.
fn held_key() -> BackupKey {
    BackupKey::from_bytes([11u8; 32])
}

/// A key the engine does **not** hold: a seal under it can never open here, but
/// might once key material catches up. The transient class.
fn foreign_key() -> BackupKey {
    BackupKey::from_bytes([22u8; 32])
}

fn engine_holding_the_key(watch: &std::path::Path) -> SyncEngine {
    test_engine_with_keys(
        watch.to_path_buf(),
        Some(OwnerSealKey::Client(held_key())),
        None,
        None,
    )
}

/// A post-flip `changes.list` row: no plaintext `path`, a sealed label, and the
/// row's own `path_hash` as the wire salt.
fn sealed_row(seq: i64, change_type: &str, path_sealed: Vec<u8>, path_hash: String) -> SyncChange {
    SyncChange {
        seq,
        path_hash,
        manifest_hash: None,
        size_bytes: 0,
        change_type: change_type.to_string(),
        created_at: 1_700_000_000_000,
        path: None,
        path_sealed: Some(path_sealed.into()),
        device_id: Some("peerpeerpeer".to_string()),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    }
}

/// A well-formed sealed row for `path`, sealed under `root`.
fn sealed_row_for(seq: i64, change_type: &str, root: &LabelRoot, path: &str) -> SyncChange {
    sealed_row(
        seq,
        change_type,
        label_custody::seal_path(root, path).unwrap(),
        hex::encode(fauna_core::sync::path_hash(path)),
    )
}

/// Drive the apply loop the way production reaches it: `fetch_changes` opens
/// every sealed path before handing the batch to `apply_remote_changes`, so a
/// test that skips that step exercises nothing but the path-less degrade.
async fn open_then_apply(
    engine: &SyncEngine,
    mut changes: Vec<SyncChange>,
    since: i64,
) -> crate::engine::AppliedBatch {
    engine.open_sealed_change_paths(&mut changes);
    engine.apply_remote_changes(&changes, since).await.unwrap()
}

fn catchup_failures(engine: &SyncEngine) -> Vec<(String, Option<String>)> {
    engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|(_, _, kind, _, _)| kind == "catchup_failed")
        .map(|(_, path, _, details, _)| (path, details))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────
// Never past what can never open — the two-class split (Do 3)
// ─────────────────────────────────────────────────────────────────────

/// The class that MUST still freeze the batch: a seal under a root this holder
/// does not hold yet. An M2 generation can lag its changes, so advancing past
/// it would lose that file on this device forever.
#[tokio::test]
async fn a_seal_this_holder_cannot_open_yet_still_holds_the_anchor() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "mine.txt", b"yadayada");

    let mine = LabelRoot::owner_of(&held_key());
    let theirs = LabelRoot::owner_of(&foreign_key());
    let changes = vec![
        sealed_row_for(70, "delete", &mine, "mine.txt"),
        sealed_row_for(71, "create", &theirs, "not-for-me.txt"),
    ];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(
        batch.deferred,
        "an unopenable-yet seal must defer the batch"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "the anchor must stay BELOW the row whose key material may still arrive"
    );
    assert!(
        catchup_failures(&engine).is_empty(),
        "a transient class must not be filed as permanently un-appliable"
    );
}

/// ⚠ The freeze this split exists to end. A `path_sealed` blob that is not a
/// decodable envelope can never open under **any** key, so the old
/// one-class rule held the anchor below it on every pull, for ever — and
/// because the nest cannot check a seal, one member's malformed row stranded
/// every other member of a shared set.
#[tokio::test]
async fn a_malformed_envelope_is_recorded_and_the_anchor_advances() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "mine.txt", b"yadayada");

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![
        sealed_row(
            70,
            "create",
            b"not a cbor envelope".to_vec(),
            hex::encode(fauna_core::sync::path_hash("whatever.txt")),
        ),
        sealed_row_for(71, "delete", &mine, "mine.txt"),
    ];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(
        !batch.deferred,
        "a permanently un-appliable row must not defer the batch"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        71,
        "the anchor must move PAST a row no key could ever open — and the rows \
         after it must apply"
    );
    assert!(
        !watch.path().join("mine.txt").exists(),
        "the row after the bad one must still apply: that is the whole point"
    );
    let failures = catchup_failures(&engine);
    assert_eq!(failures.len(), 1, "the skip must be recorded, never silent");
    assert!(
        failures[0].1.as_deref().unwrap().contains("envelope"),
        "the recorded reason must name the class: {failures:?}"
    );
}

/// The other locally-decidable half: a row whose `path_hash` is not 32 hex
/// bytes has no reconstructable salt, and the apply path has no plaintext to
/// fall back on.
#[tokio::test]
async fn a_malformed_row_hash_is_recorded_and_the_anchor_advances() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![sealed_row(
        70,
        "create",
        label_custody::seal_path(&mine, "photos/a.jpg").unwrap(),
        "not-hex".to_string(),
    )];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(!batch.deferred);
    assert_eq!(engine.db().get_anchor().unwrap(), 70);
    assert_eq!(catchup_failures(&engine).len(), 1);
}

/// The third locally-decidable half: a row with **no seal at all** and no
/// plaintext `path`. No current writer lands one on a plane whose plaintext
/// scrubs (the nest refuses a seal-less record there), and a plaintext-resting
/// plane serves its `path` — so the shape names a row no applier anywhere can
/// use. It is the `NoSeal` refusal: recorded like the other two, then advanced
/// past. It used to be a SILENT skip-and-advance, kept for the pre-expand
/// hash-only rows the compat-remnant sweep's baseline reset retired.
#[tokio::test]
async fn a_seal_less_row_is_recorded_as_refused_and_the_anchor_advances() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "mine.txt", b"yadayada");

    let mine = LabelRoot::owner_of(&held_key());
    let mut seal_less = sealed_row(
        70,
        "create",
        Vec::new(),
        hex::encode(fauna_core::sync::path_hash("whatever.txt")),
    );
    seal_less.path_sealed = None;
    let changes = vec![seal_less, sealed_row_for(71, "delete", &mine, "mine.txt")];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(
        !batch.deferred,
        "a seal-less row is permanent, never a hold"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        71,
        "the anchor must move past the seal-less row and the rows after it must apply"
    );
    assert!(
        !watch.path().join("mine.txt").exists(),
        "the row after the seal-less one must still apply"
    );
    let failures = catchup_failures(&engine);
    assert_eq!(
        failures.len(),
        1,
        "the skip must be RECORDED, never silent: {failures:?}"
    );
    assert!(
        failures[0].1.as_deref().unwrap().contains("neither"),
        "the recorded reason must name the class: {failures:?}"
    );
}

/// A refused row ABOVE a live cap is not accounted for this pass, so it must
/// not be recorded yet — it re-lists on the next pull and would otherwise
/// collect a duplicate conflict row on every one.
#[tokio::test]
async fn a_refusal_above_the_cap_waits_for_the_pass_that_accounts_for_it() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let theirs = LabelRoot::owner_of(&foreign_key());
    let changes = vec![
        sealed_row_for(70, "create", &theirs, "not-for-me.txt"),
        sealed_row(
            71,
            "create",
            b"not a cbor envelope".to_vec(),
            hex::encode(fauna_core::sync::path_hash("whatever.txt")),
        ),
    ];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(batch.deferred);
    assert_eq!(engine.db().get_anchor().unwrap(), 69, "capped at row 70");
    assert!(
        catchup_failures(&engine).is_empty(),
        "nothing at or above the cap is accounted for yet"
    );
}

// ─────────────────────────────────────────────────────────────────────
// An opened path is bound to its row (Do 4)
// ─────────────────────────────────────────────────────────────────────

/// ⚠ The impersonation this binds shut. A writer holding the set's label key
/// seals path P under Q's hash. The AEAD binds the *salt*, so the envelope
/// opens perfectly and the old apply path wrote P — while the nest's per-path
/// heads, conflicts and history all recorded Q.
#[tokio::test]
async fn a_path_sealed_under_another_rows_hash_never_reaches_the_disk() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "victim.txt", b"precious");

    // Row 70 is filed by the nest under `decoy.txt`'s hash, but its seal opens
    // to `victim.txt` — a delete that would remove a file the row never named.
    let mine = LabelRoot::owner_of(&held_key());
    let impersonation = fauna_core::path_crypto::seal_convergent(
        &mine,
        &fauna_core::sync::path_hash("decoy.txt"),
        LabelField::SyncChangePath,
        b"victim.txt",
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    let changes = vec![sealed_row(
        70,
        "delete",
        impersonation,
        hex::encode(fauna_core::sync::path_hash("decoy.txt")),
    )];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(
        watch.path().join("victim.txt").exists(),
        "a row filed under one path must never act on another"
    );
    assert!(
        !batch.deferred,
        "an unbindable row can never bind — permanent"
    );
    assert_eq!(engine.db().get_anchor().unwrap(), 70);
    let failures = catchup_failures(&engine);
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].1.as_deref().unwrap().contains("hash"),
        "the recorded reason must name the binding failure: {failures:?}"
    );
}

/// The honest row still opens and applies — the binding check must not cost a
/// well-formed seal.
#[tokio::test]
async fn a_bound_seal_applies_exactly_as_before() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "gone.txt", b"yadayada");

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![sealed_row_for(70, "delete", &mine, "gone.txt")];

    open_then_apply(&engine, changes, 69).await;

    assert!(!watch.path().join("gone.txt").exists());
    assert_eq!(engine.db().get_anchor().unwrap(), 70);
    assert!(catchup_failures(&engine).is_empty());
}

/// A self-echoed row is bound the same way: the fold that reads an own echo's
/// name runs through the same opener, so an unbound own row yields no name
/// rather than a wrong one.
#[tokio::test]
async fn an_unbound_self_echo_yields_no_path() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mine = LabelRoot::owner_of(&held_key());
    let impersonation = fauna_core::path_crypto::seal_convergent(
        &mine,
        &fauna_core::sync::path_hash("decoy.txt"),
        LabelField::SyncChangePath,
        b"victim.txt",
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    let mut row = sealed_row(
        70,
        "create",
        impersonation,
        hex::encode(fauna_core::sync::path_hash("decoy.txt")),
    );
    row.device_id = Some(our_device_hex());

    let mut rows = vec![row];
    engine.open_sealed_change_paths(&mut rows);
    assert_eq!(
        rows[0].path, None,
        "an unbound seal must never become a path, own row or peer's"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Past what it never read — the failed download (Do 1)
// ─────────────────────────────────────────────────────────────────────

/// A sealed create row whose bytes this engine will try, and fail, to fetch.
/// [`test_engine_with_keys`] points at an unreachable URL, so the fetch fails
/// the way a network blip fails: transiently.
fn sealed_create(seq: i64, root: &LabelRoot, path: &str, fill: u8) -> SyncChange {
    let mut row = sealed_row_for(seq, "create", root, path);
    row.manifest_hash = Some(hex::encode([fill; 32]));
    row.size_bytes = 11;
    row
}

/// ⚠ The lost change. A failed download used to log and `continue`, and the
/// anchor at the end of the pass advanced past the row anyway — so one blip
/// mid-fetch dropped a peer's change on this device permanently. It is a
/// transient failure, which the ruling says holds the anchor.
#[tokio::test]
async fn a_failed_download_holds_the_anchor_instead_of_advancing_past_it() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![sealed_create(70, &mine, "from-peer.txt", 0xAB)];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert_eq!(
        batch.applied, 0,
        "nothing was downloaded, so nothing applied"
    );
    assert!(
        batch.deferred,
        "a transient failure must report the pass unfinished — something has to \
         re-drive the retry"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        69,
        "the anchor must NOT move past a row this device never read"
    );
    assert!(
        catchup_failures(&engine).is_empty(),
        "a network failure is not a permanent verdict about the change"
    );
}

/// …and it must hold the anchor below the FIRST failure, not merely below the
/// batch: the rows under it are applied and accounted for, the rest re-list.
#[tokio::test]
async fn the_hold_is_at_the_first_failure_not_the_whole_batch() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    seed_tracked_file(&engine, watch.path(), "gone.txt", b"yadayada");

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![
        sealed_row_for(70, "delete", &mine, "gone.txt"),
        sealed_create(71, &mine, "from-peer.txt", 0xAB),
        sealed_create(72, &mine, "also-from-peer.txt", 0xCD),
    ];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert!(!watch.path().join("gone.txt").exists(), "row 70 applied");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "everything below the first failure is accounted for; 71 and 72 re-list"
    );
    assert!(batch.deferred);
}

/// The other half of the split: a failure that recurs identically on every
/// later pull must NOT hold the anchor, or one bad row strands every change
/// after it (measured live 2026-07-31). Here the row names a path outside the
/// sync root — a property of the row itself, so no later pull changes it.
#[tokio::test]
async fn a_permanently_unappliable_change_is_recorded_and_the_anchor_advances() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mine = LabelRoot::owner_of(&held_key());
    let changes = vec![sealed_create(70, &mine, "../outside-the-root.txt", 0xAB)];

    let batch = open_then_apply(&engine, changes, 69).await;

    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "a row no pull can ever apply must not freeze the ones after it"
    );
    assert!(!batch.deferred);
    let failures = catchup_failures(&engine);
    assert_eq!(failures.len(), 1, "the skip must be recorded, never silent");
    assert!(
        failures[0].1.as_deref().unwrap().contains("sync root"),
        "the recorded reason must name the class: {failures:?}"
    );
}

/// The classifier's default is what protects user data: an error nobody
/// classified is TRANSIENT, so an unforeseen failure costs a retry rather than
/// a silently dropped change.
#[test]
fn an_unclassified_failure_defaults_to_transient() {
    use fauna_core::apply_failure::permanent_reason;
    assert_eq!(
        permanent_reason(&anyhow::anyhow!("something nobody has classified yet")),
        None
    );
}

// ─────────────────────────────────────────────────────────────────────
// The edit stamp claims only what this device incorporated (Do 2)
// ─────────────────────────────────────────────────────────────────────

/// The anchor over-reaching by one row is not a rounding error: it is the
/// whole lost-edit defect. A device that skipped seq 100 on a path and then
/// stamps an edit `derived_through = 100` tells every peer "I incorporated
/// your row" — and a peer whose file still matches its base obeys, replacing
/// its own unread work with no conflict row.
#[test]
fn a_permanent_skip_pins_that_paths_claim_below_it() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    let hash = hex::encode(fauna_core::sync::path_hash(path));

    assert_eq!(
        causal.honest_anchor(path, 200),
        200,
        "with nothing skipped the anchor IS the honest claim"
    );

    causal.note_permanent_skip(Some(&hash), 100);
    assert_eq!(
        causal.honest_anchor(path, 200),
        99,
        "the claim must stop below the row this device never read"
    );
}

/// …and only that path's. A set-wide ceiling would tax every edit on the
/// device for ever after one bad row — the extra merge round the law prices in
/// is per path, not per device.
#[test]
fn a_skip_on_one_path_does_not_pin_another() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    causal.note_permanent_skip(
        Some(&hex::encode(fauna_core::sync::path_hash("photos/a.jpg"))),
        100,
    );

    assert_eq!(causal.honest_anchor("notes/b.md", 200), 200);
    assert_eq!(causal.honest_anchor("photos/a.jpg", 200), 99);
}

/// The release, without a clearing pass: once the path's own frontier reaches
/// the skipped seq, this device's content for it reflects a row at or past the
/// skip in nest-log order, so claiming over it loses a peer nothing it should
/// have kept.
#[test]
fn the_pin_lifts_once_a_later_row_on_that_path_lands() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    causal.note_permanent_skip(Some(&hex::encode(fauna_core::sync::path_hash(path))), 100);
    assert_eq!(causal.honest_anchor(path, 200), 99);

    causal.advance_frontier(path, 99);
    assert_eq!(
        causal.honest_anchor(path, 200),
        99,
        "a frontier still BELOW the skip does not release it"
    );

    causal.advance_frontier(path, 150);
    assert_eq!(
        causal.honest_anchor(path, 200),
        200,
        "content reflecting seq 150 supersedes the row skipped at 100"
    );
}

/// The floor keeps the LOWEST skip: the claim has to be honest about the
/// earliest row this device is missing, not the most recent.
#[test]
fn the_floor_keeps_the_earliest_skip() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    let hash = hex::encode(fauna_core::sync::path_hash(path));

    causal.note_permanent_skip(Some(&hash), 150);
    causal.note_permanent_skip(Some(&hash), 100);
    causal.note_permanent_skip(Some(&hash), 175);

    assert_eq!(causal.honest_anchor(path, 200), 99);
}

/// ⚠ **The floor is the earliest LIVE skip, not the earliest ever recorded**
/// . A floor that held one seq
/// kept the lowest skip and dropped every later one as "already covered", so
/// once the path's frontier released that lowest skip nothing remembered a
/// skip recorded after it. Shape A: skip P@100, the frontier reaches 150 (100
/// releases), then skip P@300 — every claim on P must stop at 299.
#[test]
fn a_skip_recorded_after_a_release_still_pins_the_claim() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    let hash = hex::encode(fauna_core::sync::path_hash(path));

    causal.note_permanent_skip(Some(&hash), 100);
    causal.advance_frontier(path, 150);
    assert_eq!(causal.honest_anchor(path, 200), 200, "100 has released");

    causal.note_permanent_skip(Some(&hash), 300);
    assert_eq!(
        causal.honest_anchor(path, 400),
        299,
        "the edit and resolution stamps stop below the row never read"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 400, true),
        299,
        "and so does a winner claim widened over own gaps"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 151, false),
        151,
        "a contiguous claim below the live skip is untouched"
    );

    causal.advance_frontier(path, 300);
    assert_eq!(
        causal.honest_anchor(path, 400),
        400,
        "the later skip releases the same way the first did"
    );
}

/// …and Shape B: two skips live at once, and a release that passes only the
/// lower one must leave the higher one governing — whichever order they were
/// recorded in.
#[test]
fn a_release_past_the_lower_skip_leaves_the_higher_one_live() {
    for order in [[100, 300], [300, 100]] {
        let dir = tempfile::tempdir().unwrap();
        let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
        let path = "photos/a.jpg";
        let hash = hex::encode(fauna_core::sync::path_hash(path));

        for seq in order {
            causal.note_permanent_skip(Some(&hash), seq);
        }
        assert_eq!(causal.honest_anchor(path, 400), 99, "{order:?}");

        causal.advance_frontier(path, 150);
        assert_eq!(
            causal.honest_anchor(path, 400),
            299,
            "{order:?}: the release of 100 must not forget 300"
        );
        assert_eq!(
            causal.honest_winner_claim(path, 400, true),
            299,
            "{order:?}: the winner claim reads the same live skip"
        );
    }
}

/// A per-path floor that could not be read is not overwritten into a
/// readable one that FORGETS it: the lost seq may be live, and the per-path
/// degradation (claim ≤ the path's frontier) is no strand — it climbs as the
/// path does. So a new skip keeps the loss on record rather than replacing it,
/// unlike the set-wide floor, whose unreadable state pins every claim on the
/// device and is replaced (see `a_truncated_floor_file_is_unreadable_not_absent`).
#[test]
fn a_new_skip_over_an_unreadable_per_path_floor_keeps_the_loss() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    let hash = fauna_core::sync::path_hash(path);
    causal.advance_frontier(path, 150);
    std::fs::write(
        dir.path().join(format!("skipfloor-{}", hex::encode(hash))),
        "",
    )
    .unwrap();

    causal.note_permanent_skip(Some(&hex::encode(hash)), 300);
    assert_eq!(
        causal.honest_anchor(path, 400),
        150,
        "the lost seq could sit anywhere above the frontier"
    );
    causal.advance_frontier(path, 350);
    assert_eq!(
        causal.honest_anchor(path, 400),
        350,
        "the degradation climbs with the path, never strands it"
    );
}

/// ⚠ **A floor that cannot be READ is not the absence of a skip.** The file
/// exists only because a skip was recorded under that key, so folding an
/// unreadable one into "no floor" restores the raw over-claiming anchor — the
/// very defect the reduction exists to close — and used to do it silently,
/// while the write side had always logged its own fail-open loudly.
#[test]
fn an_unreadable_floor_never_restores_the_raw_claim() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    causal.advance_frontier(path, 150);

    // A per-path floor whose seq is lost still admits a provable bound: the
    // floor had either released (`frontier ≥ floor`) or was live
    // (`floor > frontier`, so `floor − 1 ≥ frontier`). The path's own frontier
    // is honest under both — and climbs as the path does, so this is a
    // degradation, not a strand.
    let per_path = dir.path().join(format!(
        "skipfloor-{}",
        hex::encode(fauna_core::sync::path_hash(path))
    ));
    std::fs::create_dir_all(&per_path).unwrap();
    assert_eq!(
        causal.honest_anchor(path, 200),
        150,
        "an unreadable per-path floor claims no further than the path's frontier"
    );
    assert_eq!(
        causal.honest_anchor("notes/b.md", 200),
        200,
        "and still only that path pays for it"
    );

    // The set-wide floor has no release at all, so an unknown value admits no
    // bound above 0: the lost seq could be the very first row.
    std::fs::create_dir_all(dir.path().join("skipfloor-set")).unwrap();
    assert_eq!(
        causal.honest_anchor("notes/b.md", 200),
        0,
        "an unreadable set-wide floor pins every claim on the device"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 400, false),
        0,
        "the winner claim included — it reads the same floor"
    );
}

/// A floor file truncated to nothing — the state a crash between
/// `std::fs::write`'s truncate and its write used to leave — reads as
/// unreadable, never as "no skip". The write is now temp-plus-rename, so our
/// own crashes cannot produce it; external damage still can.
#[test]
fn a_truncated_floor_file_is_unreadable_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    causal.note_permanent_skip(None, 100);
    assert_eq!(causal.honest_anchor(path, 200), 99);

    std::fs::write(dir.path().join("skipfloor-set"), "").unwrap();
    assert_eq!(
        causal.honest_anchor(path, 200),
        0,
        "an empty floor file is a lost seq, not a cleared one"
    );

    // Recording a fresh skip over it restores a readable floor — logged loudly,
    // because an EARLIER skip's seq may have been what was destroyed.
    causal.note_permanent_skip(None, 120);
    assert_eq!(causal.honest_anchor(path, 200), 119);
}

/// The one row shape with no usable per-path key — a `path_hash` that is not
/// even 32 hex bytes — takes a set-wide floor, because the alternative is
/// over-claiming for whichever path it turns out to have been.
#[test]
fn an_unkeyable_skip_pins_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    causal.note_permanent_skip(None, 100);

    assert_eq!(causal.honest_anchor("photos/a.jpg", 200), 99);
    assert_eq!(causal.honest_anchor("notes/b.md", 200), 99);
}

/// ⚠ The skip floor's key is the one store key taken from the WIRE — the
/// nest-served `path_hash` — so it must never name a file outside the store
/// . A hostile nest serving `x/../../escape` must not
/// get a nest-chosen integer written wherever that resolves (on Windows `..`
/// resolves lexically, so no intermediate directory need exist; here the
/// `skipfloor-x` directory is planted so the traversal resolves on any OS).
/// Anything that is not 32 hex bytes is the malformed shape `conflicts.md` §
/// the causal watermark assigns the set-wide floor.
#[test]
fn a_nest_served_key_never_names_a_file_outside_the_store() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("store");
    std::fs::create_dir_all(store.join("skipfloor-x")).unwrap();
    let causal = crate::causal::CausalStore::new(store.clone());

    causal.note_permanent_skip(Some("x/../../escape"), 5);
    causal.note_permanent_skip(Some("x\\..\\..\\escape"), 6);
    causal.note_permanent_skip(Some("zz"), 7);

    assert!(
        !root.path().join("escape").exists(),
        "a wire key must never write outside the store"
    );
    let mut written: Vec<String> = std::fs::read_dir(&store)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "skipfloor-x")
        .collect();
    written.sort();
    assert_eq!(
        written,
        vec!["skipfloor-set".to_string()],
        "every malformed key lands on the one set-wide floor"
    );
    assert_eq!(
        causal.honest_anchor("photos/a.jpg", 200),
        4,
        "the set-wide floor is recorded at the earliest malformed skip"
    );
}

/// …and a well-formed key still takes the per-path floor, normalized to the
/// lowercase form the readers derive locally (`hex_of`) — an uppercase key
/// from the nest names the same path, not a file no reader ever opens.
#[test]
fn a_well_formed_wire_key_takes_the_per_path_floor_whatever_its_case() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    let upper = hex::encode_upper(fauna_core::sync::path_hash(path));

    causal.note_permanent_skip(Some(&upper), 100);

    assert_eq!(causal.honest_anchor(path, 200), 99);
    assert_eq!(
        causal.honest_anchor("notes/b.md", 200),
        200,
        "a usable key is per path, never set-wide"
    );
}

/// End to end: a seal-less row (the `NoSeal` shape — the easiest one for a
/// hostile nest to serve) carrying a malformed `path_hash` is refused and
/// accounted for on the set-wide floor, never keyed by its wire string.
#[tokio::test]
async fn a_seal_less_row_with_a_malformed_hash_lands_on_the_set_wide_floor() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mut hostile = sealed_row(70, "create", Vec::new(), "x/../../escape".to_string());
    hostile.path_sealed = None;

    open_then_apply(&engine, vec![hostile], 69).await;

    assert_eq!(engine.db().get_anchor().unwrap(), 70);
    assert_eq!(
        engine.causal().honest_anchor("anything.txt", 70),
        69,
        "an unkeyable skip pins every path's claim below it"
    );
}

/// End to end through the apply loop: the permanently-refused row of
/// [`a_malformed_envelope_is_recorded_and_the_anchor_advances`] must leave the
/// claim pinned, so the anchor's advance past it is never lent to an edit.
#[tokio::test]
async fn a_refused_row_pins_the_claim_it_was_accounted_for_with() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let victim = "photos/a.jpg";
    let changes = vec![sealed_row(
        70,
        "create",
        b"not a cbor envelope".to_vec(),
        hex::encode(fauna_core::sync::path_hash(victim)),
    )];

    open_then_apply(&engine, changes, 69).await;

    assert_eq!(
        engine.db().get_anchor().unwrap(),
        70,
        "the row is accounted for — that is the freeze fix"
    );
    assert_eq!(
        engine.causal().honest_anchor(victim, 70),
        69,
        "…but an edit to that path must not claim the row it never read"
    );
    assert_eq!(
        engine.causal().honest_anchor("elsewhere.txt", 70),
        70,
        "and no other path pays for it"
    );
}

/// The same for a download that can never succeed: the anchor advances, the
/// claim does not.
#[tokio::test]
async fn a_permanently_unappliable_download_pins_the_claim() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());

    let mine = LabelRoot::owner_of(&held_key());
    let escapee = "../outside-the-root.txt";
    let changes = vec![sealed_create(70, &mine, escapee, 0xAB)];

    open_then_apply(&engine, changes, 69).await;

    assert_eq!(engine.db().get_anchor().unwrap(), 70);
    assert_eq!(
        engine.causal().honest_anchor(escapee, 70),
        69,
        "the anchor accounts for the row; the claim must not"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The RESOLUTION stamp claims only what this device incorporated, too
// (ruled 2026-09-20; supersedes the same-day "edits only" narrowing)
// ─────────────────────────────────────────────────────────────────────

/// Both stamp kinds, one floor. The resolution bit says the bytes are old —
/// true at any claim — but the claim itself is what rule 3 reads as *"the
/// writer incorporated every row ≤ w"*, and this device did not incorporate
/// the row it permanently skipped.
#[tokio::test]
async fn a_permanent_skip_pins_the_resolution_stamp_too() {
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_holding_the_key(watch.path());
    let path = "photos/a.jpg";
    engine.db().set_anchor(200).unwrap();
    engine
        .causal()
        .note_permanent_skip(Some(&hex::encode(fauna_core::sync::path_hash(path))), 100);

    let resolution = engine.resolution_stamp(path);
    assert_eq!(
        resolution.is_resolution,
        Some(true),
        "the class bit is untouched"
    );
    assert_eq!(
        resolution.derived_through,
        Some(99),
        "the claim stops below the row this device never read"
    );
    assert_eq!(
        engine.edit_stamp(path).derived_through,
        Some(99),
        "one reduction serves both stamp kinds"
    );
    assert_eq!(
        engine.resolution_stamp("elsewhere.txt").derived_through,
        Some(200),
        "and only that path pays for it"
    );
}

/// The defect the reduction closes, seen from the receiver. A peer that
/// APPLIED the row this device skipped holds novel content at that seq — the
/// per-reader permanent classes (a manifest past its `check_min_reader`, a
/// seal under key material this device lacks) are exactly rows OTHER readers
/// apply. A reissue stamped at the raw anchor reads as covering there and,
/// with no unpublished work, is adopted VERBATIM: old bytes over the peer's
/// newer content, no conflict row — the lost-edit defect wearing a resolution
/// stamp. The gap-3 class upgrade is no defence: it judges the row's CLASS
/// from held bytes, never its claim, and the adopt licence runs ahead of the
/// content rung. The honest claim makes rule 2 skip the row as the
/// information-free row it is to that peer.
#[test]
fn an_over_claimed_resolution_would_regress_a_peer_past_the_skipped_row() {
    use crate::causal::{IncomingVerdict, PathFrontiers, judge_incoming};
    // The peer applied the novel row at seq 100 that the writer skipped.
    let peer = PathFrontiers {
        frontier: Some(100),
        edit_frontier: Some(100),
        content_frontier: None,
    };
    // A reissue of bytes the writer held before seq 100, landing at seq 201.
    assert_eq!(
        judge_incoming(201, Some(200), true, false, peer, false, true),
        IncomingVerdict::FastForward,
        "the raw-anchor stamp licenses the regression"
    );
    assert_eq!(
        judge_incoming(201, Some(99), true, false, peer, false, true),
        IncomingVerdict::StaleResolution,
        "the floor-reduced stamp is skipped and accounted; the peer keeps seq 100"
    );
}

/// …and nothing legitimate is stranded: every receiver whose edit-frontier is
/// below the skip still reads the reduced stamp as covering — including one
/// whose FULL frontier is past it, having applied only resolutions since
/// (gap-2: the rows between the two frontiers are order-choices this later
/// row supersedes). With unpublished local work it merges, never skips.
#[test]
fn a_reduced_resolution_still_covers_every_receiver_below_the_skip() {
    use crate::causal::{IncomingVerdict, PathFrontiers, judge_incoming};
    let behind = PathFrontiers {
        frontier: Some(80),
        edit_frontier: Some(80),
        content_frontier: None,
    };
    assert_eq!(
        judge_incoming(201, Some(99), true, false, behind, false, true),
        IncomingVerdict::FastForward,
        "a receiver below the skip adopts the covering resolution"
    );
    let order_choices_only = PathFrontiers {
        frontier: Some(150),
        edit_frontier: Some(99),
        content_frontier: None,
    };
    assert_eq!(
        judge_incoming(201, Some(99), true, false, order_choices_only, false, true),
        IncomingVerdict::FastForward,
        "a full frontier past the skip is not staleness — only novel content is"
    );
    assert!(
        matches!(
            judge_incoming(201, Some(99), true, false, behind, false, false),
            IncomingVerdict::Diverged { .. }
        ),
        "unpublished local work merges the covering resolution, never skips it"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The WINNER claim and the two floors — both reduce it; the exemption is
// retired (ruled 2026-09-22)
// ─────────────────────────────────────────────────────────────────────

/// **The control: the reduction costs the two frontier-bounded branches
/// nothing.** A live per-path skip sits above that path's frontier by
/// construction — the floor's release IS `frontier ≥ skip` — so a
/// frontier-bounded winner claim is never cut by it; that is why the
/// reduction runs unconditionally rather than branching on the claim's shape.
/// This exercises `honest_w`'s own derivation
/// ([`crate::causal::CausalStore::honest_winner_claim`], the function both
/// hosts call), not `honest_anchor` standing in for it: the previous pin used
/// the proxy, and a proxy cannot tell the frontier-bounded branches from the
/// own-gap-widened one below.
#[test]
fn a_frontier_bounded_claim_is_never_reduced_by_a_live_per_path_skip() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    causal.note_permanent_skip(Some(&hex::encode(fauna_core::sync::path_hash(path))), 100);
    causal.advance_frontier(path, 60);

    assert_eq!(
        causal.honest_winner_claim(path, 61, false),
        61,
        "the contiguous incoming seq is already below the floor"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 400, false),
        60,
        "…and so is the frontier fallback: the skip held the frontier at 60"
    );
    assert_eq!(
        causal.honest_anchor(path, 200),
        99,
        "only a claim reaching past the skip is cut — which a frontier-bounded \
         winner claim cannot"
    );
}

/// ⚠ **The own-gap-widened branch is NOT frontier-bounded, so the per-path
/// exemption does not reach it either.** `claim_gap_all_own` claims the
/// incoming seq outright, and a row skipped under this path's own `path_hash`
/// can be invisible to the caller's gap proof: at the seal-refusal mint sites
/// the row's path never resolved, so it matches no same-path filter. Both hosts mint per-path floors earlier in the same batch
/// they later claim in, so the floor can still be live when the claim is made.
#[test]
fn an_own_gap_widened_claim_is_still_cut_by_a_live_per_path_skip() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";
    causal.advance_frontier(path, 50);
    causal.note_permanent_skip(Some(&hex::encode(fauna_core::sync::path_hash(path))), 60);

    assert_eq!(
        causal.honest_winner_claim(path, 70, true),
        59,
        "widening over own gaps must not widen over a row never read"
    );

    // …and the reduction costs the two sound branches nothing, which is why it
    // runs unconditionally rather than branching on `gap_all_own`.
    assert_eq!(
        causal.honest_winner_claim(path, 51, false),
        51,
        "the contiguous incoming seq is untouched: the floor sits above it"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 70, false),
        50,
        "and so is the frontier fallback"
    );

    // Once the path's own frontier reaches the skip, the floor releases and the
    // widened claim is free again — the same release the anchor gets.
    causal.advance_frontier(path, 65);
    assert_eq!(
        causal.honest_winner_claim(path, 70, true),
        70,
        "content reflecting seq 65 supersedes the row skipped at 60"
    );
}

/// ⚠ **The defect.** The exemption above was argued for the releasing
/// per-path floor and taken for BOTH. A set-wide skip — the `Salt` refusal,
/// a `path_hash` that is not even 32 hex bytes — is attributable to no path
/// by definition, so no path's frontier is held below it and every frontier
/// climbs past it freely. A frontier-bounded claim then crosses a skipped row,
/// which is exactly the lost-edit defect the floor exists to prevent: a peer
/// whose edit-frontier reached the skipped seq judges the over-claimed winner
/// row covering and, with no unpublished work, adopts old bytes verbatim over
/// its content, with no conflict row.
#[test]
fn a_set_wide_skip_reduces_the_winner_claim_too() {
    let dir = tempfile::tempdir().unwrap();
    let causal = crate::causal::CausalStore::new(dir.path().to_path_buf());
    let path = "photos/a.jpg";

    causal.note_permanent_skip(None, 100);
    causal.advance_frontier(path, 150);
    assert_eq!(
        causal.frontier(path),
        Some(150),
        "nothing holds this path's frontier below a skip that belongs to no path"
    );

    assert_eq!(
        causal.honest_winner_claim(path, 151, false),
        99,
        "a contiguous incoming seq is still cut below the row never read"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 400, false),
        99,
        "…and so is the frontier fallback, which alone would claim 150"
    );
    assert_eq!(
        causal.honest_winner_claim(path, 400, true),
        99,
        "…and so is a claim widened over this device's own listing gaps"
    );

    // ⚠ The device used to disagree with itself about one skipped row: an edit
    // or resolution stamp claimed 99 while the winner claimed 150, honest or
    // dishonest depending only on which stamp was being minted.
    assert_eq!(
        causal.honest_anchor(path, 200),
        99,
        "one floor, one answer — whichever claim the device is minting"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The state directory IS the set scope (Do 4 of the finding — row 735)
// ─────────────────────────────────────────────────────────────────────

use crate::pull_remote_changes_test::test_engine_with_keys_named;

/// A causal state resting in the flat root itself, outside every scope.
fn flat_root_causal(watch: &std::path::Path, path: &str, frontier: i64) {
    let flat = crate::causal::CausalStore::new(watch.join(".fauna-causal"));
    flat.advance_frontier(path, frontier);
}

fn engine_bound_to(watch: &std::path::Path, folder: &str) -> SyncEngine {
    test_engine_with_keys_named(
        watch.to_path_buf(),
        folder,
        Some(OwnerSealKey::Client(held_key())),
        None,
        None,
    )
}

/// ⚠ The wrong answer this scoping closes. `seq` is per SET and the frontier
/// only grows, so a watch directory re-bound to a different set read the first
/// set's frontier as its own — and receiver rule 1 skips every incoming
/// `seq ≤ frontier` byte-free, so the new set's rows were never applied while
/// the anchor advanced past them.
#[test]
fn a_re_bind_to_another_set_does_not_inherit_the_first_sets_frontier() {
    let watch = tempfile::tempdir().unwrap();
    let path = "photos/a.jpg";

    let first = engine_bound_to(watch.path(), "photos");
    first.causal().advance_frontier(path, 900);
    assert_eq!(first.causal().frontier(path), Some(900));

    // The user re-points this same directory at a different set.
    let second = engine_bound_to(watch.path(), "private");
    assert_eq!(
        second.causal().frontier(path),
        None,
        "the second set's seq space is unrelated — inheriting 900 would skip \
         every row it lists below that as a duplicate"
    );

    // …and the first set's state is untouched by the second's arrival.
    assert_eq!(
        engine_bound_to(watch.path(), "photos")
            .causal()
            .frontier(path),
        Some(900)
    );
}

/// The hazard is CONCURRENT too, not only sequential: persisted bindings are
/// deduped by set, so one path can be bound to two sets at once and two live
/// engines share one watch directory.
#[test]
fn two_sets_bound_to_one_watch_dir_keep_separate_state() {
    let watch = tempfile::tempdir().unwrap();
    let path = "notes.txt";

    let a = engine_bound_to(watch.path(), "photos");
    let b = engine_bound_to(watch.path(), "private");

    a.causal().advance_frontier(path, 900);
    b.causal().advance_frontier(path, 7);

    assert_eq!(a.causal().frontier(path), Some(900));
    assert_eq!(
        b.causal().frontier(path),
        Some(7),
        "a monotone frontier shared between two live engines cannot hold both"
    );
}

/// Nothing is carried from the flat root: the pre-scoping flat layout
/// predates the compat-remnant sweep (`version-compatibility.md` § Dimension
/// 2, program 4), so a state resting there belongs to no set. Every set starts
/// in its own scope, and the root's own entries are left untouched.
#[test]
fn a_flat_root_state_is_never_carried_into_a_scope() {
    let watch = tempfile::tempdir().unwrap();
    let path = "photos/a.jpg";
    flat_root_causal(watch.path(), path, 900);

    let first = engine_bound_to(watch.path(), "photos");
    assert_eq!(
        first.causal().frontier(path),
        None,
        "a flat-root frontier is carried into no scope, the first set's included"
    );

    let flat = crate::causal::CausalStore::new(watch.path().join(".fauna-causal"));
    assert_eq!(
        flat.frontier(path),
        Some(900),
        "…and is left where it rests"
    );
}

/// An engine with no set has nothing to scope by — one-shot fixtures and the
/// restore download walk (a set-less engine) keep the flat root.
#[test]
fn no_set_means_no_scope_and_the_flat_root_stands() {
    let watch = tempfile::tempdir().unwrap();

    let flat = SyncEngine::resolve_state_dir(watch.path(), None, &[0u8; 32], ".fauna-causal");
    assert_eq!(flat, watch.path().join(".fauna-causal"));

    let scoped =
        SyncEngine::resolve_state_dir(watch.path(), Some("photos"), &[0u8; 32], ".fauna-causal");
    assert_ne!(scoped, flat);
    assert!(
        scoped.starts_with(&flat),
        "the scope is a subdirectory of the root"
    );
}

/// The merge base rides the same directory and the same hazard — a foreign
/// base that happens to equal the local bytes reads as "no local change" and
/// overwrites a genuinely diverged edit silently (`conflicts.md` §
/// Implementation status today). Scoping one root and not the other would
/// leave the worse half open.
#[test]
fn the_merge_base_root_is_scoped_too() {
    let watch = tempfile::tempdir().unwrap();

    let a = engine_bound_to(watch.path(), "photos");
    let b = engine_bound_to(watch.path(), "private");

    assert_ne!(
        a.base_dir_for_test(),
        b.base_dir_for_test(),
        "two sets on one watch directory must not share a merge-base root"
    );
    assert_ne!(a.causal_dir_for_test(), b.causal_dir_for_test());
}

/// The merge-base root names each base by the user's own relative path, so a
/// subfolder's base rests in a subdirectory of the root — and is carried no
/// more than a top-level one.
#[test]
fn a_nested_flat_root_base_is_never_carried_into_a_scope() {
    let watch = tempfile::tempdir().unwrap();
    let nested = watch.path().join(".fauna-bases").join("docs/a.txt");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, b"the ancestor").unwrap();

    let owner = engine_bound_to(watch.path(), "photos");
    assert!(
        !owner.base_dir_for_test().join("docs/a.txt").exists(),
        "a flat-root merge base is carried into no scope"
    );
    assert!(nested.exists(), "…and is left where it rests");
}
