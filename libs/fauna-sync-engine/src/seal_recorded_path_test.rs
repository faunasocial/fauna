//! Engine-level pins for the **one seal funnel** — `SyncEngine::record_change`'s
//! `seal_recorded_path`, the single place a client turns a user-chosen path into
//! a `fauna_core::path_crypto::SealedLabel`
//! (`docs/goal/behavior/file-sync.md` § Sealed names & paths, implementing the
//! 2026-07-29 paths-are-content ruling).
//!
//! Four properties, each of which is a different way to get sealing wrong:
//!
//! 1. **The root is the one that already seals the set's chunks** — a bound set
//!    seals under its M2 content-key generation and *stamps that version*, an
//!    owner-only set under `convergent_chunk_root()` with no version. Getting
//!    this wrong doesn't fail loudly: it produces a blob nobody in the set's
//!    real audience can open.
//! 2. **Convergent, so re-recording is idempotent.** The salt is the path's own
//!    `path_hash`, so sealing the same path twice must reproduce identical
//!    bytes — otherwise every rescan tick rewrites the column and a
//!    content-identical re-record stops looking identical.
//! 3. **Fail-closed is inherited** (FS-BIND-5): a bound engine with no content
//!    keys loaded refuses to seal, and therefore refuses to record — it must
//!    never fall through to "record the name in plaintext".
//! 4. **A keyless engine records plaintext-only** — no panic, no empty
//!    envelope, no synthesised key. The nest accepts that shape on a
//!    plaintext-resting plane (a `public`-audience folder) and refuses it
//!    everywhere else (`path_seal_required`), so it never rests seal-less on a
//!    plane whose plaintext scrubs.

use fauna_core::path_crypto::{LabelField, SealedLabel, open};

use crate::pull_remote_changes_test::{test_engine, test_engine_with_keys};

const PATH: &str = "2026/eviction_notice.pdf";

fn opened_under(sealed: &[u8], root: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
    open(
        [root],
        &fauna_core::sync::path_hash(PATH),
        LabelField::SyncChangePath,
        &SealedLabel::from_bytes(sealed)?,
    )
}

/// Property 1a + 2, owner arm: seals under `convergent_chunk_root()`, stamps no
/// generation, and is byte-stable across repeats.
#[test]
fn an_owner_only_engine_seals_under_the_chunk_root_with_no_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let backup_key = fauna_core::crypto::BackupKey::from_bytes([0x33; 32]);
    let root = backup_key.convergent_chunk_root();
    let engine = test_engine_with_keys(
        tmp.path().to_path_buf(),
        Some(fauna_core::crypto::OwnerSealKey::Client(backup_key)),
        None,
        None,
    );

    let sealed = engine
        .seal_recorded_path(PATH)
        .expect("an owner engine holds a root")
        .expect("…and therefore seals");

    let label = SealedLabel::from_bytes(&sealed).unwrap();
    assert_eq!(
        label.generation, None,
        "the owner root does not rotate, so the envelope names no generation — \
         a reader must trial the owner root, not `keys_for(v)`"
    );
    assert_eq!(
        label.nonce, None,
        "`path` is convergent (salt = path_hash), so the envelope carries no explicit nonce"
    );
    assert_eq!(opened_under(&sealed, &root).unwrap(), PATH.as_bytes());

    // Convergence: an idempotent re-record must not churn the column.
    let again = engine.seal_recorded_path(PATH).unwrap().unwrap();
    assert_eq!(
        sealed, again,
        "sealing the same path under the same root must reproduce identical bytes"
    );
}

/// Property 1b: a bound (cross-user shared) set seals under its **M2 content
/// key** and stamps the generation, so a reader knows which `keys_for(v)`
/// candidates to trial — and so a server-side row copy stays openable with no
/// per-table generation column.
#[test]
fn a_bound_engine_seals_under_the_content_key_and_stamps_its_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let content_key = [0x44u8; 32];
    let keys = fauna_core::folder_keys::FolderContentKeys::genesis(content_key, 1_700_000_000);
    let version = keys.current_version();
    let engine = test_engine_with_keys(
        tmp.path().to_path_buf(),
        // A bound engine's `backup_key` is deliberately shadowed by the
        // content-key path (FS-5DC) — pass one to prove it is NOT what seals.
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        Some(b"group-id".to_vec()),
        Some(keys),
    );

    let sealed = engine.seal_recorded_path(PATH).unwrap().unwrap();
    let label = SealedLabel::from_bytes(&sealed).unwrap();

    assert_eq!(
        label.generation,
        Some(version),
        "the envelope must name the generation it sealed under"
    );
    assert_eq!(
        opened_under(&sealed, &content_key).unwrap(),
        PATH.as_bytes(),
        "every roster member holds the M2 content key, so every member renders the name"
    );
    assert!(
        opened_under(
            &sealed,
            &fauna_core::crypto::BackupKey::from_bytes([0x33; 32]).convergent_chunk_root()
        )
        .is_err(),
        "the owner's private key must NOT be what seals a shared set's names — no member \
         but the owner holds it, so a roster member could not render the folder"
    );
}

