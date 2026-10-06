// Shared snapshot shapes for the `DevicesMachine` (`libs/fauna-devices-machine`,
// WASM twin `libs/fauna-wasm-folders`), consumed by the two Settings sub-pages
// the 2026-06-28 sync/folder UI unification splits the former combined page into:
// Settings → Devices (the roster — `DevicesSection.svelte`) and Settings → File
// sets (the control plane — `FoldersSection.svelte`). Both render slices of the
// one `DevicesSnapshot` the machine owns. See docs/goal/ui/{devices,folders}.md.
//
// Mirrors `fauna_devices_machine::DevicesSnapshot` / `fauna_folders_machine`
// serde JSON (snake_case across the serde_json boundary). The enum fields ride as
// their Rust serde variant names — PascalCase (e.g. the wizard step `"Name"` /
// `"Devices"` / `"Review"`).
import type { LocalizedText } from '$lib/i18n/localized';

// One folder this device holds a place in, with that place's three flags
// (`fauna_protocol::folders::PlaceFlags` owns what each means). The Devices page
// `device-folder-role-badge` renders it via `devicePlaceLabel`.
export interface DeviceFolderRole {
  name: string;
  originates: boolean;
  accepts: boolean;
  applies_deletes: boolean;
}
export interface DeviceSummary {
  device_id: string;
  label: string;
  capabilities: string;
  registered_at: number;
  last_seen_at: number;
  online: boolean;
  // Slice F guardian-enrolled-device marker (family-safety.md § Full visibility):
  // the ward's own list renders it (transparency by construction); always false
  // on an unsupervised account. Rides `fauna.sync.devices.list` additively.
  guardian_marked: boolean;
  folders: DeviceFolderRole[];
  // `device-p2p-participation-toggle`'s paint, drawn by the machine at every
  // snapshot (`p2p.md` § Per-device participation). Mirrors Rust
  // `DeviceSummary::p2p_participation_paint`; null only on a row not painted
  // yet — never re-derived here. Web wires no participation door, so every
  // row paints `own: false` (the request-off arm).
  p2p_participation_paint?: P2pParticipationPaint | null;
}
/** Mirrors Rust `P2pParticipationPaint`: the app draws a checkbox that is
 *  `checked`, labelled `label`, enabled exactly when `actionable`, whose click
 *  sends `setP2pParticipation(index, !checked)`. */
