//! Destination-side **generation recovery** — the user-facing half of the
//! rogue-source mitigation.
//!
//! **What it is for.** Since the segment-backup redesign the custody *writer* is
//! the owner's own source nest, and a custody writer's supersede power is delete
//! power: a compromised source could erase a whole backup by re-recording every
//! path with junk. Two destination-side mechanisms bound that. The first is the
//! **custody grace window `T`** — a superseded generation is not forgotten but
//! moved to an append-only side table at the destination, quota-charged and
//! reclaimed only after `T` (`docs/goal/architecture/message-segment-store.md`
//! § *Custody grace window (T)*). The second is the seed-holding client's
//! [audit loop](crate::audit), which is what *tells* the owner something is
//! wrong in the first place.
//!
//! This module is what makes the window **useful** rather than merely
//! protective: after revoking a rogue source's writer grant (the sibling
//! [`crate::trust`] surface), the owner lists the generations the destination
//! retained and restores the good ones.
//!
//! # Every call here is spoken to the destination
//!
//! Not one of these reads or writes may be routed through the source nest, for
//! the reason the whole plane exists: **the source is the writer being recovered
//! from.** A hostile source asked to list its victim's retained generations
//! would answer with a lie, and asked to restore one would swallow the call. So
//! the transport is the same [`BackupDestinationConnector`] leg (c) already
//! opens — the client's own authenticated connection to each destination, at the
//! URL from its own pinned `fauna.state.backup` destination list, never a URL the
//! source supplied.
//!
//! Note the shape difference from the sibling [`crate::trust::backup_trust_rows`],
//! which takes a source seam *and* a connector because its seal row genuinely is
//! a source-nest read. **Nothing here takes a source seam at all** — so
//! "accidentally ask the source" is not a bug this API can express. A tier_3 pair
//! (`bins/fauna-nest/tests/conformance_backup_generation_client.rs`) pins the
//! consequence from the other side: aimed at a nest that holds no custody, the
//! list comes back empty and the restore reports
//! [`RestoreOutcome::NoSuchGeneration`] — the quiet false reassurance a
//! mis-addressed recovery would hand a user whose backup was just overwritten.
//!
//! # `T` belongs to the nest, not to the client
//!
//! `generation.list` reports `grace_secs` on the wire precisely so no client
//! hard-codes the window. [`RetainedGeneration::expires_at`] is derived from it
//! once, here — so a destination running a nest with a different window renders
//! its real deadline rather than one computed against a stale local constant.
//! The one client reader that may not take the wire value alone is the audit's
//! vanished-ledger rule (`crate::audit`), which judges the destination's *own*
//! retention and so floors the reported window at the protocol's
//! [`fauna_protocol::backup::BACKUP_CUSTODY_GRACE_SECS`]: the audited party's
//! word cannot shorten the window it is judged by.
//!
//! # Restoring is never itself destructive
//!
//! A restore promotes one generation back to live and retains the one it
//! displaces *by the same machinery a supersede uses*, so a mistaken restore is
//! itself undoable within `T`. That is also why a
//! [`RestoreOutcome::NoSuchGeneration`] is a product state and not an error —
//! see its docs.

use fauna_core::data::BackupDestination;

use crate::trust::{BackupDestinationConnector, connect_destination};

/// One superseded custody generation a destination is still retaining, ready to
/// be rolled back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedGeneration {
    /// The custody set's reserved name (`__mail`, `__post`, `__conv/<hex>`) —
    /// half of the restore address.
    pub folder_name: String,
    /// Plaintext path, when the superseded custody row carried one. **Display
    /// only, and genuinely optional**: `path_hash` is one-way, so a row the S9 scrub
    /// nulled `path` on (beside `path_sealed`) has none. A shell renders the hash (or the
    /// set name) for such a row — it must never hide or skip it, because the
    /// rows a rogue source produced are exactly the ones a user needs to see.
    pub path: Option<String>,
    /// Hex-encoded 32-byte path hash — **the** restore address, always present.
    pub path_hash: String,
    /// Hex-encoded 32-byte manifest hash of this generation.
    pub manifest_hash: String,
    pub size_bytes: i64,
    /// Unix seconds at which this generation stopped being live.
    pub superseded_at: i64,
    /// Unix seconds at which this generation reclaims and the rollback stops
    /// being possible — `superseded_at + grace_secs`, derived from the window
    /// the *destination* reported. Never re-derived per client.
    pub expires_at: i64,
}

