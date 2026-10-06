//! Cross-set media-listing WS-RPC handler — `fauna.media.list`.
//!
//! The Media page is the media-optimized **view** over the user's folders
//! (`docs/goal/ui/media.md`); its default all-media view aggregates media items
//! across **every folder the caller may read**. No per-set surface
//! (`fauna.sync.files`) or set-list surface (`fauna.folders.list` /
//! `fauna.sync.backup_status`) returns that union, so this is the net-new
//! aggregating read `media.md` § State & data shape flagged (spec O-4, settled
//! here at nest implementation). It rides the **bearer** connection
//! (actor-scoped); gate `User | Admin` — enforced in
//! `bridge_method_allowlist::is_permitted`.
//!
//! ## How it aggregates
//!
//! `folder_authz::enumerate_readable_folders` returns exactly the sets the
//! caller may read (owned + group-member, the same S2-P3 roster boundary the
//! per-name `fauna.sync.files` read applies — plus, for a nest **admin**, every
//! group-bound set's discovery metadata, a Q5 grant that intentionally exceeds
//! the admin-less per-name reads; bytes stay group-key-sealed regardless, and
//! `MediaItem` omits `manifest_hash`); per set we list its current files
//! (`CacheDb::get_files_for_folder`, the same query `fauna.sync.files` uses)
//! and stamp each with the set's **content reachability**
//! (`chunk_relay::folder_content_reachable`, the one verdict
//! `SyncStatusReply.source_online` also carries — owner `file-sync.md`
//! § Content reachability).
//! Items are returned in stable `(folder, path)` order with **keyset**
//! pagination (`cursor` / `next_cursor` + `limit`) so a client pulls the full
//! cross-set snapshot in bounded pages even for a photo-library-scale set.
//!
//! ## Scope — every user folder is the media library
//!
//! Media is the user's **media library**, so it aggregates every readable
//! folder, website-published ones included: a folder has no type, and the
//! website toggle fans the head out to `web_files` *in addition to* the head
//! feed (`web-content-hosting.md` § Content model), never instead of it.
//! Reserved `__*` sets are already excluded by `enumerate_readable_folders`.
//!
//! Every folder records to the `sync_changes` head feed (the phase 3 head
//! unification, 2026-08-17, `file-sync.md` § Membership), so
//! `get_files_for_folder` is the one projection — the Apple "Photo Library"
//! surfaces in the media library through the same read as any other folder.
//!
//! ## Replay semantics
//!
//! `forbid_replay = false` @5 s — a pure read with no side effects. Replay
//! semantics + rationale mirror the sibling sync reads: see
//! `KindRegistry::register_media_kinds`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use fauna_protocol::media::{
    MEDIA_LIST_CURSOR_V2, MEDIA_LIST_DEFAULT_LIMIT, MEDIA_LIST_MAX_LIMIT, MediaItem,
    MediaListReply, MediaListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for the `fauna.media.*` kinds.
const MEDIA: &str = "media";

// ── error / encode helpers (mirror sync_handlers) ──────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn coded(code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns(MEDIA, code, detail)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(MEDIA, e)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── keyset cursor over the stable (folder, path) order ───────────────────────

