import { base } from '$app/paths';
import { sharedAccountPort } from './account-runtime';
import type { DevicesMachine, SharedRpcPort } from '../../static/fauna_wasm_folders.js';
import type { LocalizedText } from '$lib/i18n/localized';
import type {
  CustodyFacetView,
  DeviceSummary,
  FolderMember,
  PlaceRow,
} from '$lib/devices-machine';

// Loader for the folder / Devices WASM chunk (`libs/fauna-wasm-folders`:
// `DevicesMachine` + the embedded `FolderWizardMachine`). A separate wasm
// module from `fauna_wasm` — same separate-chunk discipline as
// `wasm-onboarding.ts`: keep the constructor call inside the module that holds
// the singleton `wasmModule` reference (a different Vite chunk would carry its
// own copy of the wasm-bindgen boilerplate and constructing the class from
// elsewhere would hit uninitialized memory).
//
// The Devices page builds one `DevicesMachine` over the SPA singleton's socket,
// lent through `sharedRpcPort` (`$lib/rpc`) — pure data across the chunk
// boundary, one WebSocket per actor; see the chunk's constructor doc.

let wasmModule: typeof import('../../static/fauna_wasm_folders.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the folders wasm chunk exactly once per page load. The init
 *  *promise* is memoized (same load-bearing shape as `wasm.ts::ensureWasm`):
 *  two concurrent callers await the SAME in-flight init, so `mod.default()`
 *  runs once — a second concurrent call would re-instantiate the chunk and
 *  reset wasm linear memory. A failed init drops the cached promise so a
 *  later call can retry. */
export function ensureFoldersWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_folders.js');
      await mod.default(`${base}/fauna_wasm_folders_bg.wasm`);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) throw new Error('Folders WASM not initialized — call ensureFoldersWasm() first');
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh, gesture, or embedded-wizard transition). */
export interface DevicesMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `DevicesMachine` over the SPA singleton's socket —
 * `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`. `secretHex` is the
 * owner's own actor secret: the folder create and delete write the set's
 * custody under it before the nest call. State starts empty — the caller
 * drives `refresh()`.
 */