export interface P2pParticipationPaint {
  own: boolean;
  checked: boolean;
  label: LocalizedText;
  actionable: boolean;
}
export interface FolderSummary {
  id: number;
  name: string;
  retention_policy: string | null;
  cached_snapshot_count: number;
  cached_total_bytes: number;
  cached_last_snapshot_at: number | null;
  include_paths: string[] | null;
  exclude_paths: string[] | null;
  // Hex raw MLS group id when the set is shared cross-user (`Some` ⇒ shared, drives
  // the owner-side `folder-shared-badge`); null/absent ⇒ owner-only. Mirrors the
  // Rust `FolderSummary::mls_group_id` (docs/goal/ui/folders.md § Sharing).
  mls_group_id?: string | null;
  // Per-set conflict policy ("auto" | "latest_wins_always"); null/absent on a
  // member row (render as "auto", the column default).
  conflict_policy?: string | null;
  // Whether this folder is currently served over WebDAV — the per-set
  // exposure gate the `folder-webdav-toggle` reflects/flips. Mirrors Rust
  // `FolderSummary::webdav_enabled` (docs/goal/behavior/webdav-server.md §
  // Independent enablement point 2). Optional/absent (member rows carry false)
  // ⇒ treat as `false` (not served).
  webdav_enabled?: boolean;
  // The subscription tier this website-enabled folder is paywalled to, or null/absent when
  // it serves publicly — the per-set gate the `folder-paywall-tier-select`
  // reflects/sets. Mirrors Rust `FolderSummary::web_paywall_tier`
  // (docs/goal/ui/folders.md § Web paywall; monetization.md § Pillar 2). Only
  // website-enabled folders can be paywalled; null on every sync/backup set. (Consumed once
  // the web paywall-select fan-out lands; carried now so the shared shape stays
  // in lockstep with the Rust snapshot.)
  web_paywall_tier?: string | null;
  // Who may read this folder: "private" | "shared" | "public" — the tri-state the
  // `folder-audience-select` reflects and sets (docs/goal/ui/folders.md § Audience
  // and website serving). Mirrors Rust `FolderSummary::audience`.
  // ⚠ **Never paint this raw.** An absent audience sends nothing
  // and the field defaults to "", which is outside the select's own option set —
  // run it through `normalizeAudience(value, bound)`, which is fail-closed: nothing
  // unparseable ever resolves to "public".
  audience?: string | null;
  // Whether this folder's head is published as the actor's website — the flag the
  // `folder-website-toggle` reflects and flips. Mirrors Rust
  // `FolderSummary::website_enabled`. Orthogonal to `audience`, which decides who
  // may READ what is published: enabled on a folder that is neither public nor
  // paywalled is allowed and inert, and the hint says so (`websiteServeHint`)
  // rather than the control being disabled. Absent ⇒ false.
  website_enabled?: boolean;
  // The folder's content residency (folders re-model phase 5 — file-sync.md §
  // Content residency): "metadata_only" ⇒ chunk bytes never rest on the nest
  // (the owner's consent-gated choice); empty/"full" ⇒ today's behavior, the
  // nest keeps content. Rides both projection arms — a member seat uploads
  // bytes too, so it must see it. Mirrors Rust `FolderSummary::residency`.
  // ⚠ **Never paint this raw** — run it through `normalizeResidency(value)`,
  // fail-closed to "full" (the nest sends an empty value for full).
  residency?: string | null;
  // "owner" (your own set) | "member" (a set shared WITH you — B3). A member row
  // renders READ-ONLY: the "Shared by ‹…›" badge + the leave button, no config
  // controls, no local paths (the nest withholds the owner's). Absent ⇒ owner
  // (the nest always stamps it). Mirrors Rust `FolderSummary::role`
  // (docs/goal/ui/folders.md § Sharing — *Member list-visibility*).
  role?: string | null;
  // The recipient badge's precomputed "Shared by ‹…›" label (handle else the
  // owner's canonical short id — the account-display-label rule owned by
  // docs/goal/behavior/value-formatting.md). RENDER THIS: never truncate
  // `owner_handle` or the actor id locally. Empty on your OWN rows (which show
  // "Shared · N" instead), so empty ⇒ not a recipient badge.
  owner_display?: string;
  // This caller's access on a member row: "reader" (default) | "writer". Advisory
  // for UI only — never an authz input (the nest gates every write itself).
  access?: string | null;
  // The nest place's snapshot policy — whether the nest keeps snapshots of this
  // folder, and how long it waits for quiet before cutting one. Mirrors Rust
  // `FolderSummary::{nest_snapshots, nest_snapshot_quiet_secs}`
  // (docs/goal/behavior/backup-restore.md § 8b).
  //
  // ⚠ Each is THREE-state, and `null`/absent is the third one: *unset*, meaning
  // "nothing authoritative said" — the nest-wide behavior, the resting value of
  // every folder, and where the knob returns when its owner picks the default.
  // Never collapse it with `false`, which is an owner's explicit "don't". The
  // editor never derives from these directly: `nestPlaceEditFromRow` turns them
  // into the four control values.
  nest_snapshots?: boolean | null;
  nest_snapshot_quiet_secs?: number | null;
  // The version-retention SIBLING pair's bounds (file-versions.md § Retention
  // ruling 1, apps row 323) — its own `folders.version_retention` wire field,
  // never folded into `retention_policy`. Always present as a number (0 = that
  // bound unset — the nest's own spelling), mirroring Rust
  // `FolderSummary::{version_retention_max_versions, version_retention_max_age_days}`.
  // The editor never derives from these directly: `versionRetentionEditFromBounds`
  // turns them into the two control values.
  version_retention_max_versions: number;
  version_retention_max_age_days: number;
}
// One entry of a shared folder's cross-user actor roster, read on demand via
// `fauna.folders.members.list_actors` (`WsRpcClient.foldersActorMembers`) — the
// OTHER-USERS the owner shared the set with, distinct from the device roster
// (`DeviceSummary.folders`). Mirrors Rust `FolderActorMember`
// (`libs/fauna-protocol/src/folders.rs`; docs/goal/ui/folders.md § Sharing).
// The nest cannot observe an MLS join, so it returns every actor the share
// reached: "Pending"/"Active" is a purely CLIENT-side distinction (web, like
// linux, renders "Active" for each returned member). No `display` field — the
// row label is `accountDisplayLabel(handle, actor_id)` (handle-else-shortid).
export interface FolderActorMember {
  actor_id: string;
  // Nest-resolved handle; empty ("") when unknown or a cross-nest member.
  handle: string;
  // "owner" | "member" — the owner-side "Shared with" list renders "member" rows
  // only (the count feeds "Shared · N").
  role: string;
  // "reader" (default) | "writer" — the member's access (multi-writer Phase 1,
  // folders.md § Sharing). Absent/None ⇒ reader.
  access?: string | null;
  // Per-member byte cap on a writer; null/absent ⇒ uncapped (blank input).
  byte_cap?: number | null;
  // Bytes this member has recorded against the owner's quota (abuse counter).
  bytes_used?: number | null;
  // Some(true) ⇒ a cross-nest member (handle is empty).
  remote?: boolean | null;
}
// One entry of a folder's enrolled DEVICE roster, read on demand via
// `fauna.folders.members.list` (`WsRpcClient.foldersMembers`) — the seats the
// device-place editor edits, distinct from the cross-user actor roster above.
// Mirrors Rust `FolderMember` (`libs/fauna-protocol/src/folders.rs`).
// ⚠ Never read `flags` here to decide what a seat does: put the reply through
// `placeRows`, the one shared rule (see `PlaceRow`).
export interface FolderMember {
  // Hex-encoded 32-byte device id.
  device_id: string;
  label: string;
  // The seat's place — always present.
  flags: { originates: boolean; accepts: boolean; applies_deletes: boolean };
}
// One roster seat, projected for the device-place editor by the shared
// `placeRows` (`fauna_folders_machine::place_editor`) — what a
// `folder-place-row` paints; every seat paints its three `folder-place-*`
// checkboxes. Rows keep roster order, because `folder-place-row[j]` is what the
// cross-app e2e contract addresses.
export interface PlaceRow {
  device_id: string;
  label: string;
  originates: boolean;
  accepts: boolean;
  applies_deletes: boolean;
}
// One device's recorded sync activity for a folder, read on demand via
// `fauna.folders.devices` (`WsRpcClient.foldersDevices`) — the ordinary
// sync-type activity signal (`folder-device-activity-item`/-label/-count),
// distinct from `FolderSummary.cached_snapshot_count`/`cached_total_bytes`
// (snapshot-only). Mirrors Rust `FolderDevice`
// (`libs/fauna-protocol/src/folders.rs`; docs/goal/behavior/file-sync.md
// § Implementation status today).
export interface FolderDevice {
  // Hex-encoded 32-byte device id.
  device_id: string;
  label: string;
  // Unix seconds of that device's most recent recorded change on this set.
  last_change_at: number;
  // Count of changes that device has recorded on this set.
  change_count: number;
}
// One enrolled backup destination, marked attached-or-not for ONE ordinary
// folder — the folders page's *Destination places* section
// (docs/goal/behavior/backup-destinations.md § Ordinary-folder coverage).
// Mirrors the Rust `fauna_client_config::FolderDestinationPlace`.
export interface FolderDestinationPlace {
  destination_id: string;
  // The config row's display_name, falling back to the id.
  label: string;
  attached: boolean;
  // The destination-side `__folder/<hex>/<id>` set name — present on attached
  // rows only, and the detach sequence's config-row key.
  folder_set?: string | null;
}
// One staged ("knocked") cross-user folder share awaiting accept/decline — a
// `folder-pending-share` row in the recipient-side "Shared with you" area
// (docs/goal/ui/folders.md § Sharing — *Recipient side*). Mirrors the wasm
// `PendingShareJs` (`libs/fauna-wasm/src/conversations.rs`), itself the web twin
// of native `FfiPendingShare`. NOT a folder: it becomes one only on accept.
// The Welcome bytes deliberately stay in Rust — `foldersAcceptShare` re-resolves
// them by `inboxId`, so the blob never crosses into JS.
export interface PendingShare {
  // The durable-inbox row id — the accept/decline target.
  inboxId: number;
  // The nest-stamped sharer (hex ActorId); null for an unstamped / cross-nest
  // relayed share. Prefer `sharedByDisplay` for display.
  sharedBy?: string | null;
  // The sharer's handle — bare for a local sharer, paired with
  // `sharedByDomain` for a verified cross-nest one; null when handle-less.
  sharedByHandle?: string | null;
  // The cross-nest sharer's handle domain; null same-nest.
  sharedByDomain?: string | null;
  // The pre-computed "Shared by ‹…›" label — RENDER THIS, never re-derive a
  // truncation (handle else the sharer's canonical short id; the account-display
  // -label rule owned by docs/goal/behavior/value-formatting.md). Empty string
  // only when the share is fully unstamped ⇒ render the unknown-sharer label.
  sharedByDisplay: string;
  groupId?: string | null;
  channelId?: string | null;
  // The shared set's name, resolved by its HOME nest. Null when the seal cannot be opened
  // (a pending share) ⇒ render the unknown-set fallback label.
  setName?: string | null;
}
export interface ConflictCandidateSummary {
  manifest_hash: string;
  device_id: string;
  size_bytes: number;
  created_at: number;
  // M2 content-key generation the candidate's manifest was sealed under —
  // echoed verbatim into the review re-point. Null/absent for unsealed sets.
  content_key_version?: number | null;
}
// A conflict row on the Folders review list (auto-resolve — file-sync.md §
// Conflicts, ratified 2026-07-10): resolved rows carry the resolution + winner
// (the losing candidate stays retained in version history); a row with
// resolved_at == null is an unresolved report from a degraded engine path
// (rendered informationally — no blocking chooser).
export interface ConflictSummary {
  id: number;
  folder: string;
  device_id: string;
  path: string;
  conflict_type: string;
  details: string | null;
  created_at: number;
  candidates: ConflictCandidateSummary[];
  resolved_at?: number | null;
  resolution?: string | null; // "merged" | "latest_wins"
  winning_manifest_hash?: string | null;
  // Precomputed at transcribe (`fauna-devices-machine`) — the single source
  // replacing a client-local winner/candidate comparison + `.slice(0, 8)`.
  has_other_version: boolean;
  file_info: string;
}
export interface WizardDevice {
  device_id: string;
  label: string;
  selected: boolean;
  // The seat's three place flags — what this device DOES in the folder
  // (`fauna_protocol::folders::PlaceFlags` is the single authority for what each
  // means).
  originates: boolean;
  accepts: boolean;
  applies_deletes: boolean;
}
export interface RetentionPolicy {
  max_snapshots: number;
  max_age_days: number;
}
/** Step 1 — the name. A folder has no type, so there is no mode picker. */
export interface NameSnapshot {
  name: string;
  continue_enabled: boolean;
}
/** Step 2 (`Devices`) as the three place-flag checkboxes render it. Every flag
 *  point is a valid place, so `continue_enabled` carries no refusal for the SPA
 *  to derive. */
