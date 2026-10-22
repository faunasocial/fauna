//! Message-kind snapshot capture — the ONE nest-side function both creators
//! call (`docs/goal/behavior/backup-restore.md` § 5 → *Creators*).
//!
//! The `fauna.filesync.snapshot.create_message_kind` handler (the Backups
//! page's create action) and the snapshot scheduler's automatic arm
//! ([`crate::backup::scheduler::SnapshotScheduler::check_all_message_kinds`])
//! both capture through [`create_message_kind_snapshot`], so neither can drift
//! from the other — the folder shape, where one capture serves the scheduler
//! and the `create_folder` handler alike. The handler keeps only decode, the
//! pure-backup gate, authorization and reply shaping.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fauna_segment_store::{KindManifest, SegmentManager, VersionedManifest};

use crate::db::CacheDb;
use crate::segments::{
    CalPlacementSegmentManager, CardPlacementSegmentManager, MailPlacementSegmentManager,
};

/// Every message kind the nest snapshots, in the order the automatic arm
/// visits them. The kind table: `message-segment-store.md` § Layout.
pub const MESSAGE_KINDS: &[&str] = &["mail", "post", "calendar", "card", "conv"];

/// The segment stores a message-kind capture reads — the content store of
/// every kind and the placement journal of the three kinds that have one.
/// Cloned out of [`crate::routes::AppState`] so the scheduler, which holds no
/// `AppState`, captures through the same stores the handler does.
#[derive(Clone)]
pub struct MessageKindStores {
    pub mail: Arc<SegmentManager>,
    pub post: Arc<SegmentManager>,
    pub calendar: Arc<SegmentManager>,
    pub card: Arc<SegmentManager>,
    pub conv: Arc<SegmentManager>,
    pub mail_placement: Arc<MailPlacementSegmentManager>,
    pub cal_placement: Arc<CalPlacementSegmentManager>,
    pub card_placement: Arc<CardPlacementSegmentManager>,
}

impl MessageKindStores {
    pub fn of(state: &crate::routes::AppState) -> Self {
        Self {
            mail: state.mail_segments.clone(),
            post: state.post_segments.clone(),
            calendar: state.cal_segments.clone(),
            card: state.card_segments.clone(),
            conv: state.conv_segments.clone(),
            mail_placement: state.mail_placement.clone(),
            cal_placement: state.cal_placement.clone(),
            card_placement: state.card_placement.clone(),
        }
    }

    /// The kind's content store, or `None` for a kind the nest does not
    /// snapshot.
    pub fn content(&self, kind: &str) -> Option<&Arc<SegmentManager>> {
        match kind {
            "mail" => Some(&self.mail),
            "post" => Some(&self.post),
            "calendar" => Some(&self.calendar),
            "card" => Some(&self.card),
            "conv" => Some(&self.conv),
            _ => None,
        }
    }
}

/// One capture's result: the snapshot row and the reserved row it hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageKindCapture {
    pub folder_id: i64,
    pub snapshot_id: i64,
}

