//! GET /api/v1/export — download a zip archive of all user data.
//!
//! The zip is streamed via ChannelWriter + spawn_blocking so the entire
//! archive is never buffered in RAM.  All async DB queries and PayloadStore
//! resolution happen before the blocking zip-writer is spawned — with one
//! deliberate exception: segment file pairs are only *enumerated*
//! async; their bytes are streamed from disk by the blocking writer, because
//! a mail corpus is the one export plane with no useful size bound and the
//! files are local (`account-data-plane.md` § Nest-side requirements item 1,
//! the *Payload stores* ruling).

use std::io::Write;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::api_error::ApiError;
use crate::db::actor_tables::TableExport;
use crate::routes::AppState;
use crate::streaming::{ChannelWriter, streaming_body};

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ExportParams {
    #[serde(default)]
    pub include_blobs: bool,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn handle_export(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    // Optional on purpose (the established extractor): both serve paths
    // inject ConnectInfo, but a test server without it must not fail the
    // export — only the audit event's IP line goes "unknown".
    crate::registration::OptionalConnectInfo(peer): crate::registration::OptionalConnectInfo,
    Query(params): Query<ExportParams>,
) -> Response {
    // Try regular bearer auth first, then fallback to eviction export token
    let actor_id = if let Some(auth_header) = headers.get("authorization") {
        let token_str = auth_header
            .to_str()
            .unwrap_or("")
            .strip_prefix("Bearer ")
            .unwrap_or("");
        match crate::auth::check_bearer_session(&state, token_str).await {
            Ok(session) => session.actor_id.0,
            // A live session bearer whose actor has lost standing keeps this
            // door's `403` (the use-time checks below say the same for an
            // eviction token), rather than falling through to the eviction
            // store and reading as an unknown token.
            Err(crate::auth::BearerRefusal::Standing) => {
                return ApiError::forbidden("account is not active").into_response();
            }
            // Fallback: try eviction export token
            Err(crate::auth::BearerRefusal::Invalid) => {
                match state.db.validate_eviction_token(token_str).await {
                    Ok(Some(aid)) => aid,
                    _ => return ApiError::unauthorized("invalid token").into_response(),
                }
            }
        }
    } else {
        return ApiError::unauthorized("authorization required").into_response();
    };

    // Verify user exists
    match state.db.get_user(&actor_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(_) => return ApiError::internal("db error").into_response(),
    }

    // Re-check the account's standing at USE time, not only at token-mint time.
    // This endpoint hands back the whole account as a zip, and it accepts an
    // **eviction** token as well as a session bearer — eviction tokens live in
    // SQLite and are untouched by `revoke_actor`, so neither a lockout nor a
    // suspension reaches them. Without these gates a bearer minted before the
    // account's standing changed (or an eviction token that outlived it) is a
    // full-account exfil the owner has already tried to stop. A session bearer
    // has already been asked its actor's standing by the one bearer validator
    // (`auth::check_bearer_session`, above) — the question every bearer door
    // asks at use, since one mint can be replayed for the token's whole TTL;
    // these re-checks are what cover the eviction token, which that validator
    // never sees.
    //
    // ⚠ **Supersession is deliberately NOT one of them — RULED, not an
    // omission.** The
    // suspend/lockout re-checks above exist because those standings do NOT
    // revoke live credentials; succession DOES — the ceremony revokes the old
    // identity's sessions, tears down its sockets AND burns its eviction
    // token (`identity-succession.md` step 3; `db/actor_tables.rs`
    // `Succession::Burn`), so every path a thief could use to REACH this
    // route is already closed at the credential plane and a use-time refusal
    // here would be redundant for the threat. It would also be destructive
    // for the data plane: the succession audit row **stays** keyed on
    // `old_actor_id` and is exported there (`actor_tables.rs:6084-6126`,
    // `Succession::Stay`: "the row records that THIS identity was
    // succeeded"), `identity-succession.md:108` leaves every attribution row
    // under the old id, and the sanctioned post-succession door — an
    // admin-re-issued eviction token pulling the retired identity's archive
    // (`succession-aftermath.md` § the eviction-token fallback) — comes
    // through exactly this route and is the ONE credential the refusal plane
    // never sees. Gating the old id would leave all of that exportable by
    // nobody. `export_api.rs::the_identity_plane_rides_including_the_published_key_halves`
    // pins the ruled behavior: a superseded actor's export carries its own
    // `Stay` row.
    if let Err(e) = state.db.check_actor_active(&actor_id).await {
        tracing::warn!("export refused: {e}");
        return ApiError::forbidden("account is not active").into_response();
    }
    match state.db.get_locked_until(&actor_id).await {
        Ok(Some(locked_until)) if locked_until > now_epoch_secs() => {
            return ApiError::forbidden("account is locked").into_response();
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("export: lockout consult failed: {e}");
            return ApiError::internal("db error").into_response();
        }
    }

    // Gather all data async, then stream zip via spawn_blocking + ChannelWriter.
    match gather_export_data(&state, &actor_id, params.include_blobs).await {
        Ok(data) => {
            // The archive is about to stream: ring the owner (
            // the whole-account disclosure must never be quieter than a
            // new-IP sign-in). Fired on BOTH credential paths (bearer and
            // eviction token), after every gate passed, spawned off the
            // response path exactly like `NewTokenIssued`.
            {
                let event = crate::security_notify::SecurityEvent::ArchiveExported {
                    ip_address: peer.map(|addr| addr.ip().to_string()),
                    include_blobs: params.include_blobs,
                };
                let notifier = state.security_notifier.clone();
                let scope = state.clone();
                let state = state.clone();
                scope.spawn_scoped(async move {
                    notifier.notify(&state, &actor_id, &event).await;
                });
            }
            let (tx, body) = streaming_body(32);
            tokio::task::spawn_blocking(move || {
                let writer = ChannelWriter::seekable(tx);
                if let Err(e) = write_export_zip(writer, data) {
                    tracing::error!("export zip write error: {e:#}");
                }
            });

            let ts = format_timestamp(now_epoch_secs());
            let filename = format!("fauna-export-{ts}.zip");
            let mut resp_headers = HeaderMap::new();
            resp_headers.insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());
            resp_headers.insert(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\"")
                    .parse()
                    .unwrap(),
            );
            (StatusCode::OK, resp_headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("export failed: {e:#}");
            ApiError::internal("export failed").into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Export structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ManifestExport {
    format: u32,
    actor_id: String,
    exported_at: i64,
    include_blobs: bool,
    blob_refs: Vec<String>,
    /// **Additive** — the format number deliberately stays 1. An older reader
    /// ignores the object; a newer one learns what this archive does not
    /// contain, which is the half of *"the user always controls their data"*
    /// (`principles.md`) that a complete-looking archive cannot express.
    coverage: CoverageExport,
    /// The segment-store disposition — additive on format 1 like
    /// `coverage`, and for the same reason: `coverage.partial == false` says
    /// no open table judgment remains, never that the archive is complete.
    segment_store: SegmentStoreExport,
    /// The conversations disposition — additive on format 1.
    /// The records always ride (`conversations/records.ndjson`,
    /// membership-resolved); `bodies_included` mirrors `include_blobs` for
    /// the live-bodies door under `conversations/bodies/`.
    conversations: ConversationsExport,
}

/// `account-data-plane.md` § Nest-side requirements item 1, the *Universe*
/// paragraph: the conv plane scopes to the CHANNEL, so its records and bodies
/// ride membership-resolved doors of their own rather than the per-actor
/// walks — and the manifest declares that plane's disposition by name.
#[derive(Serialize)]
struct ConversationsExport {
    /// Mirrors `include_blobs` — whether the exporter's channels' live
    /// sealed bodies rode under `export/conversations/bodies/`.
    bodies_included: bool,
}

/// `account-data-plane.md` § Nest-side requirements item 1, the *Payload
/// stores* ruling: whether the actor-scoped segment pairs rode, and what a
/// per-actor walk structurally cannot reach.
#[derive(Serialize)]
struct SegmentStoreExport {
    /// Mirrors `include_blobs` — ONE flag governs payload bytes; which store
    /// serves them is nest-internal mechanics, not a user choice.
    included: bool,
    /// The actor-scoped kinds the walk covers.
    kinds: Vec<&'static str>,
    /// Pairs enumerated but gone by write time (a raced compaction) — named,
    /// so the archive never silently under-delivers.
    skipped: Vec<String>,
    /// The channel-scoped plane the per-actor BYTE walk cannot reach —
    /// declared by name, never silently absent. The conv RECORDS ride through
    /// the membership-resolved `conversations/records.ndjson` domain (row
    /// 157) and the live BODIES through `conversations/bodies/`;
    /// it is the channel-scoped segment file PAIRS that stay out of
    /// `export/segments/`, deliberately — a pair carries records compaction
    /// has not yet dropped, where the bodies door reads live records.
    channel_scoped_not_included: Vec<&'static str>,
    /// Pairs the walk enumerated but the archive withholds WHOLE, by kind,
    /// segment and reason — never silently absent (decision (6) of the
    /// ruling: a `post` pair is plaintext at rest, and a pair cannot be
    /// verbatim and withhold one record).
    withheld: Vec<WithheldSegmentExport>,
}

/// One withheld pair's declaration in `segment_store.withheld`.
#[derive(Serialize)]
struct WithheldSegmentExport {
    kind: &'static str,
    segment_id: u32,
    /// The only reason today: the segment holds a post under a legal
    /// takedown (`moderation.md` § Legal takedown → *Posts*).
    reason: &'static str,
}

/// `segment_store.withheld[].reason` for a `post` pair holding a legally
/// taken-down post.
const WITHHELD_REASON_LEGAL_TAKEDOWN: &str = "legal_takedown";

/// One live conv record's sealed payload queued for the blocking writer.
/// Addressed by channel + per-channel seq — the conv plane's own
/// addressing (`record_id` is derived from them).
struct ConvBodyExport {
    channel_hex: String,
    seq: i64,
    body: Vec<u8>,
}

/// One on-disk segment pair queued for the blocking writer to stream.
struct SegmentFileExport {
    kind: &'static str,
    segment_id: u32,
    dat_path: std::path::PathBuf,
    meta_path: std::path::PathBuf,
}

/// Rule 4's coverage declaration — `account-data-plane.md` § Nest-side
/// requirements item 1: *"while any table is `Unreviewed`, the export manifest
/// says so … and withheld tables are likewise declared by name and reason
/// class"*.
///
/// Both lists are scoped to tables **this nest's schema actually has**: the
/// declaration answers *what does this box hold and not give me*, and a nest
/// built without the bridge features holds none of their tables at all.
#[derive(Serialize, Default)]
struct CoverageExport {
    /// True while any table this nest holds is still awaiting a verdict.
    partial: bool,
    /// Tables whose export disposition has not been ruled yet — the honest
    /// interim (rule 3), not a judgement that they hold nothing.
    unreviewed_tables: Vec<String>,
    /// Tables deliberately withheld, by name and reason class.
    withheld_tables: Vec<WithheldExport>,
    /// How the registry-driven `tables/*.ndjson` entries encode SQLite BLOB
    /// columns, since NDJSON cannot carry the distinction itself.
    blob_encoding: &'static str,
}

#[derive(Serialize)]
struct WithheldExport {
    table: String,
    /// `"secret"` / `"derived"` / `"operational"`.
    reason_class: String,
}

#[derive(Serialize)]
struct ProfileExport {
    actor_id: String,
    handle: Option<String>,
    tier: Option<String>,
    label: Option<String>,
    inbox_mode: String,
    created_at: Option<i64>,
    inbox_bytes_used: Option<i64>,
    storage_bytes_used: Option<i64>,
}

#[derive(Serialize)]
struct ContactExport {
    peer_id: String,
    status: String,
    accepted_at: Option<i64>,
    created_at: i64,
}

#[derive(Serialize)]
struct InboxMessageExport {
    id: i64,
    payload_hex: String,
    /// Epoch milliseconds (converted from the microsecond `content.created_at`).
    created_at: i64,
    delivered: bool,
}

#[derive(Serialize)]
struct PostExport {
    post_id: String,
    /// The post's stored bytes, hex. **Empty** when `legal_takedown_ref` is set:
    /// the body is withheld from the author's own archive exactly as from every
    /// other serve path.
    data_hex: String,
    /// Set iff the post is taken down under a legal obligation — the citation
    /// the tombstone shows (`moderation.md` § Legal takedown → *Posts*). Omitted
    /// otherwise, so every other entry is byte-identical to the pre-field
    /// shape. The entry itself stays, following `fauna.posts.list`: dropping it
    /// would make the archive's post listing silently partial, where this makes
    /// the absent body visible and says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    legal_takedown_ref: Option<String>,
}

#[derive(Serialize)]
struct KnockExport {
    id: i64,
    sender_id: String,
    sender_node: String,
    summary: String,
    created_at: i64,
}

#[derive(Serialize)]
struct DeviceExport {
    device_id: String,
    label: String,
    registered_at: i64,
    last_seen: i64,
}

#[derive(Serialize)]
struct FolderMemberExport {
    device_id: String,
    flags: fauna_protocol::folders::PlaceFlags,
}

#[derive(Serialize)]
struct FolderDestinationExport {
    id: i64,
    kind: String,
    device_id: Option<String>,
    s3_config: Option<String>,
    sync_mode: String,
}

#[derive(Serialize)]
struct FolderExport {
    id: i64,
    /// The plaintext name — the empty sentinel for a sealed set whose plaintext
    /// no longer rests; the export then names the set by the pair below, which
    /// the user's own app opens (`path-sealing.md` § the set-name plane).
    name: String,
    /// Hex `name_hash` — the salt `name_sealed` opens under.
    name_hash: Option<String>,
    /// Hex `name_sealed`, forwarded verbatim (the nest holds no key).
    name_sealed: Option<String>,
    created_at: i64,
    node_cache: bool,
    members: Vec<FolderMemberExport>,
    destinations: Vec<FolderDestinationExport>,
}

#[derive(Serialize)]
struct FeedExport {
    feed_id: String,
    name: String,
    rules_hex: String,
    combination: String,
    created_at: i64,
}

#[derive(Serialize)]
struct KeyPackageExport {
    id: String,
    data_hex: String,
    published_at: i64,
    expires_at: i64,
}

// ---------------------------------------------------------------------------
// Gathered data container (all async results collected before blocking write)
// ---------------------------------------------------------------------------

struct ExportData {
    actor_hex: String,
    exported_at: i64,
    include_blobs: bool,
    profile: ProfileExport,
    contacts: Vec<ContactExport>,
    inbox_messages: Vec<InboxMessageExport>,
    posts: Vec<PostExport>,
    knocks: Vec<KnockExport>,
    devices: Vec<DeviceExport>,
    folders: Vec<FolderExport>,
    feeds: Vec<FeedExport>,
    key_packages: Vec<KeyPackageExport>,
    blob_refs: Vec<String>,
    blobs: Vec<(String, Vec<u8>)>,
    /// Segment pairs to stream from disk in the blocking writer.
    segment_files: Vec<SegmentFileExport>,
    /// Pairs enumerated but withheld whole, for the manifest's declaration.
    segment_withheld: Vec<WithheldSegmentExport>,
    /// The kinds `actor_scoped_segment_planes` walks — threaded through so
    /// the manifest declaration cannot drift from the walk.
    segment_kinds: Vec<&'static str>,
    /// The registry-driven half — one entry per emitting table with rows.
    tables: Vec<TableExport>,
    /// The **conversations** shaped domain: the exporter's conv records,
    /// reached by resolving their channel membership. `segment_records`' other
    /// four kinds ride the registry walk above on the actor key; `conv` scopes
    /// to the channel, so it needs the membership door
    /// (`actor_tables::gather_conv_records` carries the ruling).
    conv_records: Option<TableExport>,
    /// The conv BODIES door: the exporter's channels' live records'
    /// sealed payloads, read through `segments::conv::read_after_seq` — the
    /// serving door's own primitive — behind `include_blobs`.
    conv_bodies: Vec<ConvBodyExport>,
    /// Rule 4's declaration of what this export left out.
    coverage: CoverageExport,
}

/// The actor-scoped segment planes an export walks — `conv` is deliberately
/// absent: it scopes to the CHANNEL, so a per-actor walk cannot reach it
/// (declared in the manifest instead).
fn actor_scoped_segment_planes(
    state: &AppState,
) -> [(&'static str, &Arc<fauna_segment_store::SegmentManager>); 4] {
    [
        ("mail", &state.mail_segments),
        ("post", &state.post_segments),
        ("calendar", &state.cal_segments),
        ("card", &state.card_segments),
    ]
}

// ---------------------------------------------------------------------------
// Async data gathering (all DB + PayloadStore calls happen here)
// ---------------------------------------------------------------------------

async fn gather_export_data(
    state: &AppState,
    actor_id: &[u8; 32],
    include_blobs: bool,
) -> anyhow::Result<ExportData> {
    let actor_hex = hex::encode(actor_id);
    let exported_at = now_epoch_secs();
    let mut blob_refs: Vec<String> = Vec::new();

    // -- profile --
    let user = state.db.get_user(actor_id).await?;
    let handle = state.db.get_handle(actor_id).await?;
    let inbox_mode = state.db.get_inbox_mode(actor_id).await?;
    let profile = ProfileExport {
        actor_id: actor_hex.clone(),
        handle,
        tier: user.as_ref().map(|u| u.tier.clone()),
        label: user.as_ref().map(|u| u.label.clone()),
        inbox_mode,
        created_at: user.as_ref().map(|u| u.created_at),
        inbox_bytes_used: user.as_ref().map(|u| u.inbox_bytes_used),
        storage_bytes_used: user.as_ref().map(|u| u.storage_bytes_used),
    };

    // -- contacts --
    let contacts_raw = state.db.list_contacts_full(actor_id).await?;
    let contacts: Vec<ContactExport> = contacts_raw
        .into_iter()
        .map(|c| ContactExport {
            peer_id: hex::encode(&c.peer_id),
            status: c.status,
            accepted_at: c.accepted_at,
            created_at: c.created_at,
        })
        .collect();

    // -- inbox messages (with PayloadStore resolution) --
    let inbox_raw = state.db.list_inbox_all(actor_id).await?;
    let mut inbox_messages: Vec<InboxMessageExport> = Vec::with_capacity(inbox_raw.len());
    for (id, payload, blob_hash, created_at, delivered) in &inbox_raw {
        let resolved = if let Some(ps) = &state.payload_store {
            ps.resolve_payload(payload, blob_hash.as_deref())
                .await
                .unwrap_or_else(|_| payload.clone())
        } else {
            payload.clone()
        };
        inbox_messages.push(InboxMessageExport {
            id: *id,
            payload_hex: hex::encode(&resolved),
            // micros at rest → millis in the export, so the emitted document is
            // byte-identical to what it was before `content.created_at` was
            // unified on microseconds (2026-08-03).
            created_at: *created_at / 1_000,
            delivered: *delivered,
        });
    }

    // -- posts (with PayloadStore resolution) --
    //
    // The export serves post bodies to their author, so it is bound by the
    // legal-takedown rule that withholds a body from EVERY viewer, author
    // included (`moderation.md` § Legal takedown → *Posts*) — and it reads
    // through `load_post_body`, which is deliberately flag-blind, so the gate
    // is here, checked before the body is ever read. Only the takedown arm
    // binds: quarantine is author-visible, and this archive is the author's.
    // The same split `fauna.posts.list` makes, and the same one the conv
    // bodies door below inherits from its read primitive. A flag read that
    // fails fails the export: an archive that cannot tell whether a body is
    // withheld must not guess.
    let post_ids = state.db.list_posts_by_author(actor_id).await?;
    let mut posts: Vec<PostExport> = Vec::new();
    // The taken-down posts among the author's own — the segment-pair leg
    // below resolves them to the `post` pairs it must withhold.
    //
    // Seeded with the posts this author DELETED while taken down, because the
    // scan below cannot see them: it walks `content`, and the delete removed
    // that row along with the flag, while leaving the compelled bytes in the
    // segment file for compaction to reclaim later (`moderation.md` § Legal
    // takedown → *Posts*). Their `posts`-domain entry is likewise already
    // absent — the delete destroyed the post — so only the verbatim pair leg
    // needs them, which is exactly the door they escaped through.
    let mut taken_down_posts: Vec<[u8; 32]> = state
        .db
        .taken_down_deleted_post_ids_for_author(actor_id)
        .await?;
    for post_id_bytes in &post_ids {
        let post_id_hex = hex::encode(post_id_bytes);
        if post_id_bytes.len() == 32 {
            let post_id: [u8; 32] = post_id_bytes.as_slice().try_into().unwrap();
            if let Some(reference) = state.db.get_post_legal_takedown(&post_id).await? {
                taken_down_posts.push(post_id);
                posts.push(PostExport {
                    post_id: post_id_hex,
                    data_hex: String::new(),
                    legal_takedown_ref: Some(reference),
                });
                continue;
            }
            if let Ok(Some(data)) =
                crate::segments::post::load_post_body(&state.post_segments, &state.db, &post_id)
                    .await
            {
                scan_blob_refs(&data, &mut blob_refs);
                posts.push(PostExport {
                    post_id: post_id_hex,
                    data_hex: hex::encode(&data),
                    legal_takedown_ref: None,
                });
            }
        }
    }

    // -- knocks --
    let knocks_raw = state.db.poll_knocks(actor_id).await?;
    let knocks: Vec<KnockExport> = knocks_raw
        .iter()
        .map(|knock| KnockExport {
            id: knock.id,
            sender_id: hex::encode(knock.sender_id),
            sender_node: String::from_utf8_lossy(&knock.sender_node).to_string(),
            summary: knock.summary.clone(),
            created_at: knock.created_at,
        })
        .collect();

    // -- sync devices --
    let devices_raw = state.db.list_sync_devices(actor_id).await?;
    let devices: Vec<DeviceExport> = devices_raw
        .into_iter()
        .map(|d| DeviceExport {
            device_id: hex::encode(&d.device_id),
            label: d.label,
            registered_at: d.registered_at,
            last_seen: d.last_seen,
        })
        .collect();

    // -- sync folders --
    let folders_raw = state.db.get_folders_for_actor_full(actor_id).await?;
    let mut folders: Vec<FolderExport> = Vec::new();
    for fs in &folders_raw {
        let members_raw = state.db.get_folder_members(fs.id).await?;
        let members: Vec<FolderMemberExport> = members_raw
            .into_iter()
            .map(|m| FolderMemberExport {
                device_id: hex::encode(&m.device_id),
                flags: m.flags,
            })
            .collect();

        // The phantom `folder_destinations` rail is deleted (folders re-model
        // row 7, 2026-08-18); the export keeps the key (always `[]`, as every
        // production export already was) so the artifact's shape is stable.
        let destinations: Vec<FolderDestinationExport> = Vec::new();

        folders.push(FolderExport {
            id: fs.id,
            name: fs.name.clone(),
            name_hash: fs.name_hash.as_deref().map(hex::encode),
            name_sealed: fs.name_sealed.as_deref().map(hex::encode),
            created_at: fs.created_at,
            node_cache: fs.node_cache,
            members,
            destinations,
        });
    }

    // -- feeds --
    let feeds_raw = state.db.list_feeds_by_owner(actor_id).await?;
    let feeds: Vec<FeedExport> = feeds_raw
        .iter()
        .map(|feed| FeedExport {
            feed_id: feed.feed_id.clone(),
            name: feed.name.clone(),
            rules_hex: hex::encode(&feed.rules),
            combination: feed.combination.clone(),
            created_at: feed.created_at,
        })
        .collect();

    // -- key packages --
    let kps_raw = state.db.list_key_packages_for_actor(actor_id).await?;
    let key_packages: Vec<KeyPackageExport> = kps_raw
        .iter()
        .map(|(id, data, published_at, expires_at)| KeyPackageExport {
            id: id.clone(),
            data_hex: hex::encode(data),
            published_at: *published_at,
            expires_at: *expires_at,
        })
        .collect();

    // -- blobs (optional, fetched async before handing to blocking writer) --
    //
    // ⚠ DORMANT: `scan_blob_refs` is a no-op, so `blob_refs` is always empty
    // and this loop ships no store blob today (the archive's real blob
    // carriage is the `post` segment-pair leg below, withheld whole and
    // declared). The day an extractor lands, this leg inherits the ruling
    // already taken for the author's archive (`moderation.md` § Legal takedown
    // → *The blob-serve door* → *What the withhold binds on owner- and
    // admin-scoped routes*, path 1): each gathered digest passes
    // `blob_routes::is_legally_withheld` before the store read, and a withheld
    // one is DECLARED in the manifest — the `posts` domain's shape — never
    // silently dropped. Do not land the extractor without that gate and its
    // witness.
    blob_refs.sort();
    blob_refs.dedup();
    let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
    if include_blobs {
        let local_blob_store = state
            .backup_service
            .as_ref()
            .map(|svc| svc.local_blob_store());
        if let Some(blob_store) = local_blob_store.as_ref() {
            for hash_hex in &blob_refs {
                let Ok(hash_bytes) = hex::decode(hash_hex) else {
                    continue;
                };
                let Ok(hash_arr) = <[u8; 32]>::try_from(hash_bytes.as_slice()) else {
                    continue;
                };
                let content_hash = fauna_core::data::ContentHash::from_digest_raw(hash_arr);
                if let Ok(Some(blob_data)) = blob_store.get(&content_hash).await {
                    blobs.push((hash_hex.clone(), blob_data));
                }
            }
        }
    }

    // -- conversations (the membership-resolved door onto `segment_records`) --
    //
    // Not a twelfth `Export::Shaped` verdict: this domain row-filters
    // `segment_records` to `kind = 'conv'`, and a partial `Shaped` is
    // deliberately not expressible, so the table stays `Verbatim` and the two
    // doors are keyed differently rather than overlapping — the walk below reads
    // `scope_id = ?actor` and can never return a channel-scoped row.
    let conv_records = state.db.gather_actor_conv_records(actor_id).await?;

    // -- conv bodies (the records domain one level down) --
    //
    // Behind the SAME flag as the segment pairs — one flag governs payload
    // bytes — but through the serving door's own read primitive rather than a
    // whole-pair copy: a pair carries records compaction has not yet dropped,
    // where `segments::conv::read_after_seq` reads live records and enforces
    // the legal-obligation relay-withhold at the single gate every conv serve
    // surface shares, so the export structurally cannot drift from what a
    // member is served. What rides is the at-rest sealed payload; the RAM
    // bound is the corpus — the blob arm's ruled class
    // (`account-data-plane.md` § Nest-side requirements item 1).
    let mut conv_bodies: Vec<ConvBodyExport> = Vec::new();
    if include_blobs {
        const PAGE: i64 = 1024;
        for channel in state.db.list_actor_channels(actor_id).await? {
            let mut cursor = 0i64;
            loop {
                let page = crate::segments::conv::read_after_seq(
                    &state.conv_segments,
                    &state.db,
                    &channel,
                    cursor,
                    PAGE,
                )
                .await?;
                // Loop until an EMPTY page, not a short one: a mirror row
                // whose record diverged from the segment file is dropped from
                // the page (warned inside the primitive, same as serving), so
                // a short page does not mean the channel is drained.
                let Some((last_seq, _, _)) = page.last() else {
                    break;
                };
                cursor = *last_seq;
                for (seq, body, legal_ref) in page {
                    // Withheld: the records row carries the takedown
                    // reference, so the absence is declared; the sealed bytes
                    // were never read (the gate returns an empty body).
                    if legal_ref.is_some() {
                        continue;
                    }
                    conv_bodies.push(ConvBodyExport {
                        channel_hex: hex::encode(channel),
                        seq,
                        body,
                    });
                }
            }
        }
    }

    // -- segment pairs (enumerate async; the blocking writer streams) --
    //
    // Behind the SAME flag as blobs — one flag governs payload bytes — and
    // under EITHER credential: the eviction-export-token holder gets the same
    // archive, by ruling (`account-data-plane.md` § Nest-side requirements
    // item 1, *Payload stores*). What rides is the at-rest form: sealed
    // record payloads whose keys never rest here, so the seal — not
    // credential tiering — is what makes the archive safe under the weakest
    // credential the endpoint accepts.
    //
    // The `post` plane is the ruled exception (decision (6)): its public
    // bodies rest in plaintext (`segments::post`'s module doc), so a verbatim
    // `post` pair would carry a taken-down post's body — the bytes the `posts`
    // leg above withholds and `moderation.md` § Legal takedown → *Posts*
    // withholds from every viewer. A pair cannot be verbatim AND withhold one
    // record, so a `post` pair holding a currently taken-down record is
    // withheld WHOLE (both halves — a `.meta` alone re-admits nothing) and
    // declared in the manifest by kind, segment and reason. Every other pair
    // rides as before; the withhold lasts exactly as long as the flag, since
    // a takedown never segment-tombstones the record and compaction keeps it.
    let mut segment_files: Vec<SegmentFileExport> = Vec::new();
    let mut segment_withheld: Vec<WithheldSegmentExport> = Vec::new();
    if include_blobs {
        for (kind, mgr) in actor_scoped_segment_planes(state) {
            // Pure read first — a segment-less actor must not grow scope
            // state or directories just by exporting. The manifest gains a
            // segment id at APPEND time, so an open-but-never-finalized
            // segment is already listed.
            let manifest = mgr.load_manifest(actor_id).await?;
            if manifest.kind_manifest.live_segments.is_empty() {
                continue;
            }
            // Finalize-on-read, like the list/byte routes: the pair on disk
            // becomes a self-consistent framed prefix, and finalize is what
            // writes the `.meta` sidecar.
            mgr.finalize_open(actor_id).await?;
            // The withhold set, resolved AFTER the manifest read: a compaction
            // racing the gather then moves a taken-down record into a segment
            // this walk never enumerated (its old file turns up `skipped`),
            // never out of the withhold. A lookup that fails fails the export:
            // an archive that cannot tell which pair to withhold must not
            // guess. Scoped to this author — a post's segment scope IS its
            // author, so a foreign scope names nothing this walk can reach.
            let mut withheld_segments = std::collections::BTreeSet::new();
            if kind == crate::segments::post::KIND {
                for post_id in &taken_down_posts {
                    // Tombstone-INCLUSIVE: the set below carries posts whose
                    // author deleted them while taken down, and a deleted
                    // record's mirror row is tombstoned while its bytes wait
                    // for compaction — the live-only lookup answers `None`
                    // for precisely the pair this withhold exists for.
                    if let Some((scope, seg_id)) =
                        crate::segments::post::lookup_scope_by_post_id_including_tombstoned(
                            &state.db, post_id,
                        )
                        .await?
                        && &scope == actor_id
                    {
                        withheld_segments.insert(seg_id);
                    }
                }
            }
            for seg_id in &manifest.kind_manifest.live_segments {
                if withheld_segments.contains(seg_id) {
                    segment_withheld.push(WithheldSegmentExport {
                        kind,
                        segment_id: *seg_id,
                        reason: WITHHELD_REASON_LEGAL_TAKEDOWN,
                    });
                    continue;
                }
                segment_files.push(SegmentFileExport {
                    kind,
                    segment_id: *seg_id,
                    dat_path: mgr.segment_file_path(actor_id, *seg_id),
                    meta_path: mgr.segment_meta_path(actor_id, *seg_id),
                });
            }
        }
    }

    // -- the registry-driven half (the fourth `ACTOR_TABLES` axis) --
    //
    // Every `Export::Verbatim`/`Redacted` table's rows, plus the declaration of
    // what was left out. The 11 shaped domains above stay hand-written on
    // purpose: their verdicts are `Export::Shaped`, so the walk skips them
    // rather than exporting the same rows twice.
    let export_set = state.db.gather_actor_export(actor_id).await?;
    let coverage = CoverageExport {
        partial: export_set.partial(),
        unreviewed_tables: export_set
            .unreviewed
            .iter()
            .map(|t| (*t).to_string())
            .collect(),
        withheld_tables: export_set
            .withheld
            .iter()
            .map(|w| WithheldExport {
                table: w.table.to_string(),
                reason_class: w.reason_class.to_string(),
            })
            .collect(),
        blob_encoding: "hex",
    };

    Ok(ExportData {
        actor_hex,
        exported_at,
        include_blobs,
        segment_files,
        segment_withheld,
        segment_kinds: actor_scoped_segment_planes(state).map(|(k, _)| k).to_vec(),
        tables: export_set.tables,
        conv_records,
        conv_bodies,
        coverage,
        profile,
        contacts,
        inbox_messages,
        posts,
        knocks,
        devices,
        folders,
        feeds,
        key_packages,
        blob_refs,
        blobs,
    })
}

// ---------------------------------------------------------------------------
// Synchronous zip writer (runs inside spawn_blocking)
// ---------------------------------------------------------------------------

fn write_export_zip(writer: ChannelWriter, data: ExportData) -> anyhow::Result<()> {
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    // For ciphertext entries (conv bodies, segment pairs): deflate over sealed
    // payloads buys nothing and a mail corpus makes its CPU cost real.
    let stored_opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut zip = ZipWriter::new(writer);
    // Per-file draining over the seek-back ChannelWriter (the writer must be
    // built with ChannelWriter::seekable): flush each file after its header is
    // patched, so a JSON entry larger than 64 KB does not corrupt the archive
    // ("seek into already-drained region"). See crate::streaming.
    zip.set_flush_on_finish_file(true);

    // -- profile.json --
    zip_json(&mut zip, "export/profile.json", opts, &data.profile)?;

    // -- contacts.json --
    zip_json(&mut zip, "export/contacts.json", opts, &data.contacts)?;

    // -- inbox/{id}.json --
    for msg in &data.inbox_messages {
        let path = format!("export/inbox/{}.json", msg.id);
        zip_json(&mut zip, &path, opts, msg)?;
    }

    // -- posts/{post_id_hex}.json --
    for post in &data.posts {
        let path = format!("export/posts/{}.json", post.post_id);
        zip_json(&mut zip, &path, opts, post)?;
    }

    // -- knocks/{id}.json --
    for knock in &data.knocks {
        let path = format!("export/knocks/{}.json", knock.id);
        zip_json(&mut zip, &path, opts, knock)?;
    }

    // -- sync/devices.json --
    zip_json(&mut zip, "export/sync/devices.json", opts, &data.devices)?;

    // -- sync/folders.json --
    zip_json(&mut zip, "export/sync/folders.json", opts, &data.folders)?;

    // -- feeds/{feed_id}.json --
    for feed in &data.feeds {
        let path = format!("export/feeds/{}.json", feed.feed_id);
        zip_json(&mut zip, &path, opts, feed)?;
    }

    // -- key_packages/{id}.json --
    for kp in &data.key_packages {
        let path = format!("export/key_packages/{}.json", kp.id);
        zip_json(&mut zip, &path, opts, kp)?;
    }

    // -- tables/{name}.ndjson (the registry-driven half) --
    //
    // One compact JSON object per line, so a large table streams into the zip
    // without the whole document having to be well-formed first. Written
    // through the same seek-back writer as every other entry, which
    // `set_flush_on_finish_file` above already accounts for.
    for table in &data.tables {
        let path = format!("export/tables/{}.ndjson", table.table);
        zip.start_file(&path, opts)?;
        zip.write_all(&table.ndjson)?;
    }

    // -- conversations/records.ndjson (the membership-resolved domain) --
    //
    // Its own path rather than a second writer into
    // `tables/segment_records.ndjson`: the two doors read one table on two
    // different keys, and which key produced a row is exactly what a reader
    // needs to know. Each row still names its own `scope_id` (the channel), so
    // the file is self-describing.
    if let Some(conv) = &data.conv_records {
        let path = format!("export/{}.ndjson", conv.table);
        zip.start_file(&path, opts)?;
        zip.write_all(&conv.ndjson)?;
    }

    // -- conversations/bodies/{channel_hex}/rec-{seq} --
    //
    // The live records' sealed payloads, addressed the way conv records are
    // addressed — channel + per-channel seq — so each body joins its
    // records.ndjson row without a second index. At-rest sealed form: this
    // side cannot unseal and does not re-frame.
    for cb in &data.conv_bodies {
        let name = format!(
            "export/conversations/bodies/{}/rec-{:012}",
            cb.channel_hex, cb.seq
        );
        zip.start_file(&name, stored_opts)?;
        zip.write_all(&cb.body)?;
    }

    // -- blobs/{blake3_hex} (optional, pre-fetched async) --
    for (hash_hex, blob_data) in &data.blobs {
        let path = format!("export/blobs/{}", hash_hex);
        zip.start_file(&path, opts)?;
        zip.write_all(blob_data)?;
    }

    // -- segments/{kind}/seg-NNNNNNNN.{dat,meta} (streamed from disk; row 156) --
    //
    // Verbatim at-rest form: CARv2 framing plaintext, record payloads sealed
    // by the kind's inner seal — this side cannot unseal and does not
    // re-frame.
    let mut segment_skipped: Vec<String> = Vec::new();
    for sf in &data.segment_files {
        for (path, ext) in [(&sf.dat_path, "dat"), (&sf.meta_path, "meta")] {
            let name = format!(
                "export/segments/{}/seg-{:08}.{}",
                sf.kind, sf.segment_id, ext
            );
            match std::fs::File::open(path) {
                Ok(mut f) => {
                    zip.start_file(&name, stored_opts)?;
                    std::io::copy(&mut f, &mut zip)?;
                }
                // A segment can be compacted away between gather and write;
                // the manifest names the loss rather than silently
                // under-delivering (it is written last, below).
                Err(_) => segment_skipped.push(name),
            }
        }
    }

    // -- manifest.json (written last so blob_refs + skipped are complete) --
    let manifest = ManifestExport {
        format: 1,
        actor_id: data.actor_hex,
        exported_at: data.exported_at,
        include_blobs: data.include_blobs,
        blob_refs: data.blob_refs,
        coverage: data.coverage,
        segment_store: SegmentStoreExport {
            included: data.include_blobs,
            kinds: data.segment_kinds,
            skipped: segment_skipped,
            channel_scoped_not_included: vec!["conv"],
            withheld: data.segment_withheld,
        },
        conversations: ConversationsExport {
            bodies_included: data.include_blobs,
        },
    };
    zip_json(&mut zip, "export/manifest.json", opts, &manifest)?;

    zip.finish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn zip_json<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &str,
    opts: SimpleFileOptions,
    value: &impl Serialize,
) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    zip.start_file(path, opts)?;
    zip.write_all(&json)?;
    Ok(())
}

/// Best-effort scan of raw data for potential BLAKE3 blob references.
/// We check the blob_metadata table via hex encoding — actual verification
/// would be too expensive, so we just collect all 32-byte-aligned candidates
/// that are valid hex when present in post data. For simplicity, we don't
/// attempt heuristic scanning of binary data; blob refs collected elsewhere
/// are sufficient.
fn scan_blob_refs(_data: &[u8], _blob_refs: &mut Vec<String>) {
    // Blob references inside dag-cbor-encoded posts are opaque; we would need
    // schema-aware decoding to extract ContentHash fields. For now this is a
    // placeholder — the manifest's blob_refs array will be populated by any
    // future schema-aware extractor.
}

fn now_epoch_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

// ---------------------------------------------------------------------------
// GET /api/v1/export/{session_id} — one mailbox-export blob
// ---------------------------------------------------------------------------

/// Stream one completed mailbox export's sealed blob
/// (`mail-export.md` § Download flow; inventoried in `api-layers.md`
/// § HTTP residue as a byte-bulk surface).
///
/// **What the nest hands back is ciphertext it cannot read**: the framed,
/// per-chunk-AEAD artifact the user's own client assembled and sealed under a
/// key only that user's actor key unwraps (§ Blob shape on disk, § Key
/// material). So this route is a byte pump with one authorization question —
/// *is the requester the session's owner?* — and no content question at all.
///
/// Three refusals, each deliberate:
///
/// - **Not the owner → 404, not 403.** `get_export_session` filters by actor,
///   so a foreign session is indistinguishable here from a missing one. That
///   is the answer § Cross-actor isolation wants: a wrong-actor request must
///   not confirm that some other user has an export with that id.
/// - **Not `completed` → 404.** § Architectural rules forbids a partial-blob
///   download, and this is where that is enforced rather than merely implied
///   by `download_url` being absent from the wire reply: a client that
///   constructed the URL itself gets the same answer as one that did not.
/// - **Row present, file gone → 404** (§ Expiry: "a blob that's been GC'd but
///   the user references via a stale URL gets a `404 Not Found` — there's no
///   zombie-blob recovery shape").
///
/// Sealed is not the same as harmless, so the door carries the account
/// export's other two use-time duties as well (`mail-export.md` § Download
/// flow → *Standing and the owner's notice*): the
/// bearer's actor must still have standing, and every download rings the
/// owner.
pub async fn handle_export_session_blob(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    // Optional on purpose, as on the account route: a test server without
    // ConnectInfo must not fail the download — only the notice's IP goes
    // "unknown".
    crate::registration::OptionalConnectInfo(peer): crate::registration::OptionalConnectInfo,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return ApiError::unauthorized("authorization required").into_response();
    };
    // Session bearer only. The whole-account route above also accepts an
    // eviction token; this one does not, because an export blob is not part of
    // the eviction archive and a token minted for that purpose has no business
    // reaching a per-session mail snapshot.
    //
    // The one bearer validator also asks the actor's STANDING, so a suspended
    // or locked-out actor's surviving bearer is refused here — and refused
    // `401` like every other session-only byte door (chunks, snapshots, the
    // WS upgrade), not with the account route's `403`. That route's `403`
    // exists because it has a second credential to fall through to: a
    // standing refusal there must not be retried against the eviction store.
    // This door has no fall-through, so it keeps the byte plane's one shape.
    //
    // Supersession needs no gate of its own, and unlike the account route
    // (which rules it out deliberately, above) there is nothing here to
    // strand: succession BURNS every export session of the retired identity
    // (`db/actor_tables.rs`, the `export_sessions` entry's `Succession::Burn`)
    // along with its bearers, so the old id has neither a credential to reach
    // this door nor a row for it to serve.
    let Some(actor_id) = crate::auth::validate_bearer(&state, token).await else {
        return ApiError::unauthorized("invalid token").into_response();
    };
    let actor_id = actor_id.0;

    let row = match state.db.get_export_session(&actor_id, &session_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::not_found("export not found").into_response(),
        Err(_) => return ApiError::internal("db error").into_response(),
    };
    if row.state != crate::db::mail_export::EXPORT_STATE_COMPLETED {
        return ApiError::not_found("export not found").into_response();
    }
    let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
    else {
        return ApiError::internal("data dir unavailable").into_response();
    };
    let Some(file) = crate::mail_export_blobs::resolve_export_blob(&data_dir, &row.blob_path)
    else {
        return ApiError::internal("export blob path is not the minted shape").into_response();
    };
    let mut handle = match tokio::fs::File::open(&file).await {
        Ok(f) => f,
        Err(_) => return ApiError::not_found("export not found").into_response(),
    };
    // Content-Length comes from the FILE, never from `blob_bytes`. The counter
    // is a reservation taken before the write (`append_export_blob_bytes`), so
    // a crash between the two leaves it one frame ahead — and a Content-Length
    // longer than the body is a hung transfer the client cannot diagnose.
    let Ok(meta) = handle.metadata().await else {
        return ApiError::internal("export blob unreadable").into_response();
    };
    let content_length = meta.len();

    // Every gate passed and the blob is about to stream: ring the owner. The
    // blob is sealed, but a download is still a whole mailbox leaving the
    // nest, so it is never quieter than the account archive's
    // `ArchiveExported` — spawned off the response path exactly like it.
    {
        let event = crate::security_notify::SecurityEvent::MailboxExportDownloaded {
            ip_address: peer.map(|addr| addr.ip().to_string()),
            format: row.format.clone(),
        };
        let notifier = state.security_notifier.clone();
        let scope = state.clone();
        let state = state.clone();
        scope.spawn_scoped(async move {
            notifier.notify(&state, &actor_id, &event).await;
        });
    }

    // Streamed, never buffered: § Quota composition's ceiling is 10 GiB, so
    // reading the blob into memory to answer one GET would be the shape that
    // makes a legitimate export an availability incident.
    let (tx, body) = streaming_body(8);
    // spawn-ok(request-scoped): feeds this one download and ends at the blob's
    // EOF or when the HTTP client stops reading (the send fails). Holds only
    // the open file and the body channel — never `AppState` or key material —
    // so a rotation teardown cannot leave it acting on superseded identity.
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        // The client hung up mid-transfer. Nothing to clean up:
                        // the blob is the session's and rests until its own
                        // expiry or an explicit discard.
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    // The filename the user ends up holding is the *opened* archive's
    // (§ Compression wrapper), not the sealed file's — the client decrypts
    // before it saves, so offering `.sealed` here would name a file the user
    // never sees. The session id stays out of it deliberately (§ Architectural
    // rules: "the session_id is sensitive ... the UI never exposes it").
    let disposition = format!(
        "attachment; filename=\"fauna-export-{}.zip.zst\"",
        row.format
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, content_length.to_string()),
        ],
        body,
    )
        .into_response()
}

/// Format epoch seconds as `YYYYMMDD-HHMMSS` using the Howard Hinnant
/// civil_from_days algorithm (no chrono dependency).
fn format_timestamp(epoch_secs: i64) -> String {
    let (total_days, hh, mm, ss) = fauna_core::caltime::epoch_secs_to_days_and_time(epoch_secs);
    let (y, m, d) = fauna_core::caltime::civil_from_days(total_days);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_known_anchors() {
        assert_eq!(format_timestamp(0), "19700101-000000");
        // Pins the composition itself (epoch_secs_to_days_and_time's total_days
        // feeding civil_from_days), not just its two already-tested pieces —
        // a pre-epoch case is the one that would catch a wrong argument order
        // or a reintroduced truncating division at this call site.
        assert_eq!(format_timestamp(-1), "19691231-235959");
    }
}