/// The opaque `fauna.media.list` cursor — the sort key of the last item a prior
/// page returned. Encoded as canonical-CBOR hex so its internals stay private to
/// the nest (a client only round-trips the opaque string).
///
/// Only the `v2` order is served
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths → *Migration*):
/// the `(folder_id, path_hash)` key that outlives the plaintext scrub; the
/// original plaintext `(folder, path)` `v1` order is retired.
///
/// The `version` discriminant is inside the encoded cursor as well as on the
/// request, so a cursor presented under the wrong order is caught rather than
/// silently mis-paging a listing.
///
/// ## Why v2 pages on `folder_id` and not on the set's `name_hash`
///
/// v2 shipped (S2b) keyed by `set_name_hash(name)`, chosen so the order survives
/// the plaintext scrub. It does — but the cursor is handed to the caller, and
/// that digest is an **unkeyed** hash of a user-chosen, dictionary-shaped string,
/// so a nest admin paging a set they hold no key for recovers the set's name
/// offline (security finding). A keyed digest would fix it, but this nest
/// has no persistent server secret to key it with, and minting one buys nothing
/// here: the cursor is **nest-minted and nest-consumed** — clients treat it as an
/// opaque string and only replay it — so its set component needs *stability*, not
/// *convergence* with any client-side derivation. `folders.id` is already
/// stable, nest-local and says nothing about the name, so it is strictly the
/// better key.
///
/// Reshaped in place rather than minting a v3 (v2 is days old, and a third
/// permanent pagination order is a cold-read cost forever). A v2 cursor carrying
/// the old `folder_hash` shape is refused `invalid_cursor` — see
/// [`decode_cursor`] — never silently mis-paged.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorKey {
    /// [`MEDIA_LIST_CURSOR_V2`] (the only order served).
    #[serde(default)]
    version: u32,
    /// v2: the last item's set row id — nest-local, stable, and (unlike the
    /// `folder_hash` this replaced) not a recoverable digest of the set name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder_id: Option<i64>,
    /// v2: the last item's `path_hash` — a nest-side sort/skip position key,
    /// which must stay the *true* hash regardless of the reader's audience or
    /// the keyset skip would land in the wrong place.
    ///
    /// **That is safe because the encoded cursor is sealed** under a subkey of
    /// this nest's durable deployment key before it ever leaves the process
    /// (`crate::cursor_seal`), so no holder — audience or not — can read this
    /// field. Closing finding leg (a), path-sealing S5e: S5d withheld
    /// `MediaItem.path_hash` from a non-audience (Q5 admin) reader while the
    /// cursor minted beside it still handed the same digest back, item for item,
    /// to anyone paging at `limit = 1`.
    ///
    /// ⚠ The two prior slices recorded that fixing this needed "a persistent
    /// nest secret `bins/fauna-nest` does not have". **That was wrong** — the
    /// deployment key is exactly that secret; see `cursor_seal`'s module doc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path_hash: Option<serde_bytes::ByteBuf>,
}

/// One item's position in the active order — the comparison key the sort and
/// the keyset skip both use, so they can never disagree about the order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    V2(i64, Vec<u8>),
}

fn encode_cursor(seal_key: &[u8; 32], key: &SortKey) -> String {
    let cursor = match key {
        SortKey::V2(folder_id, path_hash) => CursorKey {
            version: MEDIA_LIST_CURSOR_V2,
            folder_id: Some(*folder_id),
            path_hash: Some(serde_bytes::ByteBuf::from(path_hash.clone())),
        },
    };
    // Infallible for this plain struct, and the seal is infallible for a valid
    // 32-byte key; fall back to an empty (start) cursor rather than failing a
    // page — an empty cursor restarts the listing, the same loud-but-safe
    // degrade a refused cursor gets.
    encode_canonical(&cursor)
        .ok()
        .and_then(|v| crate::cursor_seal::seal(seal_key, &v).ok())
        .map(hex::encode)
        .unwrap_or_default()
}

