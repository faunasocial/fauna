package com.fauna.app.data.api

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

// (NodeInfo / RegistrationRequest / RegistrationResponse JSON DTOs removed:
//  node-info + handle-available discovery and POST /api/v1/register were deleted
//  from the nest router; onboarding now runs over pre-identity WS-RPC inside the
//  FFI onboarding machine. See ApiClient § Node discovery.)

// (AuthRequest / TokenResponse removed: bearer minting moved off the HTTP
//  `POST /api/v1/auth/token` twin onto the shared FFI `mintBearer` over the
//  WS-RPC silent challenge. See ApiClient.authenticate.)

// (DeviceCodeResponse / DeviceTokenResponse / DeviceTokenResult removed: the
//  RFC-8628 device-code/-token routes were deleted from the nest router and the
//  flow replaced by Ed25519 challenge-auth. See ApiClient § Node discovery.)

// -- Blobs --

@Serializable
data class BlobResponse(val hash: String)

// -- Sync: the fauna.sync.* control-plane DTOs (SyncChangeResponse, SyncStatus,
//    DestinationStatus, FolderInfo, …) were removed when android migrated off
//    the deleted /api/v1/sync/* HTTP twins onto FfiSyncClient — the apps
//    now ride the FfiSyncStatus / FfiBackupStatusEntry records
//    directly (no per-app sync model). The snapshot DTOs were dropped too
//    (see "Snapshots" below).

// -- Snapshots --
// Snapshot control-plane reads/actions ride the typed UniFFI WS-RPC seam
// (fauna.filesync.snapshot.*) — com.fauna.ffi.{FfiSnapshotSummary,
// FfiSnapshotGetReply, FfiSnapshotCreateFolderReply, FfiSnapshotPruneReply,
// FfiSnapshotCheckReply} via FfiSnapshotsClient (FfiNestClient.snapshots()).
// The old JSON DTOs (SnapshotResponse/SnapshotDetail/SnapshotFile/
// SnapshotsWrapper/PruneResponse/CheckResponse) were dropped in the rip-out
// migration; the Backups page consumes the FFI replies directly.

// Quota now rides the typed UniFFI WS-RPC seam (fauna.quota.get) —
// com.fauna.ffi.FfiQuotaGetReply (tier + per-resource inbox/storage/devices/
// features usage). The old flat {used,limit} JSON DTO was retired.

// -- Chunks --
// (CheckChunksResponse removed: the bespoke `/api/v1/chunks/*` +
//  `/api/v1/manifests/*` HTTP routes were retired in the `FfiSyncEngineHost`
//  cutover — the shared engine's own chunk pipeline replaces the hand-rolled
//  Kotlin one. See ChunkedSyncEngine's removal.)

// Knocks + contacts now ride the typed UniFFI WS-RPC seam — com.fauna.ffi
// .FfiKnockItem / FfiContactItem (mapped to the Room entities in
// data/db/SocialInboxMapping.kt). The old JSON DTOs were retired with the
// fauna.{knocks,contacts}.* migration.

// -- Resolution --
// (Recipient resolution migrated off these JSON DTOs onto the shared Rust seam
//  over UniFFI — com.fauna.ffi.resolveNest / resolveHandle return the values
//  directly; see ResolveService. The resolve-node / actor-by-handle HTTP twins
//  are deleted, Track A.)

// (Search migrated off the JSON DTOs onto the shared, stateful SearchManager
//  (fauna_client_search, over the FfiSearchManager UniFFI façade) — see
//  ApiClient.searchManager / SearchManagerHost. The page renders the manager's
//  merged SearchResultRow snapshot directly, no per-app model.)

// -- Feed --
// The feed/posts surface migrated to the fauna.feed.* / fauna.posts.* WS-RPC
// kinds; the feed list/post/query/create
// reply types now ride the shared FfiFeedSummary / FfiFeedPostItem /
// FfiFeedPostsReply / FfiFeedLocalPostsReply records directly (no per-app
// model — priority #4), so the old JSON DTOs were deleted.

// -- Calendars & Events --
//
// These are the per-app UI types the FfiCaldavClient seam's FFI records
// (FfiCalendarRow / FfiCalEvent / FfiCalAttendee) map into in ApiClient — the
// Android analogue of Linux's `db::{CalendarRow,EventRow}` (events.md Decision B,
// the encrypted `bridge_caldav_*` store). They are no longer JSON wire types (the
// legacy plaintext REST path is retired), so the response-wrapper DTOs and the
// `attendance_mode` / `capacity` fields (no canonical VEVENT home) were dropped.

@Serializable
data class FaunaCalendar(
    val id: String,            // hex calendar_id
    val name: String,
    val color: String? = null, // #RRGGBB from sealed metadata; null when none
)

