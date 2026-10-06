// Shared snapshot shapes for the page-level `MediaMachine` (`libs/fauna-media-machine`,
// WASM twin `libs/fauna-wasm-media`), consumed by the Media route
// (`routes/media/+page.svelte`) — the cross-set, Windows-Explorer-style all-media
// browser (2026-06-28 sync/folder UI unification). The page renders the whole
// Media view off one `MediaPageSnapshot` the machine owns, exactly as the Settings
// sub-pages render off `DevicesSnapshot`. See docs/goal/ui/media.md.
//
// Mirrors `fauna_media_machine::snapshots::{MediaItemSummary, MediaPageSnapshot}`
// serde JSON (snake_case across the serde_json boundary). The sort/filter values
// ride as their stable wire form ("name"/"size"/"date"; a set name or null for the
// all-media default), not the localized labels.
import type { LocalizedText } from '$lib/i18n/localized';

/** One item in the cross-set all-media browse — the `media-item` component. */
export interface MediaItemSummary {
  /** Name of the readable folder this item belongs to (the filter key). */
  folder: string;
  /** Folder-relative path, forward-slash normalized. */
  path: string;
  /** Displayed file name (`media-item-name`) — the path's basename, derived in
   *  shared Rust so every app shows the same name. */
  name: string;
  /** Stored ciphertext size in bytes (`media-item-size`). */
  size_bytes: number;
  /** Last-change timestamp, unix seconds (`media-item-date`). */
  updated_at: number;
  /** Hex thumbnail-blob hash (`media-thumbnail`) when the uploader recorded one;
   *  null until the thumbnail association lands (media.md § Impl status). */
  thumbnail_hash: string | null;
  /** Whether the backing set's source device is reachable (`media-source-status`
   *  online/offline dot). Distinct from a file's sync-state badge. */
  source_online: boolean;
  /** Whether "Share a link" (`share-link-button`) is offered on this item's
   *  detail surface — decided in shared Rust (`share_link_eligible` over the
   *  item's folder; `share-links.md` § Which files can be linked). Paint the
   *  control only when true: absent otherwise, never inert. */
  share_link_eligible: boolean;
}

/** The open share-link create surface (`share-link-create-modal`).
 *  Mirrors `fauna_media_machine::snapshots::ShareCreateSnapshot`. */
export interface ShareCreateSnapshot {
  /** The file's displayed name. */
  name: string;
  /** The chosen `share-link-expiry-select` value (one of
   *  `MediaPageSnapshot.share_expiry_options`). */
  expiry: string;
  /** A create is in flight — the create control is inert until it returns. */
  busy: boolean;
  /** The link's URL (`share-link-url` / `share-link-copy-button`) — non-null
   *  ONLY after the registration succeeded (the reveal-after-registration
   *  rule); once set, the surface shows the URL and no create control. */
  url: string | null;
  /** The file rests sealed, so the link carries its key after `#`; paints
   *  `share-link-key-notice` (share-links.md § The private-file extension). */
  key_in_fragment: boolean;
}

/** One row of the share-link list (`share-link-item`).
 *  Mirrors `fauna_media_machine::snapshots::ShareLinkSummary`. */
export interface ShareLinkSummary {
  /** The registry id (hex) — the revoke key. */
  token_id: string;
  /** The file's name, opened seal-first (`share-link-item-name`). */
  name: string;
  /** Expiry, unix seconds (`share-link-item-expires`). */
  expires_at: number;
  /** `"active"` / `"expired"` / `"revoked"` — the `share-link-item-state`
   *  element's `state` attribute; its text is the shared label. */
  state: string;
  /** The verified re-derived URL — `share-link-item-copy-button`'s presence
   *  and payload; null hides the control. */
  url: string | null;
}

/** The share-link list surface (`share-link-list`).
 *  Mirrors `fauna_media_machine::snapshots::ShareLinksSnapshot`. */
export interface ShareLinksSnapshot {
  /** The list surface is open. */
  open: boolean;
  /** Loaded since it opened — the three-state rule: `share-link-empty-state`
   *  paints only when loaded and `rows` is empty; neither = loading. */
  loaded: boolean;
  /** Newest first. */
  rows: ShareLinkSummary[];
  /** The token id whose `share-link-revoke-confirm-modal` is armed. */
  revoke_confirm: string | null;
}