export interface DevicePlacesSnapshot {
  devices: WizardDevice[];
  continue_enabled: boolean;
}
/** The four `folder-nest-*` control values, as
 *  `nestPlaceEditFromRow` produces them and `setFolderNestPlace` takes them
 *  back. Strings because that is what a text box is — every parse and format
 *  rule lives in shared Rust (`fauna_folders_machine::nest_place`), including
 *  the two the SPA must never re-derive: blank is a real value, and a zero
 *  retention bound renders blank because zero is how the nest spells unset. */
export interface NestPlaceEdit {
  snapshots: string;
  quiet_secs: string;
  retention_snapshots: string;
  retention_days: string;
}
/** A `folder-nest-snapshots-select` option (wire value + i18n label), from the
 *  shared catalog — the SPA hand-rolls neither the value list nor the labels. */
export interface NestSnapshotsOption {
  value: string;
  label: LocalizedText;
}
export interface EnrolledDeviceSummary {
  device_id: string;
  label: string;
}
export interface ReviewSnapshot {
  name: string;
  retention: RetentionPolicy | null;
  enrolled: EnrolledDeviceSummary[];
  create_enabled: boolean;
  phase: string;
  created: boolean;
  failed_members: string[];
  error: LocalizedText | null;
}
export interface WizardSnapshot {
  step: string;
  name: NameSnapshot;
  device_places: DevicePlacesSnapshot;
  review: ReviewSnapshot;
}
/**
 * A followed **public** folder — a read-only row, deliberately a SEPARATE list
 * from `folders` rather than a `FolderSummary` variant
 * (`docs/goal/behavior/folders.md` § Publicly-synced follow): a followed folder
 * has no group, no roster, no location binding and no toggles, plus a status
 * those rows have nowhere to put.
 *
 * `available: false` is the **revoke** — the owner flipped the audience back or
 * deleted the folder. Keep the row visible and loud (a re-flip resumes it under
 * the same `folder_id`); never drop it silently.
 *
 * Empty unless the page wires `setFollowedFoldersSource` — which is the correct
 * render for an app that has not built the follow surface, not an error.
 */
