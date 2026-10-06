//! The filesystem half of the succession corpus-ownership re-point, and the
//! boot heal that finishes it after a crash.
//!
//! Ownership moves in the succession transaction (`succession-aftermath.md`
//! § Re-key scope, the ownership blockquote; rows in
//! `db/successions.rs::record_succession`) — but the actor-scoped segment
//! kinds also live *on disk* under `<data_dir>/__<kind>/<actor_hex>/`, and a
//! filesystem rename cannot join a SQL transaction. So the split is: the
//! transaction is the single atomic decision point, the submit handler renames
//! the directories right after commit ([`heal_segment_dirs`]), and the boot
//! heal ([`heal_at_boot`]) closes the one gap that leaves — a crash between
//! commit and rename. The rows need no boot pass: the transaction moved them.
//!
//! **The placement journals ride along too** (2026-08-03). The per-actor
//! mail/cal/card *placement* managers keep their own on-disk journals under a
//! layout `SegmentManager` knows nothing about, so they cannot use its
//! `rename_scope` — they implement the shared `ActorScopedStore` trait instead,
//! and this module heals "every actor-scoped store" rather than a list of
//! concrete types. That is what stops the next such store from being silently
//! left behind: it falls into the heal by implementing the trait.

use crate::db::CacheDb;
use fauna_segment_store::{ActorScopedStore, ScopeRenameOutcome};

/// Rename one identity's actor-scoped directories old→new across the given
/// stores — the four actor-scoped segment kinds (mail, post, calendar, card —
/// never conv, whose scope is the channel) plus the three placement journals.
/// Returns the kinds that PARKED
/// (both ids hold a directory — merging would collide their per-scope segment
/// ids, so nothing is touched; the predecessor's files stay put, unserved but
/// intact).
///
/// Infallible by policy: a rename error on one kind is logged and must not
/// fail the succession that already committed — the boot heal retries it.
pub fn heal_segment_dirs(
    managers: &[&dyn ActorScopedStore],
    old: &[u8; 32],
    new: &[u8; 32],
) -> Vec<&'static str> {
    let mut parked = Vec::new();
    for mgr in managers {
        match mgr.rename_actor(old, new) {
            Ok(ScopeRenameOutcome::Moved) | Ok(ScopeRenameOutcome::NothingToMove) => {}
            Ok(ScopeRenameOutcome::Parked) => {
                tracing::warn!(
                    target: "recovery",
                    kind = mgr.kind(),
                    predecessor = %hex::encode(old),
                    successor = %hex::encode(new),
                    "segment directory parked at succession: both identities hold \
                     one, and two scopes' segment ids cannot merge — the \
                     predecessor's files stay under the old id (nothing lost, \
                     nothing served)"
                );
                parked.push(mgr.kind());
            }
            Err(e) => {
                tracing::error!(
                    target: "recovery",
                    kind = mgr.kind(),
                    error = %e,
                    "segment directory rename failed at succession; the boot \
                     heal will retry"
                );
            }
        }
    }
    parked
}

