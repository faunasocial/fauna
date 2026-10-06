//! WASM bindings for the Fauna Devices page.
//!
//! Two wrappers, both exposing JSON-string snapshot getters (the web parses
//! them) + a JS observer shim, mirroring `libs/fauna-wasm-onboarding`:
//! - [`DevicesMachine`] — the page-level machine (device / folder / conflict
//!   reads + page write gestures + the embedded wizard).
//! - [`FolderWizardMachine`] — the folder creation wizard.
//!
//! Loaded by the web Devices page so it renders off the shared machines instead
//! of local Svelte state.

use std::sync::Arc;

use fauna_folders_machine::{
    DeviceOption, FolderWizardMachine as InnerMachine, FolderWizardObserver as InnerObserver,
};
use fauna_wasm_panic_hook::err_to_js;
use wasm_bindgen::prelude::*;

/// The i18n key for the custody **degraded** marker — the badge that rides a
/// receipt reporting evicted or capped-short coverage (`docs/goal/ui/devices.md`
/// § Custody facet). Exported rather than hardcoded in the SPA so every app
/// agrees on the key.
///
/// ⚠ `degraded` is **orthogonal to freshness** — a FRESH receipt can truthfully
/// report dropped payload — so render this badge *alongside* the row's status
/// line, never instead of it.
#[wasm_bindgen(js_name = custodyDegradedBadgeKey)]
pub fn custody_degraded_badge_key() -> String {
    fauna_client_capabilities::view_model::CUSTODY_DEGRADED_BADGE_KEY.to_string()
}

/// The canonical conflict-policy picker option list
/// (`[{ value, label: { key, args } }]`) — the single source of the
/// `folder-conflict-policy-select` / `sync-default-conflict-policy-select`
/// option *set* (`fauna_folders_machine::conflict_policy_options`). The web
/// policy selects render this list and
/// resolve each `label` via `resolveLocalized(...)` — no hand-rolled value list
/// or label map.
#[wasm_bindgen(js_name = conflictPolicyOptions)]
pub fn conflict_policy_options() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::conflict_policy_options())
        .map_err(err_to_js)
}

/// The canonical i18n label (`LocalizedText` `{ key, args }`) for a stored
/// conflict-policy wire value (`"auto"` / `"latest_wins_always"`); an unknown
/// value degrades to `Auto`'s label, matching `ConflictPolicy::from_wire`.
#[wasm_bindgen(js_name = conflictPolicyLabel)]
pub fn conflict_policy_label(value: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::conflict_policy_label(&value))
        .map_err(err_to_js)
}

/// The canonical member-access picker option list (wire value + `LocalizedText`
/// label) — the single source of the `folder-share-role-select` /
/// `folder-member-role-select` option *set* (multi-writer Phase 1). Mirrors
/// `conflictPolicyOptions`; the web selects render this list and resolve each
/// `label` via `resolveLocalized(...)` — no hand-rolled value list.
#[wasm_bindgen(js_name = memberAccessOptions)]
pub fn member_access_options() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::member_access_options()).map_err(err_to_js)
}

/// The canonical label for a stored member-access wire value (`"reader"` /
/// `"writer"`); unknown/absent degrades to reader's label (the fail-safe
/// absent-row default). Mirrors `conflictPolicyLabel`.
#[wasm_bindgen(js_name = memberAccessLabel)]
pub fn member_access_label(value: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::member_access_label(&value))
        .map_err(err_to_js)
}

/// The canonical `folder-audience-select` option list for ONE row —
/// `[{ value, label: { key, args }, selectable }]` (`ui/folders.md` § Audience
/// and website serving).
///
/// Takes `bound` and the NORMALIZED `current` audience because **the option set
/// is exactly what the nest accepts from here**: an unbound row is offered
/// `private` + `public`, a bound row renders `shared` + `public` — and `shared`
/// is selectable exactly while the bound row is `public` (the
/// flip-back, the one exit from its public window; the pick re-seals the corpus
/// for its members). Anywhere else `shared` stays `selectable: false` —
/// bound-ness is entered through the share flow and nowhere else. The SPA must
/// honour `selectable`; offering what the nest would refuse produces a control
/// that fails on click.
#[wasm_bindgen(js_name = audienceOptions)]
pub fn audience_options(bound: bool, current: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::audience_options(bound, &current))
        .map_err(err_to_js)
}

/// The hint beside `folder-audience-select` (`LocalizedText`), on the same
/// `(bound, current)` inputs as [`audience_options`] so the copy and the option
/// set cannot disagree: unbound explains private/public, bound points at the
/// sharing section, bound-and-`public` explains that picking Shared is the way
/// back.
#[wasm_bindgen(js_name = audienceHint)]
pub fn audience_hint(bound: bool, current: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::audience_hint(bound, &current))
        .map_err(err_to_js)
}

/// The audience value `folder-audience-select` should PAINT, given the stored
/// wire value and whether the folder is group-bound.
///
/// The SPA does not derive this: a select showing a value outside its own option
/// set is unpaintable, and the case is reachable — any unparseable value (an empty string
/// included; `FolderSummary` defaults an absent audience to `""`) reaches it. **Fail-closed**:
/// anything unrecognized resolves to `shared` when bound and `private` when not,
/// never `public`.
#[wasm_bindgen(js_name = normalizeAudience)]
pub fn normalize_audience(value: String, bound: bool) -> String {
    fauna_folders_machine::normalize_audience(&value, bound)
}

/// The TRI-state hint beside `folder-website-toggle` (`LocalizedText`), keyed on
/// the live serving picture (`ui/folders.md` § Audience and website serving).
///
/// Publishing a site takes switches in TWO places — this page's audience +
/// website toggle, and the actor's own web-address opt-in (default OFF) — and a
/// user who flipped only the folder half was told nothing while the nest served
/// its info page in their site's place. `audience` is the **normalized** value;
/// `addressEnabled` rides `DevicesSnapshot.website_address_enabled`, read
/// best-effort, so `undefined` is a real arm and it hedges. The degrade
/// direction is deliberate: unknown never claims the site is live.
#[wasm_bindgen(js_name = websiteServeHint)]
pub fn website_serve_hint(
    audience: String,
    paywalled: bool,
    address_enabled: Option<bool>,
) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::website_serve_hint(
        &audience,
        paywalled,
        address_enabled,
    ))
    .map_err(err_to_js)
}