export interface FollowedFolderSummary {
  folder_id: number;
  /** Empty ⇒ homed on the user's own nest (the same-nest follow). */
  home_nest_url: string;
  owner_actor_id: string;
  /** The owner's handle as last verified; `null` for a follow made by actor id. */
  owner_handle: string | null;
  /**
   * The ONE owner string the row paints ("By …" — `devices.followed_owner`):
   * the handle while it still names the owner, else the id's short form.
   * Precomputed in shared Rust; never re-derive it here.
   */
  owner_display: string;
  display_name: string;
  available: boolean;
}
export interface DevicesSnapshot {
  devices: DeviceSummary[];
  folders: FolderSummary[];
  followed: FollowedFolderSummary[];
  conflicts: ConflictSummary[];
  /**
   * The actor's web-address opt-in, for the website toggle's tri-state hint
   * (`ui/folders.md` § Audience and website serving). `null` = unknown (an unwired adapter, or
   * a failed best-effort read): the hint hedges rather than
   * claiming the site is live.
   */
  website_address_enabled?: boolean | null;
  wizard: WizardSnapshot | null;
  error: LocalizedText | null;
  /**
   * Signed-in devices without a matching entry (`device-member-card`) — every
   * verified fleet member no roster row accounts for, in fleet-id order
   * (`docs/goal/ui/devices.md` § Members without a matching entry). Empty while
   * the account runtime is not up, and on a settled honest fleet.
   */
  members?: FleetMemberSummary[];
  /** This browser's own fleet id (hex), once the account runtime answered. */
  own_fleet_id?: string | null;
  /**
   * `device-own-fingerprint`'s text — the SAME `fleet_fingerprint` every member
   * card uses; `null` exactly when `own_fleet_id` is.
   */
  own_fingerprint?: string | null;
}