/// Whether a destination could be asked about its retained generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationsStatus {
    /// The destination answered. [`DestinationGenerations::generations`] is its
    /// real answer — an empty list here means "nothing to roll back", which is
    /// the healthy steady state.
    Listed,
    /// The read failed — unreachable, refused, or malformed. Deliberately
    /// distinct from an empty [`Listed`](Self::Listed): "we could not ask" must
    /// never render as "there is nothing to recover", which is precisely the
    /// false reassurance a hostile source or a dropped connection would buy.
    Unreachable,
}

/// One destination's retained-generation group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationGenerations {
    /// `BackupDestination::destination_id` — names the group for a restore.
    pub destination_id: String,
    /// The destination's origin URL (the restore's connect target).
    pub destination_url: String,
    /// User-facing label via the shared [`fauna_core::format::backup_destination_label`]
    /// fallback — never re-derived per client (priority #4).
    pub destination_label: String,
    pub status: GenerationsStatus,
    /// The window `T` this destination reported, in seconds. `0` when the read
    /// failed (there is no window to report).
    pub grace_secs: i64,
    /// Newest supersede first — the order the destination returned, deliberately
    /// preserved rather than re-sorted, so one nest-side ordering rule serves
    /// every app.
    pub generations: Vec<RetainedGeneration>,
}

impl DestinationGenerations {
    /// Whether this destination has anything the owner could roll back to. A
    /// shell uses this rather than `generations.is_empty()` so an unreachable
    /// destination is never mistaken for a clean one.
    pub fn has_recoverable(&self) -> bool {
        self.status == GenerationsStatus::Listed && !self.generations.is_empty()
    }
}

/// What a restore attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The generation was promoted back to live.
    Restored,
    /// The destination answered, and holds no such retained generation for this
    /// owner — an unknown manifest, or one already reclaimed past `T`.
    ///
    /// **Not a failure.** It is the outcome of asking too late, and a shell must
    /// say so ("this version is past the recovery window") rather than reporting
    /// a broken restore; the two need different words because only one of them
    /// is worth retrying.
    NoSuchGeneration,
}

/// List every configured destination's retained generations, in config order.
///
/// Never fails wholesale: a destination that cannot be reached degrades to
/// [`GenerationsStatus::Unreachable`] in its own group, so one dead destination
/// can neither blank the others nor read as "nothing to recover". This mirrors
/// [`crate::trust::backup_trust_rows`]' per-destination degrade, for the same
/// reason — the facet's job is to tell the truth about each destination
/// independently.
///
/// `destinations` is the client's own pinned `fauna.state.backup` destination list,
/// source-untrusted by construction.
pub async fn list_retained_generations(
    destinations: &[BackupDestination],
    connector: &dyn BackupDestinationConnector,
) -> Vec<DestinationGenerations> {
    // One group per DESTINATION, matching this function's own doc — a
    // destination with N covered folders has N+1 rows sharing one
    // `destination_id` (`fauna_core::data::distinct_destinations`'s own doc), and a naive per-row loop would both over-query and list
    // the same destination's generations N+1 times.
    let destinations = fauna_core::data::distinct_destinations(destinations);
    let mut out = Vec::with_capacity(destinations.len());
    for dest in &destinations {
        let read = read_one(dest, connector).await;
        let (status, grace_secs, generations) = match read {
            Ok((grace_secs, generations)) => (GenerationsStatus::Listed, grace_secs, generations),
            Err(_) => (GenerationsStatus::Unreachable, 0, Vec::new()),
        };
        out.push(DestinationGenerations {
            destination_id: dest.destination_id.clone(),
            destination_url: dest.destination_nest_url.clone(),
            destination_label: fauna_core::format::backup_destination_label(
                dest.display_name.as_deref(),
                &dest.destination_nest_url,
            ),
            status,
            grace_secs,
            generations,
        });
    }
    out
}