/// The `folder-writer-published-warning` copy for one member (`LocalizedText`),
/// or `undefined` when the grant reaches nobody outside the set (`ui/folders.md`
/// § Sharing).
///
/// A `writer` grant on a `public` or paywalled folder changes what people
/// OUTSIDE the set read — the reach test is the one `websiteServeHint` applies,
/// so the two cannot drift. `access` is the member's wire value (an absent row
/// means reader); `audience` is the **normalized** value. Advisory only: it never
/// blocks the grant, and it stacks with the uncapped-quota warning.
#[wasm_bindgen(js_name = writerGrantReach)]
pub fn writer_grant_reach(
    access: String,
    audience: String,
    paywalled: bool,
) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::writer_grant_reach(
        &access, &audience, paywalled,
    ))
    .map_err(err_to_js)
}

/// The label for a stored audience wire value — the read-only rendering, where
/// no picker is drawn. An unrecognized value degrades to `private`'s label: a
/// string this binary could not parse must never be painted `Public`. Mirrors
/// `memberAccessLabel`.
#[wasm_bindgen(js_name = audienceLabel)]
pub fn audience_label(value: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::audience_label(&value)).map_err(err_to_js)
}

/// The `folder-nest-residency-select` picker: Full (default) then
/// Metadata-only, in that order on every app (folders re-model phase 5;
/// `file-sync.md` § Content residency). Both options are always selectable —
/// the flip to metadata-only is confirm-gated in the app, not withheld.
#[wasm_bindgen(js_name = residencyOptions)]
pub fn residency_options() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::residency_options()).map_err(err_to_js)
}

/// The residency value `folder-nest-residency-select` should PAINT, given the
/// stored wire value. **Fail-closed to Full**: only the exact `metadata_only`
/// value paints as metadata-only — a binary that cannot parse the value must
/// not tell the user the nest holds no copy of their content, which is the
/// claim the metadata-only confirm is gated on.
#[wasm_bindgen(js_name = normalizeResidency)]
pub fn normalize_residency(value: String) -> String {
    fauna_folders_machine::normalize_residency(&value)
}

/// The label for a stored residency wire value — [`normalize_residency`]'s
/// fail-closed reading applied to the copy. Mirrors `audienceLabel`.
#[wasm_bindgen(js_name = residencyLabel)]
pub fn residency_label(value: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::residency_label(&value)).map_err(err_to_js)
}

/// The hint beside `folder-nest-residency-select`, keyed on the same
/// NORMALIZED value the select paints so copy and control agree: a full
/// folder explains what the nest's copy buys, a metadata-only folder states
/// the availability cost it accepted.
#[wasm_bindgen(js_name = residencyHint)]
pub fn residency_hint(current: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::residency_hint(&current))
        .map_err(err_to_js)
}

/// The canonical `folder-nest-snapshots-select` option list (wire value +
/// `LocalizedText` label) — the three-state keeps-snapshots knob of the nest
/// place's policy (`backup-restore.md` § 8b). Mirrors `conflictPolicyOptions`;
/// the default state leads, because it is where the knob rests and returns.
#[wasm_bindgen(js_name = nestSnapshotsOptions)]
pub fn nest_snapshots_options() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::nest_snapshots_options())
        .map_err(err_to_js)
}

/// Seed the four `folder-nest-*` controls from a folder row's
/// `nest_snapshots` / `nest_snapshot_quiet_secs` / `retention_policy`, returning
/// `{ snapshots, quiet_secs, retention_snapshots, retention_days }` — the values
/// the boxes should show.
///
/// The SPA does not derive these: an unset knob prefills **blank**, and so does
/// a zero retention bound, because zero is the nest's own spelling of unset —
/// rendering the `0` would turn an unset bound into one the owner appears to
/// have chosen. `quietSecs` crosses as an optional `number` rather than an
/// `Option<i64>`, which would reach JS as a `bigint`.
#[wasm_bindgen(js_name = nestPlaceEditFromRow)]
pub fn nest_place_edit_from_row(
    nest_snapshots: Option<bool>,
    quiet_secs: Option<f64>,
    retention_policy: Option<String>,
) -> Result<JsValue, JsValue> {
    let edit = fauna_folders_machine::nest_place_edit_from_row(
        nest_snapshots,
        quiet_secs.map(|s| s as i64),
        retention_policy,
    );
    serde_wasm_bindgen::to_value(&edit).map_err(err_to_js)
}

/// Seed the version-retention SIBLING pair (`folder-version-retention-count`/
/// `-days`) from a folder row's `version_retention_max_versions` /
/// `_max_age_days` — its own `folders.version_retention` wire field, never
/// folded into `retention_policy` (`file-versions.md` § Retention ruling 1,
/// apps row 323). Same blank-is-a-value rule as [`nest_place_edit_from_row`]:
/// a zero bound prefills BLANK, never `"0"`.
#[wasm_bindgen(js_name = versionRetentionEditFromBounds)]
pub fn version_retention_edit_from_bounds(
    max_versions_per_path: f64,
    max_age_days: f64,
) -> Result<JsValue, JsValue> {
    let edit = fauna_folders_machine::version_retention_edit_from_bounds(
        max_versions_per_path as u32,
        max_age_days as u32,
    );
    serde_wasm_bindgen::to_value(&edit).map_err(err_to_js)
}

/// Project a device roster (`foldersMembers`) into the rows the device-place
/// editor paints: `{ device_id, label, originates, accepts, applies_deletes }`,
/// in roster order — `folder-place-row[j]` is what the cross-app e2e contract
/// addresses, so the SPA must not filter or re-sort them.
///
/// `devices` is the page snapshot's device roster (`DevicesSnapshot.devices`,
/// already unsealed) — it NAMES the seats. Every user-chosen device label rests
/// sealed, so `members.list` sends a named seat's label empty; a seat the roster
/// does not hold keeps the nest's label (`fauna_devices_machine::place_rows`).
#[wasm_bindgen(js_name = placeRows)]
pub fn place_rows(members: JsValue, devices: JsValue) -> Result<JsValue, JsValue> {
    let members: Vec<fauna_protocol::folders::FolderMember> =
        serde_wasm_bindgen::from_value(members).map_err(err_to_js)?;
    let devices: Vec<fauna_devices_machine::DeviceSummary> =
        serde_wasm_bindgen::from_value(devices).map_err(err_to_js)?;
    serde_wasm_bindgen::to_value(&fauna_devices_machine::place_rows(&members, &devices))
        .map_err(err_to_js)
}