/** One version of a synced file — a `file-version-item` row in the
 *  `file-version-history` component (`media-item-detail` surface, media.md
 *  § Element IDs; semantics `file-sync.md` § File Versions). Mirrors
 *  `fauna_media_machine::snapshots::FileVersionSummary` serde JSON, as
 *  resolved from the wasm `fileVersions()` JSON string and round-tripped
 *  verbatim (including `content_key_version`) into `restoreVersion()`. */
export interface FileVersionSummary {
  /** The version's stable identity — the recording `sync_changes` row's `seq`
   *  (never renumbered). Display ordinals come from list position. */
  version_num: number;
  /** Hex manifest hash; a restore re-points the file at this. */
  manifest_hash: string;
  /** Logical size in bytes (`file-version-size`). */
  size_bytes: number;
  /** Recorded-at, epoch millis (`file-version-timestamp`). */
  created_at: number;
  /** The M2 content-key generation the version's chunks were sealed under —
   *  carried verbatim on restore so readers select `key_for(version)`.
   *  `null` for owner-only sets. */
  content_key_version: number | null;
  /** Pre-computed "who wrote this" label (`file-version-author`) — nest-stamped
   *  recorder's handle, else their actor id's `short_id`
   *  (`fauna_core::format::account_display_label`). */
  author_display: string;
  /** Whether this row is soft-pruned — only present (and only ever `true`) on
   *  an `include_pruned` listing (`file-versions.md` § Retention (3), apps row
   *  323). Drives `file-version-pruned-badge` + `file-version-undelete-button`,
   *  present ONLY on pruned rows. */
  pruned: boolean;
}

/** One followed public folder, as `media-folder-filter` offers it — a browse
 *  **scope**, not a set in the aggregate (`docs/goal/ui/media.md`
 *  § Followed public folders).
 *
 *  Mirrors `fauna_media_machine::snapshots::FollowedScopeOption`. Append these
 *  after the own-set options and hand the chosen `value` back to
 *  `selectFollowedScope` verbatim: the value↔identity mapping lives in shared
 *  Rust, so the SPA never composes or parses an address. */
export interface FollowedScopeOption {
  /** The machine-minted opaque select value. Stable per follow, disjoint from
   *  every set name by construction — hand it back, never parse it. */
  value: string;
  /** Display label: the follow's name plus a short owner disambiguator, so a
   *  follow named like one of the user's own sets stays tellable apart at the
   *  label level. The routing itself never keys on names. */
  label: string;
  /** Whether the home nest still served this folder at the last verdict.
   *  `false` is the loud *no longer available* rendering — the option stays
   *  offered, because that is the revoke, and a re-flip resumes it. */
  available: boolean;
}

/** The whole renderable Media page in one record. A single observer tick fully
 *  describes the page: the filtered + sorted item list plus the view state. */
export interface MediaPageSnapshot {
  /** The `media-item` rows/tiles to render — already filtered + sorted in shared
   *  Rust for the active filter/sort. */
  items: MediaItemSummary[];
  /** Distinct readable-set names that have media — the `media-folder-filter`
   *  options (alongside the all-media default). */
  folders: string[];
  /** Followed public folders offered as browse **scopes**, after the own-set
   *  options. Empty until the platform wires the followed source, which is the
   *  correct render for a page that has not built the surface. */
  followed: FollowedScopeOption[];
  /** Non-null while a followed browse scope is active: `items` then carries
   *  that folder's listing (fetched on demand from its home nest) and `filter`
   *  carries the option's `value`.
   *
   *  ⚠ While set, the scope is **read-only, structurally**: route an item tap
   *  to `downloadFollowed`, and offer no upload, delete, restore or version
   *  history — the public plane is head-only. */
  followed_scope: FollowedScopeOption | null;
  /** Active `media-sort-select` value ("name" / "size" / "date"). */
  sort: string;
  /** Whether the active sort is descending. */
  descending: boolean;
  /** Active `media-folder-filter`: a set name, or null for the all-media default. */
  filter: string | null;
  /** Active `media-view-toggle`: true = thumbnail grid, false = list. */
  view_grid: boolean;
  /** Page-level `error-message` (last refresh/gesture failure); null when clear. */
  error: LocalizedText | null;
  /** Whether a `refresh()` has ever returned successfully — gates
   *  `media-empty-state` (README.md § List pages: loading is not empty). */
  loaded: boolean;
  /** The share-link create surface, while open (`share-links.md` § Flows). */
  share_create: ShareCreateSnapshot | null;
  /** The `share-link-expiry-select` values, shortest first; label each via
   *  `shareLinkExpiryLabel`. */
  share_expiry_options: string[];
  /** The share-link list surface. */
  share_links: ShareLinksSnapshot;
}