/// One destination's read, projected. Split out so the caller's degrade path is
/// a single `Err` arm.
///
/// **This is the paged walk** (`transport.md` § Max frame corollary — a
/// retained-generation storm is exactly when this read matters, and an unpaged
/// serve breaks the 2 MiB frame at a few thousand rows). Pages follow the
/// server-minted `next_cursor` to **absence** — the cursor's tiebreaker is a
/// rowid no client can see, so absence (not an empty or short page) is the
/// drained signal; the last page mints none, so a reply without one ends
/// the walk. Contract violations (a repeated cursor,
/// an empty page claiming more, or the walk running past
/// [`crate::cursor::MAX_PAGES_PER_DESTINATION_WALK`] pages — a destination
/// alternating cursors never repeats one, so it needs its own bound) degrade
/// to `Err` — the caller renders `Unreachable`, failing toward "could not
/// ask", never toward a false "nothing to recover".
async fn read_one(
    destination: &BackupDestination,
    connector: &dyn BackupDestinationConnector,
) -> Result<(i64, Vec<RetainedGeneration>), String> {
    let seam = connect_destination(connector, destination).await?;
    // One walk, shared with the audit's inclusion arm
    // (`crate::audit::read_full_generations`), which needs the same rows to
    // tell a just-compacted path from a de-listed one.
    let (grace_secs, items) = crate::audit::read_full_generations(seam.as_ref()).await?;
    let generations = items
        .into_iter()
        .map(|g| RetainedGeneration {
            // Saturating so a nonsense clock from a destination cannot panic a
            // client that is, by construction, talking to a box it may not trust.
            expires_at: g.superseded_at.saturating_add(grace_secs),
            folder_name: g.folder_name,
            path: g.path,
            path_hash: g.path_hash,
            manifest_hash: g.manifest_hash,
            size_bytes: g.size_bytes,
            superseded_at: g.superseded_at,
        })
        .collect();
    Ok((grace_secs, generations))
}