/// Flip ONE checkbox on a projected place row and return the WHOLE resulting
/// point — the three booleans `setFolderPlace` takes.
///
/// `flag` is one of `"originates"` / `"accepts"` / `"applies_deletes"`.
/// Returns `null` for an unrecognised flag: there is no partial edit to fall
/// back to, because a place is only ever written whole — sending just the box
/// that moved would clear the two left alone.
#[wasm_bindgen(js_name = toggledPlaceRow)]
pub fn toggled_place_row(row: JsValue, flag: String) -> Result<JsValue, JsValue> {
    let row: fauna_protocol::folders::PlaceRow =
        serde_wasm_bindgen::from_value(row).map_err(err_to_js)?;
    match fauna_protocol::folders::toggled(&row, &flag) {
        Some(next) => serde_wasm_bindgen::to_value(&next).map_err(err_to_js),
        None => Ok(JsValue::NULL),
    }
}

/// The localized `conflict-type-badge` text (`LocalizedText` `{ key, args }`)
/// for one auto-resolve review row — the resolution (`merged` / `latest-kept`)
/// when resolved, else the conflict type; an unknown/live type degrades to the
/// raw wire string via `resolveLocalized`'s missing-key fallback. Derived from
/// the three `ConflictSummary` fields the web page already holds
/// (`resolution`, `resolved_at`, `conflict_type`) — the single source that
/// replaces the local `resolutionBadge()` switch. Mirrors `conflictPolicyLabel`.
#[wasm_bindgen(js_name = conflictBadgeLabel)]
pub fn conflict_badge_label(
    resolution: Option<String>,
    resolved_at: Option<i64>,
    conflict_type: String,
) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_folders_machine::conflict_badge_label(
        resolution.as_deref(),
        resolved_at,
        &conflict_type,
    ))
    .map_err(err_to_js)
}

/// Parse the `folder-include-paths` / `folder-exclude-paths` edit field into
/// the typed `string[]` `fauna.folders.update` takes (comma-split, trimmed,
/// empties dropped). Always returns an array — an emptied field parses to `[]`
/// (clear the filter), never an absent field (leave unchanged). The single
/// source replacing the page-local `split(',').map(trim).filter(Boolean)`
/// hand-roll. Mirrors `conflictBadgeLabel`.
#[wasm_bindgen(js_name = parsePathsField)]
pub fn parse_paths_field(text: String) -> Vec<String> {
    fauna_folders_machine::parse_paths_field(&text)
}

/// Render stored selective-sync paths back into the single-line edit field
/// `parsePathsField` reads: comma+space-joined; an absent or empty list
/// renders as `""`. The inverse half of the same lift.
#[wasm_bindgen(js_name = joinPathsField)]
pub fn join_paths_field(paths: Option<Vec<String>>) -> String {
    fauna_folders_machine::join_paths_field(paths.as_deref())
}

#[wasm_bindgen]
extern "C" {
    pub type JsFolderWizardObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsFolderWizardObserver);
}

struct ObserverShim(JsFolderWizardObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
// Send + Sync are required by the trait bounds but never exercised at runtime.
unsafe impl Send for ObserverShim {}
unsafe impl Sync for ObserverShim {}
impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

#[wasm_bindgen]
pub struct FolderWizardMachine(Arc<InnerMachine>);

#[wasm_bindgen]
impl FolderWizardMachine {
    /// Build the wizard over the SPA core chunk's socket, lent as `port` (a
    /// `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's `requestRaw`
    /// runs `submit()`'s `fauna.folders.create` + `places.set`, so web keeps
    /// one WebSocket per actor). Throws on an object that is not a port.
    /// `availableDevices` is a JSON-deserializable array of
    /// `{ device_id: string, label: string }`.
    ///
    /// The web Devices page builds its wizard through [`DevicesMachine`]'s
    /// embedded one; this standalone constructor is the chunk's own face of
    /// the same machine, over the same port.
    ///
    /// `accountPort` is the tab's account runtime (a `SharedAccountPort`
    /// minted for this account): the create writes the new set's custody
    /// through it, as [`DevicesMachine`]'s does. `secretHex` is the caller's
    /// own 32-byte actor secret, as [`DevicesMachine`] takes it: the owner's
    /// seal root derives from it, so a created set is sealed from birth.
    /// Throws on a malformed secret.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsFolderWizardObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        secret_hex: String,
        account_port: fauna_account_port::JsAccountPort,
        available_devices: JsValue,
    ) -> Result<FolderWizardMachine, JsValue> {
        let devices: Vec<DeviceOption> =
            if available_devices.is_undefined() || available_devices.is_null() {
                Vec::new()
            } else {
                serde_wasm_bindgen::from_value(available_devices)
                    .map_err(|e| JsValue::from_str(&format!("invalid availableDevices: {e}")))?
            };
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let keypair = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let owner_seal = fauna_core::crypto::BackupKey::derive(keypair.secret_bytes());
        let custody = port_folder_keys(account_port)?;
        Ok(FolderWizardMachine(
            fauna_folders_machine::build_folder_wizard_machine(
                client,
                Some(custody),
                Some(owner_seal),
                observer,
                devices,
            ),
        ))
    }

    // ── Read surface (JSON strings; web parses) ─────────────────────────

    #[wasm_bindgen(js_name = step)]
    pub fn step(&self) -> String {
        format!("{:?}", self.0.step())
    }

    #[wasm_bindgen(js_name = nameSnapshotJson)]
    pub fn name_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.name_snapshot()).unwrap_or_default()
    }

    /// Step 2 as the three place-flag checkboxes render it — the wizard's device
    /// step since folders re-model phase 2 slice e. Each `devices[i]` carries
    /// `originates` / `accepts` / `applies_deletes`. Every flag point is a valid
    /// place, so there is no refusal for the SPA to render and
    /// `continue_enabled` is always true.
    #[wasm_bindgen(js_name = devicePlacesSnapshotJson)]
    pub fn device_places_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.device_places_snapshot()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = reviewSnapshotJson)]
    pub fn review_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.review_snapshot()).unwrap_or_default()
    }

    /// The whole wizard in one JSON object (slots into `DevicesSnapshot.wizard`).
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    // ── Gestures ────────────────────────────────────────────────────────

    #[wasm_bindgen(js_name = setName)]
    pub fn set_name(&self, name: String) {
        self.0.set_name(name)
    }

    #[wasm_bindgen(js_name = toggleDeviceMember)]
    pub fn toggle_device_member(&self, index: u32) {
        self.0.toggle_device_member(index)
    }

    /// Set device `index`'s three place flags — the
    /// `wizard-device-{originates,accepts,applies-deletes}` checkboxes.
    #[wasm_bindgen(js_name = setDeviceFlags)]
    pub fn set_device_flags(
        &self,
        index: u32,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) {
        self.0
            .set_device_flags(index, originates, accepts, applies_deletes)
    }

    /// Inject the user's global default conflict policy (`"auto"` |
    /// `"latest_wins_always"`, from `loadSyncPrefs`) so `submit()`
    /// stamps it onto the create — the Sync-defaults contract (file-sync.md §
    /// Conflicts, policy). Not a wizard step; call once right after opening.
    #[wasm_bindgen(js_name = setDefaultConflictPolicy)]
    pub fn set_default_conflict_policy(&self, policy: Option<String>) {
        self.0.set_default_conflict_policy(policy)
    }

    #[wasm_bindgen(js_name = next)]
    pub fn next(&self) {
        self.0.next()
    }

    #[wasm_bindgen(js_name = back)]
    pub fn back(&self) {
        self.0.back()
    }

    /// Commit the folder. Resolves to the resulting step name
    /// (`"Done"` on success; `"Review"` on failure — read `reviewSnapshotJson`
    /// for the error / partial-failure detail).
    #[wasm_bindgen(js_name = submit)]
    pub fn submit(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = inner.submit().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }
}