/// Property 3: FS-BIND-5. A bound engine whose content keys are not loaded (a
/// removed member, or a startup race) must refuse — the same fail-closed
/// posture `content_seal_root` gives the chunk seal, inherited rather than
/// re-implemented.
#[test]
fn a_bound_engine_without_content_keys_refuses_to_seal() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine_with_keys(
        tmp.path().to_path_buf(),
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        Some(b"group-id".to_vec()),
        None, // bound, but no M2 generation in hand
    );

    let err = engine
        .seal_recorded_path(PATH)
        .expect_err("a bound keyless engine must not seal, and must not degrade to plaintext");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("content_seal_root"),
        "the refusal must name the fail-closed root selection so a log reader can act on it, \
         got: {msg}"
    );
}

/// Property 4: no root at all ⇒ `None`, the plaintext-only record shape. The
/// discriminator that matters is that this is `Ok(None)` and not an error: a
/// test/unkeyed engine must keep recording, just without a sealed sibling —
/// the nest then decides, per plane, whether plaintext may rest.
#[test]
fn a_keyless_engine_records_plaintext_only() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path().to_path_buf());
    assert_eq!(
        engine.seal_recorded_path(PATH).unwrap(),
        None,
        "an unbound engine with no key material seals nothing — and does not fail"
    );
}

/// Phase 4: a `public`-audience engine records plaintext paths DESPITE holding
/// a key — the deliberate arm, not the keyless degradation. Its names and
/// paths are world-readable by ratified design (they are URLs), so the one
/// label funnel yields `None` off the armed flag, never off missing material.
#[test]
fn a_public_engine_records_plaintext_paths_despite_holding_a_key() {
    let tmp = tempfile::tempdir().unwrap();
    let backup_key = fauna_core::crypto::BackupKey::from_bytes([0x44; 32]);
    let engine = test_engine_with_keys(
        tmp.path().to_path_buf(),
        Some(fauna_core::crypto::OwnerSealKey::Client(backup_key)),
        None,
        None,
    )
    .with_public_audience(true);
    assert_eq!(
        engine.seal_recorded_path(PATH).unwrap(),
        None,
        "a declassified folder's paths rest plaintext — the flag, not a missing key, decides"
    );
}

/// Domain separation, the cross-field half: the same root and the same path
/// under a *different* field tag must not open as `SyncChangePath`. This is what
/// stops a sealed blob being spliced from one column into another.
#[test]
fn a_labels_field_tag_is_bound_into_the_seal() {
    let tmp = tempfile::tempdir().unwrap();
    let backup_key = fauna_core::crypto::BackupKey::from_bytes([0x33; 32]);
    let root = backup_key.convergent_chunk_root();
    let engine = test_engine_with_keys(
        tmp.path().to_path_buf(),
        Some(fauna_core::crypto::OwnerSealKey::Client(backup_key)),
        None,
        None,
    );
    let sealed = engine.seal_recorded_path(PATH).unwrap().unwrap();

    assert!(
        open(
            [&root],
            &fauna_core::sync::path_hash(PATH),
            LabelField::ConflictDetails,
            &SealedLabel::from_bytes(&sealed).unwrap(),
        )
        .is_err(),
        "a `sync_changes.path` seal must not open under another field's tag"
    );
}

// ── the set-NAME seal (S5) ───────────────────────────────────────────────────
//
// `SyncEngine::sealed_set_name` is the second consumer of the same root
// selection. The property that matters is not "it produces a blob" but "it
// produces a blob openable by *exactly the audience that opens this set's
// paths*" — so each test below opens the name seal under the root the
// corresponding path test proved the path seal takes. A divergence between the
// two would be silent (both degrade to `Omit`, never to an error).