/// Promote one retained generation back to live, spoken over the destination's
/// own connection. Operable with the source nest hostile — which is the entire
/// point of the surface.
///
/// The address triple comes from the same [`list_retained_generations`] read
/// that built the row; a shell never assembles one itself.
pub async fn restore_generation(
    connector: &dyn BackupDestinationConnector,
    destination: &BackupDestination,
    generation: &RetainedGeneration,
) -> Result<RestoreOutcome, String> {
    let reply = connect_destination(connector, destination)
        .await?
        .generation_restore(
            generation.folder_name.clone(),
            generation.path_hash.clone(),
            generation.manifest_hash.clone(),
        )
        .await?;
    Ok(if reply.restored {
        RestoreOutcome::Restored
    } else {
        RestoreOutcome::NoSuchGeneration
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{BackupNestSeam, DestinationConnection};
    use async_trait::async_trait;
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::{
        BackupStatusReply, CustodyListReply, GenerationItem, GenerationListReply,
        GenerationRestoreReply, WriterGrantListReply,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A canned destination: answers `generation.list`, records every restore it
    /// is asked for, and panics on every *source*-nest method — so a call that
    /// wandered onto the wrong nest fails loudly rather than silently working.
    struct MockDestination {
        generations: Vec<GenerationItem>,
        grace_secs: i64,
        /// Manifest hashes this destination will actually restore. Anything else
        /// answers `restored: false`, the past-`T` / unknown case.
        restorable: Vec<String>,
        restores: Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl BackupNestSeam for MockDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("a destination is never asked for backup status")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("the seal grant lives at the source, never at a destination")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            panic!("generation recovery reads generations, not grants")
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("generation recovery never revokes — that is the trust facet's write")
        }
        async fn custody_list(&self, _cursor: Option<String>) -> Result<CustodyListReply, String> {
            panic!("live custody is the audit's read, not the recovery surface's")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<GenerationListReply, String> {
            // A single complete reply with no `next_cursor` — the drained last
            // page, which the walk must treat as drained after one page.
            Ok(GenerationListReply {
                generations: self.generations.clone(),
                grace_secs: self.grace_secs,
                next_cursor: None,
                extra: Default::default(),
            })
        }
        async fn generation_restore(
            &self,
            folder_name: String,
            path_hash: String,
            manifest_hash: String,
        ) -> Result<GenerationRestoreReply, String> {
            self.restores
                .lock()
                .unwrap()
                .push((folder_name, path_hash, manifest_hash.clone()));
            Ok(GenerationRestoreReply {
                restored: self.restorable.contains(&manifest_hash),
                extra: Default::default(),
            })
        }
    }

    /// Maps URL → canned destination; an unmapped URL fails the connect, which
    /// is the `unreachable` path.
    #[derive(Default)]
    struct MockConnector {
        by_url: HashMap<String, Arc<MockDestination>>,
        /// Every URL a connect was attempted against, so a test can prove the
        /// source nest's URL was never dialled.
        dialled: Mutex<Vec<String>>,
        /// The identity every connection proves — [`ENROLLED_ID`] unless set:
        /// the box answering the URL is then not the one the owner enrolled.
        proves: Option<[u8; 32]>,
    }

    /// The identity every [`dest`] row enrolled, and the one an honest
    /// [`MockConnector`] destination proves.
    const ENROLLED_ID: [u8; 32] = [7u8; 32];

    impl MockConnector {
        fn with(mut self, url: &str, generations: Vec<GenerationItem>, grace_secs: i64) -> Self {
            let restorable = generations
                .iter()
                .map(|g| g.manifest_hash.clone())
                .collect();
            self.by_url.insert(
                url.to_string(),
                Arc::new(MockDestination {
                    generations,
                    grace_secs,
                    restorable,
                    restores: Mutex::new(Vec::new()),
                }),
            );
            self
        }
    }

    #[async_trait]
    impl BackupDestinationConnector for MockConnector {
        async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
            self.dialled.lock().unwrap().push(url.to_string());
            self.by_url
                .get(url)
                .map(|d| DestinationConnection {
                    seam: d.clone() as Arc<dyn BackupNestSeam>,
                    bound_nest_id: self.proves.unwrap_or(ENROLLED_ID),
                })
                .ok_or_else(|| format!("unreachable: {url}"))
        }
    }

    const T_30D: i64 = 30 * 24 * 60 * 60;
    const SOURCE_URL: &str = "https://source.example";

    fn dest(id: &str, url: &str, name: Option<&str>) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: url.into(),
            destination_actor_pubkey: ENROLLED_ID,
            folder_name: "__mail".into(),
            added_at: 0,
            display_name: name.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn gen_item(path: Option<&str>, manifest: &str, superseded_at: i64) -> GenerationItem {
        GenerationItem {
            folder_name: "__mail".into(),
            path: path.map(|s| s.to_string()),
            path_hash: format!("{manifest}-pathhash"),
            manifest_hash: manifest.into(),
            size_bytes: 4096,
            superseded_at,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_retained_generation_expires_at_the_window_the_destination_reported() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "aaaa", 1_700_000_000)],
            T_30D,
        );
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", Some("Aunt's nest"))],
            &conn,
        ));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].status, GenerationsStatus::Listed);
        assert_eq!(groups[0].grace_secs, T_30D);
        assert_eq!(groups[0].destination_label, "Aunt's nest");
        let g = &groups[0].generations[0];
        assert_eq!(g.superseded_at, 1_700_000_000);
        assert_eq!(
            g.expires_at,
            1_700_000_000 + T_30D,
            "the deadline is the nest's window, never a client constant"
        );
    }

    /// A destination with an attached folder has two rows sharing
    /// one `destination_id` — the enrollment row plus a coverage clone — and
    /// this function's own doc promises "every configured destination", once
    /// each.
    #[test]
    fn a_destination_with_a_covered_folder_gets_one_group_not_one_per_coverage_row() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "aaaa", 1_700_000_000)],
            T_30D,
        );
        let enrolled = dest("d1", "https://aunt.example", Some("Aunt's nest"));
        let covered = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            ..enrolled.clone()
        };
        let groups = block_on(list_retained_generations(&[enrolled, covered], &conn));

        assert_eq!(groups.len(), 1);
    }

    /// The whole reason `grace_secs` rides the wire: a destination on a nest with
    /// a different window must render *its* deadline. A client that hard-coded
    /// 30 d would fail exactly this.
    #[test]
    fn a_destination_reporting_a_different_window_drives_its_own_deadline() {
        let seven_days = 7 * 24 * 60 * 60;
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "aaaa", 1_000)],
            seven_days,
        );
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(groups[0].grace_secs, seven_days);
        assert_eq!(groups[0].generations[0].expires_at, 1_000 + seven_days);
    }

    /// `path` is scrubbed away on some rows (S9 nulls it beside `path_sealed`). Such a generation is exactly
    /// the kind a rogue source produces, so dropping it would silently remove the
    /// user's only route back — the restore address is `path_hash`, which is
    /// always present.
    #[test]
    fn a_path_less_generation_is_still_listed_and_still_restorable() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(None, "bbbb", 500)],
            T_30D,
        );
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(
            groups[0].generations.len(),
            1,
            "a path-less row is not a skip"
        );
        let g = &groups[0].generations[0];
        assert_eq!(g.path, None);
        assert_eq!(g.path_hash, "bbbb-pathhash");

        let outcome = block_on(restore_generation(
            &conn,
            &dest("d1", "https://aunt.example", None),
            g,
        ))
        .expect("restore");
        assert_eq!(outcome, RestoreOutcome::Restored);
        assert_eq!(
            conn.by_url["https://aunt.example"]
                .restores
                .lock()
                .unwrap()
                .as_slice(),
            &[("__mail".into(), "bbbb-pathhash".into(), "bbbb".into())],
            "addressed by path_hash, which a path-less row still carries"
        );
    }

    #[test]
    fn an_unreachable_destination_degrades_only_its_own_group() {
        let conn = MockConnector::default().with(
            "https://up.example",
            vec![gen_item(Some("inbox/1"), "cccc", 9)],
            T_30D,
        );
        let groups = block_on(list_retained_generations(
            &[
                dest("down", "https://down.example", None),
                dest("up", "https://up.example", None),
            ],
            &conn,
        ));

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].status, GenerationsStatus::Unreachable);
        assert!(groups[0].generations.is_empty());
        assert!(
            !groups[0].has_recoverable(),
            "unreachable is never 'nothing to recover'"
        );
        assert_eq!(
            groups[1].status,
            GenerationsStatus::Listed,
            "a dead sibling must not blank a live destination"
        );
        assert!(groups[1].has_recoverable());
    }

    /// The healthy steady state, and the one that must not be confused with the
    /// test above: the destination answered, and holds nothing superseded.
    #[test]
    fn a_destination_holding_nothing_superseded_is_listed_not_unreachable() {
        let conn = MockConnector::default().with("https://aunt.example", vec![], T_30D);
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(groups[0].status, GenerationsStatus::Listed);
        assert!(!groups[0].has_recoverable());
        assert_eq!(groups[0].grace_secs, T_30D);
    }

    /// The load-bearing invariant of the whole surface. The source nest is the
    /// writer being recovered from, so neither call may touch it — mutation-check
    /// by pointing either call at `SOURCE_URL` and watching the mock panic.
    #[test]
    fn the_list_is_spoken_to_each_destination_and_never_to_the_source() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "dddd", 1)],
            T_30D,
        );
        let _ = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        let dialled = conn.dialled.lock().unwrap().clone();
        assert_eq!(dialled.as_slice(), &["https://aunt.example".to_string()]);
        assert!(
            !dialled.iter().any(|u| u == SOURCE_URL),
            "the source nest must never be dialled for a recovery read"
        );
    }

    #[test]
    fn a_restore_is_spoken_to_the_destination_not_the_source() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "eeee", 1)],
            T_30D,
        );
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));
        conn.dialled.lock().unwrap().clear();

        let outcome = block_on(restore_generation(
            &conn,
            &dest("d1", "https://aunt.example", None),
            &groups[0].generations[0],
        ))
        .unwrap();

        assert_eq!(outcome, RestoreOutcome::Restored);
        assert_eq!(
            conn.dialled.lock().unwrap().as_slice(),
            &["https://aunt.example".to_string()],
            "the restore must land at the custody holder — the hostile-source path"
        );
    }

    /// Past-`T` (or unknown) is a product state, not a transport failure: a shell
    /// must be able to say "too late" without saying "it broke".
    #[test]
    fn restoring_a_reclaimed_generation_reports_no_such_generation_not_an_error() {
        let conn = MockConnector::default().with("https://aunt.example", vec![], T_30D);
        let stale = RetainedGeneration {
            folder_name: "__mail".into(),
            path: Some("inbox/1".into()),
            path_hash: "gone-pathhash".into(),
            manifest_hash: "gone".into(),
            size_bytes: 1,
            superseded_at: 0,
            expires_at: T_30D,
        };

        let outcome = block_on(restore_generation(
            &conn,
            &dest("d1", "https://aunt.example", None),
            &stale,
        ))
        .expect("a reclaimed generation is not a transport failure");
        assert_eq!(outcome, RestoreOutcome::NoSuchGeneration);
    }

    /// A box answering the destination URL but proving another
    /// identity than the one enrolled is not the custody holder: its
    /// generation list is not shown as the destination's, and a restore is
    /// never spoken to it.
    #[test]
    fn a_destination_proving_a_different_identity_than_enrolled_is_refused() {
        let mut conn = MockConnector::default().with(
            "https://aunt.example",
            vec![gen_item(Some("inbox/1"), "m1", 10)],
            T_30D,
        );
        conn.proves = Some([9u8; 32]);
        let enrolled = dest("d1", "https://aunt.example", None);

        let groups = block_on(list_retained_generations(
            std::slice::from_ref(&enrolled),
            &conn,
        ));
        assert_eq!(groups[0].status, GenerationsStatus::Unreachable);
        assert!(groups[0].generations.is_empty());

        let g = RetainedGeneration {
            folder_name: "__mail".into(),
            path: None,
            path_hash: "x".into(),
            manifest_hash: "m1".into(),
            size_bytes: 1,
            superseded_at: 0,
            expires_at: T_30D,
        };
        block_on(restore_generation(&conn, &enrolled, &g))
            .expect_err("a restore must never be spoken to a box that is not the destination");
        assert!(
            conn.by_url["https://aunt.example"]
                .restores
                .lock()
                .unwrap()
                .is_empty(),
            "the impostor was never asked to restore"
        );
    }

    #[test]
    fn restoring_at_an_unreachable_destination_reports_the_failure() {
        let conn = MockConnector::default();
        let g = RetainedGeneration {
            folder_name: "__mail".into(),
            path: None,
            path_hash: "x".into(),
            manifest_hash: "y".into(),
            size_bytes: 1,
            superseded_at: 0,
            expires_at: T_30D,
        };
        let err = block_on(restore_generation(
            &conn,
            &dest("d1", "https://gone.example", None),
            &g,
        ))
        .expect_err("an unreachable destination must not report a silent success");
        assert!(
            err.contains("gone.example"),
            "error names the destination: {err}"
        );
    }

    /// Newest-supersede-first is the nest's ordering rule; re-sorting per client
    /// is exactly the drift priority #4 forbids.
    #[test]
    fn the_destinations_ordering_is_preserved_rather_than_re_sorted() {
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![
                gen_item(Some("newest"), "n", 3_000),
                gen_item(Some("middle"), "m", 2_000),
                gen_item(Some("oldest"), "o", 1_000),
            ],
            T_30D,
        );
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        let paths: Vec<_> = groups[0]
            .generations
            .iter()
            .map(|g| g.path.clone().unwrap())
            .collect();
        assert_eq!(paths, vec!["newest", "middle", "oldest"]);
    }

    #[test]
    fn a_labelless_destination_falls_back_to_the_shared_label_helper() {
        let conn = MockConnector::default().with("https://aunt.example", vec![], T_30D);
        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(
            groups[0].destination_label,
            fauna_core::format::backup_destination_label(None, "https://aunt.example"),
            "never re-derive the label locally"
        );
    }

    #[test]
    fn no_configured_destinations_yields_no_groups_and_no_dials() {
        let conn = MockConnector::default();
        let groups = block_on(list_retained_generations(&[], &conn));
        assert!(groups.is_empty());
        assert!(conn.dialled.lock().unwrap().is_empty());
    }

    // ── the paged walk ──────────────────────────────────────────────────────

    /// Serves `generation.list` in scripted pages: each entry is
    /// `(rows, next_cursor)`, keyed by the cursor the caller presented. The
    /// walk must stitch every page — a storm's rows live past page one.
    struct PagedDestination {
        /// `pages[i]` answers the request whose cursor is `expect[i]`.
        expect: Vec<Option<String>>,
        pages: Vec<(Vec<GenerationItem>, Option<String>)>,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl BackupNestSeam for PagedDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("not part of the walk")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("not part of the walk")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            panic!("not part of the walk")
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("not part of the walk")
        }
        async fn custody_list(&self, _cursor: Option<String>) -> Result<CustodyListReply, String> {
            panic!("not part of the walk")
        }
        async fn generation_list(
            &self,
            cursor: Option<String>,
        ) -> Result<GenerationListReply, String> {
            let mut calls = self.calls.lock().unwrap();
            let i = *calls;
            *calls += 1;
            assert_eq!(
                cursor, self.expect[i],
                "page {i} was requested with the wrong cursor"
            );
            let (rows, next_cursor) = self.pages[i].clone();
            Ok(GenerationListReply {
                generations: rows,
                grace_secs: T_30D,
                next_cursor,
                extra: Default::default(),
            })
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<GenerationRestoreReply, String> {
            panic!("not part of the walk")
        }
    }

    struct OneSeamConnector {
        seam: Arc<PagedDestination>,
    }

    #[async_trait]
    impl BackupDestinationConnector for OneSeamConnector {
        async fn connect(&self, _url: &str) -> Result<DestinationConnection, String> {
            Ok(DestinationConnection {
                seam: self.seam.clone() as Arc<dyn BackupNestSeam>,
                bound_nest_id: ENROLLED_ID,
            })
        }
    }

    #[test]
    fn the_walk_stitches_every_page_and_stops_at_the_absent_cursor() {
        let conn = OneSeamConnector {
            seam: Arc::new(PagedDestination {
                expect: vec![None, Some("p1".into()), Some("p2".into())],
                pages: vec![
                    (
                        vec![gen_item(Some("a"), "m1", 30), gen_item(Some("b"), "m2", 20)],
                        Some("p1".into()),
                    ),
                    (vec![gen_item(Some("c"), "m3", 10)], Some("p2".into())),
                    (vec![gen_item(Some("d"), "m4", 5)], None),
                ],
                calls: Mutex::new(0),
            }),
        };

        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(groups[0].status, GenerationsStatus::Listed);
        assert_eq!(
            groups[0]
                .generations
                .iter()
                .map(|g| g.manifest_hash.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2", "m3", "m4"],
            "every page's rows, in serve order — a walk that stops at the \
             first (or a short) page hides exactly the rows a storm produced"
        );
        assert_eq!(groups[0].grace_secs, T_30D);
    }

    #[test]
    fn a_page_that_repeats_the_cursor_degrades_to_unreachable_not_a_partial_listing() {
        // A cursor-ignoring nest that nonetheless mints `next_cursor` (or any
        // buggy/hostile pager) would loop the walk forever. The guard stops it
        // and fails toward "could not ask" — a partial set rendered as Listed
        // would be the same false reassurance the storm defect produced.
        let conn = OneSeamConnector {
            seam: Arc::new(PagedDestination {
                expect: vec![None, Some("same".into())],
                pages: vec![
                    (vec![gen_item(Some("a"), "m1", 30)], Some("same".into())),
                    (vec![gen_item(Some("a"), "m1", 30)], Some("same".into())),
                ],
                calls: Mutex::new(0),
            }),
        };

        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(groups[0].status, GenerationsStatus::Unreachable);
        assert!(groups[0].generations.is_empty());
    }

    /// The repeated-cursor guard above remembers exactly one step back, so a
    /// destination alternating two (or more) distinct cursors passes it on
    /// every page — `next` never equals the cursor just sent — while never
    /// draining. Only the page-count ceiling in
    /// [`crate::cursor`] stops this one.
    #[test]
    fn a_two_cursor_cycle_is_bounded_not_infinite() {
        let mut expect = vec![None];
        let mut pages = Vec::new();
        for i in 0..crate::cursor::MAX_PAGES_PER_DESTINATION_WALK {
            let next = if i % 2 == 0 { "b" } else { "a" };
            pages.push((
                vec![gen_item(Some("x"), "m", 30 - i as i64)],
                Some(next.to_string()),
            ));
            expect.push(Some(next.to_string()));
        }
        let conn = OneSeamConnector {
            seam: Arc::new(PagedDestination {
                expect,
                pages,
                calls: Mutex::new(0),
            }),
        };

        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(
            groups[0].status,
            GenerationsStatus::Unreachable,
            "a cycling destination must degrade, not hang or complete short"
        );
        assert!(groups[0].generations.is_empty());
    }

    #[test]
    fn an_empty_page_claiming_more_rows_degrades_to_unreachable() {
        let conn = OneSeamConnector {
            seam: Arc::new(PagedDestination {
                expect: vec![None],
                pages: vec![(vec![], Some("p1".into()))],
                calls: Mutex::new(0),
            }),
        };

        let groups = block_on(list_retained_generations(
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ));

        assert_eq!(groups[0].status, GenerationsStatus::Unreachable);
    }
}