// ── Page-level DevicesMachine ────────────────────────────────────────────────

use fauna_devices_machine::{
    DevicesMachine as InnerDevices, DevicesObserver as InnerDevicesObserver,
    MlsQuery as InnerMlsQuery,
};

#[wasm_bindgen]
extern "C" {
    pub type JsDevicesObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsDevicesObserver);
}

struct DevicesObserverShim(JsDevicesObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for DevicesObserverShim {}
unsafe impl Sync for DevicesObserverShim {}
impl InnerDevicesObserver for DevicesObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

#[wasm_bindgen]
extern "C" {
    /// The browser's local MLS join-filter — the web `MlsQuery` impl
    /// (`docs/goal/ui/folders.md` § Sharing, *Member list-visibility*). The SPA
    /// backs `isJoinedSharedSet` with the MAIN `fauna_wasm` bundle's
    /// `WasmConversationsManager.foldersIsJoined`, because the per-actor
    /// `MlsEngine` lives in that bundle and a wasm object cannot cross between two
    /// wasm-pack modules — the JS callback IS the bridge. Only this impl is
    /// per-app; the filter itself is centralized in `fauna-devices-machine`.
    pub type JsMlsQuery;
    #[wasm_bindgen(method, js_name = isJoinedSharedSet)]
    fn is_joined_shared_set(this: &JsMlsQuery, mls_group_id_hex: &str) -> bool;
}

struct MlsQueryShim(JsMlsQuery);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for MlsQueryShim {}
unsafe impl Sync for MlsQueryShim {}
impl InnerMlsQuery for MlsQueryShim {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        self.0.is_joined_shared_set(mls_group_id_hex)
    }
}

#[wasm_bindgen]
pub struct DevicesMachine(
    Arc<InnerDevices>,
    fauna_rpc_wasm::WsRpcClient,
    Arc<dyn fauna_client_folders::FolderKeyStore>,
);