/**
 * One signed-in device without a matching entry. Mirrors
 * `fauna_devices_machine::FleetMemberSummary`: no name (the name lives on the
 * nest row, which is exactly what cannot be trusted for it), only the key's
 * fingerprint and the enrollment instant the member itself claims.
 */
export interface FleetMemberSummary {
  /** Hex-encoded 32-byte fleet id — the key the remove gesture carries. */
  device_id: string;
  /** Rendered in shared Rust (`fleet_fingerprint`); never re-derive it here. */
  fingerprint: string;
  /** Self-asserted enrollment instant, unix **milliseconds** — a hint, never proof. */
  enrolled_at_ms: number;
}

// ── T16 custody facet, owner side (docs/goal/ui/devices.md § Custody facet) ──
//
// NOT part of `DevicesSnapshot`: the facet folds the `fauna.state.custody-ceremony` entries, which the
// deliberately-keyless machine cannot read, so it arrives from its own
// `DevicesMachine.custodyFacetLoad` call. Mirrors
// `fauna_client_capabilities::custody_view` — the SAME rows the four native apps
// get as `uniffi::Record`s, reaching us through serde. One projection, two
// faces: web and native cannot drift on which receipt state reads which way.
//
// ⚠ Byte fields cross serde as **arrays of numbers**, not `Uint8Array` — the
// same shape `task-delegation.ts` already handles with `Uint8Array.from(...)`.
// A gesture that passes one straight back to wasm must convert first.