/// Capture one `(scope, kind)`: pin the kind's content manifest and, for mail,
/// calendar and card, its placement manifest, on the scope's reserved
/// `__<kind>` row (conv: `__conv/<channel_hex>`, scope = the channel), and
/// record the scope's content cursor on the row (`snapshots.max_change_seq`).
///
/// No authorization and no pure-backup gate here — those are the caller's
/// (the handler authorizes the bearer, the scheduler captures as the nest).
pub async fn create_message_kind_snapshot(
    db: &CacheDb,
    stores: &MessageKindStores,
    scope: &[u8; 32],
    kind: &str,
) -> Result<MessageKindCapture> {
    let Some(content_store) = stores.content(kind) else {
        bail!("not a snapshotted message kind: {kind:?}");
    };

    // The cursor is read BEFORE the manifests: a record appended between the
    // two reads is then in the pin but past the recorded cursor, which costs at
    // most one redundant capture next hour — read after, it could be under the
    // cursor yet missing from the pin, and no capture would ever take it.
    let cursor = db.message_kind_change_cursor(scope, kind).await?;

    // finalize_open before load_manifest on every store, so the captured pin
    // reflects all appended-but-unflushed records.
    content_store
        .finalize_open(scope)
        .await
        .with_context(|| format!("finalize open {kind} segment"))?;
    let m = content_store
        .load_manifest(scope)
        .await
        .with_context(|| format!("load {kind} manifest"))?;
    let content = fauna_core::encoding::canonical_encode(&m)
        .map_err(|e| anyhow::anyhow!("{kind} Manifest: {e}"))?;

    // Mail, calendar and card pin the placement manifest with the content one
    // (§ 5: a snapshot without either half is un-restorable); post and conv
    // have no placement layer.
    let placement: Option<Vec<u8>> = match kind {
        "mail" => {
            stores.mail_placement.finalize_open(scope).await?;
            let pm = stores.mail_placement.load_manifest(scope).await?;
            Some(
                fauna_core::encoding::canonical_encode(&pm)
                    .map_err(|e| anyhow::anyhow!("MailPlacementManifest: {e}"))?,
            )
        }
        "calendar" => {
            stores.cal_placement.finalize_open(scope).await?;
            let pm = stores.cal_placement.load_manifest(scope).await?;
            Some(
                pm.encode()
                    .map_err(|e| anyhow::anyhow!("CalPlacementManifest: {e}"))?,
            )
        }
        "card" => {
            stores.card_placement.finalize_open(scope).await?;
            let pm = stores.card_placement.load_manifest(scope).await?;
            Some(
                pm.encode()
                    .map_err(|e| anyhow::anyhow!("CardPlacementManifest: {e}"))?,
            )
        }
        _ => None,
    };

    let folder_id = if kind == "conv" {
        db.get_or_create_reserved_conv_folder(scope).await?
    } else {
        db.get_or_create_reserved_folder(scope, kind).await?
    };
    let snapshot_id = db
        .create_message_kind_snapshot_row_at_cursor(
            folder_id,
            kind,
            Some(&content),
            placement.as_deref(),
            Some(cursor),
        )
        .await?;
    Ok(MessageKindCapture {
        folder_id,
        snapshot_id,
    })
}

/// Has the scope's placement state moved since `pinned` (the placement
/// manifest its newest snapshot holds)? `false` for a kind with no placement
/// layer. Reads the on-disk manifest WITHOUT finalizing — a check must not
/// rotate segments it does not capture.
///
/// The comparison ignores the manifest's segment bookkeeping
/// (`kind_manifest`): a finalize or a compaction moves that without moving a
/// single flag, mailbox or event, and counting it would cut hourly captures of
/// a scope nothing changed in. Everything else the manifest carries — mail's
/// `highestmodseq` and placements, the DAV stores' sync state, every
/// tombstone — is the placement cursor § 5 → *Creators* names. A pinned blob
/// that no longer decodes counts as moved (capture rather than guess).
pub async fn placement_changed(
    stores: &MessageKindStores,
    scope: &[u8; 32],
    kind: &str,
    pinned: Option<&[u8]>,
) -> Result<bool> {
    fn differs<M: VersionedManifest + PartialEq>(
        live: M,
        pinned: Option<&[u8]>,
        strip: impl Fn(&mut M),
    ) -> bool {
        let Some(pinned) = pinned else { return true };
        let Ok(mut pinned) = M::decode_any_version(pinned) else {
            return true;
        };
        let mut live = live;
        strip(&mut live);
        strip(&mut pinned);
        live != pinned
    }
    Ok(match kind {
        "mail" => differs(
            stores.mail_placement.load_manifest(scope).await?,
            pinned,
            |m| m.kind_manifest = KindManifest::empty(),
        ),
        "calendar" => differs(
            stores.cal_placement.load_manifest(scope).await?,
            pinned,
            |m| m.kind_manifest = KindManifest::empty(),
        ),
        "card" => differs(
            stores.card_placement.load_manifest(scope).await?,
            pinned,
            |m| m.kind_manifest = KindManifest::empty(),
        ),
        _ => false,
    })
}

/// Verify a message-kind snapshot's pinned content segments are on disk — the
/// first step every message-kind restore runs, and what a `check` on a kind
/// row reports (`ui/backups.md` § Snapshot-list shape → *Kind entries*: a kind
/// row has no `snapshot_files`, so the folder chunk walk has nothing to read).
/// Returns the pinned segment ids missing from the scope's directory.
pub fn missing_pinned_segments(
    content_store: &SegmentManager,
    scope: &[u8; 32],
    content_manifest: &fauna_segment_store::Manifest,
) -> Vec<u32> {
    let root = content_store.scope_dir(scope);
    content_manifest
        .kind_manifest
        .live_segments
        .iter()
        .copied()
        .filter(|seg_id| !root.join(format!("seg-{seg_id:08}.dat")).exists())
        .collect()
}