use crate::pull_remote_changes_test::test_engine_with_keys_named;

const SET: &str = "Family photos";

fn opened_name_under(sealed: &[u8], root: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
    open(
        [root],
        &fauna_core::path_crypto::set_name_hash(SET),
        LabelField::FolderName,
        &SealedLabel::from_bytes(sealed)?,
    )
}

/// Owner arm: the set name seals under `convergent_chunk_root()` with no
/// generation — the SAME root
/// [`an_owner_only_engine_seals_under_the_chunk_root_with_no_generation`] pins
/// for the path seal, so names and paths render for one audience.
#[test]
fn an_owner_only_engine_seals_its_set_name_under_the_same_root_as_its_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let backup_key = fauna_core::crypto::BackupKey::from_bytes([0x33; 32]);
    let root = backup_key.convergent_chunk_root();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        SET,
        Some(fauna_core::crypto::OwnerSealKey::Client(backup_key)),
        None,
        None,
    );

    let sealed = engine
        .sealed_set_name()
        .expect("an owner engine holds a root")
        .expect("…and the name is not reserved, so it seals");

    let label = SealedLabel::from_bytes(&sealed).unwrap();
    assert_eq!(
        label.generation, None,
        "an owner-root label stamps no generation (that is what selects the arm on read)"
    );
    assert_eq!(opened_name_under(&sealed, &root).unwrap(), SET.as_bytes());

    // Convergent: byte-stable across repeats, so re-stamping on every start is free.
    assert_eq!(engine.sealed_set_name().unwrap().unwrap(), sealed);

    // ...and it does NOT open under an unrelated root.
    assert!(opened_name_under(&sealed, &[0xff; 32]).is_err());
}

/// A reserved `__` rail's name is a routing constant: the engine must produce
/// nothing at all, even though it holds a perfectly good root. Pinned at the
/// engine because that is the layer that would otherwise stamp every internal
/// rail the moment sealing switched on.
#[test]
fn a_reserved_rail_engine_seals_no_set_name_despite_holding_a_root() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        "__config",
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        None,
        None,
    );
    assert_eq!(
        engine.sealed_set_name().unwrap(),
        None,
        "a routing constant must never seal — ~28 nest sites route on its literal name"
    );
    // The path seal is unaffected: a reserved set's *paths* still seal.
    assert!(engine.seal_recorded_path(PATH).unwrap().is_some());
}

/// A keyless engine stamps nothing rather than panicking or synthesising a key —
/// the set-name twin of property 4.
#[test]
fn a_keyless_engine_seals_no_set_name() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine_with_keys_named(tmp.path().to_path_buf(), SET, None, None, None);
    assert_eq!(engine.sealed_set_name().unwrap(), None);
}

/// Bound arm — the case the bind/serve hook exists for: a shared set's name
/// seals under its **M2 content-key generation**, stamps that generation, and
/// pointedly does NOT seal under the owner's private key. If it did, no roster
/// member but the owner could render the folder's name in their own list.
#[test]
fn a_bound_engine_seals_its_set_name_under_the_content_key_and_stamps_the_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let content_key = [0x44u8; 32];
    let keys = fauna_core::folder_keys::FolderContentKeys::genesis(content_key, 1_700_000_000);
    let version = keys.current_version();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        SET,
        // Passed to prove it is NOT what seals for a bound set (FS-5DC).
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        Some(b"group-id".to_vec()),
        Some(keys),
    );

    let sealed = engine.sealed_set_name().unwrap().unwrap();
    let label = SealedLabel::from_bytes(&sealed).unwrap();

    assert_eq!(
        label.generation,
        Some(version),
        "the envelope must name the generation it sealed under, or the reader picks the wrong root"
    );
    assert_eq!(
        opened_name_under(&sealed, &content_key).unwrap(),
        SET.as_bytes(),
        "every roster member holds the M2 content key, so every member renders the set name"
    );
    assert!(
        opened_name_under(
            &sealed,
            &fauna_core::crypto::BackupKey::from_bytes([0x33; 32]).convergent_chunk_root()
        )
        .is_err(),
        "sealing a shared set's NAME under the owner's private key would hide it from members"
    );
}