/** The granted coverage. `whole_account` = the owner's whole scope set (what the
 *  v1 mint always produces); otherwise `scopes` names the subset. */
export interface CustodyScopesView {
  whole_account: boolean;
  scopes: string[];
}

/** The three receipt-freshness states, as their Rust serde variant names. Three
 *  different states with three different words — never collapsed, never empty
 *  (the A7 honesty rule). */
export type CustodyReceiptStateView = 'Fresh' | 'Stale' | 'NoReceiptYet';

/** A grant's liveness relative to now. */
export type CustodyLivenessView = 'Active' | 'ExpiringSoon' | 'Expired' | 'AutoRenewing';

/**
 * The receipt facts a custody row renders, with both shared label decisions
 * already folded in — which state maps to which key, and that `degraded` is
 * **orthogonal to freshness** (a fresh receipt can honestly report dropped
 * payload, so its marker rides *alongside* the status line, never instead).
 */
export interface CustodyReceiptRowView {
  /** Carries a `{when}` placeholder for the two timestamped states. */
  status_label: LocalizedText;
  /** Epoch SECONDS to format and substitute into `{when}`; null with no receipt.
   *  The shared side hands over seconds rather than a rendered string because
   *  `format_unix_local` needs the OS tz database, which wasm lacks. */
  attested_at_secs: number | null;
  /** Carries `{held}` and `{cap}`, filled from the two resolved fields below. */
  held_bytes_label: LocalizedText;
  /** Resolve BEFORE substituting — a `LocalizedText` argument is a flat string.
   *  Reads as the shared em-dash placeholder with no receipt yet: "nothing
   *  confirmed" and "confirmed zero bytes" are different facts. */
  held: LocalizedText;
  cap: LocalizedText;
  degraded: boolean;
  held_bytes: number | null;
  attested_cap: number | null;
}

/** One owner-side custody row — `custody-holder-card`. */
export interface CustodyHolderRowView {
  /** The handle every gesture names. Acts carry the grant id, **never the row
   *  index** — a refold re-orders rows. */
  grant_id: number[];
  /** The counterpart account holding custody (32 bytes). */
  host: number[];
  /** The serving principal a revoke names; null while the ceremony is pending. */
  custodian_key: number[] | null;
  /** Non-null = the bound custodian is the host's NEST, so this row belongs to
   *  the Nests page's `nest-trust-custody-*` family and a Devices surface must
   *  skip it. One custody never renders in both places. */
  custodian_nest_url: string | null;
  scopes: CustodyScopesView | null;
  lasts_until: number | null;
  liveness: CustodyLivenessView | null;
  receipt_state: CustodyReceiptStateView;
  receipt: CustodyReceiptRowView;
  /** The ceremony has not completed — nothing is minted to revoke yet. */
  pending: boolean;
}

/**
 * The whole folded facet. `held` and `offers` cross so the boundary reports what
 * the fold found rather than lying by omission, but they are deliberately left
 * **untyped here: web must not render them.** Their controls (budget, stop,
 * accept) write the R14 (account-data-plane.md § The ratified decisions) registry row through the W3 (account-data-plane.md § Workstreams) account store, which has no
 * wasm twin, and `devices.md` defines the host-side card *as* those controls —
 * so a card without them contradicts the ratified text. Typing them would be an
 * invitation to paint them.
 */
export interface CustodyFacetView {
  rows: CustodyHolderRowView[];
  held: unknown[];
  offers: unknown[];
}

// The canonical short id (12-char prefix + `…`) is the shared
// `fauna_core::format::short_id` — import `shortId` from `$lib/wasm`. This module
// stays types-only (value-formatting.md § Short id).