#[wasm_bindgen]
impl DevicesMachine {
    /// Build the page machine over the SPA core chunk's socket, lent as
    /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's
    /// `requestRaw` runs every request this machine and its embedded wizard
    /// make, so web keeps one WebSocket per actor — and a gesture issued while
    /// the app's `connection` indicator reads online rides a socket that IS
    /// online, never a chunk-private one still asleep in backoff). Throws on
    /// an object that is not a port. State starts empty; call `refresh()` to
    /// populate it.
    ///
    /// `secretHex` is the caller's own 32-byte actor secret (a restore's change
    /// record is signed with it); `accountPort` is the tab's account runtime,
    /// lent as a `SharedAccountPort` minted for this account — the folder
    /// create and delete gestures write the set's custody through it before
    /// the nest call, and the foreign-set list and the label resolver read it
    /// (`fauna.state.folder-keys`; `mls-group-key-material.md` § M2 → *Custody
    /// shape of the set nonce*). Throws on a malformed secret or a non-port.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsDevicesObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        secret_hex: String,
        account_port: fauna_account_port::JsAccountPort,
    ) -> Result<DevicesMachine, JsValue> {
        let observer: Arc<dyn InnerDevicesObserver> = Arc::new(DevicesObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let identity = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let custody = port_folder_keys(account_port)?;
        let inner = fauna_devices_machine::build_devices_machine(
            client.clone(),
            Some(identity),
            Some(Arc::clone(&custody)),
            observer,
        );
        Ok(DevicesMachine(inner, client, custody))
    }

    /// Wire the browser's local MLS join-filter — the load-bearing B3 safety seam
    /// (`docs/goal/ui/folders.md` § Sharing, *Member list-visibility*). The nest
    /// returns every **rostered** member set, but rostered ≠ joined: a stranger's
    /// knock rosters you before you accept, so a row must render only once this
    /// client actually holds the MLS group. **Fail-safe: until this is called every
    /// `role == "member"` row is dropped**, so an unaccepted knock can never surface
    /// as a folder of yours — it surfaces only as a `folder-pending-share`.
    /// Call it right after constructing the machine, before the first `refresh()`.
    #[wasm_bindgen(js_name = setMlsQuery)]
    pub fn set_mls_query(&self, query: JsMlsQuery) {
        self.0.set_mls_query(Arc::new(MlsQueryShim(query)));
    }

    /// Wire the foreign-set (cross-nest) list source
    /// (`docs/goal/ui/folders.md` § Implementation status today) — the
    /// `setMlsQuery` pattern for
    /// [`fauna_devices_machine::ForeignSetsSource`]: until called, a set
    /// shared from another nest has no row in this list (fail-safe: same-nest
    /// shares still work, no error). `secretHex` is the caller's own 32-byte
    /// actor secret — the same hex the SPA already holds for its other
    /// secret-keyed wiring calls. Call it right after
    /// constructing the machine, before the first `refresh()`, mirroring
    /// `setMlsQuery`.
    #[wasm_bindgen(js_name = setForeignSetsSource)]
    pub fn set_foreign_sets_source(&self, secret_hex: String) -> Result<(), JsValue> {
        // The records are read from the account's folder-key custody through
        // the account port the machine was built over; the secret still gates
        // the call.
        fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        self.0.set_foreign_sets_source(Arc::new(
            fauna_devices_machine::CustodyForeignSetsSource::new(self.2.clone()),
        ));
        Ok(())
    }

    /// Wire the **followed public folders** source — the `setForeignSetsSource`
    /// pattern for [`fauna_devices_machine::FollowedFoldersSource`]
    /// (`docs/goal/behavior/folders.md` § Publicly-synced follow).
    ///
    /// A follow lives entirely in the account's own `fauna.state.follows` rows
    /// (the home nest keeps no follower state), which the core chunk's account
    /// runtime holds: the shared `StoreFollowedFoldersSource` reads them across
    /// the account `port` (a `SharedAccountPort` minted for this account — the
    /// followed-folders seam's forwarder) and probes each folder's
    /// availability. Until called, the page simply carries no followed rows —
    /// which is the correct render for an app that has not built the follow
    /// surface, not an error; with no runtime serving the account in this tab,
    /// the last rows stand. Call it right after constructing the machine,
    /// before the first `refresh()`.
    #[wasm_bindgen(js_name = setFollowedFoldersSource)]
    pub fn set_followed_folders_source(
        &self,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<(), JsValue> {
        self.0.set_followed_folders_source(Arc::new(
            fauna_devices_machine::StoreFollowedFoldersSource::new(
                self.1.clone(),
                follows_store(port)?,
            ),
        ));
        Ok(())
    }

    /// Wire this connection's label custody — the owner backup key plus the
    /// shared-folder content-key resolver — so a sealed `device-name` decodes
    /// instead of degrading to the id-shaped fallback (`devices.md` § This-device
    /// marker; `libs/fauna-devices-machine`'s `render_devices` `Omit`s an unopenable
    /// sealed label, and `DevicesSection.svelte` falls back to
    /// `shortId(device.device_id)`). The `setForeignSetsSource`/`setMlsQuery`
    /// pattern: `secretHex` is the caller's own 32-byte actor secret, same as
    /// `setForeignSetsSource`. Mirrors linux's `FaunaClient::label_custody()` +
    /// `machine.set_label_custody(...)` call-site pattern — wasm has no native
    /// `FaunaClient` to read it off, so this exists as its own export
    /// (web's last remaining leg). Call it right after
    /// constructing the machine, before the first `refresh()`, mirroring the
    /// other two `set*` wiring calls above.
    #[wasm_bindgen(js_name = setLabelCustody)]
    pub fn set_label_custody(&self, secret_hex: String) -> Result<(), JsValue> {
        let keypair = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let owner_key = fauna_core::crypto::BackupKey::derive(keypair.secret_bytes());
        let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> = Arc::new(
            fauna_client_folders::NestFolderKeyResolver::new(self.1.clone(), self.2.clone()),
        );
        self.0
            .set_label_custody(fauna_core::label_custody::LabelCustody::new(
                Some(resolver),
                Some(owner_key),
            ));
        Ok(())
    }

    /// Wire the owner's identity key so `setFolderAudience(name, 'public')`
    /// carries the owner's signed attestation (`encryption-at-rest.md`
    /// § Readable classes → *The declassification is owner-ATTESTED*). Unwired,
    /// the flip still lands but no verifying seat unseals the folder — it reads
    /// as public and rests sealed. `secretHex` is the caller's own 32-byte actor
    /// secret, as `setLabelCustody` takes it; call it right after constructing
    /// the machine, beside the other `set*` wiring calls. The browser twin of
    /// what `fauna-ffi`'s `build_devices_machine` wires for apple / android /
    /// windows.
    #[wasm_bindgen(js_name = setAudienceAttestor)]
    pub fn set_audience_attestor(&self, secret_hex: String) -> Result<(), JsValue> {
        let keypair = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        self.0.set_audience_attestor(Arc::new(keypair));
        Ok(())
    }

    /// Wire the tab's account runtime, lent as `port` (a `SharedAccountPort`
    /// — `$lib/account-runtime`'s `sharedAccountPort`, minted for this
    /// machine's account), as the page's fleet door: a removal then resolves
    /// and stages its fleet leg BEFORE the nest deletion and settles it on
    /// the outcome, and the member group reads through it — the machine and
    /// adapter the six native apps run, the runtime answering from the core
    /// chunk (`account-client-lifecycle.md` § The client-side lifecycle →
    /// *The account port*; `account-data-taxonomy.md` § *Fleet-scope
    /// reclamation*, clause (4)). With no runtime serving this account the
    /// removal is refused and deletes nothing. Throws on an object that is
    /// not a port. Call it beside the other `set*` wiring calls, before the
    /// first `refresh()`.
    #[wasm_bindgen(js_name = setAccountPort)]
    pub fn set_account_port(&self, port: fauna_account_port::JsAccountPort) -> Result<(), JsValue> {
        let transport = fauna_account_port::JsAccountTransport::new(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.0.set_fleet_removal(Arc::new(
            fauna_devices_machine::port::PortFleetRemoval::new(transport),
        ));
        Ok(())
    }

    /// The whole renderable Devices page in one JSON object
    /// (`{ devices, folders, conflicts, wizard, error }`).
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// The refresh barrier's `{started, completed, committed_gen}` triple as
    /// JSON (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`, which owns the contract)
    /// — three atomic reads; the counting and the shape are both shared Rust.
    #[wasm_bindgen(js_name = refreshesJson)]
    pub fn refreshes_json(&self) -> String {
        self.0.refreshes_json()
    }

    /// The open folder wizard, if any — drivable via the returned
    /// [`FolderWizardMachine`]. `undefined` when no wizard is open.
    #[wasm_bindgen(js_name = wizard)]
    pub fn wizard(&self) -> Option<FolderWizardMachine> {
        self.0.wizard().map(FolderWizardMachine)
    }

    /// Re-read the device / folder / conflict lists. Resolves when done (read
    /// `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = refresh)]
    pub fn refresh(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.refresh().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Load and fold the **T16 custody facet** (`docs/goal/ui/devices.md`
    /// § Custody facet) — the `custody-holder-*` family's rows, resolving to the
    /// same `CustodyFacetView` JSON shape the native apps get over UniFFI. One
    /// projection, two faces: `fauna_client_capabilities::custody_view` carries
    /// `uniffi::Record` for them and serde for us, so web cannot drift from the
    /// four native apps on which receipt state reads which way.
    ///
    /// `secretHex` is the caller's own 32-byte actor secret, same as
    /// `setForeignSetsSource`/`setLabelCustody`. Each row already carries both
    /// shared label decisions (`statusLabel` + `attestedAtSecs`,
    /// `heldBytesLabel`/`held`/`cap`/`degraded`) — resolve them through the SPA's
    /// i18n runtime and format the timestamp in JS; the shared side hands out
    /// epoch seconds because `format_unix_local` needs the OS tz database, which
    /// wasm lacks.
    ///
    /// The ceremony records (`fauna.state.custody-ceremony`) and the grant log
    /// are both the core chunk's account store's: both cross the account
    /// `port` (a `SharedAccountPort` minted for this account — the
    /// custody-ceremony and succession-ledger seams' forwarders).
    /// Resolves to `null` when either was unreadable this pass (no runtime in
    /// this tab included) — a transient, so keep the previous rows rather than
    /// painting an empty list over live ones.
    ///
    /// ⚠ Render **piece 2 only**. `held` and `offers` cross so the boundary
    /// reports what the fold found, but their controls (budget, stop, accept)
    /// write the R14 (account-data-plane.md § The ratified decisions) registry row, which no custody seam crosses the
    /// port for yet — and `devices.md` defines the host-side card *as* those
    /// controls, so a card without them contradicts the ratified text.
    #[wasm_bindgen(js_name = custodyFacetLoad)]
    pub fn custody_facet_load(
        &self,
        secret_hex: String,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<js_sys::Promise, JsValue> {
        use fauna_client_config::CustodyCeremonyStore;
        // The secret still gates the call: a malformed one is refused here,
        // as at every custody export. Its account is the one the port was
        // minted for.
        let actor = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?.actor_id();
        let ledger = port_ledger(port.clone().unchecked_into(), actor)?;
        let transport = fauna_account_port::JsAccountTransport::new(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let records = fauna_client_config::custody_port::PortCustodyStore::new(transport);
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            let Ok(custody) = records.custody().await else {
                return Ok(JsValue::NULL);
            };
            let Ok(ledger) = ledger.load().await else {
                return Ok(JsValue::NULL);
            };
            // The trust facet's render clock — the same one the native legs
            // fold against (`trust_clock`; the e2e offset is zero in real runs).
            let now_micros = fauna_client_capabilities::trust_clock::render_now_micros(
                fauna_core::data::Timestamp::now().0,
            );
            // No registry overlay: no seam crosses the port for the
            // `custodies-held` rows. An overlay-less fold is a CORRECT render —
            // an unmatched held row keeps its accept-seeded budget.
            let facet = fauna_client_capabilities::view_model::fold_custody_facet(
                &custody, &ledger, now_micros,
            );
            let view =
                fauna_client_capabilities::custody_view::CustodyFacetView::from_snapshot(&facet);
            serde_wasm_bindgen::to_value(&view).map_err(err_to_js)
        }))
    }

    /// Revoke a custody grant (`custody-holder-revoke-button`) — piece 2's one
    /// gesture, and the one custody act that needs nothing native.
    ///
    /// The assembly, including the load-bearing order (**the nest's revoke
    /// BEFORE the signed record**, so a recorded revoke the nest never saw
    /// cannot leave the capability live), is shared with the native apps in
    /// `fauna_client_capabilities::custody_acts::revoke_custody`. Web is not a
    /// re-implementation of it — it is the same function.
    ///
    /// `grantId` and `holder` are the row's own `grantId` / `custodianKey`
    /// bytes; a row still `pending` carries no holder, which is why the SPA
    /// disables the control there. Resolves to the error string for the page's
    /// `error-message` element, or `null` on success — never a silent drop (e2e
    /// convention 11). Re-run `custodyFacetLoad` afterwards to repaint.
    ///
    /// The SPA's revoke copy MUST state the honest bound (`ui/nests.md` § Trust
    /// facet — custody rows): revocation stops future carriage and serving on
    /// honest boxes; copies already held stay held.
    #[wasm_bindgen(js_name = custodyRevoke)]
    pub fn custody_revoke(
        &self,
        secret_hex: String,
        grant_id: Vec<u8>,
        holder: Option<Vec<u8>>,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<js_sys::Promise, JsValue> {
        let keypair = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let ledger = port_ledger(port, keypair.actor_id())?;
        let secret = keypair.secret_bytes().to_owned();
        let holder = match holder {
            None => None,
            Some(bytes) => Some(
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map_err(|_| JsValue::from_str("a custodian key must be exactly 32 bytes"))?,
            ),
        };
        let client = self.1.clone();
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            match fauna_client_capabilities::custody_acts::revoke_custody(
                client, &*ledger, secret, &grant_id, holder,
            )
            .await
            {
                None => Ok(JsValue::NULL),
                Some(err) => Ok(JsValue::from_str(&err)),
            }
        }))
    }

    /// **Follow a public folder** — first contact, addressed by the OWNER (a
    /// handle or a bare 64-hex actor id, the same superset `share_set` takes)
    /// + the folder's plaintext name, exactly as the follow flow collects them
    /// (`docs/goal/ui/folders.md` § Following a public folder). The browser
    /// twin of UniFFI's `folders_follow_public`.
    ///
    /// A thin façade over the shared composition
    /// [`fauna_client_folders::follow_ops::follow_public_folder`], which owns
    /// the address rules: hex-or-handle classification, the same-nest
    /// `fauna.actor.by_handle` hop, the cross-nest discovery of a
    /// `handle@domain`'s home nest (the anon hop to the peer, the record's
    /// `home_nest_url` derived from the domain), the blank-field refusal, and
    /// the folding of the three not-found causes. Until 2026-09-22 this face
    /// took a pre-resolved actor id and an always-empty `homeNestUrl` from the
    /// SPA, so web could only ever follow on its own nest — the very
    /// divergence the shared recipe exists to prevent.
    ///
    /// Resolves to the stored record — **serde snake_case**, like every
    /// snapshot type: `{ home_nest_url, home_nest_actor_id, owner_actor_id,
    /// owner_handle, folder_id, display_name }` — so the page renders exactly
    /// what was saved; re-run `refresh()` afterwards to repaint the rows with
    /// their availability.
    ///
    /// A folder that is absent, private, or misspelled all **reject the same
    /// way**, deliberately — the home nest folds them into one answer so
    /// nothing can probe for the existence of a sealed folder, and this façade
    /// preserves that rather than inventing a friendlier message per case. Put
    /// the rejection's string on the page's `error-message` (e2e convention 2)
    /// — never swallow it.
    ///
    /// The record lands in the account's `fauna.state.follows` row for the
    /// folder, across the account `port`; with no runtime serving the account
    /// in this tab the follow rejects rather than recording nowhere.
    #[wasm_bindgen(js_name = followPublicFolder)]
    pub fn follow_public_folder(
        &self,
        secret_hex: String,
        owner: String,
        folder_name: String,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<js_sys::Promise, JsValue> {
        // The secret still gates the call: a malformed one is refused here.
        fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let store = follows_store(port)?;
        let client = self.1.clone();
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            let stored = fauna_client_folders::follow_ops::follow_public_folder(
                client,
                &*store,
                &owner,
                &folder_name,
            )
            .await
            .map_err(fauna_client_folders::follow_ops::follow_error_text)
            .map_err(|text| JsValue::from_str(&text))?;
            serde_wasm_bindgen::to_value(&stored).map_err(err_to_js)
        }))
    }

    /// **Unfollow** — the account's row for the folder is tombstoned (across
    /// the account `port`); there is nothing to revoke anywhere, because the
    /// home nest never knew about this follower. Idempotent: removing
    /// something already gone succeeds. Resolves to the stored list.
    /// `folderId` is the row's pinned id (a JS `bigint`).
    #[wasm_bindgen(js_name = unfollowPublicFolder)]
    pub fn unfollow_public_folder(
        &self,
        secret_hex: String,
        home_nest_url: String,
        folder_id: i64,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<js_sys::Promise, JsValue> {
        fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let store = follows_store(port)?;
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            let stored = fauna_client_config::save_unfollow(&*store, &home_nest_url, folder_id)
                .await
                .map_err(err_to_js)?;
            serde_wasm_bindgen::to_value(&stored).map_err(err_to_js)
        }))
    }

    /// The user's followed public folders **as stored** — the list without the
    /// availability probe. The page's rows (which carry availability) come from
    /// `snapshotJson`'s `followed`; this is for a caller that only needs the
    /// records, e.g. to decide whether to offer *Follow* for an address already
    /// followed. The browser twin of UniFFI's `folders_followed_list`.
    #[wasm_bindgen(js_name = followedFolders)]
    pub fn followed_folders(
        &self,
        secret_hex: String,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<js_sys::Promise, JsValue> {
        fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let store = follows_store(port)?;
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            let stored = fauna_client_config::load_followed_folders(&*store)
                .await
                .map_err(err_to_js)?;
            serde_wasm_bindgen::to_value(&stored).map_err(err_to_js)
        }))
    }

    #[wasm_bindgen(js_name = openWizard)]
    pub fn open_wizard(&self) {
        self.0.open_wizard()
    }

    #[wasm_bindgen(js_name = closeWizard)]
    pub fn close_wizard(&self) {
        self.0.close_wizard()
    }

    /// Remove (unregister) the device at `index` into the current device list.
    #[wasm_bindgen(js_name = removeDevice)]
    pub fn remove_device(&self, index: u32) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.remove_device(index).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `device-p2p-participation-toggle` — the row's
    /// `p2p_participation_paint` says what the click sends
    /// (`!paint.checked`). This machine wires no participation door (the tab
    /// runs no listener, so it has no own row — `p2p.md` § Per-device
    /// participation → *Web*): every row takes the sibling arm, so the one
    /// thing it ever sends is a request that the device turn off, and an
    /// `on` paints the remote-enable refusal on `error-message`.
    #[wasm_bindgen(js_name = setP2pParticipation)]
    pub fn set_p2p_participation(&self, index: u32, on: bool) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.set_p2p_participation(index, on).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `device-member-remove-confirm-button` — remove the signed-in device
    /// without a matching entry whose card carried `device_id_hex`, **by its
    /// key** (`ui/devices.md` § Members without a matching entry, the gesture
    /// bullet): web never exposes a position form. An id no longer listed is
    /// ignored (the refresh already dropped its card).
    #[wasm_bindgen(js_name = removeMember)]
    pub fn remove_member(&self, device_id_hex: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.remove_member_by_id(device_id_hex).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = deleteFolder)]
    pub fn delete_folder(&self, name: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.delete_folder(name).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Resolve conflict `id` keeping candidate `winningManifestHash`
    /// (`undefined`/`null` = candidate-free (mark-only) resolve).
    #[wasm_bindgen(js_name = resolveConflict)]
    pub fn resolve_conflict(
        &self,
        id: i64,
        winning_manifest_hash: Option<String>,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.resolve_conflict(id, winning_manifest_hash).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Save selective-sync paths for folder `name`. Each of `includePaths` /
    /// `excludePaths` is a JSON-deserializable `string[]` or `undefined`/`null`
    /// to leave unchanged.
    #[wasm_bindgen(js_name = setFolderPaths)]
    pub fn set_folder_paths(
        &self,
        name: String,
        include_paths: JsValue,
        exclude_paths: JsValue,
    ) -> Result<js_sys::Promise, JsValue> {
        let include = parse_opt_string_vec(include_paths, "includePaths")?;
        let exclude = parse_opt_string_vec(exclude_paths, "excludePaths")?;
        let inner = Arc::clone(&self.0);
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            inner.set_folder_paths(name, include, exclude).await;
            Ok(JsValue::UNDEFINED)
        }))
    }

    /// Set folder `name`'s conflict policy (`"auto"` | `"latest_wins_always"`)
    /// — the per-set `folder-conflict-policy-select` edit (file-sync.md §
    /// Conflicts, policy).
    #[wasm_bindgen(js_name = setFolderConflictPolicy)]
    pub fn set_folder_conflict_policy(
        &self,
        name: String,
        conflict_policy: String,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner
                .set_folder_conflict_policy(name, conflict_policy)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Set folder `name`'s audience — the write behind `folder-audience-select`
    /// (`ui/folders.md` § Audience and website serving), then refresh.
    ///
    /// **Keyless** — a plain `fauna.folders.update`; the back-catalogue is moved
    /// by each device's own engine at its next catch-up off the projected
    /// audience, not by this caller.
    ///
    /// ⚠ Pass only `"private"` / `"public"`. `→shared` stages a custody sentinel
    /// and is not this control's direction — `audienceOptions(bound)` renders
    /// `shared` unselectable for exactly that reason.
    ///
    /// ⚠ **`→public` is confirm-gated, and the gate is the SPA's to hold**: a
    /// public folder rests UNSEALED, names and paths included, so the page arms
    /// `folder-audience-public-confirm` and calls this only once it is answered.
    /// While armed the select keeps painting the folder's CURRENT audience —
    /// showing `public` before the answer would report an audience the folder
    /// does not have.
    #[wasm_bindgen(js_name = setFolderAudience)]
    pub fn set_folder_audience(&self, name: String, audience: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.set_folder_audience(name, audience).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Flip folder `name`'s website serving — the write behind
    /// `folder-website-toggle`, then refresh.
    ///
    /// The only door to a website folder since phase 2 slice e retired the
    /// wizard's mode step. Orthogonal to the audience: keep the toggle
    /// **enabled** on a folder that is neither public nor paywalled — the
    /// setting is real, merely inert — and say so with `websiteServeHint`
    /// rather than by disabling it.
    #[wasm_bindgen(js_name = setFolderWebsiteEnabled)]
    pub fn set_folder_website_enabled(&self, name: String, enabled: bool) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.set_folder_website_enabled(name, enabled).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Set folder `name`'s content residency — the write behind
    /// `folder-nest-residency-select` / `folder-residency-confirm` (folders
    /// re-model phase 5; `file-sync.md` § Content residency), then refresh.
    ///
    /// **Keyless** — its own `fauna.folders.update` field, deliberately never
    /// folded into the batched `setFolderNestPlace` write.
    ///
    /// ⚠ **`→metadata_only` is confirm-gated, and the gate is the SPA's to
    /// hold**: the nest deletes its chunk bytes for the folder on that write,
    /// so the page arms `folder-residency-confirm` and calls this only once
    /// answered. While armed the select keeps painting the folder's CURRENT
    /// residency — showing `metadata_only` before the answer would report a
    /// residency the folder does not have (the `folder-audience-select` /
    /// `folder-audience-public-confirm` shape).
    #[wasm_bindgen(js_name = setFolderResidency)]
    pub fn set_folder_residency(&self, name: String, residency: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.set_folder_residency(name, residency).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Set one device place's flags on folder `name` — the post-create place
    /// editor's three checkboxes (`folder-place-row`), then refresh.
    ///
    /// ⚠ **The point applies whole.** Send the seat's full triple, never just
    /// the box that moved, or the two left alone are silently cleared. Every
    /// flag point is writable since phase 2 slice f, so the SPA paints three
    /// checkboxes and **no refusal** — a combination no legacy role names is
    /// sendable and comes to rest unrounded.
    ///
    /// ⚠ The per-seat rows are NOT on this machine's snapshot: after this
    /// resolves the page repaints from a roster RE-READ
    /// (`fauna.folders.members.list`), never from an optimistic local flip.
    #[wasm_bindgen(js_name = setFolderPlace)]
    pub fn set_folder_place(
        &self,
        name: String,
        device_id: String,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner
                .set_folder_place(name, device_id, originates, accepts, applies_deletes)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Save folder `name`'s **nest-place snapshot policy** — the four
    /// `folder-nest-*` controls, committed together by `folder-nest-save-button`
    /// (`backup-restore.md` § 8b).
    ///
    /// Takes the four controls' **raw values**, not a parsed policy, and routes
    /// them through the shared `nest_place_write`. That is deliberate: the rules
    /// that turn boxes into a policy are the two traps this editor has (blank is
    /// a *value*, and retention's `None` means *leave unchanged*, so a cleared
    /// retention must ride as the canonical binds-nothing policy). Parsing in
    /// TypeScript would hand web its own copy of both. It also sidesteps
    /// marshalling an `Option<i64>` across the boundary, which reaches JS as a
    /// `bigint` and is a standing source of `TypeError`s.
    ///
    /// `snapshots` is `"default"` | `"on"` | `"off"`; the other five are the
    /// text boxes verbatim, where empty means *unset*. The version-retention
    /// pair (`version_retention_count`/`_days`) is its own SIBLING family,
    /// sent whole on the same save — never folded into the
    /// snapshot `retention` above.
    #[wasm_bindgen(js_name = setFolderNestPlace)]
    pub fn set_folder_nest_place(
        &self,
        name: String,
        snapshots: String,
        quiet_secs: String,
        retention_snapshots: String,
        retention_days: String,
        version_retention_count: String,
        version_retention_days: String,
    ) -> js_sys::Promise {
        let write =
            fauna_folders_machine::nest_place_write(&fauna_folders_machine::NestPlaceEdit {
                snapshots,
                quiet_secs,
                retention_snapshots,
                retention_days,
            });
        let version_retention = fauna_folders_machine::version_retention_write(
            &fauna_folders_machine::VersionRetentionEdit {
                count: version_retention_count,
                days: version_retention_days,
            },
        );
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner
                .set_folder_nest_place(
                    name,
                    write.snapshots,
                    write.quiet_secs,
                    write.retention,
                    Some(version_retention),
                )
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The review list's one-tap "use the other version" on auto-resolved
    /// conflict `id`: re-point the file at the latest retained non-winning
    /// candidate (the § File Versions restore record, attributed to
    /// `deviceId` — the caller's recording device).
    #[wasm_bindgen(js_name = useOtherVersion)]
    pub fn use_other_version(&self, id: i64, device_id: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.use_other_version(id, device_id).await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// The account's followed-folders store across the account `port`
/// (`fauna_client_config::follows_port`), a malformed port refused by name.
fn follows_store(
    port: fauna_account_port::JsAccountPort,
) -> Result<Arc<dyn fauna_client_config::FollowsStore>, JsValue> {
    fauna_client_config::follows_port::from_js_port(port)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Parse an optional `string[]` JS value (`undefined`/`null` → `None`).
fn parse_opt_string_vec(v: JsValue, field: &str) -> Result<Option<Vec<String>>, JsValue> {
    if v.is_undefined() || v.is_null() {
        Ok(None)
    } else {
        serde_wasm_bindgen::from_value(v)
            .map(Some)
            .map_err(|e| JsValue::from_str(&format!("invalid {field}: {e}")))
    }
}

// ── Panic hook ───────────────────────────────────────────────────────────
//
// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment). `#[wasm_bindgen(start)]` runs
// automatically the moment this chunk's module is instantiated — no SPA-side
// call site to add or remember, unlike `fauna-wasm`'s explicit `installLogging`.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-folders");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}

/// The account's succession ledger through the tab's account port, minted for
/// `actor` (`fauna_client_config::succession_ledger_port`) — what the custody
/// facet reads the grant log from and the custody revoke records through.
/// Throws on an object that is not a port.
fn port_ledger(
    port: fauna_account_port::JsAccountPort,
    actor: fauna_core::identity::ActorId,
) -> Result<Arc<dyn fauna_client_config::SuccessionLedgerStore>, JsValue> {
    fauna_client_config::succession_ledger_port::from_js_port(port, actor)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// The account's folder-key custody through the tab's account port — the
/// forwarder the core chunk answers over `PlaneFolderKeys`
/// (`fauna_client_folders::port`). Throws on an object that is not a port.
fn port_folder_keys(
    port: fauna_account_port::JsAccountPort,
) -> Result<Arc<dyn fauna_client_folders::FolderKeyStore>, JsValue> {
    let transport = fauna_account_port::JsAccountTransport::new(port.into())
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(Arc::new(fauna_client_folders::port::PortFolderKeys::new(
        transport,
    )))
}