/// The succession-ownership boot heal: a [`heal_segment_dirs`] pass over every
/// recorded (predecessor → terminal successor) pair
/// ([`CacheDb::succession_heal_pairs`]). Idempotent — a pair whose directories
/// already moved is a no-op rename — and a no-op on a nest with no successions.
/// Runs before serving starts, so a crash between a succession's commit and its
/// directory rename is healed before any client can observe the split state.
pub async fn heal_at_boot(db: &CacheDb, managers: &[&dyn ActorScopedStore]) -> anyhow::Result<()> {
    for (old, terminal) in &db.succession_heal_pairs().await? {
        heal_segment_dirs(managers, old, terminal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TierRow;
    use fauna_cbor::Cid;
    use fauna_segment_store::SegmentManager;
    use tempfile::TempDir;

    const OLD: [u8; 32] = [0xA1; 32];
    const NEW: [u8; 32] = [0xB2; 32];

    fn managers_in(tmp: &TempDir) -> (SegmentManager, SegmentManager) {
        (
            SegmentManager::new(tmp.path().to_path_buf(), "mail"),
            SegmentManager::new(tmp.path().to_path_buf(), "post"),
        )
    }

    async fn seed_user(db: &CacheDb, actor: &[u8; 32]) {
        db.create_tier(&TierRow {
            name: "free".into(),
            max_inbox_bytes: 1,
            max_storage_bytes: 1,
            max_devices: 1,
            max_blob_size: 1,
            max_feeds: 1,
        })
        .await
        .ok();
        db.create_user(actor, "free", "").await.unwrap();
    }

    /// **The placement journals move with the corpus** — the (4) half of the
    /// ownership re-point (`succession-aftermath.md` § Re-key scope).
    ///
    /// Left behind, a successor's mailboxes/collections would sit under an
    /// identity the nest refuses on every call, so the account would come up
    /// looking empty while the data sat intact one directory over. The journal
    /// has its own layout and no `SegmentManager` underneath, which is exactly
    /// why it was the piece left out until now — so this asserts the FILES,
    /// not the plumbing.
    #[tokio::test]
    async fn the_boot_heal_moves_the_placement_journals_too() {
        use crate::segments::MailPlacementSegmentManager;
        use fauna_mail::segments::placement::{MailPlacementRecord, mail_placement_segments_root};

        let tmp = TempDir::new().unwrap();
        let (mail, post) = managers_in(&tmp);
        let placement = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        placement
            .append_event(
                &OLD,
                &MailPlacementRecord::Create {
                    mailbox: "INBOX".into(),
                    uid_validity: 1,
                    attrs: Vec::new(),
                },
            )
            .await
            .unwrap();
        let old_root = mail_placement_segments_root(tmp.path(), &OLD);
        let new_root = mail_placement_segments_root(tmp.path(), &NEW);
        assert!(old_root.exists(), "the journal rests under the predecessor");

        let db = CacheDb::open_in_memory().unwrap();
        seed_user(&db, &OLD).await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        heal_at_boot(&db, &[&mail, &post, &placement])
            .await
            .unwrap();

        assert!(
            !old_root.exists(),
            "nothing is left under the identity the nest now refuses"
        );
        assert!(
            new_root.exists(),
            "and the successor reaches its own placement state"
        );
    }

    /// A collision parks rather than merging — two actors' journals cannot be
    /// unioned (their per-actor record ids would collide), and the predecessor's
    /// files stay intact. Reported by kind so the caller can log it, never
    /// silently dropped.
    #[test]
    fn a_placement_journal_collision_parks_and_destroys_nothing() {
        use crate::segments::MailPlacementSegmentManager;
        use fauna_mail::segments::placement::mail_placement_segments_root;

        let tmp = TempDir::new().unwrap();
        let placement = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let old_root = mail_placement_segments_root(tmp.path(), &OLD);
        let new_root = mail_placement_segments_root(tmp.path(), &NEW);
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::create_dir_all(&new_root).unwrap();
        std::fs::write(old_root.join("marker"), b"predecessor").unwrap();

        let parked = heal_segment_dirs(&[&placement], &OLD, &NEW);

        assert_eq!(
            parked,
            vec!["mail-placement"],
            "the park is reported by kind"
        );
        assert_eq!(
            std::fs::read(old_root.join("marker")).unwrap(),
            b"predecessor",
            "and the predecessor's journal is untouched"
        );
    }

    /// The crash window: a current-code succession committed its transaction
    /// but died before the handler's directory rename. The boot heal must
    /// finish the move — the transaction already moved every row.
    #[tokio::test]
    async fn the_boot_heal_finishes_a_crash_between_commit_and_dir_rename() {
        let tmp = TempDir::new().unwrap();
        let (mail, post) = managers_in(&tmp);
        let env = b"opaque".to_vec();
        mail.append_record_with_bucket(&OLD, Cid::of_dag_cbor(&env), &env, b"", "2026-08")
            .await
            .unwrap();

        let db = CacheDb::open_in_memory().unwrap();
        seed_user(&db, &OLD).await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        // (The handler's post-commit heal never ran — the simulated crash.)

        heal_at_boot(&db, &[&mail, &post]).await.unwrap();

        assert!(!mail.scope_dir(&OLD).exists());
        assert!(mail.scope_dir(&NEW).exists());
        let manifest = mail.load_manifest(&NEW).await.unwrap();
        assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
    }

    #[tokio::test]
    async fn a_parked_directory_is_reported_and_left_intact() {
        let tmp = TempDir::new().unwrap();
        let (mail, _) = managers_in(&tmp);
        for scope in [&OLD, &NEW] {
            let env = b"opaque".to_vec();
            mail.append_record_with_bucket(scope, Cid::of_dag_cbor(&env), &env, b"", "2026-08")
                .await
                .unwrap();
        }

        let parked = heal_segment_dirs(&[&mail], &OLD, &NEW);
        assert_eq!(parked, vec!["mail"]);
        assert!(mail.scope_dir(&OLD).exists());
        assert!(mail.scope_dir(&NEW).exists());
    }
}