export async function createDevicesMachine(
  observer: DevicesMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<DevicesMachine> {
  await ensureFoldersWasm();
  return new (wasm().DevicesMachine)(
    observer,
    port,
    secretHex,
    // The account's folder-key custody — the create/delete helpers, the
    // foreign-set list and the label resolver — through the tab's account runtime.
    sharedAccountPort(secretHex),
  );
}

/** A canonical conflict-policy picker option (`folder-conflict-policy-select` /
 *  `sync-default-conflict-policy-select`) — the single source of the option
 *  *set* (file-sync.md § Conflicts, policy). */
export interface ConflictPolicyOption {
  value: string;
  label: LocalizedText;
}

/** The canonical conflict-policy picker option list. */
export function conflictPolicyOptions(): ConflictPolicyOption[] {
  return wasm().conflictPolicyOptions() as ConflictPolicyOption[];
}

/** A canonical member-access picker option (`folder-share-role-select` /
 *  `folder-member-role-select`) — the single source of the Reader/Writer option
 *  set (wire "reader"/"writer"), mirroring `conflictPolicyOptions` (folders.md §
 *  Sharing; multi-writer Phase 1). */
export interface MemberAccessOption {
  value: string;
  label: LocalizedText;
}

/** The canonical member-access picker option list (Reader default, then Writer). */
export function memberAccessOptions(): MemberAccessOption[] {
  return wasm().memberAccessOptions() as MemberAccessOption[];
}

/** The localized label for a stored member-access value; an unknown value
 *  degrades to Reader's label (mirrors `conflict_policy_label`). */
export function memberAccessLabel(value: string): LocalizedText {
  return wasm().memberAccessLabel(value) as LocalizedText;
}

/** The localized `conflict-type-badge` text for one auto-resolve review row —
 *  the single source that replaces a hand-rolled resolution/type switch.
 *  Mirrors `conflictPolicyOptions`. */
export function conflictBadgeLabel(
  resolution: string | null | undefined,
  resolvedAt: number | null | undefined,
  conflictType: string,
): LocalizedText {
  return wasm().conflictBadgeLabel(
    resolution ?? undefined,
    resolvedAt != null ? BigInt(resolvedAt) : undefined,
    conflictType,
  ) as LocalizedText;
}

/** One `folder-audience-select` option (ui/folders.md § Audience and website
 *  serving). Mirrors `MemberAccessOption`, plus the `selectable` bit. */
export interface AudienceOption {
  value: string;
  label: LocalizedText;
  /** `false` for exactly one case — `shared` on a group-bound folder that is
   *  not currently `public` — which is rendered because a bound folder must be
   *  able to say what it is, never offered, because bound-ness is entered
   *  through the share flow alone. Once the folder IS `public`, `shared`
   *  becomes selectable: the flip-back, the one exit from its public
   *  window, whose pick re-seals the corpus for its members. **Honour it**: a
   *  disabled option is the difference between a picker that states the rule
   *  and a control that fails on click. */
  selectable: boolean;
}

/** The `folder-audience-select` option set for ONE row — exactly the
 *  transitions the nest accepts from here, on the NORMALIZED current audience.
 *  An unbound folder is offered `private` + `public`; a bound one renders
 *  `shared` + `public`, `private` being withheld because the nest refuses it
 *  while bound and the honest repair is to remove the sharing first. `shared`
 *  is selectable exactly while the bound folder is `public` (the flip-back). */
export function audienceOptions(bound: boolean, current: string): AudienceOption[] {
  return wasm().audienceOptions(bound, current) as AudienceOption[];
}

/** The hint beside `folder-audience-select`, on the same `(bound, current)`
 *  inputs as `audienceOptions` so the copy and the option set cannot disagree:
 *  unbound explains private/public, bound points at the sharing section,
 *  bound-and-`public` explains that picking Shared is the way back. */
export function audienceHint(bound: boolean, current: string): LocalizedText {
  return wasm().audienceHint(bound, current) as LocalizedText;
}

/** The audience value the select should PAINT, given the stored wire value.
 *
 *  Not derived in the SPA: a select showing a value outside its own option set
 *  is unpaintable, and the case is reachable — an absent or unrecognized
 *  value. **Fail-closed**: anything unrecognized becomes
 *  `shared` when bound and `private` when not, never `public`. */
export function normalizeAudience(value: string, bound: boolean): string {
  return wasm().normalizeAudience(value, bound) as string;
}

/** The TRI-state hint beside `folder-website-toggle`.
 *
 *  `audience` is the **normalized** value; `addressEnabled` is the actor's
 *  web-address opt-in off `DevicesSnapshot.website_address_enabled`, read
 *  best-effort — so `null`/`undefined` is a real arm (unwired
 *  adapter, failed read) and it hedges rather than claiming the site is live. */
export function websiteServeHint(
  audience: string,
  paywalled: boolean,
  addressEnabled: boolean | null | undefined,
): LocalizedText {
  return wasm().websiteServeHint(
    audience,
    paywalled,
    addressEnabled ?? undefined,
  ) as LocalizedText;
}

/** The `folder-writer-published-warning` copy for one member, or `null` when the
 *  grant reaches nobody outside the set.
 *
 *  A `writer` grant on a `public` or paywalled folder changes what people
 *  OUTSIDE the set read — the reach test is the one `websiteServeHint` applies,
 *  so the two cannot drift. `access` is the member's wire value (an absent row
 *  means reader); `audience` is the **normalized** value. Advisory only. */
export function writerGrantReach(
  access: string,
  audience: string,
  paywalled: boolean,
): LocalizedText | null {
  return (
    (wasm().writerGrantReach(access, audience, paywalled) as
      | LocalizedText
      | undefined) ?? null
  );
}

/** The label for a stored audience value, where no picker is drawn. An
 *  unrecognized value degrades to `private`'s label — a string the binary could
 *  not parse must never be painted `Public`. Mirrors `memberAccessLabel`. */
export function audienceLabel(value: string): LocalizedText {
  return wasm().audienceLabel(value) as LocalizedText;
}

/** One `folder-nest-residency-select` option (folders re-model phase 5 —
 *  file-sync.md § Content residency). Mirrors `AudienceOption` minus
 *  `selectable` — both options are always selectable; the flip to
 *  metadata-only is confirm-gated in this page, not withheld. */
export interface ResidencyOption {
  value: string;
  label: LocalizedText;
}

/** The `folder-nest-residency-select` picker: Full (default) then
 *  Metadata-only, in that order on every app. */
export function residencyOptions(): ResidencyOption[] {
  return wasm().residencyOptions() as ResidencyOption[];
}

/** The residency value the select should PAINT, given the stored wire value.
 *
 *  Not derived in the SPA: a select showing a value outside its own option set
 *  is unpaintable, and the case is reachable — the nest sends an empty value for full.
 *  **Fail-closed to Full** — only the exact `metadata_only` value paints as
 *  metadata-only, since that reading is the claim the destructive confirm is
 *  gated on. */
export function normalizeResidency(value: string): string {
  return wasm().normalizeResidency(value) as string;
}

/** The label for a stored residency value, where no picker is drawn. Mirrors
 *  `audienceLabel`. */
export function residencyLabel(value: string): LocalizedText {
  return wasm().residencyLabel(value) as LocalizedText;
}

/** The hint beside `folder-nest-residency-select`, on the same NORMALIZED
 *  value the select paints so copy and control agree: a full folder explains
 *  what the nest's copy buys, a metadata-only folder states the availability
 *  cost it accepted. */
export function residencyHint(current: string): LocalizedText {
  return wasm().residencyHint(current) as LocalizedText;
}

/** A canonical `folder-nest-snapshots-select` option — the three-state
 *  keeps-snapshots knob of the nest place's policy (backup-restore.md § 8b).
 *  Mirrors `conflictPolicyOptions`; the default state leads the list, because it
 *  is where the knob rests and where it returns. */
export interface NestSnapshotsOption {
  value: string;
  label: LocalizedText;
}

/** The canonical `folder-nest-snapshots-select` option list. */
export function nestSnapshotsOptions(): NestSnapshotsOption[] {
  return wasm().nestSnapshotsOptions() as NestSnapshotsOption[];
}

/** The four `folder-nest-*` control values. */
export interface NestPlaceEdit {
  snapshots: string;
  quiet_secs: string;
  retention_snapshots: string;
  retention_days: string;
}

/** Seed the four `folder-nest-*` controls from a folder row.
 *
 *  ⚠ Do NOT derive these here. An unset knob prefills **blank**, and so does a
 *  **zero** retention bound — zero is the nest's own spelling of unset, so
 *  rendering it would turn "nothing chosen" into a bound the owner appears to
 *  have picked, and the two spellings would drift on the next save. */
export function nestPlaceEditFromRow(
  nestSnapshots: boolean | null | undefined,
  quietSecs: number | null | undefined,
  retentionPolicy: string | null | undefined,
): NestPlaceEdit {
  return wasm().nestPlaceEditFromRow(
    nestSnapshots ?? undefined,
    quietSecs ?? undefined,
    retentionPolicy ?? undefined,
  ) as NestPlaceEdit;
}

/** The version-retention SIBLING pair's control values
 *  (`folder-version-retention-count`/`-days`) — bounds file-version history,
 *  never snapshots (`file-versions.md` § Retention ruling 1, apps row 323). */
export interface VersionRetentionEdit {
  count: string;
  days: string;
}

/** Seed the version-retention pair from a folder row's
 *  `version_retention_max_versions` / `_max_age_days`.
 *
 *  ⚠ Do NOT derive these here — same blank-is-a-value rule as
 *  [`nestPlaceEditFromRow`]: a **zero** bound prefills BLANK, never `"0"`. */
export function versionRetentionEditFromBounds(
  maxVersionsPerPath: number,
  maxAgeDays: number,
): VersionRetentionEdit {
  return wasm().versionRetentionEditFromBounds(
    maxVersionsPerPath,
    maxAgeDays,
  ) as VersionRetentionEdit;
}

/** Project a device roster (`foldersMembers`) into the rows the device-place
 *  editor paints, in roster order, each seat NAMED from `devices` — the page
 *  snapshot's already-unsealed device list. Every user-chosen device label
 *  rests sealed, so `foldersMembers` sends a named seat's label empty.
 *
 *  ⚠ Do NOT derive these here, and do not filter or re-sort them:
 *  `folder-place-row[j]` is what the e2e contract addresses. */
export function placeRows(members: FolderMember[], devices: DeviceSummary[]): PlaceRow[] {
  return wasm().placeRows(members, devices) as PlaceRow[];
}

/** Flip ONE checkbox on a place row and return the WHOLE resulting point — the
 *  three booleans `setFolderPlace` takes.
 *
 *  ⚠ A place is only ever written whole: sending just the box that moved would
 *  clear the two left alone. `null` for an unrecognised flag — there is
 *  no partial edit to fall back to. */
export function toggledPlaceRow(row: PlaceRow, flag: string): PlaceRow | null {
  return wasm().toggledPlaceRow(row, flag) as PlaceRow | null;
}

/** Parse the `folder-include-paths` / `folder-exclude-paths` edit field into
 *  the typed `string[]` `fauna.folders.update` takes (comma-split, trimmed,
 *  empties dropped). Always returns an array — an emptied field parses to
 *  `[]`. The single source replacing a page-local split/trim/filter hand-roll. */
export function parsePathsField(text: string): string[] {
  return wasm().parsePathsField(text);
}

/** Render stored selective-sync paths back into the single-line edit field
 *  `parsePathsField` reads: comma+space-joined; an absent/empty list renders
 *  as `""`. The inverse half of the same lift. */
export function joinPathsField(paths: string[] | null | undefined): string {
  return wasm().joinPathsField(paths ?? undefined);
}

// ── T16 custody facet, owner side (devices.md § Custody facet, piece 2) ──

/** The i18n key for the marker a receipt whose `degraded` is set carries.
 *  A function rather than a hardcoded string so the seven apps agree on the key
 *  instead of each spelling it — the same reason `conflictPolicyLabel` exists. */
export function custodyDegradedBadgeKey(): string {
  return wasm().custodyDegradedBadgeKey();
}

/**
 * Fold the custody facet off this tab's account store: the ceremony records
 * (`fauna.state.custody-ceremony`) and the grant log (the succession ledger),
 * both lent over the account port — `sharedAccountPort`.
 *
 * `null` = either was unreadable this pass (no runtime in this tab included) —
 * a **transient**, not "no custodians": the caller keeps whatever rows it
 * already painted rather than blanking live ones.
 *
 * ⚠ Renders **piece 2 only**. `held`/`offers` cross so the boundary reports what
 * the fold found, but their controls write the R14 (account-data-plane.md § The ratified decisions) registry row, which no
 * custody seam crosses the port for yet.
 */
export async function custodyFacetLoad(
  machine: DevicesMachine,
  secretHex: string,
): Promise<CustodyFacetView | null> {
  return (await machine.custodyFacetLoad(
    secretHex,
    sharedAccountPort(secretHex),
  )) as CustodyFacetView | null;
}

/**
 * Revoke a custody grant (`custody-holder-revoke-button`) — piece 2's one
 * gesture, and the one custody act needing nothing native: the assembly, the
 * load-bearing nest-before-record order included, is the SAME shared function
 * the four native apps call.
 *
 * Resolves to the error string for the page's `error-message`, or `null` on
 * success — never a silent drop (e2e convention 11). Re-run
 * [`custodyFacetLoad`] afterwards to repaint.
 *
 * ⚠ `grantId`/`holder` arrive from the row as plain **number arrays** (serde,
 * not `Uint8Array`), and wasm-bindgen takes byte slices — hence the conversion
 * here, once, rather than at each call site.
 */
export async function custodyRevoke(
  machine: DevicesMachine,
  secretHex: string,
  grantId: number[],
  holder: number[] | null,
): Promise<string | null> {
  return (await machine.custodyRevoke(
    secretHex,
    Uint8Array.from(grantId),
    holder ? Uint8Array.from(holder) : undefined,
    sharedAccountPort(secretHex),
  )) as string | null;
}

/**
 * One stored follow record, as [`followPublicFolder`] / [`followedFolders`]
 * resolve it — the record WITHOUT an availability verdict. Rows that carry one
 * come from the snapshot's `followed` ({@link FollowedFolderSummary}'s home,
 * `$lib/devices-machine`); this is the raw `fauna.state.follows` row value
 * (serde snake_case, like every snapshot type).
 */
export interface FollowedFolderRecord {
  /** Empty ⇒ homed on the user's own nest (the same-nest follow). */
  home_nest_url: string;
  /** The home nest's SPKI-pin identity; absent if a reply carried none. */
  home_nest_actor_id?: string | null;
  owner_actor_id: string;
  /** The handle the follow was addressed by; absent for a follow by actor id. */
  owner_handle?: string | null;
  folder_id: number;
  display_name: string;
}

/**
 * Wire the followed-folders source so the snapshot's `followed` rows populate
 * (`ui/folders.md` § Following a public folder). Unwired, the list is simply
 * empty. Call right after constructing the machine, before the first
 * `refresh()`. The follows are the account's `fauna.state.follows` rows, read
 * across the one account port (`sharedAccountPort`) minted for `secretHex`'s
 * account.
 */
export function setFollowedFoldersSource(
  machine: DevicesMachine,
  secretHex: string,
): void {
  machine.setFollowedFoldersSource(sharedAccountPort(secretHex));
}

/**
 * Follow a public folder — first contact, addressed by the OWNER (a handle —
 * bare, or `handle@domain` for a folder homed on another nest — or a bare
 * 64-hex actor id, the same superset the share flow takes) + the folder's
 * plaintext name, exactly as the follow flow collects them (`ui/folders.md`
 * § Following a public folder). The address rules — hex-or-handle, the
 * same-nest lookup, the cross-nest discovery of the owner's home nest — are
 * the shared Rust recipe's (`follow_ops::follow_public_folder`); the SPA hands
 * the typed string through untouched. Resolves to the stored record; re-run
 * `refresh()` afterwards so the rows repaint with their availability.
 *
 * A folder that is absent, private, or misspelled all **reject identically**,
 * deliberately (the home nest folds them so nothing can probe for a sealed
 * folder's existence) — put the rejection's string on the page's
 * `error-message`, never a friendlier per-case message and never a silent
 * swallow (e2e convention 2). The rejection's string is already the
 * localized catalog wording, so it goes on the page as-is. The record lands in
 * the account's store across the one account port (`sharedAccountPort`); with
 * no runtime serving the account in this tab the follow rejects.
 */
export async function followPublicFolder(
  machine: DevicesMachine,
  secretHex: string,
  owner: string,
  folderName: string,
): Promise<FollowedFolderRecord> {
  return (await machine.followPublicFolder(
    secretHex,
    owner,
    folderName,
    sharedAccountPort(secretHex),
  )) as FollowedFolderRecord;
}

/**
 * Unfollow — the account's row is tombstoned across the account port (the
 * home nest never knew about this follower, so there is nothing to revoke
 * anywhere). Idempotent; resolves to the stored list.
 *
 * ⚠ `folderId` crosses as a JS **bigint** (the export's `i64` parameter); the
 * conversion from the row's plain `number` lives here, once, rather than at
 * each call site.
 */
export async function unfollowPublicFolder(
  machine: DevicesMachine,
  secretHex: string,
  homeNestUrl: string,
  folderId: number,
): Promise<FollowedFolderRecord[]> {
  return (await machine.unfollowPublicFolder(
    secretHex,
    homeNestUrl,
    BigInt(folderId),
    sharedAccountPort(secretHex),
  )) as FollowedFolderRecord[];
}

/**
 * The user's followed public folders **as stored** — the availability-less
 * records, for a caller that only needs them (e.g. to decide whether an
 * address is already followed). The page's rows come from the snapshot's
 * `followed`.
 */
export async function followedFolders(
  machine: DevicesMachine,
  secretHex: string,
): Promise<FollowedFolderRecord[]> {
  return (await machine.followedFolders(
    secretHex,
    sharedAccountPort(secretHex),
  )) as FollowedFolderRecord[];
}