/// Decode a cursor **and** check it belongs to the order this request asked
/// for. A cursor of another order replayed under this one would otherwise skip
/// or duplicate an arbitrary slice of the library, silently — so it is a typed
/// `invalid_cursor` instead.
fn decode_cursor(seal_key: &[u8; 32], s: &str, want_version: u32) -> Result<SortKey, RpcError> {
    let sealed = hex::decode(s).map_err(|e| coded("invalid_cursor", e))?;
    // Fails closed on a cursor this nest did not mint: a foreign nest's, one
    // from before the deployment key rotated, a tampered string, or a plaintext
    // cursor minted before S5e sealed them. Restart the listing.
    let bytes = crate::cursor_seal::open(seal_key, &sealed).map_err(|e| {
        coded(
            "invalid_cursor",
            format!("{e} — restart the listing rather than replaying it"),
        )
    })?;
    let key = decode::<CursorKey>(&bytes).map_err(|e| coded("invalid_cursor", e))?;
    let have_version = key.version;
    if have_version != want_version {
        return Err(coded(
            "invalid_cursor",
            format!(
                "cursor was minted for pagination order v{have_version} but this request asked \
                 for v{want_version} — restart the listing rather than mixing orders"
            ),
        ));
    }
    // A v2 cursor minted before the set component moved off `name_hash`
    // decodes cleanly but orders against a different key space, so it
    // must be refused rather than paged: `folder_id` absent IS that
    // cursor, and the caller's fix is the same as for a mixed order.
    let folder_id = key.folder_id.ok_or_else(|| {
        coded(
            "invalid_cursor",
            "v2 cursor is missing `folder_id` — it was minted under the earlier v2 key \
             shape, whose order this listing no longer uses; restart the listing",
        )
    })?;
    let path_hash = key
        .path_hash
        .ok_or_else(|| coded("invalid_cursor", "v2 cursor is missing `path_hash`"))?;
    Ok(SortKey::V2(folder_id, path_hash.into_vec()))
}

// ── fauna.media.list (the cross-set all-media aggregate) ───────────────────────

/// The head row's writer-signed statement fields as `MediaItem` carries them —
/// all `None` for a reader outside the set's label audience.
#[derive(Default)]
struct SignedFields {
    manifest_hash: Option<fauna_protocol::ByteBuf>,
    device_id: Option<fauna_protocol::ByteBuf>,
    author_actor_id: Option<fauna_protocol::ByteBuf>,
    change_type: Option<String>,
    content_key_version: Option<u64>,
    derived_through: Option<i64>,
    is_resolution: Option<bool>,
    signature: Option<fauna_protocol::ByteBuf>,
    signer_key: Option<fauna_protocol::ByteBuf>,
}