@Serializable
data class EventSummary(
    val id: String,            // hex uid_hash (the encrypted-store write key)
    val uid: String,           // plaintext iCalendar UID (inside the sealed body)
    val summary: String,
    val dtstart: String,
    val dtend: String? = null,
    @SerialName("calendar_id") val calendarId: String? = null
)

@Serializable
data class EventDetail(
    val id: String,            // hex uid_hash
    val uid: String,
    val summary: String,
    val dtstart: String,
    val dtend: String? = null,
    val description: String? = null,
    val location: String? = null,
    /** VEVENT ORGANIZER CAL-ADDRESS (email); empty when the event carries none. */
    val organizer: String = "",
    /** True iff the calling actor organizes this event — gates the author-only
     *  affordances (delete / invite). The RSVP buttons show when this is false
     *  (an event the actor was invited to). Organizer-based, per events.md. */
    val organizedByMe: Boolean = false,
    @SerialName("calendar_id") val calendarId: String? = null
)

@Serializable
data class Attendee(
    val email: String,         // CAL-ADDRESS (bare email; mailto: stripped)
    val name: String,          // CN, empty when the VEVENT carried none
    val rsvp: String,          // going | interested | tentative | declined | invited
)

@Serializable
data class CreateEventRequest(
    @SerialName("calendar_id") val calendarId: String,
    val summary: String,
    val dtstart: String,
    val dtend: String,
    val description: String? = null,
    val location: String? = null,
)

// -- Account --

// Handle change, account delete, and am-i-admin now ride the typed UniFFI
// WS-RPC seam (fauna.profile.handle.change / fauna.account.{delete,am_i_admin})
// via accountRpc(). The old HandleChangeResponse JSON DTO was retired (the
// reply is now a delayed pending-action shape, FfiChangeHandleReply, not
// surfaced to callers that only key off success).

// Inbox mode now rides the typed UniFFI WS-RPC seam (fauna.inbox.mode.*) —
// contactsRpc().inboxModeGet()/inboxModeSet(). The old JSON DTO was retired.

// -- Privacy: Email Filters --
// (Email-filter rows + create/delete now ride the typed UniFFI WS-RPC seam —
//  com.fauna.ffi.FfiEmailFilter / FfiEmailFilterRule / FfiEmailFilterAction.
//  The old JSON DTOs were retired with the fauna.email.* migration.)

// (Privacy spam preferences migrated off the JSON DTOs onto the
//  fauna.spam.{get,set}_preferences WS-RPC kinds via FfiSpamClient /
//  FfiSpamPreferences — see ApiClient.getSpamPreferences / updateSpamPreferences.)

// (MLS key-package pool publish/count migrated off the JSON DTOs onto the
//  fauna.conversations.keypackage.{upload,count} WS-RPC kinds via
//  FfiConversationsClient — see ApiClient.conversationsRpc / MlsManager.)

// -- Post Detail --

@Serializable
data class PostInteraction(
    val action: String,
    val body: String? = null
)

@Serializable
data class PostDetail(
    @SerialName("post_id") val postId: String,
    val author: String,
    @SerialName("created_at") val createdAt: Long,
    val body: String = "",
    val tags: List<String> = emptyList(),
    @SerialName("has_media") val hasMedia: Boolean = false,
    @SerialName("is_reply") val isReply: Boolean = false,
    val source: String = "",
    @SerialName("like_count") val likeCount: Int = 0,
    @SerialName("repost_count") val repostCount: Int = 0,
    @SerialName("reply_count") val replyCount: Int = 0,
    @SerialName("is_liked") val isLiked: Boolean = false,
    @SerialName("is_reposted") val isReposted: Boolean = false,
    @SerialName("reply_to_id") val replyToId: String? = null,
    // Bluesky-specific
    val uri: String? = null,
    val cid: String? = null,
    @SerialName("root_uri") val rootUri: String? = null,
    @SerialName("root_cid") val rootCid: String? = null
)

@Serializable
data class BlueskyThreadResponse(
    val post: PostDetail,
    val parents: List<PostDetail> = emptyList(),
    val replies: List<PostDetail> = emptyList()
)

// -- Calendar View --

// The calendar view vocabulary is `com.fauna.ffi.FfiCalendarViewMode`
// (`fauna_core::caltime::CalendarViewMode`), not a local enum. The retired
// android copy spelled the date-unfiltered list `LIST` — the same wrong word
// web carried until 2026-08-01 — which is exactly what the shared
// `from_wire` refuses, and it ordered its toggle LIST/DAY/WEEK/MONTH against
// the other six apps' agenda/month/week/day.

// Unified notifications now ride the typed UniFFI WS-RPC seam — com.fauna.ffi
// .FfiNotifItem / FfiNotifListReply (notificationsRpc().list()/markRead()/
// count()). The old JSON DTOs were retired with the fauna.notifications.*
// migration.