/// FS-BIND-5 is inherited by the name seal too: a bound engine with no content
/// keys loaded refuses rather than falling back to the owner root (which would
/// stamp a name only the owner could read onto a shared set).
#[test]
fn a_bound_engine_without_content_keys_refuses_to_seal_its_set_name() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        SET,
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        Some(b"group-id".to_vec()),
        None, // bound, but no content keys
    );
    assert!(
        engine.sealed_set_name().is_err(),
        "fail closed — never downgrade a bound set's name to the owner root"
    );
}

/// The one-funnel property asserted **in one place, on one engine**: a set's name and its paths must seal under the same root, and
/// until this test the two were only pinned *in parallel* — each arm's name test
/// opening under the root the corresponding path test proved, which a reader has
/// to verify by eye and which stays green if the two selections drift together
/// onto the wrong root. Both consumers now call one `label_seal_root`, so this
/// asserts the property that refactor makes structural, and would go red if a
/// second root selection were ever reintroduced.
///
/// The generation is the observable that distinguishes the two roots: the bound
/// arm stamps `Some(version)`, the owner arm `None`. Equal generations plus a
/// successful open of *both* labels under the *same* key is exactly "one root".
#[test]
fn a_set_name_and_its_paths_seal_under_one_root_on_the_same_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let content_key = [0x44u8; 32];
    let keys = fauna_core::folder_keys::FolderContentKeys::genesis(content_key, 1_700_000_000);
    let version = keys.current_version();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        SET,
        // The owner key is present precisely so a divergent selection has
        // somewhere wrong to go.
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        Some(b"group-id".to_vec()),
        Some(keys),
    );

    let sealed_path = engine.seal_recorded_path(PATH).unwrap().unwrap();
    let sealed_name = engine.sealed_set_name().unwrap().unwrap();

    let path_label = SealedLabel::from_bytes(&sealed_path).unwrap();
    let name_label = SealedLabel::from_bytes(&sealed_name).unwrap();
    assert_eq!(
        path_label.generation, name_label.generation,
        "name and path sealed under different roots — the audience that opens \
         this set's file list would not be the one that renders its name"
    );
    assert_eq!(path_label.generation, Some(version));

    // …and the one key opens both, which is the property the generations imply.
    assert_eq!(
        opened_under(&sealed_path, &content_key).unwrap(),
        PATH.as_bytes()
    );
    assert_eq!(
        opened_name_under(&sealed_name, &content_key).unwrap(),
        SET.as_bytes()
    );
}

/// The engine-start passes stamp the set's sealed name, addressed by hash.
///
/// The stamp is the keyed writer that moves a set's name to its M2 audience
/// when a share binds it (`path-sealing.md` § the set-name plane). Its only
/// caller left with the host that was retired on 2026-09-25, and since schema
/// 114 a sealed set rests no plaintext name — so a member could never open the
/// name of a set shared with them, and the set never reached their Folders
/// page. The request goes out by `name_hash` alone, because the nest finds no
/// sealed set by its plaintext name.
#[tokio::test]
async fn the_start_passes_stamp_the_set_name_addressed_by_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine_with_keys_named(
        tmp.path().to_path_buf(),
        SET,
        Some(fauna_core::crypto::OwnerSealKey::Client(
            fauna_core::crypto::BackupKey::from_bytes([0x33; 32]),
        )),
        None,
        None,
    );
    let control = crate::nest_api::FakeSyncControl::accepting();
    engine.set_control_api(std::sync::Arc::new(control.clone()));

    crate::always_resident::converge_corpus_at_start(&engine, SET).await;

    let stamps: Vec<_> = control
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            crate::nest_api::RecordedControlCall::UpdateFolder(req)
                if req.name_sealed.is_some() =>
            {
                Some(*req)
            }
            _ => None,
        })
        .collect();
    assert_eq!(stamps.len(), 1, "one name stamp per engine start");
    assert_eq!(
        stamps[0].name_sealed.as_ref().map(|b| b.to_vec()),
        engine.sealed_set_name().unwrap(),
        "the stamp carries the name sealed under the engine's current root"
    );
    assert_eq!(
        stamps[0].name_hash.as_ref().map(|b| b.to_vec()),
        Some(fauna_core::path_crypto::set_name_hash(SET).to_vec()),
        "addressed by the set's hash"
    );
    assert!(
        stamps[0].name.is_empty(),
        "the plaintext name is off the request"
    );
}