fn media_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.media.list").await?;
            let req: MediaListRequest = decode(&payload).map_err(malformed)?;

            // Which pagination order to serve. An order this nest does not
            // serve is a typed error, never a silent downgrade to a different
            // order than the caller believes it is paging.
            let cursor_version = match req.cursor_version {
                MEDIA_LIST_CURSOR_V2 => MEDIA_LIST_CURSOR_V2,
                v => {
                    return Err(coded(
                        "invalid_cursor",
                        format!(
                            "unsupported cursor_version {v}; this nest serves \
                             v{MEDIA_LIST_CURSOR_V2}"
                        ),
                    ));
                }
            };
            // The nest-held key its own cursors seal under — derived from the
            // durable deployment seed, so a cursor survives a restart but is
            // opaque to every holder (`crate::cursor_seal`).
            let seal_key =
                crate::cursor_seal::derive_cursor_key(&state.nest_identity.signing_key.to_bytes());
            let after = match req.cursor.as_deref() {
                Some(c) if !c.is_empty() => Some(decode_cursor(&seal_key, c, cursor_version)?),
                _ => None,
            };
            let limit = match req.limit {
                0 => MEDIA_LIST_DEFAULT_LIMIT,
                n => n.min(MEDIA_LIST_MAX_LIMIT),
            } as usize;

            // The sets this caller may read (owned + group-member), the same
            // S2-P3 boundary the per-name reads apply.
            let sets = crate::folder_authz::enumerate_readable_folders(&state.db, &actor_id)
                .await
                .map_err(internal)?;

            let mut items: Vec<MediaItem> = Vec::new();
            let mut sort_keys: Vec<SortKey> = Vec::new();
            for (fs, grant) in &sets {
                // Website-published folders APPEAR in Media like any other
                // folder (folders re-model open call #4, ratified 2026-08-13 —
                // the website is a serving toggle over the same substrate, not
                // a separate content plane). The former `mode == "web"`
                // exclusion is retired with the mode itself.
                // The folder's content reachability — the `source_online` dot.
                let source_online = crate::chunk_relay::folder_content_reachable(
                    &state.ws,
                    state.sync.chunk_resolver.foreign_seats(),
                    fs,
                );

                // One membership plane for every folder since the phase 3 head
                // unification (2026-08-17, `file-sync.md` § Membership):
                // the Apple "Photo Library" and website folders record into
                // the same `sync_changes` head feed as any other, so Media
                // reads one projection.
                // (Reserved `__*` sets never reach here.)
                let files = state
                    .db
                    .get_files_for_folder(fs.id)
                    .await
                    .map_err(internal)?;
                // The set-name label pair — the seal and the salt it opens under
                // — projected to this reader. Both halves ride together or not
                // at all (a seal without its salt is unrenderable once the
                // plaintext scrubs), and both are withheld from a reader who is
                // not the label's audience: the salt is an unkeyed digest of a
                // dictionary-shaped name, so handing it to a Q5 admin would give
                // back the very name the seal exists to withhold.
                //
                // The salt is read from the stored `folders.name_hash` column
                // rather than recomputed from the plaintext — the addressing
                // switch S5 owns, and what keeps this surface correct after the
                // scrub, when there is no plaintext left to recompute from.
                // Strictly a pair: `zip` so a row that has one half and not the
                // other ships neither. A seal without its salt is unrenderable
                // post-scrub, and a salt without a seal is a dictionary handle
                // with nothing to open — neither half is useful alone, so
                // neither travels alone.
                let (folder_sealed, folder_hash) = match grant.is_label_audience() {
                    true => fs
                        .name_sealed
                        .clone()
                        .zip(fs.name_hash.clone())
                        .map(|(sealed, hash)| {
                            (
                                Some(fauna_protocol::ByteBuf::from(sealed)),
                                Some(fauna_protocol::ByteBuf::from(hash)),
                            )
                        })
                        .unwrap_or((None, None)),
                    false => (None, None),
                };

                for f in files {
                    // Ordered by the set's row id, not its name digest — see
                    // `CursorKey`. Stable, nest-local, and it hands a
                    // non-audience pager nothing about the name.
                    //
                    // ⚠ Sort/skip correctness needs the *true* `path_hash`
                    // regardless of audience — this is a server-side
                    // position key, not the wire-shipped `MediaItem` field
                    // below, which projects per reader. Disclosing nothing
                    // is the *cursor seal*'s job, not this key's (S5e —
                    // see `CursorKey`); do not audience-gate it here or the
                    // keyset skip lands in the wrong place.
                    sort_keys.push(SortKey::V2(fs.id, f.path_hash.clone()));
                    // The path label pair — same audience gate and strict-pair
                    // shape as `folder_sealed`/`folder_hash` above. A reader
                    // who cannot open the set's labels (Q5 `AdminDiscovery`)
                    // gets neither: `path_sealed` is useless without its salt,
                    // and the salt alone is a dictionary handle onto the
                    // path.
                    let (path_sealed, path_hash) = if grant.is_label_audience() {
                        (
                            f.path_sealed.map(fauna_protocol::ByteBuf::from),
                            Some(fauna_protocol::ByteBuf::from(f.path_hash)),
                        )
                    } else {
                        (None, None)
                    };
                    // The head row's writer-signed statement (writer-signed
                    // change records (2) — web's Media reader has no other row
                    // source). Same audience gate as the label pair above: the
                    // statement is unbuildable without `path_hash`, and its
                    // device + signer key name the writer's device fleet.
                    let bytes = |b: Vec<u8>| fauna_protocol::ByteBuf::from(b);
                    let statement = grant.is_label_audience().then(|| SignedFields {
                        manifest_hash: Some(bytes(f.manifest_hash)),
                        device_id: f.device_id.map(bytes),
                        author_actor_id: Some(bytes(f.actor_id)),
                        change_type: Some(f.change_type),
                        content_key_version: f.content_key_version.map(|v| v as u64),
                        derived_through: f.derived_through,
                        is_resolution: f.is_resolution,
                        signature: f.signature.map(bytes),
                        signer_key: f.signer_key.map(bytes),
                    });
                    let s = statement.unwrap_or_default();
                    items.push(MediaItem {
                        folder: fs.name.clone(),
                        // The empty string is the ratified scrub sentinel on
                        // this required wire field — render seams read it as
                        // "scrubbed" and fall back to the sealed pair below.
                        path: f.path.unwrap_or_default(),
                        size_bytes: f.size_bytes,
                        updated_at: f.updated_at,
                        // The uploader-recorded `?thumb=1` pointer when the
                        // record carried one (`None` until a producer supplies it
                        // — media.md § Implementation status Remaining (a)).
                        thumbnail_hash: f.thumbnail_hash,
                        source_online,
                        path_sealed,
                        path_hash,
                        folder_sealed: folder_sealed.clone(),
                        folder_hash: folder_hash.clone(),
                        manifest_hash: s.manifest_hash,
                        device_id: s.device_id,
                        author_actor_id: s.author_actor_id,
                        change_type: s.change_type,
                        content_key_version: s.content_key_version,
                        derived_through: s.derived_through,
                        is_resolution: s.is_resolution,
                        signature: s.signature,
                        signer_key: s.signer_key,
                        // Reader-stamped, never on the wire: a reader sets it
                        // from its own verdict (ruling (8)(c)).
                        signed_as_current: false,
                        signed_as: None,
                        extra: Default::default(),
                    });
                }
            }

            // Sort by the active order's key. The key travels *with* the item
            // rather than being recomputed inside the comparator, so the sort
            // and the keyset skip below cannot disagree about the order — the
            // failure mode that silently drops or duplicates a page.
            let mut paired: Vec<(SortKey, MediaItem)> = sort_keys.into_iter().zip(items).collect();
            paired.sort_by(|a, b| a.0.cmp(&b.0));

            // Skip everything at-or-before the cursor, then take one page.
            if let Some(after) = &after {
                let start = paired.partition_point(|(key, _)| key <= after);
                paired.drain(..start);
            }
            let next_cursor = if paired.len() > limit {
                Some(encode_cursor(&seal_key, &paired[limit - 1].0))
            } else {
                None
            };
            paired.truncate(limit);

            // WHOSE aggregate this is. The reply is scoped entirely by the
            // connection's authenticated actor (`enumerate_readable_folders`
            // above is a plain `WHERE actor_id = ?`), so an `Ok` with zero
            // items is the nest saying "this caller owns no readable set" —
            // a statement about the CALLER, not about the data. Without the
            // caller on the line, a client that reconnected as the wrong
            // identity is indistinguishable from one whose account is empty,
            // and the empty page reads as a bug in whatever feature saw it.
            // Actor id + counts only: a public identifier and two numbers,
            // no names and no paths (`encryption-at-rest.md` § the sealed
            // label pair above is exactly what must not be logged).
            tracing::debug!(
                caller = %hex::encode(actor_id),
                sets = sets.len(),
                items = paired.len(),
                "fauna.media.list served",
            );

            let items: Vec<MediaItem> = paired.into_iter().map(|(_, it)| it).collect();
            // The page's side table: one cert per distinct delegated signer
            // among the items actually served (after the cut).
            let signers: Vec<([u8; 32], [u8; 32])> = items
                .iter()
                .filter_map(|it| {
                    let actor = <[u8; 32]>::try_from(&it.author_actor_id.as_ref()?[..]).ok()?;
                    let key = <[u8; 32]>::try_from(&it.signer_key.as_ref()?[..]).ok()?;
                    Some((actor, key))
                })
                .collect();
            let signer_certs = crate::sync_handlers::signer_certs_for_signers(&state.db, signers)
                .await
                .map_err(internal)?;
            encode_reply(&MediaListReply {
                items,
                next_cursor,
                cursor_version,
                signer_certs,
                extra: Default::default(),
            })
        })
    })
}

/// Register the `fauna.media.*` handler cluster. Mirrors
/// `sync_handlers::register_sync_handlers`; gate enforced in
/// `bridge_method_allowlist::is_permitted`.
pub fn register_media_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.media.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: media_list_handler(),
        },
    );
}
